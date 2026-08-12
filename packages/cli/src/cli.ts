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
    ${cyan('diff')}      Compare two profiles of the same type (baseline <profile>)
    ${cyan('dot')}       Emit call graph as DOT for use with graphviz
    ${cyan('list')}      List sampled locations grouped by file:line (heapprofile)
    ${cyan('ui')}        Start web UI server for interactive analysis
    ${cyan('help')}      Show this help message

 ${bold('Options:')}
   ${yellow('--top <n>')}       Number of top entries to show (default: 30)
   ${yellow('--filter <re>')}   Filter results by regex (timeline: names + stacks)
   ${yellow('--focus <re>')}    pprof-style focus: only frames matching contribute
   ${yellow('--ignore <re>')}   pprof-style ignore: drop flat attribution for matches
   ${yellow('--hide <re>')}     pprof-style hide: drop matching frames from visualisations
   ${yellow('--cum')}           Show cumulative (self + descendants) instead of flat only
   ${yellow('--json')}          Output as JSON
   ${yellow('--port <port>')}   Port for UI server (default: 3000)
   ${yellow('--open')}          Open browser automatically (ui command only)

 ${bold('Heap timeline analysis:')}
   analyze on a .heaptimeline prints, in addition to the by-type summary:
     - top allocation names with per-type split
     - top allocation sites as stack traces (leaf <- caller)
     - object-growth profile over the recording
   ${gray('--filter Vector3')} narrows names and stacks to matching entries.

 ${bold('Dot output:')}
   Pipe to graphviz to render a graph. Examples:
     ${gray('hprof dot file.heapprofile | dot -Tsvg -o graph.svg')}
     ${gray('hprof dot file.heapprofile | dot -Tpng -o graph.png')}

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
  focus: string | null
  ignore: string | null
  hide: string | null
  cum: boolean
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
    focus: null,
    ignore: null,
    hide: null,
    cum: false,
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
    } else if (arg === '--focus') {
      args.focus = argv[++i] ?? null
    } else if (arg === '--ignore') {
      args.ignore = argv[++i] ?? null
    } else if (arg === '--hide') {
      args.hide = argv[++i] ?? null
    } else if (arg === '--cum') {
      args.cum = true
    } else if (arg === '--port') {
      args.port = Number(argv[++i] ?? args.port)
    } else if (arg === '--open') {
      args.open = true
    } else if (arg === '--json') {
      args.json = true
    } else if (
      arg === 'analyze' ||
      arg === 'ui' ||
      arg === 'help' ||
      arg === 'bench' ||
      arg === 'diff' ||
      arg === 'dot' ||
      arg === 'list'
    ) {
      args.command = arg
    } else if (!arg.startsWith('-')) {
      args.files.push(arg)
    }
  }

  return args
}

function pct(value: number, total: number): string {
  if (total <= 0) return '0.00%'
  return `${((value / total) * 100).toFixed(2)}%`
}

function formatDelta(delta: number): string {
  if (delta === 0) return gray('±0 B')
  const abs = Math.abs(delta)
  const formatted = formatBytes(abs)
  return delta > 0 ? red(`+${formatted}`) : green(`-${formatted}`)
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms.toFixed(0)}ms`
  const s = ms / 1000
  if (s < 60) return `${s.toFixed(1)}s`
  return `${Math.floor(s / 60)}m${(s % 60).toFixed(0)}s`
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

  // Use cumulative when --cum or any pprof-style filter is set.
  const useCum = args.cum || args.focus !== null || args.ignore !== null

  if (useCum) {
    const summary = profile.summarizeCumulative({
      top: args.top,
      focus: args.focus ?? undefined,
      ignore: args.ignore ?? undefined,
      hide: args.hide ?? undefined,
    })

    if (args.json) {
      console.log(
        JSON.stringify(
          {
            file: filePath,
            type: 'heapprofile' as ProfileType,
            totalSize: summary.totalSize,
            byFrame: [...summary.byFrame.entries()].map(([name, e]) => ({
              name,
              selfSize: e.selfSize,
              cumulativeSize: e.cumulativeSize,
              selfPct: e.selfPct,
              cumulativePct: e.cumulativePct,
              count: e.count,
            })),
            byUrl: [...summary.byUrl.entries()].map(([name, e]) => ({
              name,
              selfSize: e.selfSize,
              cumulativeSize: e.cumulativeSize,
            })),
            byFunction: [...summary.byFunction.entries()].map(([name, e]) => ({
              name,
              selfSize: e.selfSize,
              cumulativeSize: e.cumulativeSize,
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
      `heapprofile | total: ${yellow(formatBytes(summary.totalSize))} | ${bold('cumulative')} mode`,
    )

    const sortByCum = (
      a: [string, { cumulativeSize: number }],
      b: [string, { cumulativeSize: number }],
    ) => b[1].cumulativeSize - a[1].cumulativeSize

    const frameRows = [...summary.byFrame.entries()].sort(sortByCum).slice(0, args.top)
    printTable(
      ['SELF', 'SELF%', 'CUM', 'CUM%', 'FRAME'],
      frameRows.map(([name, e]) => [
        e.selfSize > 0 ? green(formatBytes(e.selfSize)) : dim('0 B'),
        dim(pct(e.selfSize, summary.totalSize)),
        yellow(formatBytes(e.cumulativeSize)),
        dim(pct(e.cumulativeSize, summary.totalSize)),
        name,
      ]),
    )

    const fnRows = [...summary.byFunction.entries()].sort(sortByCum).slice(0, args.top)
    printTable(
      ['SELF', 'SELF%', 'CUM', 'CUM%', 'FUNCTION'],
      fnRows.map(([name, e]) => [
        e.selfSize > 0 ? green(formatBytes(e.selfSize)) : dim('0 B'),
        dim(pct(e.selfSize, summary.totalSize)),
        yellow(formatBytes(e.cumulativeSize)),
        dim(pct(e.cumulativeSize, summary.totalSize)),
        magenta(name),
      ]),
    )
    return
  }

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

  printHeader(filePath, `heapprofile | total sampled: ${yellow(formatBytes(summary.totalSize))}`)

  const toRows = (map: Map<string, number>) =>
    [...map.entries()].sort((a, b) => b[1] - a[1]).slice(0, args.top)

  const frameRows = toRows(summary.byFrame)
  printTable(
    ['SIZE', '%', 'FRAME'],
    frameRows.map(([key, size]) => [
      green(formatBytes(size)),
      dim(pct(size, summary.totalSize)),
      key,
    ]),
  )

  const urlRows = toRows(summary.byUrl)
  printTable(
    ['SIZE', '%', 'URL'],
    urlRows.map(([key, size]) => [
      green(formatBytes(size)),
      dim(pct(size, summary.totalSize)),
      gray(key),
    ]),
  )

  const fnRows = toRows(summary.byFunction)
  printTable(
    ['SIZE', '%', 'FUNCTION'],
    fnRows.map(([key, size]) => [
      green(formatBytes(size)),
      dim(pct(size, summary.totalSize)),
      magenta(key),
    ]),
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
    ['SIZE', '%', 'COUNT', 'NAME'],
    nameRows.map(([name, info]) => [
      green(formatBytes(info.size)),
      dim(pct(info.size, summary.totalSize)),
      dim(String(info.count)),
      name,
    ]),
  )

  const typeRows = [...summary.byNodeType.entries()]
    .sort((a, b) => b[1].size - a[1].size)
    .slice(0, args.top)
  printTable(
    ['SIZE', '%', 'COUNT', 'TYPE'],
    typeRows.map(([type, info]) => [
      green(formatBytes(info.size)),
      dim(pct(info.size, summary.totalSize)),
      dim(String(info.count)),
      magenta(type),
    ]),
  )
}

function formatStack(stack: { name: string }[]): string {
  return stack
    .map((f) => f.name)
    .filter((n) => n !== '(root)' && n !== '')
    .join(' <- ')
}

async function analyzeHeapTimeline(filePath: string, args: CliArgs) {
  const timeline = new HeapTimeline(filePath)
  const meta = timeline.meta

  if (!args.json) {
    process.stderr.write(`  ${dim('parsing…')}\r`)
  }
  const [summary, names, stacks, growth] = await Promise.all([
    timeline.streamSummary({ top: args.top, filter: args.filter ?? undefined }),
    timeline.topNames({ top: args.top, filter: args.filter ?? undefined }),
    timeline.topStacks({ top: args.top, filter: args.filter ?? undefined }),
    timeline.growth(),
  ])
  if (!args.json) {
    process.stderr.write('\r\x1b[K')
  }

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
          names: names.entries.map((e) => ({
            name: e.name,
            size: e.size,
            count: e.count,
            types: e.types.map((t) => ({ type: t.name, size: t.size, count: t.count })),
          })),
          stacks: stacks.entries.map((e) => ({
            size: e.size,
            count: e.count,
            stack: e.stack.map((f) => ({
              name: f.name,
              script: f.script,
              line: f.line,
              column: f.column,
            })),
          })),
          growth: {
            spanUs: growth.spanUs,
            objectsStart: growth.objectsStart,
            objectsEnd: growth.objectsEnd,
            samples: growth.samples,
          },
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
      `recording: ${bold(formatDuration(growth.spanUs / 1000))}`,
    ].join(' | '),
  )

  // ---- growth / time profile ----
  if (growth.samples.length > 1) {
    const maxRate = Math.max(
      ...growth.samples.slice(1).map((s, i) => {
        const [t0, o0] = growth.samples[i]
        const dt = (s[0] - t0) / 1e6
        return dt > 0 ? (s[1] - o0) / dt : 0
      }),
      1,
    )
    const bar = (rate: number) => dim('|') + cyan('#'.repeat(Math.round((rate / maxRate) * 24)))
    const timeLine: string[] = []
    for (let i = 1; i < growth.samples.length; i++) {
      const [t0, o0] = growth.samples[i - 1]
      const [t1, o1] = growth.samples[i]
      const dt = (t1 - t0) / 1e6
      const rate = dt > 0 ? (o1 - o0) / dt : 0
      timeLine.push(bar(rate))
    }
    printHeader(
      'Objects allocated over time',
      `+${(growth.objectsEnd - growth.objectsStart).toLocaleString()} objects in ${formatDuration(growth.spanUs / 1000)}`,
    )
    console.log('  ' + timeLine.join(''))
    console.log(
      `  ${dim('0s')}${' '.repeat(30)}${dim('end')} (density = objects/s, peaks are game-creation phases)`,
    )
  }

  // ---- by type ----
  printHeader('By type')
  const typeRows = [...summary.byType.entries()].slice(0, args.top)
  printTable(
    ['ALLOCATED', '%', 'COUNT', 'TYPE'],
    typeRows.map(([type, info]) => [
      green(formatBytes(info.allocated)),
      dim(pct(info.allocated, summary.totalAllocated)),
      dim(String(info.count)),
      magenta(type),
    ]),
  )

  // ---- top names ----
  printHeader('Top allocations by name', `of ${formatBytes(names.totalSize)} total`)
  const nameRows = names.entries.map((e) => {
    const typeStr = e.types.map((t) => `${t.name} ${pct(t.size, e.size)}`).join(' · ')
    return [
      green(formatBytes(e.size)),
      dim(pct(e.size, names.totalSize)),
      dim(String(e.count)),
      e.name,
      dim(typeStr !== e.name ? typeStr : ''),
    ]
  })
  printTable(['ALLOCATED', '%', 'COUNT', 'NAME', 'BY TYPE'], nameRows)

  // ---- top stacks ----
  if (stacks.entries.length > 0) {
    printHeader(
      'Top allocation sites (stack traces)',
      `${stacks.entries.length} sites · ${formatBytes(stacks.totalSize)} tracked`,
    )
    const stackRows = stacks.entries.map((e) => [
      green(formatBytes(e.size)),
      dim(String(e.count)),
      formatStack(e.stack),
    ])
    printTable(['SIZE', 'COUNT', 'STACK (leaf <- caller)'], stackRows)
  }
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

  if (args.command === 'dot') {
    for (const file of args.files) {
      try {
        const type = detectProfileType(file)
        if (type !== 'heapprofile') {
          console.error(
            `  ${red('Error:')} dot output is currently only supported for .heapprofile files`,
          )
          continue
        }
        const profile = new HeapProfile(file)
        const dot = profile.dot({
          top: args.top,
          focus: args.focus ?? undefined,
          ignore: args.ignore ?? undefined,
          hide: args.hide ?? undefined,
        })
        process.stdout.write(dot)
      } catch (err) {
        console.error(`  ${red('Error:')} ${(err as Error).message}`)
      }
    }
    return
  }

  if (args.command === 'list') {
    for (const file of args.files) {
      try {
        const type = detectProfileType(file)
        if (type !== 'heapprofile') {
          console.error(`  ${red('Error:')} list is only supported for .heapprofile files`)
          continue
        }
        runList(file, args)
      } catch (err) {
        console.error(`  ${red('Error:')} ${(err as Error).message}`)
      }
    }
    return
  }

  if (args.command === 'diff') {
    if (args.files.length < 2) {
      console.error(`  ${red('Error:')} diff requires two files: <baseline> <profile>`)
      process.exit(1)
    }
    const [baselinePath, profilePath] = args.files
    if (!baselinePath || !profilePath) {
      console.error(`  ${red('Error:')} diff requires two files`)
      process.exit(1)
    }
    try {
      const baseType = detectProfileType(baselinePath!)
      const profType = detectProfileType(profilePath!)
      if (baseType !== profType) {
        console.error(
          `  ${red('Error:')} cannot diff ${baseType} with ${profType} — types must match`,
        )
        process.exit(1)
      }
      await runDiff(baselinePath!, profilePath!, baseType, args)
    } catch (err) {
      console.error(`  ${red('Error:')} ${(err as Error).message}`)
      process.exit(1)
    }
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

async function runDiff(
  baselinePath: string,
  profilePath: string,
  type: ProfileType,
  args: CliArgs,
) {
  if (type === 'heapprofile') {
    const baseline = new HeapProfile(baselinePath)
    const profile = new HeapProfile(profilePath)
    const d = profile.diff(baseline)

    if (args.json) {
      console.log(
        JSON.stringify(
          {
            baseline: baselinePath,
            profile: profilePath,
            type: 'heapprofile' as ProfileType,
            baselineTotal: d.baselineTotal,
            profileTotal: d.profileTotal,
            deltaTotal: d.deltaTotal,
            byFrame: d.byFrame,
            byUrl: d.byUrl,
            byFunction: d.byFunction,
          },
          null,
          2,
        ),
      )
      return
    }

    printHeader(
      `${profilePath} vs ${baselinePath}`,
      `diff | baseline: ${formatBytes(d.baselineTotal)} → profile: ${yellow(formatBytes(d.profileTotal))} | delta: ${formatDelta(d.deltaTotal)}`,
    )

    printDiffTable('FUNCTION DELTA', d.byFunction, args.top)
    printDiffTable('FRAME DELTA', d.byFrame, args.top)
    return
  }

  if (type === 'heapsnapshot') {
    const baseline = new HeapSnapshot(baselinePath)
    const profile = new HeapSnapshot(profilePath)
    const d = await profile.diff(baseline)

    if (args.json) {
      console.log(
        JSON.stringify(
          {
            baseline: baselinePath,
            profile: profilePath,
            type: 'heapsnapshot' as ProfileType,
            baselineTotal: d.baselineTotal,
            profileTotal: d.profileTotal,
            deltaTotal: d.deltaTotal,
            byNodeName: d.byNodeName,
            byNodeType: d.byNodeType,
          },
          null,
          2,
        ),
      )
      return
    }

    printHeader(
      `${profilePath} vs ${baselinePath}`,
      `diff | baseline: ${formatBytes(d.baselineTotal)} → profile: ${yellow(formatBytes(d.profileTotal))} | delta: ${formatDelta(d.deltaTotal)}`,
    )

    printDiffTable('NODE NAME DELTA', d.byNodeName, args.top)
    printDiffTable('NODE TYPE DELTA', d.byNodeType, args.top)
    return
  }

  console.error(`  ${red('Error:')} diff is not implemented for heaptimeline files`)
  process.exit(1)
}

function printDiffTable(
  title: string,
  entries: {
    name: string
    delta: number
    deltaPct: number | null
    baselineSize: number
    profileSize: number
  }[],
  top: number,
) {
  if (!entries.length) return
  printHeader(title)
  printTable(
    ['BASELINE', 'PROFILE', 'DELTA', '%', 'NAME'],
    entries
      .slice(0, top)
      .map((e) => [
        dim(formatBytes(e.baselineSize)),
        formatBytes(e.profileSize),
        formatDelta(e.delta),
        e.deltaPct === null
          ? dim('new')
          : e.deltaPct > 0
            ? red(`+${(e.deltaPct * 100).toFixed(1)}%`)
            : green(`${(e.deltaPct * 100).toFixed(1)}%`),
        e.name,
      ]),
  )
}

function runList(filePath: string, args: CliArgs) {
  const profile = new HeapProfile(filePath)
  const frames = profile.flatten()

  // Group by url.
  const byUrl = new Map<
    string,
    { size: number; lines: Map<number, { size: number; count: number; fns: Set<string> }> }
  >()
  let total = 0
  const filterRe = args.filter ? new RegExp(args.filter, 'i') : null
  for (const frame of frames) {
    if (filterRe && !filterRe.test(frame.url) && !filterRe.test(frame.functionName)) continue
    let g = byUrl.get(frame.url)
    if (!g) {
      g = { size: 0, lines: new Map() }
      byUrl.set(frame.url, g)
    }
    g.size += frame.selfSize
    total += frame.selfSize
    let line = g.lines.get(frame.lineNumber)
    if (!line) {
      line = { size: 0, count: 0, fns: new Set() }
      g.lines.set(frame.lineNumber, line)
    }
    line.size += frame.selfSize
    line.count += 1
    line.fns.add(frame.functionName)
  }

  if (args.json) {
    console.log(
      JSON.stringify(
        {
          file: filePath,
          type: 'heapprofile' as ProfileType,
          totalSize: total,
          byUrl: [...byUrl.entries()]
            .sort((a, b) => b[1].size - a[1].size)
            .slice(0, args.top)
            .map(([url, g]) => ({
              url,
              size: g.size,
              lines: [...g.lines.entries()]
                .sort((a, b) => b[1].size - a[1].size)
                .map(([lineNumber, info]) => ({
                  lineNumber: lineNumber + 1,
                  size: info.size,
                  count: info.count,
                  functions: [...info.fns],
                })),
            })),
        },
        null,
        2,
      ),
    )
    return
  }

  printHeader(filePath, `heapprofile | total: ${yellow(formatBytes(total))} | ${bold('list')} mode`)

  const sortedUrls = [...byUrl.entries()].sort((a, b) => b[1].size - a[1].size).slice(0, args.top)
  for (const [url, g] of sortedUrls) {
    console.log()
    console.log(`  ${bold(cyan(url))} ${dim(`(${pct(g.size, total)})`)}`)
    const sortedLines = [...g.lines.entries()].sort((a, b) => b[1].size - a[1].size).slice(0, 10)
    printTable(
      ['SIZE', '%', '×', 'LINE', 'FUNCTION'],
      sortedLines.map(([lineNumber, info]) => [
        green(formatBytes(info.size)),
        dim(pct(info.size, total)),
        dim(String(info.count)),
        yellow(`:${lineNumber + 1}`),
        magenta([...info.fns].slice(0, 2).join(', ')),
      ]),
    )
  }
}

main().catch((err) => {
  console.error(`${red('Error:')} ${err.message}`)
  process.exit(1)
})
