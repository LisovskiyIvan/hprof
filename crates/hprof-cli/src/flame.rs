//! `flame` command — folded stacks for flamegraph.pl / speedscope.
//!
//! Mirrors chperf's `--flame`: one `a;b;c <value>` line per (aggregated)
//! stack, frames joined with `;`. For .heapprofile the tree is already
//! aggregated; for .heaptimeline identical allocation stacks are merged.

use hprof_core::{HeapProfile, HeapTimeline};

use crate::Args;

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    let out = match type_name {
        "heapprofile" => {
            let mut profile = HeapProfile::new(file.to_string());
            profile.folded_stacks().map_err(|e| e.to_string())?
        }
        "heaptimeline" => {
            let mut timeline = HeapTimeline::new(file.to_string());
            timeline.folded_stacks().map_err(|e| e.to_string())?
        }
        other => {
            return Err(format!(
                "flame is only supported for .heapprofile and .heaptimeline files (got {other})"
            ))
        }
    };

    let mut lines: Vec<&str> = out.lines().collect();
    if let Some(f) = &args.filter {
        let re = regex::Regex::new(f).map_err(|e| format!("invalid --filter regex: {e}"))?;
        lines.retain(|l| re.is_match(l));
    }
    if args.top_explicit {
        lines.truncate(args.top);
    }
    for line in lines {
        println!("{line}");
    }
    Ok(())
}
