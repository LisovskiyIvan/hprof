//! `smaps` command — source-map data share in heap snapshots.
//!
//! Dev builds keep big source maps in the heap (raw map JSON, inline
//! `sourceMappingURL=data:` bundles, `//# sourceMappingURL=` comments, plus
//! the consumer library objects). That share is a build artifact, not app
//! memory — exclude it before reasoning about leaks.

use hprof_core::HeapSnapshot;
use serde_json::json;

use crate::{cyan, dim, format_bytes, pct, print_header, print_table, yellow, Args, WorkingNote};

pub fn run(file: &str, type_name: &str, args: &Args) -> Result<(), String> {
    if type_name != "heapsnapshot" {
        return Err("smaps is only supported for .heapsnapshot files".to_string());
    }
    let mut snapshot = HeapSnapshot::new(file.to_string());
    let _note = WorkingNote::new("measuring source-map footprint…");
    let fp = snapshot
        .sourcemap_footprint(args.top)
        .map_err(|e| e.to_string())?;
    drop(_note);

    if args.json {
        let mut v = serde_json::to_value(&fp).map_err(|e| e.to_string())?;
        v["file"] = json!(file);
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return Ok(());
    }

    print_header(
        file,
        Some(&format!(
            "sourcemaps | total: {} ({:.1}% of heap) | heap: {}",
            yellow(&format_bytes(fp.bytes)),
            fp.share_pct,
            format_bytes(fp.heap_total_size)
        )),
    );

    if fp.bytes == 0 {
        println!("  {}", dim("no source-map data found in this snapshot"));
        println!();
        return Ok(());
    }

    print_table(
        &["BYTES", "%", "COUNT", "CATEGORY"],
        &[
            (
                fp.map_json_bytes,
                fp.map_json_count,
                "mapJson (raw map JSON)",
            ),
            (
                fp.inline_uri_bytes,
                fp.inline_uri_count,
                "inlineUri (sourceMappingURL=data:)",
            ),
            (
                fp.comment_bytes,
                fp.comment_count,
                "comment (sourceMappingURL=)",
            ),
        ]
        .iter()
        .map(|(bytes, count, cat)| {
            vec![
                yellow(&format_bytes(*bytes)),
                dim(&pct(*bytes, fp.heap_total_size)),
                dim(&count.to_string()),
                cat.to_string(),
            ]
        })
        .collect::<Vec<_>>(),
    );

    if !fp.lib_objects.is_empty() {
        print_table(
            &["COUNT", "SELF", "NAME"],
            &fp.lib_objects
                .iter()
                .map(|e| {
                    vec![
                        dim(&e.count.to_string()),
                        yellow(&format_bytes(e.self_size)),
                        cyan(&e.name),
                    ]
                })
                .collect::<Vec<_>>(),
        );
    }

    print_table(
        &["SIZE", "COUNT", "CATEGORY", "PREVIEW"],
        &fp.top_strings
            .iter()
            .map(|e| {
                vec![
                    yellow(&format_bytes(e.size)),
                    dim(&e.count.to_string()),
                    dim(&e.category),
                    cyan(&e.preview),
                ]
            })
            .collect::<Vec<_>>(),
    );

    if fp.share_pct >= 1.0 {
        println!(
            "  {}",
            dim("dev/build artifact — exclude from app memory analysis")
        );
    }
    Ok(())
}
