# hprof

CLI for exploring V8 memory profiles. The core (parsing + analysis) and the
CLI are native Rust; a web UI (Rust-backed, via the FFI bridge) is planned.

Supports:

- `.heapsnapshot`
- `.heapprofile`
- `.heaptimeline`

## Build

```bash
cargo build --release
```

The binary is `target/release/hprof`. `cargo run -p hprof-cli --release -- <args>`
works too.

## Usage

Analyze a profile in the terminal:

```bash
hprof analyze snapshots/Heap-20260508T151623.heapsnapshot
```

Show help:

```bash
hprof help
```

Useful flags:

- `--top <n>`: limit top rows in summaries
- `--filter <re>`: filter results by regex
- `--json`: print machine-readable output

## Heap snapshot inspection

`analyze` shows self sizes; `--retained` adds exclusive retained sizes
(DevTools-style: a node's retained memory is attributed to its constructor
unless its immediate dominator shares it) — the answer to "what actually
holds this memory":

```bash
hprof analyze Heap.heapsnapshot --retained
```

Drill into instances and retention paths:

```bash
# top instances whose name matches, ranked by retained size
hprof inspect Heap.heapsnapshot --name JSArrayBufferData

# node details + shortest path from the GC root (who keeps it alive)
hprof inspect Heap.heapsnapshot --index 6456602
```

The path view follows incoming edges root → … → target with edge names, e.g.:

```
• #0 (synthetic)
  └─ [element:1] → #1 (GC roots)
    └─ [internal:constructor] → #4555750 AssetLoadHub
      └─ [property:cache] → #6337719 Map
        └─ [internal:backing_store] → #6456602 system / JSArrayBufferData
```

For a 7.4M-node snapshot the dominator computation (Lengauer–Tarjan) takes
~2 s and is cached per process, so `analyze --retained` and `inspect` share
it.

### Name queries, properties, retainers (no dominator analysis)

These scan the nodes/edges arrays directly — seconds on multi-GB dumps, no
Lengauer–Tarjan. They replace the hand-rolled scripts people used to write
against raw `.heapsnapshot` JSON:

```bash
# every node named exactly "RenderingGroup" (index, id, self, type, edges)
hprof find Heap.heapsnapshot --name RenderingGroup --exact

# substring match, skip nodes under 1 MB, only objects, all results
hprof find Heap.heapsnapshot --name particle --min-self 1048576 --type object --top 0

# a node's fields with values resolved — numbers/strings inlined,
# objects as "name (type, index=..., id=...)"; read renderingGroupId etc.
hprof props Heap.heapsnapshot --index 7396246

# who keeps a node alive: every incoming edge
hprof retainers Heap.heapsnapshot --index 7396246

# first-parent (owner) chain, target first
hprof retainers Heap.heapsnapshot --index 7396246 --depth 12

# group matching nodes by their "owner -> parent -> ..." chain and diff
# the groups across several snapshots — the classic "(object elements)
# grouped by owner" leak analysis
hprof owners a.heapsnapshot b.heapsnapshot --name '(object elements)' --exact \
  --min-self 1048576 --depth 4
```

`diff` accepts more than two files and compares them pairwise.

### Leak-hunting helpers

```bash
# largest individual nodes; add --exact to force a dominator calculation
hprof top Heap.heapsnapshot --top 30
hprof top Heap.heapsnapshot --top 30 --exact

# V8 detached nodes, optionally with their owner chain
hprof detached Heap.heapsnapshot --depth 8

# find objects that have a property/edge with a given name
hprof edges Heap.heapsnapshot --name cache --exact --edge-type property

# power-of-two self-size distribution and repeated string contents
hprof sizes Heap.heapsnapshot
hprof strings Heap.heapsnapshot --top 50

# object-identity growth across every adjacent pair of snapshots
hprof trend before.heapsnapshot after.heapsnapshot later.heapsnapshot
```

`diff` now includes object-identity counts and bounded lists of new, deleted,
and growing nodes. Object ids are stable across V8 snapshots, so the reported
profile indexes can be passed directly to `props`, `retainers`, or `inspect`.
For snapshots above five million nodes, `top` is approximate by default and
labels the result; `--exact` explicitly opts into the high-memory dominator
calculation.

## Heap Timeline Analysis

`analyze` on a `.heaptimeline` prints, in addition to the by-type summary:

- **object-growth profile** — how many objects were allocated per second across the recording
- **top allocation names** — constructor names ranked by total self-size, with the per-type split (`system / JSArrayBufferData`, `Vector3`, …)
- **top allocation sites** — stack traces (leaf ← caller) from the allocation trace tree, so you can see _who_ allocates
- `--filter <re>` narrows both names and stacks (e.g. `--filter 'Vector3|Particle'`)

The file is mmap'd and parsed once per process, so repeated queries are cheap:

```bash
hprof analyze snapshots/Heap-20260508T151658.heaptimeline --top 20
hprof analyze snapshots/Heap-20260508T151658.heaptimeline --filter 'Vector3' --json
```

## Other commands

- `diff <baseline> <profile> [<more>...]` — compare profiles of the same
  type; with 3+ files the comparison is pairwise
- `trend <before> <after> [<more>...]` — object-identity growth between
  adjacent heap snapshots
- `find` / `props` / `retainers` / `owners` — heap snapshot queries, see above
- `top` / `detached` / `edges` / `sizes` / `strings` — leak-hunting reports
- `session <file.heapsnapshot>` — persistent REPL; parsed columns, edges,
  parent map, and dominators are reused between commands
- `list <file.heapprofile>` — sampled locations grouped by file:line
- `dot <file.heapprofile>` — emit a call graph as DOT for graphviz
- `dot <file.heapsnapshot> --index <n> --depth 3` — bounded object subgraph

## Large Profiles

- `analyze`, `inspect` are optimized for very large `.heapsnapshot` files
- `retained` sizes use an exact dominator tree (Lengauer–Tarjan, flat CSR
  reverse graph) — ~2 s and ~1 GB peak on a 7.4M-node / 24M-edge snapshot

## Project layout

- `crates/hprof-core`: parsers, summarizers, dominator/retained analysis
- `crates/hprof-cli`: the `hprof` binary
- `crates/hprof-c` + `packages/core` + `packages/ui`: FFI bridge and web UI
  (deferred; kept for the upcoming UI work)

Run core tests:

```bash
cargo test --workspace
```

## Web UI

The web UI is available through the CLI when Bun and the workspace packages are
installed:

```bash
cargo build --release -p hprof-c
hprof ui snapshots/Heap-20260508T151623.heapsnapshot --open
```

It includes snapshot insights for detached nodes, size histograms, repeated
strings, property search, retained-size exact mode, and object diff tables.
