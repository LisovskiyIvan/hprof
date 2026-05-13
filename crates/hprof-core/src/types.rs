use serde_derive::Serialize;
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Unsupported node field layout")]
    UnsupportedLayout,
    #[error("Node not found: index {0}")]
    NodeNotFound(usize),
    #[error("Cannot parse snapshot header")]
    HeaderParseFailed,
    #[error("Unsupported file type: .{0}")]
    UnsupportedType(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotMeta {
    pub node_count: usize,
    pub edge_count: usize,
    pub extra_native_bytes: Option<usize>,
    pub meta: SnapshotMetaFields,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotMetaFields {
    pub node_fields: Vec<String>,
    pub node_types: Vec<Vec<String>>,
    pub edge_fields: Vec<String>,
    pub edge_types: Vec<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct NodeFieldOffsets {
    pub type_: usize,
    pub name: usize,
    pub self_size: usize,
    pub id: usize,
    pub edge_count: usize,
}

impl NodeFieldOffsets {
    pub fn from_fields(fields: &[String]) -> crate::Result<Self> {
        let type_ = fields.iter().position(|f| f == "type").ok_or(Error::UnsupportedLayout)?;
        let name = fields.iter().position(|f| f == "name").ok_or(Error::UnsupportedLayout)?;
        let self_size = fields.iter().position(|f| f == "self_size").ok_or(Error::UnsupportedLayout)?;
        let id = fields.iter().position(|f| f == "id").ok_or(Error::UnsupportedLayout)?;
        let edge_count = fields.iter().position(|f| f == "edge_count").ok_or(Error::UnsupportedLayout)?;
        Ok(Self { type_, name, self_size, id, edge_count })
    }
}

#[derive(Debug, Clone)]
pub struct EdgeFieldOffsets {
    pub type_: usize,
    pub name_or_index: usize,
    pub to_node: usize,
}

impl EdgeFieldOffsets {
    pub fn from_fields(fields: &[String]) -> crate::Result<Self> {
        let type_ = fields.iter().position(|f| f == "type").ok_or(Error::UnsupportedLayout)?;
        let name_or_index = fields.iter().position(|f| f == "name_or_index").ok_or(Error::UnsupportedLayout)?;
        let to_node = fields.iter().position(|f| f == "to_node").ok_or(Error::UnsupportedLayout)?;
        Ok(Self { type_, name_or_index, to_node })
    }
}

#[derive(Debug, Clone)]
pub struct HeapSnapshotNode {
    pub type_: String,
    pub name: String,
    pub self_size: usize,
    pub retention_size: Option<usize>,
    pub id: usize,
    pub edge_count: usize,
}

#[derive(Debug, Clone)]
pub struct HeapSnapshotEdge {
    pub type_: String,
    pub name_or_index: EdgeName,
    pub to_node: usize,
}

#[derive(Debug, Clone)]
pub enum EdgeName {
    String(String),
    Index(usize),
}

#[derive(Debug, Clone)]
pub struct HeapSnapshotSummary {
    pub total_size: usize,
    pub total_count: usize,
    pub by_node_type: HashMap<String, TypeSummary>,
    pub by_node_name: HashMap<String, TypeSummary>,
}

#[derive(Debug, Clone)]
pub struct TypeSummary {
    pub size: usize,
    pub count: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct NodePageOptions<'a> {
    pub page: usize,
    pub page_size: usize,
    pub type_filter: Option<&'a str>,
    pub query: Option<&'a str>,
    pub sort: SortField,
    pub dir: SortDir,
}

#[derive(Debug, Clone, Copy)]
pub enum SortField {
    Id,
    Type,
    Name,
    SelfSize,
    EdgeCount,
}

#[derive(Debug, Clone, Copy)]
pub enum SortDir {
    Asc,
    Desc,
}

impl Default for NodePageOptions<'_> {
    fn default() -> Self {
        Self {
            page: 0,
            page_size: 100,
            type_filter: None,
            query: None,
            sort: SortField::SelfSize,
            dir: SortDir::Desc,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NodePage {
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
    pub nodes: Vec<HeapSnapshotNode>,
}

#[derive(Debug, Clone)]
pub struct SearchMatch {
    pub index: usize,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct RetainedEntry {
    pub node_index: usize,
    pub name: String,
    pub type_: String,
    pub self_size: usize,
    pub retained_size: usize,
    pub approximate: bool,
}

#[derive(Debug, Clone)]
pub struct RetainedResult {
    pub approximate: bool,
    pub retained: Vec<RetainedEntry>,
}

#[derive(Debug, Clone)]
pub struct CallFrame {
    pub function_name: String,
    pub script_id: String,
    pub url: String,
    pub line_number: i32,
    pub column_number: i32,
}

#[derive(Debug, Clone)]
pub struct HeapProfileNode {
    pub call_frame: CallFrame,
    pub self_size: usize,
    pub children: Vec<HeapProfileNode>,
}

#[derive(Debug, Clone)]
pub struct HeapProfileResult {
    pub head: HeapProfileNode,
    pub start_time: f64,
    pub end_time: f64,
}

#[derive(Debug, Clone)]
pub struct HeapProfileSummary {
    pub total_size: usize,
    pub by_frame: HashMap<String, usize>,
    pub by_url: HashMap<String, usize>,
    pub by_function: HashMap<String, usize>,
}

#[derive(Debug, Clone)]
pub struct FlatCallFrame {
    pub function_name: String,
    pub url: String,
    pub line_number: i32,
    pub column_number: i32,
    pub self_size: usize,
    pub stack: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TimelineEntry {
    pub type_: String,
    pub timestamp: f64,
    pub node_id: usize,
    pub size: usize,
}

#[derive(Debug, Clone)]
pub struct HeapTimelineSummary {
    pub total_allocated: usize,
    pub total_freed: usize,
    pub by_type: HashMap<String, TimelineTypeSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineTypeSummary {
    pub allocated: usize,
    pub freed: usize,
    pub count: usize,
}

pub fn detect_profile_type(path: &str) -> crate::Result<&'static str> {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "heapsnapshot" => Ok("heapsnapshot"),
        "heapprofile" => Ok("heapprofile"),
        "heaptimeline" => Ok("heaptimeline"),
        _ => Err(Error::UnsupportedType(ext.to_string())),
    }
}

pub fn format_bytes(bytes: usize) -> String {
    let units = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let precision = if value >= 100.0 { 0 } else if value >= 10.0 { 1 } else { 2 };
    format!("{:.prec$} {}", value, units[unit], prec = precision)
}
