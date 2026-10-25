use ahash::AHashMap;
use rayon::prelude::*;

use super::parser::{aligned_record_chunks_par, parse_numbers_par};
use super::strings::{contains_ci, eq_ci, eq_ci_name, raw_string_matches};
use super::{HeapSnapshot, RawData};
use crate::types::*;

pub struct ParentMap {
    pub index: Vec<u32>,
}

impl HeapSnapshot {
    /// Build (or reuse) the per-node edge-start index. `get_node_edges`,
    /// `ensure_retained` and path reconstruction all need it.
    pub(crate) fn ensure_edge_starts(&mut self) {
        if self.edge_starts.is_some() {
            return;
        }
        let raw = self.raw.as_ref().unwrap();
        let node_count = raw.meta.node_count;
        let mut starts = Vec::with_capacity(node_count + 1);
        let mut offset = 0u32;
        for i in 0..node_count {
            starts.push(offset);
            offset += raw.nodes.edge_counts[i];
        }
        starts.push(offset);
        self.edge_starts = Some(starts);
    }

    /// The edge from `from` to `to` (bounded scan of `from`'s forward edges),
    /// as `(edge_type, property name / "[i]" / "i")`.
    pub(crate) fn path_edge(
        &self,
        raw: &RawData,
        edge_starts: &[u32],
        from: usize,
        to: usize,
        bracket_element: bool,
    ) -> Option<(String, String)> {
        let start = edge_starts[from] as usize;
        let end = edge_starts[from + 1] as usize;
        let weak_edge_type = raw.edge_types.iter().position(|t| t == "weak");
        let mut result: Option<(String, String)> = None;
        raw.for_each_edge_in_range(
            start,
            end,
            |_edge_index, type_idx, name_or_index, to_node| {
                if result.is_some() || to_node != to || Some(type_idx) == weak_edge_type {
                    return;
                }
                let type_ = raw
                    .edge_types
                    .get(type_idx)
                    .cloned()
                    .unwrap_or_else(|| type_idx.to_string());
                let name_or_index = name_or_index as usize;
                let name = if type_ == "element" {
                    if bracket_element {
                        format!("[{name_or_index}]")
                    } else {
                        name_or_index.to_string()
                    }
                } else {
                    raw.strings
                        .resolve(&raw.mmap, name_or_index)
                        .unwrap_or_else(|| format!("#{name_or_index}"))
                };
                result = Some((type_, name));
            },
        );
        result
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
            return Ok(ShortestPath {
                found: false,
                nodes: Vec::new(),
                edges: Vec::new(),
            });
        }
        if target_pre == 0 {
            let root_node = Self::create_node_raw(raw, 0, true);
            return Ok(ShortestPath {
                found: true,
                nodes: vec![PathNode {
                    index: 0,
                    id: root_node.id,
                    name: root_node.name,
                    type_: root_node.type_,
                    self_size: root_node.self_size,
                }],
                edges: Vec::new(),
            });
        }

        let reachable = rd.vertex.len();
        // Single BFS-tree array: `next[w] = u` means the root→target path
        // continues from `w` to `u`; `u32::MAX` marks unvisited. Depth is
        // tracked level-wise so no dense `dist` array is needed (saves
        // 4 bytes x reachable transient per query).
        let mut next = vec![u32::MAX; reachable];
        let mut queue: std::collections::VecDeque<u32> =
            std::collections::VecDeque::with_capacity(1024);

        next[target_pre as usize] = target_pre;
        queue.push_back(target_pre);
        let mut found = false;
        let mut depth = 0usize;

        let rev_starts = rd.rev_starts.as_slice();
        let rev_sources = rd.rev_sources.as_slice();

        while !queue.is_empty() {
            let level_len = queue.len();
            let mut reached_root = false;
            for _ in 0..level_len {
                let w = queue.pop_front().unwrap();
                if w == 0 {
                    found = true;
                    reached_root = true;
                    break;
                }
                if depth >= max_depth {
                    continue;
                }
                let s_start = rev_starts[w as usize] as usize;
                let s_end = rev_starts[w as usize + 1] as usize;
                for &u in &rev_sources[s_start..s_end] {
                    if next[u as usize] == u32::MAX {
                        next[u as usize] = w;
                        queue.push_back(u);
                    }
                }
            }
            if reached_root {
                break;
            }
            depth += 1;
        }

        if !found {
            return Ok(ShortestPath {
                found: false,
                nodes: Vec::new(),
                edges: Vec::new(),
            });
        }

        let mut pres: Vec<usize> = vec![0usize];
        let mut cur = 0usize;
        while cur != target_pre as usize {
            cur = next[cur] as usize;
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
                self.path_edge(raw, edge_starts, from, to, false)
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
    /// forward edges; the first edge into a node wins. O(E), no dominator
    /// analysis, and stores only `index: Vec<u32>` (67MB instead of 201MB).
    pub(crate) fn ensure_parent_map(&mut self) -> crate::Result<()> {
        if self.parents.is_some() {
            return Ok(());
        }
        self.ensure_raw()?;
        self.ensure_edges()?;
        self.ensure_edge_starts();
        let raw = self.raw.as_ref().unwrap();
        let edge_starts = self.edge_starts.as_ref().unwrap();
        let node_count = raw.meta.node_count;
        let edges = raw.edges.as_ref().unwrap();
        let edge_types = edges.types.as_slice();
        let edge_to_nodes = edges.to_nodes.as_slice();

        let weak_edge_type = raw
            .edge_types
            .iter()
            .position(|t| t == "weak")
            .map(|idx| idx as u8)
            .unwrap_or(u8::MAX);

        let mut index = vec![u32::MAX; node_count];
        for n in 0..node_count {
            let start = edge_starts[n] as usize;
            let end = edge_starts[n + 1] as usize;
            for edge_index in start..end {
                if edge_types[edge_index] == weak_edge_type {
                    continue;
                }
                let to_node = edge_to_nodes[edge_index] as usize;
                if to_node < node_count && index[to_node] == u32::MAX {
                    index[to_node] = n as u32;
                }
            }
        }
        self.parents = Some(ParentMap { index });
        Ok(())
    }

    /// Find nodes by name (exact or substring) with optional self-size and
    /// node-type filters.
    pub fn find_nodes(&mut self, query: &NameQuery) -> crate::Result<Vec<NameMatch>> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        if query.name.is_empty() {
            return Ok(Vec::new());
        }
        let col = &raw.nodes;

        let exact = query.exact;
        let min_self = query.min_self;
        let limit = query.limit;
        let query_name = query.name.as_str();
        let type_filter = query.type_filter.as_deref();
        let node_types = raw.node_types.as_slice();

        let (sbase, spans_guard) = raw.strings.all_spans(&raw.mmap);
        let spans: &[(u32, u32)] = &spans_guard;
        let mmap_bytes: &[u8] = &raw.mmap[..];
        let num_strings = spans.len();

        let matching_strings: Vec<bool> = (0..num_strings)
            .into_par_iter()
            .map(|idx| {
                let (s, e) = spans[idx];
                raw_string_matches(
                    &mmap_bytes[sbase + s as usize..sbase + e as usize],
                    query_name,
                    exact,
                )
            })
            .collect();
        drop(spans_guard);

        if !matching_strings.iter().any(|&m| m) {
            return Ok(Vec::new());
        }

        let total_nodes = raw.meta.node_count;
        let num_threads = rayon::current_num_threads().max(1);
        let num_chunks = (num_threads * 4).max(1);
        let chunk_size = total_nodes.div_ceil(num_chunks).max(1);
        let parts: Vec<Vec<(usize, usize)>> = (0..num_chunks)
            .into_par_iter()
            .map(|c| {
                let start = (c * chunk_size).min(total_nodes);
                let end = ((c + 1) * chunk_size).min(total_nodes);
                let mut found: Vec<(usize, usize)> = Vec::new();
                for n in start..end {
                    let name_idx = col.names[n] as usize;
                    if name_idx >= num_strings || !matching_strings[name_idx] {
                        continue;
                    }
                    let self_size = col.self_sizes[n] as usize;
                    if self_size < min_self {
                        continue;
                    }
                    if let Some(tf) = type_filter {
                        let t = node_types
                            .get(col.types[n] as usize)
                            .map(|s| s.as_str())
                            .unwrap_or("");
                        if !eq_ci(t, tf) {
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
                let type_idx = col.types[idx] as usize;
                let name_idx = col.names[idx] as usize;
                NameMatch {
                    node_index: idx,
                    id: col.ids[idx] as usize,
                    name: raw.strings.resolve(&raw.mmap, name_idx).unwrap_or_default(),
                    type_: raw
                        .node_types
                        .get(type_idx)
                        .cloned()
                        .unwrap_or_else(|| type_idx.to_string()),
                    self_size: col.self_sizes[idx] as usize,
                    edge_count: col.edge_counts[idx] as usize,
                }
            })
            .collect())
    }

    /// Resolve a node's edges into displayable properties. When `raw.edges` is
    /// not yet loaded, reads the single node's edges directly from the mmap
    /// without allocating the 202MB edge table.
    pub fn get_node_properties(
        &mut self,
        node_index: usize,
    ) -> crate::Result<(HeapSnapshotNode, Vec<NodeProperty>)> {
        self.ensure_raw()?;
        let _ = self.locate_edges_range()?;
        self.ensure_edge_starts();
        let raw = self.raw.as_ref().unwrap();
        if node_index >= raw.meta.node_count {
            return Err(Error::NodeNotFound(node_index));
        }

        let node = Self::create_node(raw, node_index);
        let edge_starts = self.edge_starts.as_ref().unwrap();
        let start = edge_starts[node_index] as usize;
        let end = edge_starts[node_index + 1] as usize;
        let col = &raw.nodes;

        let mut props = Vec::with_capacity(end - start);
        raw.for_each_edge_in_range(
            start,
            end,
            |_edge_index, type_idx, name_or_index, to_node| {
                let name_or_index = name_or_index as usize;
                let edge_type = raw
                    .edge_types
                    .get(type_idx)
                    .cloned()
                    .unwrap_or_else(|| type_idx.to_string());
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
                    let ttype_idx = col.types[to_node] as usize;
                    let ttype = raw
                        .node_types
                        .get(ttype_idx)
                        .cloned()
                        .unwrap_or_else(|| ttype_idx.to_string());
                    let tname = raw
                        .strings
                        .resolve(&raw.mmap, col.names[to_node] as usize)
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
                            id: col.ids[to_node] as usize,
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
            },
        );

        Ok((node, props))
    }

    /// Search the edges array for edges matching `query` (name, source-type,
    /// edge-type). Streamed directly from the mmap so querying edges does
    /// not force the 202MB edge array into memory.
    pub fn find_edges(&mut self, query: &EdgeQuery) -> crate::Result<Vec<EdgeMatch>> {
        self.ensure_raw()?;
        let (eb_start, eb_end) = self.locate_edges_range()?;
        self.ensure_edge_starts();
        let raw = self.raw.as_ref().unwrap();
        if query.name.is_empty() {
            return Ok(Vec::new());
        }
        let edge_starts = self.edge_starts.as_ref().unwrap();

        let exact = query.exact;
        let query_name = query.name.as_str();

        let (sbase, spans_guard) = raw.strings.all_spans(&raw.mmap);
        let spans: &[(u32, u32)] = &spans_guard;
        let mmap_bytes: &[u8] = &raw.mmap[..];
        let num_strings = spans.len();

        let matching_strings: Vec<bool> = (0..num_strings)
            .into_par_iter()
            .map(|idx| {
                let (s, e) = spans[idx];
                raw_string_matches(
                    &mmap_bytes[sbase + s as usize..sbase + e as usize],
                    query_name,
                    exact,
                )
            })
            .collect();
        drop(spans_guard);

        let allowed_source_types: Vec<bool> = (0..raw.node_types.len().max(16))
            .map(|idx| match query.type_filter.as_deref() {
                None => true,
                Some(filter) => {
                    let st = raw
                        .node_types
                        .get(idx)
                        .cloned()
                        .unwrap_or_else(|| idx.to_string());
                    eq_ci(&st, filter)
                }
            })
            .collect();

        let allowed_edge_types: Vec<bool> = (0..raw.edge_types.len().max(16))
            .map(|idx| match query.edge_type.as_deref() {
                None => true,
                Some(filter) => {
                    let et = raw
                        .edge_types
                        .get(idx)
                        .cloned()
                        .unwrap_or_else(|| idx.to_string());
                    eq_ci(&et, filter)
                }
            })
            .collect();

        let element_type_idx = raw.edge_types.iter().position(|t| t == "element");
        let element_can_match = query
            .name
            .bytes()
            .all(|b| b == b'[' || b == b']' || b.is_ascii_digit());
        let any_string_matches = matching_strings.iter().any(|&m| m);
        if !any_string_matches && !element_can_match {
            return Ok(Vec::new());
        }

        let edges_bytes = &raw.mmap[eb_start..=eb_end];
        let efc = raw.edge_field_count;
        let nfc = raw.node_field_count.max(1) as u32;
        let type_off = raw.edge_offsets.type_;
        let name_off = raw.edge_offsets.name_or_index;
        let to_off = raw.edge_offsets.to_node;
        let limit = if query.limit == 0 {
            usize::MAX
        } else {
            query.limit
        };
        let node_types_col = raw.nodes.types.as_slice();

        let chunks = aligned_record_chunks_par(edges_bytes, efc);
        let raw_matches: Vec<(usize, usize, u32, usize)> = if let Some(chunks) = chunks {
            let parts: Vec<Vec<(usize, usize, u32, usize)>> = chunks
                .par_iter()
                .map(|&(byte_s, byte_e, edge_s, _edge_e)| {
                    let mut local = Vec::new();
                    let mut source_idx = edge_starts
                        .partition_point(|&s| (s as usize) <= edge_s)
                        .saturating_sub(1);
                    let mut edge_idx = edge_s;
                    let slice = &edges_bytes[byte_s..byte_e];
                    let mut i = 0usize;
                    let len = slice.len();
                    let mut rec = [0u32; 8];
                    let mut f = 0usize;
                    while i < len {
                        let ch = slice[i];
                        if ch == b']' {
                            break;
                        }
                        if ch.is_ascii_digit() {
                            let mut val = (ch - b'0') as u32;
                            i += 1;
                            while i < len && slice[i].is_ascii_digit() {
                                val = val * 10 + (slice[i] - b'0') as u32;
                                i += 1;
                            }
                            rec[f] = val;
                            f += 1;
                            if f == efc {
                                f = 0;
                                while source_idx + 1 < edge_starts.len()
                                    && edge_idx >= edge_starts[source_idx + 1] as usize
                                {
                                    source_idx += 1;
                                }
                                let etype = rec[type_off] as usize;
                                let stype =
                                    node_types_col.get(source_idx).copied().unwrap_or(0) as usize;
                                if allowed_edge_types.get(etype).copied().unwrap_or(false)
                                    && allowed_source_types.get(stype).copied().unwrap_or(false)
                                {
                                    let name_or_idx = rec[name_off];
                                    let is_elem = Some(etype) == element_type_idx;
                                    let matched = if !is_elem {
                                        (name_or_idx as usize) < num_strings
                                            && matching_strings[name_or_idx as usize]
                                    } else if element_can_match {
                                        let formatted = format!("[{name_or_idx}]");
                                        if exact {
                                            eq_ci_name(&formatted, query_name)
                                        } else {
                                            contains_ci(&formatted, query_name)
                                        }
                                    } else {
                                        false
                                    };
                                    if matched {
                                        let to_node = (rec[to_off] / nfc) as usize;
                                        local.push((source_idx, etype, name_or_idx, to_node));
                                        if local.len() >= limit {
                                            break;
                                        }
                                    }
                                }
                                edge_idx += 1;
                            }
                            continue;
                        }
                        i += 1;
                    }
                    local
                })
                .collect();
            let mut out = Vec::new();
            for p in parts {
                for item in p {
                    out.push(item);
                    if out.len() >= limit {
                        break;
                    }
                }
                if out.len() >= limit {
                    break;
                }
            }
            out
        } else {
            let mut out = Vec::new();
            let flat = parse_numbers_par(edges_bytes);
            let mut source_idx = 0usize;
            for (edge_idx, rec) in flat.chunks(efc).enumerate() {
                if rec.len() < efc {
                    break;
                }
                while source_idx + 1 < edge_starts.len()
                    && edge_idx >= edge_starts[source_idx + 1] as usize
                {
                    source_idx += 1;
                }
                let etype = rec[type_off] as usize;
                let stype = node_types_col.get(source_idx).copied().unwrap_or(0) as usize;
                if allowed_edge_types.get(etype).copied().unwrap_or(false)
                    && allowed_source_types.get(stype).copied().unwrap_or(false)
                {
                    let name_or_idx = rec[name_off];
                    let is_elem = Some(etype) == element_type_idx;
                    let matched = if !is_elem {
                        (name_or_idx as usize) < num_strings
                            && matching_strings[name_or_idx as usize]
                    } else if element_can_match {
                        let formatted = format!("[{name_or_idx}]");
                        if exact {
                            eq_ci_name(&formatted, query_name)
                        } else {
                            contains_ci(&formatted, query_name)
                        }
                    } else {
                        false
                    };
                    if matched {
                        let to_node = (rec[to_off] / nfc) as usize;
                        out.push((source_idx, etype, name_or_idx, to_node));
                        if out.len() >= limit {
                            break;
                        }
                    }
                }
            }
            out
        };

        let mut matches = Vec::with_capacity(raw_matches.len());
        for (source_index, edge_type_index, name_or_index, target_index) in raw_matches {
            let source_type = raw
                .node_types
                .get(raw.nodes.types[source_index] as usize)
                .cloned()
                .unwrap_or_default();
            let edge_type = raw
                .edge_types
                .get(edge_type_index)
                .cloned()
                .unwrap_or_else(|| edge_type_index.to_string());
            let name_or_index = name_or_index as usize;
            let name = if edge_type == "element" {
                format!("[{name_or_index}]")
            } else {
                raw.strings
                    .resolve(&raw.mmap, name_or_index)
                    .unwrap_or_else(|| format!("#{name_or_index}"))
            };
            let (target_id, target_name, target_type) = if target_index < raw.meta.node_count {
                let target = Self::create_node(raw, target_index);
                (target.id, target.name, target.type_)
            } else {
                (0, format!("<out of range #{target_index}>"), String::new())
            };
            let source_name = raw
                .strings
                .resolve(&raw.mmap, raw.nodes.names[source_index] as usize)
                .unwrap_or_default();
            matches.push(EdgeMatch {
                source_index,
                source_id: raw.nodes.ids[source_index] as usize,
                source_name,
                source_type,
                edge_type,
                name,
                target_index,
                target_id,
                target_name,
                target_type,
            });
        }
        Ok(matches)
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
        let to_nodes = edges.to_nodes.as_slice();
        let target_u32 = node_index as u32;
        let mut out = Vec::new();
        for n in 0..raw.meta.node_count {
            let start = edge_starts[n] as usize;
            let end = edge_starts[n + 1] as usize;
            for edge_index in start..end {
                if to_nodes[edge_index] != target_u32 {
                    continue;
                }
                raw.for_each_edge_in_range(
                    edge_index,
                    edge_index + 1,
                    |_, type_idx, name_or_index, _| {
                        let name_or_index = name_or_index as usize;
                        let edge_type = raw
                            .edge_types
                            .get(type_idx)
                            .cloned()
                            .unwrap_or_else(|| type_idx.to_string());
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
                    },
                );
            }
        }
        Ok(out)
    }

    /// Walk the first-parent chain from `node_index` up to `max_depth` hops,
    /// stopping on cycles (an already-seen node) or when a node has no
    /// parent.
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
        let edge_starts = self.edge_starts.as_ref().unwrap();
        let col = &raw.nodes;
        let mut chain: Vec<RetainerChainNode> = Vec::with_capacity(max_depth.min(64) + 1);
        let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut cur = node_index;
        let mut hops = 0usize;
        loop {
            if !seen.insert(cur) {
                if let Some(last) = chain.last_mut() {
                    last.cycle = true;
                }
                break;
            }
            let type_idx = col.types[cur] as usize;
            let name_idx = col.names[cur] as usize;
            let parent = pm.index[cur];
            let (edge_type, edge_name) = if parent == u32::MAX {
                (String::new(), String::new())
            } else {
                self.path_edge(raw, edge_starts, parent as usize, cur, true)
                    .unwrap_or_default()
            };
            chain.push(RetainerChainNode {
                node_index: cur,
                id: col.ids[cur] as usize,
                name: raw.strings.resolve(&raw.mmap, name_idx).unwrap_or_default(),
                type_: raw
                    .node_types
                    .get(type_idx)
                    .cloned()
                    .unwrap_or_else(|| type_idx.to_string()),
                self_size: col.self_sizes[cur] as usize,
                edge_count: col.edge_counts[cur] as usize,
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

    /// Classify nodes matched by `query` into owner groups.
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
            let mut parts: Vec<String> = Vec::new();
            for (i, hop) in chain.iter().enumerate().skip(1) {
                if i == 1 {
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

    pub fn get_node_edges(
        &mut self,
        node_index: usize,
    ) -> crate::Result<(HeapSnapshotNode, Vec<HeapSnapshotEdge>)> {
        self.ensure_raw()?;
        let _ = self.locate_edges_range()?;
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
        raw.for_each_edge_in_range(
            edge_start,
            edge_end,
            |_edge_idx, type_idx, name_or_index_val, to_node| {
                let name_or_index_val = name_or_index_val as usize;
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
            },
        );

        Ok((node, edges))
    }

    /// Emit a bounded outgoing object graph around a node as Graphviz DOT.
    pub fn to_dot_subgraph(
        &mut self,
        root: usize,
        max_depth: usize,
        max_nodes: usize,
    ) -> crate::Result<String> {
        self.ensure_raw()?;
        let _ = self.locate_edges_range()?;
        self.ensure_edge_starts();
        let raw = self.raw.as_ref().unwrap();
        if root >= raw.meta.node_count {
            return Err(Error::NodeNotFound(root));
        }
        let node_limit = if max_nodes == 0 { 1_000 } else { max_nodes };
        let edge_starts = self.edge_starts.as_ref().unwrap();
        let mut seen = std::collections::HashSet::with_capacity(node_limit.min(1024));
        let mut queue = std::collections::VecDeque::new();
        let mut graph_edges: Vec<(usize, usize, String)> = Vec::new();
        seen.insert(root);
        queue.push_back((root, 0usize));

        while let Some((source, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let start = edge_starts[source] as usize;
            let end = edge_starts[source + 1] as usize;
            raw.for_each_edge_in_range(
                start,
                end,
                |_edge_index, type_index, name_index, target| {
                    if target >= raw.meta.node_count {
                        return;
                    }
                    if !seen.contains(&target) && seen.len() >= node_limit {
                        return;
                    }
                    let is_new = seen.insert(target);
                    let edge_type = raw
                        .edge_types
                        .get(type_index)
                        .cloned()
                        .unwrap_or_else(|| type_index.to_string());
                    let name_index = name_index as usize;
                    let edge_name = if edge_type == "element" {
                        format!("[{name_index}]")
                    } else {
                        raw.strings
                            .resolve(&raw.mmap, name_index)
                            .unwrap_or_else(|| format!("#{name_index}"))
                    };
                    graph_edges.push((source, target, format!("{edge_type}: {edge_name}")));
                    if is_new && depth + 1 < max_depth {
                        queue.push_back((target, depth + 1));
                    }
                },
            );
        }

        fn dot_escape(value: &str) -> String {
            value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
        }

        let mut nodes: Vec<usize> = seen.into_iter().collect();
        nodes.sort_unstable();
        let mut out = String::from("digraph heap {\n  rankdir=LR;\n");
        for index in nodes {
            let node = Self::create_node(raw, index);
            let label = format!(
                "#{} {}\\n{}\\nself {}",
                index,
                node.name,
                node.type_,
                crate::types::format_bytes(node.self_size)
            );
            let shape = if index == root { "doublecircle" } else { "box" };
            out.push_str(&format!(
                "  n{index} [shape={shape}, label=\"{}\"];\n",
                dot_escape(&label)
            ));
        }
        for (source, target, label) in graph_edges {
            out.push_str(&format!(
                "  n{source} -> n{target} [label=\"{}\"];\n",
                dot_escape(&label)
            ));
        }
        out.push_str("}\n");
        Ok(out)
    }
}
