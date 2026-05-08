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

const C = {
  reset: "\x1b[0m",
  bold: "\x1b[1m",
  dim: "\x1b[2m",
  cyan: "\x1b[36m",
  yellow: "\x1b[33m",
  green: "\x1b[32m",
  red: "\x1b[31m",
  magenta: "\x1b[35m",
  blue: "\x1b[34m",
  gray: "\x1b[90m",
};

function bold(s: string) { return `${C.bold}${s}${C.reset}`; }
function cyan(s: string) { return `${C.cyan}${s}${C.reset}`; }
function yellow(s: string) { return `${C.yellow}${s}${C.reset}`; }
function green(s: string) { return `${C.green}${s}${C.reset}`; }
function red(s: string) { return `${C.red}${s}${C.reset}`; }
function dim(s: string) { return `${C.dim}${s}${C.reset}`; }
function magenta(s: string) { return `${C.magenta}${s}${C.reset}`; }
function gray(s: string) { return `${C.gray}${s}${C.reset}`; }

function progressBar(pct: number, phase: string, width = 30) {
  const filled = Math.round((pct / 100) * width);
  const bar = "█".repeat(filled) + "░".repeat(width - filled);
  process.stderr.write(`\r  ${dim(phase.padEnd(10))} ${bar} ${pct}%`);
  if (pct >= 100) process.stderr.write("\n");
}

function printUsage() {
  console.log(`
${bold("Usage:")} hprof <command> [options] <file>

${bold("Commands:")}
  ${cyan("analyze")}   Analyze profile file and print summary to stdout (default)
  ${cyan("ui")}        Start web UI server for interactive analysis
  ${cyan("help")}      Show this help message

${bold("Options:")}
  ${yellow("--top <n>")}       Number of top entries to show (default: 30)
  ${yellow("--filter <re>")}   Filter results by regex
  ${yellow("--json")}          Output as JSON
  ${yellow("--port <port>")}   Port for UI server (default: 3000)
  ${yellow("--open")}          Open browser automatically (ui command only)

${bold("Supported formats:")}
  ${green(".heapsnapshot")}   V8 heap snapshot
  ${green(".heapprofile")}    V8 sampling heap profile
  ${green(".heaptimeline")}   V8 heap allocation timeline
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

function printTable(headers: string[], rows: string[][]) {
  const widths = headers.map((h, i) =>
    Math.max(
      h.length,
      ...rows.map((r) => (r[i] ?? "").length),
    ),
  );

  const headerLine = headers
    .map((h, i) => h.padEnd(widths[i]!))
    .join("  ");
  console.log(`  ${dim(headerLine)}`);
  console.log(`  ${dim("─".repeat(headerLine.length))}`);

  for (const row of rows) {
    const line = row
      .map((cell, i) => cell.padEnd(widths[i]!))
      .join("  ");
    console.log(`  ${line}`);
  }
}

function printHeader(title: string, subtitle?: string) {
  console.log();
  console.log(`  ${bold(cyan(title))}`);
  if (subtitle) console.log(`  ${dim(subtitle)}`);
}

function analyzeHeapProfile(filePath: string, args: CliArgs) {
  const data = parseHeapProfile(filePath);
  const summary = summarizeHeapProfile(data, {
    top: args.top,
    filter: args.filter ?? undefined,
  });

  if (args.json) {
    console.log(JSON.stringify({
      file: filePath,
      type: "heapprofile" as ProfileType,
      totalSize: summary.totalSize,
      byFrame: [...summary.byFrame.entries()],
      byUrl: [...summary.byUrl.entries()],
      byFunction: [...summary.byFunction.entries()],
    }, null, 2));
    return;
  }

  const toRows = (map: Map<string, number>) =>
    [...map.entries()]
      .sort((a, b) => b[1] - a[1])
      .slice(0, args.top);

  printHeader(filePath, `heapprofile | total sampled: ${yellow(formatBytes(summary.totalSize))}`);

  const frameRows = toRows(summary.byFrame);
  printTable(
    ["SIZE", "FRAME"],
    frameRows.map(([key, size]) => [green(formatBytes(size)), key]),
  );

  const urlRows = toRows(summary.byUrl);
  printTable(
    ["SIZE", "URL"],
    urlRows.map(([key, size]) => [green(formatBytes(size)), gray(key)]),
  );

  const fnRows = toRows(summary.byFunction);
  printTable(
    ["SIZE", "FUNCTION"],
    fnRows.map(([key, size]) => [green(formatBytes(size)), magenta(key)]),
  );
}

async function analyzeHeapSnapshot(filePath: string, args: CliArgs) {
  const meta = parseSnapshotMeta(filePath);
  const summary = await streamHeapSnapshotSummary(filePath, {
    top: args.top,
    filter: args.filter ?? undefined,
    onProgress: args.json ? undefined : (phase, pct) => progressBar(pct, phase),
  });

  if (args.json) {
    console.log(JSON.stringify({
      file: filePath,
      type: "heapsnapshot" as ProfileType,
      nodeCount: meta.node_count,
      edgeCount: meta.edge_count,
      extraNativeBytes: meta.extra_native_bytes ?? 0,
      totalSize: summary.totalSize,
      totalCount: summary.totalCount,
      byNodeName: [...summary.byNodeName.entries()].map(([name, info]) => ({
        name, size: info.size, count: info.count,
      })),
      byNodeType: [...summary.byNodeType.entries()].map(([type, info]) => ({
        type, size: info.size, count: info.count,
      })),
    }, null, 2));
    return;
  }

  printHeader(
    filePath,
    [
      `heapsnapshot`,
      `nodes: ${bold(meta.node_count.toLocaleString())}`,
      `edges: ${bold(meta.edge_count.toLocaleString())}`,
      `total self size: ${yellow(formatBytes(summary.totalSize))}`,
    ].join(" | "),
  );

  const nameRows = [...summary.byNodeName.entries()].slice(0, args.top);
  printTable(
    ["SIZE", "COUNT", "NAME"],
    nameRows.map(([name, info]) => [
      green(formatBytes(info.size)),
      dim(String(info.count)),
      name,
    ]),
  );

  const typeRows = [...summary.byNodeType.entries()]
    .sort((a, b) => b[1].size - a[1].size)
    .slice(0, args.top);
  printTable(
    ["SIZE", "COUNT", "TYPE"],
    typeRows.map(([type, info]) => [
      green(formatBytes(info.size)),
      dim(String(info.count)),
      magenta(type),
    ]),
  );
}

async function analyzeHeapTimeline(filePath: string, args: CliArgs) {
  const meta = parseSnapshotMeta(filePath);
  const summary = await streamHeapTimelineSummary(filePath, {
    top: args.top,
    filter: args.filter ?? undefined,
  });

  if (args.json) {
    console.log(JSON.stringify({
      file: filePath,
      type: "heaptimeline" as ProfileType,
      nodeCount: meta.node_count,
      edgeCount: meta.edge_count,
      totalAllocated: summary.totalAllocated,
      totalFreed: summary.totalFreed,
      byType: [...summary.byType.entries()].map(([type, info]) => ({
        type, allocated: info.allocated, freed: info.freed, count: info.count,
      })),
    }, null, 2));
    return;
  }

  printHeader(
    filePath,
    [
      `heaptimeline`,
      `nodes: ${bold(meta.node_count.toLocaleString())}`,
      `total allocated: ${yellow(formatBytes(summary.totalAllocated))}`,
    ].join(" | "),
  );

  const typeRows = [...summary.byType.entries()].slice(0, args.top);
  printTable(
    ["ALLOCATED", "COUNT", "TYPE"],
    typeRows.map(([type, info]) => [
      green(formatBytes(info.allocated)),
      dim(String(info.count)),
      magenta(type),
    ]),
  );
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
        console.error(`  ${red("Error:")} ${(err as Error).message}`);
      }
    }
  }
}

main().catch((err) => {
  console.error(`${red("Error:")} ${err.message}`);
  process.exit(1);
});
