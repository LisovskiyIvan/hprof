use std::collections::HashMap;
use std::fs;
use std::io::BufReader;

use crate::types::*;
use crate::heapsnapshot::stream_json_numbers;

pub struct HeapTimeline {
    file_path: String,
    snapshot_meta: Option<SnapshotMeta>,
}

impl HeapTimeline {
    pub fn new(file_path: String) -> Self {
        Self { file_path, snapshot_meta: None }
    }

    pub fn meta(&mut self) -> crate::Result<&SnapshotMeta> {
        if self.snapshot_meta.is_none() {
            let mut snap = crate::HeapSnapshot::new(self.file_path.clone());
            let meta = snap.meta()?.clone();
            self.snapshot_meta = Some(meta);
        }
        Ok(self.snapshot_meta.as_ref().unwrap())
    }

    pub fn stream_summary(&mut self, top: Option<usize>, _filter: Option<&str>) -> crate::Result<HeapTimelineSummary> {
        let _ = self.meta()?;
        let meta = self.snapshot_meta.as_ref().unwrap();
        let node_fields = &meta.meta.node_fields;
        let node_types = meta.meta.node_types.get(0).map(|v| v.as_slice()).unwrap_or(&[]);
        let node_field_count = node_fields.len();
        let type_offset = node_fields.iter().position(|f| f == "type").ok_or(Error::UnsupportedLayout)?;
        let self_size_offset = node_fields.iter().position(|f| f == "self_size").ok_or(Error::UnsupportedLayout)?;

        let mut reader = BufReader::new(fs::File::open(&self.file_path)?);
        let nodes = stream_json_numbers(&mut reader, b"\"nodes\":[")?;

        let mut by_type_idx: HashMap<usize, TimelineTypeSummary> = HashMap::new();
        let mut total_allocated = 0usize;

        for chunk in nodes.chunks(node_field_count) {
            if chunk.len() < node_field_count { break; }
            let type_idx = chunk[type_offset];
            let self_size = chunk[self_size_offset];
            total_allocated += self_size;

            if self_size > 0 {
                let entry = by_type_idx.entry(type_idx).or_insert(TimelineTypeSummary { allocated: 0, freed: 0, count: 0 });
                entry.allocated += self_size;
                entry.count += 1;
            }
        }

        let top = top.unwrap_or(30);
        let mut by_type: HashMap<String, TimelineTypeSummary> = HashMap::new();
        for (&type_idx, info) in &by_type_idx {
            let type_name = node_types.get(type_idx).cloned().unwrap_or_else(|| type_idx.to_string());
            by_type.insert(type_name, info.clone());
        }

        let mut sorted: Vec<_> = by_type.into_iter().collect();
        sorted.sort_by(|a, b| b.1.allocated.cmp(&a.1.allocated));
        sorted.truncate(top);

        Ok(HeapTimelineSummary {
            total_allocated,
            total_freed: 0,
            by_type: sorted.into_iter().collect(),
        })
    }
}
