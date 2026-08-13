//! `owners` command — classify nodes by their owner (first-parent) chain.
//!
//!   hprof owners file.heapsnapshot --name '(object elements)' --exact --min-self 1048576
//!   hprof owners a.heapsnapshot b.heapsnapshot c.heapsnapshot --name RenderingGroup --exact
//!
//! Each matched node is walked up its first-parent chain (`--depth` hops) and
//! grouped by the resulting "owner -> parent -> ..." chain, summed by self
//! size. With several files, pairwise diffs of the owner groups are printed.

use hprof_core::{HeapSnapshot, NameQuery, OwnerAnalysis};
use serde_json::json;

use crate::{bold, format_bytes, green, print_header, print_table, red, Args};

const MIN_DELTA: usize = 10 * 1024;

fn analyze_file(path: &str, name: &str, args: &Args) -> Result<OwnerAnalysis, String> {
    let mut snapshot = HeapSnapshot::new(path.to_string());
    let query = NameQuery {
        exact: args.exact,
        name: name.to_string(),
        min_self: args.min_self,
        type_filter: None,
        limit: 0,
    };
    snapshot
        .owner_groups(&query, args.depth.unwrap_or(8), args.top)
        .map_err(|e| e.to_string())
}

fn print_groups(file: &str, a: &OwnerAnalysis) {
    print_header(
        file,
        Some(&format!(
            "owners | name: {} | {} nodes, {} total",
            bold(&a.name),
            bold(&a.total_nodes.to_string()),
            format_bytes(a.total_self)
        )),
    );
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(a.groups.len());
    for g in &a.groups {
        rows.push(vec![
            green(&format_bytes(g.self_size)),
            g.count.to_string(),
            g.chain.clone(),
        ]);
    }
    print_table(&["SELF", "COUNT", "OWNER CHAIN"], &rows);
}

fn print_pair_diff(baseline_path: &str, profile_path: &str, b: &OwnerAnalysis, p: &OwnerAnalysis) {
    let mut keys: Vec<&String> = b
        .groups
        .iter()
        .map(|g| &g.chain)
        .chain(p.groups.iter().map(|g| &g.chain))
        .collect();
    keys.sort();
    keys.dedup();

    let mut rows: Vec<Vec<String>> = Vec::new();
    for k in &keys {
        let bg = b.groups.iter().find(|g| &g.chain == *k);
        let pg = p.groups.iter().find(|g| &g.chain == *k);
        let bself = bg.map(|g| g.self_size).unwrap_or(0);
        let pself = pg.map(|g| g.self_size).unwrap_or(0);
        let dself = pself as i64 - bself as i64;
        if dself.unsigned_abs() < MIN_DELTA as u64 {
            continue;
        }
        let bcount = bg.map(|g| g.count).unwrap_or(0);
        let pcount = pg.map(|g| g.count).unwrap_or(0);
        let dcount = pcount as i64 - bcount as i64;
        rows.push(vec![
            if dself >= 0 {
                red(&format!("+{}", format_bytes(dself as usize)))
            } else {
                green(&format!("-{}", format_bytes(dself.unsigned_abs() as usize)))
            },
            if dcount >= 0 {
                red(&format!("+{dcount}"))
            } else {
                green(&format!("{dcount}"))
            },
            k.to_string(),
        ]);
    }
    rows.sort_by(|a, b| b[1].cmp(&a[1]).then_with(|| a[2].cmp(&b[2])));
    if rows.is_empty() {
        return;
    }
    println!();
    print_header(
        &format!("{profile_path} vs {baseline_path}"),
        Some("owner-chain delta (self, count)"),
    );
    print_table(&["SELF DELTA", "COUNT DELTA", "OWNER CHAIN"], &rows);
}

pub fn run(files: &[String], args: &Args) -> Result<(), String> {
    if files.is_empty() {
        return Err("owners requires at least one .heapsnapshot file".to_string());
    }
    let name = args.name.as_ref().ok_or("owners requires --name <name>")?;

    let mut analyses: Vec<(String, OwnerAnalysis)> = Vec::with_capacity(files.len());
    for f in files {
        if detect_profile_type_for(f)? != "heapsnapshot" {
            return Err(format!(
                "owners is only supported for .heapsnapshot files: {f}"
            ));
        }
        let a = analyze_file(f, name, args)?;
        analyses.push((f.clone(), a));
    }

    if args.json {
        let mut out = json!({
            "type": "heapsnapshot",
            "query": {
                "name": name,
                "exact": args.exact,
                "minSelf": args.min_self,
                "depth": args.depth,
                "top": args.top,
            },
            "files": [],
            "diffs": [],
        });
        let file_arr = analyses
            .iter()
            .map(|(path, a)| {
                json!({
                    "file": path,
                    "totalNodes": a.total_nodes,
                    "totalSelf": a.total_self,
                    "groups": a.groups,
                })
            })
            .collect::<Vec<_>>();
        out["files"] = json!(file_arr);
        let diff_arr = analyses
            .windows(2)
            .map(|pair| {
                let b = &pair[0].1;
                let p = &pair[1].1;
                let mut keys: Vec<&String> = b
                    .groups
                    .iter()
                    .map(|g| &g.chain)
                    .chain(p.groups.iter().map(|g| &g.chain))
                    .collect();
                keys.sort();
                keys.dedup();
                let entries = keys
                    .iter()
                    .filter_map(|k| {
                        let bg = b.groups.iter().find(|g| &g.chain == *k);
                        let pg = p.groups.iter().find(|g| &g.chain == *k);
                        let bself = bg.map(|g| g.self_size).unwrap_or(0);
                        let pself = pg.map(|g| g.self_size).unwrap_or(0);
                        let dself = pself as i64 - bself as i64;
                        if dself.unsigned_abs() < MIN_DELTA as u64 {
                            return None;
                        }
                        Some(json!({
                            "chain": k,
                            "baselineSelf": bself,
                            "profileSelf": pself,
                            "deltaSelf": dself,
                            "baselineCount": bg.map(|g| g.count).unwrap_or(0),
                            "profileCount": pg.map(|g| g.count).unwrap_or(0),
                            "deltaCount": pcount_delta(bg, pg),
                        }))
                    })
                    .collect::<Vec<_>>();
                Some(json!({
                    "baseline": pair[0].0,
                    "profile": pair[1].0,
                    "entries": entries,
                }))
            })
            .collect::<Vec<_>>();
        out["diffs"] = json!(diff_arr);
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return Ok(());
    }

    for (i, (file, a)) in analyses.iter().enumerate() {
        if i > 0 {
            println!();
        }
        print_groups(file, a);
    }
    for pair in analyses.windows(2) {
        print_pair_diff(&pair[0].0, &pair[1].0, &pair[0].1, &pair[1].1);
    }
    Ok(())
}

fn pcount_delta(bg: Option<&hprof_core::OwnerGroup>, pg: Option<&hprof_core::OwnerGroup>) -> i64 {
    let b = bg.map(|g| g.count).unwrap_or(0) as i64;
    let p = pg.map(|g| g.count).unwrap_or(0) as i64;
    p - b
}

fn detect_profile_type_for(path: &str) -> Result<&'static str, String> {
    hprof_core::detect_profile_type(path).map_err(|e| e.to_string())
}
