use std::collections::HashMap;
use std::fs;
use std::io::Read;

use ahash::AHashMap;
use memchr::memchr;
use memmap2::Mmap;
use rayon::prelude::*;

use crate::types::*;

pub struct HeapSnapshot {
    file_path: String,
    meta: Option<SnapshotMeta>,
    raw: Option<RawData>,
    edge_starts: Option<Vec<u32>>,
    /// Cached full (top=MAX, no-filter) summary. Built once on first
    /// `stream_summary` call; subsequent calls slice from this. This is the
    /// single biggest perf win for snapshot analysis — avoids re-parsing the
    /// 1.5GB mmap on every diff / flamegraph / treemap call.
    full_summary: Option<HeapSnapshotSummary>,
}

pub struct RawData {
    pub meta: SnapshotMeta,
    pub nodes: Vec<u32>,
    pub edges: Vec<u32>,
    pub strings: Vec<String>,
    pub node_offsets: NodeFieldOffsets,
    pub edge_offsets: EdgeFieldOffsets,
    pub node_types: Vec<String>,
    pub edge_types: Vec<String>,
    pub node_field_count: usize,
    pub edge_field_count: usize,
}

impl HeapSnapshot {
    pub fn new(file_path: String) -> Self {
        Self {
            file_path,
            meta: None,
            raw: None,
            edge_starts: None,
            full_summary: None,
        }
    }

    pub fn meta(&mut self) -> crate::Result<&SnapshotMeta> {
        if self.meta.is_none() {
            self.parse_meta()?;
        }
        Ok(self.meta.as_ref().unwrap())
    }

    fn mmap_file(&self) -> crate::Result<Mmap> {
        let file = fs::File::open(&self.file_path)?;
        unsafe { Ok(Mmap::map(&file)?) }
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

    fn ensure_raw(&mut self) -> crate::Result<()> {
        if self.raw.is_some() {
            return Ok(());
        }

        let _ = self.meta()?;
        let meta = self.meta.as_ref().unwrap().clone();

        let mmap = self.mmap_file()?;
        let data = &mmap[..];

        let nodes_marker = b"\"nodes\":[";
        let edges_marker = b"\"edges\":[";
        let strings_marker = b"\"strings\":[";

        let nodes_start = find_marker(data, nodes_marker).ok_or(Error::HeaderParseFailed)?;
        let edges_start = find_marker(data, edges_marker).ok_or(Error::HeaderParseFailed)?;
        let strings_start = find_marker(data, strings_marker).ok_or(Error::HeaderParseFailed)?;

        let nodes = parse_numbers_fast(&data[nodes_start + nodes_marker.len()..]);
        let edges = parse_numbers_fast(&data[edges_start + edges_marker.len()..]);
        let strings = parse_strings_fast(&data[strings_start + strings_marker.len()..]);

        let node_offsets = NodeFieldOffsets::from_fields(&meta.meta.node_fields)?;
        let edge_offsets = EdgeFieldOffsets::from_fields(&meta.meta.edge_fields)?;
        let node_types = meta.meta.node_types.first().cloned().unwrap_or_default();
        let edge_types = meta.meta.edge_types.first().cloned().unwrap_or_default();
        let node_field_count = meta.meta.node_fields.len();
        let edge_field_count = meta.meta.edge_fields.len();

        self.raw = Some(RawData {
            meta,
            nodes,
            edges,
            strings,
            node_offsets,
            edge_offsets,
            node_types,
            edge_types,
            node_field_count,
            edge_field_count,
        });
        Ok(())
    }

    fn create_node(raw: &RawData, node_index: usize) -> HeapSnapshotNode {
        let base = node_index * raw.node_field_count;
        let type_idx = raw.nodes[base + raw.node_offsets.type_] as usize;
        let name_idx = raw.nodes[base + raw.node_offsets.name] as usize;
        let self_size = raw.nodes[base + raw.node_offsets.self_size] as usize;
        let id = raw.nodes[base + raw.node_offsets.id] as usize;
        let edge_count = raw.nodes[base + raw.node_offsets.edge_count] as usize;

        let type_ = raw
            .node_types
            .get(type_idx)
            .cloned()
            .unwrap_or_else(|| type_idx.to_string());
        let name = raw
            .strings
            .get(name_idx)
            .cloned()
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
        // Build the full (untruncated, unfiltered) summary once and cache it.
        // Subsequent calls — including those from `diff`, `to_flamegraph` and
        // `to_treemap` — slice from this cached value. For a 1.5GB snapshot
        // this turns a 6-second analysis into a 1-second one.
        if self.full_summary.is_none() {
            let full = self.compute_full_summary()?;
            self.full_summary = Some(full);
        }
        let full = self.full_summary.as_ref().unwrap();
        Ok(slice_summary(full, top, filter))
    }

    fn compute_full_summary(&mut self) -> crate::Result<HeapSnapshotSummary> {
        let _ = self.meta()?;
        let meta = self.meta.as_ref().unwrap();
        let node_fields = &meta.meta.node_fields;
        let node_types = meta
            .meta
            .node_types
            .first()
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let node_field_count = node_fields.len();
        let type_offset = node_fields
            .iter()
            .position(|f| f == "type")
            .ok_or(Error::UnsupportedLayout)?;
        let name_offset = node_fields
            .iter()
            .position(|f| f == "name")
            .ok_or(Error::UnsupportedLayout)?;
        let self_size_offset = node_fields
            .iter()
            .position(|f| f == "self_size")
            .ok_or(Error::UnsupportedLayout)?;

        let mmap = self.mmap_file()?;
        let data = &mmap[..];
        let nodes_marker = b"\"nodes\":[";
        let nodes_start = find_marker(data, nodes_marker).ok_or(Error::HeaderParseFailed)?;
        let nodes = parse_numbers_fast(&data[nodes_start + nodes_marker.len()..]);

        let total_node_count = nodes.len() / node_field_count;

        let chunks_per_thread = (total_node_count / rayon::current_num_threads().max(1)).max(1);
        let partials: Vec<(
            usize,
            usize,
            AHashMap<u32, (usize, usize, u32)>,
            AHashMap<u32, (usize, usize)>,
        )> = nodes
            .par_chunks(chunks_per_thread * node_field_count)
            .map(|chunk| {
                let mut local_size = 0usize;
                let mut local_count = 0usize;
                let mut local_by_name: AHashMap<u32, (usize, usize, u32)> = AHashMap::new();
                let mut local_by_type: AHashMap<u32, (usize, usize)> = AHashMap::new();

                for c in chunk.chunks(node_field_count) {
                    if c.len() < node_field_count {
                        break;
                    }
                    let type_idx = c[type_offset];
                    let name_idx = c[name_offset];
                    let self_size = c[self_size_offset] as usize;
                    local_count += 1;
                    local_size += self_size;

                    if self_size > 0 {
                        let e = local_by_name.entry(name_idx).or_insert((0, 0, type_idx));
                        e.0 += self_size;
                        e.1 += 1;
                        let te = local_by_type.entry(type_idx).or_insert((0, 0));
                        te.0 += self_size;
                        te.1 += 1;
                    }
                }
                (local_size, local_count, local_by_name, local_by_type)
            })
            .collect();

        let mut total_size = 0usize;
        let mut total_count = 0usize;
        let mut by_name_idx: AHashMap<u32, (usize, usize, u32)> = AHashMap::new();
        let mut by_type_idx: AHashMap<u32, (usize, usize)> = AHashMap::new();

        for (size, count, local_names, local_types) in partials {
            total_size += size;
            total_count += count;
            for (name_idx, (sz, cnt, type_idx)) in local_names {
                let e = by_name_idx.entry(name_idx).or_insert((0, 0, 0));
                e.0 += sz;
                e.1 += cnt;
                e.2 = type_idx;
            }
            for (type_idx, (sz, cnt)) in local_types {
                let e = by_type_idx.entry(type_idx).or_insert((0, 0));
                e.0 += sz;
                e.1 += cnt;
            }
        }

        let strings_marker = b"\"strings\":[";
        let strings_start = find_marker(data, strings_marker).ok_or(Error::HeaderParseFailed)?;
        let strings = parse_strings_fast(&data[strings_start + strings_marker.len()..]);

        // Build the full (untruncated, unfiltered) name and type maps. We keep
        // ALL names regardless of size, so subsequent slice_summary() can apply
        // any (top, filter) combination cheaply without re-parsing the mmap.
        let mut by_node_name: HashMap<String, TypeSummary> = HashMap::new();
        for (&name_idx, &(size, count, _type_idx)) in &by_name_idx {
            let name = strings
                .get(name_idx as usize)
                .cloned()
                .unwrap_or_else(|| format!("<string#{}>", name_idx));
            let entry = by_node_name
                .entry(name)
                .or_insert(TypeSummary { size: 0, count: 0 });
            entry.size += size;
            entry.count += count;
        }

        let mut by_node_type: HashMap<String, TypeSummary> = HashMap::new();
        for (&type_idx, &(size, count)) in &by_type_idx {
            let type_name = node_types
                .get(type_idx as usize)
                .cloned()
                .unwrap_or_else(|| type_idx.to_string());
            by_node_type.insert(type_name, TypeSummary { size, count });
        }

        Ok(HeapSnapshotSummary {
            total_size,
            total_count,
            by_node_name,
            by_node_type,
        })
    }

    pub fn get_node_page(&mut self, options: NodePageOptions) -> crate::Result<NodePage> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        let page = options.page;
        let page_size = options.page_size;
        let wanted = (page + 1) * page_size;
        let mut selected: Vec<(usize, HeapSnapshotNode)> = Vec::with_capacity(wanted);
        let mut total = 0usize;

        let query_lower;
        let q_match = if let Some(q) = options.query {
            if q.is_empty() {
                None
            } else {
                query_lower = q.to_lowercase();
                Some(&query_lower[..])
            }
        } else {
            None
        };

        for node_index in 0..raw.meta.node_count {
            let base = node_index * raw.node_field_count;
            let type_idx = raw.nodes[base + raw.node_offsets.type_] as usize;
            let type_ = raw
                .node_types
                .get(type_idx)
                .cloned()
                .unwrap_or_else(|| type_idx.to_string());

            if let Some(ft) = options.type_filter {
                if type_ != ft {
                    continue;
                }
            }

            let name_idx = raw.nodes[base + raw.node_offsets.name] as usize;
            let name = raw
                .strings
                .get(name_idx)
                .cloned()
                .unwrap_or_else(|| format!("<string#{}>", name_idx));

            if let Some(ql) = q_match {
                if !name.to_lowercase().contains(ql) {
                    continue;
                }
            }

            let self_size = raw.nodes[base + raw.node_offsets.self_size] as usize;
            let id = raw.nodes[base + raw.node_offsets.id] as usize;
            let edge_count = raw.nodes[base + raw.node_offsets.edge_count] as usize;

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

            if compare_nodes(&candidate, &selected[0], options.sort, options.dir)
                == std::cmp::Ordering::Less
            {
                selected[0] = candidate;
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

    pub fn get_node_edges(
        &mut self,
        node_index: usize,
    ) -> crate::Result<(HeapSnapshotNode, Vec<HeapSnapshotEdge>)> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        if node_index >= raw.meta.node_count {
            return Err(Error::NodeNotFound(node_index));
        }

        if self.edge_starts.is_none() {
            let mut starts = Vec::with_capacity(raw.meta.node_count + 1);
            let mut offset = 0u32;
            for i in 0..raw.meta.node_count {
                starts.push(offset);
                let base = i * raw.node_field_count;
                offset += raw.nodes[base + raw.node_offsets.edge_count];
            }
            starts.push(offset);
            self.edge_starts = Some(starts);
        }

        let node = Self::create_node(raw, node_index);
        let edge_starts = self.edge_starts.as_ref().unwrap();
        let edge_start = edge_starts[node_index] as usize;
        let edge_end = edge_starts[node_index + 1] as usize;

        let mut edges = Vec::with_capacity(edge_end - edge_start);
        for edge_index in edge_start..edge_end {
            let base = edge_index * raw.edge_field_count;
            let type_idx = raw.edges[base + raw.edge_offsets.type_] as usize;
            let name_or_index_val = raw.edges[base + raw.edge_offsets.name_or_index] as usize;
            let to_node =
                raw.edges[base + raw.edge_offsets.to_node] as usize / raw.node_field_count;

            let edge_type = raw
                .edge_types
                .get(type_idx)
                .cloned()
                .unwrap_or_else(|| type_idx.to_string());
            let name = if edge_type == "element" {
                EdgeName::Index(name_or_index_val)
            } else {
                raw.strings
                    .get(name_or_index_val)
                    .cloned()
                    .map(EdgeName::String)
                    .unwrap_or(EdgeName::Index(name_or_index_val))
            };

            edges.push(HeapSnapshotEdge {
                type_: edge_type,
                name_or_index: name,
                to_node,
            });
        }

        Ok((node, edges))
    }

    pub fn search_strings(&mut self, query: &str) -> crate::Result<Vec<SearchMatch>> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        let query_lower = query.to_lowercase();
        let mut matches_ = Vec::new();
        for (index, value) in raw.strings.iter().enumerate() {
            if value.to_lowercase().contains(&query_lower) {
                matches_.push(SearchMatch {
                    index,
                    value: value.clone(),
                });
                if matches_.len() >= 100 {
                    break;
                }
            }
        }
        Ok(matches_)
    }

    pub fn get_retained_entries(&mut self, top_n: usize) -> crate::Result<RetainedResult> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();

        if raw.meta.node_count > 5_000_000 {
            let mut selected: Vec<(usize, HeapSnapshotNode)> = Vec::with_capacity(top_n);
            for node_index in 0..raw.meta.node_count {
                let node = Self::create_node(raw, node_index);
                if selected.len() < top_n {
                    selected.push((node_index, node));
                    continue;
                }
                let worst_pos = selected
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, (_, n))| n.self_size)
                    .map(|(i, _)| i)
                    .unwrap();
                if node.self_size > selected[worst_pos].1.self_size {
                    selected[worst_pos] = (node_index, node);
                }
            }
            selected.sort_unstable_by(|a, b| b.1.self_size.cmp(&a.1.self_size));
            return Ok(RetainedResult {
                approximate: true,
                retained: selected
                    .into_iter()
                    .map(|(idx, node)| RetainedEntry {
                        node_index: idx,
                        name: node.name,
                        type_: node.type_,
                        self_size: node.self_size,
                        retained_size: node.self_size,
                        approximate: true,
                    })
                    .collect(),
            });
        }

        let retained = self.build_retained_sizes(raw)?;
        let mut indexed: Vec<(usize, f64)> = retained.into_iter().enumerate().collect();
        indexed.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        indexed.truncate(top_n);

        Ok(RetainedResult {
            approximate: false,
            retained: indexed
                .into_iter()
                .map(|(idx, size)| {
                    let node = Self::create_node(raw, idx);
                    RetainedEntry {
                        node_index: idx,
                        name: node.name,
                        type_: node.type_,
                        self_size: node.self_size,
                        retained_size: size as usize,
                        approximate: false,
                    }
                })
                .collect(),
        })
    }

    fn build_retained_sizes(&self, raw: &RawData) -> crate::Result<Vec<f64>> {
        let node_count = raw.meta.node_count;
        let mut edge_starts = vec![0u32; node_count + 1];
        let mut offset = 0u32;
        for i in 0..node_count {
            edge_starts[i] = offset;
            let base = i * raw.node_field_count;
            offset += raw.nodes[base + raw.node_offsets.edge_count];
        }
        edge_starts[node_count] = offset;

        let mut post_order = Vec::with_capacity(node_count);
        let mut visited = vec![false; node_count];
        let mut stack = vec![0i64];
        while let Some(node_idx) = stack.pop() {
            if node_idx < 0 {
                post_order.push((!node_idx) as usize);
                continue;
            }
            let node_idx = node_idx as usize;
            if visited[node_idx] {
                continue;
            }
            visited[node_idx] = true;
            stack.push(!(node_idx as i64));
            let start = edge_starts[node_idx] as usize;
            let end = edge_starts[node_idx + 1] as usize;
            for edge_index in start..end {
                let base = edge_index * raw.edge_field_count;
                let to_node =
                    raw.edges[base + raw.edge_offsets.to_node] as usize / raw.node_field_count;
                if to_node < node_count && !visited[to_node] {
                    stack.push(to_node as i64);
                }
            }
        }

        let mut idoms = vec![-1i32; node_count];
        idoms[0] = 0;

        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); node_count];
        for n in 0..node_count {
            let start = edge_starts[n] as usize;
            let end = edge_starts[n + 1] as usize;
            for edge_index in start..end {
                let base = edge_index * raw.edge_field_count;
                let to_node =
                    raw.edges[base + raw.edge_offsets.to_node] as usize / raw.node_field_count;
                if to_node < node_count {
                    preds[to_node].push(n);
                }
            }
        }

        fn intersect(idoms: &[i32], mut a: usize, mut b: usize) -> usize {
            while a != b {
                while a > b {
                    a = idoms[a] as usize;
                }
                while b > a {
                    b = idoms[b] as usize;
                }
            }
            a
        }

        let mut changed = true;
        while changed {
            changed = false;
            for &n in &post_order {
                if n == 0 {
                    continue;
                }
                let pred_list = &preds[n];
                if pred_list.is_empty() {
                    continue;
                }
                let mut new_idom: Option<usize> = None;
                for &p in pred_list {
                    if idoms[p] == -1 {
                        continue;
                    }
                    new_idom = Some(match new_idom {
                        None => p,
                        Some(cur) => intersect(&idoms, cur, p),
                    });
                }
                if let Some(nid) = new_idom {
                    if idoms[n] as usize != nid {
                        idoms[n] = nid as i32;
                        changed = true;
                    }
                }
            }
        }

        let mut retained = vec![0.0f64; node_count];
        for i in 0..node_count {
            let base = i * raw.node_field_count;
            retained[i] = raw.nodes[base + raw.node_offsets.self_size] as f64;
        }

        for &n in &post_order {
            if n == 0 {
                continue;
            }
            let dom = idoms[n] as usize;
            if dom < node_count {
                retained[dom] += retained[n];
            }
        }

        Ok(retained)
    }

    pub fn node_field_count(&self) -> Option<usize> {
        self.raw.as_ref().map(|r| r.node_field_count)
    }

    pub fn edge_field_count(&self) -> Option<usize> {
        self.raw.as_ref().map(|r| r.edge_field_count)
    }

    /// Build a flamegraph for a heap snapshot. We aggregate by node name
    /// (constructor name) at the root, and break down by type beneath.
    pub fn to_flamegraph(
        &mut self,
        top: Option<usize>,
        filter: Option<&str>,
    ) -> crate::Result<FlamegraphFrame> {
        let summary = self.stream_summary(top.unwrap_or(50), filter)?;
        let total_size = summary.total_size;
        let _total_count = summary.total_count;

        // Root: "Heap".
        let mut children: Vec<FlamegraphFrame> = Vec::new();

        // Group by type, then by name.
        let mut by_type: HashMap<String, Vec<(String, usize, usize)>> = HashMap::new();
        for (name, info) in &summary.by_node_name {
            // Find type for this name from by_node_type — we don't have a direct mapping,
            // so we'll just store under "(unknown)" if missing.
            // The TypeSummary in summary.by_node_name doesn't carry type, so just
            // attribute everything under "all".
            by_type.entry("all".to_string()).or_default().push((
                name.clone(),
                info.size,
                info.count,
            ));
        }
        // Also push type totals as top-level entries when name info isn't available.
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

    /// Build a treemap by node type -> node name.
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

        // Group node names under their type. We don't have a direct name->type map
        // (the summary structure doesn't carry it), so we use by_node_type as the
        // top-level grouping and put a synthetic "all" bucket for names.
        let mut type_nodes: Vec<TreemapNode> = Vec::new();
        for (type_name, info) in &summary.by_node_type {
            let type_node = TreemapNode {
                name: type_name.clone(),
                size: info.size,
                children: Vec::new(),
            };
            // Children: not directly mappable, so leave type-level only.
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

    /// Compute a diff between this snapshot (the "profile"/current) and a `baseline` snapshot.
    pub fn diff(&mut self, baseline: &mut HeapSnapshot) -> crate::Result<SnapshotDiff> {
        let base = self.stream_summary(usize::MAX, None)?;
        let other = baseline.stream_summary(usize::MAX, None)?;

        Ok(SnapshotDiff {
            baseline_total: other.total_size,
            profile_total: base.total_size,
            delta_total: base.total_size as i64 - other.total_size as i64,
            by_node_name: diff_snapshot_maps(&other.by_node_name, &base.by_node_name),
            by_node_type: diff_snapshot_maps(&other.by_node_type, &base.by_node_type),
        })
    }
}

fn diff_snapshot_maps(
    baseline: &HashMap<String, TypeSummary>,
    profile: &HashMap<String, TypeSummary>,
) -> Vec<DiffEntry> {
    let mut keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    keys.extend(baseline.keys().cloned());
    keys.extend(profile.keys().cloned());

    let mut entries: Vec<DiffEntry> = keys
        .into_iter()
        .map(|k| {
            let b = baseline.get(&k).map(|e| e.size).unwrap_or(0) as i64;
            let p = profile.get(&k).map(|e| e.size).unwrap_or(0) as i64;
            let delta = p - b;
            let delta_pct = if b == 0 {
                None
            } else {
                Some(delta as f64 / b as f64)
            };
            DiffEntry {
                name: k,
                baseline_size: b as usize,
                profile_size: p as usize,
                delta,
                delta_pct,
            }
        })
        .collect();
    entries.sort_by(|a, b| b.delta.abs().cmp(&a.delta.abs()));
    entries
}

/// Slice a (cached) full summary down to (top, filter). Avoids re-parsing the
/// snapshot mmap on repeated calls — turning a ~1 second operation into a
/// ~1 millisecond one.
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
    names.sort_by(|a, b| b.1.size.cmp(&a.1.size));
    if top != usize::MAX {
        names.truncate(top);
    }

    HeapSnapshotSummary {
        total_size: full.total_size,
        total_count: full.total_count,
        by_node_name: names.into_iter().collect(),
        by_node_type: full.by_node_type.clone(),
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

fn parse_meta_from_value(json: &str) -> crate::Result<SnapshotMeta> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let meta_obj = v.get("meta").ok_or(Error::HeaderParseFailed)?;
    let node_fields = meta_obj
        .get("node_fields")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let node_types = meta_obj
        .get("node_types")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|v| {
                    v.as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| match v {
                                    serde_json::Value::String(s) => Some(s.clone()),
                                    _ => None,
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();
    let edge_fields = meta_obj
        .get("edge_fields")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let edge_types = meta_obj
        .get("edge_types")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|v| {
                    v.as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| match v {
                                    serde_json::Value::String(s) => Some(s.clone()),
                                    _ => None,
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(SnapshotMeta {
        node_count: v.get("node_count").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        edge_count: v.get("edge_count").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        extra_native_bytes: v
            .get("extra_native_bytes")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize),
        meta: SnapshotMetaFields {
            node_fields,
            node_types,
            edge_fields,
            edge_types,
        },
    })
}

#[inline]
fn find_matching_brace(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for i in start..bytes.len() {
        let ch = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == b'\\' {
                escaped = true;
            } else if ch == b'"' {
                in_string = false;
            }
            continue;
        }
        if ch == b'"' {
            in_string = true;
            continue;
        }
        if ch == b'{' {
            depth += 1;
        } else if ch == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

#[inline]
pub fn find_marker(data: &[u8], marker: &[u8]) -> Option<usize> {
    if marker.len() > data.len() {
        return None;
    }
    let first = marker[0];
    let mut pos = 0;
    while pos <= data.len() - marker.len() {
        let found = memchr(first, &data[pos..])?;
        let abs = pos + found;
        if abs + marker.len() > data.len() {
            return None;
        }
        if &data[abs..abs + marker.len()] == marker {
            return Some(abs);
        }
        pos = abs + 1;
    }
    None
}

#[inline]
pub fn parse_numbers_fast(data: &[u8]) -> Vec<u32> {
    let mut numbers = Vec::with_capacity(data.len() / 4);
    let mut i = 0;
    let len = data.len();

    while i < len {
        let ch = data[i];
        if ch == b']' {
            break;
        }
        if ch.is_ascii_digit() {
            let mut val: u32 = 0;
            while i < len && data[i].is_ascii_digit() {
                val = val * 10 + (data[i] - b'0') as u32;
                i += 1;
            }
            numbers.push(val);
            continue;
        }
        i += 1;
    }
    numbers
}

#[inline]
fn parse_strings_fast(data: &[u8]) -> Vec<String> {
    let mut strings = Vec::with_capacity(65536);
    let mut i = 0;
    let len = data.len();

    while i < len {
        if data[i] == b']' {
            break;
        }
        if data[i] != b'"' {
            i += 1;
            continue;
        }
        i += 1;

        let mut buf: Vec<u8> = Vec::with_capacity(64);
        while i < len {
            let ch = data[i];
            if ch == b'\\' {
                i += 1;
                if i >= len {
                    break;
                }
                let decoded = match data[i] {
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'\\' => b'\\',
                    b'"' => b'"',
                    b'/' => b'/',
                    b'b' => 8,
                    b'f' => 12,
                    b'u' => {
                        if i + 4 < len {
                            let hex = &data[i + 1..i + 5];
                            let cp =
                                u32::from_str_radix(std::str::from_utf8(hex).unwrap_or("0000"), 16)
                                    .unwrap_or(0);
                            i += 4;
                            let mut tmp = [0u8; 4];
                            let s = std::char::from_u32(cp)
                                .unwrap_or('\u{FFFD}')
                                .encode_utf8(&mut tmp);
                            let len = s.len();
                            buf.extend_from_slice(&tmp[..len]);
                            i += 1;
                            continue;
                        }
                        data[i]
                    }
                    _ => data[i],
                };
                buf.push(decoded);
                i += 1;
                continue;
            }
            if ch == b'"' {
                i += 1;
                break;
            }
            buf.push(ch);
            i += 1;
        }

        strings.push(unsafe { String::from_utf8_unchecked(buf) });
    }

    strings
}
