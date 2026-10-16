use std::collections::HashMap;
use std::fs;
use std::io::Read;

use ahash::AHashMap;
use memchr::{memchr, memchr2};
use memmap2::Mmap;
use rayon::prelude::*;

use crate::types::*;

pub struct HeapSnapshot {
    file_path: String,
    meta: Option<SnapshotMeta>,
    raw: Option<RawData>,
    edge_starts: Option<Vec<u32>>,
    /// Cachedcompact raw summary (`(name_idx, size, count)` sorted by size desc).
    /// Avoids decoding all 100k+ inline strings when only top-K rows or total
    /// heap size are needed.
    raw_summary: Option<RawSummary>,
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

struct RawSummary {
    total_size: usize,
    total_count: usize,
    /// `(representative_name_idx, total_size, total_count)` sorted by `size` descending.
    by_name_idx: Vec<(u32, usize, usize)>,
    by_node_type: HashMap<String, TypeSummary>,
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
/// iteration (predecessors) and the shortest-path BFS.
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

/// First incoming edge per node (the "owner" node), in node order.
/// `index[n]` is the record index of the first node with an edge into `n`
/// (`u32::MAX` when `n` has no incoming edges). Edge type/name are resolved
/// on demand for the few hops walked by `retainer_chain`, saving 134MB on a
/// 16.7M-node snapshot.
pub struct ParentMap {
    pub index: Vec<u32>,
}

/// Node records in struct-of-arrays layout, one entry per node. The fields
/// V8 writes per record are split into typed columns; fields we never read
/// (`trace_node_id`) are dropped entirely.
pub struct NodeColumns {
    /// index into `node_types` (V8 has a small fixed set; u8 is ample)
    pub types: Vec<u8>,
    /// index into the string table
    pub names: Vec<u32>,
    pub ids: Vec<u32>,
    pub self_sizes: Vec<u32>,
    pub edge_counts: Vec<u32>,
    /// V8 detachedness marker; zero means attached/unknown.
    pub detachedness: Vec<u8>,
}

pub const EDGE_SYNC_STRIDE: usize = 1024;

/// Compact edge columns: `types` (1B/edge) + `to_nodes` (4B/edge) + coarse
/// `sync_offsets` into the mmap for reading `name_or_index` on demand without
/// keeping an extra 162MB `Vec<u32>` resident in memory.
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
    /// byte position of `[` opening the `"edges":[` array
    edges_open: Option<usize>,
    /// byte range of the `edges` array content (start..=end), located on demand
    edges_range: Option<(usize, usize)>,
    pub edges: Option<EdgeColumns>,
    pub strings: StringTable,
    pub edge_offsets: EdgeFieldOffsets,
    pub node_types: Vec<String>,
    pub edge_types: Vec<String>,
    pub node_field_count: usize,
    pub edge_field_count: usize,
}

/// Strings larger than this are decoded on every `resolve` without memoizing,
/// so multi-megabyte V8 strings (e.g. data: URLs) never pile up in memory.
const MEMO_MAX_STRING_BYTES: usize = 64 * 1024;
const MEMO_MAX_ENTRIES: usize = 32 * 1024;

pub(crate) struct StringScanState {
    /// offset into the mmap where the string content starts (past `[`)
    start: usize,
    /// offset from which to search backwards for `"strings":[` if `start == 0`
    search_from: usize,
    /// per-string (start, end) relative to `start`, content still escaped
    spans: Vec<(u32, u32)>,
    cursor: usize,
    done: bool,
}

impl StringScanState {
    fn ensure_start(&mut self, mmap: &Mmap) {
        if self.start == 0 && !self.done {
            let data = &mmap[..];
            let strings_marker = b"\"strings\":[";
            let from = self.search_from.min(data.len());
            if let Some(rel) = rfind_marker(&data[from..], strings_marker) {
                let s = from + rel + strings_marker.len();
                self.start = s;
                self.cursor = s;
            } else if let Some(abs) = find_marker(data, strings_marker) {
                let s = abs + strings_marker.len();
                self.start = s;
                self.cursor = s;
            } else {
                self.done = true;
            }
        }
    }

    fn scan_until(&mut self, mmap: &Mmap, target_idx: usize) {
        self.ensure_start(mmap);
        if self.done || self.spans.len() > target_idx {
            return;
        }
        let data = &mmap[..];
        let start = self.start;
        let end = data.len();
        let mut i = self.cursor;

        while i < end && self.spans.len() <= target_idx {
            if data[i] == b',' {
                i += 1;
                if i < end && data[i] == b'\n' {
                    i += 1;
                }
            }
            if i >= end {
                self.done = true;
                break;
            }
            let b = data[i];
            let s0 = if b == b'"' {
                i + 1
            } else if b == b']' {
                self.done = true;
                break;
            } else {
                match memchr2(b'"', b']', &data[i..]) {
                    Some(off) => {
                        let pos = i + off;
                        if data[pos] == b']' {
                            self.cursor = pos;
                            self.done = true;
                            break;
                        }
                        pos + 1
                    }
                    None => {
                        self.cursor = end;
                        self.done = true;
                        break;
                    }
                }
            };
            i = s0;
            loop {
                match memchr2(b'"', b'\\', &data[i..]) {
                    Some(off) => {
                        let pos = i + off;
                        if data[pos] == b'\\' {
                            i = (pos + 2).min(end);
                        } else {
                            self.spans.push(((s0 - start) as u32, (pos - start) as u32));
                            i = pos + 1;
                            break;
                        }
                    }
                    None => {
                        i = end;
                        self.done = true;
                        break;
                    }
                }
            }
        }
        self.cursor = i;
    }
}

impl std::ops::Deref for StringScanState {
    type Target = [(u32, u32)];
    fn deref(&self) -> &Self::Target {
        &self.spans
    }
}

/// Lazy string table: byte spans into the mmap are scanned incrementally on
/// demand with SIMD `memchr2`, and decoded strings are cached in a bounded
/// hash map (0 bytes upfront instead of a 38MB `Vec<Option<String>>`).
pub struct StringTable {
    state: std::sync::RwLock<StringScanState>,
    memo: std::sync::RwLock<AHashMap<usize, String>>,
}

impl StringTable {
    /// Create a lazy string table whose `"strings":[` array opens at `strings_open`.
    pub(crate) fn new_at(strings_open: usize) -> StringTable {
        let start = strings_open + 1;
        StringTable {
            state: std::sync::RwLock::new(StringScanState {
                start,
                search_from: 0,
                spans: Vec::new(),
                cursor: start,
                done: false,
            }),
            memo: std::sync::RwLock::new(AHashMap::new()),
        }
    }

    /// Create a fully lazy string table that locates `"strings":[` on first access.
    fn new_deferred(search_from: usize) -> StringTable {
        StringTable {
            state: std::sync::RwLock::new(StringScanState {
                start: 0,
                search_from,
                spans: Vec::new(),
                cursor: 0,
                done: false,
            }),
            memo: std::sync::RwLock::new(AHashMap::new()),
        }
    }

    #[cfg(test)]
    fn from_spans_for_test(start: usize, spans: Vec<(u32, u32)>) -> StringTable {
        StringTable {
            state: std::sync::RwLock::new(StringScanState {
                start,
                search_from: 0,
                spans,
                cursor: start,
                done: true,
            }),
            memo: std::sync::RwLock::new(AHashMap::new()),
        }
    }

    /// Ensure all string spans are scanned and return `(start_offset, spans_guard)`.
    pub(crate) fn all_spans<'a>(
        &'a self,
        mmap: &Mmap,
    ) -> (usize, std::sync::RwLockReadGuard<'a, StringScanState>) {
        {
            let mut state = self.state.write().unwrap();
            if !state.done {
                if state.spans.capacity() == 0 {
                    state.spans.reserve(1 << 16);
                }
                state.scan_until(mmap, usize::MAX);
            }
        }
        let guard = self.state.read().unwrap();
        let start = guard.start;
        (start, guard)
    }

    /// Decode string `idx` (escapes and surrogate pairs handled).
    pub fn resolve(&self, mmap: &Mmap, idx: usize) -> Option<String> {
        if let Some(cached) = self.memo.read().unwrap().get(&idx) {
            return Some(cached.clone());
        }
        let (start, span) = {
            let mut state = self.state.write().unwrap();
            if idx >= state.spans.len() && !state.done {
                state.scan_until(mmap, idx);
            }
            let span = state.spans.get(idx).copied()?;
            (state.start, span)
        };
        let raw = &mmap[start + span.0 as usize..start + span.1 as usize];
        if span.1.saturating_sub(span.0) as usize > MEMO_MAX_STRING_BYTES {
            return Some(decode_json_string(raw));
        }
        let decoded = decode_json_string(raw);
        let mut memo = self.memo.write().unwrap();
        if memo.len() < MEMO_MAX_ENTRIES {
            memo.insert(idx, decoded.clone());
        }
        Some(decoded)
    }

    /// All strings containing `query` (case-insensitive), up to `limit`.
    pub fn search(&self, mmap: &Mmap, query: &str, limit: usize) -> Vec<(usize, String)> {
        let (sbase, spans_guard) = self.all_spans(mmap);
        let spans: &[(u32, u32)] = &spans_guard;
        let mmap_bytes = &mmap[..];
        let mut out = Vec::new();
        for (idx, &(s, e)) in spans.iter().enumerate() {
            let raw = &mmap_bytes[sbase + s as usize..sbase + e as usize];
            if raw_string_matches(raw, query, false) {
                let v = decode_json_string(raw);
                out.push((idx, v));
                if out.len() >= limit {
                    break;
                }
            }
        }
        out
    }
}

/// Decode one JSON string from `raw[start..end]` (escape sequences included).
pub(crate) fn decode_json_string(raw: &[u8]) -> String {
    if memchr(b'\\', raw).is_none() {
        return match std::str::from_utf8(raw) {
            Ok(s) => s.to_owned(),
            Err(_) => String::from_utf8_lossy(raw).into_owned(),
        };
    }
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
                        if idx + 4 > len {
                            return 0;
                        }
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
                    let mut cp: u32 = hexv(i + 2) as u32;
                    i += 6;
                    if (0xD800..0xDC00).contains(&cp)
                        && i + 5 < len
                        && raw[i] == b'\\'
                        && raw[i + 1] == b'u'
                    {
                        let lo = hexv(i + 2) as u32;
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

/// Allocation-free match of a raw JSON string slice against `query_name`.
/// Only decodes into a temporary `String` when backslash escapes are present.
#[inline]
fn raw_string_matches(raw: &[u8], query_name: &str, exact: bool) -> bool {
    if query_name.is_ascii() && raw.len() < query_name.len() {
        return false;
    }
    if memchr(b'\\', raw).is_none() {
        if let Ok(s) = std::str::from_utf8(raw) {
            return if exact {
                eq_ci_name(s, query_name)
            } else {
                contains_ci(s, query_name)
            };
        }
    }
    let decoded = decode_json_string(raw);
    if exact {
        eq_ci_name(&decoded, query_name)
    } else {
        contains_ci(&decoded, query_name)
    }
}

/// ASCII-fast, allocation-free case-insensitive substring search. Non-ASCII
/// input falls back to a lowercased comparison. `needle` is the raw (not
/// pre-lowercased) query.
#[inline]
fn contains_ci(hay: &str, needle: &str) -> bool {
    if hay.is_ascii() && needle.is_ascii() {
        let h = hay.as_bytes();
        let n = needle.as_bytes();
        if n.is_empty() {
            return true;
        }
        if n.len() > h.len() {
            return false;
        }
        let n0 = n[0].to_ascii_lowercase();
        let mut i = 0usize;
        while i + n.len() <= h.len() {
            if h[i].to_ascii_lowercase() == n0 {
                let mut j = 1usize;
                while j < n.len() && h[i + j].eq_ignore_ascii_case(&n[j]) {
                    j += 1;
                }
                if j == n.len() {
                    return true;
                }
            }
            i += 1;
        }
        false
    } else {
        hay.to_lowercase().contains(&needle.to_lowercase())
    }
}

/// ASCII-fast, allocation-free case-insensitive equality (non-ASCII input
/// falls back to lowercased comparison, matching the pre-fold behaviour).
#[inline]
fn eq_ci(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        a.eq_ignore_ascii_case(b)
    } else {
        a.to_lowercase() == b.to_lowercase()
    }
}

/// Case-insensitive exact name match that also accepts the V8 display-name
/// suffix after the last `" / "` (e.g. `"system / ExternalStringData"`
/// matches `"ExternalStringData"`).
#[inline]
fn eq_ci_name(hay: &str, needle: &str) -> bool {
    if eq_ci(hay, needle) {
        return true;
    }
    match hay.rfind(" / ") {
        Some(pos) => eq_ci(&hay[pos + 3..], needle),
        None => false,
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

        // nodes are needed by every query; edges and strings are parsed
        // lazily to keep memory and page faults down on multi-GB snapshots.
        let nodes_open = nodes_start + nodes_marker.len() - 1; // position of '['
        let nodes_end = find_array_end(data, nodes_open);
        let nodes_bytes = &data[nodes_open + 1..=nodes_end];

        // `"edges":[` immediately follows the closing `]` of `nodes`. Check the
        // small window right after `nodes_end` (already in L1 cache) so we know
        // where `edges` starts without scanning `nodes` a second time later.
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

        // For small snapshots (< 16 MiB), validate `"strings":[` immediately;
        // for large snapshots, defer locating `"strings":[` until the first
        // string resolution so commands like `sizes` and `detached` never fault
        // the `edges`, `locations`, or `strings` mmap pages into RSS.
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

    fn locate_edges_range(&mut self) -> crate::Result<(usize, usize)> {
        let raw = self.raw.as_mut().unwrap();
        if let Some(r) = raw.edges_range {
            return Ok(r);
        }
        let open = match raw.edges_open {
            Some(o) => o,
            None => {
                let edges_marker = b"\"edges\":[";
                let start =
                    find_marker(&raw.mmap, edges_marker).ok_or(Error::HeaderParseFailed)?;
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

    fn ensure_edges(&mut self) -> crate::Result<()> {
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

    fn create_node(raw: &RawData, node_index: usize) -> HeapSnapshotNode {
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
                let t = raw.node_types.get(type_idx).map(|s| s.as_str()).unwrap_or("");
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
                if !contains_ci(&name, ql) {
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

    /// Build a node from a record index, resolving the name (used by the fast
    /// numeric-sort page path after selection).
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
        raw.for_each_edge_in_range(edge_start, edge_end, |_edge_idx, type_idx, name_or_index_val, to_node| {
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
        });

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
            raw.for_each_edge_in_range(start, end, |_edge_index, type_index, name_index, target| {
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
            });
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
            offset += raw.nodes.edge_counts[i];
        }
        starts.push(offset);
        self.edge_starts = Some(starts);
    }

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
                let u = eval(v, threshold_before, &semi, &mut label, &mut dfs_parent, &mut scratch);
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
                let u = eval(v, threshold_after, &semi, &mut label, &mut dfs_parent, &mut scratch);
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
        // Sort by `size` descending BEFORE resolving strings so we only decode
        // the top `top` constructor names instead of all 100k+ inline strings!
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
                raw_string_matches(&mmap_bytes[sbase + s as usize..sbase + e as usize], query, false)
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

    /// Resolve a DevTools node id to a record index (single scan; ids are not
    /// stored in index order).
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
    /// as `(edge_type, property name / "[i]" / "i")`.
    fn path_edge(
        &self,
        raw: &RawData,
        edge_starts: &[u32],
        from: usize,
        to: usize,
        bracket_element: bool,
    ) -> Option<(String, String)> {
        let start = edge_starts[from] as usize;
        let end = edge_starts[from + 1] as usize;
        let weak_edge_type = raw
            .edge_types
            .iter()
            .position(|t| t == "weak");
        let mut result: Option<(String, String)> = None;
        raw.for_each_edge_in_range(start, end, |_edge_index, type_idx, name_or_index, to_node| {
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
                    .unwrap_or_else(|| format!("#{}", name_or_index))
            };
            result = Some((type_, name));
        });
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

        let n = rd.vertex.len();
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
                raw_string_matches(&mmap_bytes[sbase + s as usize..sbase + e as usize], query_name, exact)
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
        raw.for_each_edge_in_range(start, end, |_edge_index, type_idx, name_or_index, to_node| {
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
        });
        Ok((node, props))
    }

    /// Find incoming object properties/edges by their displayed name.
    /// Pre-matches strings and streams edge chunks in parallel without
    /// allocating `String`s inside the 40.4M-edge loop or materializing
    /// `EdgeColumns`.
    pub fn find_edges(&mut self, query: &EdgeQuery) -> crate::Result<Vec<EdgeMatch>> {
        self.ensure_raw()?;
        if query.name.is_empty() {
            return Ok(Vec::new());
        }
        let (eb_start, eb_end) = self.locate_edges_range()?;
        self.ensure_edge_starts();
        let raw = self.raw.as_ref().unwrap();
        let edge_starts = self.edge_starts.as_ref().unwrap();

        let (sbase, spans_guard) = raw.strings.all_spans(&raw.mmap);
        let spans: &[(u32, u32)] = &spans_guard;
        let mmap_bytes: &[u8] = &raw.mmap[..];
        let num_strings = spans.len();
        let exact = query.exact;
        let query_name = query.name.as_str();

        let matching_strings: Vec<bool> = (0..num_strings)
            .into_par_iter()
            .map(|idx| {
                let (s, e) = spans[idx];
                raw_string_matches(&mmap_bytes[sbase + s as usize..sbase + e as usize], query_name, exact)
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
        let limit = if query.limit == 0 { usize::MAX } else { query.limit };
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
                                let stype = node_types_col
                                    .get(source_idx)
                                    .copied()
                                    .unwrap_or(0) as usize;
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
                        (name_or_idx as usize) < num_strings && matching_strings[name_or_idx as usize]
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
                        out.push((source_idx, etype, name_or_idx, (rec[to_off] / nfc) as usize));
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
            let source_type_index = raw.nodes.types[source_index] as usize;
            let source_type = raw
                .node_types
                .get(source_type_index)
                .cloned()
                .unwrap_or_else(|| source_type_index.to_string());
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
                raw.for_each_edge_in_range(edge_index, edge_index + 1, |_, type_idx, name_or_index, _| {
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
                });
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

    pub fn node_field_count(&self) -> Option<usize> {
        self.raw.as_ref().map(|r| r.node_field_count)
    }

    pub fn edge_field_count(&self) -> Option<usize> {
        self.raw.as_ref().map(|r| r.edge_field_count)
    }

    /// Build a flamegraph for a heap snapshot.
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

    /// Compare object identities between this snapshot (the current/profile)
    /// and `baseline`.
    pub fn object_diff(
        &mut self,
        baseline: &mut HeapSnapshot,
        limit: usize,
    ) -> crate::Result<SnapshotObjectDiff> {
        self.ensure_raw()?;
        baseline.ensure_raw()?;
        let profile_raw = self.raw.as_ref().unwrap();
        let baseline_raw = baseline.raw.as_ref().unwrap();
        Ok(diff_snapshot_objects(baseline_raw, profile_raw, limit))
    }

    /// Return detached V8 nodes, ranked by self size.
    pub fn detached_summary(
        &mut self,
        limit: usize,
        depth: usize,
    ) -> crate::Result<DetachedSummary> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        let mut candidates: Vec<(usize, usize)> = Vec::with_capacity(limit.min(64));
        let mut total_count = 0usize;
        let mut total_size = 0usize;
        for index in 0..raw.meta.node_count {
            let marker = raw.nodes.detachedness.get(index).copied().unwrap_or(0);
            if marker == 0 {
                continue;
            }
            let size = raw.nodes.self_sizes[index] as usize;
            total_count += 1;
            total_size += size;
            if limit == 0 {
                continue;
            }
            candidates.push((index, size));
            if candidates.len() > limit {
                let worst = candidates
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, item)| (item.1, item.0))
                    .map(|(i, _)| i)
                    .unwrap();
                candidates.swap_remove(worst);
            }
        }
        candidates.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let mut entries = Vec::with_capacity(candidates.len());
        for (index, _) in candidates {
            let (node, detachedness) = {
                let raw = self.raw.as_ref().unwrap();
                (
                    snapshot_object(raw, index),
                    raw.nodes.detachedness.get(index).copied().unwrap_or(0),
                )
            };
            let owner_chain = if depth == 0 {
                String::new()
            } else {
                let chain = self.retainer_chain(index, depth)?;
                let parts = chain
                    .iter()
                    .skip(1)
                    .map(|hop| {
                        if hop.name.is_empty() {
                            format!("({})", hop.type_)
                        } else {
                            hop.name.clone()
                        }
                    })
                    .collect::<Vec<_>>();
                if parts.is_empty() {
                    "(none)".to_string()
                } else {
                    parts.join(" → ")
                }
            };
            entries.push(DetachedNode {
                node,
                detachedness,
                owner_chain,
            });
        }

        Ok(DetachedSummary {
            total_count,
            total_size,
            entries,
        })
    }

    /// Build a power-of-two histogram of node self sizes.
    pub fn size_histogram(&mut self) -> crate::Result<SizeHistogram> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        let mut counts = [0usize; 65];
        let mut sizes = [0usize; 65];
        let mut total_size = 0usize;
        for &raw_size in &raw.nodes.self_sizes {
            let size = raw_size as usize;
            let bucket = if size == 0 {
                0
            } else {
                (usize::BITS - size.leading_zeros()) as usize
            };
            counts[bucket] += 1;
            sizes[bucket] += size;
            total_size += size;
        }
        let bits = usize::BITS as usize;
        let mut buckets = Vec::new();
        for bucket in 0..65 {
            let count = counts[bucket];
            if count == 0 {
                continue;
            }
            let min_size = if bucket == 0 {
                0
            } else {
                1usize << (bucket - 1)
            };
            let max_size = if bucket == 0 {
                0
            } else if bucket >= bits {
                usize::MAX
            } else {
                (1usize << bucket) - 1
            };
            buckets.push(SizeBucket {
                min_size,
                max_size,
                count,
                total_size: sizes[bucket],
            });
        }
        Ok(SizeHistogram {
            total_count: raw.nodes.self_sizes.len(),
            total_size,
            buckets,
        })
    }

    /// Summarize strings by their actual contents and reference count.
    /// Streams edge references in parallel directly from the mmap (no 485MB
    /// edge vector allocation) and merges duplicates via a compact `u64`
    /// `(hash40 << 24 | idx)` sort before decoding only the top `limit`
    /// entries.
    pub fn string_stats(&mut self, limit: usize) -> crate::Result<StringStats> {
        self.ensure_raw()?;
        let (eb_start, eb_end) = self.locate_edges_range()?;
        let raw = self.raw.as_ref().unwrap();
        let (sbase, spans_guard) = raw.strings.all_spans(&raw.mmap);
        let spans: &[(u32, u32)] = &spans_guard;
        let num_strings = spans.len();
        if num_strings == 0 {
            return Ok(StringStats {
                total_strings: 0,
                total_bytes: 0,
                referenced_strings: 0,
                referenced_bytes: 0,
                entries: Vec::new(),
            });
        }

        let mut references = vec![0u32; num_strings];
        for &name_index in &raw.nodes.names {
            if let Some(count) = references.get_mut(name_index as usize) {
                *count = count.saturating_add(1);
            }
        }

        let element_type_idx = raw
            .edge_types
            .iter()
            .position(|kind| kind == "element")
            .unwrap_or(usize::MAX);
        let edges_bytes = &raw.mmap[eb_start..=eb_end];
        let efc = raw.edge_field_count;
        let type_off = raw.edge_offsets.type_;
        let name_off = raw.edge_offsets.name_or_index;

        if efc > name_off {
            if let Some(chunks) = aligned_record_chunks_par(edges_bytes, efc) {
                let atomic_refs: &[std::sync::atomic::AtomicU32] = unsafe {
                    std::slice::from_raw_parts(
                        references.as_ptr() as *const std::sync::atomic::AtomicU32,
                        references.len(),
                    )
                };
                chunks.par_iter().for_each(|&(byte_s, byte_e, _, _)| {
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
                                if rec[type_off] as usize != element_type_idx {
                                    let idx = rec[name_off] as usize;
                                    if idx < num_strings {
                                        atomic_refs[idx]
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    }
                                }
                            }
                            continue;
                        }
                        i += 1;
                    }
                });
            } else {
                let flat = parse_numbers_par(edges_bytes);
                for edge in flat.chunks(efc) {
                    if edge.len() <= name_off {
                        continue;
                    }
                    if edge[type_off] as usize == element_type_idx {
                        continue;
                    }
                    let name_index = edge[name_off] as usize;
                    if let Some(count) = references.get_mut(name_index) {
                        *count = count.saturating_add(1);
                    }
                }
            }
        }

        let mmap_bytes = &raw.mmap[..];
        let mut total_bytes = 0usize;
        for &(start, end) in spans {
            total_bytes += end.saturating_sub(start) as usize;
        }

        // Fast path for merging duplicate string contents without allocating
        // 1.5M `String`s: pack `(hash40 << 24) | idx` into a single `u64` per
        // referenced string, sort in parallel, and only resolve strings that
        // collide on their 40-bit hash or survive into the top `limit`.
        let mut referenced_string_count = 0usize;
        let mut referenced_bytes = 0usize;
        let mut empty_refs = 0usize;
        let hasher_builder = ahash::RandomState::with_seeds(1, 2, 3, 4);
        let mut packed: Vec<u64> = Vec::with_capacity(num_strings / 2);

        for (index, &count_u32) in references.iter().enumerate() {
            if count_u32 == 0 {
                continue;
            }
            let count = count_u32 as usize;
            referenced_string_count += 1;
            let (s, e) = spans[index];
            let raw_slice = &mmap_bytes[sbase + s as usize..sbase + e as usize];
            let byte_length = decoded_json_byte_len(raw_slice);
            referenced_bytes += byte_length.saturating_mul(count);
            if byte_length == 0 {
                empty_refs += count;
                continue;
            }
            let h = if memchr(b'\\', raw_slice).is_none() {
                hasher_builder.hash_one(raw_slice)
            } else {
                let dec = decode_json_string(raw_slice);
                hasher_builder.hash_one(dec.as_bytes())
            };
            packed.push((h & !0xFF_FFFFu64) | (index as u64 & 0xFF_FFFF));
        }

        packed.par_sort_unstable();

        // Keep top-`limit` candidates `(referenced_bytes, references, byte_length, rep_idx)`.
        let keep_all = limit == 0;
        let cap = if keep_all { packed.len() + 1 } else { limit + 1 };
        let mut top_candidates: Vec<(usize, usize, usize, usize)> = Vec::with_capacity(cap.min(1024));

        let mut push_candidate = |ref_bytes: usize, refs: usize, byte_len: usize, rep_idx: usize| {
            if keep_all || top_candidates.len() < limit {
                top_candidates.push((ref_bytes, refs, byte_len, rep_idx));
                return;
            }
            let mut worst = 0usize;
            for i in 1..top_candidates.len() {
                let a = top_candidates[i];
                let b = top_candidates[worst];
                if a.0 < b.0 || (a.0 == b.0 && a.1 < b.1) {
                    worst = i;
                }
            }
            let w = top_candidates[worst];
            if ref_bytes > w.0 || (ref_bytes == w.0 && refs >= w.1) {
                top_candidates[worst] = (ref_bytes, refs, byte_len, rep_idx);
            }
        };

        if empty_refs > 0 {
            if let Some(empty_idx) = (0..num_strings).find(|&i| spans[i].0 == spans[i].1) {
                push_candidate(0, empty_refs, 0, empty_idx);
            }
        }

        let mut i = 0usize;
        while i < packed.len() {
            let h_top = packed[i] & !0xFF_FFFFu64;
            let mut j = i + 1;
            while j < packed.len() && (packed[j] & !0xFF_FFFFu64) == h_top {
                j += 1;
            }
            if j == i + 1 {
                let idx = (packed[i] & 0xFF_FFFF) as usize;
                let refs = references[idx] as usize;
                let (s, e) = spans[idx];
                let raw_slice = &mmap_bytes[sbase + s as usize..sbase + e as usize];
                let byte_len = decoded_json_byte_len(raw_slice);
                push_candidate(refs.saturating_mul(byte_len), refs, byte_len, idx);
            } else {
                // Hash bucket with >1 string index: group by exact content.
                let mut groups: Vec<(usize, usize, usize, Vec<u8>)> = Vec::with_capacity(j - i);
                for &pk in &packed[i..j] {
                    let idx = (pk & 0xFF_FFFF) as usize;
                    let refs = references[idx] as usize;
                    let (s, e) = spans[idx];
                    let raw_slice = &mmap_bytes[sbase + s as usize..sbase + e as usize];
                    let content: std::borrow::Cow<'_, [u8]> = if memchr(b'\\', raw_slice).is_none() {
                        std::borrow::Cow::Borrowed(raw_slice)
                    } else {
                        std::borrow::Cow::Owned(decode_json_string(raw_slice).into_bytes())
                    };
                    if let Some(g) = groups.iter_mut().find(|g| g.3.as_slice() == content.as_ref()) {
                        g.1 += refs;
                    } else {
                        let blen = content.len();
                        groups.push((idx, refs, blen, content.into_owned()));
                    }
                }
                for (rep_idx, refs, byte_len, _) in groups {
                    push_candidate(refs.saturating_mul(byte_len), refs, byte_len, rep_idx);
                }
            }
            i = j;
        }
        drop(spans_guard);

        let mut entries: Vec<StringStatEntry> = top_candidates
            .into_iter()
            .map(|(ref_bytes, refs, byte_length, rep_idx)| {
                let value = raw
                    .strings
                    .resolve(&raw.mmap, rep_idx)
                    .unwrap_or_else(|| format!("<string#{rep_idx}>"));
                StringStatEntry {
                    value,
                    references: refs,
                    byte_length,
                    referenced_bytes: ref_bytes,
                }
            })
            .collect();

        entries.sort_unstable_by(|a, b| {
            b.referenced_bytes
                .cmp(&a.referenced_bytes)
                .then_with(|| b.references.cmp(&a.references))
                .then_with(|| a.value.cmp(&b.value))
        });
        if limit > 0 && entries.len() > limit {
            entries.truncate(limit);
        }

        Ok(StringStats {
            total_strings: num_strings,
            total_bytes,
            referenced_strings: referenced_string_count,
            referenced_bytes,
            entries,
        })
    }

    /// Automatically identify potential memory leaks and memory bloat suspects.
    pub fn leak_suspects(&mut self, limit: usize) -> crate::Result<LeakSuspectsReport> {
        self.ensure_raw()?;
        self.ensure_raw_summary()?;
        let total_nodes = self.raw.as_ref().unwrap().meta.node_count;
        let total_heap = self.raw_summary.as_ref().unwrap().total_size.max(1);

        let mut suspects: Vec<SuspectEntry> = Vec::new();

        // 1. Dominator suspects: large individual nodes (retained size >= 5% of heap or top dominators)
        let retained_res = self.get_retained_entries(10)?;
        for entry in retained_res.retained.iter().take(5) {
            let pct = (entry.retained_size as f64 / total_heap as f64) * 100.0;
            if pct >= 5.0 || suspects.is_empty() {
                let node_id = self.raw.as_ref().unwrap().nodes.ids[entry.node_index] as usize;
                suspects.push(SuspectEntry {
                    category: "Large Dominator".to_string(),
                    description: format!(
                        "Object '{}' ({}) retains {} ({:.1}% of heap).",
                        entry.name, entry.type_, format_bytes(entry.retained_size), pct
                    ),
                    node_index: Some(entry.node_index),
                    node_id: Some(node_id),
                    node_name: entry.name.clone(),
                    size_bytes: entry.retained_size,
                    pct_of_heap: pct,
                    recommendation: format!(
                        "Inspect retention tree: `hprof inspect --id {}` or check its fields: `hprof props --id {}`",
                        node_id, node_id
                    ),
                });
            }
        }

        // 2. Accumulator collections: collections with huge element count or self size
        let raw = self.raw.as_ref().unwrap();
        let mut largest_collections: Vec<(usize, usize, usize)> = Vec::new();
        for i in 0..total_nodes {
            let edge_count = raw.nodes.edge_counts[i] as usize;
            let self_size = raw.nodes.self_sizes[i] as usize;
            if edge_count > 5_000 || self_size > 500_000 {
                largest_collections.push((i, edge_count, self_size));
            }
        }
        largest_collections.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)));
        for &(idx, edge_count, self_size) in largest_collections.iter().take(3) {
            let node = Self::create_node(raw, idx);
            let pct = (self_size as f64 / total_heap as f64) * 100.0;
            suspects.push(SuspectEntry {
                category: "Collection Accumulator".to_string(),
                description: format!(
                    "Collection '{}' ({}) has {} elements/edges and self-size {}.",
                    node.name, node.type_, edge_count, format_bytes(self_size)
                ),
                node_index: Some(idx),
                node_id: Some(node.id),
                node_name: node.name,
                size_bytes: self_size,
                pct_of_heap: pct,
                recommendation: format!(
                    "Check if elements are ever cleared: `hprof retainers --id {} --depth 8`",
                    node.id
                ),
            });
        }

        // 3. Detached DOM nodes
        let detached = self.detached_summary(5, 4)?;
        if detached.total_count > 0 {
            let pct = (detached.total_size as f64 / total_heap as f64) * 100.0;
            let top_names: Vec<String> = detached.entries.iter().take(3).map(|e| e.node.name.clone()).collect();
            suspects.push(SuspectEntry {
                category: "Detached DOM Nodes".to_string(),
                description: format!(
                    "Found {} detached DOM nodes totaling {} ({:.1}% of heap): {}",
                    detached.total_count,
                    format_bytes(detached.total_size),
                    pct,
                    top_names.join(", ")
                ),
                node_index: detached.entries.first().map(|e| e.node.index),
                node_id: detached.entries.first().map(|e| e.node.id),
                node_name: "Detached DOM Tree".to_string(),
                size_bytes: detached.total_size,
                pct_of_heap: pct,
                recommendation: "Run `hprof detached --depth 8` to see which event listeners or owners hold them alive.".to_string(),
            });
        }

        // 4. Repeated strings waste
        let string_stats = self.string_stats(10)?;
        let mut wasted_bytes = 0usize;
        for entry in &string_stats.entries {
            if entry.references > 1 {
                wasted_bytes += (entry.references - 1) * entry.byte_length;
            }
        }
        if wasted_bytes > 500_000 {
            let pct = (wasted_bytes as f64 / total_heap as f64) * 100.0;
            suspects.push(SuspectEntry {
                category: "Duplicate Strings".to_string(),
                description: format!(
                    "Repeated strings waste approximately {} ({:.1}% of heap) across identical string values.",
                    format_bytes(wasted_bytes),
                    pct
                ),
                node_index: None,
                node_id: None,
                node_name: "Duplicated Strings".to_string(),
                size_bytes: wasted_bytes,
                pct_of_heap: pct,
                recommendation: "Run `hprof strings --top 30` to inspect duplicated content and consider string deduplication/interning.".to_string(),
            });
        }

        suspects.sort_unstable_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
        if limit > 0 && suspects.len() > limit {
            suspects.truncate(limit);
        }

        Ok(LeakSuspectsReport {
            total_heap_size: total_heap,
            total_nodes,
            suspects,
        })
    }

    /// Analyze ArrayBuffers, TypedArrays, and WebAssembly Memory.
    pub fn buffer_analysis(&mut self, limit: usize) -> crate::Result<BufferAnalysis> {
        self.ensure_raw()?;
        self.ensure_parent_map()?;
        let raw = self.raw.as_ref().unwrap();
        let total_nodes = raw.meta.node_count;

        let buffer_needles = [
            "arraybuffer",
            "typedarray",
            "uint8",
            "uint16",
            "uint32",
            "int8",
            "int16",
            "int32",
            "float32",
            "float64",
            "dataview",
            "jsarraybufferdata",
            "webassembly.memory",
        ];

        let (sbase, spans_guard) = raw.strings.all_spans(&raw.mmap);
        let spans: &[(u32, u32)] = &spans_guard;
        let mmap_bytes = &raw.mmap[..];
        let num_strings = spans.len();

        let matching_strings: Vec<bool> = (0..num_strings)
            .into_par_iter()
            .map(|idx| {
                let (s, e) = spans[idx];
                let raw_slice = &mmap_bytes[sbase + s as usize..sbase + e as usize];
                if raw_slice.len() < 4 {
                    return false;
                }
                if memchr(b'\\', raw_slice).is_none() {
                    if let Ok(st) = std::str::from_utf8(raw_slice) {
                        return buffer_needles.iter().any(|needle| contains_ci(st, needle));
                    }
                }
                let decoded = decode_json_string(raw_slice);
                buffer_needles.iter().any(|needle| contains_ci(&decoded, needle))
            })
            .collect();
        drop(spans_guard);

        let mut buffer_indices: Vec<usize> = Vec::new();
        let mut total_self_size = 0usize;

        for i in 0..total_nodes {
            let name_idx = raw.nodes.names[i] as usize;
            if name_idx < num_strings && matching_strings[name_idx] {
                buffer_indices.push(i);
                total_self_size += raw.nodes.self_sizes[i] as usize;
            }
        }

        let col = &raw.nodes;
        buffer_indices.sort_unstable_by(|&a, &b| col.self_sizes[b].cmp(&col.self_sizes[a]));

        let total_count = buffer_indices.len();
        if limit > 0 && buffer_indices.len() > limit {
            buffer_indices.truncate(limit);
        }

        let parents = self.parents.as_ref().unwrap();
        let mut entries = Vec::with_capacity(buffer_indices.len());
        let mut total_retained_size = 0usize;

        for &idx in &buffer_indices {
            let node = Self::create_node(raw, idx);
            let owner_idx = parents.index[idx];
            let owner_name = if owner_idx != u32::MAX && (owner_idx as usize) < total_nodes {
                let owner_node = Self::create_node(raw, owner_idx as usize);
                format!("{} ({}, id={})", owner_node.name, owner_node.type_, owner_node.id)
            } else {
                "(root / unknown)".to_string()
            };

            total_retained_size += node.self_size;
            entries.push(BufferEntry {
                node_index: idx,
                node_id: node.id,
                name: node.name,
                buffer_type: node.type_,
                self_size: node.self_size,
                retained_size: node.self_size,
                owner_name,
            });
        }

        Ok(BufferAnalysis {
            total_count,
            total_self_size,
            total_retained_size,
            entries,
        })
    }

    /// Analyze closures, JS execution contexts, and captured variables.
    pub fn closure_analysis(&mut self, limit: usize) -> crate::Result<ClosureAnalysis> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        let total_nodes = raw.meta.node_count;

        let context_edge_type = raw
            .edge_types
            .iter()
            .position(|t| t == "context");
        let closure_node_type = raw
            .node_types
            .iter()
            .position(|t| t == "closure")
            .map(|idx| idx as u8);

        let mut closure_indices: Vec<usize> = Vec::new();
        for i in 0..total_nodes {
            if Some(raw.nodes.types[i]) == closure_node_type {
                closure_indices.push(i);
            }
        }

        if closure_indices.is_empty() && context_edge_type.is_some() {
            self.ensure_edges()?;
            self.ensure_edge_starts();
            let raw = self.raw.as_ref().unwrap();
            let edge_starts = self.edge_starts.as_ref().unwrap();
            let edge_types = raw.edges.as_ref().unwrap().types.as_slice();
            let ctype = context_edge_type.unwrap() as u8;
            for i in 0..total_nodes {
                let start = edge_starts[i] as usize;
                let end = edge_starts[i + 1] as usize;
                if edge_types[start..end].contains(&ctype) {
                    closure_indices.push(i);
                }
            }
        }

        let _ = self.locate_edges_range()?;
        self.ensure_edge_starts();
        let raw = self.raw.as_ref().unwrap();
        let edge_starts = self.edge_starts.as_ref().unwrap();

        let total_closures = closure_indices.len();
        closure_indices.sort_unstable_by(|&a, &b| {
            raw.nodes.self_sizes[b]
                .cmp(&raw.nodes.self_sizes[a])
                .then_with(|| raw.nodes.edge_counts[b].cmp(&raw.nodes.edge_counts[a]))
        });

        if limit > 0 && closure_indices.len() > limit {
            closure_indices.truncate(limit);
        }

        let mut entries = Vec::with_capacity(closure_indices.len());
        let mut total_retained = 0usize;

        for &idx in &closure_indices {
            let node = Self::create_node(raw, idx);
            total_retained += node.self_size;

            let mut context_id = None;
            let mut context_name = String::from("(none)");
            let mut captured_vars = Vec::new();

            let start = edge_starts[idx] as usize;
            let end = edge_starts[idx + 1] as usize;
            let mut found_ctx = None;
            raw.for_each_edge_in_range(start, end, |_, etype, _, ctx_node_idx| {
                if found_ctx.is_none() && Some(etype) == context_edge_type {
                    found_ctx = Some(ctx_node_idx);
                }
            });
            if let Some(ctx_node_idx) = found_ctx {
                if ctx_node_idx < total_nodes {
                    let ctx_node = Self::create_node(raw, ctx_node_idx);
                    context_id = Some(ctx_node.id);
                    context_name = format!("{} (id={})", ctx_node.name, ctx_node.id);

                    let c_start = edge_starts[ctx_node_idx] as usize;
                    let c_end = edge_starts[ctx_node_idx + 1] as usize;
                    raw.for_each_edge_in_range(c_start, c_end, |_, _, var_name_idx, _| {
                        if let Some(var_name) = raw.strings.resolve(&raw.mmap, var_name_idx as usize) {
                            if !var_name.is_empty() && !var_name.starts_with("__") {
                                captured_vars.push(var_name);
                            }
                        }
                    });
                }
            }

            entries.push(ClosureEntry {
                node_index: idx,
                node_id: node.id,
                name: node.name,
                context_id,
                context_name,
                self_size: node.self_size,
                retained_size: node.self_size,
                captured_vars,
            });
        }

        Ok(ClosureAnalysis {
            total_closures,
            total_retained,
            entries,
        })
    }
}

impl RawData {
    /// Iterate over edges `start..end`, invoking `f(edge_index, type_idx, name_or_index, to_node)`.
    /// Uses `EdgeColumns::sync_offsets` when `self.edges` is loaded, or seeks directly in `self.mmap`
    /// when `self.edges` is `None`.
    fn for_each_edge_in_range<F>(&self, start: usize, end: usize, mut f: F)
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

        // Direct mmap scan (when `self.edges` is `None` or fallback path)
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

/// Compute the UTF-8 byte length of `decode_json_string(raw)` without allocating.
#[inline]
fn decoded_json_byte_len(raw: &[u8]) -> usize {
    if memchr(b'\\', raw).is_none() {
        return raw.len();
    }
    decode_json_string(raw).len()
}

#[derive(Clone, Copy)]
struct ObjectPresenceCandidate {
    id: u32,
    index: usize,
    size: usize,
}

#[derive(Clone, Copy)]
struct ObjectGrowthCandidate {
    id: u32,
    baseline_index: usize,
    profile_index: usize,
    baseline_size: usize,
    profile_size: usize,
    delta: i64,
}

fn sorted_node_indices(ids: &[u32]) -> Option<Vec<u32>> {
    if ids.windows(2).all(|w| w[0] <= w[1]) {
        return None;
    }
    let mut order: Vec<u32> = (0..ids.len() as u32).collect();
    order.par_sort_unstable_by_key(|&index| ids[index as usize]);
    Some(order)
}

#[inline]
fn ordered_id_index(ids: &[u32], order: Option<&Vec<u32>>, position: usize) -> (u32, usize) {
    let index = order
        .and_then(|items| items.get(position).copied())
        .map(|index| index as usize)
        .unwrap_or(position);
    (ids[index], index)
}

fn push_presence_candidate(
    candidates: &mut Vec<ObjectPresenceCandidate>,
    candidate: ObjectPresenceCandidate,
    limit: usize,
) {
    if limit == 0 {
        return;
    }
    candidates.push(candidate);
    if candidates.len() > limit {
        let worst = candidates
            .iter()
            .enumerate()
            .min_by_key(|(_, item)| (item.size, item.id))
            .map(|(index, _)| index)
            .unwrap();
        candidates.swap_remove(worst);
    }
}

fn push_growth_candidate(
    candidates: &mut Vec<ObjectGrowthCandidate>,
    candidate: ObjectGrowthCandidate,
    limit: usize,
) {
    if limit == 0 {
        return;
    }
    candidates.push(candidate);
    if candidates.len() > limit {
        let worst = candidates
            .iter()
            .enumerate()
            .min_by_key(|(_, item)| (item.delta, item.id))
            .map(|(index, _)| index)
            .unwrap();
        candidates.swap_remove(worst);
    }
}

fn snapshot_object(raw: &RawData, index: usize) -> SnapshotObject {
    let node = HeapSnapshot::create_node(raw, index);
    SnapshotObject {
        index,
        id: node.id,
        name: node.name,
        type_: node.type_,
        self_size: node.self_size,
        edge_count: node.edge_count,
    }
}

fn diff_snapshot_objects(
    baseline: &RawData,
    profile: &RawData,
    limit: usize,
) -> SnapshotObjectDiff {
    let baseline_ids = &baseline.nodes.ids;
    let profile_ids = &profile.nodes.ids;
    let baseline_order = sorted_node_indices(baseline_ids);
    let profile_order = sorted_node_indices(profile_ids);

    let mut base_pos = 0usize;
    let mut profile_pos = 0usize;
    let mut matched_count = 0usize;
    let mut new_count = 0usize;
    let mut deleted_count = 0usize;
    let mut new_size = 0usize;
    let mut deleted_size = 0usize;
    let mut delta_size = 0i64;
    let mut new_objects = Vec::with_capacity(limit.min(64));
    let mut deleted_objects = Vec::with_capacity(limit.min(64));
    let mut grown_objects = Vec::with_capacity(limit.min(64));

    while base_pos < baseline_ids.len() || profile_pos < profile_ids.len() {
        let base = (base_pos < baseline_ids.len())
            .then(|| ordered_id_index(baseline_ids, baseline_order.as_ref(), base_pos));
        let current = (profile_pos < profile_ids.len())
            .then(|| ordered_id_index(profile_ids, profile_order.as_ref(), profile_pos));

        match (base, current) {
            (Some((base_id, base_index)), Some((profile_id, profile_index)))
                if base_id == profile_id =>
            {
                matched_count += 1;
                let baseline_node_size = baseline.nodes.self_sizes[base_index] as usize;
                let profile_node_size = profile.nodes.self_sizes[profile_index] as usize;
                let delta = profile_node_size as i64 - baseline_node_size as i64;
                delta_size += delta;
                if delta > 0 {
                    push_growth_candidate(
                        &mut grown_objects,
                        ObjectGrowthCandidate {
                            id: profile_id,
                            baseline_index: base_index,
                            profile_index,
                            baseline_size: baseline_node_size,
                            profile_size: profile_node_size,
                            delta,
                        },
                        limit,
                    );
                }
                base_pos += 1;
                profile_pos += 1;
            }
            (Some((base_id, base_index)), Some((profile_id, _))) if base_id < profile_id => {
                let size = baseline.nodes.self_sizes[base_index] as usize;
                deleted_count += 1;
                deleted_size += size;
                delta_size -= size as i64;
                push_presence_candidate(
                    &mut deleted_objects,
                    ObjectPresenceCandidate {
                        id: base_id,
                        index: base_index,
                        size,
                    },
                    limit,
                );
                base_pos += 1;
            }
            (Some((_base_id, _)), Some((_profile_id, profile_index))) => {
                let size = profile.nodes.self_sizes[profile_index] as usize;
                new_count += 1;
                new_size += size;
                delta_size += size as i64;
                push_presence_candidate(
                    &mut new_objects,
                    ObjectPresenceCandidate {
                        id: profile_ids[profile_index],
                        index: profile_index,
                        size,
                    },
                    limit,
                );
                profile_pos += 1;
            }
            (Some((base_id, base_index)), None) => {
                let size = baseline.nodes.self_sizes[base_index] as usize;
                deleted_count += 1;
                deleted_size += size;
                delta_size -= size as i64;
                push_presence_candidate(
                    &mut deleted_objects,
                    ObjectPresenceCandidate {
                        id: base_id,
                        index: base_index,
                        size,
                    },
                    limit,
                );
                base_pos += 1;
            }
            (None, Some((_profile_id, profile_index))) => {
                let size = profile.nodes.self_sizes[profile_index] as usize;
                new_count += 1;
                new_size += size;
                delta_size += size as i64;
                push_presence_candidate(
                    &mut new_objects,
                    ObjectPresenceCandidate {
                        id: profile_ids[profile_index],
                        index: profile_index,
                        size,
                    },
                    limit,
                );
                profile_pos += 1;
            }
            (None, None) => break,
        }
    }

    new_objects.sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.id.cmp(&b.id)));
    deleted_objects.sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.id.cmp(&b.id)));
    grown_objects.sort_unstable_by(|a, b| b.delta.cmp(&a.delta).then_with(|| a.id.cmp(&b.id)));

    SnapshotObjectDiff {
        matched_count,
        new_count,
        deleted_count,
        new_size,
        deleted_size,
        delta_size,
        new_objects: new_objects
            .into_iter()
            .map(|candidate| snapshot_object(profile, candidate.index))
            .collect(),
        deleted_objects: deleted_objects
            .into_iter()
            .map(|candidate| snapshot_object(baseline, candidate.index))
            .collect(),
        grown_objects: grown_objects
            .into_iter()
            .map(|candidate| {
                let node = snapshot_object(profile, candidate.profile_index);
                SnapshotObjectChange {
                    id: candidate.id as usize,
                    baseline_index: candidate.baseline_index,
                    profile_index: candidate.profile_index,
                    name: node.name,
                    type_: node.type_,
                    baseline_size: candidate.baseline_size,
                    profile_size: candidate.profile_size,
                    delta: candidate.delta,
                }
            })
            .collect(),
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
        // partial selection: isolate the top-k (O(N)) and sort only those,
        // instead of a full O(N log N) sort of every distinct name
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
/// Uses SIMD `memchr3`/`memchr2` to jump directly between brackets and quotes
/// instead of inspecting every digit byte.
pub fn find_array_end(data: &[u8], open: usize) -> usize {
    let mut i = open + 1;
    let len = data.len();
    let mut depth = 1i32;
    while i < len {
        let Some(rel) = memchr::memchr3(b']', b'[', b'"', &data[i..]) else {
            return len.saturating_sub(1);
        };
        i += rel;
        match data[i] {
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
                i += 1;
            }
            b'[' => {
                depth += 1;
                i += 1;
            }
            b'"' => {
                i += 1;
                while i < len {
                    let Some(srel) = memchr2(b'"', b'\\', &data[i..]) else {
                        return len.saturating_sub(1);
                    };
                    i += srel;
                    if data[i] == b'\\' {
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                }
            }
            _ => i += 1,
        }
    }
    len.saturating_sub(1)
}

#[inline]
pub fn find_marker(data: &[u8], marker: &[u8]) -> Option<usize> {
    memchr::memmem::find(data, marker)
}

#[inline]
pub fn rfind_marker(data: &[u8], marker: &[u8]) -> Option<usize> {
    memchr::memmem::rfind(data, marker)
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

/// Divide `data` into `num_slices` slices and compute prefix sums of comma
/// counts in parallel using SIMD `memchr_iter(b',', ...)`.
/// Returns `(slice_starts, comma_prefix_sums)` where both have length `slice_cnt + 1`.
pub(crate) fn parallel_comma_prefixes(data: &[u8], num_slices: usize) -> (Vec<usize>, Vec<usize>) {
    let n = num_slices.max(1);
    let step = (data.len() / n).max(1);
    let mut starts = Vec::with_capacity(n + 1);
    let mut pos = 0usize;
    while pos < data.len() && starts.len() < n {
        starts.push(pos);
        pos = (pos + step).min(data.len());
    }
    starts.push(data.len());
    let slice_cnt = starts.len() - 1;
    let counts: Vec<usize> = if data.len() >= (1 << 19) && slice_cnt > 1 {
        (0..slice_cnt)
            .into_par_iter()
            .map(|i| memchr::memchr_iter(b',', &data[starts[i]..starts[i + 1]]).count())
            .collect()
    } else {
        (0..slice_cnt)
            .map(|i| memchr::memchr_iter(b',', &data[starts[i]..starts[i + 1]]).count())
            .collect()
    };
    let mut prefixes = Vec::with_capacity(slice_cnt + 1);
    prefixes.push(0);
    let mut acc = 0usize;
    for c in counts {
        acc += c;
        prefixes.push(acc);
    }
    (starts, prefixes)
}

/// Find the byte offset immediately after the `target_commas`-th comma in `data`
/// (or `0` when `target_commas == 0`), using parallel SIMD comma counting on large slices.
pub(crate) fn find_nth_comma_par(data: &[u8], target_commas: usize) -> Option<usize> {
    if target_commas == 0 {
        return Some(0);
    }
    const MIN_PAR: usize = 1 << 20;
    if data.len() < MIN_PAR {
        return memchr::memchr_iter(b',', data)
            .nth(target_commas - 1)
            .map(|p| p + 1);
    }
    let threads = rayon::current_num_threads().max(1);
    let (starts, prefixes) = parallel_comma_prefixes(data, threads * 4);
    let total = *prefixes.last().unwrap();
    if target_commas > total {
        return None;
    }
    // Find the slice `k` containing the `target_commas`-th comma (1-indexed).
    let k = prefixes.partition_point(|&p| p < target_commas).saturating_sub(1);
    let rem = target_commas - prefixes[k];
    let slice = &data[starts[k]..starts[k + 1]];
    memchr::memchr_iter(b',', slice)
        .nth(rem - 1)
        .map(|rel| starts[k] + rel + 1)
}

/// Partition `data` into record-aligned chunks `(byte_start, byte_end, rec_start, rec_end)`
/// in parallel using SIMD comma prefix sums. Returns `None` if `(total_commas + 1)`
/// is not a multiple of `field_count`.
pub(crate) fn aligned_record_chunks_par(
    data: &[u8],
    field_count: usize,
) -> Option<Vec<(usize, usize, usize, usize)>> {
    if field_count == 0 || data.is_empty() {
        return None;
    }
    let threads = rayon::current_num_threads().max(1);
    let num_chunks = (threads * 4).max(1);
    let (starts, prefixes) = parallel_comma_prefixes(data, num_chunks);
    let total_commas = *prefixes.last().unwrap();
    let total_nums = total_commas + 1;
    if !total_nums.is_multiple_of(field_count) {
        return None;
    }
    let total_recs = total_nums / field_count;
    let mut bounds: Vec<(usize, usize)> = Vec::with_capacity(starts.len());
    bounds.push((0, 0));
    for k in 1..starts.len() - 1 {
        let raw_pos = starts[k];
        if raw_pos <= bounds.last().unwrap().0 {
            continue;
        }
        let commas_before = prefixes[k];
        let need = field_count - (commas_before % field_count);
        if let Some(rel) = memchr::memchr_iter(b',', &data[raw_pos..]).nth(need - 1) {
            let pos = raw_pos + rel + 1;
            let rec_idx = (commas_before + need) / field_count;
            if pos < data.len() && rec_idx < total_recs && rec_idx > bounds.last().unwrap().1 {
                bounds.push((pos, rec_idx));
            }
        }
    }
    bounds.push((data.len(), total_recs));
    Some(
        bounds
            .windows(2)
            .filter_map(|w| {
                let (bs, rs) = w[0];
                let (be, re) = w[1];
                (be > bs && re > rs).then_some((bs, be, rs, re))
            })
            .collect(),
    )
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

/// Split a flat comma-separated integer array into byte ranges that each
/// START at a record boundary (every `field_count` numbers), so chunks can be
/// parsed independently into complete records and streamed into columns
/// without materializing the whole interleaved number vector.
#[cfg(test)]
pub(crate) fn record_aligned_chunks(
    data: &[u8],
    field_count: usize,
    chunks: usize,
) -> Vec<(usize, usize)> {
    let mut bounds: Vec<usize> = Vec::with_capacity(chunks + 2);
    bounds.push(0);
    if field_count > 0 && !data.is_empty() {
        let target = (data.len() / chunks.max(1)).max(1);
        let data_len = data.len();
        let mut pos = 0usize;
        let mut commas_total = 0usize;
        let mut next_at = target;
        while pos < data_len && bounds.len() <= chunks {
            let span_end = next_at.min(data_len);
            if span_end > pos {
                commas_total += memchr::memchr_iter(b',', &data[pos..span_end]).count();
                pos = span_end;
            }
            match memchr(b',', &data[pos..]) {
                Some(c) => {
                    pos += c + 1;
                    commas_total += 1;
                }
                None => break,
            }
            while !commas_total.is_multiple_of(field_count) {
                match memchr(b',', &data[pos..]) {
                    Some(c) => {
                        pos += c + 1;
                        commas_total += 1;
                    }
                    None => {
                        pos = data_len;
                        break;
                    }
                }
            }
            if pos < data_len {
                bounds.push(pos);
            } else {
                break;
            }
            next_at = next_at.saturating_add(target).max(pos);
        }
    }
    bounds.push(data.len());
    bounds
        .windows(2)
        .map(|w| (w[0], w[1]))
        .filter(|&(s, e)| e > s)
        .collect()
}

/// Stream the flat node array directly into pre-allocated `NodeColumns` slices
/// in parallel without any intermediate per-chunk vectors or full interleaved
/// `Vec<u32>`. Returns `None` when record-alignment validation fails.
fn node_columns_streaming(
    data: &[u8],
    nfc: usize,
    offsets: &NodeFieldOffsets,
) -> Option<NodeColumns> {
    let chunks = aligned_record_chunks_par(data, nfc)?;
    let total_recs = chunks.last().map(|c| c.3).unwrap_or(0);
    if total_recs == 0 {
        return None;
    }

    let mut types = vec![0u8; total_recs];
    let mut names = vec![0u32; total_recs];
    let mut ids = vec![0u32; total_recs];
    let mut self_sizes = vec![0u32; total_recs];
    let mut edge_counts = vec![0u32; total_recs];
    let mut detachedness = vec![0u8; total_recs];

    let mut tasks: Vec<(
        usize,
        usize,
        &mut [u8],
        &mut [u32],
        &mut [u32],
        &mut [u32],
        &mut [u32],
        &mut [u8],
    )> = Vec::with_capacity(chunks.len());

    let mut rem_types = types.as_mut_slice();
    let mut rem_names = names.as_mut_slice();
    let mut rem_ids = ids.as_mut_slice();
    let mut rem_sizes = self_sizes.as_mut_slice();
    let mut rem_edges = edge_counts.as_mut_slice();
    let mut rem_det = detachedness.as_mut_slice();

    for &(bs, be, rs, re) in &chunks {
        let len = re - rs;
        let (t_head, t_tail) = rem_types.split_at_mut(len);
        let (n_head, n_tail) = rem_names.split_at_mut(len);
        let (i_head, i_tail) = rem_ids.split_at_mut(len);
        let (s_head, s_tail) = rem_sizes.split_at_mut(len);
        let (e_head, e_tail) = rem_edges.split_at_mut(len);
        let (d_head, d_tail) = rem_det.split_at_mut(len);
        rem_types = t_tail;
        rem_names = n_tail;
        rem_ids = i_tail;
        rem_sizes = s_tail;
        rem_edges = e_tail;
        rem_det = d_tail;
        tasks.push((bs, be, t_head, n_head, i_head, s_head, e_head, d_head));
    }

    let type_off = offsets.type_;
    let name_off = offsets.name;
    let id_off = offsets.id;
    let size_off = offsets.self_size;
    let edge_off = offsets.edge_count;
    let det_off = offsets.detachedness;

    let ok = tasks.into_par_iter().all(
        |(bs, be, out_t, out_n, out_id, out_sz, out_ec, out_det)| {
            let slice = &data[bs..be];
            let expected_recs = out_t.len();
            let mut i = 0usize;
            let len = slice.len();
            let mut rec = [0u32; 16];
            if nfc > rec.len() {
                return false;
            }
            let mut field = 0usize;
            let mut out_i = 0usize;
            while i < len {
                let ch = slice[i];
                if ch == b']' {
                    break;
                }
                if ch.is_ascii_digit() {
                    if out_i == expected_recs {
                        return false;
                    }
                    let mut val = (ch - b'0') as u32;
                    i += 1;
                    while i < len && slice[i].is_ascii_digit() {
                        val = val * 10 + (slice[i] - b'0') as u32;
                        i += 1;
                    }
                    rec[field] = val;
                    field += 1;
                    if field == nfc {
                        field = 0;
                        out_t[out_i] = rec[type_off] as u8;
                        out_n[out_i] = rec[name_off];
                        out_id[out_i] = rec[id_off];
                        out_sz[out_i] = rec[size_off];
                        out_ec[out_i] = rec[edge_off];
                        out_det[out_i] = det_off.map(|o| rec[o] as u8).unwrap_or(0);
                        out_i += 1;
                    }
                    continue;
                }
                i += 1;
            }
            out_i == expected_recs && field == 0
        },
    );

    if !ok {
        return None;
    }

    Some(NodeColumns {
        types,
        names,
        ids,
        self_sizes,
        edge_counts,
        detachedness,
    })
}

/// Stream the flat edge array directly into pre-allocated `EdgeColumns`
/// (`types: Vec<u8>`, `to_nodes: Vec<u32>`, and `sync_offsets` every 1,024
/// edges) without materializing `name_or_index` or a flat `Vec<u32>`.
fn edge_columns_streaming(
    data: &[u8],
    bytes_start: usize,
    bytes_end: usize,
    efc: usize,
    nfc: u32,
    offsets: &EdgeFieldOffsets,
) -> Option<EdgeColumns> {
    let chunks = aligned_record_chunks_par(data, efc)?;
    let total_recs = chunks.last().map(|c| c.3).unwrap_or(0);
    if total_recs == 0 {
        return None;
    }

    let mut types = vec![0u8; total_recs];
    let mut to_nodes = vec![0u32; total_recs];
    let sync_len = total_recs.div_ceil(EDGE_SYNC_STRIDE);
    let sync_offsets = vec![0usize; sync_len];
    let sync_ptr = sync_offsets.as_ptr() as usize;

    let mut tasks: Vec<(usize, usize, usize, &mut [u8], &mut [u32])> =
        Vec::with_capacity(chunks.len());
    let mut rem_types = types.as_mut_slice();
    let mut rem_to = to_nodes.as_mut_slice();

    for &(bs, be, rs, re) in &chunks {
        let len = re - rs;
        let (t_head, t_tail) = rem_types.split_at_mut(len);
        let (to_head, to_tail) = rem_to.split_at_mut(len);
        rem_types = t_tail;
        rem_to = to_tail;
        tasks.push((bs, be, rs, t_head, to_head));
    }

    let type_off = offsets.type_;
    let to_off = offsets.to_node;
    let nfc_div = nfc.max(1);

    let ok = tasks
        .into_par_iter()
        .all(|(bs, be, rs, out_t, out_to)| {
            let slice = &data[bs..be];
            let expected_recs = out_t.len();
            let mut i = 0usize;
            let len = slice.len();
            let mut rec = [0u32; 8];
            if efc > rec.len() {
                return false;
            }
            let mut field = 0usize;
            let mut out_i = 0usize;
            while i < len {
                let ch = slice[i];
                if ch == b']' {
                    break;
                }
                if ch.is_ascii_digit() {
                    if out_i == expected_recs {
                        return false;
                    }
                    if field == 0 {
                        let global_edge = rs + out_i;
                        if global_edge.is_multiple_of(EDGE_SYNC_STRIDE) {
                            let sync_idx = global_edge / EDGE_SYNC_STRIDE;
                            // SAFETY: each `global_edge` is visited by exactly one task,
                            // so `sync_idx` is disjoint across all parallel tasks.
                            unsafe {
                                *(sync_ptr as *mut usize).add(sync_idx) = bs + i;
                            }
                        }
                    }
                    let mut val = (ch - b'0') as u32;
                    i += 1;
                    while i < len && slice[i].is_ascii_digit() {
                        val = val * 10 + (slice[i] - b'0') as u32;
                        i += 1;
                    }
                    rec[field] = val;
                    field += 1;
                    if field == efc {
                        field = 0;
                        out_t[out_i] = rec[type_off] as u8;
                        out_to[out_i] = rec[to_off] / nfc_div;
                        out_i += 1;
                    }
                    continue;
                }
                i += 1;
            }
            out_i == expected_recs && field == 0
        });

    if !ok {
        return None;
    }

    Some(EdgeColumns {
        types,
        to_nodes,
        sync_offsets,
        bytes_start,
        bytes_end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_summary(size: usize, count: usize) -> TypeSummary {
        TypeSummary { size, count }
    }

    // -------------------------------------------------------------------
    // Low-level parsers
    // -------------------------------------------------------------------

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
        // > 1MiB of digits so the parallel path (splits at commas) triggers
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
        // property test over several layouts: ranges are ordered, disjoint,
        // cover the whole array, and every range parses into complete records
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
                let ranges = record_aligned_chunks(data, nfc, chunks);
                assert!(!ranges.is_empty(), "nfc={nfc} chunks={chunks}");
                assert_eq!(ranges[0].0, 0, "must start at 0");
                // cover everything, ordered and disjoint
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
        // the helper itself just returns ranges; the *validation* happens in
        // the streaming extractors. Simulate a non-canonical layout (space
        // instead of a comma) and check the chunks no longer divide into
        // complete records, i.e. the extractor would return None and the
        // caller falls back to the whole-array path.
        let s = "1,2,3 4,5,6,7,8,9,10,11,12";
        let data = s.as_bytes();
        let ranges = record_aligned_chunks(data, 3, 2);
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
        // reference: the fallback interleaved parse + column sweeps
        let offsets = NodeFieldOffsets {
            type_: 0,
            name: 1,
            id: 2,
            self_size: 3,
            edge_count: 4,
            detachedness: Some(5),
        };
        let nfc = 6usize; // incl. an unread field (detachedness at index 5)
        let mut s = String::new();
        for r in 0..50_000u32 {
            for f in 0..nfc {
                if !(r == 0 && f == 0) {
                    s.push(',');
                }
                s.push_str(&match f {
                    0 => (r % 15).to_string(),          // type
                    1 => (r % 70000).to_string(),       // name idx
                    2 => (r * 13).to_string(),          // id
                    3 => ((r * 37) % 4096).to_string(), // self size
                    4 => (r % 30).to_string(),          // edge count
                    _ => (r % 3).to_string(),           // detachedness
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
        // U+1F600 "😀" is encoded as the \uD83D\uDE00 pair
        assert_eq!(
            decode_json_string("x\\uD83D\\uDE00y".as_bytes()),
            "x\u{1F600}y"
        );
    }

    #[test]
    fn contains_ci_matches_mixed_case_needles() {
        // regression: the sliding window once lowercased only the haystack
        // bytes, so a mixed-case needle like "vECTOR3" matched almost nothing
        assert!(contains_ci("new BABYLON.Vector3(0)", "vECTOR3"));
        assert!(contains_ci("vector3", "VECTOR3"));
        assert!(contains_ci("VECTOR3", "vector3"));
        assert!(contains_ci("Vector3", "VeCtOr3"));
        assert!(contains_ci("abc", ""));
        assert!(!contains_ci("Vector", "vector3"));
        assert!(!contains_ci("vec", "vector3"));
        // non-ASCII falls back to Unicode-aware comparison
        assert!(contains_ci("Функция (ФУНКЦИЯ)", "функция"));
        assert!(!contains_ci("Функция", "вектор"));
    }

    #[test]
    fn eq_ci_ascii_and_unicode() {
        assert!(eq_ci("RenderingGroup", "renderinggroup"));
        assert!(eq_ci("object", "OBJECT"));
        assert!(!eq_ci("object", "objects"));
        assert!(eq_ci("Функция", "функция"));
        assert!(!eq_ci("Функция", "вектор"));
        // empty strings are equal
        assert!(eq_ci("", ""));
    }

    #[test]
    fn eq_ci_name_matches_display_suffix() {
        assert!(eq_ci_name("system / ExternalStringData", "ExternalStringData"));
        assert!(eq_ci_name("system / ExternalStringData", "externalstringdata"));
        assert!(eq_ci_name("ExternalStringData", "ExternalStringData"));
        assert!(eq_ci_name("ExternalStringData", "externalstringdata"));
        assert!(!eq_ci_name("system / InternalStringData", "ExternalStringData"));
        assert!(!eq_ci_name("InternalStringData", "ExternalStringData"));
        assert!(!eq_ci_name("system / ExternalStringDataExtra", "ExternalStringData"));
    }

    #[test]
    fn string_table_skips_memo_for_large_strings() {
        let short = b"hello";
        let long_len = MEMO_MAX_STRING_BYTES + 1;
        let mut data = Vec::with_capacity(short.len() + long_len);
        data.extend_from_slice(short);
        data.resize(short.len() + long_len, b'a');
        let path = std::env::temp_dir().join(format!(
            "hprof_string_memo_{}.bin",
            std::process::id()
        ));
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
        // nested arrays
        assert_eq!(find_array_end(b"[[1,2],[3,4]]\"x\"", 0), 12);
        // brackets inside strings (incl. escaped quotes) must be ignored
        let d = b"[\"a]\", \"b\\\"[\"]";
        assert_eq!(find_array_end(d, 0), 13);
    }

    #[test]
    fn find_matching_brace_nested_and_strings() {
        let data = br#"{"a":{"b":1},"c":"}"}"#;
        let start = 0usize;
        // the '}' inside the "c" string must not close the object; the final
        // brace is at index 20
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

    // -------------------------------------------------------------------
    // Summary slicing
    // -------------------------------------------------------------------

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
        // matches n90..n99 (10 entries); the single-digit "n9" must not match
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

    // -------------------------------------------------------------------
    // Dominator analysis on unreachable nodes (regression)
    // -------------------------------------------------------------------

    /// Synthetic V8 `.heapsnapshot`. Node 3 has no incoming edges and is
    /// unreachable from the root: the dominator analysis used to index
    /// `dfs_parent` (sized to the reachable set) by a node_count-sized
    /// preorder number and panicked. Retained sizes:
    ///   node 1 retains only itself (root also reaches node 2 directly),
    ///   node 2 the same; node 3 is never dominated.
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
        assert!(!res.approximate); // 4 nodes → exact dominators, not the fallback
        let by_index = |i: usize| res.retained.iter().find(|e| e.node_index == i);
        assert!(
            by_index(3).is_none(),
            "unreachable node must have retained == 0"
        );
        assert_eq!(by_index(1).unwrap().retained_size, 10);
        assert_eq!(by_index(2).unwrap().retained_size, 10);

        // the downstream consumers must also survive the unreachable node
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
        // Linear chain: Root (0) -> Node 1 (10 bytes) -> Node 2 (20 bytes) -> Node 3 (30 bytes)
        // Root has 1 edge to 1 (offset 6). Node 1 has 1 edge to 2 (offset 12). Node 2 has 1 edge to 3 (offset 18).
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

        let by_index = |i: usize| res.retained.iter().find(|e| e.node_index == i).map(|e| e.retained_size);
        // Node 1 dominates 2 and 3: retained = 10 + 20 + 30 = 60
        assert_eq!(by_index(1), Some(60));
        // Node 2 dominates 3: retained = 20 + 30 = 50
        assert_eq!(by_index(2), Some(50));
        // Node 3 dominates only itself: retained = 30
        assert_eq!(by_index(3), Some(30));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn weak_edges_do_not_retain() {
        // Root (0) -> Node 1 (strong property). Node 1 -> Node 2 (weak edge, type index 2).
        let json = r#"{"snapshot":{"meta":{
            "node_fields":["type","name","id","self_size","edge_count","detachedness"],
            "node_types":[["hidden","object","synthetic"],"string","number","number","number","number"],
            "edge_fields":["type","name_or_index","to_node"],
            "edge_types":[["property","element","weak"],"string_or_number","node"]
        },"node_count":3,"edge_count":2},
        "nodes":[2,0,0,0,1,0, 1,1,1,10,1,0, 1,2,2,20,0,0],
        "edges":[0,0,6, 2,0,12],
        "strings":["","n1","n2"]}"#;

        let path = std::env::temp_dir().join(format!(
            "hprof_weak_{}.heapsnapshot",
            std::process::id()
        ));
        std::fs::write(&path, json).unwrap();

        let mut snap = HeapSnapshot::new(path.to_string_lossy().into_owned());
        let res = snap.get_retained_entries(10).unwrap();

        let by_index = |i: usize| res.retained.iter().find(|e| e.node_index == i);
        // Node 2 is only referenced via a weak edge, so it is unreachable from root!
        assert!(by_index(2).is_none());
        // Node 1 retains only itself (10 bytes), not node 2
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

        let path = std::env::temp_dir().join(format!(
            "hprof_reports_{}.heapsnapshot",
            std::process::id()
        ));
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
