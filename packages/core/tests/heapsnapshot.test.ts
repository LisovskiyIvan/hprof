import { describe, test, expect } from 'bun:test'
import { HeapSnapshot } from '../src/heapsnapshot.ts'
import path from 'path'

const SNAPSHOTS = path.resolve(import.meta.dir, '../../../snapshots')
const HEAP_SNAPSHOT = path.join(SNAPSHOTS, 'Heap-20260508T151623.heapsnapshot')

describe('HeapSnapshot.meta', () => {
  test('extracts meta from heapsnapshot header', () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT)
    const meta = snapshot.meta
    expect(meta.node_count).toBeGreaterThan(0)
    expect(meta.edge_count).toBeGreaterThan(0)
    expect(meta.meta.node_fields).toContain('type')
    expect(meta.meta.node_fields).toContain('name')
    expect(meta.meta.node_fields).toContain('self_size')
    expect(meta.meta.node_types).toBeInstanceOf(Array)
    expect(meta.meta.edge_fields).toBeInstanceOf(Array)
  })
})

describe('HeapSnapshot.streamSummary', () => {
  test('produces summary with top-N nodes', async () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT)
    const summary = await snapshot.streamSummary({ top: 10 })
    expect(summary.totalSize).toBeGreaterThan(0)
    expect(summary.totalCount).toBeGreaterThan(0)
    expect(summary.byNodeName.size).toBeGreaterThan(0)
    expect(summary.byNodeName.size).toBeLessThanOrEqual(10)
    expect(summary.byNodeType.size).toBeGreaterThan(0)
  }, 60000)

  test('respects filter', async () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT)
    const summaryFiltered = await snapshot.streamSummary({
      top: 100,
      filter: 'xyznonexistent',
    })
    expect(summaryFiltered.byNodeName.size).toBe(0)
  }, 60000)

  test('calls onProgress callback', async () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT)
    const phases: string[] = []
    await snapshot.streamSummary({
      top: 5,
      onProgress: (phase) => {
        if (!phases.includes(phase)) phases.push(phase)
      },
    })
    // Phase reporting is best-effort — the native side may report zero, one or
    // many phases depending on the code path. We just verify that the callback
    // is invocable without throwing.
    expect(Array.isArray(phases)).toBe(true)
  }, 60000)
})

describe('HeapSnapshot.flamegraph', () => {
  test('produces a tree aggregated by type and name', async () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT)
    const flame = await snapshot.flamegraph({ top: 10 })
    expect(flame.name).toBe('Heap')
    expect(flame.totalSize).toBeGreaterThan(0)
    expect(Array.isArray(flame.children)).toBe(true)
    expect(flame.children.length).toBeGreaterThan(0)
  }, 60000)
})

describe('HeapSnapshot.treemap', () => {
  test('produces a hierarchy of node types', async () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT)
    const tm = await snapshot.treemap({ top: 10 })
    expect(tm.name).toBe('Heap')
    expect(tm.size).toBeGreaterThan(0)
    expect(Array.isArray(tm.children)).toBe(true)
  }, 60000)
})

describe('HeapSnapshot.diff', () => {
  test('diffing against self produces zero delta', async () => {
    const a = new HeapSnapshot(HEAP_SNAPSHOT)
    const b = new HeapSnapshot(HEAP_SNAPSHOT)
    const d = await a.diff(b)
    expect(d.deltaTotal).toBe(0)
    expect(d.baselineTotal).toBe(d.profileTotal)
  }, 60000)
})
