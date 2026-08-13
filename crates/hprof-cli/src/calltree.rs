//! `calltree` command — inclusive call tree for .heapprofile.
//!
//! Mirrors chperf's `--calltree`: an indented tree where every node shows
//! its self and inclusive (self + subtree) sizes, e.g.
//!
//! ```text
//! (root)  self 0 B · incl 4.15 MB (100.0%)
//!   _renderLoop  [chunk-FJ7G77YS.js?v=e3c2406d]  self 0 B · incl 3.95 MB (95.3%)
//! ```
//!
//! `--url <substr>` / `--focus <re>` keep only paths through matching frames
//! (ancestors are shown as pass-through); `--top` caps the rendered nodes.

use std::collections::HashMap;

use hprof_core::{HeapProfile, HeapProfileNode};
use regex::Regex;

use crate::{bold, dim, format_bytes, yellow, Args};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapprofile" {
        return Err("calltree is only supported for .heapprofile files".to_string());
    }
    let mut profile = HeapProfile::new(file.to_string());
    let data = profile.data().map_err(|e| e.to_string())?;

    let url_filter = args
        .url
        .as_deref()
        .filter(|u| !u.is_empty())
        .map(|u| u.to_lowercase());
    let focus_re = args.focus.as_deref().and_then(|f| Regex::new(f).ok());
    let hide_re = args.hide.as_deref().and_then(|f| Regex::new(f).ok());

    // Iterative post-order: per-node (incl, visible) + tree size.
    let mut total_nodes = 0usize;
    let mut info: HashMap<*const HeapProfileNode, (usize, bool)> = HashMap::new();
    let mut post: Vec<(*const HeapProfileNode, bool)> =
        vec![(&data.head as *const HeapProfileNode, false)];
    while let Some((ptr, done)) = post.pop() {
        if !done {
            total_nodes += 1;
            post.push((ptr, true));
            // SAFETY: nodes live in the Arc'd profile data, which outlives us
            let node = unsafe { &*ptr };
            for c in &node.children {
                post.push((c as *const HeapProfileNode, false));
            }
        } else {
            // SAFETY: same
            let node = unsafe { &*ptr };
            let mut incl = node.self_size;
            let mut visible = node_matches(
                node,
                url_filter.as_deref(),
                focus_re.as_ref(),
                hide_re.as_ref(),
            );
            for c in &node.children {
                let (ci, cv) = info[&(c as *const HeapProfileNode)];
                incl += ci;
                visible |= cv;
            }
            info.insert(ptr, (incl, visible));
        }
    }

    let root_incl = info[&(&data.head as *const HeapProfileNode)].0;
    let budget = args.top;
    let mut rendered = 0usize;

    println!();
    println!(
        "  {}",
        bold(&format!(
            "Inclusive call tree · {} nodes · total {}",
            total_nodes,
            yellow(&format_bytes(root_incl))
        ))
    );
    println!(
        "  {}",
        dim("self + subtree per frame · --url/--focus to prune · --top to show more")
    );
    println!();

    render(
        &data.head,
        &info,
        url_filter.as_deref(),
        focus_re.as_ref(),
        hide_re.as_ref(),
        0,
        budget,
        &mut rendered,
        root_incl,
    );

    if rendered >= budget {
        println!();
        println!(
            "  {} showing {} of {} nodes (use --top to see more)",
            dim("_"),
            rendered,
            total_nodes
        );
    }
    println!();
    Ok(())
}

fn node_matches(
    node: &HeapProfileNode,
    url: Option<&str>,
    focus: Option<&Regex>,
    hide: Option<&Regex>,
) -> bool {
    if hide.is_some_and(|re| re.is_match(&node.call_frame.function_name)) {
        return false;
    }
    // no positive filters → everything is visible; with filters, a node is
    // visible only when it (or a descendant) matches
    if focus.is_none() && url.is_none() {
        return true;
    }
    if focus.is_some_and(|re| re.is_match(&frame_label(node))) {
        return true;
    }
    url.is_some_and(|u| node.call_frame.url.to_lowercase().contains(u))
}

fn render(
    node: &HeapProfileNode,
    info: &HashMap<*const HeapProfileNode, (usize, bool)>,
    url: Option<&str>,
    focus: Option<&Regex>,
    hide: Option<&Regex>,
    depth: usize,
    budget: usize,
    rendered: &mut usize,
    root_incl: usize,
) {
    let (incl, visible) = info[&(node as *const HeapProfileNode)];
    if !visible {
        return;
    }

    // a hidden frame still renders its visible children (reattached)
    let hidden = hide.is_some_and(|re| re.is_match(&node.call_frame.function_name));
    if !hidden {
        *rendered += 1;
        if *rendered > budget {
            return;
        }
        let name = if node.call_frame.function_name.is_empty() {
            "(anonymous)".to_string()
        } else {
            node.call_frame.function_name.clone()
        };
        let pct = if root_incl > 0 {
            (incl as f64 / root_incl as f64) * 100.0
        } else {
            0.0
        };
        println!(
            "{}{}  {}  self {} · incl {} ({:.1}%)",
            "  ".repeat(depth),
            name,
            dim(&format!("[{}]", basename(&node.call_frame.url))),
            if node.self_size > 0 {
                format_bytes(node.self_size)
            } else {
                dim("0 B")
            },
            yellow(&format_bytes(incl)),
            pct
        );
    }

    let mut children: Vec<(&HeapProfileNode, usize)> = node
        .children
        .iter()
        .map(|c| (c, info[&(c as *const HeapProfileNode)].0))
        .collect();
    children.sort_by(|a, b| b.1.cmp(&a.1));
    for (child, _) in children {
        if *rendered >= budget {
            return;
        }
        render(
            child,
            info,
            url,
            focus,
            hide,
            depth + 1,
            budget,
            rendered,
            root_incl,
        );
    }
}

fn frame_label(node: &HeapProfileNode) -> String {
    let fn_name = if node.call_frame.function_name.is_empty() {
        "(anonymous)".to_string()
    } else {
        node.call_frame.function_name.clone()
    };
    format!(
        "{} @ {}:{}",
        fn_name,
        node.call_frame.url,
        node.call_frame.line_number + 1
    )
}

/// Last path segment of a script URL (keeps the ?query), matching chperf's
/// `[file.js?v=...]` convention.
fn basename(url: &str) -> String {
    if url.is_empty() {
        return "<no-url>".to_string();
    }
    url.rsplit('/').next().unwrap_or(url).to_string()
}
