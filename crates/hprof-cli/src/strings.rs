//! `strings` command — referenced string statistics and duplicate contents.

use hprof_core::HeapSnapshot;
use serde_json::json;

use crate::{cyan, dim, format_bytes, print_header, print_table, Args, WorkingNote};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapsnapshot" {
        return Err("strings is only supported for .heapsnapshot files".to_string());
    }
    let mut snapshot = HeapSnapshot::new(file.to_string());
    let _note = WorkingNote::new("collecting string statistics…");
    let stats = snapshot.string_stats(args.top).map_err(|e| e.to_string())?;
    drop(_note);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": file,
                "type": "heapsnapshot",
                "totalStrings": stats.total_strings,
                "totalBytes": stats.total_bytes,
                "referencedStrings": stats.referenced_strings,
                "referencedBytes": stats.referenced_bytes,
                "entries": stats.entries,
            }))
            .unwrap()
        );
        return Ok(());
    }

    print_header(
        file,
        Some(&format!(
            "strings: {} total / {} referenced | referenced bytes: {}",
            stats.total_strings,
            stats.referenced_strings,
            format_bytes(stats.referenced_bytes)
        )),
    );
    let rows = stats
        .entries
        .iter()
        .map(|entry| {
            vec![
                entry.references.to_string(),
                format_bytes(entry.byte_length),
                format_bytes(entry.referenced_bytes),
                cyan(&entry.value),
            ]
        })
        .collect::<Vec<_>>();
    print_table(&["REFERENCES", "LENGTH", "TOTAL", "VALUE"], &rows);
    println!(
        "  {} table bytes: {}",
        dim("info:"),
        format_bytes(stats.total_bytes)
    );
    Ok(())
}
