#!/usr/bin/env bun

const USE_COLOR = Boolean(process.stdout.isTTY) && process.env.NO_COLOR === undefined

import {
  detectProfileType,
  formatBytes,
  HeapProfile,
  HeapSnapshot,
  HeapTimeline,
} from '@hprof/core'
import type { ProfileType } from '@hprof/core'

const C = {
  reset: '\x1b[0m',
  bold: '\x1b[1m',
  dim: '\x1b[2m',
  cyan: '\x1b[36m',
  yellow: '\x1b[33m',
  green: '\x1b[32m',
  red: '\x1b[31m',
  magenta: '\x1b[35m',
  blue: '\x1b[34m',
  gray: '\x1b[90m',
}

function color(code: string, s: string) {
  return USE_COLOR ? `${code}${s}${C.reset}` : s
}

function bold(s: string) {
  return color(C.bold, s)
}
function cyan(s: string) {
  return color(C.cyan, s)
}
function yellow(s: string) {
  return color(C.yellow, s)
}
function green(s: string) {
  return color(C.green, s)
}
function red(s: string) {
  return color(C.red, s)
}
function dim(s: string) {
  return color(C.dim, s)
}
function magenta(s: string) {
  return color(C.magenta, s)
}
function gray(s: string) {
  return color(C.gray, s)
}

const ANSI_RE_SOURCE = '\\u001B\\[[0-9;]*m'
const ANSI_RE = new RegExp(ANSI_RE_SOURCE, 'g')
const CONTROL_RE = new RegExp(
  `[${String.fromCharCode(0)}-${String.fromCharCode(31)}${String.fromCharCode(127)}]+`,
  'g',
)

function stripAnsi(s: string) {
  return s.replace(ANSI_RE, '')
}

function normalizeCell(s: string) {
  return s.replace(ANSI_RE, '').replace(CONTROL_RE, ' ').replace(/\s+/g, ' ').trim()
}

function applyCellStyle(source: string, text: string) {
  if (!USE_COLOR) return text
  const match = source.match(
    new RegExp(`^((?:${ANSI_RE_SOURCE})+)([\\s\\S]*?)((?:${ANSI_RE_SOURCE})+)$`),
  )
  if (!match) return text
  return `${match[1]}${text}${match[3]}`
}

function truncateVisible(source: string, width: number) {
  const plain = normalizeCell(source)
  if (plain.length <= width) return plain
  if (width <= 1) return '…'
  return `${plain.slice(0, width - 1)}…`
}

function padVisible(s: string, width: number) {
  return s + ' '.repeat(Math.max(0, width - stripAnsi(s).length))
}

function progressBar(pct: number, phase: string, width = 30) {
  const filled = Math.round((pct / 100) * width)
  const bar = '█'.repeat(filled) + '░'.repeat(width - filled)
  process.stderr.write(`\r  ${dim(phase.padEnd(10))} ${bar} ${pct}%`)
  if (pct >= 100) process.stderr.write('\n')
}

function printUsage() {
  console.log(`
 ${bold('Usage:')} hprof <command> [options] <file>

 ${bold('Commands:')}
   ${cyan('analyze')}   Analyze profile file and print summary to stdout (default)
   ${cyan('ui')}        Start web UI server for interactive analysis
   ${cyan('help')}      Show this help message

 ${bold('Options:')}
   ${yellow('--top <n>')}       Number of top entries to show (default: 30)
   ${yellow('--filter <re>')}   Filter results by regex
   ${yellow('--json')}          Output as JSON
   ${yellow('--port <port>')}   Port for UI server (default: 3000)
   ${yellow('--open')}          Open browser automatically (ui command only)

 ${bold('Supported formats:')}
   ${green('.heapsnapshot')}   V8 heap snapshot
   ${green('.heapprofile')}    V8 sampling heap profile
   ${green('.heaptimeline')}   V8 heap allocation timeline
`)
}

interface CliArgs {
  command: string
  files: string[]
  top: number
  filter: string | null
  port: number
  open: boolean
  json: boolean
}

function parseArgs(argv: string[]): CliArgs {
  const args: CliArgs = {
    command: 'analyze',
    files: [],
    top: 30,
    filter: null,
    port: 3000,
    open: false,
    json: false,
  }

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]!
    if (arg === '--top') {
      args.top = Number(argv[++i] ?? args.top)
    } else if (arg === '--filter') {
      args.filter = argv[++i] ?? null
    } else if (arg === '--port') {
      args.port = Number(argv[++i] ?? args.port)
    } else if (arg === '--open') {
      args.open = true
    } else if (arg === '--json') {
      args.json = true
    } else if (arg === 'analyze' || arg === 'ui' || arg === 'help') {
      args.command = arg
    } else if (!arg.startsWith('-')) {
      args.files.push(arg)
    }
  }

  return args
}

function printTable(headers: string[], rows: string[][]) {
  const normalizedRows = rows.map((row) => row.map((cell) => normalizeCell(cell)))
  const widths = headers.map((h, i) =>
    Math.max(h.length, ...normalizedRows.map((r) => stripAnsi(r[i] ?? '').length)),
  )

  if (widths.length > 0) {
    const terminalWidth = Math.max(process.stdout.columns ?? 120, 60)
    const paddingWidth = 2 * (widths.length - 1)
    const fixedColumnsWidth = widths.slice(0, -1).reduce((sum, width) => sum + width, 0)
    const lastColumnMinWidth = headers[widths.length - 1]!.length
    const lastColumnMaxWidth = Math.max(
      lastColumnMinWidth,
      terminalWidth - 2 - paddingWidth - fixedColumnsWidth,
    )

    widths[widths.length - 1] = Math.min(widths[widths.length - 1]!, lastColumnMaxWidth)
  }

  const headerLine = headers.map((h, i) => h.padEnd(widths[i]!)).join('  ')
  console.log(`  ${dim(headerLine)}`)
  console.log(
    `  ${dim('─'.repeat(widths.reduce((sum, width) => sum + width, 0) + 2 * (widths.length - 1)))}`,
  )

  for (const [rowIndex, row] of rows.entries()) {
    const line = row
      .map((cell, i) => {
        if (i === row.length - 1) {
          const truncated = truncateVisible(cell, widths[i]!)
          return padVisible(applyCellStyle(cell, truncated), widths[i]!)
        }

        const normalized = normalizedRows[rowIndex]![i]!
        if (normalized === stripAnsi(cell)) {
          return padVisible(cell, widths[i]!)
        }

        return padVisible(normalized, widths[i]!)
      })
      .join('  ')
    console.log(`  ${line}`)
  }

  console.log()
}

function printHeader(title: string, subtitle?: string) {
  console.log()
  console.log(`  ${bold(cyan(title))}`)
  if (subtitle) console.log(`  ${dim(subtitle)}`)
}

function analyzeHeapProfile(filePath: string, args: CliArgs) {
  const profile = new HeapProfile(filePath)
  const summary = profile.summarize({
    top: args.top,
    filter: args.filter ?? undefined,
  })

  if (args.json) {
    console.log(
      JSON.stringify(
        {
          file: filePath,
          type: 'heapprofile' as ProfileType,
          totalSize: summary.totalSize,
          byFrame: [...summary.byFrame.entries()],
          byUrl: [...summary.byUrl.entries()],
          byFunction: [...summary.byFunction.entries()],
        },
        null,
        2,
      ),
    )
    return
  }

  const toRows = (map: Map<string, number>) =>
    [...map.entries()].sort((a, b) => b[1] - a[1]).slice(0, args.top)

  printHeader(filePath, `heapprofile | total sampled: ${yellow(formatBytes(summary.totalSize))}`)

  const frameRows = toRows(summary.byFrame)
  printTable(
    ['SIZE', 'FRAME'],
    frameRows.map(([key, size]) => [green(formatBytes(size)), key]),
  )

  const urlRows = toRows(summary.byUrl)
  printTable(
    ['SIZE', 'URL'],
    urlRows.map(([key, size]) => [green(formatBytes(size)), gray(key)]),
  )

  const fnRows = toRows(summary.byFunction)
  printTable(
    ['SIZE', 'FUNCTION'],
    fnRows.map(([key, size]) => [green(formatBytes(size)), magenta(key)]),
  )
}

async function analyzeHeapSnapshot(filePath: string, args: CliArgs) {
  const snapshot = new HeapSnapshot(filePath)
  const meta = snapshot.meta
  const summary = await snapshot.streamSummary({
    top: args.top,
    filter: args.filter ?? undefined,
    onProgress: args.json ? undefined : (phase, pct) => progressBar(pct, phase),
  })

  if (args.json) {
    console.log(
      JSON.stringify(
        {
          file: filePath,
          type: 'heapsnapshot' as ProfileType,
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
        },
        null,
        2,
      ),
    )
    return
  }

  printHeader(
    filePath,
    [
      `heapsnapshot`,
      `nodes: ${bold(meta.node_count.toLocaleString())}`,
      `edges: ${bold(meta.edge_count.toLocaleString())}`,
      `total self size: ${yellow(formatBytes(summary.totalSize))}`,
    ].join(' | '),
  )

  const nameRows = [...summary.byNodeName.entries()].slice(0, args.top)
  printTable(
    ['SIZE', 'COUNT', 'NAME'],
    nameRows.map(([name, info]) => [green(formatBytes(info.size)), dim(String(info.count)), name]),
  )

  const typeRows = [...summary.byNodeType.entries()]
    .sort((a, b) => b[1].size - a[1].size)
    .slice(0, args.top)
  printTable(
    ['SIZE', 'COUNT', 'TYPE'],
    typeRows.map(([type, info]) => [
      green(formatBytes(info.size)),
      dim(String(info.count)),
      magenta(type),
    ]),
  )
}

async function analyzeHeapTimeline(filePath: string, args: CliArgs) {
  const timeline = new HeapTimeline(filePath)
  const meta = timeline.meta
  const summary = await timeline.streamSummary({
    top: args.top,
    filter: args.filter ?? undefined,
    onProgress: args.json ? undefined : (phase, pct) => progressBar(pct, phase),
  })

  if (args.json) {
    console.log(
      JSON.stringify(
        {
          file: filePath,
          type: 'heaptimeline' as ProfileType,
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
        },
        null,
        2,
      ),
    )
    return
  }

  printHeader(
    filePath,
    [
      `heaptimeline`,
      `nodes: ${bold(meta.node_count.toLocaleString())}`,
      `total allocated: ${yellow(formatBytes(summary.totalAllocated))}`,
    ].join(' | '),
  )

  const typeRows = [...summary.byType.entries()].slice(0, args.top)
  printTable(
    ['ALLOCATED', 'COUNT', 'TYPE'],
    typeRows.map(([type, info]) => [
      green(formatBytes(info.allocated)),
      dim(String(info.count)),
      magenta(type),
    ]),
  )
}

async function main() {
  const args = parseArgs(process.argv.slice(2))

  if (args.command === 'help' || (args.files.length === 0 && args.command !== 'help')) {
    printUsage()
    process.exit(args.command === 'help' ? 0 : 1)
  }

  if (args.command === 'ui') {
    const { startServer } = await import('@hprof/ui')
    await startServer({
      files: args.files,
      port: args.port,
      open: args.open,
    })
    return
  }

  if (args.command === 'analyze') {
    for (const file of args.files) {
      const type = detectProfileType(file)
      try {
        switch (type) {
          case 'heapprofile':
            analyzeHeapProfile(file, args)
            break
          case 'heapsnapshot':
            await analyzeHeapSnapshot(file, args)
            break
          case 'heaptimeline':
            await analyzeHeapTimeline(file, args)
            break
        }
      } catch (err) {
        console.error(`  ${red('Error:')} ${(err as Error).message}`)
      }
    }
  }
}

main().catch((err) => {
  console.error(`${red('Error:')} ${err.message}`)
  process.exit(1)
})
