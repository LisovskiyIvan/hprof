use std::collections::HashMap;

use ahash::AHashMap;
use rayon::prelude::*;

use super::strings::raw_string_matches;
use super::HeapSnapshot;
use crate::types::*;

/// Cached dominator-tree analysis (per-node retained sizes + reverse graph).
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

impl HeapSnapshot {
    /// Compute (and cache) the dominator-tree analysis: per-node retained
    /// sizes plus a flat reverse (incoming-edge) CSR.
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
        let weak_edge_type = raw
            .edge_types
            .iter()
            .position(|t| t == "weak")
            .map(|idx| idx as u8)
            .unwrap_or(u8::MAX);
        let edges = raw.edges.as_ref().unwrap();
        let edge_types = edges.types.as_slice();
        let edge_to_nodes = edges.to_nodes.as_slice();

        // Iterative DFS from root (node 0), assigning preorder numbers.
        // Instead of a 67MB `next_edge: Vec<u32>`, each active stack frame holds
        // `(v_pre, next_e, end_e)` and leaf nodes (`to_start == to_end`, >45% of
        // the heap) are assigned preorder numbers without pushing/popping a
        // stack frame — producing the exact same DFS preorder traversal while
        // saving 67MB and ~7.5M loop iterations.
        let mut preorder = vec![u32::MAX; node_count];
        let mut vertex: Vec<u32> = Vec::with_capacity(node_count);
        let mut dfs_parent: Vec<u32> = Vec::with_capacity(node_count);
        let mut pre = 0u32;
        preorder[0] = 0;
        vertex.push(0);
        dfs_parent.push(0);
        let mut stack: Vec<(u32, u32, u32)> = Vec::with_capacity(4096);
        if edge_starts[0] < edge_starts[1] {
            stack.push((0, edge_starts[0], edge_starts[1]));
        }
        while let Some(&mut (v_pre, ref mut next_e, end_e)) = stack.last_mut() {
            let mut e = *next_e as usize;
            let end = end_e as usize;
            let mut descended = false;
            while e < end {
                if edge_types[e] == weak_edge_type {
                    e += 1;
                    continue;
                }
                let to_node = edge_to_nodes[e] as usize;
                if to_node < node_count && preorder[to_node] == u32::MAX {
                    pre += 1;
                    preorder[to_node] = pre;
                    vertex.push(to_node as u32);
                    dfs_parent.push(v_pre);
                    let to_start = edge_starts[to_node];
                    let to_end = edge_starts[to_node + 1];
                    if to_start < to_end {
                        *next_e = (e + 1) as u32;
                        stack.push((pre, to_start, to_end));
                        descended = true;
                        break;
                    }
                }
                e += 1;
            }
            if !descended {
                stack.pop();
            }
        }
        drop(stack);

        // Flat reverse CSR in pre space built in-place on `rev_starts` (no
        // separate 66MB `counts`/`cursor` vector).
        let reachable = vertex.len();
        let mut rev_starts = vec![0u32; reachable + 1];
        for &n_u32 in &vertex {
            let n = n_u32 as usize;
            let start = edge_starts[n] as usize;
            let end = edge_starts[n + 1] as usize;
            for edge_index in start..end {
                if edge_types[edge_index] == weak_edge_type {
                    continue;
                }
                let to_node = edge_to_nodes[edge_index] as usize;
                if to_node < node_count {
                    let w = preorder[to_node];
                    if w != u32::MAX {
                        rev_starts[w as usize + 1] += 1;
                    }
                }
            }
        }
        for i in 1..=reachable {
            rev_starts[i] += rev_starts[i - 1];
        }

        let mut rev_sources = vec![0u32; rev_starts[reachable] as usize];
        for (v_pre, &n_u32) in vertex.iter().enumerate() {
            let n = n_u32 as usize;
            let start = edge_starts[n] as usize;
            let end = edge_starts[n + 1] as usize;
            for edge_index in start..end {
                if edge_types[edge_index] == weak_edge_type {
                    continue;
                }
                let to_node = edge_to_nodes[edge_index] as usize;
                if to_node < node_count {
                    let w = preorder[to_node];
                    if w != u32::MAX {
                        let pos = rev_starts[w as usize] as usize;
                        rev_sources[pos] = v_pre as u32;
                        rev_starts[w as usize] = (pos + 1) as u32;
                    }
                }
            }
        }
        for i in (1..=reachable).rev() {
            rev_starts[i] = rev_starts[i - 1];
        }
        rev_starts[0] = 0;

        // Lengauer–Tarjan dominators in preorder space.
        // Memory optimizations:
        // 1. `dfs_parent` doubles as `ancestor` in place: vertex `x` is linked
        //    into the forest iff `x > threshold` (saving 66MB).
        // 2. `idom_pre` doubles as `bucket_next`: each vertex `v` is linked
        //    into bucket `semi[v]` once and its `idom_pre[v]` is written only
        //    after `bucket_next[v]` is read on bucket drain (saving 66MB).
        #[inline]
        fn anc(x: u32, threshold: u32, dfs_parent: &[u32]) -> u32 {
            if x > threshold {
                dfs_parent[x as usize]
            } else {
                0
            }
        }

        #[inline]
        fn eval(
            v: u32,
            threshold: u32,
            semi: &[u32],
            label: &mut [u32],
            dfs_parent: &mut [u32],
            scratch: &mut Vec<u32>,
        ) -> u32 {
            let a = anc(v, threshold, dfs_parent);
            if a == 0 {
                return label[v as usize];
            }
            if anc(a, threshold, dfs_parent) != 0 {
                compress(v, threshold, semi, label, dfs_parent, scratch);
            }
            label[v as usize]
        }

        fn compress(
            v: u32,
            threshold: u32,
            semi: &[u32],
            label: &mut [u32],
            dfs_parent: &mut [u32],
            scratch: &mut Vec<u32>,
        ) {
            scratch.clear();
            let mut x = v;
            while anc(anc(x, threshold, dfs_parent), threshold, dfs_parent) != 0 {
                scratch.push(x);
                x = dfs_parent[x as usize];
            }
            while let Some(y) = scratch.pop() {
                let a = dfs_parent[y as usize];
                if semi[label[a as usize] as usize] < semi[label[y as usize] as usize] {
                    label[y as usize] = label[a as usize];
                }
                dfs_parent[y as usize] = anc(a, threshold, dfs_parent);
            }
        }

        let n = vertex.len();
        let mut semi: Vec<u32> = (0..n as u32).collect();
        let mut label: Vec<u32> = (0..n as u32).collect();
        let mut idom_pre = vec![u32::MAX; n];
        if n > 0 {
            idom_pre[0] = 0;
        }
        let mut bucket_head = vec![u32::MAX; n];
        let mut scratch: Vec<u32> = Vec::with_capacity(16);

        for w in (1..n).rev() {
            let wu = w;
            let threshold_before = w as u32;
            let preds = &rev_sources[rev_starts[wu] as usize..rev_starts[wu + 1] as usize];
            for &v in preds {
                let u = eval(
                    v,
                    threshold_before,
                    &semi,
                    &mut label,
                    &mut dfs_parent,
                    &mut scratch,
                );
                if semi[u as usize] < semi[wu] {
                    semi[wu] = semi[u as usize];
                }
            }
            let sw = semi[wu] as usize;
            idom_pre[wu] = bucket_head[sw];
            bucket_head[sw] = w as u32;
            let p = dfs_parent[wu] as usize;
            let threshold_after = (w - 1) as u32;
            let mut v = bucket_head[p];
            while v != u32::MAX {
                let vu = v as usize;
                let next_v = idom_pre[vu];
                let u = eval(
                    v,
                    threshold_after,
                    &semi,
                    &mut label,
                    &mut dfs_parent,
                    &mut scratch,
                );
                idom_pre[vu] = if semi[u as usize] < semi[vu] {
                    u
                } else {
                    p as u32
                };
                v = next_v;
            }
            bucket_head[p] = u32::MAX;
        }
        for w in 1..n {
            if idom_pre[w] != semi[w] {
                idom_pre[w] = idom_pre[idom_pre[w] as usize];
            }
        }
        drop(dfs_parent);
        drop(semi);
        drop(label);
        drop(bucket_head);
        drop(scratch);

        // Build `idoms` first and drop `idom_pre` (66MB) BEFORE allocating
        // `retained` (134MB) so peak memory stays minimal.
        let mut idoms = vec![u32::MAX; node_count];
        for (w, &node) in vertex.iter().enumerate() {
            idoms[node as usize] = vertex[idom_pre[w] as usize];
        }
        drop(idom_pre);

        let mut retained = vec![0.0f64; node_count];
        for &node in &vertex {
            retained[node as usize] = raw.nodes.self_sizes[node as usize] as f64;
        }
        for w in (1..vertex.len()).rev() {
            let child = vertex[w] as usize;
            let dom = idoms[child] as usize;
            retained[dom] += retained[child];
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
        self.get_retained_entries_with_mode(top_n, false)
    }

    /// Return the largest individual nodes by retained size. On very large
    /// snapshots the default is an explicitly marked self-size approximation;
    /// callers that can afford the dominator peak may request an exact result.
    pub fn get_retained_entries_exact(&mut self, top_n: usize) -> crate::Result<RetainedResult> {
        self.get_retained_entries_with_mode(top_n, true)
    }

    fn get_retained_entries_with_mode(
        &mut self,
        top_n: usize,
        exact: bool,
    ) -> crate::Result<RetainedResult> {
        self.ensure_raw()?;

        if top_n == 0 {
            return Ok(RetainedResult {
                approximate: !exact && self.raw.as_ref().unwrap().meta.node_count > 5_000_000,
                retained: Vec::new(),
            });
        }
        if !exact && self.raw.as_ref().unwrap().meta.node_count > 5_000_000 {
            let raw = self.raw.as_ref().unwrap();
            let mut heap: std::collections::BinaryHeap<std::cmp::Reverse<(u32, usize)>> =
                std::collections::BinaryHeap::with_capacity(top_n + 1);
            for node_index in 0..raw.meta.node_count {
                let size = raw.nodes.self_sizes[node_index];
                if size == 0 {
                    continue;
                }
                if heap.len() < top_n {
                    heap.push(std::cmp::Reverse((size, node_index)));
                } else if size > heap.peek().unwrap().0 .0 {
                    heap.pop();
                    heap.push(std::cmp::Reverse((size, node_index)));
                }
            }
            let mut selected: Vec<(u32, usize)> = heap.into_iter().map(|r| r.0).collect();
            selected.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            return Ok(RetainedResult {
                approximate: true,
                retained: selected
                    .into_iter()
                    .map(|(size, idx)| {
                        let node = Self::create_node(raw, idx);
                        RetainedEntry {
                            node_index: idx,
                            name: node.name,
                            type_: node.type_,
                            self_size: size as usize,
                            retained_size: size as usize,
                            approximate: true,
                        }
                    })
                    .collect(),
            });
        }

        self.ensure_retained()?;
        let raw = self.raw.as_ref().unwrap();
        let rd = self.retained_data.as_ref().unwrap();
        let mut heap: std::collections::BinaryHeap<std::cmp::Reverse<(usize, usize)>> =
            std::collections::BinaryHeap::with_capacity(top_n + 1);
        for (idx, &size) in rd.retained.iter().enumerate() {
            if size <= 0.0 {
                continue;
            }
            let size = size as usize;
            if heap.len() < top_n {
                heap.push(std::cmp::Reverse((size, idx)));
            } else if size > heap.peek().unwrap().0 .0 {
                heap.pop();
                heap.push(std::cmp::Reverse((size, idx)));
            }
        }
        let mut indexed: Vec<(usize, usize)> = heap.into_iter().map(|r| (r.0 .1, r.0 .0)).collect();
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

    /// Aggregate *exclusive* retained sizes by node name and type.
    pub fn retained_summary(
        &mut self,
        top: usize,
        filter: Option<&str>,
    ) -> crate::Result<HeapSnapshotSummary> {
        self.ensure_retained()?;
        self.ensure_raw_summary()?;
        let raw_summary = self.raw_summary.as_ref().unwrap();
        let total_size = raw_summary.total_size;
        let total_count = raw_summary.total_count;
        let raw = self.raw.as_ref().unwrap();
        let rd = self.retained_data.as_ref().unwrap();
        let node_count = raw.meta.node_count;

        let mut by_name: AHashMap<u32, (usize, usize)> = AHashMap::new();
        let mut by_type: AHashMap<u32, (usize, usize)> = AHashMap::new();
        let col = &raw.nodes;
        let retained = &rd.retained;
        let idoms = &rd.idoms;
        let num_threads = rayon::current_num_threads().max(1);
        let num_chunks = (num_threads * 4).max(1);
        let chunk_size = node_count.div_ceil(num_chunks).max(1);
        let partials: Vec<(AHashMap<u32, (usize, usize)>, AHashMap<u32, (usize, usize)>)> = (0
            ..num_chunks)
            .into_par_iter()
            .map(|c| {
                let start = (c * chunk_size).max(1).min(node_count);
                let end = ((c + 1) * chunk_size).min(node_count);
                let mut local_name: AHashMap<u32, (usize, usize)> = AHashMap::new();
                let mut local_type: AHashMap<u32, (usize, usize)> = AHashMap::new();
                for n in start..end {
                    let ret = retained[n] as usize;
                    if ret == 0 {
                        continue;
                    }
                    let name_idx = col.names[n];
                    let type_idx = col.types[n] as u32;
                    let dom = idoms[n] as usize;
                    let (dom_name, dom_type) = if dom < node_count && dom != n {
                        (col.names[dom], col.types[dom] as u32)
                    } else {
                        (u32::MAX, u32::MAX)
                    };
                    if dom_name != name_idx {
                        let e = local_name.entry(name_idx).or_insert((0, 0));
                        e.0 += ret;
                        e.1 += 1;
                    }
                    if dom_type != type_idx {
                        let e = local_type.entry(type_idx).or_insert((0, 0));
                        e.0 += ret;
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
        let mut raw_names: Vec<(u32, usize, usize)> = by_name
            .into_iter()
            .filter(|&(_, (sz, _))| sz > 0)
            .map(|(idx, (sz, cnt))| (idx, sz, cnt))
            .collect();
        raw_names.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let mut by_node_name: HashMap<String, TypeSummary> = HashMap::new();
        for (idx, size, count) in raw_names {
            if top != usize::MAX && by_node_name.len() >= top {
                break;
            }
            let name = raw
                .strings
                .resolve(&raw.mmap, idx as usize)
                .unwrap_or_else(|| format!("<string#{}>", idx));
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
            total_size,
            total_count,
            by_node_name,
            by_node_type: types.into_iter().collect(),
        })
    }

    /// Top instances whose (case-insensitive) name contains `query`, ranked by
    /// retained size. Powers `hprof inspect --name`.
    pub fn search_nodes(&mut self, query: &str, top: usize) -> crate::Result<Vec<RetainedEntry>> {
        self.ensure_retained()?;
        let raw = self.raw.as_ref().unwrap();
        let rd = self.retained_data.as_ref().unwrap();
        let col = &raw.nodes;

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
                    query,
                    false,
                )
            })
            .collect();
        drop(spans_guard);

        if !matching_strings.iter().any(|&m| m) {
            return Ok(Vec::new());
        }

        let retained = &rd.retained;
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
                    let ret = retained[n] as usize;
                    if ret == 0 {
                        continue;
                    }
                    found.push((n, ret));
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
}
