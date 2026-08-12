//! V8 heap allocation timeline (`.heaptimeline`) parser with full analysis:
//!
//! - by-type summary
//! - top allocation names (constructor names) with per-type split
//! - allocation sites (stack traces from the serialized trace tree)
//! - per-name stack attribution
//! - object-growth profile from the `samples` array
//!
//! Format notes (verified against V8 `heap-snapshot-generator.cc` and the
//! DevTools frontend `AllocationProfile.ts`):
//! - `trace_function_infos`: flat, 6 fields per record
//!   `[function_id, name, script_name, script_id, line, column]`
//! - `trace_tree`: the DevTools frontend re-serializes the V8 tree into a
//!   *flat* children layout: each node occupies 5 slots of its parent's
//!   children array — `[id, function_info_index, count, size, children]` —
//!   without per-node brackets.
//! - `samples`: flat pairs `[timestamp_us, last_assigned_id]`.
//!
//! The whole file is mmap'd once and cached on the handle, so repeated
//! queries (e.g. from the web UI) are cheap. Strings are decoded lazily.

use std::collections::{HashMap, HashSet};
use std::fs;

use memmap2::Mmap;
use regex::Regex;

use crate::heapsnapshot::find_marker;
use crate::types::*;

// ---------------------------------------------------------------------------
// Parsed structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct TraceInfo {
    name_idx: u32,
    script_idx: u32,
    line: u32,
    column: u32,
}

#[derive(Debug, Clone)]
struct TraceNode {
    func_idx: u32,
    node_id: u32,
    count: u32,
    size: u32,
    parent: Option<usize>,
}

struct TimelineData {
    mmap: Mmap,
    // compact per-node arrays (one entry per node record)
    node_count: usize,
    types: Vec<u8>,
    names: Vec<u32>,
    sizes: Vec<u32>,
    tids: Vec<u32>,
    type_names: Vec<String>,
    total_allocated: usize,
    // strings table: byte spans (relative to `strings_start`) + lazy decode memo
    strings_start: usize,
    string_spans: Vec<(u32, u32)>,
    string_memo: Vec<Option<String>>,
    // allocation stacks
    trace_infos: Vec<TraceInfo>,
    trace_nodes: Vec<TraceNode>,
    trace_by_id: HashMap<u32, usize>,
    // growth samples
    samples: Vec<(u64, u64)>,
}

// ---------------------------------------------------------------------------
// Low-level byte helpers
// ---------------------------------------------------------------------------

#[inline]
fn read_num(data: &[u8], pos: &mut usize) -> u64 {
    // skip separators; stop at ']' (returns 0 without advancing)
    while *pos < data.len() && (data[*pos] == b',' || data[*pos].is_ascii_whitespace()) {
        *pos += 1;
    }
    let mut v: u64 = 0;
    while *pos < data.len() && data[*pos].is_ascii_digit() {
        v = v * 10 + (data[*pos] - b'0') as u64;
        *pos += 1;
    }
    v
}

/// Find the index of the closing `]` for the array opened at `open`.
fn find_array_end(data: &[u8], open: usize) -> usize {
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

/// Decode one JSON string from `raw[start..end]` (escape sequences included).
fn decode_json_string(raw: &[u8]) -> String {
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

// ---------------------------------------------------------------------------
// Trace tree parsing (flat DevTools layout)
// ---------------------------------------------------------------------------

/// Parse the children array starting at `data[*pos] == '['` (recursive).
/// Each child node occupies 4 numbers + one nested children array.
fn parse_children(data: &[u8], pos: &mut usize, parent: Option<usize>, out: &mut Vec<TraceNode>) {
    // pos is at '['
    *pos += 1;
    loop {
        // skip whitespace/commas
        while *pos < data.len() && (data[*pos] == b',' || data[*pos].is_ascii_whitespace()) {
            *pos += 1;
        }
        if *pos >= data.len() {
            return;
        }
        if data[*pos] == b']' {
            *pos += 1;
            return;
        }
        // read the 4 numbers of a child node
        let id = read_num(data, pos) as u32;
        let fii = read_num(data, pos) as u32;
        let count = read_num(data, pos) as u32;
        let size = read_num(data, pos) as u32;
        let idx = out.len();
        out.push(TraceNode {
            func_idx: fii,
            node_id: id,
            count,
            size,
            parent,
        });
        // node's own children array
        while *pos < data.len() && data[*pos] != b'[' {
            *pos += 1;
        }
        if *pos < data.len() {
            parse_children(data, pos, Some(idx), out);
        }
    }
}

// ---------------------------------------------------------------------------
// TimelineData construction
// ---------------------------------------------------------------------------

fn parse_timeline(file_path: &str, meta: &SnapshotMeta) -> crate::Result<TimelineData> {
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
    let trace_node_offset = node_fields
        .iter()
        .position(|f| f == "trace_node_id")
        .ok_or(Error::UnsupportedLayout)?;

    let mmap = unsafe { Mmap::map(&fs::File::open(file_path)?)? };
    let data = &mmap[..];

    // ---- nodes: stream numbers directly into compact arrays ----
    let nodes_marker = b"\"nodes\":[";
    let nodes_start = find_marker(data, nodes_marker).ok_or(Error::HeaderParseFailed)?;
    let nodes_open = nodes_start + nodes_marker.len() - 1; // position of '['
    let nodes_end = find_array_end(data, nodes_open);
    let nodes_data = &data[nodes_open + 1..=nodes_end];

    let mut types: Vec<u8> = Vec::with_capacity(nodes_data.len() / 24);
    let mut names: Vec<u32> = Vec::with_capacity(nodes_data.len() / 24);
    let mut sizes: Vec<u32> = Vec::with_capacity(nodes_data.len() / 24);
    let mut tids: Vec<u32> = Vec::with_capacity(nodes_data.len() / 24);
    let mut total_allocated: usize = 0;
    let mut pos = 0usize;
    loop {
        // read node_field_count numbers; stop at the array end
        let mut field = 0usize;
        let mut node_vals = [0u32; 16];
        while field < node_field_count {
            // skip commas/whitespace, stop at ']'
            while pos < nodes_data.len() && nodes_data[pos] != b']' && !nodes_data[pos].is_ascii_digit() {
                pos += 1;
            }
            if pos >= nodes_data.len() || nodes_data[pos] == b']' {
                break;
            }
            node_vals[field] = read_num(nodes_data, &mut pos) as u32;
            field += 1;
        }
        if field < node_field_count {
            break;
        }
        types.push(node_vals[type_offset] as u8);
        names.push(node_vals[name_offset]);
        let size = node_vals[self_size_offset];
        sizes.push(size);
        total_allocated += size as usize;
        tids.push(node_vals[trace_node_offset]);
    }
    let node_count = types.len();

    // ---- strings table ----
    let strings_marker = b"\"strings\":[";
    let strings_start = find_marker(data, strings_marker).ok_or(Error::HeaderParseFailed)? + strings_marker.len();
    let strings_end = find_array_end(data, strings_start - 1);

    let mut string_spans: Vec<(u32, u32)> = Vec::with_capacity(1 << 16);
    let mut sp = strings_start;
    while sp < strings_end {
        if data[sp] != b'"' {
            sp += 1;
            continue;
        }
        let start = sp + 1;
        sp += 1;
        while sp < strings_end {
            if data[sp] == b'\\' {
                sp += 2;
            } else if data[sp] == b'"' {
                string_spans.push(((start - strings_start) as u32, (sp - strings_start) as u32));
                sp += 1;
                break;
            } else {
                sp += 1;
            }
        }
    }
    let string_memo: Vec<Option<String>> = vec![None; string_spans.len()];

    // ---- trace_function_infos: 6 fields per record ----
    let fi_marker = b"\"trace_function_infos\":[";
    let fi_start = find_marker(data, fi_marker).ok_or(Error::HeaderParseFailed)? + fi_marker.len();
    let fi_end = find_array_end(data, fi_start - 1);
    let fi_nums = crate::heapsnapshot::parse_numbers_fast(&data[fi_start..=fi_end]);
    let mut trace_infos: Vec<TraceInfo> = Vec::with_capacity(fi_nums.len() / 6);
    for c in fi_nums.chunks(6) {
        if c.len() < 6 {
            break;
        }
        trace_infos.push(TraceInfo {
            name_idx: c[1],
            script_idx: c[2],
            line: c[4],
            column: c[5],
        });
    }

    // ---- trace_tree: flat layout, stride 5 ----
    let tt_marker = b"\"trace_tree\":[";
    let tt_start = find_marker(data, tt_marker).ok_or(Error::HeaderParseFailed)? + tt_marker.len();
    let tt_end = find_array_end(data, tt_start - 1);
    let tt_data = &data[tt_start - 1..=tt_end]; // includes the root '['
    let mut trace_nodes: Vec<TraceNode> = Vec::new();
    let mut tpos = 1usize; // past the root '['
    // root node: fields are [id, function_info_index, count, size, children]
    let root_id = read_num(tt_data, &mut tpos) as u32;
    let root_fii = read_num(tt_data, &mut tpos) as u32;
    let root_count = read_num(tt_data, &mut tpos) as u32;
    let root_size = read_num(tt_data, &mut tpos) as u32;
    trace_nodes.push(TraceNode {
        func_idx: root_fii,
        node_id: root_id,
        count: root_count,
        size: root_size,
        parent: None,
    });
    // skip to the root's children array '['
    while tpos < tt_data.len() && tt_data[tpos] != b'[' {
        tpos += 1;
    }
    if tpos < tt_data.len() {
        parse_children(tt_data, &mut tpos, Some(0), &mut trace_nodes);
    }
    let mut trace_by_id: HashMap<u32, usize> = HashMap::with_capacity(trace_nodes.len());
    for (i, tn) in trace_nodes.iter().enumerate() {
        trace_by_id.insert(tn.node_id, i);
    }

    // ---- samples: flat pairs (timestamp_us, last_assigned_id) ----
    let smp_marker = b"\"samples\":[";
    let smp_start = find_marker(data, smp_marker).ok_or(Error::HeaderParseFailed)? + smp_marker.len();
    let smp_end = find_array_end(data, smp_start - 1);
    let smp_nums = crate::heapsnapshot::parse_numbers_fast(&data[smp_start..=smp_end]);
    let mut samples: Vec<(u64, u64)> = Vec::with_capacity(smp_nums.len() / 2);
    for c in smp_nums.chunks(2) {
        if c.len() < 2 {
            break;
        }
        samples.push((c[0] as u64, c[1] as u64));
    }

    Ok(TimelineData {
        mmap,
        node_count,
        types,
        names,
        sizes,
        tids,
        type_names: node_types.iter().cloned().collect(),
        total_allocated,
        strings_start,
        string_spans,
        string_memo,
        trace_infos,
        trace_nodes,
        trace_by_id,
        samples,
    })
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

fn resolve_string(data: &mut TimelineData, idx: u32) -> String {
    if let Some(cached) = data.string_memo.get(idx as usize) {
        if let Some(s) = cached {
            return s.clone();
        }
    }
    let span = data.string_spans.get(idx as usize).cloned();
    let decoded = match span {
        Some((s, e)) => {
            let raw = &data.mmap[data.strings_start + s as usize..data.strings_start + e as usize];
            decode_json_string(raw)
        }
        None => format!("<str {}>", idx),
    };
    if let Some(slot) = data.string_memo.get_mut(idx as usize) {
        *slot = Some(decoded.clone());
    }
    decoded
}

/// Walk a trace tree node's ancestors root->leaf and decode each frame.
fn stack_frames(data: &mut TimelineData, ti: usize) -> Vec<TimelineStackFrame> {
    let mut frames: Vec<TimelineStackFrame> = Vec::new();
    let mut cur = Some(ti);
    let mut guard = 0;
    while let Some(ci) = cur {
        let (fii, parent) = match data.trace_nodes.get(ci) {
            Some(tn) => (tn.func_idx, tn.parent),
            None => break,
        };
        let (name_idx, script_idx, line, column) = match data.trace_infos.get(fii as usize) {
            Some(info) => (info.name_idx, info.script_idx, info.line, info.column),
            None => (0, 0, 0, 0),
        };
        let name = if name_idx == 0 {
            "(root)".to_string()
        } else {
            resolve_string(data, name_idx)
        };
        let script = resolve_string(data, script_idx);
        frames.push(TimelineStackFrame {
            name,
            script,
            line,
            column,
        });
        cur = parent;
        guard += 1;
        if guard > 128 {
            break;
        }
    }
    frames.reverse();
    frames
}

pub struct HeapTimeline {
    file_path: String,
    snapshot_meta: Option<SnapshotMeta>,
    data: Option<TimelineData>,
}

impl HeapTimeline {
    pub fn new(file_path: String) -> Self {
        Self {
            file_path,
            snapshot_meta: None,
            data: None,
        }
    }

    pub fn meta(&mut self) -> crate::Result<&SnapshotMeta> {
        if self.snapshot_meta.is_none() {
            let mut snap = crate::HeapSnapshot::new(self.file_path.clone());
            let meta = snap.meta()?.clone();
            self.snapshot_meta = Some(meta);
        }
        Ok(self.snapshot_meta.as_ref().unwrap())
    }

    fn data(&mut self) -> crate::Result<&mut TimelineData> {
        if self.data.is_none() {
            let meta = self.meta()?.clone();
            self.data = Some(parse_timeline(&self.file_path, &meta)?);
        }
        Ok(self.data.as_mut().unwrap())
    }

    fn resolve_string(&mut self, idx: u32) -> String {
        let data = match self.data.as_mut() {
            Some(d) => d,
            None => return format!("<str {}>", idx),
        };
        resolve_string(data, idx)
    }

    /// Legacy summary grouped by node type. `filter` matches type names.
    pub fn stream_summary(
        &mut self,
        top: Option<usize>,
        filter: Option<&str>,
    ) -> crate::Result<HeapTimelineSummary> {
        let data = self.data()?;
        let mut by_type_idx: HashMap<u32, TimelineTypeSummary> = HashMap::new();
        for i in 0..data.node_count {
            let size = data.sizes[i] as usize;
            if size == 0 {
                continue;
            }
            let ty = data.types[i] as u32;
            let entry = by_type_idx.entry(ty).or_insert(TimelineTypeSummary {
                allocated: 0,
                freed: 0,
                count: 0,
            });
            entry.allocated += size;
            entry.count += 1;
        }
        let filter_re = filter.and_then(|f| Regex::new(f).ok());
        let top = top.unwrap_or(30);
        let mut by_type: HashMap<String, TimelineTypeSummary> = HashMap::new();
        for (type_idx, info) in by_type_idx {
            let type_name = data
                .type_names
                .get(type_idx as usize)
                .cloned()
                .unwrap_or_else(|| type_idx.to_string());
            if let Some(re) = &filter_re {
                if !re.is_match(&type_name) {
                    continue;
                }
            }
            by_type.insert(type_name, info);
        }
        let mut sorted: Vec<_> = by_type.into_iter().collect();
        sorted.sort_by(|a, b| b.1.allocated.cmp(&a.1.allocated));
        sorted.truncate(top);
        Ok(HeapTimelineSummary {
            total_allocated: data.total_allocated,
            total_freed: 0,
            by_type: sorted.into_iter().collect(),
        })
    }

    /// Top allocation names by total self-size, with per-type split.
    pub fn top_names(
        &mut self,
        top: Option<usize>,
        filter: Option<&str>,
    ) -> crate::Result<TimelineNamesResult> {
        let data = self.data()?;
        // name_idx -> (size, count, per-type (type_idx, size, count))
        let mut agg: HashMap<u32, (u64, u64, Vec<(u8, u64, u64)>)> = HashMap::new();
        for i in 0..data.node_count {
            let size = data.sizes[i] as u64;
            if size == 0 {
                continue;
            }
            let name = data.names[i];
            let ty = data.types[i];
            let e = agg.entry(name).or_insert((0, 0, Vec::new()));
            e.0 += size;
            e.1 += 1;
            match e.2.iter_mut().find(|t| t.0 == ty) {
                Some(t) => {
                    t.1 += size;
                    t.2 += 1;
                }
                None => e.2.push((ty, size, 1)),
            }
        }
        let mut sorted: Vec<(u32, u64, u64)> = agg.iter().map(|(&k, &(s, c, _))| (k, s, c)).collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));

        let total_size = data.total_allocated;
        let total_count = data.node_count;

        let filter_re = filter.and_then(|f| Regex::new(f).ok());
        let top = top.unwrap_or(30);
        let mut entries: Vec<TimelineNameEntry> = Vec::new();
        for (name_idx, size, count) in sorted {
            let name = self.resolve_string(name_idx);
            if let Some(re) = &filter_re {
                if !re.is_match(&name) {
                    continue;
                }
            }
            let types = agg[&name_idx]
                .2
                .iter()
                .map(|(t, s, c)| TimelineNameType {
                    name: self
                        .data
                        .as_ref()
                        .and_then(|d| d.type_names.get(*t as usize).cloned())
                        .unwrap_or_else(|| t.to_string()),
                    size: *s as usize,
                    count: *c as usize,
                })
                .collect();
            entries.push(TimelineNameEntry {
                name,
                size: size as usize,
                count: count as usize,
                types,
            });
            if entries.len() >= top {
                break;
            }
        }
        Ok(TimelineNamesResult {
            total_size,
            total_count,
            entries,
        })
    }

    /// Top allocation sites: stacks with the bytes/count recorded on their
    /// leaf frames in the trace tree. `filter` matches any frame name.
    pub fn top_stacks(
        &mut self,
        top: Option<usize>,
        filter: Option<&str>,
    ) -> crate::Result<TimelineStacksResult> {
        let filter_re = filter.and_then(|f| Regex::new(f).ok());
        let top = top.unwrap_or(30);
        let _ = self.data()?;

        let mut stack_memo: HashMap<usize, Vec<TimelineStackFrame>> = HashMap::new();
        let mut agg: HashMap<Vec<TimelineStackFrame>, (u64, u64)> = HashMap::new();
        let node_count = self.data.as_ref().map(|d| d.trace_nodes.len()).unwrap_or(0);
        for ti in 0..node_count {
            let (count, size) = {
                let data = self.data.as_ref().unwrap();
                let tn = &data.trace_nodes[ti];
                (tn.count, tn.size)
            };
            if count == 0 {
                continue;
            }
            let frames = match stack_memo.get(&ti) {
                Some(f) => f.clone(),
                None => {
                    let f = stack_frames(self.data.as_mut().unwrap(), ti);
                    stack_memo.insert(ti, f.clone());
                    f
                }
            };
            if let Some(re) = &filter_re {
                if !frames.iter().any(|f| re.is_match(&f.name)) {
                    continue;
                }
            }
            let e = agg.entry(frames).or_insert((0, 0));
            e.0 += size as u64;
            e.1 += count as u64;
        }
        let mut total_size = 0u64;
        let mut total_count = 0u64;
        let mut entries: Vec<TimelineStackEntry> = agg
            .into_iter()
            .map(|(stack, (size, count))| {
                total_size += size;
                total_count += count;
                TimelineStackEntry {
                    size: size as usize,
                    count: count as usize,
                    stack,
                }
            })
            .collect();
        entries.sort_by(|a, b| b.size.cmp(&a.size));
        entries.truncate(top);
        Ok(TimelineStacksResult {
            total_size: total_size as usize,
            total_count: total_count as usize,
            entries,
        })
    }

    /// Stack distribution for nodes whose name matches `name_re`.
    pub fn name_stacks(
        &mut self,
        name_re: &str,
        top: Option<usize>,
    ) -> crate::Result<TimelineNameStacksResult> {
        let re = Regex::new(name_re)
            .map_err(|e| Error::Other(format!("invalid name regex: {e}")))?;
        let top = top.unwrap_or(10);

        // find matching name indices (decode each distinct name once)
        let distinct_names: Vec<u32> = {
            let data = self.data()?;
            let mut set: HashSet<u32> = HashSet::with_capacity(1 << 16);
            for &n in &data.names {
                set.insert(n);
            }
            set.into_iter().collect()
        };
        let mut matching: HashSet<u32> = HashSet::new();
        for &n in &distinct_names {
            if re.is_match(&self.resolve_string(n)) {
                matching.insert(n);
            }
        }

        // aggregate stacks over matching nodes
        let mut stack_memo: HashMap<usize, Vec<TimelineStackFrame>> = HashMap::new();
        let mut agg: HashMap<Vec<TimelineStackFrame>, (u64, u64)> = HashMap::new();
        let mut total_size = 0u64;
        let mut total_count = 0u64;
        {
            let data = self.data()?;
            for i in 0..data.node_count {
                if !matching.contains(&data.names[i]) {
                    continue;
                }
                let size = data.sizes[i] as u64;
                if size == 0 {
                    continue;
                }
                total_size += size;
                total_count += 1;
                let tid = data.tids[i];
                let Some(&ti) = data.trace_by_id.get(&tid) else {
                    continue;
                };
                let frames = match stack_memo.get(&ti) {
                    Some(f) => f.clone(),
                    None => {
                        let f = stack_frames(data, ti);
                        stack_memo.insert(ti, f.clone());
                        f
                    }
                };
                let e = agg.entry(frames).or_insert((0, 0));
                e.0 += size;
                e.1 += 1;
            }
        }
        let mut entries: Vec<TimelineStackEntry> = agg
            .into_iter()
            .map(|(stack, (size, count))| TimelineStackEntry {
                size: size as usize,
                count: count as usize,
                stack,
            })
            .collect();
        entries.sort_by(|a, b| b.size.cmp(&a.size));
        entries.truncate(top);
        Ok(TimelineNameStacksResult {
            name: name_re.to_string(),
            total_size: total_size as usize,
            total_count: total_count as usize,
            entries,
        })
    }

    /// Object-growth profile from the samples array.
    pub fn growth(&mut self) -> crate::Result<TimelineGrowth> {
        let data = self.data()?;
        let span_us = data.samples.last().map(|s| s.0).unwrap_or(0);
        let objects_start = data.samples.first().map(|s| s.1).unwrap_or(0);
        let objects_end = data.samples.last().map(|s| s.1).unwrap_or(0);
        Ok(TimelineGrowth {
            span_us,
            objects_start,
            objects_end,
            samples: data.samples.iter().map(|&(t, o)| [t, o]).collect(),
        })
    }

    /// Node names containing `query` (case-insensitive), ranked by size.
    pub fn search_strings(&mut self, query: &str) -> crate::Result<Vec<TimelineNameEntry>> {
        let q = query.to_lowercase();
        let data = self.data()?;
        let mut agg: HashMap<u32, (u64, u64)> = HashMap::new();
        for i in 0..data.node_count {
            let size = data.sizes[i] as u64;
            if size == 0 {
                continue;
            }
            let e = agg.entry(data.names[i]).or_insert((0, 0));
            e.0 += size;
            e.1 += 1;
        }
        let mut sorted: Vec<(u32, u64, u64)> = agg
            .into_iter()
            .map(|(k, (s, c))| (k, s, c))
            .collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        let mut out = Vec::new();
        for (name_idx, size, count) in sorted {
            let name = self.resolve_string(name_idx);
            if !name.to_lowercase().contains(&q) {
                continue;
            }
            out.push(TimelineNameEntry {
                name,
                size: size as usize,
                count: count as usize,
                types: Vec::new(),
            });
            if out.len() >= 100 {
                break;
            }
        }
        Ok(out)
    }
}
