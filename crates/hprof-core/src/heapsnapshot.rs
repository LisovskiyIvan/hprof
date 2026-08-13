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
    /// Cached dominator-tree analysis. Built on demand by `ensure_retained`
    /// and shared by `retained_summary`, `search_nodes` and `shortest_path`.
    retained_data: Option<RetainedData>,
    /// Cached first-incoming-edge (parent) map. Built on demand by
    /// `ensure_parent_map` and shared by `retainer_chain` and `owner_groups`.
    /// Cheaper than `retained_data` — a single pass over the edges, no
    /// dominator computation — so queries that only need "who owns this"
    /// never pay for Lengauer–Tarjan.
    parents: Option<ParentMap>,
}

/// Cached dominator-tree analysis (per-node retained sizes + reverse graph).
///
/// The reverse CSR and all dominator arrays are indexed by *preorder number*
/// (pre space) — that is what Lengauer–Tarjan works in, and the BFS in
/// `shortest_path` reuses the same index. `preorder`/`vertex` translate
/// between node indices and pre numbers.
///
/// `rev_starts`/`rev_sources` form a flat CSR of *incoming* edges: node with
/// pre `w` is referenced by sources `rev_sources[rev_starts[w]..rev_starts[w+1]]`.
/// One pass over the forward edges builds it, and it serves both the idom
/// iteration (predecessors) and the shortest-path BFS. The old per-node
/// `Vec<Vec<usize>>` predecessor table cost ~370MB for a 7.4M-node snapshot;
/// this flat layout costs ~124MB and is faster to build.
pub struct RetainedData {
    /// retained (self + dominated subtree) size per node, node space
    pub retained: Vec<f64>,
    /// incoming-edge CSR indexed by preorder number
    pub rev_starts: Vec<u32>,
    /// preorder numbers of the sources of incoming edges
    pub rev_sources: Vec<u32>,
    /// immediate dominator per node (node space); `u32::MAX` for nodes
    /// unreachable from root
    pub idoms: Vec<u32>,
    /// node index -> preorder number (`u32::MAX` when unreachable)
    pub preorder: Vec<u32>,
    /// preorder number -> node index
    pub vertex: Vec<u32>,
}

/// First incoming edge per node (the "owner" edge), in node order.
/// `index[n]` is the record index of the first node with an edge into `n`;
/// `edge_type[n]` / `edge_name_or_index[n]` describe that edge (`u32::MAX`
/// when `n` has no incoming edges).
pub struct ParentMap {
    pub index: Vec<u32>,
    pub edge_type: Vec<u32>,
    pub edge_name_or_index: Vec<u32>,
}

pub struct RawData {
    pub meta: SnapshotMeta,
    pub mmap: Mmap,
    pub nodes: Vec<u32>,
    /// byte range of the `edges` array content (start..=end), parsed on demand
    edges_range: Option<(usize, usize)>,
    pub edges: Option<Vec<u32>>,
    pub strings: StringTable,
    pub node_offsets: NodeFieldOffsets,
    pub edge_offsets: EdgeFieldOffsets,
    pub node_types: Vec<String>,
    pub edge_types: Vec<String>,
    pub node_field_count: usize,
    pub edge_field_count: usize,
}

/// Lazy string table: byte spans into the mmap, decoded on demand with a
/// memo so repeated resolutions (e.g. per-row node names) are cheap.
pub struct StringTable {
    /// offset into the mmap where the string content starts
    start: usize,
    /// per-string (start, end) relative to `start`, content still escaped
    spans: Vec<(u32, u32)>,
    memo: std::cell::RefCell<Vec<Option<String>>>,
}

impl StringTable {
    /// Scan the `"strings":[` array starting at `strings_start` (position of
    /// the `[`), recording the byte span of each string.
    fn scan(mmap: &Mmap, strings_start: usize) -> StringTable {
        let data = &mmap[..];
        let start = strings_start + 1; // past '['
        let end = data.len();
        let mut spans: Vec<(u32, u32)> = Vec::with_capacity(1 << 16);
        let mut i = start;
        while i < end {
            if data[i] != b'"' {
                if data[i] == b']' {
                    break;
                }
                i += 1;
                continue;
            }
            let s0 = i + 1;
            i += 1;
            while i < end {
                if data[i] == b'\\' {
                    i += 2;
                } else if data[i] == b'"' {
                    spans.push(((s0 - start) as u32, (i - start) as u32));
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        }
        let span_count = spans.len();
        StringTable {
            start,
            spans,
            memo: std::cell::RefCell::new(vec![None; span_count]),
        }
    }

    /// Decode string `idx` (escapes and surrogate pairs handled).
    pub fn resolve(&self, mmap: &Mmap, idx: usize) -> Option<String> {
        let memo = self.memo.borrow();
        if let Some(cached) = memo.get(idx).and_then(|s| s.as_ref()) {
            return Some(cached.clone());
        }
        drop(memo);
        let span = self.spans.get(idx).copied()?;
        let raw = &mmap[self.start + span.0 as usize..self.start + span.1 as usize];
        let decoded = decode_json_string(raw);
        self.memo.borrow_mut()[idx] = Some(decoded.clone());
        Some(decoded)
    }

    /// All strings containing `query` (case-insensitive), up to `limit`.
    pub fn search(&self, mmap: &Mmap, query: &str, limit: usize) -> Vec<(usize, String)> {
        let q = query.to_lowercase();
        let mut out = Vec::new();
        for idx in 0..self.spans.len() {
            if let Some(v) = self.resolve(mmap, idx) {
                if v.to_lowercase().contains(&q) {
                    out.push((idx, v));
                    if out.len() >= limit {
                        break;
                    }
                }
            }
        }
        out
    }
}

/// Decode one JSON string from `raw[start..end]` (escape sequences included).
pub(crate) fn decode_json_string(raw: &[u8]) -> String {
    let mut s: Vec<u8> = Vec::with_capacity(raw.len());
    let mut i = 0usize;
    let len = raw.len();
    while i < len {
        let c = raw[i];
        if c == b'\\' && i + 1 < len {
            match raw[i + 1] {
                b'"' => s.push(b'"'),
                b'\\' => s.push(b'\\'),
                b'/' => s.push(b'/'),
                b'b' => s.push(8),
                b'f' => s.push(12),
                b'n' => s.push(b'\n'),
                b'r' => s.push(b'\r'),
                b't' => s.push(b'\t'),
                b'u' => {
                    let hexv = |idx: usize| -> u16 {
                        let mut v: u16 = 0;
                        for &b in &raw[idx..idx + 4] {
                            v = v * 16
                                + match b {
                                    b'0'..=b'9' => (b - b'0') as u16,
                                    b'a'..=b'f' => (b - b'a' + 10) as u16,
                                    b'A'..=b'F' => (b - b'A' + 10) as u16,
                                    _ => 0,
                                };
                        }
                        v
                    };
                    let hi = i + 2;
                    let mut cp: u32 = hexv(hi) as u32;
                    i += 6;
                    if (0xD800..0xDC00).contains(&cp)
                        && i + 5 < len
                        && raw[i + 1] == b'\\'
                        && raw[i + 2] == b'u'
                    {
                        let lo = hexv(i + 3) as u32;
                        i += 6;
                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                    }
                    if let Some(ch) = char::from_u32(cp) {
                        let mut buf = [0u8; 4];
                        s.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    continue;
                }
                other => s.push(other),
            }
            i += 2;
            continue;
        }
        s.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&s).into_owned()
}

impl HeapSnapshot {
    pub fn new(file_path: String) -> Self {
        Self {
            file_path,
            meta: None,
            raw: None,
            edge_starts: None,
            full_summary: None,
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
        let strings_marker = b"\"strings\":[";

        let nodes_start = find_marker(data, nodes_marker).ok_or(Error::HeaderParseFailed)?;
        let strings_start = find_marker(data, strings_marker).ok_or(Error::HeaderParseFailed)?;

        // nodes are needed by every query; edges and strings are parsed
        // lazily (get_node_edges / name resolution) to keep memory down on
        // multi-GB snapshots. The region is bounded to the nodes array so the
        // parallel parser can split it at commas.
        let nodes_open = nodes_start + nodes_marker.len() - 1; // position of '['
        let nodes_end = find_array_end(data, nodes_open);
        let nodes = parse_numbers_par(&data[nodes_open + 1..=nodes_end]);

        let node_offsets = NodeFieldOffsets::from_fields(&meta.meta.node_fields)?;
        let edge_offsets = EdgeFieldOffsets::from_fields(&meta.meta.edge_fields)?;
        let node_types = meta.meta.node_types.first().cloned().unwrap_or_default();
        let edge_types = meta.meta.edge_types.first().cloned().unwrap_or_default();
        let node_field_count = meta.meta.node_fields.len();
        let edge_field_count = meta.meta.edge_fields.len();

        // the edges array is located lazily in ensure_edges — locating it
        // here would scan the whole (often ~700MB) edges array even for
        // queries that never touch edges

        // string table: scan byte spans lazily, decode on demand
        let strings_table = StringTable::scan(&mmap, strings_start + strings_marker.len() - 1);

        self.raw = Some(RawData {
            meta,
            mmap,
            nodes,
            edges_range: None,
            edges: None,
            strings: strings_table,
            node_offsets,
            edge_offsets,
            node_types,
            edge_types,
            node_field_count,
            edge_field_count,
        });
        Ok(())
    }

    fn ensure_edges(&mut self) -> crate::Result<()> {
        let needs = match self.raw.as_ref() {
            Some(raw) => raw.edges.is_none(),
            None => return Ok(()),
        };
        if !needs {
            return Ok(());
        }
        let edges = {
            let raw = self.raw.as_ref().unwrap();
            let (start, end) = match raw.edges_range {
                Some(r) => r,
                None => {
                    let edges_marker = b"\"edges\":[";
                    let start =
                        find_marker(&raw.mmap, edges_marker).ok_or(Error::HeaderParseFailed)?;
                    let open = start + edges_marker.len() - 1;
                    let end = find_array_end(&raw.mmap, open);
                    (open + 1, end)
                }
            };
            parse_numbers_par(&raw.mmap[start..=end])
        };
        self.raw.as_mut().unwrap().edges = Some(edges);
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
        // Reuse the already-parsed node records from ensure_raw instead of
        // re-parsing the mmap — in the analyze --retained flow the nodes
        // array would otherwise be parsed twice (~350MB of scanning).
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        let nodes = &raw.nodes;
        let node_field_count = raw.node_field_count;
        let type_offset = raw.node_offsets.type_;
        let name_offset = raw.node_offsets.name;
        let self_size_offset = raw.node_offsets.self_size;
        let node_types = raw.node_types.as_slice();
        let strings = &raw.strings;

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

        let table = strings;

        // Build the full (untruncated, unfiltered) name and type maps. We keep
        // ALL names regardless of size, so subsequent slice_summary() can apply
        // any (top, filter) combination cheaply without re-parsing the mmap.
        let mut by_node_name: HashMap<String, TypeSummary> = HashMap::new();
        for (&name_idx, &(size, count, _type_idx)) in &by_name_idx {
            let name = table
                .resolve(&raw.mmap, name_idx as usize)
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

    /// Fetch a single node by record index (name resolved). Used by the CLI
    /// `inspect --id` flow to render a node's details without pulling its
    /// edge list.
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

        // Numeric sorts (the common case: size / id / edge-count pages) select
        // with a bounded heap over plain numbers, so the loop stays
        // allocation-free. Name/type sorts and query filtering need the
        // strings resolved, so they take the slower path.
        let numeric_sort = match options.sort {
            SortField::SelfSize | SortField::Id | SortField::EdgeCount => true,
            SortField::Name | SortField::Type => false,
        };
        let need_name = !numeric_sort || q_match.is_some();

        let mut selected: Vec<(usize, HeapSnapshotNode)> = Vec::with_capacity(wanted);

        if numeric_sort {
            // keep the top-`wanted` by the numeric key
            let key_of = |node_index: usize| -> u64 {
                let base = node_index * raw.node_field_count;
                match options.sort {
                    SortField::SelfSize => raw.nodes[base + raw.node_offsets.self_size] as u64,
                    SortField::Id => raw.nodes[base + raw.node_offsets.id] as u64,
                    SortField::EdgeCount => raw.nodes[base + raw.node_offsets.edge_count] as u64,
                    _ => unreachable!(),
                }
            };
            // desc: keep largest (min-heap over the key) · asc: keep smallest
            let keep_largest = matches!(options.dir, SortDir::Desc);
            let mut heap: std::collections::BinaryHeap<HeapItem> =
                std::collections::BinaryHeap::with_capacity(wanted + 1);

            for node_index in 0..raw.meta.node_count {
                let base = node_index * raw.node_field_count;
                if let Some(ft) = options.type_filter {
                    let type_idx = raw.nodes[base + raw.node_offsets.type_] as usize;
                    let type_ = raw
                        .node_types
                        .get(type_idx)
                        .cloned()
                        .unwrap_or_else(|| type_idx.to_string());
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
            // materialize the kept nodes, resolve strings for the page rows
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
            let name = if need_name {
                raw.strings
                    .resolve(&raw.mmap, name_idx)
                    .unwrap_or_else(|| format!("<string#{}>", name_idx))
            } else {
                String::new()
            };

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

            // replace the current worst kept candidate if this one is better
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

    /// Build a node from a record index, resolving the name (used by the fast
    /// numeric-sort page path after selection).
    fn create_node_raw(raw: &RawData, node_index: usize, with_name: bool) -> HeapSnapshotNode {
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

    pub fn get_node_edges(
        &mut self,
        node_index: usize,
    ) -> crate::Result<(HeapSnapshotNode, Vec<HeapSnapshotEdge>)> {
        self.ensure_raw()?;
        self.ensure_edges()?;
        self.ensure_edge_starts();
        let raw = self.raw.as_ref().unwrap();
        if node_index >= raw.meta.node_count {
            return Err(Error::NodeNotFound(node_index));
        }

        let node = Self::create_node(raw, node_index);
        let edge_starts = self.edge_starts.as_ref().unwrap();
        let edge_start = edge_starts[node_index] as usize;
        let edge_end = edge_starts[node_index + 1] as usize;

        let mut edges = Vec::with_capacity(edge_end - edge_start);
        for edge_index in edge_start..edge_end {
            let edges_vec = raw.edges.as_ref().unwrap();
            let base = edge_index * raw.edge_field_count;
            let type_idx = edges_vec[base + raw.edge_offsets.type_] as usize;
            let name_or_index_val = edges_vec[base + raw.edge_offsets.name_or_index] as usize;
            let to_node =
                edges_vec[base + raw.edge_offsets.to_node] as usize / raw.node_field_count;

            let edge_type = raw
                .edge_types
                .get(type_idx)
                .cloned()
                .unwrap_or_else(|| type_idx.to_string());
            let name = if edge_type == "element" {
                EdgeName::Index(name_or_index_val)
            } else {
                raw.strings
                    .resolve(&raw.mmap, name_or_index_val)
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
        Ok(raw
            .strings
            .search(&raw.mmap, query, 100)
            .into_iter()
            .map(|(index, value)| SearchMatch { index, value })
            .collect())
    }

    /// Build (or reuse) the per-node edge-start index. `get_node_edges`,
    /// `ensure_retained` and path reconstruction all need it.
    fn ensure_edge_starts(&mut self) {
        if self.edge_starts.is_some() {
            return;
        }
        let raw = self.raw.as_ref().unwrap();
        let node_count = raw.meta.node_count;
        let mut starts = Vec::with_capacity(node_count + 1);
        let mut offset = 0u32;
        for i in 0..node_count {
            starts.push(offset);
            let base = i * raw.node_field_count;
            offset += raw.nodes[base + raw.node_offsets.edge_count];
        }
        starts.push(offset);
        self.edge_starts = Some(starts);
    }

    /// Compute (and cache) the dominator-tree analysis: per-node retained
    /// sizes plus a flat reverse (incoming-edge) CSR. Shared by
    /// `get_retained_entries`, `retained_summary`, `search_nodes` and
    /// `shortest_path`, so a CLI session that uses several of these pays the
    /// cost once. The reverse CSR doubles as the predecessor table for the
    /// idom iteration and as the traversal index for path BFS.
    pub fn ensure_retained(&mut self) -> crate::Result<()> {
        if self.retained_data.is_some() {
            return Ok(());
        }
        self.ensure_raw()?;
        self.ensure_edges()?;
        self.ensure_edge_starts();
        let raw = self.raw.as_ref().unwrap();
        let edge_starts = self.edge_starts.as_ref().unwrap();
        let node_count = raw.meta.node_count;

        // iterative DFS from root (node 0), assigning preorder numbers. The
        // DFS tree (dfs_parent) plus the reverse CSR feed Lengauer–Tarjan.
        let mut preorder = vec![u32::MAX; node_count];
        let mut vertex: Vec<u32> = Vec::with_capacity(node_count);
        let mut dfs_parent: Vec<u32> = Vec::with_capacity(node_count);
        // per-node cursor into its own edge range (edge_starts[v]..edge_starts[v+1])
        let mut next_edge: Vec<u32> = edge_starts[..node_count].to_vec();
        let mut pre = 0u32;
        preorder[0] = 0;
        vertex.push(0);
        dfs_parent.push(0);
        let mut stack: Vec<u32> = vec![0u32];
        while let Some(&v_node) = stack.last() {
            let end = edge_starts[v_node as usize + 1] as usize;
            let mut e = next_edge[v_node as usize] as usize;
            let mut descended = false;
            while e < end {
                let base = e * raw.edge_field_count;
                let to_node = raw.edges.as_ref().unwrap()[base + raw.edge_offsets.to_node] as usize
                    / raw.node_field_count;
                if to_node < node_count && preorder[to_node] == u32::MAX {
                    next_edge[v_node as usize] = (e + 1) as u32;
                    pre += 1;
                    preorder[to_node] = pre;
                    vertex.push(to_node as u32);
                    dfs_parent.push(preorder[v_node as usize]);
                    stack.push(to_node as u32);
                    descended = true;
                    break;
                }
                e += 1;
            }
            if !descended {
                stack.pop();
            }
        }

        // flat reverse CSR in pre space: node with pre `w` is referenced by
        // sources rev_sources[rev_starts[w]..rev_starts[w+1]] (pre numbers).
        // Sources unreachable from root cannot dominate anything and are
        // skipped.
        let mut counts = vec![0u32; node_count];
        for n in 0..node_count {
            let start = edge_starts[n] as usize;
            let end = edge_starts[n + 1] as usize;
            for edge_index in start..end {
                let base = edge_index * raw.edge_field_count;
                let to_node = raw.edges.as_ref().unwrap()[base + raw.edge_offsets.to_node] as usize
                    / raw.node_field_count;
                if to_node < node_count && preorder[to_node] != u32::MAX {
                    counts[preorder[to_node] as usize] += 1;
                }
            }
        }
        let mut rev_starts = vec![0u32; node_count + 1];
        for n in 0..node_count {
            rev_starts[n + 1] = rev_starts[n] + counts[n];
        }
        let mut cursor = rev_starts.clone();
        let mut rev_sources = vec![0u32; rev_starts[node_count] as usize];
        for n in 0..node_count {
            let start = edge_starts[n] as usize;
            let end = edge_starts[n + 1] as usize;
            for edge_index in start..end {
                let base = edge_index * raw.edge_field_count;
                let to_node = raw.edges.as_ref().unwrap()[base + raw.edge_offsets.to_node] as usize
                    / raw.node_field_count;
                if to_node < node_count && preorder[to_node] != u32::MAX {
                    let w = preorder[to_node] as usize;
                    rev_sources[cursor[w] as usize] = preorder[n];
                    cursor[w] += 1;
                }
            }
        }

        // Lengauer–Tarjan dominators (with path compression). All arrays are
        // indexed by preorder number; 0 = root. Runs in O(E · α(V)) — the
        // Cooper–Harvey–Kennedy refinement took ~74s on a 7.4M-node snapshot,
        // this finishes in seconds.
        // eval/compress with a caller-owned scratch buffer: compress is called
        // once per predecessor (~24M times on a 7.4M-node snapshot), and a
        // fresh Vec per call was measurable allocation churn.
        fn eval(
            v: u32,
            semi: &[u32],
            label: &mut [u32],
            ancestor: &mut [u32],
            scratch: &mut Vec<u32>,
        ) -> u32 {
            if ancestor[v as usize] == 0 {
                return label[v as usize];
            }
            compress(v, semi, label, ancestor, scratch);
            label[v as usize]
        }

        // Iterative compress (recursion could blow the stack on deep chains).
        fn compress(
            v: u32,
            semi: &[u32],
            label: &mut [u32],
            ancestor: &mut [u32],
            scratch: &mut Vec<u32>,
        ) {
            if ancestor[ancestor[v as usize] as usize] == 0 {
                return;
            }
            scratch.clear();
            let mut x = v;
            while ancestor[ancestor[x as usize] as usize] != 0 {
                scratch.push(x);
                x = ancestor[x as usize];
            }
            while let Some(y) = scratch.pop() {
                let a = ancestor[y as usize];
                if semi[label[a as usize] as usize] < semi[label[y as usize] as usize] {
                    label[y as usize] = label[a as usize];
                }
                ancestor[y as usize] = ancestor[a as usize];
            }
        }

        let n = node_count;
        let mut semi: Vec<u32> = (0..n as u32).collect();
        let mut label: Vec<u32> = (0..n as u32).collect();
        let mut ancestor = vec![0u32; n];
        let mut idom_pre = vec![u32::MAX; n];
        idom_pre[0] = 0;
        let mut bucket_head = vec![u32::MAX; n];
        let mut bucket_next = vec![u32::MAX; n];
        let mut scratch: Vec<u32> = Vec::with_capacity(16);

        for w in (1..n).rev() {
            let wu = w;
            let preds = &rev_sources[rev_starts[wu] as usize..rev_starts[wu + 1] as usize];
            for &v in preds {
                let u = eval(v, &semi, &mut label, &mut ancestor, &mut scratch);
                if semi[u as usize] < semi[wu] {
                    semi[wu] = semi[u as usize];
                }
            }
            let sw = semi[wu] as usize;
            bucket_next[wu] = bucket_head[sw];
            bucket_head[sw] = w as u32;
            ancestor[wu] = dfs_parent[wu];
            let p = dfs_parent[wu] as usize;
            let mut v = bucket_head[p];
            while v != u32::MAX {
                let vu = v as usize;
                let u = eval(v, &semi, &mut label, &mut ancestor, &mut scratch);
                idom_pre[vu] = if semi[u as usize] < semi[vu] {
                    u
                } else {
                    p as u32
                };
                v = bucket_next[vu];
            }
            bucket_head[p] = u32::MAX;
        }
        for w in 1..n {
            let wu = w;
            if idom_pre[wu] != semi[wu] {
                idom_pre[wu] = idom_pre[idom_pre[wu] as usize];
            }
        }

        // retained = self + dominated subtree, accumulated in pre space, then
        // mapped back to node space. Process in REVERSE preorder so children
        // (higher pre) are final before they are folded into their idom.
        let mut retained_pre = vec![0.0f64; n];
        for (w, &node) in vertex.iter().enumerate() {
            let base = node as usize * raw.node_field_count;
            retained_pre[w] = raw.nodes[base + raw.node_offsets.self_size] as f64;
        }
        for w in (1..vertex.len()).rev() {
            retained_pre[idom_pre[w] as usize] += retained_pre[w];
        }
        let mut retained = vec![0.0f64; node_count];
        for (w, &node) in vertex.iter().enumerate() {
            retained[node as usize] = retained_pre[w];
        }

        let mut idoms = vec![u32::MAX; node_count];
        for (w, &node) in vertex.iter().enumerate() {
            idoms[node as usize] = vertex[idom_pre[w] as usize];
        }

        self.retained_data = Some(RetainedData {
            retained,
            rev_starts,
            rev_sources,
            idoms,
            preorder,
            vertex,
        });
        Ok(())
    }

    pub fn get_retained_entries(&mut self, top_n: usize) -> crate::Result<RetainedResult> {
        self.ensure_raw()?;

        // Exact retained sizes need the full dominator computation (~1GB peak
        // on a 7.4M-node snapshot). For very large snapshots the UI keeps the
        // fast approximate top-by-self-size view so it stays responsive.
        if self.raw.as_ref().unwrap().meta.node_count > 5_000_000 {
            let raw = self.raw.as_ref().unwrap();
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

        self.ensure_retained()?;
        let raw = self.raw.as_ref().unwrap();
        let rd = self.retained_data.as_ref().unwrap();
        let mut indexed: Vec<(usize, usize)> = Vec::with_capacity(top_n);
        for (idx, &size) in rd.retained.iter().enumerate() {
            if size <= 0.0 {
                continue;
            }
            if indexed.len() < top_n {
                indexed.push((idx, size as usize));
                continue;
            }
            let mut worst = 0usize;
            for i in 1..indexed.len() {
                if indexed[i].1 < indexed[worst].1 {
                    worst = i;
                }
            }
            if size as usize > indexed[worst].1 {
                indexed[worst] = (idx, size as usize);
            }
        }
        indexed.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

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
                        retained_size: size,
                        approximate: false,
                    }
                })
                .collect(),
        })
    }

    /// Aggregate *exclusive* retained sizes by node name and type: a node's
    /// retained size is attributed to its name bucket only when its immediate
    /// dominator carries a different name (the same convention Chrome
    /// DevTools uses for the "Retained size by constructor" view). This makes
    /// the summary answer "what actually holds this memory", not just "what
    /// has big self size".
    pub fn retained_summary(
        &mut self,
        top: usize,
        filter: Option<&str>,
    ) -> crate::Result<HeapSnapshotSummary> {
        self.ensure_retained()?;
        // total sizes come from the cached full summary
        let _ = self.stream_summary(usize::MAX, None)?;
        let full = self.full_summary.as_ref().unwrap();
        let raw = self.raw.as_ref().unwrap();
        let rd = self.retained_data.as_ref().unwrap();
        let node_count = raw.meta.node_count;

        // node 0 is the synthetic root (name "", type "synthetic"); its
        // retained is the whole heap plus exclusive double-counts, which
        // is pure noise in a "by constructor" view. DevTools shows the
        // "(GC roots)" bucket instead, which is real.
        let mut by_name: AHashMap<u32, (usize, usize)> = AHashMap::new();
        let mut by_type: AHashMap<u32, (usize, usize)> = AHashMap::new();
        // capture only Sync fields — RawData contains a RefCell string memo
        let nodes = &raw.nodes;
        let nfc = raw.node_field_count;
        let name_off = raw.node_offsets.name;
        let type_off = raw.node_offsets.type_;
        let retained = &rd.retained;
        let idoms = &rd.idoms;
        let chunks_per_thread = (node_count / rayon::current_num_threads().max(1)).max(1);
        let partials: Vec<(AHashMap<u32, (usize, usize)>, AHashMap<u32, (usize, usize)>)> = (1
            ..node_count)
            .into_par_iter()
            .chunks(chunks_per_thread)
            .map(|chunk| {
                let mut local_name: AHashMap<u32, (usize, usize)> = AHashMap::new();
                let mut local_type: AHashMap<u32, (usize, usize)> = AHashMap::new();
                for n in chunk {
                    let retained = retained[n] as usize;
                    if retained == 0 {
                        continue;
                    }
                    let base = n * nfc;
                    let name_idx = nodes[base + name_off];
                    let type_idx = nodes[base + type_off];
                    let dom = idoms[n] as usize;
                    let (dom_name, dom_type) = if dom < node_count && dom != n {
                        let dbase = dom * nfc;
                        (nodes[dbase + name_off], nodes[dbase + type_off])
                    } else {
                        (u32::MAX, u32::MAX)
                    };
                    if dom_name != name_idx {
                        let e = local_name.entry(name_idx).or_insert((0, 0));
                        e.0 += retained;
                        e.1 += 1;
                    }
                    if dom_type != type_idx {
                        let e = local_type.entry(type_idx).or_insert((0, 0));
                        e.0 += retained;
                        e.1 += 1;
                    }
                }
                (local_name, local_type)
            })
            .collect();
        for (local_name, local_type) in partials {
            for (idx, (sz, cnt)) in local_name {
                let e = by_name.entry(idx).or_insert((0, 0));
                e.0 += sz;
                e.1 += cnt;
            }
            for (idx, (sz, cnt)) in local_type {
                let e = by_type.entry(idx).or_insert((0, 0));
                e.0 += sz;
                e.1 += cnt;
            }
        }

        let filter_re = filter.and_then(|f| regex::Regex::new(f).ok());
        let mut names: Vec<(String, TypeSummary)> = by_name
            .into_iter()
            .filter_map(|(idx, (size, count))| {
                if size == 0 {
                    return None;
                }
                let name = raw
                    .strings
                    .resolve(&raw.mmap, idx as usize)
                    .unwrap_or_else(|| format!("<string#{}>", idx));
                if let Some(re) = &filter_re {
                    if !re.is_match(&name) {
                        return None;
                    }
                }
                Some((name, TypeSummary { size, count }))
            })
            .collect();
        names.sort_by(|a, b| b.1.size.cmp(&a.1.size));
        if top != usize::MAX {
            names.truncate(top);
        }

        let mut types: Vec<(String, TypeSummary)> = by_type
            .into_iter()
            .filter_map(|(idx, (size, count))| {
                if size == 0 {
                    return None;
                }
                let type_name = raw
                    .node_types
                    .get(idx as usize)
                    .cloned()
                    .unwrap_or_else(|| idx.to_string());
                if let Some(re) = &filter_re {
                    if !re.is_match(&type_name) {
                        return None;
                    }
                }
                Some((type_name, TypeSummary { size, count }))
            })
            .collect();
        types.sort_by(|a, b| b.1.size.cmp(&a.1.size));
        if top != usize::MAX {
            types.truncate(top);
        }

        Ok(HeapSnapshotSummary {
            total_size: full.total_size,
            total_count: full.total_count,
            by_node_name: names.into_iter().collect(),
            by_node_type: types.into_iter().collect(),
        })
    }

    /// Top instances whose (case-insensitive) name contains `query`, ranked by
    /// retained size. Powers `hprof inspect --name`.
    pub fn search_nodes(&mut self, query: &str, top: usize) -> crate::Result<Vec<RetainedEntry>> {
        self.ensure_retained()?;
        let raw = self.raw.as_ref().unwrap();
        let rd = self.retained_data.as_ref().unwrap();
        let q = query.to_lowercase();

        // Pre-resolve every distinct node name once (the shared string memo
        // is a RefCell, so parallel chunks decode through their own cache —
        // which would re-copy 3.6MB source maps per chunk). Then scan in
        // parallel, reading only the Sync cache.
        let nodes = &raw.nodes;
        let nfc = raw.node_field_count;
        let name_off = raw.node_offsets.name;
        let mut distinct: Vec<u32> = {
            let mut set: std::collections::HashSet<u32> = std::collections::HashSet::new();
            set.reserve(raw.meta.node_count / 16);
            for n in 0..raw.meta.node_count {
                set.insert(nodes[n * nfc + name_off]);
            }
            set.into_iter().collect()
        };
        let mut name_cache: HashMap<u32, String> = HashMap::with_capacity(distinct.len());
        for idx in distinct.drain(..) {
            let lower = raw
                .strings
                .resolve(&raw.mmap, idx as usize)
                .unwrap_or_default()
                .to_lowercase();
            name_cache.insert(idx, lower);
        }

        let retained = &rd.retained;
        let chunks_per_thread = (raw.meta.node_count / rayon::current_num_threads().max(1)).max(1);
        let parts: Vec<Vec<(usize, usize)>> = (0..raw.meta.node_count)
            .into_par_iter()
            .chunks(chunks_per_thread)
            .map(|chunk| {
                let mut found: Vec<(usize, usize)> = Vec::new();
                for n in chunk {
                    let base = n * nfc;
                    let name_idx = nodes[base + name_off];
                    let lower = &name_cache[&name_idx];
                    if !lower.contains(&q) {
                        continue;
                    }
                    let retained = retained[n] as usize;
                    if retained == 0 {
                        continue;
                    }
                    found.push((n, retained));
                }
                found
            })
            .collect();
        let mut found: Vec<(usize, usize)> = parts.into_iter().flatten().collect();
        found.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        found.truncate(top);

        Ok(found
            .into_iter()
            .map(|(idx, retained)| {
                let node = Self::create_node_raw(raw, idx, true);
                RetainedEntry {
                    node_index: idx,
                    name: node.name,
                    type_: node.type_,
                    self_size: node.self_size,
                    retained_size: retained,
                    approximate: false,
                }
            })
            .collect())
    }

    /// Resolve a DevTools node id to a record index (single scan; ids are not
    /// stored in index order).
    pub fn find_by_id(&mut self, id: usize) -> crate::Result<Option<usize>> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        let id_offset = raw.node_offsets.id;
        for n in 0..raw.meta.node_count {
            let base = n * raw.node_field_count;
            if raw.nodes[base + id_offset] as usize == id {
                return Ok(Some(n));
            }
        }
        Ok(None)
    }

    /// Retained size of a single node (drives `inspect --id` details). Forces
    /// the dominator computation when it has not run yet.
    pub fn retained_size_of(&mut self, node_index: usize) -> crate::Result<usize> {
        self.ensure_retained()?;
        let raw = self.raw.as_ref().unwrap();
        let rd = self.retained_data.as_ref().unwrap();
        if node_index >= raw.meta.node_count {
            return Err(Error::NodeNotFound(node_index));
        }
        Ok(rd.retained[node_index] as usize)
    }

    /// The edge from `from` to `to` (bounded scan of `from`'s forward edges),
    /// as (edge_type, property name / "[i]").
    fn path_edge(
        &self,
        raw: &RawData,
        edge_starts: &[u32],
        from: usize,
        to: usize,
    ) -> Option<(String, String)> {
        let start = edge_starts[from] as usize;
        let end = edge_starts[from + 1] as usize;
        for edge_index in start..end {
            let base = edge_index * raw.edge_field_count;
            let to_node = raw.edges.as_ref().unwrap()[base + raw.edge_offsets.to_node] as usize
                / raw.node_field_count;
            if to_node != to {
                continue;
            }
            let type_idx = raw.edges.as_ref().unwrap()[base + raw.edge_offsets.type_] as usize;
            let type_ = raw
                .edge_types
                .get(type_idx)
                .cloned()
                .unwrap_or_else(|| type_idx.to_string());
            let name_or_index =
                raw.edges.as_ref().unwrap()[base + raw.edge_offsets.name_or_index] as usize;
            let name = if type_ == "element" {
                name_or_index.to_string()
            } else {
                raw.strings
                    .resolve(&raw.mmap, name_or_index)
                    .unwrap_or_else(|| format!("#{}", name_or_index))
            };
            return Some((type_, name));
        }
        None
    }

    /// Shortest path from the GC root to `node_index`, following incoming
    /// edges (BFS over the reverse CSR). Returns `found: false` when the node
    /// is unreachable from the root within `max_depth` hops.
    pub fn shortest_path(
        &mut self,
        node_index: usize,
        max_depth: usize,
    ) -> crate::Result<ShortestPath> {
        self.ensure_retained()?;
        let raw = self.raw.as_ref().unwrap();
        let rd = self.retained_data.as_ref().unwrap();
        let node_count = raw.meta.node_count;
        if node_index >= node_count {
            return Err(Error::NodeNotFound(node_index));
        }
        let target_pre = rd.preorder[node_index];
        if target_pre == u32::MAX {
            // unreachable from root — no path exists
            return Ok(ShortestPath {
                found: false,
                nodes: Vec::new(),
                edges: Vec::new(),
            });
        }

        // BFS from the target over incoming edges (pre space) until root.
        let n = node_count;
        let mut parent: Vec<u32> = vec![u32::MAX; n];
        let mut depth = vec![0u32; n];
        parent[target_pre as usize] = target_pre;
        let mut queue: Vec<u32> = vec![target_pre];
        let mut head = 0usize;
        let mut found = false;
        while head < queue.len() {
            let w = queue[head] as usize;
            head += 1;
            if w == 0 {
                found = true;
                break;
            }
            let d = depth[w];
            if d >= max_depth as u32 {
                continue;
            }
            for &s in &rd.rev_sources[rd.rev_starts[w] as usize..rd.rev_starts[w + 1] as usize] {
                let s = s as usize;
                if parent[s] == u32::MAX {
                    parent[s] = w as u32;
                    depth[s] = d + 1;
                    queue.push(s as u32);
                }
            }
        }

        if !found {
            return Ok(ShortestPath {
                found: false,
                nodes: Vec::new(),
                edges: Vec::new(),
            });
        }

        // reconstruct: root -> ... -> target via parent pointers (pre space),
        // mapped to node indices
        let mut pres: Vec<usize> = vec![0usize];
        let mut cur = 0usize;
        while cur != target_pre as usize {
            cur = parent[cur] as usize;
            pres.push(cur);
        }

        let edge_starts = self.edge_starts.as_ref().unwrap();
        let nodes = pres
            .iter()
            .map(|&w| {
                let i = rd.vertex[w] as usize;
                let node = Self::create_node_raw(raw, i, true);
                PathNode {
                    index: i,
                    id: node.id,
                    name: node.name,
                    type_: node.type_,
                    self_size: node.self_size,
                }
            })
            .collect();
        let mut edges = Vec::with_capacity(pres.len().saturating_sub(1));
        for w in pres.windows(2) {
            let from = rd.vertex[w[0]] as usize;
            let to = rd.vertex[w[1]] as usize;
            edges.push(
                self.path_edge(raw, edge_starts, from, to)
                    .map(|(type_, name)| PathEdge { type_, name })
                    .unwrap_or_else(|| PathEdge {
                        type_: String::new(),
                        name: String::new(),
                    }),
            );
        }

        Ok(ShortestPath {
            found: true,
            nodes,
            edges,
        })
    }

    /// Build (or reuse) the first-incoming-edge map. One pass over the
    /// forward edges; the first edge into a node wins (matches the classic
    /// (object elements) owner walk). O(E), no dominator analysis.
    fn ensure_parent_map(&mut self) -> crate::Result<()> {
        if self.parents.is_some() {
            return Ok(());
        }
        self.ensure_raw()?;
        self.ensure_edges()?;
        self.ensure_edge_starts();
        let raw = self.raw.as_ref().unwrap();
        let edge_starts = self.edge_starts.as_ref().unwrap();
        let node_count = raw.meta.node_count;
        let nfc = raw.node_field_count;
        let edges = raw.edges.as_ref().unwrap();

        let mut index = vec![u32::MAX; node_count];
        let mut edge_type = vec![u32::MAX; node_count];
        let mut edge_name_or_index = vec![u32::MAX; node_count];
        for n in 0..node_count {
            let start = edge_starts[n] as usize;
            let end = edge_starts[n + 1] as usize;
            for edge_index in start..end {
                let base = edge_index * raw.edge_field_count;
                let to_node = edges[base + raw.edge_offsets.to_node] as usize / nfc;
                if to_node < node_count && index[to_node] == u32::MAX {
                    index[to_node] = n as u32;
                    edge_type[to_node] = edges[base + raw.edge_offsets.type_];
                    edge_name_or_index[to_node] = edges[base + raw.edge_offsets.name_or_index];
                }
            }
        }
        self.parents = Some(ParentMap {
            index,
            edge_type,
            edge_name_or_index,
        });
        Ok(())
    }

    /// Find nodes by name (exact or substring) with optional self-size and
    /// node-type filters. Unlike `search_nodes` this does NOT require the
    /// dominator analysis — it is a plain parallel scan, returns every match
    /// (including nodes with `retained == 0`), and ranks by self size.
    pub fn find_nodes(&mut self, query: &NameQuery) -> crate::Result<Vec<NameMatch>> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        let q = query.name.to_lowercase();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let nodes = &raw.nodes;
        let nfc = raw.node_field_count;
        let name_off = raw.node_offsets.name;
        let type_off = raw.node_offsets.type_;
        let self_off = raw.node_offsets.self_size;
        let id_off = raw.node_offsets.id;
        let edge_count_off = raw.node_offsets.edge_count;

        // Pre-resolve every distinct node name once (the shared string memo
        // is a RefCell, so parallel chunks decode through their own cache).
        let mut distinct: std::collections::HashSet<u32> = std::collections::HashSet::new();
        distinct.reserve(raw.meta.node_count / 16);
        for n in 0..raw.meta.node_count {
            distinct.insert(nodes[n * nfc + name_off]);
        }
        // store lowercased names so exact/substring comparisons against the
        // lowercased query are case-insensitive (search_nodes does the same)
        let mut name_cache: HashMap<u32, String> = HashMap::with_capacity(distinct.len());
        for idx in distinct.drain() {
            let lower = raw
                .strings
                .resolve(&raw.mmap, idx as usize)
                .unwrap_or_default()
                .to_lowercase();
            name_cache.insert(idx, lower);
        }

        let exact = query.exact;
        let min_self = query.min_self;
        let type_filter = query.type_filter.as_ref().map(|t| t.to_lowercase());
        let limit = query.limit;
        // read-only slices for the parallel closure (RawData is !Sync — its
        // string memo is a RefCell — so only Sync fields may be captured)
        let node_types = raw.node_types.as_slice();

        let chunks_per_thread = (raw.meta.node_count / rayon::current_num_threads().max(1)).max(1);
        let parts: Vec<Vec<(usize, usize)>> = (0..raw.meta.node_count)
            .into_par_iter()
            .chunks(chunks_per_thread)
            .map(|chunk| {
                let mut found: Vec<(usize, usize)> = Vec::new();
                for n in chunk {
                    let base = n * nfc;
                    let name = &name_cache[&nodes[base + name_off]];
                    let matches = if exact { name == &q } else { name.contains(&q) };
                    if !matches {
                        continue;
                    }
                    let self_size = nodes[base + self_off] as usize;
                    if self_size < min_self {
                        continue;
                    }
                    if let Some(tf) = &type_filter {
                        let t = node_types
                            .get(nodes[base + type_off] as usize)
                            .map(|s| s.to_lowercase())
                            .unwrap_or_default();
                        if &t != tf {
                            continue;
                        }
                    }
                    found.push((n, self_size));
                }
                found
            })
            .collect();
        let mut found: Vec<(usize, usize)> = parts.into_iter().flatten().collect();
        found.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        if limit > 0 && found.len() > limit {
            found.truncate(limit);
        }

        Ok(found
            .into_iter()
            .map(|(idx, _)| {
                let base = idx * nfc;
                let type_idx = nodes[base + type_off] as usize;
                let name_idx = nodes[base + name_off] as usize;
                NameMatch {
                    node_index: idx,
                    id: nodes[base + id_off] as usize,
                    name: raw.strings.resolve(&raw.mmap, name_idx).unwrap_or_default(),
                    type_: raw
                        .node_types
                        .get(type_idx)
                        .cloned()
                        .unwrap_or_else(|| type_idx.to_string()),
                    self_size: nodes[base + self_off] as usize,
                    edge_count: nodes[base + edge_count_off] as usize,
                }
            })
            .collect())
    }

    /// Resolve a node's edges into displayable properties: primitive values
    /// (numbers, strings) are inlined, object values become a reference to
    /// the target node. This is what lets you read e.g. `renderingGroupId`
    /// of a GPUParticleSystem without walking value nodes by hand.
    pub fn get_node_properties(
        &mut self,
        node_index: usize,
    ) -> crate::Result<(HeapSnapshotNode, Vec<NodeProperty>)> {
        self.ensure_raw()?;
        self.ensure_edges()?;
        self.ensure_edge_starts();
        let raw = self.raw.as_ref().unwrap();
        if node_index >= raw.meta.node_count {
            return Err(Error::NodeNotFound(node_index));
        }

        let node = Self::create_node(raw, node_index);
        let edge_starts = self.edge_starts.as_ref().unwrap();
        let start = edge_starts[node_index] as usize;
        let end = edge_starts[node_index + 1] as usize;
        let edges = raw.edges.as_ref().unwrap();
        let nodes = &raw.nodes;
        let nfc = raw.node_field_count;

        let mut props = Vec::with_capacity(end - start);
        for edge_index in start..end {
            let base = edge_index * raw.edge_field_count;
            let type_idx = edges[base + raw.edge_offsets.type_] as usize;
            let edge_type = raw
                .edge_types
                .get(type_idx)
                .cloned()
                .unwrap_or_else(|| type_idx.to_string());
            let name_or_index = edges[base + raw.edge_offsets.name_or_index] as usize;
            let to_node = edges[base + raw.edge_offsets.to_node] as usize / nfc;
            let name = if edge_type == "element" {
                format!("[{name_or_index}]")
            } else {
                raw.strings
                    .resolve(&raw.mmap, name_or_index)
                    .unwrap_or_else(|| format!("#{name_or_index}"))
            };

            let value = if to_node >= raw.meta.node_count {
                PropertyValue::Ref {
                    index: to_node,
                    id: 0,
                    node_type: String::new(),
                    name: format!("<out of range #{to_node}>"),
                }
            } else {
                let tbase = to_node * nfc;
                let ttype_idx = nodes[tbase + raw.node_offsets.type_] as usize;
                let ttype = raw
                    .node_types
                    .get(ttype_idx)
                    .cloned()
                    .unwrap_or_else(|| ttype_idx.to_string());
                let tname = raw
                    .strings
                    .resolve(&raw.mmap, nodes[tbase + raw.node_offsets.name] as usize)
                    .unwrap_or_default();
                if ttype == "number" || ttype == "bigint" {
                    match tname.parse::<f64>() {
                        Ok(v) => PropertyValue::Number(v),
                        Err(_) => PropertyValue::Str(tname),
                    }
                } else if ttype == "string" {
                    PropertyValue::Str(tname)
                } else {
                    PropertyValue::Ref {
                        index: to_node,
                        id: nodes[tbase + raw.node_offsets.id] as usize,
                        node_type: ttype,
                        name: tname,
                    }
                }
            };
            props.push(NodeProperty {
                name,
                edge_type,
                value,
            });
        }
        Ok((node, props))
    }

    /// All incoming edges of a node: who retains it and how. Single pass
    /// over the edges array (no retained/dominator analysis needed).
    pub fn get_retainers(&mut self, node_index: usize) -> crate::Result<Vec<RetainerRef>> {
        self.ensure_raw()?;
        self.ensure_edges()?;
        self.ensure_edge_starts();
        let raw = self.raw.as_ref().unwrap();
        if node_index >= raw.meta.node_count {
            return Err(Error::NodeNotFound(node_index));
        }

        let edge_starts = self.edge_starts.as_ref().unwrap();
        let edges = raw.edges.as_ref().unwrap();
        let nfc = raw.node_field_count;
        let mut out = Vec::new();
        for n in 0..raw.meta.node_count {
            let start = edge_starts[n] as usize;
            let end = edge_starts[n + 1] as usize;
            for edge_index in start..end {
                let base = edge_index * raw.edge_field_count;
                let to_node = edges[base + raw.edge_offsets.to_node] as usize / nfc;
                if to_node != node_index {
                    continue;
                }
                let type_idx = edges[base + raw.edge_offsets.type_] as usize;
                let edge_type = raw
                    .edge_types
                    .get(type_idx)
                    .cloned()
                    .unwrap_or_else(|| type_idx.to_string());
                let name_or_index = edges[base + raw.edge_offsets.name_or_index] as usize;
                let name = if edge_type == "element" {
                    format!("[{name_or_index}]")
                } else {
                    raw.strings
                        .resolve(&raw.mmap, name_or_index)
                        .unwrap_or_else(|| format!("#{name_or_index}"))
                };
                out.push(RetainerRef {
                    source: n,
                    edge_type,
                    name,
                });
            }
        }
        Ok(out)
    }

    /// Walk the first-parent chain from `node_index` up to `max_depth` hops,
    /// stopping on cycles (an already-seen node) or when a node has no
    /// parent. The chain is ordered from the target upward: `[0]` is the
    /// target itself, the last element is the top of the chain. `cycle` is
    /// set on the final hop when the walk was cut short by a cycle.
    pub fn retainer_chain(
        &mut self,
        node_index: usize,
        max_depth: usize,
    ) -> crate::Result<Vec<RetainerChainNode>> {
        self.ensure_raw()?;
        self.ensure_parent_map()?;
        let raw = self.raw.as_ref().unwrap();
        if node_index >= raw.meta.node_count {
            return Err(Error::NodeNotFound(node_index));
        }
        if max_depth == 0 {
            return Ok(Vec::new());
        }

        let pm = self.parents.as_ref().unwrap();
        let nodes = &raw.nodes;
        let nfc = raw.node_field_count;
        let mut chain: Vec<RetainerChainNode> = Vec::with_capacity(max_depth.min(64) + 1);
        let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut cur = node_index;
        let mut hops = 0usize;
        loop {
            if !seen.insert(cur) {
                // cycle: the parent chain loops back to an already-seen node
                if let Some(last) = chain.last_mut() {
                    last.cycle = true;
                }
                break;
            }
            let base = cur * nfc;
            let type_idx = nodes[base + raw.node_offsets.type_] as usize;
            let name_idx = nodes[base + raw.node_offsets.name] as usize;
            let parent = pm.index[cur];
            let edge_type = if parent == u32::MAX {
                String::new()
            } else {
                raw.edge_types
                    .get(pm.edge_type[cur] as usize)
                    .cloned()
                    .unwrap_or_else(|| pm.edge_type[cur].to_string())
            };
            let edge_name = if parent == u32::MAX {
                String::new()
            } else if edge_type == "element" {
                format!("[{}]", pm.edge_name_or_index[cur])
            } else {
                raw.strings
                    .resolve(&raw.mmap, pm.edge_name_or_index[cur] as usize)
                    .unwrap_or_else(|| format!("#{}", pm.edge_name_or_index[cur]))
            };
            chain.push(RetainerChainNode {
                node_index: cur,
                id: nodes[base + raw.node_offsets.id] as usize,
                name: raw.strings.resolve(&raw.mmap, name_idx).unwrap_or_default(),
                type_: raw
                    .node_types
                    .get(type_idx)
                    .cloned()
                    .unwrap_or_else(|| type_idx.to_string()),
                self_size: nodes[base + raw.node_offsets.self_size] as usize,
                edge_count: nodes[base + raw.node_offsets.edge_count] as usize,
                edge_type,
                edge_name,
                cycle: false,
            });
            if parent == u32::MAX {
                break;
            }
            cur = parent as usize;
            hops += 1;
            if hops >= max_depth {
                break;
            }
        }
        Ok(chain)
    }

    /// Classify nodes matched by `query` into owner groups: each match is
    /// walked up its first-parent chain (`depth` hops) and the resulting
    /// "owner -> parent -> ..." chain string is the group key. Groups carry
    /// count and summed self size, sorted by self size descending.
    pub fn owner_groups(
        &mut self,
        query: &NameQuery,
        depth: usize,
        top: usize,
    ) -> crate::Result<OwnerAnalysis> {
        let matches = self.find_nodes(query)?;
        let mut map: AHashMap<String, (usize, usize)> = AHashMap::new();
        let mut total_self = 0usize;
        for m in &matches {
            total_self += m.self_size;
            let chain = self.retainer_chain(m.node_index, depth)?;
            // chain[0] is the match itself; the owner chain starts at [1].
            let mut parts: Vec<String> = Vec::new();
            for (i, hop) in chain.iter().enumerate().skip(1) {
                if i == 1 {
                    // immediate owner: name, with a type fallback like the
                    // classic scripts use for unnamed nodes
                    if hop.name.is_empty() {
                        parts.push(format!("({})", hop.type_));
                    } else {
                        parts.push(hop.name.clone());
                    }
                } else if !hop.name.is_empty() {
                    parts.push(hop.name.clone());
                }
            }
            let key = if parts.is_empty() {
                "(none)".to_string()
            } else {
                parts.join(" → ")
            };
            let e = map.entry(key).or_insert((0, 0));
            e.0 += m.self_size;
            e.1 += 1;
        }
        let mut groups: Vec<OwnerGroup> = map
            .into_iter()
            .map(|(chain, (self_size, count))| OwnerGroup {
                chain,
                count,
                self_size,
            })
            .collect();
        groups.sort_unstable_by(|a, b| {
            b.self_size
                .cmp(&a.self_size)
                .then_with(|| a.chain.cmp(&b.chain))
        });
        if top > 0 && groups.len() > top {
            groups.truncate(top);
        }
        Ok(OwnerAnalysis {
            name: query.name.clone(),
            total_nodes: matches.len(),
            total_self,
            groups,
        })
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

/// Heap entry for the bounded top-K selection in `get_node_page`. The value
/// is the sort key (already inverted for ascending order).
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

/// Compare two node records by a numeric field (plus index tiebreak),
/// honouring the requested sort direction.
fn compare_node_indices(
    raw: &RawData,
    a: usize,
    b: usize,
    sort: SortField,
    dir: SortDir,
) -> std::cmp::Ordering {
    let num = |idx: usize| -> u64 {
        let base = idx * raw.node_field_count;
        match sort {
            SortField::SelfSize => raw.nodes[base + raw.node_offsets.self_size] as u64,
            SortField::Id => raw.nodes[base + raw.node_offsets.id] as u64,
            SortField::EdgeCount => raw.nodes[base + raw.node_offsets.edge_count] as u64,
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

/// Find the index of the closing `]` for the array opened at `open`.
pub fn find_array_end(data: &[u8], open: usize) -> usize {
    let mut i = open + 1;
    let len = data.len();
    let mut depth = 1i32;
    while i < len {
        match data[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            b'"' => {
                i += 1;
                while i < len {
                    if data[i] == b'\\' {
                        i += 2;
                    } else if data[i] == b'"' {
                        break;
                    } else {
                        i += 1;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    len - 1
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

/// Parse all integers from a flat comma-separated byte region in parallel.
/// The region must be bounded (a single array, no nested arrays/strings);
/// chunk boundaries are placed at comma positions so no number is split.
/// Small regions fall back to the serial `parse_numbers_fast`.
pub fn parse_numbers_par(data: &[u8]) -> Vec<u32> {
    const MIN_PAR: usize = 1 << 20; // 1 MiB
    if data.len() < MIN_PAR {
        return parse_numbers_fast(data);
    }
    let threads = rayon::current_num_threads().max(1);
    let chunks = threads * 4;

    // boundaries at comma positions, roughly data.len()/chunks apart
    let target = data.len() / chunks;
    let mut bounds: Vec<usize> = Vec::with_capacity(chunks + 1);
    bounds.push(0);
    let mut pos = 0usize;
    for _ in 1..chunks {
        let mut p = (pos + target).min(data.len());
        while p < data.len() && data[p] != b',' {
            p += 1;
        }
        pos = (p + 1).min(data.len());
        bounds.push(pos);
    }
    bounds.push(data.len());

    let ranges: Vec<(usize, usize)> = bounds
        .windows(2)
        .map(|w| (w[0], w[1]))
        .filter(|(s, e)| e > s)
        .collect();
    let parts: Vec<Vec<u32>> = ranges
        .par_iter()
        .map(|&(s, e)| parse_numbers_fast(&data[s..e]))
        .collect();
    let total: usize = parts.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(total);
    for part in parts {
        out.extend_from_slice(&part);
    }
    out
}
