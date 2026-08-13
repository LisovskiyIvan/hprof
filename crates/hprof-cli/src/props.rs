//! `props` command — node properties with resolved values.
//!
//!   hprof props file.heapsnapshot --index 7396246
//!   hprof props file.heapsnapshot --id 12345
//!
//! Primitive values (numbers, strings) are inlined; objects are shown as a
//! reference to the target node. This is how you read e.g. `renderingGroupId`
//! of a GPUParticleSystem without walking value nodes by hand.

use hprof_core::{HeapSnapshot, PropertyValue};
use serde_json::json;

use crate::{
    bold, cyan, dim, format_bytes, green, magenta, print_header, print_table, yellow, Args,
};

/// Resolve `--index <n>` or `--id <n>` to a record index (shared with the
/// `retainers` command).
pub fn resolve_target(file: &str, args: &Args) -> Result<usize, String> {
    match (args.index, args.id) {
        (Some(idx), _) => Ok(idx),
        (None, Some(id)) => {
            let mut snapshot = HeapSnapshot::new(file.to_string());
            snapshot
                .find_by_id(id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no node with id {id} found in this snapshot"))
        }
        (None, None) => Err("requires --index <n> or --id <n>".to_string()),
    }
}

fn render_value(value: &PropertyValue) -> (String, String) {
    match value {
        PropertyValue::Number(v) => ("number".to_string(), v.to_string()),
        PropertyValue::Str(s) => ("string".to_string(), format!("\"{s}\"")),
        PropertyValue::Ref {
            index,
            id,
            node_type,
            name,
        } => (
            "ref".to_string(),
            format!("{name} ({node_type}, index={index}, id={id})"),
        ),
    }
}

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapsnapshot" {
        return Err("props is only supported for .heapsnapshot files".to_string());
    }
    let target = resolve_target(file, args)?;
    let mut snapshot = HeapSnapshot::new(file.to_string());
    let (node, props) = snapshot
        .get_node_properties(target)
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
                "properties": props,
            }))
            .unwrap()
        );
        return Ok(());
    }

    print_header(
        file,
        Some(&format!(
            "node #{} (id {}) | {} | {}",
            bold(&target.to_string()),
            node.id,
            magenta(&node.type_),
            cyan(&node.name)
        )),
    );
    let details: Vec<Vec<String>> = vec![
        vec![dim("index"), target.to_string()],
        vec![dim("id"), node.id.to_string()],
        vec![dim("type"), node.type_.clone()],
        vec![dim("name"), node.name.clone()],
        vec![dim("self size"), green(&format_bytes(node.self_size))],
        vec![dim("edge count"), node.edge_count.to_string()],
    ];
    print_table(&["", ""], &details);
    println!();

    let mut rows: Vec<Vec<String>> = Vec::with_capacity(props.len());
    for p in &props {
        let (kind, value) = render_value(&p.value);
        rows.push(vec![
            p.name.clone(),
            dim(&p.edge_type),
            yellow(&kind),
            value,
        ]);
    }
    print_table(&["PROPERTY", "EDGE TYPE", "KIND", "VALUE"], &rows);
    Ok(())
}
