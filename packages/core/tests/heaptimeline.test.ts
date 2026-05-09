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
    expect(summary.intervals).toEqual([])
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
    expect(phases).toContain('nodes')
    expect(phases).toContain('done')
  }, 60000)
})
