#!/usr/bin/env node

import {
  detectProfileType,
  formatBytes,
  parseHeapProfile,
  summarizeHeapProfile,
  streamHeapSnapshotSummary,
  parseSnapshotMeta,
  streamHeapTimelineSummary,
} from "@hprof/core";
import type { ProfileType } from "@hprof/core";

function printUsage() {
  console.log(`
Usage: hprof <command> [options] <file>

Commands:
  analyze   Analyze profile file and print summary to stdout (default)
  ui        Start web UI server for interactive analysis
  help      Show this help message

Options:
  --top <n>       Number of top entries to show (default: 30)
  --filter <re>   Filter results by regex
  --json          Output as JSON
  --port <port>   Port for UI server (default: 3000)
  --open          Open browser automatically (ui command only)

Supported formats:
  .heapsnapshot   V8 heap snapshot
  .heapprofile    V8 sampling heap profile
  .heaptimeline   V8 heap allocation timeline
`);
}

interface CliArgs {
  command: string;
  files: string[];
  top: number;
  filter: string | null;
  port: number;
  open: boolean;
  json: boolean;
}

function parseArgs(argv: string[]): CliArgs {
  const args: CliArgs = {
    command: "analyze",
    files: [],
    top: 30,
    filter: null,
    port: 3000,
    open: false,
    json: false,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]!;
    if (arg === "--top") {
      args.top = Number(argv[++i] ?? args.top);
    } else if (arg === "--filter") {
      args.filter = argv[++i] ?? null;
    } else if (arg === "--port") {
      args.port = Number(argv[++i] ?? args.port);
    } else if (arg === "--open") {
      args.open = true;
    } else if (arg === "--json") {
      args.json = true;
    } else if (arg === "analyze" || arg === "ui" || arg === "help") {
      args.command = arg;
    } else if (!arg.startsWith("-")) {
      args.files.push(arg);
    }
  }

  return args;
}

function printSection(title: string, rows: Record<string, string>[]) {
  console.log(`\n${title}`);
  if (!rows.length) {
    console.log("  (empty)");
    return;
  }

  for (const row of rows) {
    const parts = Object.entries(row).map(([key, value]) => `${key}=${value}`);
    console.log(`  ${parts.join(" | ")}`);
  }
}

function analyzeHeapProfile(filePath: string, args: CliArgs) {
  const data = parseHeapProfile(filePath);
  const summary = summarizeHeapProfile(data, {
    top: args.top,
    filter: args.filter ?? undefined,
  });

  if (args.json) {
    const obj = {
      file: filePath,
      type: "heapprofile" as ProfileType,
      totalSize: summary.totalSize,
      byFrame: [...summary.byFrame.entries()],
      byUrl: [...summary.byUrl.entries()],
      byFunction: [...summary.byFunction.entries()],
    };
    console.log(JSON.stringify(obj, null, 2));
    return;
  }

  const toRows = (map: Map<string, number>) =>
    [...map.entries()]
      .sort((a, b) => b[1] - a[1])
      .map(([key, size]) => ({ size: formatBytes(size), key }));

  console.log(`\n=== ${filePath} (heapprofile) ===`);
  console.log(`totalSize=${formatBytes(summary.totalSize)}`);
  printSection("Top Frames", toRows(summary.byFrame));
  printSection("Top URLs", toRows(summary.byUrl));
  printSection("Top Functions", toRows(summary.byFunction));
}

async function analyzeHeapSnapshot(filePath: string, args: CliArgs) {
  const meta = parseSnapshotMeta(filePath);
  const summary = await streamHeapSnapshotSummary(filePath, {
    top: args.top,
    filter: args.filter ?? undefined,
  });

  if (args.json) {
    const obj = {
      file: filePath,
      type: "heapsnapshot" as ProfileType,
      nodeCount: meta.node_count,
      edgeCount: meta.edge_count,
      extraNativeBytes: meta.extra_native_bytes ?? 0,
      totalSize: summary.totalSize,
      totalCount: summary.totalCount,
      byNodeName: [...summary.byNodeName.entries()].map(([name, info]) => ({
        name,
        size: info.size,
        count: info.count,
      })),
      byNodeType: [...summary.byNodeType.entries()].map(([type, info]) => ({
        type,
        size: info.size,
        count: info.count,
      })),
    };
    console.log(JSON.stringify(obj, null, 2));
    return;
  }

  console.log(`\n=== ${filePath} (heapsnapshot) ===`);
  console.log(
    `node_count=${meta.node_count} | edge_count=${meta.edge_count} | extra_native_bytes=${formatBytes(meta.extra_native_bytes ?? 0)}`,
  );

  const nameRows = [...summary.byNodeName.entries()].map(([name, info]) => ({
    size: formatBytes(info.size),
    count: String(info.count),
    name,
  }));
  printSection("Top Node Names By Self Size", nameRows);

  const typeRows = [...summary.byNodeType.entries()]
    .sort((a, b) => b[1].size - a[1].size)
    .slice(0, args.top)
    .map(([type, info]) => ({
      size: formatBytes(info.size),
      count: String(info.count),
      type,
    }));
  printSection("Top Node Types By Self Size", typeRows);
}

async function analyzeHeapTimeline(filePath: string, args: CliArgs) {
  const meta = parseSnapshotMeta(filePath);
  const summary = await streamHeapTimelineSummary(filePath, {
    top: args.top,
    filter: args.filter ?? undefined,
  });

  if (args.json) {
    const obj = {
      file: filePath,
      type: "heaptimeline" as ProfileType,
      nodeCount: meta.node_count,
      edgeCount: meta.edge_count,
      totalAllocated: summary.totalAllocated,
      totalFreed: summary.totalFreed,
      byType: [...summary.byType.entries()].map(([type, info]) => ({
        type,
        allocated: info.allocated,
        freed: info.freed,
        count: info.count,
      })),
    };
    console.log(JSON.stringify(obj, null, 2));
    return;
  }

  console.log(`\n=== ${filePath} (heaptimeline) ===`);
  console.log(
    `node_count=${meta.node_count} | edge_count=${meta.edge_count} | total_allocated=${formatBytes(summary.totalAllocated)}`,
  );

  const typeRows = [...summary.byType.entries()].map(([type, info]) => ({
    allocated: formatBytes(info.allocated),
    count: String(info.count),
    type,
  }));
  printSection("Allocations By Type", typeRows);
}

async function main() {
  const args = parseArgs(process.argv.slice(2));

  if (args.command === "help" || (args.files.length === 0 && args.command !== "help")) {
    printUsage();
    process.exit(args.command === "help" ? 0 : 1);
  }

  if (args.command === "ui") {
    const { startServer } = await import("@hprof/ui");
    await startServer({
      files: args.files,
      port: args.port,
      open: args.open,
    });
    return;
  }

  if (args.command === "analyze") {
    for (const file of args.files) {
      const type = detectProfileType(file);
      try {
        switch (type) {
          case "heapprofile":
            analyzeHeapProfile(file, args);
            break;
          case "heapsnapshot":
            await analyzeHeapSnapshot(file, args);
            break;
          case "heaptimeline":
            await analyzeHeapTimeline(file, args);
            break;
        }
      } catch (err) {
        console.error(`Error analyzing ${file}: ${(err as Error).message}`);
      }
    }
  }
}

main().catch((err) => {
  console.error(err.message);
  process.exit(1);
});
