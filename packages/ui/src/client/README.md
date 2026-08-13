# hprof UI Client

React frontend for the `hprof` web UI.

## Install

```bash
bun install
```

## Local development

Start the profile API server from the repository root (the UI is
deferred — the CLI is native Rust now; the server is launched directly):

```bash
cargo build --release -p hprof-c
bun -e 'import { startServer } from "@hprof/ui"; await startServer({ files: ["snapshots/Heap-20260508T151623.heapsnapshot"], port: 3000 })'
```

Then start the Vite dev server in this directory:

```bash
bun run dev
```

The client proxies `/api` requests to `http://localhost:3000`.

## Build

```bash
bun run build
```

The generated `dist/` bundle is what the `@hprof/ui` server serves in production.
