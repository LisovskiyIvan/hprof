//! `diff` command — compare profiles of the same type.
//!
//! With more than two files the comparison is pairwise:
//!   hprof diff a b c   →  a vs b, then b vs c.

use hprof_core::{DiffEntry, HeapProfile, HeapSnapshot, SnapshotObjectDiff};
use serde_json::json;

use crate::{dim, format_bytes, format_delta, green, print_header, print_table, red, yellow, Args};

pub fn run(args: &Args) -> Result<(), String> {
    if args.files.len() < 2 {
        return Err(
            "diff requires at least two files: <baseline> <profile> [<more>...]".to_string(),
        );
    }
    let mut docs: Vec<serde_json::Value> = Vec::new();
    // pairwise: files[0] vs files[1], files[1] vs files[2], ...
    for pair in args.files.windows(2) {
        let baseline_path = &pair[0];
        let profile_path = &pair[1];

        let base_type =
            hprof_core::detect_profile_type(baseline_path).map_err(|e| e.to_string())?;
        let prof_type = hprof_core::detect_profile_type(profile_path).map_err(|e| e.to_string())?;
        if base_type != prof_type {
            return Err(format!(
                "cannot diff {base_type} with {prof_type} — types must match ({baseline_path} vs {profile_path})"
            ));
        }

        match prof_type {
            "heapprofile" => {
                let d = diff_heapprofile(baseline_path, profile_path)?;
                if args.json {
                    docs.push(json!({
                        "baseline": baseline_path,
                        "profile": profile_path,
                        "type": "heapprofile",
                        "baselineTotal": d.baseline_total,
                        "profileTotal": d.profile_total,
                        "deltaTotal": d.delta_total,
                        "byFrame": d.by_frame,
                        "byUrl": d.by_url,
                        "byFunction": d.by_function,
                    }));
                } else {
                    print_header(
                        &format!("{profile_path} vs {baseline_path}"),
                        Some(&format!(
                            "diff | baseline: {} → profile: {} | delta: {}",
                            format_bytes(d.baseline_total),
                            yellow(&format_bytes(d.profile_total)),
                            format_delta(d.delta_total)
                        )),
                    );
                    print_diff_table("FUNCTION DELTA", &d.by_function, args.top);
                    print_diff_table("FRAME DELTA", &d.by_frame, args.top);
                }
            }
            "heapsnapshot" => {
                let (d, objects) = diff_heapsnapshot(baseline_path, profile_path, args.top)?;
                if args.json {
                    docs.push(json!({
                        "baseline": baseline_path,
                        "profile": profile_path,
                        "type": "heapsnapshot",
                        "baselineTotal": d.baseline_total,
                        "profileTotal": d.profile_total,
                        "deltaTotal": d.delta_total,
                        "byNodeName": d.by_node_name,
                        "byNodeType": d.by_node_type,
                        "objects": objects,
                    }));
                } else {
                    print_header(
                        &format!("{profile_path} vs {baseline_path}"),
                        Some(&format!(
                            "diff | baseline: {} → profile: {} | delta: {}",
                            format_bytes(d.baseline_total),
                            yellow(&format_bytes(d.profile_total)),
                            format_delta(d.delta_total)
                        )),
                    );
                    print_diff_table("NODE NAME DELTA", &d.by_node_name, args.top);
                    print_diff_table("NODE TYPE DELTA", &d.by_node_type, args.top);
                    print_object_diff(&objects);
                }
            }
            _ => {
                return Err(format!(
                    "diff is not implemented for heaptimeline files ({profile_path})"
                ));
            }
        }

        if !args.json && args.files.len() > 2 {
            println!();
        }
    }

    if args.json {
        if docs.len() == 1 {
            println!("{}", serde_json::to_string_pretty(&docs[0]).unwrap());
        } else {
            println!("{}", serde_json::to_string_pretty(&json!(docs)).unwrap());
        }
    }
    Ok(())
}

fn diff_heapprofile(
    baseline_path: &str,
    profile_path: &str,
) -> Result<hprof_core::ProfileDiff, String> {
    let mut baseline = HeapProfile::new(baseline_path.to_string());
    let mut profile = HeapProfile::new(profile_path.to_string());
    profile.diff(&mut baseline).map_err(|e| e.to_string())
}

fn diff_heapsnapshot(
    baseline_path: &str,
    profile_path: &str,
    limit: usize,
) -> Result<(hprof_core::SnapshotDiff, SnapshotObjectDiff), String> {
    let mut baseline = HeapSnapshot::new(baseline_path.to_string());
    let mut profile = HeapSnapshot::new(profile_path.to_string());
    let summary = profile.diff(&mut baseline).map_err(|e| e.to_string())?;
    let objects = profile
        .object_diff(&mut baseline, limit)
        .map_err(|e| e.to_string())?;
    Ok((summary, objects))
}

fn print_diff_table(title: &str, entries: &[DiffEntry], top: usize) {
    if entries.is_empty() {
        return;
    }
    print_header(title, None);
    let rows: Vec<Vec<String>> = entries
        .iter()
        .take(top)
        .map(|e| {
            let pct_cell = match e.delta_pct {
                None => dim("new"),
                Some(p) if p > 0.0 => red(&format!("+{:.1}%", p * 100.0)),
                Some(p) => green(&format!("{:.1}%", p * 100.0)),
            };
            vec![
                dim(&format_bytes(e.baseline_size)),
                format_bytes(e.profile_size),
                format_delta(e.delta),
                pct_cell,
                e.name.clone(),
            ]
        })
        .collect();
    print_table(&["BASELINE", "PROFILE", "DELTA", "%", "NAME"], &rows);
}

fn print_object_diff(diff: &SnapshotObjectDiff) {
    print_header(
        "OBJECT IDENTITY DELTA",
        Some(&format!(
            "matched: {} | new: {} ({}) | deleted: {} ({}) | self delta: {}",
            diff.matched_count,
            diff.new_count,
            format_bytes(diff.new_size),
            diff.deleted_count,
            format_bytes(diff.deleted_size),
            format_delta(diff.delta_size),
        )),
    );

    if !diff.grown_objects.is_empty() {
        print_header("GROWN OBJECTS", None);
        let rows = diff
            .grown_objects
            .iter()
            .map(|item| {
                vec![
                    item.id.to_string(),
                    item.baseline_index.to_string(),
                    item.profile_index.to_string(),
                    format_bytes(item.baseline_size),
                    format_bytes(item.profile_size),
                    red(&format_delta(item.delta)),
                    item.name.clone(),
                ]
            })
            .collect::<Vec<_>>();
        print_table(
            &[
                "ID",
                "BASE IDX",
                "PROFILE IDX",
                "BASELINE",
                "PROFILE",
                "DELTA",
                "NAME",
            ],
            &rows,
        );
    }

    if !diff.new_objects.is_empty() {
        print_header("NEW OBJECTS", None);
        let rows = diff
            .new_objects
            .iter()
            .map(|item| {
                vec![
                    item.id.to_string(),
                    item.index.to_string(),
                    format_bytes(item.self_size),
                    item.type_.clone(),
                    item.name.clone(),
                ]
            })
            .collect::<Vec<_>>();
        print_table(&["ID", "INDEX", "SELF", "TYPE", "NAME"], &rows);
    }

    if !diff.deleted_objects.is_empty() {
        print_header("DELETED OBJECTS", None);
        let rows = diff
            .deleted_objects
            .iter()
            .map(|item| {
                vec![
                    item.id.to_string(),
                    item.index.to_string(),
                    format_bytes(item.self_size),
                    item.type_.clone(),
                    item.name.clone(),
                ]
            })
            .collect::<Vec<_>>();
        print_table(&["ID", "INDEX", "SELF", "TYPE", "NAME"], &rows);
    }
}
