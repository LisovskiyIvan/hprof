//! `dot` command — emit a call graph as DOT for graphviz.

use hprof_core::{FilterOptions, HeapProfile, HeapSnapshot};

use crate::{red, Args};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name == "heapsnapshot" {
        let mut snapshot = HeapSnapshot::new(file.to_string());
        let root = if let Some(index) = args.index {
            index
        } else if let Some(id) = args.id {
            snapshot
                .find_by_id(id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no node with id {id} found in this snapshot"))?
        } else {
            return Err("snapshot dot requires --index <n> or --id <n>".to_string());
        };
        let dot = snapshot
            .to_dot_subgraph(root, args.depth.unwrap_or(2), args.top)
            .map_err(|e| e.to_string())?;
        print!("{dot}");
        return Ok(());
    }
    if type_name != "heapprofile" {
        return Err(format!(
            "{} dot output is currently only supported for .heapprofile files",
            red("Error:")
        ));
    }
    let mut profile = HeapProfile::new(file.to_string());
    let dot = profile
        .to_dot(
            Some(args.top),
            &FilterOptions {
                focus: args.focus.clone(),
                ignore: args.ignore.clone(),
                hide: args.hide.clone(),
            },
        )
        .map_err(|e| e.to_string())?;
    print!("{dot}");
    Ok(())
}
