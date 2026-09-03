//! `closures` command — inspect closures, execution contexts, and captured variables.

use hprof_core::HeapSnapshot;
use serde_json::json;

use crate::{
    bold, cyan, dim, format_bytes, green, magenta, print_header, print_table, Args, WorkingNote,
};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapsnapshot" {
        return Err("closures is only supported for .heapsnapshot files".to_string());
    }

    let mut snapshot = HeapSnapshot::new(file.to_string());
    let _note = WorkingNote::new("scanning closures and lexical contexts…");
    let analysis = snapshot
        .closure_analysis(args.top)
        .map_err(|e| e.to_string())?;
    drop(_note);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&json!(analysis)).unwrap());
        return Ok(());
    }

    print_header(
        file,
        Some(&format!(
            "closures & lexical contexts | {} closures found",
            bold(&analysis.total_closures.to_string())
        )),
    );

    let rows: Vec<Vec<String>> = analysis
        .entries
        .iter()
        .map(|c| {
            let captured = if c.captured_vars.is_empty() {
                dim("(none)")
            } else {
                dim(&c.captured_vars.join(", "))
            };

            vec![
                c.node_index.to_string(),
                c.node_id.to_string(),
                cyan(&c.name),
                green(&format_bytes(c.self_size)),
                magenta(&c.context_name),
                captured,
            ]
        })
        .collect();

    print_table(
        &["INDEX", "ID", "CLOSURE FUNCTION", "SELF", "RETAINED CONTEXT", "CAPTURED VARIABLES"],
        &rows,
    );

    println!();
    println!("  {}: closures retain their entire parent Context even if only one variable is used.", dim("tip"));
    println!("  {}: use `hprof props --id <context_id>` to view all properties retained by the context.", dim("tip"));
    println!();

    Ok(())
}
