//! `suspects` command — automated leak suspects and memory bloat report.

use hprof_core::HeapSnapshot;
use serde_json::json;

use crate::{
    bold, cyan, dim, format_bytes, green, magenta, print_header, print_table, red, yellow, Args,
    WorkingNote,
};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapsnapshot" {
        return Err("suspects is only supported for .heapsnapshot files".to_string());
    }

    let mut snapshot = HeapSnapshot::new(file.to_string());
    let _note = WorkingNote::new("analyzing heap for leak suspects…");
    let report = snapshot
        .leak_suspects(args.top)
        .map_err(|e| e.to_string())?;
    drop(_note);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&json!(report)).unwrap());
        return Ok(());
    }

    print_header(
        file,
        Some(&format!(
            "leak suspects report | heap: {} | {} nodes",
            yellow(&format_bytes(report.total_heap_size)),
            bold(&report.total_nodes.to_string())
        )),
    );

    if report.suspects.is_empty() {
        println!("  {} no prominent leak suspects detected.", green("✓"));
        println!();
        return Ok(());
    }

    let rows: Vec<Vec<String>> = report
        .suspects
        .iter()
        .map(|s| {
            let id_str = s
                .node_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string());
            let pct_str = format!("{:.1}%", s.pct_of_heap);
            let colored_pct = if s.pct_of_heap >= 10.0 {
                red(&pct_str)
            } else if s.pct_of_heap >= 5.0 {
                yellow(&pct_str)
            } else {
                dim(&pct_str)
            };

            vec![
                magenta(&s.category),
                cyan(&s.node_name),
                id_str,
                yellow(&format_bytes(s.size_bytes)),
                colored_pct,
                s.recommendation.clone(),
            ]
        })
        .collect();

    print_table(
        &["CATEGORY", "NAME / TARGET", "ID", "SIZE", "HEAP %", "ACTION / RECOMMENDATION"],
        &rows,
    );

    println!();
    println!("  {}: use `hprof inspect --id <id>` to view retention paths.", dim("tip"));
    println!("  {}: use `hprof props --id <id>` to inspect fields and values.", dim("tip"));
    println!();

    Ok(())
}
