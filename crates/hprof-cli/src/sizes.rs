//! `sizes` command — self-size histogram for a heap snapshot.

use hprof_core::HeapSnapshot;
use serde_json::json;

use crate::{format_bytes, print_header, print_table, Args, WorkingNote};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapsnapshot" {
        return Err("sizes is only supported for .heapsnapshot files".to_string());
    }
    let mut snapshot = HeapSnapshot::new(file.to_string());
    let _note = WorkingNote::new("building size histogram…");
    let histogram = snapshot.size_histogram().map_err(|e| e.to_string())?;
    drop(_note);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": file,
                "type": "heapsnapshot",
                "totalCount": histogram.total_count,
                "totalSize": histogram.total_size,
                "buckets": histogram.buckets,
            }))
            .unwrap()
        );
        return Ok(());
    }

    print_header(
        file,
        Some(&format!(
            "self-size histogram | {} nodes | {}",
            histogram.total_count,
            format_bytes(histogram.total_size)
        )),
    );
    let rows = histogram
        .buckets
        .iter()
        .map(|bucket| {
            vec![
                if bucket.min_size == bucket.max_size {
                    format_bytes(bucket.min_size)
                } else {
                    format!(
                        "{} – {}",
                        format_bytes(bucket.min_size),
                        format_bytes(bucket.max_size)
                    )
                },
                bucket.count.to_string(),
                format_bytes(bucket.total_size),
            ]
        })
        .collect::<Vec<_>>();
    print_table(&["SELF SIZE", "COUNT", "TOTAL"], &rows);
    Ok(())
}
