//! `retainers` command — who retains a node, and how.
//!
//!   hprof retainers file.heapsnapshot --index 7396246
//!       Flat list of every incoming edge (all retainers).
//!   hprof retainers file.heapsnapshot --index 7396246 --depth 12
//!       First-parent (owner) chain walked up to 12 hops, target first.

use hprof_core::HeapSnapshot;
use serde_json::json;

use crate::{bold, cyan, dim, format_bytes, green, magenta, print_header, print_table, red, Args};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapsnapshot" {
        return Err("retainers is only supported for .heapsnapshot files".to_string());
    }
    let mut snapshot = HeapSnapshot::new(file.to_string());
    let target = crate::props::resolve_target(&mut snapshot, args)?;
    let node = snapshot.get_node(target).map_err(|e| e.to_string())?;

    if let Some(depth) = args.depth {
        // owner chain mode
        let chain = snapshot
            .retainer_chain(target, depth)
            .map_err(|e| e.to_string())?;
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "file": file,
                    "type": "heapsnapshot",
                    "node": {
                        "index": target,
                        "id": node.id,
                        "name": node.name,
                        "type": node.type_,
                        "selfSize": node.self_size,
                        "edgeCount": node.edge_count,
                    },
                    "depth": depth,
                    "chain": chain,
                }))
                .unwrap()
            );
            return Ok(());
        }
        print_header(
            file,
            Some(&format!(
                "retainer chain (depth {depth}) | node #{} (id {}) | {} | {}",
                bold(&target.to_string()),
                node.id,
                magenta(&node.type_),
                cyan(&node.name)
            )),
        );
        println!(
            "  #{}  {}  self={}  edges={}",
            target,
            crate::analyze::display_name(&node.name),
            green(&format_bytes(node.self_size)),
            node.edge_count
        );
        for (i, hop) in chain.iter().enumerate().skip(1) {
            let indent = "  ".repeat(i);
            let edge = if hop.edge_type.is_empty() {
                String::new()
            } else {
                format!("[{} .{}] ", dim(&hop.edge_type), hop.edge_name)
            };
            let cycle = if hop.cycle {
                red(" (cycle)")
            } else {
                "".to_string()
            };
            println!(
                "{indent}← {edge}#{}  {}  self={}  edges={}{}",
                hop.node_index,
                crate::analyze::display_name(&hop.name),
                green(&format_bytes(hop.self_size)),
                hop.edge_count,
                cycle
            );
        }
        if chain.is_empty() {
            println!("  {}", dim("(no chain — node unreachable or depth 0)"));
        }
        return Ok(());
    }

    // flat mode: all incoming edges
    let retainers = snapshot.get_retainers(target).map_err(|e| e.to_string())?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": file,
                "type": "heapsnapshot",
                "node": {
                    "index": target,
                    "id": node.id,
                    "name": node.name,
                    "type": node.type_,
                    "selfSize": node.self_size,
                    "edgeCount": node.edge_count,
                },
                "total": retainers.len(),
                "retainers": retainers,
            }))
            .unwrap()
        );
        return Ok(());
    }
    print_header(
        file,
        Some(&format!(
            "incoming edges ({} retainer{}) | node #{} (id {}) | {} | {}",
            retainers.len(),
            if retainers.len() == 1 { "" } else { "s" },
            bold(&target.to_string()),
            node.id,
            magenta(&node.type_),
            cyan(&node.name)
        )),
    );
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(retainers.len());
    for r in &retainers {
        let src = snapshot.get_node(r.source).map_err(|e| e.to_string())?;
        rows.push(vec![
            r.source.to_string(),
            crate::analyze::display_name(&src.name),
            r.name.clone(),
            dim(&r.edge_type),
        ]);
    }
    print_table(&["SOURCE IDX", "SOURCE", "EDGE", "TYPE"], &rows);
    if !retainers.is_empty() {
        println!();
        println!(
            "  {} add --depth <n> to walk the first-parent (owner) chain instead",
            dim("tip:")
        );
    }
    Ok(())
}
