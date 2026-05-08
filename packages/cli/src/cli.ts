#!/usr/bin/env node

import { detectProfileType } from "@hprof/core";

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
}

function parseArgs(argv: string[]): CliArgs {
  const args: CliArgs = {
    command: "analyze",
    files: [],
    top: 30,
    filter: null,
    port: 3000,
    open: false,
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
    } else if (arg === "analyze" || arg === "ui" || arg === "help") {
      args.command = arg;
    } else if (!arg.startsWith("-")) {
      args.files.push(arg);
    }
  }

  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));

  if (args.command === "help" || args.files.length === 0 && args.command !== "help") {
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
      console.log(`Analyzing ${file} (${type})...`);
      // TODO: delegate to core parsers
    }
  }
}

main().catch((err) => {
  console.error(err.message);
  process.exit(1);
});
