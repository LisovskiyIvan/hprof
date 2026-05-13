#!/usr/bin/env bun
import { existsSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import {
  detectProfileType,
  formatBytes,
  HeapProfile,
  HeapSnapshot,
  HeapTimeline,
} from '@hprof/core'

const __dirname = dirname(fileURLToPath(import.meta.url))
const RESULTS_DIR = join(__dirname, 'results')
const RESULTS_FILE = join(RESULTS_DIR, 'bench.json')

const WARMUP = parseInt(process.env.WARMUP ?? '1')
const ITER = parseInt(process.env.ITER ?? '3')
const REGRESSION_THRESHOLD = parseFloat(process.env.THRESHOLD ?? '1.3')

interface BenchPhase {
  name: string
  fn: () => Promise<void> | void
}

interface PhaseResult {
  name: string
  avg: number
  min: number
  max: number
  runs: number[]
}

interface FileResult {
  file: string
  type: string
  phases: PhaseResult[]
}

interface BenchRecord {
  timestamp: string
  commit: string
  files: FileResult[]
}

function getPhases(filePath: string, type: string, top: number): BenchPhase[] {
  switch (type) {
    case 'heapsnapshot':
      return [
        {
          name: 'meta',
          fn: () => {
            new HeapSnapshot(filePath).meta
          },
        },
        {
          name: 'summary',
          fn: async () => {
            const s = new HeapSnapshot(filePath)
            s.meta
            await s.streamSummary({ top })
          },
        },
        {
          name: 'node_page',
          fn: async () => {
            const s = new HeapSnapshot(filePath)
            s.meta
            await s.getNodePage({ page: 0, pageSize: 100 })
          },
        },
        {
          name: 'edges',
          fn: async () => {
            const s = new HeapSnapshot(filePath)
            s.meta
            await s.getNodeEdges(0)
          },
        },
        {
          name: 'search',
          fn: async () => {
            const s = new HeapSnapshot(filePath)
            s.meta
            await s.searchStrings('Object')
          },
        },
        {
          name: 'retained',
          fn: async () => {
            const s = new HeapSnapshot(filePath)
            s.meta
            await s.getRetainedEntries(30)
          },
        },
      ]
    case 'heapprofile':
      return [
        {
          name: 'summarize',
          fn: () => {
            new HeapProfile(filePath).summarize({ top })
          },
        },
        {
          name: 'flatten',
          fn: () => {
            new HeapProfile(filePath).flatten()
          },
        },
      ]
    case 'heaptimeline':
      return [
        {
          name: 'meta',
          fn: () => {
            new HeapTimeline(filePath).meta
          },
        },
        {
          name: 'summary',
          fn: async () => {
            const t = new HeapTimeline(filePath)
            t.meta
            await t.streamSummary({ top })
          },
        },
      ]
    default:
      return []
  }
}

async function benchFile(filePath: string, top: number): Promise<FileResult> {
  const type = detectProfileType(filePath)
  const phases = getPhases(filePath, type, top)
  const results: PhaseResult[] = []

  const label = filePath.split('/').pop()!

  for (const phase of phases) {
    for (let i = 0; i < WARMUP; i++) {
      await phase.fn()
    }

    const runs: number[] = []
    for (let i = 0; i < ITER; i++) {
      const start = performance.now()
      await phase.fn()
      runs.push(performance.now() - start)
    }

    const avg = runs.reduce((a, b) => a + b, 0) / runs.length
    results.push({
      name: phase.name,
      avg,
      min: Math.min(...runs),
      max: Math.max(...runs),
      runs,
    })

    const runsStr = runs.map((t) => `${t.toFixed(1)}ms`).join(' ')
    console.log(`  ${phase.name.padEnd(14)} ${avg.toFixed(1).padStart(8)}ms  [${runsStr}]`)
  }

  return { file: label, type, phases: results }
}

function loadHistory(): BenchRecord[] {
  if (!existsSync(RESULTS_FILE)) return []
  try {
    return JSON.parse(readFileSync(RESULTS_FILE, 'utf-8'))
  } catch {
    return []
  }
}

function saveRecord(record: BenchRecord) {
  mkdirSync(RESULTS_DIR, { recursive: true })
  const history = loadHistory()
  history.push(record)
  writeFileSync(RESULTS_FILE, JSON.stringify(history, null, 2))
}

function getCommit(): string {
  try {
    const { execSync } = require('child_process')
    return execSync('git rev-parse --short HEAD', { encoding: 'utf-8' }).trim()
  } catch {
    return 'unknown'
  }
}

function checkRegressions(current: FileResult[], previous: FileResult[] | undefined): boolean {
  if (!previous) return false

  let hasRegression = false

  for (const cur of current) {
    const prev = previous.find((p) => p.file === cur.file && p.type === cur.type)
    if (!prev) continue

    for (const curPhase of cur.phases) {
      const prevPhase = prev.phases.find((p) => p.name === curPhase.name)
      if (!prevPhase) continue

      const ratio = curPhase.avg / prevPhase.avg
      if (ratio > REGRESSION_THRESHOLD) {
        console.log(
          `  \x1b[31mREGRESSION\x1b[0m ${cur.file} ${curPhase.name}: ${curPhase.avg.toFixed(1)}ms vs ${prevPhase.avg.toFixed(1)}ms (\x1b[31m${ratio.toFixed(2)}x\x1b[0m)`,
        )
        hasRegression = true
      } else if (ratio < 1 / REGRESSION_THRESHOLD) {
        console.log(
          `  \x1b[32mIMPROVED\x1b[0m   ${cur.file} ${curPhase.name}: ${curPhase.avg.toFixed(1)}ms vs ${prevPhase.avg.toFixed(1)}ms (\x1b[32m${ratio.toFixed(2)}x\x1b[0m)`,
        )
      }
    }
  }

  return hasRegression
}

async function main() {
  const command = process.argv[2]

  if (command === 'show') {
    const history = loadHistory()
    if (history.length === 0) {
      console.log('No bench results yet. Run `bun bench/run.ts` first.')
      return
    }

    const last = history[history.length - 1]!
    console.log(`\n  Latest: ${last.timestamp} (${last.commit})\n`)

    for (const file of last.files) {
      console.log(`  ${file.file} (${file.type})`)
      for (const phase of file.phases) {
        console.log(
          `    ${phase.name.padEnd(14)} ${phase.avg.toFixed(1).padStart(8)}ms  min ${phase.min.toFixed(1)} max ${phase.max.toFixed(1)}`,
        )
      }
      console.log()
    }
    return
  }

  if (command === 'diff') {
    const history = loadHistory()
    if (history.length < 2) {
      console.log('Need at least 2 runs to diff.')
      return
    }
    const prev = history[history.length - 2]!
    const cur = history[history.length - 1]!
    console.log(
      `\n  Comparing ${prev.timestamp} (${prev.commit}) → ${cur.timestamp} (${cur.commit})\n`,
    )
    const failed = checkRegressions(cur.files, prev.files)
    if (!failed) {
      console.log('  \x1b[32mNo regressions\x1b[0m\n')
    }
    process.exit(failed ? 1 : 0)
    return
  }

  if (command === 'history') {
    const history = loadHistory()
    if (history.length === 0) {
      console.log('No bench results yet.')
      return
    }
    for (const record of history) {
      const totalMs = record.files.flatMap((f) => f.phases).reduce((sum, p) => sum + p.avg, 0)
      console.log(
        `  ${record.timestamp}  ${record.commit}  total ${totalMs.toFixed(0)}ms  (${record.files.length} files)`,
      )
    }
    return
  }

  const files = [
    join(__dirname, '..', 'snapshots', 'Heap-20260508T151711.heapprofile'),
    join(__dirname, '..', 'snapshots', 'Heap-20260508T151658.heaptimeline'),
    join(__dirname, '..', 'snapshots', 'Heap-20260508T151623.heapsnapshot'),
  ].filter((f) => existsSync(f))

  if (files.length === 0) {
    console.error(
      'No snapshot files found in ./snapshots/. Place .heapsnapshot/.heapprofile/.heaptimeline files there.',
    )
    process.exit(1)
  }

  console.log(
    `\n  \x1b[1mhprof bench\x1b[0m  warmup=${WARMUP} iter=${ITER} threshold=${REGRESSION_THRESHOLD}x\n`,
  )

  const fileResults: FileResult[] = []
  for (const filePath of files) {
    const label = filePath.split('/').pop()!
    console.log(`  \x1b[1m${label}\x1b[0m`)
    const result = await benchFile(filePath, 30)
    fileResults.push(result)
    console.log()
  }

  const commit = getCommit()
  const history = loadHistory()
  const previous = history.length > 0 ? history[history.length - 1]!.files : undefined

  const record: BenchRecord = {
    timestamp: new Date().toISOString(),
    commit,
    files: fileResults,
  }

  saveRecord(record)

  console.log(`  Saved to ${RESULTS_FILE}`)

  if (previous) {
    console.log(
      `\n  \x1b[1mRegression check\x1b[0m (vs ${history[history.length - 1]!.timestamp})\n`,
    )
    const failed = checkRegressions(fileResults, previous)
    if (!failed) {
      console.log('  \x1b[32mNo regressions\x1b[0m\n')
    }
    if (failed) process.exit(1)
  }
}

main()
