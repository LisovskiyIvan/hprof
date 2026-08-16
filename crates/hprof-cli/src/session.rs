//! Small persistent heap-snapshot REPL. Keeping one `HeapSnapshot` alive means
//! raw columns, edges, parent maps and dominators are reused across commands.

use std::io::{self, BufRead, Write};

use hprof_core::{HeapSnapshot, NameQuery};

use crate::format_bytes;

pub fn run(file: &str) -> Result<(), String> {
    if hprof_core::detect_profile_type(file).map_err(|e| e.to_string())? != "heapsnapshot" {
        return Err("session currently supports .heapsnapshot files".to_string());
    }
    let mut snapshot = HeapSnapshot::new(file.to_string());
    println!("hprof session: {file}");
    println!("commands: summary | top [n] [--exact] | find <name> | props <index> | retainers <index> | detached | help | quit");
    print!("> ");
    io::stdout().flush().map_err(|e| e.to_string())?;

    for line in io::stdin().lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        let command = line.trim();
        if command.is_empty() {
            print!("> ");
            io::stdout().flush().map_err(|e| e.to_string())?;
            continue;
        }
        if command == "quit" || command == "exit" {
            break;
        }
        let result = execute(&mut snapshot, command);
        if let Err(error) = result {
            eprintln!("error: {error}");
        }
        print!("> ");
        io::stdout().flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn execute(snapshot: &mut HeapSnapshot, command: &str) -> Result<(), String> {
    let mut parts = command.splitn(2, char::is_whitespace);
    let verb = parts.next().unwrap_or_default();
    let rest = parts.next().unwrap_or_default().trim();
    match verb {
        "help" => {
            println!("summary | top [n] [--exact] | find <name> | props <index> | retainers <index> | detached | quit");
        }
        "summary" => {
            let summary = snapshot
                .stream_summary(20, None)
                .map_err(|e| e.to_string())?;
            println!(
                "total: {} in {} nodes",
                format_bytes(summary.total_size),
                summary.total_count
            );
            for (name, info) in sorted_summary(&summary.by_node_name) {
                println!("  {:>12} {:>8} {}", format_bytes(info.0), info.1, name);
            }
        }
        "top" => {
            let tokens = rest.split_whitespace().collect::<Vec<_>>();
            let limit = tokens
                .first()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(20);
            let exact = tokens.contains(&"--exact");
            let result = if exact {
                snapshot
                    .get_retained_entries_exact(limit)
                    .map_err(|e| e.to_string())?
            } else {
                snapshot
                    .get_retained_entries(limit)
                    .map_err(|e| e.to_string())?
            };
            println!(
                "top retained ({})",
                if result.approximate {
                    "approximate"
                } else {
                    "exact"
                }
            );
            for entry in result.retained {
                println!(
                    "  #{:<10} self={:<10} retained={:<10} {} ({})",
                    entry.node_index,
                    format_bytes(entry.self_size),
                    format_bytes(entry.retained_size),
                    entry.name,
                    entry.type_
                );
            }
        }
        "find" => {
            if rest.is_empty() {
                return Err("find requires a name".to_string());
            }
            let matches = snapshot
                .find_nodes(&NameQuery {
                    exact: false,
                    name: rest.to_string(),
                    limit: 20,
                    ..NameQuery::default()
                })
                .map_err(|e| e.to_string())?;
            for item in matches {
                println!(
                    "  #{} id={} self={} {} ({})",
                    item.node_index,
                    item.id,
                    format_bytes(item.self_size),
                    item.name,
                    item.type_
                );
            }
        }
        "props" => {
            let index = parse_index(rest)?;
            let (node, properties) = snapshot
                .get_node_properties(index)
                .map_err(|e| e.to_string())?;
            println!("#{} {} ({})", index, node.name, node.type_);
            for property in properties {
                println!(
                    "  {} [{}] {:?}",
                    property.name, property.edge_type, property.value
                );
            }
        }
        "retainers" => {
            let index = parse_index(rest)?;
            let retainers = snapshot.get_retainers(index).map_err(|e| e.to_string())?;
            for retainer in retainers {
                println!(
                    "  #{} [{}] {}",
                    retainer.source, retainer.edge_type, retainer.name
                );
            }
        }
        "detached" => {
            let summary = snapshot
                .detached_summary(20, 4)
                .map_err(|e| e.to_string())?;
            println!(
                "detached: {} nodes, {}",
                summary.total_count,
                format_bytes(summary.total_size)
            );
            for entry in summary.entries {
                println!(
                    "  #{} {} {} owner={}",
                    entry.node.index,
                    format_bytes(entry.node.self_size),
                    entry.node.name,
                    entry.owner_chain
                );
            }
        }
        _ => return Err(format!("unknown command `{verb}`; type help")),
    }
    Ok(())
}

fn parse_index(value: &str) -> Result<usize, String> {
    value
        .split_whitespace()
        .next()
        .ok_or_else(|| "node index required".to_string())
        .and_then(|value| value.parse().map_err(|_| "invalid node index".to_string()))
}

fn sorted_summary(
    map: &std::collections::HashMap<String, hprof_core::TypeSummary>,
) -> Vec<(String, (usize, usize))> {
    let mut values = map
        .iter()
        .map(|(name, info)| (name.clone(), (info.size, info.count)))
        .collect::<Vec<_>>();
    values.sort_unstable_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(&b.0)));
    values.truncate(20);
    values
}
