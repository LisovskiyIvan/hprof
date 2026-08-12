use std::collections::HashMap;
use std::fs;
use std::sync::Arc;

use rayon::prelude::*;

use crate::types::*;

pub struct HeapProfile {
    file_path: String,
    data: Option<Arc<HeapProfileResult>>,
    /// Cached full flat summary (no top, no filter). All `summarize`,
    /// `summarize_cumulative`, `diff` calls slice from this. Massive speedup
    /// for repeated calls (e.g. UI server hitting /summary then /cumulative).
    full_flat: Option<Arc<HeapProfileSummary>>,
}

impl HeapProfile {
    pub fn new(file_path: String) -> Self {
        Self {
            file_path,
            data: None,
            full_flat: None,
        }
    }

    pub fn data(&mut self) -> crate::Result<&HeapProfileResult> {
        if self.data.is_none() {
            let result = parse_profile(&self.file_path)?;
            self.data = Some(Arc::new(result));
        }
        Ok(self.data.as_ref().unwrap())
    }

    /// Return an Arc handle to the parsed tree. Used by `to_flamegraph`,
    /// `to_dot`, etc. so they don't take `&mut self` and can run concurrently.
    fn data_arc(&mut self) -> crate::Result<Arc<HeapProfileResult>> {
        if self.data.is_none() {
            let _ = self.data()?;
        }
        Ok(self.data.as_ref().unwrap().clone())
    }

    pub fn summarize(
        &mut self,
        top: Option<usize>,
        filter: Option<&str>,
    ) -> crate::Result<HeapProfileSummary> {
        // Cache only when (top=MAX, filter=None). Otherwise slice from cache if
        // available, else re-compute.
        let wants_full = top.is_none() && filter.is_none();
        if wants_full {
            if self.full_flat.is_none() {
                let data = self.data_arc()?;
                let s = compute_flat_summary(&data)?;
                self.full_flat = Some(Arc::new(s));
            }
            return Ok(self.full_flat.as_ref().unwrap().as_ref().clone());
        }

        if let Some(full) = &self.full_flat {
            return Ok(slice_flat_summary(full.as_ref(), top, filter));
        }

        let data = self.data_arc()?;
        let s = compute_flat_summary(&data)?;
        Ok(slice_flat_summary(&s, top, filter))
    }

    /// Produces a cumulative summary, optionally applying `focus`/`ignore` filters
    /// (modelled on Go pprof's behaviour).
    pub fn summarize_cumulative(
        &mut self,
        top: Option<usize>,
        filters: &FilterOptions,
    ) -> crate::Result<CumulativeSummary> {
        let data = self.data_arc()?;
        compute_cumulative_summary(&data, top, filters)
    }

    pub fn flatten(&mut self) -> crate::Result<Vec<FlatCallFrame>> {
        let data = self.data_arc()?;
        let mut result = Vec::new();
        walk_flatten(&data.head, &[], &mut result);
        Ok(result)
    }

    /// Build a flamegraph tree. Single recursive pass; children are processed
    /// in parallel via rayon where the tree is wide enough to benefit.
    pub fn to_flamegraph(&mut self, filters: &FilterOptions) -> crate::Result<FlamegraphFrame> {
        let data = self.data_arc()?;
        let focus_re = filters
            .focus
            .as_ref()
            .and_then(|s| regex::Regex::new(s).ok());
        let hide_re = filters
            .hide
            .as_ref()
            .and_then(|s| regex::Regex::new(s).ok());

        Ok(
            build_flame_frame(&data.head, focus_re.as_ref(), hide_re.as_ref()).unwrap_or(
                FlamegraphFrame {
                    name: "root".to_string(),
                    self_size: 0,
                    total_size: 0,
                    children: vec![],
                },
            ),
        )
    }

    /// Emit a DOT-format call graph. Single pass over the tree collecting node
    /// sizes and edge weights; emits DOT string at the end.
    pub fn to_dot(&mut self, top: Option<usize>, filters: &FilterOptions) -> crate::Result<String> {
        let data = self.data_arc()?;
        let top = top.unwrap_or(usize::MAX);
        let focus_re = filters
            .focus
            .as_ref()
            .and_then(|s| regex::Regex::new(s).ok());

        // Single pass: collect node self-sizes and parent→child edge weights.
        let mut node_sizes: HashMap<String, usize> = HashMap::new();
        let mut edges: HashMap<(String, String), usize> = HashMap::new();
        walk_collect(&data.head, None, &mut node_sizes, &mut edges);

        // Focus filter: keep only nodes that match (or have a matching descendant).
        let kept: Option<std::collections::HashSet<String>> = if let Some(re) = &focus_re {
            let mut kept: std::collections::HashSet<String> = std::collections::HashSet::new();
            mark_subtree_match(&data.head, re, &mut kept);
            Some(kept)
        } else {
            None
        };

        // Sort by size desc, keep top-N.
        let mut sorted_nodes: Vec<(String, usize)> = node_sizes.into_iter().collect();
        sorted_nodes.sort_by(|a, b| b.1.cmp(&a.1));
        if let Some(ref kept_set) = kept {
            sorted_nodes.retain(|(k, _)| kept_set.contains(k));
        }
        let kept_names: std::collections::HashSet<String> = sorted_nodes
            .iter()
            .take(top)
            .map(|(k, _)| k.clone())
            .collect();
        let sorted_nodes: Vec<(String, usize)> = sorted_nodes.into_iter().take(top).collect();

        let max_size = sorted_nodes.first().map(|(_, s)| *s).unwrap_or(1).max(1);

        // Pre-allocate a generous buffer; DOT output is verbose.
        let estimated = sorted_nodes.len() * 200 + edges.len() * 4 * 80 + 256;
        let mut out = String::with_capacity(estimated);
        out.push_str("digraph heapprofile {\n");
        out.push_str("  node [shape=box, style=filled, fontname=\"Helvetica\", fontsize=10];\n");
        out.push_str("  edge [fontname=\"Helvetica\", fontsize=9];\n");
        out.push_str("  rankdir=TB;\n");

        for (name, size) in &sorted_nodes {
            let pct = *size as f64 / max_size as f64;
            let label = format!("{}\\n{}", dot_escape(name), format_bytes(*size));
            let color = color_for(pct);
            out.push_str(&format!(
                "  \"{}\" [label=\"{}\", fillcolor=\"{}\"];\n",
                dot_escape(name),
                label,
                color
            ));
        }

        // Sort edges by weight desc; keep up to 4x top.
        let mut sorted_edges: Vec<((String, String), usize)> = edges.into_iter().collect();
        sorted_edges.sort_by(|a, b| b.1.cmp(&a.1));
        let edge_cap = top.saturating_mul(4);
        for ((from, to), w) in sorted_edges.iter().take(edge_cap) {
            if !kept_names.contains(from) || !kept_names.contains(to) {
                continue;
            }
            if *w == 0 {
                continue;
            }
            out.push_str(&format!(
                "  \"{}\" -> \"{}\" [label=\"{}\", penwidth={}];\n",
                dot_escape(from),
                dot_escape(to),
                format_bytes(*w),
                ((*w as f64 / max_size as f64) * 4.0).clamp(0.5, 4.0)
            ));
        }

        out.push_str("}\n");
        Ok(out)
    }

    /// Build a hierarchical treemap: group by URL host, then path, then function.
    pub fn to_treemap(&mut self, filters: &FilterOptions) -> crate::Result<TreemapNode> {
        let data = self.data_arc()?;
        let focus_re = filters
            .focus
            .as_ref()
            .and_then(|s| regex::Regex::new(s).ok());

        let mut by_host: HashMap<String, TreemapAccum> = HashMap::new();
        let mut total = 0usize;

        walk_treemap(&data.head, &mut by_host, &mut total, focus_re.as_ref());

        let mut root = TreemapNode {
            name: "root".to_string(),
            size: 0,
            children: Vec::new(),
        };

        let mut hosts: Vec<(String, TreemapAccum)> = by_host.into_iter().collect();
        hosts.sort_by(|a, b| b.1.size.cmp(&a.1.size));
        for (host, accum) in hosts {
            let mut host_node = TreemapNode {
                name: host,
                size: 0,
                children: Vec::new(),
            };
            let mut fns: Vec<(String, usize)> = accum.fns.into_iter().collect();
            fns.sort_by(|a, b| b.1.cmp(&a.1));
            for (fn_key, size) in fns {
                host_node.children.push(TreemapNode {
                    name: fn_key,
                    size,
                    children: Vec::new(),
                });
                host_node.size += size;
            }
            root.size += host_node.size;
            root.children.push(host_node);
        }

        Ok(root)
    }

    /// Diff two profiles of the same type. Uses cached full summaries when
    /// available so diffing a profile against itself is essentially free.
    pub fn diff(&mut self, baseline: &mut HeapProfile) -> crate::Result<ProfileDiff> {
        let base = self.summarize(None, None)?;
        let other = baseline.summarize(None, None)?;

        Ok(ProfileDiff {
            baseline_total: other.total_size,
            profile_total: base.total_size,
            delta_total: base.total_size as i64 - other.total_size as i64,
            by_frame: diff_maps(&other.by_frame, &base.by_frame),
            by_url: diff_maps(&other.by_url, &base.by_url),
            by_function: diff_maps(&other.by_function, &base.by_function),
        })
    }
}

// ============================================================================
// Free-standing tree walkers. They take `&HeapProfileNode` so multiple can run
// concurrently on the same Arc-ed tree.
// ============================================================================

fn frame_label(node: &HeapProfileNode) -> String {
    let fn_name = if node.call_frame.function_name.is_empty() {
        "(anonymous)".to_string()
    } else {
        node.call_frame.function_name.clone()
    };
    let url = if node.call_frame.url.is_empty() {
        "<no-url>".to_string()
    } else {
        node.call_frame.url.clone()
    };
    format!("{} @ {}:{}", fn_name, url, node.call_frame.line_number + 1)
}

fn fn_name_of(node: &HeapProfileNode) -> String {
    if node.call_frame.function_name.is_empty() {
        "(anonymous)".to_string()
    } else {
        node.call_frame.function_name.clone()
    }
}

fn url_of(node: &HeapProfileNode) -> String {
    if node.call_frame.url.is_empty() {
        "<no-url>".to_string()
    } else {
        node.call_frame.url.clone()
    }
}

/// Walk the tree once and populate the flat `by_frame`/`by_url`/`by_function`
/// maps. Single-threaded but with minimal allocation.
fn compute_flat_summary(data: &HeapProfileResult) -> crate::Result<HeapProfileSummary> {
    let mut by_frame: HashMap<String, usize> = HashMap::new();
    let mut by_url: HashMap<String, usize> = HashMap::new();
    let mut by_function: HashMap<String, usize> = HashMap::new();
    let mut total_size = 0usize;

    // Iterative DFS to avoid blowing the stack on deep trees.
    let mut stack: Vec<&HeapProfileNode> = vec![&data.head];
    while let Some(node) = stack.pop() {
        let self_size = node.self_size;
        if self_size > 0 {
            let frame = frame_label(node);
            let url = url_of(node);
            let fn_name = fn_name_of(node);
            total_size += self_size;
            *by_frame.entry(frame).or_insert(0) += self_size;
            *by_url.entry(url).or_insert(0) += self_size;
            *by_function.entry(fn_name).or_insert(0) += self_size;
        }
        for child in &node.children {
            stack.push(child);
        }
    }

    Ok(HeapProfileSummary {
        total_size,
        by_frame,
        by_url,
        by_function,
    })
}

/// Slice a full flat summary by (top, filter). Cheap: O(n log n) sort.
/// When `filter` is set, total_size is recomputed to be the sum of matching
/// entries (matching pprof's `-filter` semantics where the total reflects only
/// what's visible after filtering).
fn slice_flat_summary(
    full: &HeapProfileSummary,
    top: Option<usize>,
    filter: Option<&str>,
) -> HeapProfileSummary {
    let top = top.unwrap_or(usize::MAX);
    let filter_re = filter.and_then(|f| regex::Regex::new(f).ok());

    let trim = |m: &HashMap<String, usize>| -> HashMap<String, usize> {
        let mut v: Vec<(String, usize)> = m
            .iter()
            .filter(|(_, s)| **s > 0)
            .filter(|(k, _)| match &filter_re {
                None => true,
                Some(re) => re.is_match(k.as_str()),
            })
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        if top != usize::MAX {
            v.truncate(top);
        }
        v.into_iter().collect()
    };

    let by_frame = trim(&full.by_frame);
    let by_url = trim(&full.by_url);
    let by_function = trim(&full.by_function);

    // When filter is set, recompute total from the filtered frames.
    let total_size = if filter_re.is_some() {
        by_frame.values().sum()
    } else {
        full.total_size
    };

    HeapProfileSummary {
        total_size,
        by_frame,
        by_url,
        by_function,
    }
}

/// Compute cumulative (self + descendants) attribution. Walks the tree once
/// with the path stack; for each leaf sample, attributes self_size to every
/// frame along the path (cumulative) and to the leaf (flat, unless ignored).
fn compute_cumulative_summary(
    data: &HeapProfileResult,
    top: Option<usize>,
    filters: &FilterOptions,
) -> crate::Result<CumulativeSummary> {
    let top = top.unwrap_or(usize::MAX);
    let focus_re = filters
        .focus
        .as_ref()
        .and_then(|s| regex::Regex::new(s).ok());
    let ignore_re = filters
        .ignore
        .as_ref()
        .and_then(|s| regex::Regex::new(s).ok());

    let mut self_frame: HashMap<String, usize> = HashMap::new();
    let mut self_url: HashMap<String, usize> = HashMap::new();
    let mut self_fn: HashMap<String, usize> = HashMap::new();
    let mut cum_frame: HashMap<String, usize> = HashMap::new();
    let mut cum_url: HashMap<String, usize> = HashMap::new();
    let mut cum_fn: HashMap<String, usize> = HashMap::new();
    let mut count_frame: HashMap<String, usize> = HashMap::new();
    let mut total_size = 0usize;

    // Iterative DFS with path tracking. Avoids recursion overhead.
    // Stack entries: (node, path_frames, path_urls, path_fns, child_idx).
    // We need to push the paths onto the stack to track them per node — for
    // very deep trees this is O(n*depth) memory which is acceptable for V8
    // profiles (typical depth <50).
    enum Frame<'a> {
        Enter {
            node: &'a HeapProfileNode,
            path_frames: Vec<String>,
            path_urls: Vec<String>,
            path_fns: Vec<String>,
            has_focus_match_in_path: bool,
        },
    }

    let mut stack: Vec<Frame> = vec![Frame::Enter {
        node: &data.head,
        path_frames: Vec::with_capacity(32),
        path_urls: Vec::with_capacity(32),
        path_fns: Vec::with_capacity(32),
        has_focus_match_in_path: false,
    }];

    while let Some(frame) = stack.pop() {
        let Frame::Enter {
            node,
            mut path_frames,
            mut path_urls,
            mut path_fns,
            has_focus_match_in_path,
        } = frame;

        let fn_name = fn_name_of(node);
        let url = url_of(node);
        let frame_label = format!("{} @ {}:{}", fn_name, url, node.call_frame.line_number + 1);

        // Determine focus match for this node + path.
        let node_matches_focus = match &focus_re {
            None => true,
            Some(re) => re.is_match(&frame_label) || re.is_match(&url) || re.is_match(&fn_name),
        };
        let path_has_match = has_focus_match_in_path || node_matches_focus;

        path_frames.push(frame_label.clone());
        path_urls.push(url.clone());
        path_fns.push(fn_name.clone());

        let self_size = node.self_size;
        if self_size > 0 && (focus_re.is_none() || path_has_match) {
            let leaf_ignored = ignore_re
                .as_ref()
                .map(|re| re.is_match(&frame_label) || re.is_match(&url) || re.is_match(&fn_name))
                .unwrap_or(false);

            total_size += self_size;
            if !leaf_ignored {
                *self_frame.entry(frame_label.clone()).or_insert(0) += self_size;
                *self_url.entry(url.clone()).or_insert(0) += self_size;
                *self_fn.entry(fn_name.clone()).or_insert(0) += self_size;
                *count_frame.entry(frame_label.clone()).or_insert(0) += 1;
            }

            // Cumulative: every frame on the path gets self_size.
            for f in path_frames.iter() {
                *cum_frame.entry(f.clone()).or_insert(0) += self_size;
            }
            for u in path_urls.iter() {
                *cum_url.entry(u.clone()).or_insert(0) += self_size;
            }
            for f in path_fns.iter() {
                *cum_fn.entry(f.clone()).or_insert(0) += self_size;
            }
        }

        // Push children (in reverse so they're processed in order — not strictly necessary).
        for child in node.children.iter().rev() {
            stack.push(Frame::Enter {
                node: child,
                path_frames: path_frames.clone(),
                path_urls: path_urls.clone(),
                path_fns: path_fns.clone(),
                has_focus_match_in_path: path_has_match,
            });
        }
    }

    Ok(CumulativeSummary {
        total_size,
        by_frame: merge_size_entries(self_frame, cum_frame, count_frame, top),
        by_url: merge_size_entries(self_url, cum_url, HashMap::new(), top),
        by_function: merge_size_entries(self_fn, cum_fn, HashMap::new(), top),
    })
}

fn walk_flatten(node: &HeapProfileNode, stack: &[String], result: &mut Vec<FlatCallFrame>) {
    let fn_name = fn_name_of(node);
    let url = url_of(node);
    let frame = format!("{} @ {}:{}", fn_name, url, node.call_frame.line_number + 1);
    let mut next_stack = stack.to_vec();
    next_stack.push(frame);

    if node.self_size > 0 {
        result.push(FlatCallFrame {
            function_name: fn_name,
            url,
            line_number: node.call_frame.line_number,
            column_number: node.call_frame.column_number,
            self_size: node.self_size,
            stack: next_stack.clone(),
        });
    }

    for child in &node.children {
        walk_flatten(child, &next_stack, result);
    }
}

/// Recursive flamegraph builder. Returns None when the entire sub-tree is
/// empty (after focus/hide filtering).
fn build_flame_frame(
    node: &HeapProfileNode,
    focus_re: Option<&regex::Regex>,
    hide_re: Option<&regex::Regex>,
) -> Option<FlamegraphFrame> {
    // Focus: skip if neither this node nor any descendant matches.
    if let Some(re) = focus_re {
        if !subtree_contains_match(node, re) {
            return None;
        }
    }

    let fn_name = fn_name_of(node);
    let url = url_of(node);
    let name = format!("{} @ {}:{}", fn_name, url, node.call_frame.line_number + 1);

    let is_hidden = hide_re
        .map(|re| re.is_match(&fn_name) || re.is_match(&url) || re.is_match(&name))
        .unwrap_or(false);

    let mut self_size = node.self_size;
    let mut children: Vec<FlamegraphFrame> = Vec::new();

    // Process children in parallel if there are many of them. The threshold
    // below is chosen so we don't pay rayon overhead on tiny trees.
    if node.children.len() >= 16 {
        let results: Vec<Option<FlamegraphFrame>> = node
            .children
            .par_iter()
            .map(|c| build_flame_frame(c, focus_re, hide_re))
            .collect();
        for cf in results.into_iter().flatten() {
            if is_hidden {
                self_size += cf.self_size;
                children.extend(cf.children);
            } else {
                children.push(cf);
            }
        }
    } else {
        for child in &node.children {
            if let Some(cf) = build_flame_frame(child, focus_re, hide_re) {
                if is_hidden {
                    self_size += cf.self_size;
                    children.extend(cf.children);
                } else {
                    children.push(cf);
                }
            }
        }
    }

    if self_size == 0 && children.is_empty() {
        return None;
    }

    let total_size = self_size + children.iter().map(|c| c.total_size).sum::<usize>();
    Some(FlamegraphFrame {
        name,
        self_size,
        total_size,
        children,
    })
}

fn matches(node: &HeapProfileNode, re: &regex::Regex) -> bool {
    re.is_match(&node.call_frame.function_name)
        || re.is_match(&node.call_frame.url)
        || re.is_match(&format!(
            "{} @ {}:{}",
            node.call_frame.function_name,
            node.call_frame.url,
            node.call_frame.line_number + 1
        ))
}

fn subtree_contains_match(node: &HeapProfileNode, re: &regex::Regex) -> bool {
    if matches(node, re) {
        return true;
    }
    // Use rayon for wide trees.
    if node.children.len() >= 16 {
        node.children
            .par_iter()
            .any(|c| subtree_contains_match(c, re))
    } else {
        node.children.iter().any(|c| subtree_contains_match(c, re))
    }
}

fn walk_collect(
    node: &HeapProfileNode,
    parent_name: Option<&str>,
    node_sizes: &mut HashMap<String, usize>,
    edges: &mut HashMap<(String, String), usize>,
) {
    let name = frame_label(node);
    *node_sizes.entry(name.clone()).or_insert(0) += node.self_size;
    if let Some(p) = parent_name {
        *edges.entry((p.to_string(), name.clone())).or_insert(0) += node.self_size;
    }
    for child in &node.children {
        walk_collect(child, Some(&name), node_sizes, edges);
    }
}

fn mark_subtree_match(
    node: &HeapProfileNode,
    re: &regex::Regex,
    kept: &mut std::collections::HashSet<String>,
) -> bool {
    let name = frame_label(node);
    let mut hit = re.is_match(&name);
    for c in &node.children {
        hit |= mark_subtree_match(c, re, kept);
    }
    if hit {
        kept.insert(name);
    }
    hit
}

fn walk_treemap(
    node: &HeapProfileNode,
    by_host: &mut HashMap<String, TreemapAccum>,
    total: &mut usize,
    focus_re: Option<&regex::Regex>,
) {
    let fn_name = fn_name_of(node);
    let url = url_of(node);

    if node.self_size > 0 {
        let matches_focus = match focus_re {
            None => true,
            Some(re) => re.is_match(&fn_name) || re.is_match(&url),
        };
        if matches_focus {
            let host = host_of(&url);
            let path = path_of(&url);
            let e = by_host.entry(host).or_default();
            e.size += node.self_size;
            let key = format!("{}:{}", path, fn_name);
            *e.fns.entry(key).or_insert(0) += node.self_size;
            *total += node.self_size;
        }
    }

    for child in &node.children {
        walk_treemap(child, by_host, total, focus_re);
    }
}

#[derive(Default)]
struct TreemapAccum {
    size: usize,
    fns: HashMap<String, usize>,
}

fn host_of(url: &str) -> String {
    let s = url.split("://").nth(1).unwrap_or(url);
    let h = s.split('/').next().unwrap_or(s);
    if h.is_empty() {
        "<no-host>".to_string()
    } else {
        h.to_string()
    }
}

fn path_of(url: &str) -> String {
    let s = url.split("://").nth(1).unwrap_or(url);
    if let Some(idx) = s.find('/') {
        s[idx..].to_string()
    } else {
        "/".to_string()
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn merge_size_entries(
    self_map: HashMap<String, usize>,
    cum_map: HashMap<String, usize>,
    count_map: HashMap<String, usize>,
    top: usize,
) -> HashMap<String, SizeEntry> {
    let mut keys: Vec<String> = self_map.keys().chain(cum_map.keys()).cloned().collect();
    keys.sort();
    keys.dedup();

    let mut entries: Vec<(String, SizeEntry)> = keys
        .into_iter()
        .map(|k| {
            let self_size = *self_map.get(&k).unwrap_or(&0);
            let cumulative_size = *cum_map.get(&k).unwrap_or(&0);
            let count = *count_map.get(&k).unwrap_or(&0);
            (
                k,
                SizeEntry {
                    self_size,
                    cumulative_size,
                    count,
                },
            )
        })
        .collect();

    if top != usize::MAX {
        entries.sort_by(|a, b| b.1.cumulative_size.cmp(&a.1.cumulative_size));
        entries.truncate(top);
    }

    entries.into_iter().collect()
}

fn diff_maps(
    baseline: &HashMap<String, usize>,
    profile: &HashMap<String, usize>,
) -> Vec<DiffEntry> {
    let mut keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    keys.extend(baseline.keys().cloned());
    keys.extend(profile.keys().cloned());

    let mut entries: Vec<DiffEntry> = keys
        .into_iter()
        .map(|k| {
            let b = *baseline.get(&k).unwrap_or(&0) as i64;
            let p = *profile.get(&k).unwrap_or(&0) as i64;
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

fn color_for(pct: f64) -> &'static str {
    if pct > 0.75 {
        "#ef4444"
    } else if pct > 0.50 {
        "#f97316"
    } else if pct > 0.25 {
        "#eab308"
    } else {
        "#3b82f6"
    }
}

fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ============================================================================
// Streaming parser (no full-DOM serde_json) — mmap + recursive descent
// ============================================================================

use crate::heapsnapshot::{decode_json_string, find_marker};

fn parse_profile(file_path: &str) -> crate::Result<HeapProfileResult> {
    let mmap = unsafe { memmap2::Mmap::map(&fs::File::open(file_path)?)? };
    let data = &mmap[..];

    let head_marker = b"\"head\":";
    let head_start = find_marker(data, head_marker)
        .map(|s| s + head_marker.len())
        .ok_or(Error::HeaderParseFailed)?;
    let mut pos = head_start;
    let head = parse_profile_node(data, &mut pos);

    let start_time = number_after_key(data, b"\"startTime\"");
    let end_time = number_after_key(data, b"\"endTime\"");

    Ok(HeapProfileResult {
        head,
        start_time,
        end_time,
    })
}

#[inline]
fn skip_ws(data: &[u8], pos: &mut usize) {
    while *pos < data.len() && (data[*pos] == b' ' || data[*pos] == b'\n' || data[*pos] == b'\t' || data[*pos] == b'\r') {
        *pos += 1;
    }
}

/// Parse a JSON string starting at `data[*pos] == '"'`, returning its content.
fn parse_string(data: &[u8], pos: &mut usize) -> String {
    // pos at '"'
    *pos += 1;
    let start = *pos;
    while *pos < data.len() {
        if data[*pos] == b'\\' {
            *pos += 2;
        } else if data[*pos] == b'"' {
            let end = *pos;
            *pos += 1;
            return decode_json_string(&data[start..end]);
        } else {
            *pos += 1;
        }
    }
    decode_json_string(&data[start..*pos])
}

/// Parse the JSON number at `*pos` as u64 (digits only, optional sign).
fn parse_u64(data: &[u8], pos: &mut usize) -> u64 {
    skip_ws(data, pos);
    if *pos < data.len() && data[*pos] == b'-' {
        *pos += 1;
    }
    let mut v: u64 = 0;
    while *pos < data.len() && data[*pos].is_ascii_digit() {
        v = v * 10 + (data[*pos] - b'0') as u64;
        *pos += 1;
    }
    v
}

/// Parse the JSON number at `*pos` as f64 (handles fractions/exponents).
fn parse_f64(data: &[u8], pos: &mut usize) -> f64 {
    skip_ws(data, pos);
    let start = *pos;
    if *pos < data.len() && (data[*pos] == b'-' || data[*pos] == b'+') {
        *pos += 1;
    }
    while *pos < data.len() && (data[*pos].is_ascii_digit() || matches!(data[*pos], b'.' | b'e' | b'E' | b'-' | b'+')) {
        *pos += 1;
    }
    std::str::from_utf8(&data[start..*pos])
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0)
}

/// Skip a JSON value (scalar, object or array) starting at `*pos`.
fn skip_value(data: &[u8], pos: &mut usize) {
    skip_ws(data, pos);
    if *pos >= data.len() {
        return;
    }
    match data[*pos] {
        b'"' => {
            let _ = parse_string(data, pos);
        }
        b'{' | b'[' => {
            let mut depth = 0i32;
            while *pos < data.len() {
                match data[*pos] {
                    b'"' => {
                        let _ = parse_string(data, pos);
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        *pos += 1;
                        if depth <= 0 {
                            return;
                        }
                    }
                    _ => *pos += 1,
                }
            }
        }
        _ => {
            let _ = parse_f64(data, pos);
        }
    }
}

/// Parse a call frame object: `{"functionName":...,"scriptId":...,"url":...,"lineNumber":N,"columnNumber":N}`.
fn parse_call_frame(data: &[u8], pos: &mut usize) -> CallFrame {
    let mut frame = CallFrame {
        function_name: String::new(),
        script_id: String::new(),
        url: String::new(),
        line_number: 0,
        column_number: 0,
    };
    skip_ws(data, pos);
    if *pos < data.len() && data[*pos] == b'{' {
        *pos += 1;
    }
    loop {
        skip_ws(data, pos);
        if *pos >= data.len() || data[*pos] == b'}' {
            *pos += 1;
            break;
        }
        let key = parse_string(data, pos);
        skip_ws(data, pos);
        if *pos < data.len() && data[*pos] == b':' {
            *pos += 1;
        }
        skip_ws(data, pos);
        match key.as_str() {
            "functionName" => frame.function_name = parse_string(data, pos),
            "scriptId" => {
                // V8 writes scriptId as a string; tolerate numbers too
                frame.script_id = if *pos < data.len() && data[*pos] == b'"' {
                    parse_string(data, pos)
                } else {
                    parse_f64(data, pos).to_string()
                };
            }
            "url" => frame.url = parse_string(data, pos),
            "lineNumber" => frame.line_number = parse_f64(data, pos) as i32,
            "columnNumber" => frame.column_number = parse_f64(data, pos) as i32,
            _ => skip_value(data, pos),
        }
        skip_ws(data, pos);
        if *pos < data.len() && data[*pos] == b',' {
            *pos += 1;
        }
    }
    frame
}

/// Parse one profile tree node: `{"callFrame":{...},"selfSize":N,"id":N,"children":[...]}`.
fn parse_profile_node(data: &[u8], pos: &mut usize) -> HeapProfileNode {
    let mut node = HeapProfileNode {
        call_frame: CallFrame {
            function_name: String::new(),
            script_id: String::new(),
            url: String::new(),
            line_number: 0,
            column_number: 0,
        },
        self_size: 0,
        children: Vec::new(),
    };
    skip_ws(data, pos);
    if *pos < data.len() && data[*pos] == b'{' {
        *pos += 1;
    }
    loop {
        skip_ws(data, pos);
        if *pos >= data.len() || data[*pos] == b'}' {
            *pos += 1;
            break;
        }
        let key = parse_string(data, pos);
        skip_ws(data, pos);
        if *pos < data.len() && data[*pos] == b':' {
            *pos += 1;
        }
        skip_ws(data, pos);
        match key.as_str() {
            "callFrame" => node.call_frame = parse_call_frame(data, pos),
            "selfSize" => node.self_size = parse_u64(data, pos) as usize,
            "id" => {
                let _ = parse_u64(data, pos);
            }
            "children" => {
                skip_ws(data, pos);
                if *pos < data.len() && data[*pos] == b'[' {
                    *pos += 1;
                }
                loop {
                    skip_ws(data, pos);
                    if *pos >= data.len() || data[*pos] == b']' {
                        *pos += 1;
                        break;
                    }
                    node.children.push(parse_profile_node(data, pos));
                    skip_ws(data, pos);
                    if *pos < data.len() && data[*pos] == b',' {
                        *pos += 1;
                    }
                }
            }
            _ => skip_value(data, pos),
        }
        skip_ws(data, pos);
        if *pos < data.len() && data[*pos] == b',' {
            *pos += 1;
        }
    }
    node
}

/// Find `key` in the file and parse the f64 following it (used for start/end time).
fn number_after_key(data: &[u8], key: &[u8]) -> f64 {
    let Some(start) = find_marker(data, key) else {
        return 0.0;
    };
    let mut pos = start + key.len();
    skip_ws(data, &mut pos);
    if pos < data.len() && data[pos] == b':' {
        pos += 1;
    }
    parse_f64(data, &mut pos)
}

