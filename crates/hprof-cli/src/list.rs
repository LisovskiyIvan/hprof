//! `list` command — sampled locations grouped by file:line (heapprofile).

use std::collections::{BTreeSet, HashMap};

use hprof_core::HeapProfile;
use serde_json::json;

use crate::{
    bold, cyan, dim, format_bytes, green, magenta, pct, print_header, print_table, yellow, Args,
};

struct LineInfo {
    size: usize,
    count: usize,
    fns: BTreeSet<String>,
}

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapprofile" {
        return Err("list is only supported for .heapprofile files".to_string());
    }

    let mut profile = HeapProfile::new(file.to_string());
    let frames = profile.flatten().map_err(|e| e.to_string())?;

    let filter_re = args
        .filter
        .as_deref()
        .and_then(|f| regex::Regex::new(f).ok());

    let mut by_url: HashMap<String, (usize, HashMap<i32, LineInfo>)> = HashMap::new();
    let mut total = 0usize;
    for frame in &frames {
        if let Some(re) = &filter_re {
            if !re.is_match(&frame.url) && !re.is_match(&frame.function_name) {
                continue;
            }
        }
        let g = by_url
            .entry(frame.url.clone())
            .or_insert_with(|| (0, HashMap::new()));
        g.0 += frame.self_size;
        total += frame.self_size;
        let line = g.1.entry(frame.line_number).or_insert_with(|| LineInfo {
            size: 0,
            count: 0,
            fns: BTreeSet::new(),
        });
        line.size += frame.self_size;
        line.count += 1;
        line.fns.insert(frame.function_name.clone());
    }

    if args.json {
        let mut urls: Vec<(&String, &(usize, HashMap<i32, LineInfo>))> = by_url.iter().collect();
        urls.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
        let payload: Vec<serde_json::Value> = urls
            .iter()
            .take(args.top)
            .map(|(url, g)| {
                let mut lines: Vec<(&i32, &LineInfo)> = g.1.iter().collect();
                lines.sort_by(|a, b| b.1.size.cmp(&a.1.size));
                json!({
                    "url": url,
                    "size": g.0,
                    "lines": lines.iter().map(|(line_number, info)| json!({
                        "lineNumber": *line_number + 1,
                        "size": info.size,
                        "count": info.count,
                        "functions": info.fns.iter().collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": file,
                "type": "heapprofile",
                "totalSize": total,
                "byUrl": payload,
            }))
            .unwrap()
        );
        return Ok(());
    }

    print_header(
        file,
        Some(&format!(
            "heapprofile | total: {} | {} mode",
            yellow(&format_bytes(total)),
            bold("list")
        )),
    );

    let mut sorted_urls: Vec<(&String, &(usize, HashMap<i32, LineInfo>))> = by_url.iter().collect();
    sorted_urls.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    for (url, g) in sorted_urls.iter().take(args.top) {
        println!();
        println!(
            "  {} {}",
            bold(&cyan(url)),
            dim(&format!("({})", pct(g.0, total)))
        );
        let mut sorted_lines: Vec<(&i32, &LineInfo)> = g.1.iter().collect();
        sorted_lines.sort_by(|a, b| b.1.size.cmp(&a.1.size));
        let rows: Vec<Vec<String>> = sorted_lines
            .iter()
            .take(10)
            .map(|(line_number, info)| {
                vec![
                    green(&format_bytes(info.size)),
                    dim(&pct(info.size, total)),
                    dim(&info.count.to_string()),
                    yellow(&format!(":{}", **line_number + 1)),
                    magenta(
                        &info
                            .fns
                            .iter()
                            .take(2)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                ]
            })
            .collect();
        print_table(&["SIZE", "%", "×", "LINE", "FUNCTION"], &rows);
    }

    Ok(())
}
