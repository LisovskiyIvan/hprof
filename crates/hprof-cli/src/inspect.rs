//! `inspect` command — heap snapshot drill-down.
//!
//! Two modes:
//!   hprof inspect file.heapsnapshot --name <re>   top instances by retained size
//!   hprof inspect file.heapsnapshot --id <n>      node details + path from root
//!   hprof inspect file.heapsnapshot --index <n>   same, by record index

use hprof_core::HeapSnapshot;
use serde_json::json;

use crate::{
    bold, cyan, dim, format_bytes, gray, green, magenta, print_header, print_table, yellow, Args,
    WorkingNote,
};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapsnapshot" {
        return Err("inspect is only supported for .heapsnapshot files".to_string());
    }

    let has_name = args.name.is_some();
    let has_target = args.id.is_some() || args.index.is_some();
    if !has_name && !has_target {
        return Err("inspect requires --name <re>, --id <n>, or --index <n>".to_string());
    }

    let mut snapshot = HeapSnapshot::new(file.to_string());
    let meta = snapshot.meta().map_err(|e| e.to_string())?.clone();

    let mut name_entries = None;
    if let Some(name) = &args.name {
        let entries = {
            let _note = WorkingNote::new("computing retained sizes…");
            snapshot
                .search_nodes(name, args.top)
                .map_err(|e| e.to_string())?
        };
        if args.json {
            name_entries = Some(entries);
        } else {
            print_header(
                file,
                Some(&format!(
                    "heapsnapshot | nodes: {} | name match: {}",
                    bold(&meta.node_count.to_string()),
                    bold(name)
                )),
            );
            let mut rows: Vec<Vec<String>> = Vec::with_capacity(entries.len());
            for e in &entries {
                let node = snapshot.get_node(e.node_index).map_err(|e| e.to_string())?;
                rows.push(vec![
                    e.node_index.to_string(),
                    node.id.to_string(),
                    green(&format_bytes(e.self_size)),
                    yellow(&format_bytes(e.retained_size)),
                    e.type_.clone(),
                    crate::analyze::display_name(&e.name),
                ]);
            }
            print_table(&["INDEX", "ID", "SELF", "RETAINED", "TYPE", "NAME"], &rows);
            if !entries.is_empty() {
                println!(
                    "  {} use --index <n> to see a node's retention path",
                    dim("tip:")
                );
                println!();
            }
        }
    }

    let target_index = if let Some(idx) = args.index {
        Some(idx)
    } else if let Some(id) = args.id {
        let _note = WorkingNote::new("resolving node id…");
        match snapshot.find_by_id(id).map_err(|e| e.to_string())? {
            Some(idx) => Some(idx),
            None => {
                return Err(format!("no node with id {id} found in this snapshot"));
            }
        }
    } else {
        None
    };

    if let Some(idx) = target_index {
        let (node, retained, path) = {
            let _note = WorkingNote::new("computing retained sizes…");
            let node = snapshot.get_node(idx).map_err(|e| e.to_string())?;
            let retained = snapshot.retained_size_of(idx).map_err(|e| e.to_string())?;
            let path = snapshot.shortest_path(idx, 64).map_err(|e| e.to_string())?;
            (node, retained, path)
        };

        if args.json {
            let mut obj = json!({
                "file": file,
                "type": "heapsnapshot",
                "node": {
                    "index": idx,
                    "id": node.id,
                    "name": node.name,
                    "type": node.type_,
                    "selfSize": node.self_size,
                    "retainedSize": retained,
                    "edgeCount": node.edge_count,
                },
                "path": {
                    "found": path.found,
                    "nodes": path.nodes,
                    "edges": path.edges,
                },
            });
            if let Some(entries) = name_entries {
                obj["entries"] = json!(entries
                    .iter()
                    .map(|e| {
                        let n = snapshot.get_node(e.node_index).map(|n| n.id).unwrap_or(0);
                        json!({
                            "index": e.node_index,
                            "id": n,
                            "name": e.name,
                            "type": e.type_,
                            "selfSize": e.self_size,
                            "retainedSize": e.retained_size,
                        })
                    })
                    .collect::<Vec<_>>());
            }
            println!("{}", serde_json::to_string_pretty(&obj).unwrap());
            return Ok(());
        }

        print_header(
            file,
            Some(&format!(
                "node #{} (id {}) | {} | {}",
                bold(&idx.to_string()),
                node.id,
                magenta(&node.type_),
                cyan(&node.name)
            )),
        );
        let details: Vec<Vec<String>> = vec![
            vec![dim("index"), idx.to_string()],
            vec![dim("id"), node.id.to_string()],
            vec![dim("type"), node.type_.clone()],
            vec![dim("name"), node.name.clone()],
            vec![dim("self size"), green(&format_bytes(node.self_size))],
            vec![dim("retained"), yellow(&format_bytes(retained))],
            vec![dim("edge count"), node.edge_count.to_string()],
        ];
        print_table(&["", ""], &details);

        render_path(&path);
    } else if args.json {
        // --name only, no target
        let mut obj = json!({
            "file": file,
            "type": "heapsnapshot",
        });
        if let Some(entries) = name_entries {
            obj["entries"] = json!(entries
                .iter()
                .map(|e| {
                    let n = snapshot.get_node(e.node_index).map(|n| n.id).unwrap_or(0);
                    json!({
                        "index": e.node_index,
                        "id": n,
                        "name": e.name,
                        "type": e.type_,
                        "selfSize": e.self_size,
                        "retainedSize": e.retained_size,
                    })
                })
                .collect::<Vec<_>>());
        }
        println!("{}", serde_json::to_string_pretty(&obj).unwrap());
    }

    Ok(())
}

fn render_path(path: &hprof_core::ShortestPath) {
    let subtitle = if path.found {
        format!("{} steps · who keeps this object alive", path.nodes.len())
    } else {
        "object is not reachable from the root (garbage)".to_string()
    };
    print_header(
        if path.found {
            "Path from GC root"
        } else {
            "No path from GC root"
        },
        Some(&subtitle),
    );
    if !path.found {
        return;
    }
    for (i, n) in path.nodes.iter().enumerate() {
        // root nodes carry their identity in the type, not the name
        let label = if n.name.is_empty() {
            format!("({})", n.type_)
        } else {
            truncate(&n.name, 72)
        };
        if i == 0 {
            println!("  • #{} {}", dim(&n.index.to_string()), label);
            continue;
        }
        let e = &path.edges[i - 1];
        let edge = if e.type_.is_empty() && e.name.is_empty() {
            String::new()
        } else {
            format!("[{}:{}]", e.type_, truncate(&e.name, 48))
        };
        let indent = "  ".repeat(i);
        println!(
            "{indent}└─ {} → #{} {}",
            gray(&edge),
            dim(&n.index.to_string()),
            label
        );
    }
    println!();
}

fn truncate(s: &str, width: usize) -> String {
    let mut out: String = s.chars().take(width).collect();
    if s.chars().count() > width {
        out.push('…');
    }
    out
}
