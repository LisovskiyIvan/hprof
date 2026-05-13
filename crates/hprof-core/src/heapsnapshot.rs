use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, Read};

use crate::types::*;

pub struct HeapSnapshot {
    file_path: String,
    meta: Option<SnapshotMeta>,
    raw: Option<RawData>,
    edge_starts: Option<Vec<u32>>,
}

#[allow(dead_code)]

pub struct RawData {
    pub meta: SnapshotMeta,
    pub nodes: Vec<usize>,
    pub edges: Vec<usize>,
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
        Self { file_path, meta: None, raw: None, edge_starts: None }
    }

    pub fn meta(&mut self) -> crate::Result<&SnapshotMeta> {
        if self.meta.is_none() {
            self.parse_meta()?;
        }
        Ok(self.meta.as_ref().unwrap())
    }

    fn parse_meta(&mut self) -> crate::Result<()> {
        let file = fs::File::open(&self.file_path)?;
        let mut reader = BufReader::new(file);
        let mut chunk_size = 2 * 1024 * 1024;
        let max_chunk = 64 * 1024 * 1024;

        while chunk_size <= max_chunk {
            let mut buffer = vec![0u8; chunk_size];
            let bytes_read = reader.read(&mut buffer)?;
            let prefix = String::from_utf8_lossy(&buffer[..bytes_read]);

            if let Some(snapshot_start) = prefix.find("\"snapshot\":") {
                if let Some(nodes_start) = prefix.find("\"nodes\":[") {
                    if snapshot_start < nodes_start {
                        let object_start = prefix[snapshot_start..].find('{');
                        if let Some(obj_offset) = object_start {
                            let obj_start = snapshot_start + obj_offset;
                            if let Some(end) = find_matching_brace(&prefix, obj_start) {
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
                reader = BufReader::new(fs::File::open(&self.file_path)?);
                continue;
            }

            return Err(Error::HeaderParseFailed);
        }

        Err(Error::HeaderParseFailed)
    }

    fn ensure_raw(&mut self) -> crate::Result<()> {
        if self.raw.is_some() { return Ok(()); }

        let _ = self.meta()?;
        let meta = self.meta.as_ref().unwrap().clone();

        let mut reader1 = BufReader::new(fs::File::open(&self.file_path)?);
        let nodes = stream_json_numbers(&mut reader1, b"\"nodes\":[")?;

        let mut reader2 = BufReader::new(fs::File::open(&self.file_path)?);
        let edges = stream_json_numbers(&mut reader2, b"\"edges\":[")?;

        let mut reader3 = BufReader::new(fs::File::open(&self.file_path)?);
        let strings = stream_json_strings(&mut reader3, b"\"strings\":[")?;

        let node_offsets = NodeFieldOffsets::from_fields(&meta.meta.node_fields)?;
        let edge_offsets = EdgeFieldOffsets::from_fields(&meta.meta.edge_fields)?;
        let node_types = meta.meta.node_types.get(0).cloned().unwrap_or_default();
        let edge_types = meta.meta.edge_types.get(0).cloned().unwrap_or_default();
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
        let type_idx = raw.nodes[base + raw.node_offsets.type_];
        let name_idx = raw.nodes[base + raw.node_offsets.name];
        let self_size = raw.nodes[base + raw.node_offsets.self_size];
        let id = raw.nodes[base + raw.node_offsets.id];
        let edge_count = raw.nodes[base + raw.node_offsets.edge_count];

        let type_ = raw.node_types.get(type_idx).cloned().unwrap_or_else(|| type_idx.to_string());
        let name = raw.strings.get(name_idx).cloned().unwrap_or_else(|| format!("<string#{}>", name_idx));

        HeapSnapshotNode { type_, name, self_size, retention_size: None, id, edge_count }
    }

    pub fn stream_summary(&mut self, top: usize, filter: Option<&str>) -> crate::Result<HeapSnapshotSummary> {
        let _ = self.meta()?;
        let meta = self.meta.as_ref().unwrap();
        let node_fields = &meta.meta.node_fields;
        let node_types = meta.meta.node_types.get(0).map(|v| v.as_slice()).unwrap_or(&[]);
        let node_field_count = node_fields.len();
        let type_offset = node_fields.iter().position(|f| f == "type").ok_or(Error::UnsupportedLayout)?;
        let name_offset = node_fields.iter().position(|f| f == "name").ok_or(Error::UnsupportedLayout)?;
        let self_size_offset = node_fields.iter().position(|f| f == "self_size").ok_or(Error::UnsupportedLayout)?;

        let file = fs::File::open(&self.file_path)?;
        let mut reader = BufReader::new(file);

        let nodes = stream_json_numbers(&mut reader, b"\"nodes\":[")?;

        let mut by_name_idx: HashMap<usize, (usize, usize, usize)> = HashMap::new();
        let mut by_type_idx: HashMap<usize, (usize, usize)> = HashMap::new();
        let mut total_count = 0usize;
        let mut total_size = 0usize;

        for chunk in nodes.chunks(node_field_count) {
            if chunk.len() < node_field_count { break; }
            let type_idx = chunk[type_offset];
            let name_idx = chunk[name_offset];
            let self_size = chunk[self_size_offset];
            total_count += 1;
            total_size += self_size;

            if self_size > 0 {
                let entry = by_name_idx.entry(name_idx).or_insert((0, 0, type_idx));
                entry.0 += self_size;
                entry.1 += 1;

                let type_entry = by_type_idx.entry(type_idx).or_insert((0, 0));
                type_entry.0 += self_size;
                type_entry.1 += 1;
            }
        }

        let mut reader2 = BufReader::new(fs::File::open(&self.file_path)?);
        let strings = stream_json_strings(&mut reader2, b"\"strings\":[")?;

        let filter_re = filter.and_then(|f| regex::Regex::new(f).ok());
        let mut by_node_name: HashMap<String, TypeSummary> = HashMap::new();
        for (&name_idx, &(size, count, type_idx)) in &by_name_idx {
            let name = strings.get(name_idx).cloned().unwrap_or_else(|| format!("<string#{}>", name_idx));
            if let Some(ref re) = filter_re {
                let type_name = node_types.get(type_idx).map(|s| s.as_str()).unwrap_or("");
                if !re.is_match(&format!("{} {}", name, type_name)) {
                    continue;
                }
            }
            let entry = by_node_name.entry(name).or_insert(TypeSummary { size: 0, count: 0 });
            entry.size += size;
            entry.count += count;
        }

        let mut sorted_names: Vec<_> = by_node_name.into_iter().collect();
        sorted_names.sort_by(|a, b| b.1.size.cmp(&a.1.size));
        sorted_names.truncate(top);

        let mut by_node_type: HashMap<String, TypeSummary> = HashMap::new();
        for (&type_idx, &(size, count)) in &by_type_idx {
            let type_name = node_types.get(type_idx).cloned().unwrap_or_else(|| type_idx.to_string());
            by_node_type.insert(type_name, TypeSummary { size, count });
        }

        Ok(HeapSnapshotSummary {
            total_size,
            total_count,
            by_node_name: sorted_names.into_iter().collect(),
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

        for node_index in 0..raw.meta.node_count {
            let node = Self::create_node(raw, node_index);
            if let Some(ft) = options.type_filter {
                if node.type_ != ft { continue; }
            }
            if let Some(q) = options.query {
                if !q.is_empty() && !node.name.to_lowercase().contains(&q.to_lowercase()) {
                    continue;
                }
            }
            total += 1;

            let candidate = (node_index, node);
            if selected.len() < wanted {
                selected.push(candidate);
                continue;
            }

            if compare_nodes(&candidate, &selected[0], options.sort, options.dir) == std::cmp::Ordering::Less {
                selected[0] = candidate;
            }
        }

        selected.sort_by(|a, b| compare_nodes(a, b, options.sort, options.dir));

        Ok(NodePage {
            total,
            page,
            page_size,
            nodes: selected.into_iter().skip(page * page_size).take(page_size).map(|(_, n)| n).collect(),
        })
    }

    pub fn get_node_edges(&mut self, node_index: usize) -> crate::Result<(HeapSnapshotNode, Vec<HeapSnapshotEdge>)> {
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
                offset += raw.nodes[base + raw.node_offsets.edge_count] as u32;
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
            let type_idx = raw.edges[base + raw.edge_offsets.type_];
            let name_or_index_val = raw.edges[base + raw.edge_offsets.name_or_index];
            let to_node = raw.edges[base + raw.edge_offsets.to_node] / raw.node_field_count;

            let edge_type = raw.edge_types.get(type_idx).cloned().unwrap_or_else(|| type_idx.to_string());
            let name = if edge_type == "element" {
                EdgeName::Index(name_or_index_val)
            } else {
                raw.strings.get(name_or_index_val)
                    .cloned()
                    .map(EdgeName::String)
                    .unwrap_or(EdgeName::Index(name_or_index_val))
            };

            edges.push(HeapSnapshotEdge { type_: edge_type, name_or_index: name, to_node });
        }

        Ok((node, edges))
    }

    pub fn search_strings(&mut self, query: &str) -> crate::Result<Vec<SearchMatch>> {
        self.ensure_raw()?;
        let raw = self.raw.as_ref().unwrap();
        let mut matches_ = Vec::new();
        for (index, value) in raw.strings.iter().enumerate() {
            if value.to_lowercase().contains(&query.to_lowercase()) {
                matches_.push(SearchMatch { index, value: value.clone() });
                if matches_.len() >= 100 { break; }
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
                let worst_pos = selected.iter().enumerate().min_by_key(|(_, (_, n))| n.self_size).map(|(i, _)| i).unwrap();
                if node.self_size > selected[worst_pos].1.self_size {
                    selected[worst_pos] = (node_index, node);
                }
            }
            selected.sort_by(|a, b| b.1.self_size.cmp(&a.1.self_size));
            return Ok(RetainedResult {
                approximate: true,
                retained: selected.into_iter().map(|(idx, node)| RetainedEntry {
                    node_index: idx,
                    name: node.name,
                    type_: node.type_,
                    self_size: node.self_size,
                    retained_size: node.self_size,
                    approximate: true,
                }).collect(),
            });
        }

        let retained = self.build_retained_sizes(raw)?;
        let mut indexed: Vec<(usize, f64)> = retained.into_iter().enumerate().collect();
        indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        indexed.truncate(top_n);

        Ok(RetainedResult {
            approximate: false,
            retained: indexed.into_iter().map(|(idx, size)| {
                let node = Self::create_node(raw, idx);
                RetainedEntry {
                    node_index: idx,
                    name: node.name,
                    type_: node.type_,
                    self_size: node.self_size,
                    retained_size: size as usize,
                    approximate: false,
                }
            }).collect(),
        })
    }

    fn build_retained_sizes(&self, raw: &RawData) -> crate::Result<Vec<f64>> {
        let node_count = raw.meta.node_count;
        let mut edge_starts = vec![0u32; node_count + 1];
        let mut offset = 0u32;
        for i in 0..node_count {
            edge_starts[i] = offset;
            let base = i * raw.node_field_count;
            offset += raw.nodes[base + raw.node_offsets.edge_count] as u32;
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
            if visited[node_idx] { continue; }
            visited[node_idx] = true;
            stack.push(!(node_idx as i64));
            let start = edge_starts[node_idx] as usize;
            let end = edge_starts[node_idx + 1] as usize;
            for edge_index in start..end {
                let base = edge_index * raw.edge_field_count;
                let to_node = raw.edges[base + raw.edge_offsets.to_node] / raw.node_field_count;
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
                let to_node = raw.edges[base + raw.edge_offsets.to_node] / raw.node_field_count;
                if to_node < node_count {
                    preds[to_node].push(n);
                }
            }
        }

        fn intersect(idoms: &[i32], mut a: usize, mut b: usize) -> usize {
            while a != b {
                while a > b { a = idoms[a] as usize; }
                while b > a { b = idoms[b] as usize; }
            }
            a
        }

        let mut changed = true;
        while changed {
            changed = false;
            for &n in &post_order {
                if n == 0 { continue; }
                let pred_list = &preds[n];
                if pred_list.is_empty() { continue; }
                let mut new_idom: Option<usize> = None;
                for &p in pred_list {
                    if idoms[p] == -1 { continue; }
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
            if n == 0 { continue; }
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
    let node_fields = meta_obj.get("node_fields")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let node_types = meta_obj.get("node_types")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| {
            v.as_array().map(|arr| arr.iter().filter_map(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                _ => None,
            }).collect()).unwrap_or_default()
        }).collect())
        .unwrap_or_default();
    let edge_fields = meta_obj.get("edge_fields")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let edge_types = meta_obj.get("edge_types")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| {
            v.as_array().map(|arr| arr.iter().filter_map(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                _ => None,
            }).collect()).unwrap_or_default()
        }).collect())
        .unwrap_or_default();
    Ok(SnapshotMeta {
        node_count: v.get("node_count").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        edge_count: v.get("edge_count").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        extra_native_bytes: v.get("extra_native_bytes").and_then(|v| v.as_u64()).map(|n| n as usize),
        meta: SnapshotMetaFields { node_fields, node_types, edge_fields, edge_types },
    })
}

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

fn _parse_json_numbers(val: &serde_json::Value) -> Vec<usize> {
    match val {
        serde_json::Value::Array(arr) => arr.iter().filter_map(|v| v.as_u64().map(|n| n as usize)).collect(),
        _ => Vec::new(),
    }
}

fn _parse_json_strings(val: &serde_json::Value) -> Vec<String> {
    match val {
        serde_json::Value::Array(arr) => arr.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    }
}

pub fn stream_json_numbers(reader: &mut BufReader<fs::File>, marker: &[u8]) -> crate::Result<Vec<usize>> {
    skip_to(reader, marker)?;
    let mut numbers = Vec::new();
    let mut num_buf = String::new();
    let mut byte = [0u8; 1];
    loop {
        if reader.read(&mut byte)? == 0 { break; }
        let ch = byte[0];
        if ch == b']' { break; }
        if ch == b',' || ch == b' ' || ch == b'\n' || ch == b'\r' || ch == b'\t' {
            if !num_buf.is_empty() {
                if let Ok(n) = num_buf.parse::<usize>() {
                    numbers.push(n);
                }
                num_buf.clear();
            }
        } else if ch == b'-' || ch.is_ascii_digit() {
            num_buf.push(ch as char);
        }
    }
    if !num_buf.is_empty() {
        if let Ok(n) = num_buf.parse::<usize>() {
            numbers.push(n);
        }
    }
    Ok(numbers)
}

pub fn stream_json_strings(reader: &mut BufReader<fs::File>, marker: &[u8]) -> crate::Result<Vec<String>> {
    skip_to(reader, marker)?;
    let mut strings = Vec::new();
    let mut buf = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut byte = [0u8; 1];
    loop {
        if reader.read(&mut byte)? == 0 { break; }
        let ch = byte[0];
        if !in_string {
            if ch == b'"' {
                in_string = true;
                buf.clear();
            } else if ch == b']' {
                break;
            }
        } else if escaped {
            let decoded = match ch {
                b'n' => b'\n',
                b'r' => b'\r',
                b't' => b'\t',
                b'\\' => b'\\',
                b'"' => b'"',
                b'/' => b'/',
                b'b' => 8,
                b'f' => 12,
                _ => ch,
            };
            buf.push(decoded);
            escaped = false;
        } else if ch == b'\\' {
            escaped = true;
        } else if ch == b'"' {
            in_string = false;
            strings.push(String::from_utf8_lossy(&buf).into_owned());
        } else {
            buf.push(ch);
        }
    }
    Ok(strings)
}

pub fn skip_to(reader: &mut BufReader<fs::File>, marker: &[u8]) -> crate::Result<()> {
    let mut window = vec![0u8; marker.len()];
    reader.read_exact(&mut window)?;
    let mut byte = [0u8; 1];
    let marker_len = marker.len();
    loop {
        if window == marker { return Ok(()); }
        if reader.read(&mut byte)? == 0 { break; }
        window.copy_within(1..marker_len, 0);
        window[marker_len - 1] = byte[0];
    }
    Err(Error::HeaderParseFailed)
}
