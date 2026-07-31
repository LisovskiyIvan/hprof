import { describe, test, expect } from 'bun:test'
import { detectProfileType, formatBytes, HeapProfile } from '../src/index.ts'
import path from 'path'

const SNAPSHOTS = path.resolve(import.meta.dir, '../../../snapshots')

describe('detectProfileType', () => {
  test('detects heapsnapshot', () => {
    expect(detectProfileType('file.heapsnapshot')).toBe('heapsnapshot')
  })

  test('detects heapprofile', () => {
    expect(detectProfileType('file.heapprofile')).toBe('heapprofile')
  })

  test('detects heaptimeline', () => {
    expect(detectProfileType('file.heaptimeline')).toBe('heaptimeline')
  })

  test('throws for unknown extension', () => {
    expect(() => detectProfileType('file.json')).toThrow('Unsupported file type')
  })
})

describe('formatBytes', () => {
  test('formats bytes', () => {
    expect(formatBytes(0)).toBe('0.00 B')
  })

  test('formats KB', () => {
    expect(formatBytes(1024)).toBe('1.00 KB')
  })

  test('formats MB', () => {
    expect(formatBytes(1048576)).toBe('1.00 MB')
  })

  test('formats GB', () => {
    expect(formatBytes(1073741824)).toBe('1.00 GB')
  })
})

describe('HeapProfile', () => {
  const filePath = path.join(SNAPSHOTS, 'Heap-20260508T151711.heapprofile')
  let profile: HeapProfile

  test('getFullData parses the file', () => {
    profile = new HeapProfile(filePath)
    const result = profile.getFullData()
    expect(result.head).toBeDefined()
    expect(result.head.callFrame).toBeDefined()
    expect(result.head.children).toBeInstanceOf(Array)
    expect(typeof result.startTime === 'number' || result.startTime === undefined).toBe(true)
  })

  test('summarize aggregates by frame/url/function', () => {
    const summary = profile.summarize()
    expect(summary.totalSize).toBeGreaterThan(0)
    expect(summary.byFrame.size).toBeGreaterThan(0)
    expect(summary.byUrl.size).toBeGreaterThan(0)
    expect(summary.byFunction.size).toBeGreaterThan(0)
  })

  test('summarize respects --top', () => {
    const summary = profile.summarize({ top: 5 })
    expect(summary.byFrame.size).toBeLessThanOrEqual(5)
    expect(summary.byUrl.size).toBeLessThanOrEqual(5)
    expect(summary.byFunction.size).toBeLessThanOrEqual(5)
  })

  test('summarize respects --filter', () => {
    const summaryFiltered = profile.summarize({ filter: 'xyznonexistent' })
    expect(summaryFiltered.totalSize).toBe(0)
    expect(summaryFiltered.byFrame.size).toBe(0)
  })

  test('flatten returns flat array', () => {
    const flat = profile.flatten()
    expect(flat.length).toBeGreaterThan(0)
    expect(flat[0]).toHaveProperty('functionName')
    expect(flat[0]).toHaveProperty('selfSize')
    expect(flat[0]).toHaveProperty('stack')
    expect(flat[0]!.stack.length).toBeGreaterThan(0)
  })

  test('summarizeCumulative computes self and cumulative sizes', () => {
    const summary = profile.summarizeCumulative({ top: 5 })
    expect(summary.totalSize).toBeGreaterThan(0)
    expect(summary.byFrame.size).toBeGreaterThan(0)
    const firstEntry = [...summary.byFrame.values()][0]!
    expect(firstEntry.cumulativeSize).toBeGreaterThanOrEqual(firstEntry.selfSize)
    expect(firstEntry.cumulativePct).toBeGreaterThanOrEqual(firstEntry.selfPct)
  })

  test('summarizeCumulative respects focus filter', () => {
    const baseline = profile.summarizeCumulative()
    const focused = profile.summarizeCumulative({ focus: 'render' })
    expect(focused.totalSize).toBeLessThanOrEqual(baseline.totalSize)
  })

  test('flamegraph produces a tree of frames', () => {
    const flame = profile.flamegraph()
    expect(flame.name).toBeDefined()
    expect(flame.totalSize).toBeGreaterThan(0)
    expect(Array.isArray(flame.children)).toBe(true)
  })

  test('dot emits a graphviz digraph', () => {
    const dot = profile.dot({ top: 5 })
    expect(dot).toContain('digraph')
    expect(dot).toContain('->')
  })

  test('treemap produces a hierarchical structure', () => {
    const tm = profile.treemap()
    expect(tm.name).toBeDefined()
    expect(tm.size).toBeGreaterThan(0)
    expect(Array.isArray(tm.children)).toBe(true)
  })

  test('diff against self produces zero delta', () => {
    const other = new HeapProfile(filePath)
    const d = profile.diff(other)
    expect(d.deltaTotal).toBe(0)
    expect(d.baselineTotal).toBe(d.profileTotal)
  })
})
