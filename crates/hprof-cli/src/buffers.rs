//! `buffers` command — inspect ArrayBuffers, TypedArrays, and WebAssembly Memory.

use hprof_core::HeapSnapshot;
use serde_json::json;

use crate::{
    bold, cyan, dim, format_bytes, green, magenta, print_header, print_table, yellow, Args,
    WorkingNote,
};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapsnapshot" {
        return Err("buffers is only supported for .heapsnapshot files".to_string());
    }

    let mut snapshot = HeapSnapshot::new(file.to_string());
    let _note = WorkingNote::new("scanning binary buffers and backing stores…");
    let analysis = snapshot
        .buffer_analysis(args.top)
        .map_err(|e| e.to_string())?;
    drop(_note);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&json!(analysis)).unwrap());
        return Ok(());
    }

    print_header(
        file,
        Some(&format!(
            "buffers & typed arrays | total self: {} | {} buffers found",
            yellow(&format_bytes(analysis.total_self_size)),
            bold(&analysis.total_count.to_string())
        )),
    );

    let rows: Vec<Vec<String>> = analysis
        .entries
        .iter()
        .map(|b| {
            vec![
                b.node_index.to_string(),
                b.node_id.to_string(),
                cyan(&b.name),
                magenta(&b.buffer_type),
                green(&format_bytes(b.self_size)),
                dim(&b.owner_name),
            ]
        })
        .collect();

    print_table(
        &["INDEX", "ID", "BUFFER NAME", "TYPE", "SELF SIZE", "OWNER (RETAINER)"],
        &rows,
    );

    println!();
    println!("  {}: use `hprof retainers --id <id> --depth 8` to inspect full retainer chains.", dim("tip"));
    println!();

    Ok(())
}
