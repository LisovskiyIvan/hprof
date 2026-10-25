use ahash::AHashMap;
use memchr::{memchr, memchr2};
use memmap2::Mmap;

use super::parser::{find_marker, rfind_marker};

pub(crate) const MEMO_MAX_STRING_BYTES: usize = 64 * 1024;
pub(crate) const MEMO_MAX_ENTRIES: usize = 32 * 1024;

pub(crate) struct StringScanState {
    /// offset into the mmap where the string content starts (past `[`)
    pub(crate) start: usize,
    /// offset from which to search backwards for `"strings":[` if `start == 0`
    pub(crate) search_from: usize,
    /// per-string (start, end) relative to `start`, content still escaped
    pub(crate) spans: Vec<(u32, u32)>,
    /// byte offset in mmap where scanning was left off
    pub(crate) cursor: usize,
    /// whether the closing `]` of strings array has been reached
    pub(crate) done: bool,
}

impl std::ops::Deref for StringScanState {
    type Target = [(u32, u32)];
    fn deref(&self) -> &Self::Target {
        &self.spans
    }
}

impl StringScanState {
    fn ensure_start(&mut self, mmap: &Mmap) {
        if self.start == 0 && !self.done {
            let data = &mmap[..];
            let limit = self.search_from.min(data.len());
            let marker = b"\"strings\":[";
            // The strings section sits after the nodes array in V8 output, so
            // scan forward from `search_from` first and only then fall back to
            // a whole-file search (older writers may place it elsewhere).
            if let Some(pos) = rfind_marker(&data[limit..], marker) {
                let s = limit + pos + marker.len();
                self.start = s;
                self.cursor = s;
            } else if let Some(pos) = find_marker(data, marker) {
                let s = pos + marker.len();
                self.start = s;
                self.cursor = s;
            } else {
                self.done = true;
            }
        }
    }

    pub(crate) fn scan_until(&mut self, mmap: &Mmap, target_idx: usize) {
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

/// Lazy string table: byte spans into the mmap are scanned incrementally on
/// demand with SIMD `memchr2`, and decoded strings are cached in a bounded
/// hash map (0 bytes upfront instead of a 38MB `Vec<Option<String>>`).
pub struct StringTable {
    pub(crate) state: std::sync::RwLock<StringScanState>,
    pub(crate) memo: std::sync::RwLock<AHashMap<usize, String>>,
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
    pub(crate) fn new_deferred(search_from: usize) -> StringTable {
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
    pub(crate) fn from_spans_for_test(start: usize, spans: Vec<(u32, u32)>) -> StringTable {
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
pub fn decode_json_string(raw: &[u8]) -> String {
    if memchr(b'\\', raw).is_none() {
        return match std::str::from_utf8(raw) {
            Ok(s) => s.to_string(),
            Err(_) => String::from_utf8_lossy(raw).into_owned(),
        };
    }
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' && i + 1 < raw.len() {
            match raw[i + 1] {
                b'"' => {
                    out.push('"');
                    i += 2;
                }
                b'\\' => {
                    out.push('\\');
                    i += 2;
                }
                b'/' => {
                    out.push('/');
                    i += 2;
                }
                b'b' => {
                    out.push('\x08');
                    i += 2;
                }
                b'f' => {
                    out.push('\x0c');
                    i += 2;
                }
                b'n' => {
                    out.push('\n');
                    i += 2;
                }
                b'r' => {
                    out.push('\r');
                    i += 2;
                }
                b't' => {
                    out.push('\t');
                    i += 2;
                }
                b'u' if i + 5 < raw.len() => {
                    let hex = &raw[i + 2..i + 6];
                    if let Ok(s) = std::str::from_utf8(hex) {
                        if let Ok(cp) = u32::from_str_radix(s, 16) {
                            if (0xD800..=0xDBFF).contains(&cp)
                                && i + 11 < raw.len()
                                && raw[i + 6] == b'\\'
                                && raw[i + 7] == b'u'
                            {
                                let hex2 = &raw[i + 8..i + 12];
                                if let Ok(s2) = std::str::from_utf8(hex2) {
                                    if let Ok(cp2) = u32::from_str_radix(s2, 16) {
                                        if (0xDC00..=0xDFFF).contains(&cp2) {
                                            let full =
                                                0x10000 + ((cp - 0xD800) << 10) + (cp2 - 0xDC00);
                                            if let Some(ch) = char::from_u32(full) {
                                                out.push(ch);
                                                i += 12;
                                                continue;
                                            }
                                        }
                                    }
                                }
                            }
                            if let Some(ch) = char::from_u32(cp) {
                                out.push(ch);
                                i += 6;
                                continue;
                            }
                        }
                    }
                    out.push('\\');
                    i += 1;
                }
                _ => {
                    out.push(raw[i] as char);
                    i += 1;
                }
            }
        } else {
            let start = i;
            while i < raw.len() && raw[i] != b'\\' {
                i += 1;
            }
            if let Ok(s) = std::str::from_utf8(&raw[start..i]) {
                out.push_str(s);
            } else {
                out.push_str(&String::from_utf8_lossy(&raw[start..i]));
            }
        }
    }
    out
}

/// Compute the UTF-8 byte length of `decode_json_string(raw)` without allocating.
#[inline]
pub(crate) fn decoded_json_byte_len(raw: &[u8]) -> usize {
    if memchr(b'\\', raw).is_none() {
        return raw.len();
    }
    decode_json_string(raw).len()
}

/// Fast-path string match directly over raw unescaped JSON bytes in the mmap.
/// Avoids UTF-8 allocation and JSON unescaping when the raw slice does not contain backslashes.
#[inline]
pub(crate) fn raw_string_matches(raw: &[u8], query_name: &str, exact: bool) -> bool {
    if exact && query_name.is_empty() {
        return raw.is_empty();
    }
    if memchr(b'\\', raw).is_none() {
        if let Ok(st) = std::str::from_utf8(raw) {
            return if exact {
                eq_ci(st, query_name) || eq_ci_name(st, query_name)
            } else {
                contains_ci(st, query_name)
            };
        }
    }
    let decoded = decode_json_string(raw);
    if exact {
        eq_ci(&decoded, query_name) || eq_ci_name(&decoded, query_name)
    } else {
        contains_ci(&decoded, query_name)
    }
}

/// Case-insensitive substring match (ASCII fast path, Unicode fallback).
pub(crate) fn contains_ci(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if hay.len() < needle.len() {
        return false;
    }
    if hay.is_ascii() && needle.is_ascii() {
        let hay_b = hay.as_bytes();
        let n_b = needle.as_bytes();
        let n_len = n_b.len();
        let first_lower = n_b[0].to_ascii_lowercase();
        let first_upper = n_b[0].to_ascii_uppercase();
        let mut i = 0;
        let limit = hay_b.len() - n_len;
        while i <= limit {
            let b = hay_b[i];
            if b == first_lower || b == first_upper {
                if hay_b[i..i + n_len].eq_ignore_ascii_case(n_b) {
                    return true;
                }
            }
            i += 1;
        }
        return false;
    }
    hay.to_lowercase().contains(&needle.to_lowercase())
}

/// Case-insensitive equality (ASCII fast path, Unicode fallback).
pub(crate) fn eq_ci(a: &str, b: &str) -> bool {
    if a.len() == b.len() && a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    a.to_lowercase() == b.to_lowercase()
}

/// Match node names by suffix after `" / "`, e.g. `"system / ExternalStringData"`.
pub(crate) fn eq_ci_name(hay: &str, needle: &str) -> bool {
    if eq_ci(hay, needle) {
        return true;
    }
    if let Some(pos) = hay.rfind(" / ") {
        let suffix = &hay[pos + 3..];
        return eq_ci(suffix, needle);
    }
    false
}
