//! `detached` command — report V8 detached nodes and their owners.

use hprof_core::HeapSnapshot;
use serde_json::json;

use crate::{
    bold, cyan, dim, format_bytes, green, magenta, print_header, print_table, yellow, Args,
    WorkingNote,
};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapsnapshot" {
        return Err("detached is only supported for .heapsnapshot files".to_string());
    }

    let mut snapshot = HeapSnapshot::new(file.to_string());
    let depth = args.depth.unwrap_or(0);
    let _note = WorkingNote::new("scanning detached nodes…");
    let summary = snapshot
        .detached_summary(args.top, depth)
        .map_err(|e| e.to_string())?;
    drop(_note);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": file,
                "type": "heapsnapshot",
                "totalCount": summary.total_count,
                "totalSize": summary.total_size,
                "entries": summary.entries,
            }))
            .unwrap()
        );
        return Ok(());
    }

    print_header(
        file,
        Some(&format!(
            "detached nodes: {} | total self: {} | top {}",
            bold(&summary.total_count.to_string()),
            yellow(&format_bytes(summary.total_size)),
            summary.entries.len()
        )),
    );
    if summary.entries.is_empty() {
        println!("  {} no non-zero detachedness markers found", dim("ok:"));
        return Ok(());
    }

    let rows = summary
        .entries
        .iter()
        .map(|entry| {
            vec![
                entry.node.index.to_string(),
                entry.node.id.to_string(),
                green(&format_bytes(entry.node.self_size)),
                magenta(&entry.node.type_),
                cyan(&entry.node.name),
                entry.detachedness.to_string(),
                entry.owner_chain.clone(),
            ]
        })
        .collect::<Vec<_>>();
    print_table(
        &["INDEX", "ID", "SELF", "TYPE", "NAME", "MARK", "OWNER CHAIN"],
        &rows,
    );
    Ok(())
}
