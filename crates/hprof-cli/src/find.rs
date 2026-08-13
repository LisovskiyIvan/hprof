//! `find` command — name lookup over heap snapshot nodes.
//!
//!   hprof find file.heapsnapshot --name RenderingGroup --exact
//!   hprof find file.heapsnapshot --name '(object elements)' --exact --min-self 1048576
//!
//! Unlike `inspect --name` this needs no dominator analysis: it is a plain
//! scan and returns matches even when their retained size is 0.

use hprof_core::{HeapSnapshot, NameQuery};
use serde_json::json;

use crate::{bold, dim, format_bytes, green, print_header, print_table, Args};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapsnapshot" {
        return Err("find is only supported for .heapsnapshot files".to_string());
    }
    let name = args.name.as_ref().ok_or("find requires --name <name>")?;
    let query = NameQuery {
        exact: args.exact,
        name: name.clone(),
        min_self: args.min_self,
        type_filter: args.node_type.clone(),
        limit: args.top, // 0 = unlimited
    };

    let mut snapshot = HeapSnapshot::new(file.to_string());
    let meta = snapshot.meta().map_err(|e| e.to_string())?.clone();
    let matches = snapshot.find_nodes(&query).map_err(|e| e.to_string())?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": file,
                "type": "heapsnapshot",
                "query": {
                    "name": name,
                    "exact": args.exact,
                    "minSelf": args.min_self,
                    "nodeType": args.node_type,
                },
                "total": matches.len(),
                "matches": matches,
            }))
            .unwrap()
        );
        return Ok(());
    }

    print_header(
        file,
        Some(&format!(
            "heapsnapshot | nodes: {} | name match: {} ({})",
            bold(&meta.node_count.to_string()),
            bold(name),
            if args.exact { "exact" } else { "substring" }
        )),
    );
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(matches.len());
    for m in &matches {
        rows.push(vec![
            m.node_index.to_string(),
            m.id.to_string(),
            green(&format_bytes(m.self_size)),
            m.type_.clone(),
            m.edge_count.to_string(),
            crate::analyze::display_name(&m.name),
        ]);
    }
    print_table(&["INDEX", "ID", "SELF", "TYPE", "EDGES", "NAME"], &rows);
    if args.top > 0 && matches.len() >= args.top {
        println!();
        println!(
            "  {} only the top {} shown — raise --top or pass --top 0 for all",
            dim("note:"),
            args.top
        );
    }
    Ok(())
}
