#[cfg(test)]
use memchr::memchr;
use memchr::{memchr2, memchr3};
use rayon::prelude::*;

use super::{EdgeColumns, NodeColumns, EDGE_SYNC_STRIDE};
use crate::types::*;

/// Find the index of the closing `]` for the array opened at `open`.
/// Uses SIMD `memchr3`/`memchr2` to jump directly between brackets and quotes
/// instead of inspecting every digit byte.
pub fn find_array_end(data: &[u8], open: usize) -> usize {
    let mut i = open + 1;
    let len = data.len();
    let mut depth = 1i32;
    while i < len {
        let Some(rel) = memchr3(b']', b'[', b'"', &data[i..]) else {
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
pub fn find_matching_brace(s: &str, start: usize) -> Option<usize> {
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

pub fn parse_meta_from_value(json: &str) -> crate::Result<SnapshotMeta> {
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
pub fn find_nth_comma_par(data: &[u8], target_commas: usize) -> Option<usize> {
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
    let k = prefixes
        .partition_point(|&p| p < target_commas)
        .saturating_sub(1);
    let rem = target_commas - prefixes[k];
    let slice = &data[starts[k]..starts[k + 1]];
    memchr::memchr_iter(b',', slice)
        .nth(rem - 1)
        .map(|rel| starts[k] + rel + 1)
}

/// Partition `data` into record-aligned chunks `(byte_start, byte_end, rec_start, rec_end)`
/// in parallel using SIMD comma prefix sums. Returns `None` if `(total_commas + 1)`
/// is not a multiple of `field_count`.
pub fn aligned_record_chunks_par(
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

#[cfg(test)]
pub fn record_aligned_chunks(
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
pub fn node_columns_streaming(
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

    let ok =
        tasks
            .into_par_iter()
            .all(|(bs, be, out_t, out_n, out_id, out_sz, out_ec, out_det)| {
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
            });

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

/// Stream the flat edge array into a single `to_nodes` adjacency column
/// (`u32::MAX` marks weak edges, which dominator analysis ignores) without
/// materializing `types`, `name_or_index` or sync offsets. Indexed by global
/// edge record, so per-node ranges come from `edge_starts`.
pub fn edge_adj_streaming(
    data: &[u8],
    efc: usize,
    nfc: u32,
    offsets: &EdgeFieldOffsets,
    weak_type: u8,
) -> Option<Vec<u32>> {
    let chunks = aligned_record_chunks_par(data, efc)?;
    let total_recs = chunks.last().map(|c| c.3).unwrap_or(0);
    if total_recs == 0 {
        return None;
    }

    let mut to_nodes = vec![0u32; total_recs];
    let mut tasks: Vec<(usize, usize, usize, &mut [u32])> = Vec::with_capacity(chunks.len());
    let mut rem_to = to_nodes.as_mut_slice();
    for &(bs, be, rs, re) in &chunks {
        let len = re - rs;
        let (to_head, to_tail) = rem_to.split_at_mut(len);
        rem_to = to_tail;
        tasks.push((bs, be, rs, to_head));
    }

    let type_off = offsets.type_;
    let to_off = offsets.to_node;
    let nfc_div = nfc.max(1);

    let ok = tasks.into_par_iter().all(|(bs, be, _rs, out_to)| {
        let slice = &data[bs..be];
        let expected_recs = out_to.len();
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
                    out_to[out_i] = if rec[type_off] as u8 == weak_type {
                        u32::MAX
                    } else {
                        rec[to_off] / nfc_div
                    };
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

    Some(to_nodes)
}

/// Stream the flat edge array directly into pre-allocated `EdgeColumns`
/// (`types: Vec<u8>`, `to_nodes: Vec<u32>`, and `sync_offsets` every 1,024
/// edges) without materializing `name_or_index` or a flat `Vec<u32>`.
pub fn edge_columns_streaming(
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

    let ok = tasks.into_par_iter().all(|(bs, be, rs, out_t, out_to)| {
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
