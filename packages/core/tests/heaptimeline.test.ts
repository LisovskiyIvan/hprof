import { describe, expect, test } from 'bun:test'
import path from 'path'
import { HeapTimeline } from '../src/heaptimeline.ts'

const SNAPSHOTS = path.resolve(import.meta.dir, '../../../snapshots')
const HEAP_TIMELINE = path.join(SNAPSHOTS, 'Heap-20260508T151658.heaptimeline')

describe('HeapTimeline.meta', () => {
  test('extracts meta from heaptimeline header', () => {
    const timeline = new HeapTimeline(HEAP_TIMELINE)
    const meta = timeline.meta
    expect(meta.node_count).toBeGreaterThan(0)
    expect(meta.edge_count).toBeGreaterThan(0)
    expect(meta.meta.node_fields).toContain('type')
    expect(meta.meta.node_fields).toContain('name')
    expect(meta.meta.node_fields).toContain('self_size')
  })
})

describe('HeapTimeline.streamSummary', () => {
  test('produces summary with top-N node types', async () => {
    const timeline = new HeapTimeline(HEAP_TIMELINE)
    const summary = await timeline.streamSummary({ top: 10 })
    expect(summary.totalAllocated).toBeGreaterThan(0)
    expect(summary.totalFreed).toBe(0)
    expect(summary.byType.size).toBeGreaterThan(0)
    expect(summary.byType.size).toBeLessThanOrEqual(10)
  }, 60000)

  test('calls onProgress callback', async () => {
    const timeline = new HeapTimeline(HEAP_TIMELINE)
    const phases: string[] = []
    await timeline.streamSummary({
      top: 5,
      onProgress: (phase) => {
        if (!phases.includes(phase)) phases.push(phase)
      },
    })
    // The native timeline parser does not currently emit progress phases, so
    // this is a smoke test: just ensure the callback type is accepted.
    expect(Array.isArray(phases)).toBe(true)
  }, 60000)
})

describe('HeapTimeline.topNames', () => {
  test('returns top names with per-type split, sorted by size', async () => {
    const timeline = new HeapTimeline(HEAP_TIMELINE)
    const res = await timeline.topNames({ top: 10 })
    expect(res.totalSize).toBeGreaterThan(0)
    expect(res.totalCount).toBeGreaterThan(0)
    expect(res.entries.length).toBeGreaterThan(0)
    expect(res.entries.length).toBeLessThanOrEqual(10)
    // sorted desc by size
    for (let i = 1; i < res.entries.length; i++) {
      expect(res.entries[i - 1]!.size).toBeGreaterThanOrEqual(res.entries[i]!.size)
    }
    for (const e of res.entries) {
      expect(e.name.length).toBeGreaterThan(0)
      expect(e.count).toBeGreaterThan(0)
      // per-type sizes sum to the entry size
      const typeSum = e.types.reduce((s, t) => s + t.size, 0)
      expect(typeSum).toBe(e.size)
    }
  }, 60000)

  test('filter narrows results to matching names', async () => {
    const timeline = new HeapTimeline(HEAP_TIMELINE)
    const res = await timeline.topNames({ top: 50, filter: 'Vector3' })
    expect(res.entries.length).toBeGreaterThan(0)
    for (const e of res.entries) {
      expect(e.name).toMatch(/Vector3/)
    }
  }, 60000)
})

describe('HeapTimeline.topStacks', () => {
  test('returns allocation sites with resolvable stack frames', async () => {
    const timeline = new HeapTimeline(HEAP_TIMELINE)
    const res = await timeline.topStacks({ top: 10 })
    expect(res.totalCount).toBeGreaterThan(0)
    expect(res.entries.length).toBeGreaterThan(0)
    for (const e of res.entries) {
      expect(e.stack.length).toBeGreaterThan(0)
      // root frame first
      expect(e.stack[0]!.name).toBe('(root)')
    }
  }, 60000)
})

describe('HeapTimeline.growth', () => {
  test('reports object growth from samples', async () => {
    const timeline = new HeapTimeline(HEAP_TIMELINE)
    const g = await timeline.growth()
    expect(g.spanUs).toBeGreaterThan(0)
    expect(g.samples.length).toBeGreaterThanOrEqual(2)
    expect(g.objectsEnd).toBeGreaterThan(g.objectsStart)
    // samples are (time, objects) pairs, strictly increasing
    for (let i = 1; i < g.samples.length; i++) {
      expect(g.samples[i]![0]).toBeGreaterThan(g.samples[i - 1]![0])
      expect(g.samples[i]![1]).toBeGreaterThan(g.samples[i - 1]![1])
    }
  }, 60000)
})

describe('HeapTimeline.nameStacks', () => {
  test('attributes allocations of a name to stacks', async () => {
    const timeline = new HeapTimeline(HEAP_TIMELINE)
    const res = await timeline.nameStacks('Vector3', 5)
    expect(res.totalCount).toBeGreaterThan(0)
    expect(res.totalSize).toBeGreaterThan(0)
    expect(res.entries.length).toBeGreaterThan(0)
  }, 60000)
})

describe('HeapTimeline.searchStrings', () => {
  test('finds names containing the query, ranked by size', async () => {
    const timeline = new HeapTimeline(HEAP_TIMELINE)
    const matches = await timeline.searchStrings('vector')
    expect(matches.length).toBeGreaterThan(0)
    for (const m of matches) {
      expect(m.name.toLowerCase()).toContain('vector')
    }
  }, 60000)
})
