//! `top` command — largest individual heap nodes by retained size.

use hprof_core::HeapSnapshot;
use serde_json::json;

use crate::{
    bold, cyan, dim, format_bytes, green, magenta, print_header, print_table, yellow, Args,
    WorkingNote,
};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapsnapshot" {
        return Err("top is only supported for .heapsnapshot files".to_string());
    }

    let mut snapshot = HeapSnapshot::new(file.to_string());
    let _note = WorkingNote::new(if args.exact {
        "computing exact retained sizes…"
    } else {
        "finding largest retained nodes…"
    });
    let result = if args.exact {
        snapshot
            .get_retained_entries_exact(args.top)
            .map_err(|e| e.to_string())?
    } else {
        snapshot
            .get_retained_entries(args.top)
            .map_err(|e| e.to_string())?
    };
    drop(_note);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": file,
                "type": "heapsnapshot",
                "approximate": result.approximate,
                "entries": result.retained,
            }))
            .unwrap()
        );
        return Ok(());
    }

    let mode = if result.approximate {
        "approximate (self-size; use --exact for dominators)"
    } else {
        "exact"
    };
    print_header(
        file,
        Some(&format!(
            "largest nodes by retained size | {} | {} entries",
            bold(mode),
            result.retained.len()
        )),
    );
    let rows = result
        .retained
        .iter()
        .map(|entry| {
            vec![
                entry.node_index.to_string(),
                entry.name.clone(),
                magenta(&entry.type_),
                entry.self_size.to_string(),
                green(&format_bytes(entry.self_size)),
                yellow(&format_bytes(entry.retained_size)),
                cyan(&entry.approximate.to_string()),
            ]
        })
        .collect::<Vec<_>>();
    print_table(
        &[
            "INDEX", "NAME", "TYPE", "SELF B", "SELF", "RETAINED", "APPROX",
        ],
        &rows,
    );
    if result.approximate {
        println!(
            "  {} exact mode builds the dominator graph and may require substantial RAM",
            dim("tip:")
        );
    }
    Ok(())
}
