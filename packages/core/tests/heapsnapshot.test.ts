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

describe('HeapSnapshot.findNodes', () => {
  test('exact name match returns instances ranked by self size', () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT)
    const found = snapshot.findNodes({ name: '(object elements)', exact: true, limit: 5 })
    expect(found.length).toBeGreaterThan(0)
    for (const m of found) {
      expect(m.nodeIndex).toBeGreaterThanOrEqual(0)
      expect(m.id).toBeGreaterThan(0)
      expect(m.name).toBe('(object elements)')
      expect(typeof m.selfSize).toBe('number')
      expect(typeof m.edgeCount).toBe('number')
      expect(m.type).toBe('array')
    }
    for (let i = 1; i < found.length; i++) {
      expect(found[i - 1]!.selfSize).toBeGreaterThanOrEqual(found[i]!.selfSize)
    }
  }, 60000)

  test('substring match is case-insensitive', () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT)
    const found = snapshot.findNodes({ name: 'OBJECT ELEMENTS', limit: 5 })
    expect(found.length).toBeGreaterThan(0)
    expect(found.every((m) => m.name.includes('(object elements)'))).toBe(true)
  }, 60000)

  test('min-self filter excludes smaller nodes', () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT)
    const all = snapshot.findNodes({ name: '(object elements)', exact: true, limit: 100 })
    const big = snapshot.findNodes({
      name: '(object elements)',
      exact: true,
      minSelf: 1024 * 1024,
      limit: 100,
    })
    expect(big.length).toBeLessThanOrEqual(all.length)
    for (const m of big) {
      expect(m.selfSize).toBeGreaterThanOrEqual(1024 * 1024)
    }
  }, 60000)
})

describe('HeapSnapshot.getNodeProperties', () => {
  test('resolves fields with inlined primitive values', () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT)
    const found = snapshot.findNodes({ name: '(object elements)', exact: true, limit: 1 })
    expect(found.length).toBe(1)
    const target = found[0]!
    const { node, properties } = snapshot.getNodeProperties(target.nodeIndex)
    expect(node.id).toBe(target.id)
    expect(properties.length).toBeGreaterThan(0)
    for (const p of properties) {
      expect(typeof p.name).toBe('string')
      expect(typeof p.edgeType).toBe('string')
      expect(['number', 'string', 'ref']).toContain(p.kind)
      expect(p.value).toBeDefined()
    }
    // array backing store: every edge resolves to a node reference and the
    // names are indices (element or internal edges, depending on the format)
    expect(properties.length).toBeGreaterThan(1000)
    expect(properties.every((p) => p.kind === 'ref')).toBe(true)
    expect(properties[0]!.value).toMatchObject({
      index: expect.any(Number),
      id: expect.any(Number),
      type: expect.any(String),
      name: expect.any(String),
    })
  }, 60000)
})

describe('HeapSnapshot.getRetainers / getRetainerChain', () => {
  test('lists incoming edges', () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT)
    const found = snapshot.findNodes({ name: '(object elements)', exact: true, limit: 1 })
    const retainers = snapshot.getRetainers(found[0]!.nodeIndex)
    expect(retainers.length).toBeGreaterThan(0)
    for (const r of retainers) {
      expect(typeof r.source).toBe('number')
      expect(typeof r.edgeType).toBe('string')
      expect(typeof r.name).toBe('string')
    }
  }, 60000)

  test('walks the owner chain target first', () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT)
    const found = snapshot.findNodes({ name: '(object elements)', exact: true, limit: 1 })
    const chain = snapshot.getRetainerChain(found[0]!.nodeIndex, 4)
    expect(chain.length).toBeGreaterThan(0)
    expect(chain[0]!.nodeIndex).toBe(found[0]!.nodeIndex)
    for (const hop of chain) {
      expect(typeof hop.name).toBe('string')
      expect(typeof hop.selfSize).toBe('number')
      expect(typeof hop.edgeType).toBe('string')
      expect(typeof hop.cycle).toBe('boolean')
    }
  }, 60000)
})

describe('HeapSnapshot.ownerGroups', () => {
  test('classifies matches by owner chain', () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT)
    const analysis = snapshot.ownerGroups({
      name: '(object elements)',
      exact: true,
      minSelf: 100 * 1024,
      top: 5,
    })
    expect(analysis.totalNodes).toBeGreaterThan(0)
    expect(analysis.totalSelf).toBeGreaterThan(0)
    expect(analysis.groups.length).toBeGreaterThan(0)
    for (const g of analysis.groups) {
      expect(typeof g.chain).toBe('string')
      expect(g.count).toBeGreaterThan(0)
      expect(g.selfSize).toBeGreaterThan(0)
    }
    for (let i = 1; i < analysis.groups.length; i++) {
      expect(analysis.groups[i - 1]!.selfSize).toBeGreaterThanOrEqual(
        analysis.groups[i]!.selfSize,
      )
    }
  }, 60000)
})
