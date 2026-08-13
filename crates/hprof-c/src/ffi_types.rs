use std::ffi::{c_char, c_void, CString};
use std::ptr;

use hprof_core::*;
use serde_derive::Serialize;

#[repr(C)]
pub struct HprofResult {
    pub success: bool,
    pub error: *mut c_char,
    pub handle: *mut c_void,
}

impl HprofResult {
    pub fn ok_empty() -> *mut Self {
        Box::into_raw(Box::new(Self {
            success: true,
            error: ptr::null_mut(),
            handle: ptr::null_mut(),
        }))
    }

    pub fn ok_handle(handle: *mut c_void) -> *mut Self {
        Box::into_raw(Box::new(Self {
            success: true,
            error: ptr::null_mut(),
            handle,
        }))
    }

    pub fn ok_string(s: &str) -> *mut Self {
        let cstr = CString::new(s).unwrap_or_default().into_raw();
        Box::into_raw(Box::new(Self {
            success: true,
            error: ptr::null_mut(),
            handle: cstr as *mut c_void,
        }))
    }

    pub fn err(msg: &str) -> *mut Self {
        Box::into_raw(Box::new(Self {
            success: false,
            error: CString::new(msg).unwrap_or_default().into_raw(),
            handle: ptr::null_mut(),
        }))
    }
}

/// Wrap a `FilterOptions` decoded from C strings. Each NULL pointer => None.
pub unsafe fn decode_filters(
    focus: *const c_char,
    ignore: *const c_char,
    hide: *const c_char,
) -> FilterOptions {
    let mut f = FilterOptions::default();
    if !focus.is_null() {
        if let Some(s) = unsafe { cstr_to_str(focus) } {
            if !s.is_empty() {
                f.focus = Some(s.to_string());
            }
        }
    }
    if !ignore.is_null() {
        if let Some(s) = unsafe { cstr_to_str(ignore) } {
            if !s.is_empty() {
                f.ignore = Some(s.to_string());
            }
        }
    }
    if !hide.is_null() {
        if let Some(s) = unsafe { cstr_to_str(hide) } {
            if !s.is_empty() {
                f.hide = Some(s.to_string());
            }
        }
    }
    f
}

unsafe fn cstr_to_str<'a>(s: *const c_char) -> Option<&'a str> {
    if s.is_null() {
        None
    } else {
        unsafe { std::ffi::CStr::from_ptr(s).to_str().ok() }
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
            by_node_type: s
                .by_node_type
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        TypeSummaryJson {
                            size: v.size,
                            count: v.count,
                        },
                    )
                })
                .collect(),
            by_node_name: s
                .by_node_name
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        TypeSummaryJson {
                            size: v.size,
                            count: v.count,
                        },
                    )
                })
                .collect(),
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
        Self {
            type_: n.type_.clone(),
            name: n.name.clone(),
            self_size: n.self_size,
            id: n.id,
            edge_count: n.edge_count,
        }
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
pub struct HprofPropertiesResult {
    pub node: HprofNodeJson,
    pub properties: Vec<NodeProperty>,
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
            retained: r
                .retained
                .into_iter()
                .map(|e| HprofRetainedEntryJson {
                    node_index: e.node_index,
                    name: e.name,
                    type_: e.type_,
                    self_size: e.self_size,
                    retained_size: e.retained_size,
                    approximate: e.approximate,
                })
                .collect(),
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CumulativeSummaryJson {
    pub total_size: usize,
    pub by_frame: std::collections::HashMap<String, SizeEntryJson>,
    pub by_url: std::collections::HashMap<String, SizeEntryJson>,
    pub by_function: std::collections::HashMap<String, SizeEntryJson>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeEntryJson {
    pub self_size: usize,
    pub cumulative_size: usize,
    pub count: usize,
    /// self_size / total_size * 100.0
    pub self_pct: f64,
    /// cumulative_size / total_size * 100.0
    pub cumulative_pct: f64,
}

impl From<CumulativeSummary> for CumulativeSummaryJson {
    fn from(s: CumulativeSummary) -> Self {
        let total = s.total_size.max(1) as f64;
        let map_it = |m: std::collections::HashMap<String, SizeEntry>| {
            m.into_iter()
                .map(|(k, v)| {
                    let json = SizeEntryJson {
                        self_size: v.self_size,
                        cumulative_size: v.cumulative_size,
                        count: v.count,
                        self_pct: v.self_size as f64 / total * 100.0,
                        cumulative_pct: v.cumulative_size as f64 / total * 100.0,
                    };
                    (k, json)
                })
                .collect()
        };
        Self {
            total_size: s.total_size,
            by_frame: map_it(s.by_frame),
            by_url: map_it(s.by_url),
            by_function: map_it(s.by_function),
        }
    }
}
