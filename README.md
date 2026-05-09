# hprof

CLI and web UI for exploring V8 memory profiles.

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
