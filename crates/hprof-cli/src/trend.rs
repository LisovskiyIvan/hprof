//! `trend` command — adjacent object-growth reports across snapshots.

use hprof_core::HeapSnapshot;
use serde_json::json;

use crate::{format_bytes, format_delta, print_header, print_table, red, Args};

pub fn run(args: &Args) -> Result<(), String> {
    if args.files.len() < 2 {
        return Err("trend requires at least two .heapsnapshot files".to_string());
    }
    let mut documents = Vec::new();
    for pair in args.files.windows(2) {
        let baseline = &pair[0];
        let profile = &pair[1];
        let base_type = hprof_core::detect_profile_type(baseline).map_err(|e| e.to_string())?;
        let profile_type = hprof_core::detect_profile_type(profile).map_err(|e| e.to_string())?;
        if base_type != "heapsnapshot" || profile_type != "heapsnapshot" {
            return Err("trend only supports .heapsnapshot files".to_string());
        }
        let mut base_snapshot = HeapSnapshot::new(baseline.clone());
        let mut profile_snapshot = HeapSnapshot::new(profile.clone());
        let diff = profile_snapshot
            .object_diff(&mut base_snapshot, args.top)
            .map_err(|e| e.to_string())?;
        if args.json {
            documents.push(json!({
                "baseline": baseline,
                "profile": profile,
                "matchedCount": diff.matched_count,
                "newCount": diff.new_count,
                "deletedCount": diff.deleted_count,
                "newSize": diff.new_size,
                "deletedSize": diff.deleted_size,
                "deltaSize": diff.delta_size,
                "grownObjects": diff.grown_objects,
            }));
        } else {
            print_header(
                &format!("{} → {}", baseline, profile),
                Some(&format!(
                    "matched: {} | new: {} ({}) | deleted: {} ({}) | delta: {}",
                    diff.matched_count,
                    diff.new_count,
                    format_bytes(diff.new_size),
                    diff.deleted_count,
                    format_bytes(diff.deleted_size),
                    format_delta(diff.delta_size),
                )),
            );
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
            if rows.is_empty() {
                println!("  no growing objects in this interval");
            } else {
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
            if args.files.len() > 2 {
                println!();
            }
        }
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&documents).unwrap());
    }
    Ok(())
}
