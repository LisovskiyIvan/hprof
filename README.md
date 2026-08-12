# hprof

CLI and web UI for exploring V8 memory profiles.

This project is Bun-first and is intended to be run with `bun`.

Supports:

- `.heapsnapshot`
- `.heapprofile`
- `.heaptimeline`

## Install

```bash
bun install
```

## Usage

Analyze a profile in the terminal:

```bash
bun packages/cli/src/cli.ts analyze snapshots/Heap-20260508T151623.heapsnapshot
```

Open the web UI on `http://localhost:3000`:

```bash
bun packages/cli/src/cli.ts ui snapshots/Heap-20260508T151623.heapsnapshot
```

Show help:

```bash
bun packages/cli/src/cli.ts help
```

Useful flags:

- `--top <n>`: limit top rows in summaries
- `--filter <re>`: filter results by regex
- `--json`: print machine-readable output
- `--port <port>`: change UI server port
- `--open`: open the UI in a browser

## Large Profiles

- `summary`, `nodes`, and `search` are optimized for very large `.heapsnapshot` files under Bun
- `retained` falls back to an approximate top-self-size view for very large snapshots so the UI stays responsive

## Heap Timeline Analysis

`analyze` on a `.heaptimeline` prints, in addition to the by-type summary:

- **object-growth profile** — how many objects were allocated per second across the recording
- **top allocation names** — constructor names ranked by total self-size, with the per-type split (`system / JSArrayBufferData`, `Vector3`, …)
- **top allocation sites** — stack traces (leaf ← caller) from the allocation trace tree, so you can see *who* allocates
- `--filter <re>` narrows both names and stacks (e.g. `--filter 'Vector3|Particle'`)

The file is mmap'd and parsed once per process, so repeated queries (and the web UI) are cheap:

```bash
bun packages/cli/src/cli.ts analyze snapshots/Heap-20260508T151658.heaptimeline --top 20
bun packages/cli/src/cli.ts analyze snapshots/Heap-20260508T151658.heaptimeline --filter 'Vector3' --json
```

In the web UI, the Timeline tab shows the growth chart, clickable name table (click a name to see where it is allocated), and the top stack traces.

## Development

Project layout:

- `packages/core`: parsers and summarizers
- `packages/cli`: terminal interface
- `packages/ui`: HTTP API server and bundled React UI

Run core tests:

```bash
bun test packages/core/tests
```

Rebuild the bundled UI after frontend changes:

```bash
cd packages/ui/src/client
bun run build
```

For client-side UI development with Vite:

1. Start the API server:

```bash
bun packages/cli/src/cli.ts ui snapshots/Heap-20260508T151623.heapsnapshot
```
