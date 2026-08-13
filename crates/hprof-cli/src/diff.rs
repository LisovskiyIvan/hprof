//! `diff` command — compare two profiles of the same type.

use hprof_core::{DiffEntry, HeapProfile, HeapSnapshot};
use serde_json::json;

use crate::{dim, format_bytes, format_delta, green, print_header, print_table, red, yellow, Args};

pub fn run(args: &Args) -> Result<(), String> {
    if args.files.len() < 2 {
        return Err("diff requires two files: <baseline> <profile>".to_string());
    }
    let baseline_path = &args.files[0];
    let profile_path = &args.files[1];

    let base_type = hprof_core::detect_profile_type(baseline_path).map_err(|e| e.to_string())?;
    let prof_type = hprof_core::detect_profile_type(profile_path).map_err(|e| e.to_string())?;
    if base_type != prof_type {
        return Err(format!(
            "cannot diff {base_type} with {prof_type} — types must match"
        ));
    }

    match prof_type {
        "heapprofile" => diff_heapprofile(baseline_path, profile_path, args),
        "heapsnapshot" => diff_heapsnapshot(baseline_path, profile_path, args),
        _ => Err("diff is not implemented for heaptimeline files".to_string()),
    }
}

fn diff_heapprofile(baseline_path: &str, profile_path: &str, args: &Args) -> Result<(), String> {
    let mut baseline = HeapProfile::new(baseline_path.to_string());
    let mut profile = HeapProfile::new(profile_path.to_string());
    let d = profile.diff(&mut baseline).map_err(|e| e.to_string())?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "baseline": baseline_path,
                "profile": profile_path,
                "type": "heapprofile",
                "baselineTotal": d.baseline_total,
                "profileTotal": d.profile_total,
                "deltaTotal": d.delta_total,
                "byFrame": d.by_frame,
                "byUrl": d.by_url,
                "byFunction": d.by_function,
            }))
            .unwrap()
        );
        return Ok(());
    }

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
    Ok(())
}

fn diff_heapsnapshot(baseline_path: &str, profile_path: &str, args: &Args) -> Result<(), String> {
    let mut baseline = HeapSnapshot::new(baseline_path.to_string());
    let mut profile = HeapSnapshot::new(profile_path.to_string());
    let d = profile.diff(&mut baseline).map_err(|e| e.to_string())?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "baseline": baseline_path,
                "profile": profile_path,
                "type": "heapsnapshot",
                "baselineTotal": d.baseline_total,
                "profileTotal": d.profile_total,
                "deltaTotal": d.delta_total,
                "byNodeName": d.by_node_name,
                "byNodeType": d.by_node_type,
            }))
            .unwrap()
        );
        return Ok(());
    }

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
    Ok(())
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
