//! hprof — CLI for exploring V8 memory profiles.
//!
//! Native Rust port of the original Bun/TypeScript CLI. Talks to
//! `hprof-core` directly (no FFI), so the whole analysis stack is one
//! binary: parse, summarize, retained sizes, retention paths.

mod analyze;
mod calltree;
mod diff;
mod dot;
mod flame;
mod inspect;
mod list;

use std::io::IsTerminal;
use std::process::ExitCode;

use hprof_core::detect_profile_type;

// ============================================================================
// Color / formatting helpers (ported from the TS CLI)
// ============================================================================

pub fn use_color() -> bool {
    static C: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *C.get_or_init(|| {
        !cfg!(windows) && std::io::stdout().is_terminal() && std::env::var("NO_COLOR").is_err()
    })
}

fn color(code: &str, s: &str) -> String {
    if use_color() {
        format!("{code}{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    color("\x1b[1m", s)
}
pub fn cyan(s: &str) -> String {
    color("\x1b[36m", s)
}
pub fn yellow(s: &str) -> String {
    color("\x1b[33m", s)
}
pub fn green(s: &str) -> String {
    color("\x1b[32m", s)
}
pub fn red(s: &str) -> String {
    color("\x1b[31m", s)
}
pub fn dim(s: &str) -> String {
    color("\x1b[2m", s)
}
pub fn magenta(s: &str) -> String {
    color("\x1b[35m", s)
}
pub fn gray(s: &str) -> String {
    color("\x1b[90m", s)
}

const ANSI_RE: &str = r"\x1b\[[0-9;]*m";

fn strip_ansi(s: &str) -> String {
    regex::Regex::new(ANSI_RE)
        .unwrap()
        .replace_all(s, "")
        .into_owned()
}

/// Collapse ANSI codes + control characters + whitespace (for column width
/// measurement).
fn normalize_cell(s: &str) -> String {
    let re = regex::Regex::new(ANSI_RE).unwrap();
    let plain = re.replace_all(s, "");
    let plain = plain
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_visible(source: &str, width: usize) -> String {
    let plain = normalize_cell(source);
    let len = plain.chars().count();
    if len <= width {
        return plain;
    }
    if width <= 1 {
        return "…".to_string();
    }
    let mut out: String = plain.chars().take(width - 1).collect();
    out.push('…');
    out
}

fn pad_visible(s: &str, width: usize) -> String {
    let visible = strip_ansi(s).chars().count();
    let mut out = s.to_string();
    if visible < width {
        out.push_str(&" ".repeat(width - visible));
    }
    out
}

/// Re-apply the leading color code of `source` to `text` (used for the
/// truncated last column so the colour survives truncation).
fn apply_cell_style(source: &str, text: &str) -> String {
    let re = regex::Regex::new(&format!(r"(?s)^({ANSI_RE})(.*?)({ANSI_RE})?$")).unwrap();
    if let Some(caps) = re.captures(source) {
        if caps.get(1).is_some() {
            let lead = caps.get(1).unwrap().as_str();
            let trail = caps.get(3).map_or("", |m| m.as_str());
            return format!("{lead}{text}{trail}");
        }
    }
    text.to_string()
}

pub fn format_bytes(bytes: usize) -> String {
    hprof_core::format_bytes(bytes)
}

pub fn pct(value: usize, total: usize) -> String {
    if total == 0 {
        return "0.00%".to_string();
    }
    format!("{:.2}%", (value as f64 / total as f64) * 100.0)
}

pub fn format_delta(delta: i64) -> String {
    if delta == 0 {
        return gray("±0 B");
    }
    let abs = delta.unsigned_abs() as usize;
    let formatted = format_bytes(abs);
    if delta > 0 {
        red(&format!("+{formatted}"))
    } else {
        green(&format!("-{formatted}"))
    }
}

pub fn format_duration(ms: f64) -> String {
    if ms < 1000.0 {
        return format!("{ms:.0}ms");
    }
    let s = ms / 1000.0;
    if s < 60.0 {
        return format!("{s:.1}s");
    }
    format!("{}m{:.0}s", (s / 60.0).floor() as u64, s % 60.0)
}

// ============================================================================
// Tables (ported from the TS printTable)
// ============================================================================

pub fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let normalized_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|cell| normalize_cell(cell)).collect())
        .collect();

    let mut widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let cell_max = normalized_rows
                .iter()
                .map(|r| r.get(i).map(|c| c.chars().count()).unwrap_or(0))
                .max()
                .unwrap_or(0);
            h.chars().count().max(cell_max)
        })
        .collect();

    let padding_width = 2 * (widths.len().saturating_sub(1));
    if !widths.is_empty() {
        let terminal_width = terminal_width();
        let fixed_columns_width: usize = widths[..widths.len() - 1].iter().sum();
        let last_min = headers[widths.len() - 1].chars().count();
        let last_max = (terminal_width - 2 - padding_width - fixed_columns_width).max(last_min);
        let last = widths.len() - 1;
        widths[last] = widths[last].min(last_max);
    }

    let header_line = headers
        .iter()
        .enumerate()
        .map(|(i, h)| pad_visible(&dim(h), widths[i]))
        .collect::<Vec<_>>()
        .join("  ");
    println!("  {header_line}");
    let sep_width: usize = widths.iter().sum::<usize>() + padding_width;
    println!("  {}", dim(&"─".repeat(sep_width)));

    for (row_index, row) in rows.iter().enumerate() {
        let line = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let w = widths[i];
                if i == row.len() - 1 {
                    let truncated = truncate_visible(cell, w);
                    let styled = apply_cell_style(cell, &truncated);
                    return pad_visible(&styled, w);
                }
                let normalized = &normalized_rows[row_index][i];
                if *normalized == strip_ansi(cell) {
                    return pad_visible(cell, w);
                }
                pad_visible(normalized, w)
            })
            .collect::<Vec<_>>()
            .join("  ");
        println!("  {line}");
    }
    println!();
}

fn terminal_width() -> usize {
    // The TS CLI read process.stdout.columns; without a libc dep we take
    // COLUMNS and otherwise fall back to 120.
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|w| *w >= 60)
        .unwrap_or(120)
}

pub fn print_header(title: &str, subtitle: Option<&str>) {
    println!();
    println!("  {}", bold(&cyan(title)));
    if let Some(sub) = subtitle {
        println!("  {}", dim(sub));
    }
}

/// Write a transient "working…" note to stderr, cleared on drop.
pub struct WorkingNote;

impl WorkingNote {
    pub fn new(text: &str) -> Self {
        if use_color() {
            eprint!("\r  {}", dim(text));
        } else {
            eprint!("\r  {text}");
        }
        Self
    }
}

impl Drop for WorkingNote {
    fn drop(&mut self) {
        eprint!("\r\x1b[K");
        let _ = std::io::Write::flush(&mut std::io::stderr());
    }
}

// ============================================================================
// Args
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Analyze,
    Diff,
    Dot,
    List,
    Inspect,
    Calltree,
    Flame,
    Ui,
    Help,
}

pub struct Args {
    pub command: Command,
    pub files: Vec<String>,
    pub top: usize,
    pub filter: Option<String>,
    pub focus: Option<String>,
    pub ignore: Option<String>,
    pub hide: Option<String>,
    pub cum: bool,
    pub json: bool,
    /// analyze heapsnapshot: include exclusive retained sizes
    pub retained: bool,
    /// inspect: drill into instances whose name matches
    pub name: Option<String>,
    /// inspect: show a single node by DevTools id
    pub id: Option<usize>,
    /// inspect: show a single node by record index
    pub index: Option<usize>,
    /// heapprofile: restrict contribution to frames from URLs containing this
    /// substring (analyze, calltree)
    pub url: Option<String>,
    /// whether --top was passed explicitly (flame does not cap by default)
    pub top_explicit: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            command: Command::Analyze,
            files: Vec::new(),
            top: 30,
            filter: None,
            focus: None,
            ignore: None,
            hide: None,
            cum: false,
            json: false,
            retained: false,
            name: None,
            id: None,
            index: None,
            url: None,
            top_explicit: false,
        }
    }
}

fn parse_args(argv: &[String]) -> Args {
    let mut args = Args::default();
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        let next = |i: &mut usize| {
            *i += 1;
            argv.get(*i).cloned()
        };
        match arg.as_str() {
            "--top" => {
                args.top_explicit = true;
                if let Some(v) = next(&mut i) {
                    args.top = v.parse().unwrap_or(args.top);
                }
            }
            "--filter" => {
                if let Some(v) = next(&mut i) {
                    args.filter = Some(v);
                }
            }
            "--focus" => {
                if let Some(v) = next(&mut i) {
                    args.focus = Some(v);
                }
            }
            "--ignore" => {
                if let Some(v) = next(&mut i) {
                    args.ignore = Some(v);
                }
            }
            "--hide" => {
                if let Some(v) = next(&mut i) {
                    args.hide = Some(v);
                }
            }
            "--cum" => args.cum = true,
            "--json" => args.json = true,
            "--retained" => args.retained = true,
            "--name" => {
                if let Some(v) = next(&mut i) {
                    args.name = Some(v);
                }
            }
            "--id" => {
                if let Some(v) = next(&mut i) {
                    args.id = v.parse().ok();
                }
            }
            "--index" => {
                if let Some(v) = next(&mut i) {
                    args.index = v.parse().ok();
                }
            }
            "--url" => {
                if let Some(v) = next(&mut i) {
                    args.url = Some(v);
                }
            }
            "analyze" => args.command = Command::Analyze,
            "diff" => args.command = Command::Diff,
            "dot" => args.command = Command::Dot,
            "list" => args.command = Command::List,
            "inspect" => args.command = Command::Inspect,
            "calltree" => args.command = Command::Calltree,
            "flame" => args.command = Command::Flame,
            "ui" => args.command = Command::Ui,
            "help" => args.command = Command::Help,
            "bench" => {
                eprintln!(
                    "  {} the bench command was part of the Bun CLI and is gone",
                    red("Error:")
                );
                std::process::exit(1);
            }
            _ if arg.starts_with('-') => {
                eprintln!("  {} unknown option: {arg}", red("Error:"));
                std::process::exit(1);
            }
            _ => args.files.push(arg.clone()),
        }
        i += 1;
    }
    args
}

fn print_usage() {
    let usage = r#"
 {b}Usage:{r} hprof <command> [options] <file>

 {b}Commands:{r}
    {c}analyze{r}   Analyze profile file and print summary to stdout (default)
    {c}diff{r}      Compare two profiles of the same type (baseline <profile>)
    {c}dot{r}       Emit call graph as DOT for use with graphviz
    {c}list{r}      List sampled locations grouped by file:line (heapprofile)
    {c}inspect{r}   Inspect a heap snapshot: instances by name, paths from root
    {c}calltree{r}  Inclusive call tree for a sampling profile (heapprofile)
    {c}flame{r}     Folded stacks (a;b;c <size>) for flamegraph.pl / speedscope
    {c}help{r}      Show this help message

 {b}Options:{r}
   {y}--top <n>{r}       Number of top entries to show (default: 30)
   {y}--filter <re>{r}   Filter results by regex (timeline: names + stacks)
   {y}--focus <re>{r}    pprof-style focus: only frames matching contribute
   {y}--ignore <re>{r}   pprof-style ignore: drop flat attribution for matches
   {y}--hide <re>{r}     pprof-style hide: drop matching frames from visualisations
   {y}--cum{r}           Show cumulative (self + descendants) instead of flat only
   {y}--retained{r}      heapsnapshot: add exclusive retained sizes to the summary
   {y}--url <substr>{r}   heapprofile: only frames from URLs containing this contribute
   {y}--name <re>{r}      heaptimeline: show allocation stacks for matching constructor names
   {y}--json{r}          Output as JSON

 {b}Heap snapshot inspection:{r}
   hprof inspect file.heapsnapshot --name JSArrayBufferData
       Top instances whose name matches, ranked by retained size.
   hprof inspect file.heapsnapshot --id 123456
       Node details + shortest path from the GC root (who keeps it alive).
   hprof inspect file.heapsnapshot --index 6456602
       Same, addressing the node by record index (as printed by --name).

 {b}Heap timeline analysis:{r}
   analyze on a .heaptimeline prints, in addition to the by-type summary:
     - top allocation names with per-type split
     - top allocation sites as stack traces (leaf <- caller)
     - object-growth profile over the recording
   {gr}--filter Vector3{r} narrows names and stacks to matching entries.

 {b}Flame output:{r}
   Folded stacks for flamegraph.pl / speedscope (heapprofile, heaptimeline):
     {gr}hprof flame file.heapprofile | flamegraph.pl > flame.svg{r}
     {gr}hprof flame file.heaptimeline | flamegraph.pl > flame.svg{r}
   {y}--top <n>{r} caps the lines; {y}--filter <re>{r} keeps only matching stacks.

 {b}Call tree:{r}
   {gr}hprof calltree file.heapprofile{r} — inclusive (self + subtree) tree,
   prune with {y}--url <substr>{r} or {y}--focus <re>{r}.

 {b}Dot output:{r}
   Pipe to graphviz to render a graph. Examples:
     {gr}hprof dot file.heapprofile | dot -Tsvg -o graph.svg{r}
     {gr}hprof dot file.heapprofile | dot -Tpng -o graph.png{r}

 {b}Supported formats:{r}
   {g}.heapsnapshot{r}   V8 heap snapshot
   {g}.heapprofile{r}    V8 sampling heap profile
   {g}.heaptimeline{r}   V8 heap allocation timeline
"#
    .replace("{b}", &bold(""))
    .replace("{r}", "\x1b[0m")
    .replace("{c}", &cyan(""))
    .replace("{y}", &yellow(""))
    .replace("{g}", &green(""))
    .replace("{gr}", &gray(""))
    .replace("{m}", &magenta(""));
    println!("{usage}");
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(&argv);

    match args.command {
        Command::Help => {
            print_usage();
            return ExitCode::SUCCESS;
        }
        Command::Ui => {
            eprintln!(
                "  {} the web UI is not part of the Rust CLI yet — it will be re-added later",
                red("Error:")
            );
            return ExitCode::FAILURE;
        }
        _ => {}
    }

    if args.files.is_empty() {
        print_usage();
        return ExitCode::FAILURE;
    }

    // diff consumes both files as one operation — run it outside the per-file
    // loop so the output is not duplicated
    if args.command == Command::Diff {
        return match diff::run(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("  {} {e}", red("Error:"));
                ExitCode::FAILURE
            }
        };
    }

    let mut code = ExitCode::SUCCESS;
    for file in &args.files {
        let type_name = match detect_profile_type(file) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("  {} {file}: {e}", red("Error:"));
                code = ExitCode::FAILURE;
                continue;
            }
        };

        let result = match args.command {
            Command::Analyze => analyze::run(file, type_name, &args),
            Command::Dot => dot::run(file, type_name, &args),
            Command::List => list::run(file, type_name, &args),
            Command::Inspect => inspect::run(file, type_name, &args),
            Command::Calltree => calltree::run(file, type_name, &args),
            Command::Flame => flame::run(file, type_name, &args),
            _ => unreachable!(),
        };

        if let Err(e) = result {
            eprintln!("  {} {e}", red("Error:"));
            code = ExitCode::FAILURE;
        }
    }

    code
}
