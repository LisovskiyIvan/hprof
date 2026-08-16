//! `edges` command — search object properties and V8 edge names.

use hprof_core::{EdgeQuery, HeapSnapshot};
use serde_json::json;

use crate::{cyan, magenta, print_header, print_table, Args, WorkingNote};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapsnapshot" {
        return Err("edges is only supported for .heapsnapshot files".to_string());
    }
    let name = args
        .name
        .as_deref()
        .ok_or("edges requires --name <property-or-edge-name>")?;
    let mut snapshot = HeapSnapshot::new(file.to_string());
    let _note = WorkingNote::new("searching edge names…");
    let matches = snapshot
        .find_edges(&EdgeQuery {
            name: name.to_string(),
            exact: args.exact,
            type_filter: args.node_type.clone(),
            edge_type: args.edge_type.clone(),
            limit: args.top,
        })
        .map_err(|e| e.to_string())?;
    drop(_note);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": file,
                "type": "heapsnapshot",
                "query": name,
                "exact": args.exact,
                "matches": matches,
            }))
            .unwrap()
        );
        return Ok(());
    }

    print_header(
        file,
        Some(&format!(
            "edge name match: {} | {} result(s)",
            name,
            matches.len()
        )),
    );
    let rows = matches
        .iter()
        .map(|item| {
            vec![
                item.source_index.to_string(),
                item.source_id.to_string(),
                magenta(&item.source_type),
                item.source_name.clone(),
                item.edge_type.clone(),
                cyan(&item.name),
                item.target_index.to_string(),
                item.target_id.to_string(),
                item.target_type.clone(),
                item.target_name.clone(),
            ]
        })
        .collect::<Vec<_>>();
    print_table(
        &[
            "SOURCE IDX",
            "SOURCE ID",
            "SOURCE TYPE",
            "SOURCE",
            "EDGE TYPE",
            "EDGE",
            "TARGET IDX",
            "TARGET ID",
            "TARGET TYPE",
            "TARGET",
        ],
        &rows,
    );
    Ok(())
}
