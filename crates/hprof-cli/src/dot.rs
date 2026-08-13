//! `dot` command — emit a call graph as DOT for graphviz.

use hprof_core::{FilterOptions, HeapProfile};

use crate::{red, Args};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
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
