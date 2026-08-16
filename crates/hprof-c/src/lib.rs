use std::ffi::{c_char, c_void, CStr, CString};

use hprof_core::*;

mod ffi_types;
use ffi_types::*;

unsafe fn cstr_to_str<'a>(s: *const c_char) -> Option<&'a str> {
    if s.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(s).to_str().ok() }.filter(|s| !s.is_empty())
    }
}

struct HprofSnapshot {
    snapshot: HeapSnapshot,
}

struct HprofProfile {
    profile: HeapProfile,
}

struct HprofTimeline {
    timeline: HeapTimeline,
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_open(path: *const c_char) -> *mut HprofResult {
    let path_str = match unsafe { cstr_to_str(path) } {
        Some(s) => s,
        _none => return HprofResult::err("path is null"),
    };
    let snapshot = HeapSnapshot::new(path_str.to_string());
    let instance = Box::new(HprofSnapshot { snapshot });
    HprofResult::ok_handle(Box::into_raw(instance) as *mut c_void)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_meta(handle: *mut c_void) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst.snapshot.meta() {
        Ok(meta) => {
            let json = serde_json::to_string(meta).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_summary(
    handle: *mut c_void,
    top: u32,
    filter: *const c_char,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let filter_str = unsafe { cstr_to_str(filter) };
    match inst.snapshot.stream_summary(top as usize, filter_str) {
        Ok(summary) => {
            let json = serde_json::to_string(&HprofSummaryJson::from(summary)).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_node_page(
    handle: *mut c_void,
    page: u32,
    page_size: u32,
    type_filter: *const c_char,
    query: *const c_char,
    sort: u8,
    dir: u8,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let sort_field = match sort {
        0 => SortField::SelfSize,
        1 => SortField::Id,
        2 => SortField::Type,
        3 => SortField::Name,
        4 => SortField::EdgeCount,
        _ => SortField::SelfSize,
    };
    let sort_dir = if dir == 0 {
        SortDir::Desc
    } else {
        SortDir::Asc
    };
    let options = NodePageOptions {
        page: page as usize,
        page_size: page_size as usize,
        type_filter: unsafe { cstr_to_str(type_filter) },
        query: unsafe { cstr_to_str(query) },
        sort: sort_field,
        dir: sort_dir,
    };
    match inst.snapshot.get_node_page(options) {
        Ok(page_result) => {
            let json =
                serde_json::to_string(&HprofNodePageJson::from(page_result)).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_edges(
    handle: *mut c_void,
    node_index: u32,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst.snapshot.get_node_edges(node_index as usize) {
        Ok((node, edges)) => {
            let result = HprofEdgesResult {
                node: HprofNodeJson::from_node(&node),
                edges: edges.into_iter().map(HprofEdgeJson::from_edge).collect(),
            };
            let json = serde_json::to_string(&result).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_search(
    handle: *mut c_void,
    query: *const c_char,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let query_str = unsafe { cstr_to_str(query) }.unwrap_or("");
    match inst.snapshot.search_strings(query_str) {
        Ok(matches_) => {
            let json = serde_json::to_string(
                &matches_
                    .into_iter()
                    .map(|m| HprofSearchMatchJson {
                        index: m.index,
                        value: m.value,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_retained(
    handle: *mut c_void,
    top_n: u32,
) -> *mut HprofResult {
    unsafe { hprof_snapshot_retained_mode(handle, top_n, 0) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_retained_mode(
    handle: *mut c_void,
    top_n: u32,
    exact: u8,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let result = if exact != 0 {
        inst.snapshot.get_retained_entries_exact(top_n as usize)
    } else {
        inst.snapshot.get_retained_entries(top_n as usize)
    };
    match result {
        Ok(result) => {
            let json =
                serde_json::to_string(&HprofRetainedResultJson::from(result)).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_destroy(handle: *mut c_void) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle as *mut HprofSnapshot));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_profile_open(path: *const c_char) -> *mut HprofResult {
    let path_str = match unsafe { cstr_to_str(path) } {
        Some(s) => s,
        None => return HprofResult::err("path is null"),
    };
    let profile = HeapProfile::new(path_str.to_string());
    let instance = Box::new(HprofProfile { profile });
    HprofResult::ok_handle(Box::into_raw(instance) as *mut c_void)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_profile_data(handle: *mut c_void) -> *mut HprofResult {
    let inst = match unsafe { as_profile(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst.profile.data() {
        Ok(data) => {
            let json = serde_json::to_string(&HprofProfileDataJson {
                start_time: data.start_time,
                end_time: data.end_time,
            })
            .unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_profile_summarize(
    handle: *mut c_void,
    top: u32,
    filter: *const c_char,
) -> *mut HprofResult {
    let inst = match unsafe { as_profile(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let filter_str = unsafe { cstr_to_str(filter) };
    match inst
        .profile
        .summarize(if top == 0 { None } else { Some(top as usize) }, filter_str)
    {
        Ok(summary) => {
            let json = serde_json::to_string(&serde_json::json!({
                "totalSize": summary.total_size,
                "byFrame": summary.by_frame,
                "byUrl": summary.by_url,
                "byFunction": summary.by_function,
            }))
            .unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_profile_flatten(handle: *mut c_void) -> *mut HprofResult {
    let inst = match unsafe { as_profile(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst.profile.flatten() {
        Ok(frames) => {
            let json = serde_json::to_string(
                &frames
                    .into_iter()
                    .map(|f| {
                        serde_json::json!({
                            "functionName": f.function_name,
                            "url": f.url,
                            "lineNumber": f.line_number,
                            "columnNumber": f.column_number,
                            "selfSize": f.self_size,
                            "stack": f.stack,
                        })
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_profile_summarize_cumulative(
    handle: *mut c_void,
    top: u32,
    focus: *const c_char,
    ignore: *const c_char,
    hide: *const c_char,
) -> *mut HprofResult {
    let inst = match unsafe { as_profile(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let filters = unsafe { decode_filters(focus, ignore, hide) };
    let top_arg = if top == 0 { None } else { Some(top as usize) };
    match inst.profile.summarize_cumulative(top_arg, &filters) {
        Ok(summary) => {
            let json =
                serde_json::to_string(&CumulativeSummaryJson::from(summary)).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_profile_flamegraph(
    handle: *mut c_void,
    focus: *const c_char,
    ignore: *const c_char,
    hide: *const c_char,
) -> *mut HprofResult {
    let inst = match unsafe { as_profile(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let filters = unsafe { decode_filters(focus, ignore, hide) };
    match inst.profile.to_flamegraph(&filters) {
        Ok(frame) => {
            let json = serde_json::to_string(&frame).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_profile_dot(
    handle: *mut c_void,
    top: u32,
    focus: *const c_char,
    ignore: *const c_char,
    hide: *const c_char,
) -> *mut HprofResult {
    let inst = match unsafe { as_profile(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let filters = unsafe { decode_filters(focus, ignore, hide) };
    let top_arg = if top == 0 { None } else { Some(top as usize) };
    match inst.profile.to_dot(top_arg, &filters) {
        Ok(dot) => HprofResult::ok_string(&dot),
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_profile_treemap(
    handle: *mut c_void,
    focus: *const c_char,
    ignore: *const c_char,
    hide: *const c_char,
) -> *mut HprofResult {
    let inst = match unsafe { as_profile(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let filters = unsafe { decode_filters(focus, ignore, hide) };
    match inst.profile.to_treemap(&filters) {
        Ok(node) => {
            let json = serde_json::to_string(&node).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_profile_diff(
    handle: *mut c_void,
    baseline_handle: *mut c_void,
) -> *mut HprofResult {
    let inst = match unsafe { as_profile(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let baseline = match unsafe { as_profile(baseline_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst.profile.diff(&mut baseline.profile) {
        Ok(diff) => {
            let json = serde_json::to_string(&diff).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_flamegraph(
    handle: *mut c_void,
    top: u32,
    filter: *const c_char,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let filter_str = unsafe { cstr_to_str(filter) };
    let top_arg = if top == 0 { None } else { Some(top as usize) };
    match inst.snapshot.to_flamegraph(top_arg, filter_str) {
        Ok(frame) => {
            let json = serde_json::to_string(&frame).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_treemap(
    handle: *mut c_void,
    top: u32,
    filter: *const c_char,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let filter_str = unsafe { cstr_to_str(filter) };
    let top_arg = if top == 0 { None } else { Some(top as usize) };
    match inst.snapshot.to_treemap(top_arg, filter_str) {
        Ok(node) => {
            let json = serde_json::to_string(&node).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_diff(
    handle: *mut c_void,
    baseline_handle: *mut c_void,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let baseline = match unsafe { as_snapshot(baseline_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst.snapshot.diff(&mut baseline.snapshot) {
        Ok(diff) => match inst.snapshot.object_diff(&mut baseline.snapshot, 100) {
            Ok(objects) => {
                let mut value = serde_json::to_value(diff).unwrap_or_default();
                if let serde_json::Value::Object(map) = &mut value {
                    map.insert(
                        "objects".to_string(),
                        serde_json::to_value(objects).unwrap_or_default(),
                    );
                }
                HprofResult::ok_string(&serde_json::to_string(&value).unwrap_or_default())
            }
            Err(e) => HprofResult::err(&e.to_string()),
        },
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_object_diff(
    handle: *mut c_void,
    baseline_handle: *mut c_void,
    limit: u32,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let baseline = match unsafe { as_snapshot(baseline_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst
        .snapshot
        .object_diff(&mut baseline.snapshot, limit as usize)
    {
        Ok(diff) => HprofResult::ok_string(&serde_json::to_string(&diff).unwrap_or_default()),
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_detached(
    handle: *mut c_void,
    limit: u32,
    depth: u32,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst
        .snapshot
        .detached_summary(limit as usize, depth as usize)
    {
        Ok(result) => HprofResult::ok_string(&serde_json::to_string(&result).unwrap_or_default()),
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_size_histogram(handle: *mut c_void) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst.snapshot.size_histogram() {
        Ok(result) => HprofResult::ok_string(&serde_json::to_string(&result).unwrap_or_default()),
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_string_stats(
    handle: *mut c_void,
    limit: u32,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst.snapshot.string_stats(limit as usize) {
        Ok(result) => HprofResult::ok_string(&serde_json::to_string(&result).unwrap_or_default()),
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_find_edges(
    handle: *mut c_void,
    exact: u8,
    name: *const c_char,
    type_filter: *const c_char,
    edge_type: *const c_char,
    limit: u32,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let query = EdgeQuery {
        name: unsafe { cstr_to_str(name) }.unwrap_or("").to_string(),
        exact: exact != 0,
        type_filter: unsafe { cstr_to_str(type_filter) }
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        edge_type: unsafe { cstr_to_str(edge_type) }
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        limit: limit as usize,
    };
    match inst.snapshot.find_edges(&query) {
        Ok(result) => HprofResult::ok_string(&serde_json::to_string(&result).unwrap_or_default()),
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_dot(
    handle: *mut c_void,
    node_index: u32,
    depth: u32,
    max_nodes: u32,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst
        .snapshot
        .to_dot_subgraph(node_index as usize, depth as usize, max_nodes as usize)
    {
        Ok(result) => HprofResult::ok_string(&result),
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_find(
    handle: *mut c_void,
    exact: u8,
    name: *const c_char,
    min_self: u64,
    type_filter: *const c_char,
    limit: u32,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let query = NameQuery {
        exact: exact != 0,
        name: unsafe { cstr_to_str(name) }.unwrap_or("").to_string(),
        min_self: min_self as usize,
        type_filter: unsafe { cstr_to_str(type_filter) }
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        limit: limit as usize,
    };
    match inst.snapshot.find_nodes(&query) {
        Ok(matches) => {
            let json = serde_json::to_string(&matches).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_properties(
    handle: *mut c_void,
    node_index: u32,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst.snapshot.get_node_properties(node_index as usize) {
        Ok((node, props)) => {
            let result = HprofPropertiesResult {
                node: HprofNodeJson::from_node(&node),
                properties: props,
            };
            let json = serde_json::to_string(&result).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_retainers(
    handle: *mut c_void,
    node_index: u32,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst.snapshot.get_retainers(node_index as usize) {
        Ok(retainers) => {
            let json = serde_json::to_string(&retainers).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_chain(
    handle: *mut c_void,
    node_index: u32,
    max_depth: u32,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst
        .snapshot
        .retainer_chain(node_index as usize, max_depth as usize)
    {
        Ok(chain) => {
            let json = serde_json::to_string(&chain).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_snapshot_owners(
    handle: *mut c_void,
    exact: u8,
    name: *const c_char,
    min_self: u64,
    max_depth: u32,
    top: u32,
) -> *mut HprofResult {
    let inst = match unsafe { as_snapshot(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let query = NameQuery {
        exact: exact != 0,
        name: unsafe { cstr_to_str(name) }.unwrap_or("").to_string(),
        min_self: min_self as usize,
        type_filter: None,
        limit: 0,
    };
    match inst
        .snapshot
        .owner_groups(&query, max_depth as usize, top as usize)
    {
        Ok(analysis) => {
            let json = serde_json::to_string(&analysis).unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_profile_destroy(handle: *mut c_void) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle as *mut HprofProfile));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_timeline_open(path: *const c_char) -> *mut HprofResult {
    let path_str = match unsafe { cstr_to_str(path) } {
        Some(s) => s,
        None => return HprofResult::err("path is null"),
    };
    let timeline = HeapTimeline::new(path_str.to_string());
    let instance = Box::new(HprofTimeline { timeline });
    HprofResult::ok_handle(Box::into_raw(instance) as *mut c_void)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_timeline_meta(handle: *mut c_void) -> *mut HprofResult {
    let inst = match unsafe { as_timeline(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst.timeline.meta() {
        Ok(meta) => {
            let json = serde_json::to_string(&serde_json::json!({
                "node_count": meta.node_count,
                "edge_count": meta.edge_count,
                "extra_native_bytes": meta.extra_native_bytes,
                "meta": {
                    "node_fields": meta.meta.node_fields,
                    "node_types": meta.meta.node_types,
                    "edge_fields": meta.meta.edge_fields,
                    "edge_types": meta.meta.edge_types,
                },
            }))
            .unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_timeline_summary(
    handle: *mut c_void,
    top: u32,
    filter: *const c_char,
) -> *mut HprofResult {
    let inst = match unsafe { as_timeline(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let filter_str = unsafe { cstr_to_str(filter) };
    match inst
        .timeline
        .stream_summary(if top == 0 { None } else { Some(top as usize) }, filter_str)
    {
        Ok(summary) => {
            let json = serde_json::to_string(&serde_json::json!({
                "totalAllocated": summary.total_allocated,
                "totalFreed": summary.total_freed,
                "byType": summary.by_type,
            }))
            .unwrap_or_default();
            HprofResult::ok_string(&json)
        }
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_timeline_top_names(
    handle: *mut c_void,
    top: u32,
    filter: *const c_char,
) -> *mut HprofResult {
    let inst = match unsafe { as_timeline(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let filter_str = unsafe { cstr_to_str(filter) };
    match inst
        .timeline
        .top_names(if top == 0 { None } else { Some(top as usize) }, filter_str)
    {
        Ok(res) => HprofResult::ok_string(&serde_json::to_string(&res).unwrap_or_default()),
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_timeline_top_stacks(
    handle: *mut c_void,
    top: u32,
    filter: *const c_char,
) -> *mut HprofResult {
    let inst = match unsafe { as_timeline(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let filter_str = unsafe { cstr_to_str(filter) };
    match inst
        .timeline
        .top_stacks(if top == 0 { None } else { Some(top as usize) }, filter_str)
    {
        Ok(res) => HprofResult::ok_string(&serde_json::to_string(&res).unwrap_or_default()),
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_timeline_name_stacks(
    handle: *mut c_void,
    name_re: *const c_char,
    top: u32,
) -> *mut HprofResult {
    let inst = match unsafe { as_timeline(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let name_str = match unsafe { cstr_to_str(name_re) } {
        Some(s) => s,
        None => return HprofResult::err("name filter is null"),
    };
    match inst
        .timeline
        .name_stacks(name_str, if top == 0 { None } else { Some(top as usize) })
    {
        Ok(res) => HprofResult::ok_string(&serde_json::to_string(&res).unwrap_or_default()),
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_timeline_growth(handle: *mut c_void) -> *mut HprofResult {
    let inst = match unsafe { as_timeline(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    match inst.timeline.growth() {
        Ok(res) => HprofResult::ok_string(&serde_json::to_string(&res).unwrap_or_default()),
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_timeline_search(
    handle: *mut c_void,
    query: *const c_char,
) -> *mut HprofResult {
    let inst = match unsafe { as_timeline(handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let q = unsafe { cstr_to_str(query) }.unwrap_or("");
    match inst.timeline.search_strings(q) {
        Ok(res) => HprofResult::ok_string(&serde_json::to_string(&res).unwrap_or_default()),
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_timeline_destroy(handle: *mut c_void) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle as *mut HprofTimeline));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_detect_type(path: *const c_char) -> *mut HprofResult {
    let path_str = match unsafe { cstr_to_str(path) } {
        Some(s) => s,
        None => return HprofResult::err("path is null"),
    };
    match detect_profile_type(path_str) {
        Ok(t) => HprofResult::ok_string(t),
        Err(e) => HprofResult::err(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_format_bytes(bytes: u64) -> *mut HprofResult {
    HprofResult::ok_string(&format_bytes(bytes as usize))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_free_result(result: *mut HprofResult) {
    if result.is_null() {
        return;
    }
    unsafe {
        let r = Box::from_raw(result);
        if !r.error.is_null() {
            drop(CString::from_raw(r.error));
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hprof_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            drop(CString::from_raw(s));
        }
    }
}

unsafe fn as_snapshot<'a>(
    handle: *mut c_void,
) -> std::result::Result<&'a mut HprofSnapshot, *mut HprofResult> {
    if handle.is_null() {
        Err(HprofResult::err("handle is null"))
    } else {
        Ok(unsafe { &mut *(handle as *mut HprofSnapshot) })
    }
}

unsafe fn as_profile<'a>(
    handle: *mut c_void,
) -> std::result::Result<&'a mut HprofProfile, *mut HprofResult> {
    if handle.is_null() {
        Err(HprofResult::err("handle is null"))
    } else {
        Ok(unsafe { &mut *(handle as *mut HprofProfile) })
    }
}

unsafe fn as_timeline<'a>(
    handle: *mut c_void,
) -> std::result::Result<&'a mut HprofTimeline, *mut HprofResult> {
    if handle.is_null() {
        Err(HprofResult::err("handle is null"))
    } else {
        Ok(unsafe { &mut *(handle as *mut HprofTimeline) })
    }
}
