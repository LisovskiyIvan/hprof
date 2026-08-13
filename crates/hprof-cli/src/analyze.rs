//! `analyze` command — summaries for all three profile formats.

use std::collections::HashMap;

use hprof_core::{
    FilterOptions, HeapProfile, HeapSnapshot, HeapTimeline, TimelineNamesResult,
    TimelineStacksResult,
};
use serde_json::json;

use crate::{
    bold, cyan, dim, format_bytes, format_duration, gray, green, magenta, pct, print_header,
    print_table, yellow, Args, WorkingNote,
};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    match type_name {
        "heapprofile" => analyze_heapprofile(file, args),
        "heapsnapshot" => analyze_heapsnapshot(file, args),
        "heaptimeline" => analyze_heaptimeline(file, args),
        other => Err(format!("unsupported profile type: {other}")),
    }
}

// ============================================================================
// heapprofile
// ============================================================================

fn analyze_heapprofile(file: &str, args: &Args) -> Result<(), String> {
    let mut profile = HeapProfile::new(file.to_string());

    let use_cum = args.cum || args.focus.is_some() || args.ignore.is_some();

    if use_cum {
        let summary = profile
            .summarize_cumulative(
                Some(args.top),
                &FilterOptions {
                    focus: args.focus.clone(),
                    ignore: args.ignore.clone(),
                    hide: args.hide.clone(),
                },
            )
            .map_err(|e| e.to_string())?;
        let total = summary.total_size.max(1) as f64;

        if args.json {
            let entry = |(name, e): (String, &hprof_core::SizeEntry)| {
                json!({
                    "name": name,
                    "selfSize": e.self_size,
                    "cumulativeSize": e.cumulative_size,
                    "selfPct": e.self_size as f64 / total * 100.0,
                    "cumulativePct": e.cumulative_size as f64 / total * 100.0,
                    "count": e.count,
                })
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "file": file,
                    "type": "heapprofile",
                    "totalSize": summary.total_size,
                    "byFrame": sorted_entries(&summary.by_frame).into_iter().map(entry).collect::<Vec<_>>(),
                    "byUrl": sorted_entries(&summary.by_url).into_iter().map(entry).collect::<Vec<_>>(),
                    "byFunction": sorted_entries(&summary.by_function).into_iter().map(entry).collect::<Vec<_>>(),
                }))
                .unwrap()
            );
            return Ok(());
        }

        print_header(
            file,
            Some(&format!(
                "heapprofile | total: {} | {} mode",
                yellow(&format_bytes(summary.total_size)),
                bold("cumulative")
            )),
        );

        let frame_rows = sorted_entries(&summary.by_frame);
        print_table(
            &["SELF", "SELF%", "CUM", "CUM%", "FRAME"],
            &frame_rows
                .iter()
                .map(|(name, e)| {
                    vec![
                        if e.self_size > 0 {
                            green(&format_bytes(e.self_size))
                        } else {
                            dim("0 B")
                        },
                        dim(&pct(e.self_size, summary.total_size)),
                        yellow(&format_bytes(e.cumulative_size)),
                        dim(&pct(e.cumulative_size, summary.total_size)),
                        name.clone(),
                    ]
                })
                .collect::<Vec<_>>(),
        );

        let fn_rows = sorted_entries(&summary.by_function);
        print_table(
            &["SELF", "SELF%", "CUM", "CUM%", "FUNCTION"],
            &fn_rows
                .iter()
                .map(|(name, e)| {
                    vec![
                        if e.self_size > 0 {
                            green(&format_bytes(e.self_size))
                        } else {
                            dim("0 B")
                        },
                        dim(&pct(e.self_size, summary.total_size)),
                        yellow(&format_bytes(e.cumulative_size)),
                        dim(&pct(e.cumulative_size, summary.total_size)),
                        magenta(name),
                    ]
                })
                .collect::<Vec<_>>(),
        );
        return Ok(());
    }

    let summary = profile
        .summarize(Some(args.top), args.filter.as_deref())
        .map_err(|e| e.to_string())?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": file,
                "type": "heapprofile",
                "totalSize": summary.total_size,
                "byFrame": size_pairs(&summary.by_frame),
                "byUrl": size_pairs(&summary.by_url),
                "byFunction": size_pairs(&summary.by_function),
            }))
            .unwrap()
        );
        return Ok(());
    }

    print_header(
        file,
        Some(&format!(
            "heapprofile | total sampled: {}",
            yellow(&format_bytes(summary.total_size))
        )),
    );

    let frame_rows = size_pairs(&summary.by_frame);
    print_table(
        &["SIZE", "%", "FRAME"],
        &frame_rows
            .iter()
            .map(|(key, size)| {
                vec![
                    green(&format_bytes(*size)),
                    dim(&pct(*size, summary.total_size)),
                    key.clone(),
                ]
            })
            .collect::<Vec<_>>(),
    );

    let url_rows = size_pairs(&summary.by_url);
    print_table(
        &["SIZE", "%", "URL"],
        &url_rows
            .iter()
            .map(|(key, size)| {
                vec![
                    green(&format_bytes(*size)),
                    dim(&pct(*size, summary.total_size)),
                    gray(key),
                ]
            })
            .collect::<Vec<_>>(),
    );

    let fn_rows = size_pairs(&summary.by_function);
    print_table(
        &["SIZE", "%", "FUNCTION"],
        &fn_rows
            .iter()
            .map(|(key, size)| {
                vec![
                    green(&format_bytes(*size)),
                    dim(&pct(*size, summary.total_size)),
                    magenta(key),
                ]
            })
            .collect::<Vec<_>>(),
    );

    Ok(())
}

fn sorted_entries<T>(map: &HashMap<String, T>) -> Vec<(String, &T)>
where
    T: SizeOf,
{
    let mut v: Vec<(String, &T)> = map.iter().map(|(k, v)| (k.clone(), v)).collect();
    v.sort_by(|a, b| b.1.size_of().cmp(&a.1.size_of()));
    v
}

trait SizeOf {
    fn size_of(&self) -> usize;
}

impl SizeOf for hprof_core::SizeEntry {
    fn size_of(&self) -> usize {
        self.cumulative_size
    }
}

impl SizeOf for usize {
    fn size_of(&self) -> usize {
        *self
    }
}

fn size_pairs(map: &HashMap<String, usize>) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = map.iter().map(|(k, v)| (k.clone(), *v)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

// ============================================================================
// heapsnapshot
// ============================================================================

fn analyze_heapsnapshot(file: &str, args: &Args) -> Result<(), String> {
    let mut snapshot = HeapSnapshot::new(file.to_string());
    let meta = snapshot.meta().map_err(|e| e.to_string())?.clone();
    let summary = snapshot
        .stream_summary(args.top, args.filter.as_deref())
        .map_err(|e| e.to_string())?;

    let retained = if args.retained {
        let _note = WorkingNote::new("computing retained sizes…");
        Some(
            snapshot
                .retained_summary(args.top, args.filter.as_deref())
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };

    if args.json {
        let mut obj = json!({
            "file": file,
            "type": "heapsnapshot",
            "nodeCount": meta.node_count,
            "edgeCount": meta.edge_count,
            "extraNativeBytes": meta.extra_native_bytes.unwrap_or(0),
            "totalSize": summary.total_size,
            "totalCount": summary.total_count,
            "byNodeName": sorted_names(&summary.by_node_name).into_iter().map(|(name, size, count)| json!({"name": name, "size": size, "count": count})).collect::<Vec<_>>(),
            "byNodeType": sorted_names(&summary.by_node_type).into_iter().map(|(name, size, count)| json!({"type": name, "size": size, "count": count})).collect::<Vec<_>>(),
        });
        if let Some(r) = &retained {
            obj["retainedByNodeName"] = json!(sorted_names(&r.by_node_name)
                .into_iter()
                .map(|(name, size, count)| json!({"name": name, "size": size, "count": count}))
                .collect::<Vec<_>>());
            obj["retainedByNodeType"] = json!(sorted_names(&r.by_node_type)
                .into_iter()
                .map(|(name, size, count)| json!({"type": name, "size": size, "count": count}))
                .collect::<Vec<_>>());
        }
        println!("{}", serde_json::to_string_pretty(&obj).unwrap());
        return Ok(());
    }

    print_header(
        file,
        Some(
            &[
                "heapsnapshot".to_string(),
                format!("nodes: {}", bold(&meta.node_count.to_string())),
                format!("edges: {}", bold(&meta.edge_count.to_string())),
                format!(
                    "total self size: {}",
                    yellow(&format_bytes(summary.total_size))
                ),
            ]
            .join(" | "),
        ),
    );

    if let Some(r) = &retained {
        let mut rows: Vec<(String, usize, usize, usize, usize)> = Vec::new();
        for (name, size, count) in sorted_names(&summary.by_node_name) {
            rows.push((name, size, count, 0, 0));
        }
        for (name, size, count) in sorted_names(&r.by_node_name) {
            if let Some(row) = rows.iter_mut().find(|r| r.0 == name) {
                row.3 = size;
                row.4 = count;
            } else {
                rows.push((name, 0, 0, size, count));
            }
        }
        rows.sort_by(|a, b| b.3.cmp(&a.3));
        print_table(
            &["SELF", "SELF%", "RETAINED", "RET%", "COUNT", "NAME"],
            &rows
                .iter()
                .take(args.top)
                .map(|(name, self_size, self_count, ret_size, ret_count)| {
                    vec![
                        green(&format_bytes(*self_size)),
                        dim(&pct(*self_size, summary.total_size)),
                        yellow(&format_bytes(*ret_size)),
                        dim(&pct(*ret_size, summary.total_size)),
                        dim(&format!("{} / {}", self_count, ret_count)),
                        display_name(name),
                    ]
                })
                .collect::<Vec<_>>(),
        );

        let mut type_rows: Vec<(String, usize, usize, usize, usize)> = Vec::new();
        for (name, size, count) in sorted_names(&summary.by_node_type) {
            type_rows.push((name, size, count, 0, 0));
        }
        for (name, size, count) in sorted_names(&r.by_node_type) {
            if let Some(row) = type_rows.iter_mut().find(|r| r.0 == name) {
                row.3 = size;
                row.4 = count;
            } else {
                type_rows.push((name, 0, 0, size, count));
            }
        }
        type_rows.sort_by(|a, b| b.3.cmp(&a.3));
        print_table(
            &["SELF", "SELF%", "RETAINED", "RET%", "COUNT", "TYPE"],
            &type_rows
                .iter()
                .take(args.top)
                .map(|(name, self_size, self_count, ret_size, ret_count)| {
                    vec![
                        green(&format_bytes(*self_size)),
                        dim(&pct(*self_size, summary.total_size)),
                        yellow(&format_bytes(*ret_size)),
                        dim(&pct(*ret_size, summary.total_size)),
                        dim(&format!("{} / {}", self_count, ret_count)),
                        magenta(name),
                    ]
                })
                .collect::<Vec<_>>(),
        );
        return Ok(());
    }

    let name_rows = sorted_names(&summary.by_node_name);
    print_table(
        &["SIZE", "%", "COUNT", "NAME"],
        &name_rows
            .iter()
            .take(args.top)
            .map(|(name, size, count)| {
                vec![
                    green(&format_bytes(*size)),
                    dim(&pct(*size, summary.total_size)),
                    dim(&count.to_string()),
                    display_name(name),
                ]
            })
            .collect::<Vec<_>>(),
    );

    let type_rows = sorted_names(&summary.by_node_type);
    print_table(
        &["SIZE", "%", "COUNT", "TYPE"],
        &type_rows
            .iter()
            .take(args.top)
            .map(|(name, size, count)| {
                vec![
                    green(&format_bytes(*size)),
                    dim(&pct(*size, summary.total_size)),
                    dim(&count.to_string()),
                    magenta(name),
                ]
            })
            .collect::<Vec<_>>(),
    );

    Ok(())
}

/// Empty node names are hidden V8 bookkeeping nodes (array element backing
/// stores, anonymous closures); render them so the bucket is not invisible.
pub fn display_name(name: &str) -> String {
    if name.is_empty() {
        "(unnamed)".to_string()
    } else {
        name.to_string()
    }
}

fn sorted_names(map: &HashMap<String, hprof_core::TypeSummary>) -> Vec<(String, usize, usize)> {
    let mut v: Vec<(String, usize, usize)> = map
        .iter()
        .map(|(k, v)| (k.clone(), v.size, v.count))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

// ============================================================================
// heaptimeline
// ============================================================================

fn analyze_heaptimeline(file: &str, args: &Args) -> Result<(), String> {
    let mut timeline = HeapTimeline::new(file.to_string());
    let meta = timeline.meta().map_err(|e| e.to_string())?.clone();

    let summary = timeline
        .stream_summary(Some(args.top), args.filter.as_deref())
        .map_err(|e| e.to_string())?;
    let names = timeline
        .top_names(Some(args.top), args.filter.as_deref())
        .map_err(|e| e.to_string())?;
    let stacks = timeline
        .top_stacks(Some(args.top), args.filter.as_deref())
        .map_err(|e| e.to_string())?;
    let growth = timeline.growth().map_err(|e| e.to_string())?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": file,
                "type": "heaptimeline",
                "nodeCount": meta.node_count,
                "edgeCount": meta.edge_count,
                "totalAllocated": summary.total_allocated,
                "totalFreed": summary.total_freed,
                "byType": sorted_types(&summary.by_type).into_iter().map(|(t, a, f, c)| json!({"type": t, "allocated": a, "freed": f, "count": c})).collect::<Vec<_>>(),
                "names": names_json(&names),
                "stacks": stacks_json(&stacks),
                "growth": {
                    "spanUs": growth.span_us,
                    "objectsStart": growth.objects_start,
                    "objectsEnd": growth.objects_end,
                    "samples": growth.samples,
                },
            }))
            .unwrap()
        );
        return Ok(());
    }

    print_header(
        file,
        Some(
            &[
                "heaptimeline".to_string(),
                format!("nodes: {}", bold(&meta.node_count.to_string())),
                format!(
                    "total allocated: {}",
                    yellow(&format_bytes(summary.total_allocated))
                ),
                format!(
                    "recording: {}",
                    bold(&format_duration(growth.span_us as f64 / 1000.0))
                ),
            ]
            .join(" | "),
        ),
    );

    // ---- growth / time profile ----
    if growth.samples.len() > 1 {
        let rates: Vec<f64> = growth
            .samples
            .windows(2)
            .map(|w| {
                let dt = (w[1][0] - w[0][0]) as f64 / 1e6;
                if dt > 0.0 {
                    (w[1][1] - w[0][1]) as f64 / dt
                } else {
                    0.0
                }
            })
            .collect();
        let max_rate = rates.iter().cloned().fold(1.0f64, f64::max);
        let bar = |rate: f64| {
            format!(
                "{}{}",
                dim("|"),
                cyan(&"#".repeat((rate / max_rate * 24.0) as usize))
            )
        };
        print_header(
            "Objects allocated over time",
            Some(&format!(
                "+{} objects in {}",
                (growth.objects_end - growth.objects_start).to_string(),
                format_duration(growth.span_us as f64 / 1000.0)
            )),
        );
        let time_line: String = rates.iter().map(|&r| bar(r)).collect();
        println!("  {time_line}");
        println!(
            "  {}{} {} (density = objects/s, peaks are game-creation phases)",
            dim("0s"),
            " ".repeat(30),
            dim("end")
        );
    }

    // ---- by type ----
    print_header("By type", None);
    let type_rows = sorted_types(&summary.by_type);
    print_table(
        &["ALLOCATED", "%", "COUNT", "TYPE"],
        &type_rows
            .iter()
            .take(args.top)
            .map(|(t, a, _f, c)| {
                vec![
                    green(&format_bytes(*a)),
                    dim(&pct(*a, summary.total_allocated)),
                    dim(&c.to_string()),
                    magenta(t),
                ]
            })
            .collect::<Vec<_>>(),
    );

    // ---- top names ----
    print_header(
        "Top allocations by name",
        Some(&format!("of {} total", format_bytes(names.total_size))),
    );
    let name_rows: Vec<Vec<String>> = names
        .entries
        .iter()
        .map(|e| {
            let type_str = e
                .types
                .iter()
                .map(|t| format!("{} {}", t.name, pct(t.size, e.size)))
                .collect::<Vec<_>>()
                .join(" · ");
            vec![
                green(&format_bytes(e.size)),
                dim(&pct(e.size, names.total_size)),
                dim(&e.count.to_string()),
                e.name.clone(),
                dim(&if type_str != e.name {
                    type_str
                } else {
                    String::new()
                }),
            ]
        })
        .collect();
    print_table(&["ALLOCATED", "%", "COUNT", "NAME", "BY TYPE"], &name_rows);

    // ---- top stacks ----
    if !stacks.entries.is_empty() {
        print_header(
            "Top allocation sites (stack traces)",
            Some(&format!(
                "{} sites · {} tracked",
                stacks.entries.len(),
                format_bytes(stacks.total_size)
            )),
        );
        let stack_rows: Vec<Vec<String>> = stacks
            .entries
            .iter()
            .map(|e| {
                vec![
                    green(&format_bytes(e.size)),
                    dim(&e.count.to_string()),
                    format_stack(&e.stack),
                ]
            })
            .collect();
        print_table(&["SIZE", "COUNT", "STACK (leaf <- caller)"], &stack_rows);
    }

    Ok(())
}

fn sorted_types(
    map: &HashMap<String, hprof_core::TimelineTypeSummary>,
) -> Vec<(String, usize, usize, usize)> {
    let mut v: Vec<(String, usize, usize, usize)> = map
        .iter()
        .map(|(k, e)| (k.clone(), e.allocated, e.freed, e.count))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

fn names_json(names: &TimelineNamesResult) -> serde_json::Value {
    json!(names
        .entries
        .iter()
        .map(|e| json!({
            "name": e.name,
            "size": e.size,
            "count": e.count,
            "types": e.types.iter().map(|t| json!({"type": t.name, "size": t.size, "count": t.count})).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}

fn stacks_json(stacks: &TimelineStacksResult) -> serde_json::Value {
    json!(stacks
        .entries
        .iter()
        .map(|e| json!({
            "size": e.size,
            "count": e.count,
            "stack": e.stack.iter().map(|f| json!({"name": f.name, "script": f.script, "line": f.line, "column": f.column})).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}

pub fn format_stack(stack: &[hprof_core::TimelineStackFrame]) -> String {
    stack
        .iter()
        .map(|f| f.name.as_str())
        .filter(|n| *n != "(root)" && !n.is_empty())
        .collect::<Vec<_>>()
        .join(" <- ")
}
