use memchr::memchr;
use rayon::prelude::*;
use std::collections::HashMap;

use super::diff::{push_bounded_by_key, snapshot_object};
use super::parser::{aligned_record_chunks_par, parse_numbers_par};
use super::strings::{contains_ci, decode_json_string, decoded_json_byte_len};
use super::HeapSnapshot;
use crate::types::*;

impl HeapSnapshot {
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
            push_bounded_by_key(&mut candidates, (index, size), limit, |item| {
                (item.1, item.0)
            });
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

    /// Size histogram over power-of-two self-size buckets.
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

        let keep_all = limit == 0;
        let cap = if keep_all {
            packed.len() + 1
        } else {
            limit + 1
        };
        let mut top_candidates: Vec<(usize, usize, usize, usize)> =
            Vec::with_capacity(cap.min(1024));

        let mut push_candidate =
            |ref_bytes: usize, refs: usize, byte_len: usize, rep_idx: usize| {
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
                let mut groups: Vec<(usize, usize, usize, Vec<u8>)> = Vec::with_capacity(j - i);
                for &pk in &packed[i..j] {
                    let idx = (pk & 0xFF_FFFF) as usize;
                    let refs = references[idx] as usize;
                    let (s, e) = spans[idx];
                    let raw_slice = &mmap_bytes[sbase + s as usize..sbase + e as usize];
                    let content: std::borrow::Cow<'_, [u8]> = if memchr(b'\\', raw_slice).is_none()
                    {
                        std::borrow::Cow::Borrowed(raw_slice)
                    } else {
                        std::borrow::Cow::Owned(decode_json_string(raw_slice).into_bytes())
                    };
                    if let Some(g) = groups
                        .iter_mut()
                        .find(|g| g.3.as_slice() == content.as_ref())
                    {
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

        // 1. Dominator suspects
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

        // 2. Accumulator collections
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
                    node.name,
                    node.type_,
                    edge_count,
                    format_bytes(self_size)
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
            let top_names: Vec<String> = detached
                .entries
                .iter()
                .take(3)
                .map(|e| e.node.name.clone())
                .collect();
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
    /// Uses an in-register BitSet (`Vec<u64>`) to hold matching string flags,
    /// fitting in L2 cache instead of a 1.58 MB `Vec<bool>`.
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

        let num_words = (num_strings + 63) / 64;
        let mut matching_bits = vec![0u64; num_words];

        let word_matches: Vec<u64> = (0..num_words)
            .into_par_iter()
            .map(|word_idx| {
                let mut word = 0u64;
                let start_idx = word_idx * 64;
                let end_idx = (start_idx + 64).min(num_strings);
                for idx in start_idx..end_idx {
                    let (s, e) = spans[idx];
                    let raw_slice = &mmap_bytes[sbase + s as usize..sbase + e as usize];
                    if raw_slice.len() < 4 {
                        continue;
                    }
                    let matched = if memchr(b'\\', raw_slice).is_none() {
                        if let Ok(st) = std::str::from_utf8(raw_slice) {
                            buffer_needles.iter().any(|needle| contains_ci(st, needle))
                        } else {
                            false
                        }
                    } else {
                        let decoded = decode_json_string(raw_slice);
                        buffer_needles
                            .iter()
                            .any(|needle| contains_ci(&decoded, needle))
                    };
                    if matched {
                        word |= 1u64 << (idx - start_idx);
                    }
                }
                word
            })
            .collect();
        drop(spans_guard);
        matching_bits.copy_from_slice(&word_matches);

        let mut buffer_indices: Vec<usize> = Vec::new();
        let mut total_self_size = 0usize;

        for i in 0..total_nodes {
            let name_idx = raw.nodes.names[i] as usize;
            if name_idx < num_strings
                && ((matching_bits[name_idx / 64] & (1u64 << (name_idx % 64))) != 0)
            {
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
                format!(
                    "{} ({}, id={})",
                    owner_node.name, owner_node.type_, owner_node.id
                )
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

        let context_edge_type = raw.edge_types.iter().position(|t| t == "context");
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
                        if let Some(var_name) =
                            raw.strings.resolve(&raw.mmap, var_name_idx as usize)
                        {
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

// Source-map footprint: byte-signature scan for source-map data in the heap.
// No map parsing — categories come from marker substrings only.
const SMAP_NONE: u8 = 0;
const SMAP_MAP_JSON: u8 = 1;
const SMAP_INLINE_URI: u8 = 2;
const SMAP_COMMENT: u8 = 3;

// Known source-map library constructors (matched against node names).
const SMAP_LIB_NAMES: [&str; 9] = [
    "SourceMap",
    "SourceMapConsumer",
    "BasicSourceMapConsumer",
    "IndexedSourceMapConsumer",
    "SourceMapGenerator",
    "SourceNode",
    "SourceMapConsumerV3",
    "MappingList",
    "Mapping",
];

fn smap_category_name(cat: u8) -> &'static str {
    match cat {
        SMAP_MAP_JSON => "mapJson",
        SMAP_INLINE_URI => "inlineUri",
        _ => "comment",
    }
}

/// Classify one raw (still JSON-escaped) string span. Prefilters on
/// quote-free cores with `memmem` over the whole span, then decodes once and
/// applies the exact markers with precedence mapJson > inlineUri > comment.
fn classify_sourcemap_str(raw: &[u8]) -> u8 {
    use memchr::memmem::find;
    if find(raw, b"sourceMappingURL").is_none()
        && find(raw, b"mappings").is_none()
        && find(raw, b"sourcesContent").is_none()
        && find(raw, b"version").is_none()
    {
        return SMAP_NONE;
    }
    let decoded = decode_json_string(raw);
    if decoded.trim_start().starts_with("{\"version\":3")
        || decoded.contains("\"mappings\":\"")
        || decoded.contains("\"sourcesContent\"")
    {
        return SMAP_MAP_JSON;
    }
    if decoded.contains("sourceMappingURL=data:") {
        return SMAP_INLINE_URI;
    }
    if decoded.contains("sourceMappingURL=") {
        return SMAP_COMMENT;
    }
    SMAP_NONE
}

/// Preview text: decoded content truncated to ~80 chars, controls escaped.
fn smap_preview(decoded: &str) -> String {
    let mut out = String::with_capacity(80);
    for c in decoded.chars().take(80) {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

impl HeapSnapshot {
    /// Measure how much of the heap is source-map data. Dev builds keep big
    /// maps in the heap; that share is a build artifact, not app memory.
    pub fn sourcemap_footprint(&mut self, top: usize) -> crate::Result<SourceMapFootprint> {
        self.ensure_raw()?;
        self.ensure_raw_summary()?;
        let raw = self.raw.as_ref().unwrap();
        let heap_total_size = self.raw_summary.as_ref().unwrap().total_size;

        // 1. per-string category over ALL spans, in parallel.
        let (sbase, spans_guard) = raw.strings.all_spans(&raw.mmap);
        let spans: &[(u32, u32)] = &spans_guard;
        let mmap_bytes = &raw.mmap[..];
        let cats: Vec<u8> = (0..spans.len())
            .into_par_iter()
            .map(|idx| {
                let (s, e) = spans[idx];
                classify_sourcemap_str(&mmap_bytes[sbase + s as usize..sbase + e as usize])
            })
            .collect();
        drop(spans_guard);

        let string_type = raw
            .node_types
            .iter()
            .position(|t| t == "string")
            .unwrap_or(usize::MAX);
        let col = &raw.nodes;
        let mut cat_bytes = [0usize; 4];
        let mut cat_counts = [0usize; 4];
        let mut per_string: HashMap<u32, (usize, usize)> = HashMap::new();
        let mut lib: HashMap<String, (usize, usize)> = HashMap::new();

        // 2-3. single pass over node columns.
        for n in 0..raw.meta.node_count {
            let size = col.self_sizes[n] as usize;
            let name_idx = col.names[n];
            if col.types[n] as usize == string_type {
                if let Some(&cat) = cats.get(name_idx as usize) {
                    if cat != SMAP_NONE {
                        cat_bytes[cat as usize] += size;
                        cat_counts[cat as usize] += 1;
                        let e = per_string.entry(name_idx).or_insert((0, 0));
                        e.0 += size;
                        e.1 += 1;
                    }
                }
            }
            if let Some(name) = raw.strings.resolve(&raw.mmap, name_idx as usize) {
                if SMAP_LIB_NAMES.contains(&name.as_str()) {
                    let e = lib.entry(name).or_insert((0, 0));
                    e.0 += 1;
                    e.1 += size;
                }
            }
        }

        let bytes: usize = cat_bytes[1..].iter().sum();
        let count: usize = cat_counts[1..].iter().sum();
        let share_pct = if heap_total_size == 0 {
            0.0
        } else {
            bytes as f64 / heap_total_size as f64 * 100.0
        };

        let mut lib_objects: Vec<SourceMapLibEntry> = lib
            .into_iter()
            .map(|(name, (count, self_size))| SourceMapLibEntry {
                name,
                count,
                self_size,
            })
            .collect();
        lib_objects.sort_by(|a, b| {
            b.self_size
                .cmp(&a.self_size)
                .then_with(|| a.name.cmp(&b.name))
        });

        let raw_ref = self.raw.as_ref().unwrap();
        let mut top_strings: Vec<SourceMapStringEntry> = per_string
            .iter()
            .map(|(&idx, &(size, count))| {
                let decoded = raw_ref
                    .strings
                    .resolve(&raw_ref.mmap, idx as usize)
                    .unwrap_or_default();
                SourceMapStringEntry {
                    size,
                    count,
                    category: smap_category_name(cats[idx as usize]).to_string(),
                    preview: smap_preview(&decoded),
                }
            })
            .collect();
        top_strings.sort_by(|a, b| {
            b.size
                .cmp(&a.size)
                .then_with(|| b.count.cmp(&a.count))
                .then_with(|| a.preview.cmp(&b.preview))
        });
        if top > 0 && top_strings.len() > top {
            top_strings.truncate(top);
        }

        Ok(SourceMapFootprint {
            heap_total_size,
            bytes,
            count,
            share_pct,
            map_json_bytes: cat_bytes[SMAP_MAP_JSON as usize],
            map_json_count: cat_counts[SMAP_MAP_JSON as usize],
            inline_uri_bytes: cat_bytes[SMAP_INLINE_URI as usize],
            inline_uri_count: cat_counts[SMAP_INLINE_URI as usize],
            comment_bytes: cat_bytes[SMAP_COMMENT as usize],
            comment_count: cat_counts[SMAP_COMMENT as usize],
            lib_objects,
            top_strings,
        })
    }
}

#[cfg(test)]
mod footprint_tests {
    use super::*;

    fn write_snapshot(name: &str, json: &str) -> String {
        let path = std::env::temp_dir().join(format!(
            "hprof_smap_{name}_{}.heapsnapshot",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, json).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn footprint_categories_and_lib() {
        let json = r#"{"snapshot":{"meta":{
            "node_fields":["type","name","id","self_size","edge_count","detachedness"],
            "node_types":[["hidden","object","string"],"string","number","number","number","number"],
            "edge_fields":["type","name_or_index","to_node"],
            "edge_types":[["property"]],
            "trace_function_info_fields":["function_id","name","script_name","script_id","line","column"],
            "trace_node_fields":["id","function_info_index","count","size","children"],
            "sample_fields":["timestamp_us","last_assigned_id"],
            "location_fields":["object_index","script_id","line","column"]
        },"node_count":5,"edge_count":0},
        "nodes":[2,1,1,1000,0,0, 2,2,2,500,0,0, 2,3,3,32,0,0, 1,4,4,16,0,0, 2,5,5,64,0,0],
        "edges":[],
        "strings":["","{\"version\":3,\"sources\":[\"a.ts\"],\"mappings\":\"AAAA\"}","//# sourceMappingURL=data:application/json;base64,AAAA","//# sourceMappingURL=app.js.map","SourceMapConsumer","hello mappings and sourcesContent-ish"]}"#;
        let path = write_snapshot("foot", json);
        let mut snap = HeapSnapshot::new(path.clone());
        let fp = snap.sourcemap_footprint(10).unwrap();

        assert_eq!(fp.heap_total_size, 1612);
        assert_eq!(fp.bytes, 1532);
        assert_eq!(fp.count, 3);
        assert!((fp.share_pct - 1532.0 / 1612.0 * 100.0).abs() < 1e-9);
        assert_eq!((fp.map_json_bytes, fp.map_json_count), (1000, 1));
        assert_eq!((fp.inline_uri_bytes, fp.inline_uri_count), (500, 1));
        assert_eq!((fp.comment_bytes, fp.comment_count), (32, 1));
        assert_eq!(fp.lib_objects.len(), 1);
        assert_eq!(fp.lib_objects[0].name, "SourceMapConsumer");
        assert_eq!(fp.lib_objects[0].count, 1);
        assert_eq!(fp.lib_objects[0].self_size, 16);
        assert_eq!(fp.top_strings.len(), 3);
        assert_eq!(fp.top_strings[0].size, 1000);
        assert_eq!(fp.top_strings[0].category, "mapJson");
        assert_eq!(fp.top_strings[1].category, "inlineUri");
        assert_eq!(fp.top_strings[2].category, "comment");
        assert!(!fp.top_strings[0].preview.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn footprint_empty_snapshot() {
        let json = r#"{"snapshot":{"meta":{
            "node_fields":["type","name","id","self_size","edge_count","detachedness"],
            "node_types":[["hidden","object","string"],"string","number","number","number","number"],
            "edge_fields":["type","name_or_index","to_node"],
            "edge_types":[["property"]],
            "trace_function_info_fields":[],
            "trace_node_fields":[],
            "sample_fields":[],
            "location_fields":[]
        },"node_count":1,"edge_count":0},
        "nodes":[1,1,1,8,0,0],
        "edges":[],
        "strings":["","plain"]}"#;
        let path = write_snapshot("empty", json);
        let mut snap = HeapSnapshot::new(path.clone());
        let fp = snap.sourcemap_footprint(10).unwrap();
        assert_eq!(fp.bytes, 0);
        assert_eq!(fp.count, 0);
        assert_eq!(fp.share_pct, 0.0);
        assert!(fp.lib_objects.is_empty() && fp.top_strings.is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
