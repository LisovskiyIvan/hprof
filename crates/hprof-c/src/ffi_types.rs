use std::ffi::{CString, c_char, c_void};
use std::ptr;

use serde_derive::Serialize;
use hprof_core::*;

#[repr(C)]
pub struct HprofResult {
    pub success: bool,
    pub error: *mut c_char,
    pub handle: *mut c_void,
}

impl HprofResult {
    pub fn ok_empty() -> *mut Self {
        Box::into_raw(Box::new(Self { success: true, error: ptr::null_mut(), handle: ptr::null_mut() }))
    }

    pub fn ok_handle(handle: *mut c_void) -> *mut Self {
        Box::into_raw(Box::new(Self { success: true, error: ptr::null_mut(), handle }))
    }

    pub fn ok_string(s: &str) -> *mut Self {
        let cstr = CString::new(s).unwrap_or_default().into_raw();
        Box::into_raw(Box::new(Self { success: true, error: ptr::null_mut(), handle: cstr as *mut c_void }))
    }

    pub fn err(msg: &str) -> *mut Self {
        Box::into_raw(Box::new(Self {
            success: false,
            error: CString::new(msg).unwrap_or_default().into_raw(),
            handle: ptr::null_mut(),
        }))
    }
}

#[derive(Serialize)]
pub struct HprofSummaryJson {
    pub total_size: usize,
    pub total_count: usize,
    pub by_node_type: std::collections::HashMap<String, TypeSummaryJson>,
    pub by_node_name: std::collections::HashMap<String, TypeSummaryJson>,
}

#[derive(Serialize)]
pub struct TypeSummaryJson {
    pub size: usize,
    pub count: usize,
}

impl From<HeapSnapshotSummary> for HprofSummaryJson {
    fn from(s: HeapSnapshotSummary) -> Self {
        Self {
            total_size: s.total_size,
            total_count: s.total_count,
            by_node_type: s.by_node_type.into_iter().map(|(k, v)| (k, TypeSummaryJson { size: v.size, count: v.count })).collect(),
            by_node_name: s.by_node_name.into_iter().map(|(k, v)| (k, TypeSummaryJson { size: v.size, count: v.count })).collect(),
        }
    }
}

#[derive(Serialize)]
pub struct HprofNodePageJson {
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
    pub nodes: Vec<HprofNodeJson>,
}

impl From<NodePage> for HprofNodePageJson {
    fn from(p: NodePage) -> Self {
        Self {
            total: p.total,
            page: p.page,
            page_size: p.page_size,
            nodes: p.nodes.iter().map(HprofNodeJson::from_node).collect(),
        }
    }
}

#[derive(Serialize)]
pub struct HprofNodeJson {
    #[serde(rename = "type")]
    pub type_: String,
    pub name: String,
    pub self_size: usize,
    pub id: usize,
    pub edge_count: usize,
}

impl HprofNodeJson {
    pub fn from_node(n: &HeapSnapshotNode) -> Self {
        Self { type_: n.type_.clone(), name: n.name.clone(), self_size: n.self_size, id: n.id, edge_count: n.edge_count }
    }
}

#[derive(Serialize)]
pub struct HprofEdgeJson {
    #[serde(rename = "type")]
    pub type_: String,
    pub name_or_index: serde_json::Value,
    pub to_node: usize,
}

impl HprofEdgeJson {
    pub fn from_edge(e: HeapSnapshotEdge) -> Self {
        Self {
            type_: e.type_,
            name_or_index: match e.name_or_index {
                EdgeName::String(s) => serde_json::Value::String(s),
                EdgeName::Index(i) => serde_json::Value::Number(i.into()),
            },
            to_node: e.to_node,
        }
    }
}

#[derive(Serialize)]
pub struct HprofEdgesResult {
    pub node: HprofNodeJson,
    pub edges: Vec<HprofEdgeJson>,
}

#[derive(Serialize)]
pub struct HprofSearchMatchJson {
    pub index: usize,
    pub value: String,
}

#[derive(Serialize)]
pub struct HprofRetainedResultJson {
    pub approximate: bool,
    pub retained: Vec<HprofRetainedEntryJson>,
}

impl From<RetainedResult> for HprofRetainedResultJson {
    fn from(r: RetainedResult) -> Self {
        Self {
            approximate: r.approximate,
            retained: r.retained.into_iter().map(|e| HprofRetainedEntryJson {
                node_index: e.node_index,
                name: e.name,
                type_: e.type_,
                self_size: e.self_size,
                retained_size: e.retained_size,
                approximate: e.approximate,
            }).collect(),
        }
    }
}

#[derive(Serialize)]
pub struct HprofRetainedEntryJson {
    pub node_index: usize,
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub self_size: usize,
    pub retained_size: usize,
    pub approximate: bool,
}

#[derive(Serialize)]
pub struct HprofProfileDataJson {
    pub start_time: f64,
    pub end_time: f64,
}
