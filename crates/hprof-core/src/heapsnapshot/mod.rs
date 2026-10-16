pub mod diff;
pub mod dominator;
pub mod parser;
pub mod paths;
pub mod reports;
pub mod strings;

use std::collections::HashMap;
use std::fs;
use std::io::Read;

use ahash::AHashMap;
use memmap2::Mmap;
use rayon::prelude::*;

use crate::types::*;

pub use dominator::RetainedData;
pub use parser::{
    aligned_record_chunks_par, edge_columns_streaming, find_array_end, find_marker,
    find_matching_brace, find_nth_comma_par, node_columns_streaming, parse_meta_from_value,
    parse_numbers_fast, parse_numbers_par, rfind_marker,
};
pub use paths::ParentMap;
pub use strings::{decode_json_string, StringTable};

pub const EDGE_SYNC_STRIDE: usize = 1024;

pub struct HeapSnapshot {
    pub(crate) file_path: String,
    pub(crate) meta: Option<SnapshotMeta>,
    pub(crate) raw: Option<RawData>,
    pub(crate) edge_starts: Option<Vec<u32>>,
    pub(crate) raw_summary: Option<RawSummary>,
    pub(crate) retained_data: Option<RetainedData>,
    pub(crate) parents: Option<ParentMap>,
}

pub(crate) struct RawSummary {
    pub(crate) total_size: usize,
    pub(crate) total_count: usize,
    pub(crate) by_name_idx: Vec<(u32, usize, usize)>,
    pub(crate) by_node_type: HashMap<String, TypeSummary>,
}

pub struct NodeColumns {
    pub types: Vec<u8>,
    pub names: Vec<u32>,
    pub ids: Vec<u32>,
    pub self_sizes: Vec<u32>,
    pub edge_counts: Vec<u32>,
    pub detachedness: Vec<u8>,
}

pub struct EdgeColumns {
    pub types: Vec<u8>,
    pub to_nodes: Vec<u32>,
    pub sync_offsets: Vec<usize>,
    pub bytes_start: usize,
    pub bytes_end: usize,
}

pub struct RawData {
    pub meta: SnapshotMeta,
    pub mmap: Mmap,
    pub nodes: NodeColumns,
    pub(crate) edges_open: Option<usize>,
    pub(crate) edges_range: Option<(usize, usize)>,
    pub edges: Option<EdgeColumns>,
    pub strings: StringTable,
    pub edge_offsets: EdgeFieldOffsets,
    pub node_types: Vec<String>,
    pub edge_types: Vec<String>,
    pub node_field_count: usize,
    pub edge_field_count: usize,
}

impl RawData {
    pub(crate) fn for_each_edge_in_range<F>(&self, start: usize, end: usize, mut f: F)
    where
        F: FnMut(usize, usize, u32, usize),
    {
        if start >= end {
            return;
        }
        let efc = self.edge_field_count;
        if efc == 0 {
            return;
        }
        let type_off = self.edge_offsets.type_;
        let name_off = self.edge_offsets.name_or_index;
        let to_off = self.edge_offsets.to_node;
        let nfc = self.node_field_count.max(1) as u32;

        if let Some(ref cols) = self.edges {
            if !cols.sync_offsets.is_empty() {
                let sync_idx = start / EDGE_SYNC_STRIDE;
                let sync_edge = sync_idx * EDGE_SYNC_STRIDE;
                let skip_nums = (start - sync_edge) * efc;
                let base_byte = cols.bytes_start + cols.sync_offsets[sync_idx];
                let slice = &self.mmap[base_byte..cols.bytes_end];
                let mut pos = if skip_nums == 0 {
                    0
                } else {
                    match memchr::memchr_iter(b',', slice).nth(skip_nums - 1) {
                        Some(rel) => rel + 1,
                        None => return,
                    }
                };
                let len = slice.len();
                let mut rec = [0u32; 8];
                let mut field = 0usize;
                let mut edge_idx = start;
                while pos < len && edge_idx < end {
                    let ch = slice[pos];
                    if ch == b']' {
                        break;
                    }
                    if ch.is_ascii_digit() {
                        let mut val = (ch - b'0') as u32;
                        pos += 1;
                        while pos < len && slice[pos].is_ascii_digit() {
                            val = val * 10 + (slice[pos] - b'0') as u32;
                            pos += 1;
                        }
                        rec[field] = val;
                        field += 1;
                        if field == efc {
                            field = 0;
                            f(
                                edge_idx,
                                cols.types[edge_idx] as usize,
                                rec[name_off],
                                cols.to_nodes[edge_idx] as usize,
                            );
                            edge_idx += 1;
                        }
                        continue;
                    }
                    pos += 1;
                }
                return;
            }
        }

        let (eb_start, eb_end) = match self.edges_range {
            Some(r) => r,
            None => return,
        };
        let edges_bytes = &self.mmap[eb_start..=eb_end];
        let skip_nums = start * efc;
        let mut pos = match find_nth_comma_par(edges_bytes, skip_nums) {
            Some(p) => p,
            None => return,
        };
        let len = edges_bytes.len();
        let mut rec = [0u32; 8];
        let mut field = 0usize;
        let mut edge_idx = start;
        while pos < len && edge_idx < end {
            let ch = edges_bytes[pos];
            if ch == b']' {
                break;
            }
            if ch.is_ascii_digit() {
                let mut val = (ch - b'0') as u32;
                pos += 1;
                while pos < len && edges_bytes[pos].is_ascii_digit() {
                    val = val * 10 + (edges_bytes[pos] - b'0') as u32;
                    pos += 1;
                }
                rec[field] = val;
                field += 1;
                if field == efc {
                    field = 0;
                    f(
                        edge_idx,
                        rec[type_off] as usize,
                        rec[name_off],
                        (rec[to_off] / nfc) as usize,
                    );
                    edge_idx += 1;
                }
                continue;
            }
            pos += 1;
        }
    }
}

impl HeapSnapshot {
    pub fn new(file_path: String) -> Self {
        Self {
            file_path,
            meta: None,
            raw: None,
            edge_starts: None,
            raw_summary: None,
            retained_data: None,
            parents: None,
        }
    }

    pub fn meta(&mut self) -> crate::Result<&SnapshotMeta> {
        if self.meta.is_none() {
            self.parse_meta()?;
        }
        Ok(self.meta.as_ref().unwrap())
    }

    pub(crate) fn mmap_file(&self) -> crate::Result<Mmap> {
        let file = fs::File::open(&self.file_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        #[cfg(unix)]
        {
            let _ = mmap.advise(memmap2::Advice::Sequential);
        }
        Ok(mmap)
    }

    fn parse_meta(&mut self) -> crate::Result<()> {
        let file = fs::File::open(&self.file_path)?;
        let mut reader = std::io::BufReader::new(file);
        let mut chunk_size = 2 * 1024 * 1024;
        let max_chunk = 64 * 1024 * 1024;

        while chunk_size <= max_chunk {
            let mut buffer = vec![0u8; chunk_size];
            let bytes_read = reader.read(&mut buffer)?;
            let prefix = unsafe { std::str::from_utf8_unchecked(&buffer[..bytes_read]) };

            if let Some(snapshot_start) = prefix.find("\"snapshot\":") {
                if let Some(nodes_start) = prefix.find("\"nodes\":[") {
                    if snapshot_start < nodes_start {
                        let object_start = prefix[snapshot_start..].find('{');
                        if let Some(obj_offset) = object_start {
                            let obj_start = snapshot_start + obj_offset;
                            if let Some(end) = find_matching_brace(prefix, obj_start) {
                                let json_str = &prefix[obj_start..=end];
                                let meta = parse_meta_from_value(json_str)?;
                                self.meta = Some(meta);
                                return Ok(());
                            }
                        }
                    }
                }
            }

            if !prefix.contains("\"nodes\":[") {
                chunk_size *= 2;
                reader = std::io::BufReader::new(fs::File::open(&self.file_path)?);
                continue;
            }

            return Err(Error::HeaderParseFailed);
        }

        Err(Error::HeaderParseFailed)
    }

    pub(crate) fn ensure_raw(&mut self) -> crate::Result<()> {
        if self.raw.is_some() {
            return Ok(());
        }

        let _ = self.meta()?;
        let meta = self.meta.as_ref().unwrap().clone();

        let mmap = self.mmap_file()?;
        let data = &mmap[..];

        let nodes_marker = b"\"nodes\":[";
        let strings_marker = b"\"strings\":[";

        let nodes_start = find_marker(data, nodes_marker).ok_or(Error::HeaderParseFailed)?;

        let nodes_open = nodes_start + nodes_marker.len() - 1;
        let nodes_end = find_array_end(data, nodes_open);
        let nodes_bytes = &data[nodes_open + 1..=nodes_end];

        let edges_marker = b"\"edges\":[";
        let lookahead_end = (nodes_end + 1024).min(data.len());
        let edges_open = if nodes_end + 1 < lookahead_end {
            find_marker(&data[nodes_end + 1..lookahead_end], edges_marker)
                .map(|rel| nodes_end + 1 + rel + edges_marker.len() - 1)
        } else {
            None
        };

        let node_offsets = NodeFieldOffsets::from_fields(&meta.meta.node_fields)?;
        let edge_offsets = EdgeFieldOffsets::from_fields(&meta.meta.edge_fields)?;
        let node_types = meta.meta.node_types.first().cloned().unwrap_or_default();
        let edge_types = meta.meta.edge_types.first().cloned().unwrap_or_default();
        let node_field_count = meta.meta.node_fields.len();
        let edge_field_count = meta.meta.edge_fields.len();

        let nodes = match node_columns_streaming(nodes_bytes, node_field_count, &node_offsets) {
            Some(cols) => cols,
            None => {
                let flat = parse_numbers_par(nodes_bytes);
                let types: Vec<u8> = flat
                    .par_chunks(node_field_count)
                    .map(|r| r[node_offsets.type_] as u8)
                    .collect();
                let names: Vec<u32> = flat
                    .par_chunks(node_field_count)
                    .map(|r| r[node_offsets.name])
                    .collect();
                let ids: Vec<u32> = flat
                    .par_chunks(node_field_count)
                    .map(|r| r[node_offsets.id])
                    .collect();
                let self_sizes: Vec<u32> = flat
                    .par_chunks(node_field_count)
                    .map(|r| r[node_offsets.self_size])
                    .collect();
                let edge_counts: Vec<u32> = flat
                    .par_chunks(node_field_count)
                    .map(|r| r[node_offsets.edge_count])
                    .collect();
                let detachedness: Vec<u8> = flat
                    .par_chunks(node_field_count)
                    .map(|r| {
                        node_offsets
                            .detachedness
                            .map(|offset| r[offset] as u8)
                            .unwrap_or(0)
                    })
                    .collect();
                drop(flat);
                NodeColumns {
                    types,
                    names,
                    ids,
                    self_sizes,
                    edge_counts,
                    detachedness,
                }
            }
        };

        let strings_table = if data.len() < (16 << 20) {
            let search_from = (nodes_end + 1).min(data.len());
            let strings_start = rfind_marker(&data[search_from..], strings_marker)
                .map(|rel| search_from + rel)
                .or_else(|| find_marker(data, strings_marker))
                .ok_or(Error::HeaderParseFailed)?;
            StringTable::new_at(strings_start + strings_marker.len() - 1)
        } else {
            StringTable::new_deferred(nodes_end + 1)
        };

        self.raw = Some(RawData {
            meta,
            mmap,
            nodes,
            edges_open,
            edges_range: None,
            edges: None,
            strings: strings_table,
            edge_offsets,
            node_types,
            edge_types,
            node_field_count,
            edge_field_count,
        });
        Ok(())
    }

    pub(crate) fn locate_edges_range(&mut self) -> crate::Result<(usize, usize)> {
        let raw = self.raw.as_mut().unwrap();
        if let Some(r) = raw.edges_range {
            return Ok(r);
        }
        let open = match raw.edges_open {
            Some(o) => o,
            None => {
                let edges_marker = b"\"edges\":[";
                let start = find_marker(&raw.mmap, edges_marker).ok_or(Error::HeaderParseFailed)?;
                let o = start + edges_marker.len() - 1;
                raw.edges_open = Some(o);
                o
            }
        };
        let end = find_array_end(&raw.mmap, open);
        let range = (open + 1, end);
        raw.edges_range = Some(range);
        Ok(range)
    }

    pub(crate) fn ensure_edges(&mut self) -> crate::Result<()> {
        let needs = match self.raw.as_ref() {
            Some(raw) => raw.edges.is_none(),
            None => return Ok(()),
        };
        if !needs {
            return Ok(());
        }
        let (start, end) = self.locate_edges_range()?;
        let edges = {
            let raw = self.raw.as_ref().unwrap();
            let edges_bytes = &raw.mmap[start..=end];
            let nfc = raw.node_field_count.max(1) as u32;
            let efc = raw.edge_field_count;
            match edge_columns_streaming(edges_bytes, start, end + 1, efc, nfc, &raw.edge_offsets) {
                Some(cols) => cols,
                None => {
                    let flat = parse_numbers_par(edges_bytes);
                    let type_off = raw.edge_offsets.type_;
                    let to_off = raw.edge_offsets.to_node;
                    let types: Vec<u8> = flat
                        .par_chunks(efc)
                        .map(|r| r.get(type_off).copied().unwrap_or(0) as u8)
                        .collect();
                    let to_nodes: Vec<u32> = flat
                        .par_chunks(efc)
                        .map(|r| r.get(to_off).copied().unwrap_or(0) / nfc)
                        .collect();
                    EdgeColumns {
                        types,
                        to_nodes,
                        sync_offsets: Vec::new(),
                        bytes_start: start,
                        bytes_end: end + 1,
                    }
                }
            }
        };
        self.raw.as_mut().unwrap().edges = Some(edges);
        Ok(())
    }

    pub(crate) fn create_node(raw: &RawData, node_index: usize) -> HeapSnapshotNode {
        let col = &raw.nodes;
        let type_idx = col.types[node_index] as usize;
        let name_idx = col.names[node_index] as usize;
        let self_size = col.self_sizes[node_index] as usize;
        let id = col.ids[node_index] as usize;
        let edge_count = col.edge_counts[node_index] as usize;

        let type_ = raw
            .node_types
            .get(type_idx)
            .cloned()
            .unwrap_or_else(|| type_idx.to_string());
        let name = raw
            .strings
            .resolve(&raw.mmap, name_idx)
            .unwrap_or_else(|| format!("<string#{}>", name_idx));

        HeapSnapshotNode {
            type_,
            name,
            self_size,
            retention_size: None,
            id,
            edge_count,
        }
    }

    pub fn stream_summary(
        &mut self,
        top: usize,
        filter: Option<&str>,
    ) -> crate::Result<HeapSnapshotSummary> {
        self.ensure_raw_summary()?;
        let raw = self.raw.as_ref().unwrap();
        let summary = self.raw_summary.as_ref().unwrap();
        let filter_re = filter.and_then(|f| regex::Regex::new(f).ok());

        let mut by_node_name: HashMap<String, TypeSummary> = HashMap::new();
        for &(name_idx, size, count) in &summary.by_name_idx {
            if top != usize::MAX && by_node_name.len() >= top {
                break;
            }
            if size == 0 {
                continue;
            }
            let name = raw
                .strings
                .resolve(&raw.mmap, name_idx as usize)
                .unwrap_or_else(|| format!("<string#{}>", name_idx));
            if let Some(re) = &filter_re {
                if !re.is_match(&name) {
                    continue;
                }
            }
            let entry = by_node_name
                .entry(name)
                .or_insert(TypeSummary { size: 0, count: 0 });
            entry.size += size;
            entry.count += count;
        }

        Ok(HeapSnapshotSummary {
            total_size: summary.total_size,
            total_count: summary.total_count,
            by_node_name,
            by_node_type: summary.by_node_type.clone(),
        })
    }

    fn ensure_raw_summary(&mut self) -> crate::Result<()> {
        if self.raw_summary.is_some() {
            return Ok(());
        }
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        let col = &raw.nodes;
        let node_types = raw.node_types.as_slice();

        let total_node_count = col.ids.len();
        let num_threads = rayon::current_num_threads().max(1);
        let num_chunks = (num_threads * 4).max(1);
        let chunk_size = total_node_count.div_ceil(num_chunks).max(1);

        let partials: Vec<(
            usize,
            usize,
            AHashMap<u32, (usize, usize)>,
            Vec<(usize, usize)>,
        )> = (0..num_chunks)
            .into_par_iter()
            .map(|c| {
                let start = (c * chunk_size).min(total_node_count);
                let end = ((c + 1) * chunk_size).min(total_node_count);
                let mut local_size = 0usize;
                let mut local_count = 0usize;
                let mut local_by_name: AHashMap<u32, (usize, usize)> = AHashMap::new();
                let mut local_by_type: Vec<(usize, usize)> = vec![(0, 0); node_types.len().max(16)];

                for n in start..end {
                    let self_size = col.self_sizes[n] as usize;
                    local_count += 1;
                    local_size += self_size;

                    if self_size > 0 {
                        let type_idx = col.types[n] as usize;
                        let name_idx = col.names[n];
                        let e = local_by_name.entry(name_idx).or_insert((0, 0));
                        e.0 += self_size;
                        e.1 += 1;
                        if type_idx >= local_by_type.len() {
                            local_by_type.resize(type_idx + 1, (0, 0));
                        }
                        local_by_type[type_idx].0 += self_size;
                        local_by_type[type_idx].1 += 1;
                    }
                }
                (local_size, local_count, local_by_name, local_by_type)
            })
            .collect();

        let mut total_size = 0usize;
        let mut total_count = 0usize;
        let mut by_name_map: AHashMap<u32, (usize, usize)> = AHashMap::new();
        let mut by_type_vec: Vec<(usize, usize)> = vec![(0, 0); node_types.len().max(16)];

        for (size, count, local_names, local_types) in partials {
            total_size += size;
            total_count += count;
            for (name_idx, (sz, cnt)) in local_names {
                let e = by_name_map.entry(name_idx).or_insert((0, 0));
                e.0 += sz;
                e.1 += cnt;
            }
            if local_types.len() > by_type_vec.len() {
                by_type_vec.resize(local_types.len(), (0, 0));
            }
            for (type_idx, (sz, cnt)) in local_types.into_iter().enumerate() {
                by_type_vec[type_idx].0 += sz;
                by_type_vec[type_idx].1 += cnt;
            }
        }

        let mut by_name_idx: Vec<(u32, usize, usize)> = by_name_map
            .into_iter()
            .filter(|&(_, (sz, _))| sz > 0)
            .map(|(idx, (sz, cnt))| (idx, sz, cnt))
            .collect();
        by_name_idx.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let mut by_node_type: HashMap<String, TypeSummary> = HashMap::new();
        for (type_idx, (size, count)) in by_type_vec.into_iter().enumerate() {
            if size == 0 && count == 0 {
                continue;
            }
            let type_name = node_types
                .get(type_idx)
                .cloned()
                .unwrap_or_else(|| type_idx.to_string());
            by_node_type.insert(type_name, TypeSummary { size, count });
        }

        self.raw_summary = Some(RawSummary {
            total_size,
            total_count,
            by_name_idx,
            by_node_type,
        });
        Ok(())
    }

    pub fn get_node(&mut self, node_index: usize) -> crate::Result<HeapSnapshotNode> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        if node_index >= raw.meta.node_count {
            return Err(Error::NodeNotFound(node_index));
        }
        Ok(Self::create_node(raw, node_index))
    }

    pub fn get_node_page(&mut self, options: NodePageOptions) -> crate::Result<NodePage> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        let page = options.page;
        let page_size = options.page_size;
        let wanted = (page + 1) * page_size;
        let mut total = 0usize;

        let query_orig;
        let q_match = if let Some(q) = options.query {
            if q.is_empty() {
                None
            } else {
                query_orig = q;
                Some(query_orig)
            }
        } else {
            None
        };

        let numeric_sort = match options.sort {
            SortField::SelfSize | SortField::Id | SortField::EdgeCount => true,
            SortField::Name | SortField::Type => false,
        };
        let need_name = !numeric_sort || q_match.is_some();

        let mut selected: Vec<(usize, HeapSnapshotNode)> = Vec::with_capacity(wanted);

        if numeric_sort && q_match.is_none() {
            let col = &raw.nodes;
            let key_of = |node_index: usize| -> u64 {
                match options.sort {
                    SortField::SelfSize => col.self_sizes[node_index] as u64,
                    SortField::Id => col.ids[node_index] as u64,
                    SortField::EdgeCount => col.edge_counts[node_index] as u64,
                    _ => unreachable!(),
                }
            };
            let keep_largest = matches!(options.dir, SortDir::Desc);
            let mut heap: std::collections::BinaryHeap<HeapItem> =
                std::collections::BinaryHeap::with_capacity(wanted + 1);

            for node_index in 0..raw.meta.node_count {
                if let Some(ft) = options.type_filter {
                    let type_idx = col.types[node_index] as usize;
                    let type_ = raw
                        .node_types
                        .get(type_idx)
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    if type_ != ft {
                        continue;
                    }
                }
                let key = key_of(node_index);
                let item = if keep_largest {
                    HeapItem {
                        value: key,
                        idx: node_index,
                    }
                } else {
                    HeapItem {
                        value: u64::MAX - key,
                        idx: node_index,
                    }
                };
                heap.push(item);
                if heap.len() > wanted {
                    heap.pop();
                }
                total += 1;
            }
            let mut kept: Vec<usize> = heap.into_iter().map(|h| h.idx).collect();
            kept.sort_by(|&a, &b| compare_node_indices(raw, a, b, options.sort, options.dir));
            let rows: Vec<usize> = kept
                .into_iter()
                .skip(page * page_size)
                .take(page_size)
                .collect();
            let nodes = rows
                .iter()
                .map(|&idx| Self::create_node_raw(raw, idx, true))
                .collect();
            return Ok(NodePage {
                total,
                page,
                page_size,
                nodes,
            });
        }

        let col = &raw.nodes;
        for node_index in 0..raw.meta.node_count {
            let type_idx = col.types[node_index] as usize;
            if let Some(ft) = options.type_filter {
                let t = raw
                    .node_types
                    .get(type_idx)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                if t != ft {
                    continue;
                }
            }
            let type_ = raw
                .node_types
                .get(type_idx)
                .cloned()
                .unwrap_or_else(|| type_idx.to_string());

            let name_idx = col.names[node_index] as usize;
            let name = if need_name {
                raw.strings
                    .resolve(&raw.mmap, name_idx)
                    .unwrap_or_else(|| format!("<string#{}>", name_idx))
            } else {
                String::new()
            };

            if let Some(ql) = q_match {
                if !strings::contains_ci(&name, ql) {
                    continue;
                }
            }

            let self_size = col.self_sizes[node_index] as usize;
            let id = col.ids[node_index] as usize;
            let edge_count = col.edge_counts[node_index] as usize;

            let node = HeapSnapshotNode {
                type_,
                name,
                self_size,
                retention_size: None,
                id,
                edge_count,
            };
            total += 1;

            let candidate = (node_index, node);
            if selected.len() < wanted {
                selected.push(candidate);
                continue;
            }

            let worst_idx = selected
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| compare_nodes(a, b, options.sort, options.dir))
                .map(|(i, _)| i)
                .unwrap();
            if compare_nodes(&candidate, &selected[worst_idx], options.sort, options.dir)
                == std::cmp::Ordering::Less
            {
                selected[worst_idx] = candidate;
            }
        }

        selected.sort_unstable_by(|a, b| compare_nodes(a, b, options.sort, options.dir));

        Ok(NodePage {
            total,
            page,
            page_size,
            nodes: selected
                .into_iter()
                .skip(page * page_size)
                .take(page_size)
                .map(|(_, n)| n)
                .collect(),
        })
    }

    fn create_node_raw(raw: &RawData, node_index: usize, with_name: bool) -> HeapSnapshotNode {
        let col = &raw.nodes;
        let type_idx = col.types[node_index] as usize;
        let name_idx = col.names[node_index] as usize;
        let self_size = col.self_sizes[node_index] as usize;
        let id = col.ids[node_index] as usize;
        let edge_count = col.edge_counts[node_index] as usize;

        let type_ = raw
            .node_types
            .get(type_idx)
            .cloned()
            .unwrap_or_else(|| type_idx.to_string());
        let name = if with_name {
            raw.strings
                .resolve(&raw.mmap, name_idx)
                .unwrap_or_else(|| format!("<string#{}>", name_idx))
        } else {
            String::new()
        };

        HeapSnapshotNode {
            type_,
            name,
            self_size,
            retention_size: None,
            id,
            edge_count,
        }
    }

    pub fn search_strings(&mut self, query: &str) -> crate::Result<Vec<SearchMatch>> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        Ok(raw
            .strings
            .search(&raw.mmap, query, 100)
            .into_iter()
            .map(|(index, value)| SearchMatch { index, value })
            .collect())
    }

    pub fn find_by_id(&mut self, id: usize) -> crate::Result<Option<usize>> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        let ids = &raw.nodes.ids;
        for n in 0..raw.meta.node_count {
            if ids[n] as usize == id {
                return Ok(Some(n));
            }
        }
        Ok(None)
    }

    pub fn node_field_count(&self) -> Option<usize> {
        self.raw.as_ref().map(|r| r.node_field_count)
    }

    pub fn edge_field_count(&self) -> Option<usize> {
        self.raw.as_ref().map(|r| r.edge_field_count)
    }

    pub fn to_flamegraph(
        &mut self,
        top: Option<usize>,
        filter: Option<&str>,
    ) -> crate::Result<FlamegraphFrame> {
        let summary = self.stream_summary(top.unwrap_or(50), filter)?;
        let total_size = summary.total_size;
        let _total_count = summary.total_count;

        let mut children: Vec<FlamegraphFrame> = Vec::new();
        let mut by_type: HashMap<String, Vec<(String, usize, usize)>> = HashMap::new();
        for (name, info) in &summary.by_node_name {
            by_type.entry("all".to_string()).or_default().push((
                name.clone(),
                info.size,
                info.count,
            ));
        }
        for (type_name, info) in &summary.by_node_type {
            by_type.entry(type_name.clone()).or_default().push((
                format!("<{}>", type_name),
                info.size,
                info.count,
            ));
        }

        for (type_name, names) in by_type {
            let mut type_children: Vec<FlamegraphFrame> = names
                .into_iter()
                .map(|(name, size, count)| FlamegraphFrame {
                    name: format!("{} (×{})", name, count),
                    self_size: size,
                    total_size: size,
                    children: Vec::new(),
                })
                .collect();
            type_children.sort_by(|a, b| b.total_size.cmp(&a.total_size));

            let type_total: usize = type_children.iter().map(|c| c.total_size).sum();
            children.push(FlamegraphFrame {
                name: type_name,
                self_size: 0,
                total_size: type_total,
                children: type_children,
            });
        }

        children.sort_by(|a, b| b.total_size.cmp(&a.total_size));

        Ok(FlamegraphFrame {
            name: "Heap".to_string(),
            self_size: 0,
            total_size,
            children,
        })
    }

    pub fn to_treemap(
        &mut self,
        top: Option<usize>,
        filter: Option<&str>,
    ) -> crate::Result<TreemapNode> {
        let summary = self.stream_summary(top.unwrap_or(50), filter)?;
        let total = summary.total_size;

        let mut root = TreemapNode {
            name: "Heap".to_string(),
            size: 0,
            children: Vec::new(),
        };

        let mut type_nodes: Vec<TreemapNode> = Vec::new();
        for (type_name, info) in &summary.by_node_type {
            let type_node = TreemapNode {
                name: type_name.clone(),
                size: info.size,
                children: Vec::new(),
            };
            type_nodes.push(type_node);
        }
        type_nodes.sort_by(|a, b| b.size.cmp(&a.size));

        let mut all_names = TreemapNode {
            name: "all".to_string(),
            size: 0,
            children: Vec::new(),
        };
        let mut name_nodes: Vec<TreemapNode> = summary
            .by_node_name
            .iter()
            .map(|(name, info)| TreemapNode {
                name: format!("{} (×{})", name, info.count),
                size: info.size,
                children: Vec::new(),
            })
            .collect();
        name_nodes.sort_by(|a, b| b.size.cmp(&a.size));
        all_names.children = name_nodes;
        all_names.size = all_names.children.iter().map(|c| c.size).sum();

        root.children = type_nodes;
        root.children.push(all_names);
        root.size = total;

        Ok(root)
    }

    pub fn diff(&mut self, baseline: &mut HeapSnapshot) -> crate::Result<SnapshotDiff> {
        let base = self.stream_summary(usize::MAX, None)?;
        let other = baseline.stream_summary(usize::MAX, None)?;

        Ok(SnapshotDiff {
            baseline_total: other.total_size,
            profile_total: base.total_size,
            delta_total: base.total_size as i64 - other.total_size as i64,
            by_node_name: diff::diff_snapshot_maps(&other.by_node_name, &base.by_node_name),
            by_node_type: diff::diff_snapshot_maps(&other.by_node_type, &base.by_node_type),
        })
    }

    pub fn object_diff(
        &mut self,
        baseline: &mut HeapSnapshot,
        limit: usize,
    ) -> crate::Result<SnapshotObjectDiff> {
        self.ensure_raw()?;
        baseline.ensure_raw()?;
        let profile_raw = self.raw.as_ref().unwrap();
        let baseline_raw = baseline.raw.as_ref().unwrap();
        Ok(diff::diff_snapshot_objects(
            baseline_raw,
            profile_raw,
            limit,
        ))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct HeapItem {
    value: u64,
    idx: usize,
}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .value
            .cmp(&self.value)
            .then_with(|| other.idx.cmp(&self.idx))
    }
}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_node_indices(
    raw: &RawData,
    a: usize,
    b: usize,
    sort: SortField,
    dir: SortDir,
) -> std::cmp::Ordering {
    let col = &raw.nodes;
    let num = |idx: usize| -> u64 {
        match sort {
            SortField::SelfSize => col.self_sizes[idx] as u64,
            SortField::Id => col.ids[idx] as u64,
            SortField::EdgeCount => col.edge_counts[idx] as u64,
            _ => 0,
        }
    };
    let cmp = num(a).cmp(&num(b)).then_with(|| a.cmp(&b));
    match dir {
        SortDir::Desc => cmp.reverse(),
        SortDir::Asc => cmp,
    }
}

fn compare_nodes(
    a: &(usize, HeapSnapshotNode),
    b: &(usize, HeapSnapshotNode),
    sort: SortField,
    dir: SortDir,
) -> std::cmp::Ordering {
    let cmp = match sort {
        SortField::Id => a.1.id.cmp(&b.1.id),
        SortField::Type => a.1.type_.cmp(&b.1.type_),
        SortField::Name => a.1.name.cmp(&b.1.name),
        SortField::EdgeCount => a.1.edge_count.cmp(&b.1.edge_count),
        SortField::SelfSize => a.1.self_size.cmp(&b.1.self_size),
    };
    let result = cmp.then_with(|| a.0.cmp(&b.0));
    match dir {
        SortDir::Desc => result.reverse(),
        SortDir::Asc => result,
    }
}

#[cfg(test)]
fn slice_summary(
    full: &HeapSnapshotSummary,
    top: usize,
    filter: Option<&str>,
) -> HeapSnapshotSummary {
    let filter_re = filter.and_then(|f| regex::Regex::new(f).ok());

    let mut names: Vec<(String, TypeSummary)> = full
        .by_node_name
        .iter()
        .filter(|(name, info)| {
            if info.size == 0 {
                return false;
            }
            match &filter_re {
                None => true,
                Some(re) => re.is_match(name.as_str()),
            }
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if top != usize::MAX && top < names.len() {
        let cmp = |a: &(String, TypeSummary), b: &(String, TypeSummary)| {
            b.1.size.cmp(&a.1.size).then_with(|| a.0.cmp(&b.0))
        };
        names.select_nth_unstable_by(top, cmp);
        names[..top].sort_unstable_by(cmp);
        names.truncate(top);
    } else {
        names.sort_by(|a, b| b.1.size.cmp(&a.1.size));
        if top != usize::MAX {
            names.truncate(top);
        }
    }

    HeapSnapshotSummary {
        total_size: full.total_size,
        total_count: full.total_count,
        by_node_name: names.into_iter().collect(),
        by_node_type: full.by_node_type.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_summary(size: usize, count: usize) -> TypeSummary {
        TypeSummary { size, count }
    }

    #[test]
    fn parse_numbers_fast_ignores_separators() {
        let data = b"0, 1,\n2,\t3,456 ,789";
        assert_eq!(parse_numbers_fast(data), vec![0, 1, 2, 3, 456, 789]);
    }

    #[test]
    fn parse_numbers_fast_stops_at_bracket() {
        let data = b"5, 7] 9, 11";
        assert_eq!(parse_numbers_fast(data), vec![5, 7]);
    }

    #[test]
    fn parse_numbers_par_matches_serial_across_chunk_boundaries() {
        let mut s = String::with_capacity(2 << 20);
        for i in 0..250_000u32 {
            s.push_str(&i.to_string());
            s.push(',');
        }
        let data = s.as_bytes();
        let fast = parse_numbers_fast(data);
        let par = parse_numbers_par(data);
        assert_eq!(par, fast);
        assert_eq!(par.len(), 250_000);
    }

    #[test]
    fn record_aligned_chunks_partition_at_record_boundaries() {
        for nfc in [1usize, 3, 6, 7] {
            let mut s = String::from("[");
            let records = 5000;
            for r in 0..records {
                for f in 0..nfc {
                    if !(r == 0 && f == 0) {
                        s.push(',');
                    }
                    s.push_str(&((r * 31 + f * 7 + 1) % 100000).to_string());
                }
            }
            s.push(']');
            let data = s.as_bytes();
            for chunks in [1usize, 4, 17, 64] {
                let ranges = parser::record_aligned_chunks(data, nfc, chunks);
                assert!(!ranges.is_empty(), "nfc={nfc} chunks={chunks}");
                assert_eq!(ranges[0].0, 0, "must start at 0");
                for w in ranges.windows(2) {
                    assert_eq!(w[0].1, w[1].0, "ranges must be contiguous");
                }
                assert_eq!(ranges.last().unwrap().1, data.len());
                for &(s, e) in &ranges {
                    let nums = parse_numbers_fast(&data[s..e]);
                    assert!(
                        nums.len().is_multiple_of(nfc),
                        "chunk not record-aligned: nfc={nfc} chunks={chunks} len={}",
                        nums.len()
                    );
                }
                let total: usize = ranges
                    .iter()
                    .map(|&(s, e)| parse_numbers_fast(&data[s..e]).len())
                    .sum();
                assert_eq!(total, records * nfc, "no number may be lost or duplicated");
            }
        }
    }

    #[test]
    fn record_aligned_chunks_rejects_none_on_misaligned_input() {
        let s = "1,2,3 4,5,6,7,8,9,10,11,12";
        let data = s.as_bytes();
        let ranges = parser::record_aligned_chunks(data, 3, 2);
        let all_aligned = ranges
            .iter()
            .all(|&(s, e)| parse_numbers_fast(&data[s..e]).len().is_multiple_of(3));
        assert!(
            !all_aligned,
            "space-separated numbers must break comma-counted alignment"
        );
    }

    #[test]
    fn node_columns_streaming_matches_reference() {
        let offsets = NodeFieldOffsets {
            type_: 0,
            name: 1,
            id: 2,
            self_size: 3,
            edge_count: 4,
            detachedness: Some(5),
        };
        let nfc = 6usize;
        let mut s = String::new();
        for r in 0..50_000u32 {
            for f in 0..nfc {
                if !(r == 0 && f == 0) {
                    s.push(',');
                }
                s.push_str(&match f {
                    0 => (r % 15).to_string(),
                    1 => (r % 70000).to_string(),
                    2 => (r * 13).to_string(),
                    3 => ((r * 37) % 4096).to_string(),
                    4 => (r % 30).to_string(),
                    _ => (r % 3).to_string(),
                });
            }
        }
        let data = s.as_bytes();
        let streamed = node_columns_streaming(data, nfc, &offsets).expect("canonical input");
        let flat = parse_numbers_par(data);
        let reference = NodeColumns {
            types: flat.par_chunks(nfc).map(|r| r[0] as u8).collect(),
            names: flat.par_chunks(nfc).map(|r| r[1]).collect(),
            ids: flat.par_chunks(nfc).map(|r| r[2]).collect(),
            self_sizes: flat.par_chunks(nfc).map(|r| r[3]).collect(),
            edge_counts: flat.par_chunks(nfc).map(|r| r[4]).collect(),
            detachedness: flat.par_chunks(nfc).map(|r| r[5] as u8).collect(),
        };
        assert_eq!(streamed.types, reference.types);
        assert_eq!(streamed.names, reference.names);
        assert_eq!(streamed.ids, reference.ids);
        assert_eq!(streamed.self_sizes, reference.self_sizes);
        assert_eq!(streamed.edge_counts, reference.edge_counts);
        assert_eq!(streamed.detachedness, reference.detachedness);
        assert_eq!(streamed.types.len(), 50_000);
    }

    #[test]
    fn decode_json_string_plain_and_escapes() {
        assert_eq!(decode_json_string(b"hello"), "hello");
        assert_eq!(
            decode_json_string(b"a\\nb\\t\\\\c\\\"d\\/e"),
            "a\nb\t\\c\"d/e"
        );
        assert_eq!(decode_json_string(b"\\u00410\\u0fff"), "A0\u{0fff}");
    }

    #[test]
    fn decode_json_string_surrogate_pair() {
        assert_eq!(
            decode_json_string("x\\uD83D\\uDE00y".as_bytes()),
            "x\u{1F600}y"
        );
    }

    #[test]
    fn contains_ci_matches_mixed_case_needles() {
        assert!(strings::contains_ci("new BABYLON.Vector3(0)", "vECTOR3"));
        assert!(strings::contains_ci("vector3", "VECTOR3"));
        assert!(strings::contains_ci("VECTOR3", "vector3"));
        assert!(strings::contains_ci("Vector3", "VeCtOr3"));
        assert!(strings::contains_ci("abc", ""));
        assert!(!strings::contains_ci("Vector", "vector3"));
        assert!(!strings::contains_ci("vec", "vector3"));
        assert!(strings::contains_ci("Функция (ФУНКЦИЯ)", "функция"));
        assert!(!strings::contains_ci("Функция", "вектор"));
    }

    #[test]
    fn eq_ci_ascii_and_unicode() {
        assert!(strings::eq_ci("RenderingGroup", "renderinggroup"));
        assert!(strings::eq_ci("object", "OBJECT"));
        assert!(!strings::eq_ci("object", "objects"));
        assert!(strings::eq_ci("Функция", "функция"));
        assert!(!strings::eq_ci("Функция", "вектор"));
        assert!(strings::eq_ci("", ""));
    }

    #[test]
    fn eq_ci_name_matches_display_suffix() {
        assert!(strings::eq_ci_name(
            "system / ExternalStringData",
            "ExternalStringData"
        ));
        assert!(strings::eq_ci_name(
            "system / ExternalStringData",
            "externalstringdata"
        ));
        assert!(strings::eq_ci_name(
            "ExternalStringData",
            "ExternalStringData"
        ));
        assert!(strings::eq_ci_name(
            "ExternalStringData",
            "externalstringdata"
        ));
        assert!(!strings::eq_ci_name(
            "system / InternalStringData",
            "ExternalStringData"
        ));
        assert!(!strings::eq_ci_name(
            "InternalStringData",
            "ExternalStringData"
        ));
        assert!(!strings::eq_ci_name(
            "system / ExternalStringDataExtra",
            "ExternalStringData"
        ));
    }

    #[test]
    fn string_table_skips_memo_for_large_strings() {
        let short = b"hello";
        let long_len = strings::MEMO_MAX_STRING_BYTES + 1;
        let mut data = Vec::with_capacity(short.len() + long_len);
        data.extend_from_slice(short);
        data.resize(short.len() + long_len, b'a');
        let path =
            std::env::temp_dir().join(format!("hprof_string_memo_{}.bin", std::process::id()));
        std::fs::write(&path, &data).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let mmap = unsafe { Mmap::map(&file).unwrap() };
        let table = StringTable::from_spans_for_test(
            0,
            vec![
                (0, short.len() as u32),
                (short.len() as u32, (short.len() + long_len) as u32),
            ],
        );
        assert_eq!(table.resolve(&mmap, 0).as_deref(), Some("hello"));
        assert!(table.memo.read().unwrap().contains_key(&0));
        let decoded = table.resolve(&mmap, 1).unwrap();
        assert_eq!(decoded.len(), long_len);
        assert!(!table.memo.read().unwrap().contains_key(&1));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn find_array_end_respects_nesting_and_string_content() {
        assert_eq!(find_array_end(b"[[1,2],[3,4]]\"x\"", 0), 12);
        let d = b"[\"a]\", \"b\\\"[\"]";
        assert_eq!(find_array_end(d, 0), 13);
    }

    #[test]
    fn find_matching_brace_nested_and_strings() {
        let data = br#"{"a":{"b":1},"c":"}"}"#;
        let start = 0usize;
        assert_eq!(
            find_matching_brace(std::str::from_utf8(data).unwrap(), start),
            Some(20)
        );
    }

    #[test]
    fn find_marker_basic() {
        assert_eq!(find_marker(b"xx\"nodes\":[yy", b"\"nodes\":["), Some(2));
        assert_eq!(find_marker(b"xxnode:[yy", b"\"nodes\":["), None);
    }

    #[test]
    fn slice_summary_selects_top_k_sorted_desc() {
        let mut full = HeapSnapshotSummary {
            total_size: 1000,
            total_count: 100,
            by_node_name: HashMap::new(),
            by_node_type: HashMap::new(),
        };
        for i in 0..100usize {
            full.by_node_name
                .insert(format!("n{i}"), type_summary(i, 1));
        }
        let sliced = slice_summary(&full, 5, None);
        assert_eq!(sliced.by_node_name.len(), 5);
        let mut sorted: Vec<_> = sliced.by_node_name.into_iter().collect();
        sorted.sort_by(|a, b| b.1.size.cmp(&a.1.size));
        assert_eq!(
            sorted.iter().map(|(_, t)| t.size).collect::<Vec<_>>(),
            vec![99, 98, 97, 96, 95]
        );
    }

    #[test]
    fn slice_summary_applies_filter() {
        let mut full = HeapSnapshotSummary {
            total_size: 1000,
            total_count: 100,
            by_node_name: HashMap::new(),
            by_node_type: HashMap::new(),
        };
        for i in 0..100usize {
            full.by_node_name
                .insert(format!("n{i}"), type_summary(i, 1));
        }
        let sliced = slice_summary(&full, usize::MAX, Some("n9[0-9]$"));
        assert_eq!(sliced.by_node_name.len(), 10);
        assert!(sliced.by_node_name.values().all(|t| t.size >= 90));
    }

    #[test]
    fn slice_summary_top_usize_max_returns_all() {
        let mut full = HeapSnapshotSummary {
            total_size: 10,
            total_count: 3,
            by_node_name: HashMap::new(),
            by_node_type: HashMap::new(),
        };
        full.by_node_name.insert("a".into(), type_summary(1, 1));
        full.by_node_name.insert("b".into(), type_summary(2, 1));
        let sliced = slice_summary(&full, usize::MAX, None);
        assert_eq!(sliced.by_node_name.len(), 2);
    }

    #[test]
    fn retained_analysis_handles_unreachable_nodes() {
        let json = r#"{"snapshot":{"meta":{
            "node_fields":["type","name","id","self_size","edge_count","detachedness"],
            "node_types":[["hidden","object","synthetic"],"string","number","number","number","number"],
            "edge_fields":["type","name_or_index","to_node"],
            "edge_types":[["internal","property","element"],"string_or_number","node"],
            "trace_function_info_fields":["function_id","name","script_name","script_id","line","column"],
            "trace_node_fields":["id","function_info_index","count","size","children"],
            "sample_fields":["timestamp_us","last_assigned_id"],
            "location_fields":["object_index","script_id","line","column"]
        },"node_count":4,"edge_count":4},
        "nodes":[2,0,0,0,2,0, 1,1,1,10,1,0, 1,2,2,10,1,0, 1,3,3,10,0,0],
        "edges":[0,0,6, 0,0,12, 0,0,12, 0,0,6],
        "strings":["","n1","n2","n3"]}"#;
        let path = std::env::temp_dir().join(format!(
            "hprof_unreachable_{}.heapsnapshot",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, json).unwrap();

        let mut snap = HeapSnapshot::new(path.to_string_lossy().into_owned());
        let res = snap.get_retained_entries(10).unwrap();
        assert!(!res.approximate);
        let by_index = |i: usize| res.retained.iter().find(|e| e.node_index == i);
        assert!(
            by_index(3).is_none(),
            "unreachable node must have retained == 0"
        );
        assert_eq!(by_index(1).unwrap().retained_size, 10);
        assert_eq!(by_index(2).unwrap().retained_size, 10);

        let _ = snap.retained_summary(usize::MAX, None).unwrap();
        let found = snap.search_nodes("n", 10).unwrap();
        assert!(!found.iter().any(|e| e.node_index == 3));
        let path_res = snap.shortest_path(3, 16).unwrap();
        assert!(!path_res.found, "node 3 is garbage — no path from root");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn object_diff_and_snapshot_reports_use_identity_and_detachedness() {
        let meta = r#"{"node_fields":["type","name","id","self_size","edge_count","detachedness"],"node_types":[["hidden","object"]],"edge_fields":["type","name_or_index","to_node"],"edge_types":[["property"]]}"#;
        let baseline_json = format!(
            r#"{{"snapshot":{{"meta":{meta},"node_count":3,"edge_count":0}},"nodes":[0,0,1,0,0,0,1,1,3,10,0,1,1,2,5,5,0,0],"edges":[],"strings":["","A","B"]}}"#
        );
        let profile_json = format!(
            r#"{{"snapshot":{{"meta":{meta},"node_count":3,"edge_count":0}},"nodes":[0,0,1,0,0,0,1,1,3,30,0,0,1,2,7,8,0,0],"edges":[],"strings":["","A","C"]}}"#
        );
        let base_path = std::env::temp_dir().join(format!(
            "hprof_object_diff_base_{}.heapsnapshot",
            std::process::id()
        ));
        let profile_path = std::env::temp_dir().join(format!(
            "hprof_object_diff_profile_{}.heapsnapshot",
            std::process::id()
        ));
        std::fs::write(&base_path, baseline_json).unwrap();
        std::fs::write(&profile_path, profile_json).unwrap();

        let mut baseline = HeapSnapshot::new(base_path.to_string_lossy().into_owned());
        let mut profile = HeapSnapshot::new(profile_path.to_string_lossy().into_owned());
        let diff = profile.object_diff(&mut baseline, 10).unwrap();
        assert_eq!(diff.matched_count, 2);
        assert_eq!(diff.new_count, 1);
        assert_eq!(diff.deleted_count, 1);
        assert_eq!(diff.delta_size, 23);
        assert_eq!(diff.grown_objects[0].id, 3);
        assert_eq!(diff.grown_objects[0].delta, 20);

        let detached = profile.detached_summary(10, 0).unwrap();
        assert_eq!(detached.total_count, 0);
        let histogram = profile.size_histogram().unwrap();
        assert_eq!(histogram.total_count, 3);
        assert_eq!(histogram.total_size, 38);
        let strings = profile.string_stats(10).unwrap();
        assert!(strings.entries.iter().any(|entry| entry.value == "A"));

        let _ = std::fs::remove_file(base_path);
        let _ = std::fs::remove_file(profile_path);
    }

    #[test]
    fn dominator_tree_chain_and_retained_sizes() {
        let json = r#"{"snapshot":{"meta":{
            "node_fields":["type","name","id","self_size","edge_count","detachedness"],
            "node_types":[["hidden","object","synthetic"],"string","number","number","number","number"],
            "edge_fields":["type","name_or_index","to_node"],
            "edge_types":[["property","element","weak"],"string_or_number","node"]
        },"node_count":4,"edge_count":3},
        "nodes":[2,0,0,0,1,0, 1,1,1,10,1,0, 1,2,2,20,1,0, 1,3,3,30,0,0],
        "edges":[0,0,6, 0,0,12, 0,0,18],
        "strings":["","n1","n2","n3"]}"#;

        let path = std::env::temp_dir().join(format!(
            "hprof_dom_chain_{}.heapsnapshot",
            std::process::id()
        ));
        std::fs::write(&path, json).unwrap();

        let mut snap = HeapSnapshot::new(path.to_string_lossy().into_owned());
        let res = snap.get_retained_entries(10).unwrap();
        assert!(!res.approximate);

        let by_index = |i: usize| {
            res.retained
                .iter()
                .find(|e| e.node_index == i)
                .map(|e| e.retained_size)
        };
        assert_eq!(by_index(1), Some(60));
        assert_eq!(by_index(2), Some(50));
        assert_eq!(by_index(3), Some(30));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn weak_edges_do_not_retain() {
        let json = r#"{"snapshot":{"meta":{
            "node_fields":["type","name","id","self_size","edge_count","detachedness"],
            "node_types":[["hidden","object","synthetic"],"string","number","number","number","number"],
            "edge_fields":["type","name_or_index","to_node"],
            "edge_types":[["property","element","weak"],"string_or_number","node"]
        },"node_count":3,"edge_count":2},
        "nodes":[2,0,0,0,1,0, 1,1,1,10,1,0, 1,2,2,20,0,0],
        "edges":[0,0,6, 2,0,12],
        "strings":["","n1","n2"]}"#;

        let path =
            std::env::temp_dir().join(format!("hprof_weak_{}.heapsnapshot", std::process::id()));
        std::fs::write(&path, json).unwrap();

        let mut snap = HeapSnapshot::new(path.to_string_lossy().into_owned());
        let res = snap.get_retained_entries(10).unwrap();

        let by_index = |i: usize| res.retained.iter().find(|e| e.node_index == i);
        assert!(by_index(2).is_none());
        assert_eq!(by_index(1).unwrap().retained_size, 10);

        let path_res = snap.shortest_path(2, 16).unwrap();
        assert!(!path_res.found);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn suspects_buffers_and_closures_reports() {
        let json = r#"{"snapshot":{"meta":{
            "node_fields":["type","name","id","self_size","edge_count","detachedness"],
            "node_types":[["hidden","object","closure","synthetic"],"string","number","number","number","number"],
            "edge_fields":["type","name_or_index","to_node"],
            "edge_types":[["property","context","weak"],"string_or_number","node"]
        },"node_count":4,"edge_count":3},
        "nodes":[3,0,0,0,2,0, 2,1,1,50,1,0, 1,2,2,200,0,0, 1,3,3,1000,0,0],
        "edges":[0,0,6, 0,0,18, 1,0,12],
        "strings":["","myClosure","Context","Uint8Array"]}"#;

        let path =
            std::env::temp_dir().join(format!("hprof_reports_{}.heapsnapshot", std::process::id()));
        std::fs::write(&path, json).unwrap();

        let mut snap = HeapSnapshot::new(path.to_string_lossy().into_owned());

        let suspects = snap.leak_suspects(10).unwrap();
        assert!(!suspects.suspects.is_empty());

        let buffers = snap.buffer_analysis(10).unwrap();
        assert_eq!(buffers.total_count, 1);
        assert_eq!(buffers.entries[0].name, "Uint8Array");
        assert_eq!(buffers.entries[0].self_size, 1000);

        let closures = snap.closure_analysis(10).unwrap();
        assert_eq!(closures.total_closures, 1);
        assert_eq!(closures.entries[0].name, "myClosure");

        let _ = std::fs::remove_file(&path);
    }
}
