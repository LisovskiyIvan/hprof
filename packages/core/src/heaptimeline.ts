import fs from 'fs'
import { HeapSnapshot } from './heapsnapshot.ts'
import type { HeapSnapshotMeta } from './heapsnapshot.ts'

export interface HeapTimelineResult {
  meta: HeapSnapshotMeta
  nodes: TimelineNode[]
  strings: string[]
  timeline: TimelineEntry[]
}

export interface TimelineNode {
  type: string
  name: string
  selfSize: number
  id: number
}

export interface TimelineEntry {
  type: 'Allocation' | 'Relocation'
  timestamp: number
  nodeId: number
  size: number
}

export interface HeapTimelineSummary {
  totalAllocated: number
  totalFreed: number
  byType: Map<string, { allocated: number; freed: number; count: number }>
  intervals: TimeInterval[]
}

export interface TimeInterval {
  start: number
  end: number
  allocated: number
  freed: number
}

interface HeapTimelineRawData {
  strings: string[]
  timeline: TimelineEntry[]
}

export class HeapTimeline {
  readonly filePath: string
  private _snapshot: HeapSnapshot | null = null
  private _data: HeapTimelineResult | null = null
  private _rawData: HeapTimelineRawData | null = null
  private _rawDataPromise: Promise<HeapTimelineRawData> | null = null

  constructor(filePath: string) {
    this.filePath = filePath
  }

  private get snapshot(): HeapSnapshot {
    if (!this._snapshot) {
      this._snapshot = new HeapSnapshot(this.filePath)
    }
    return this._snapshot
  }

  get meta(): HeapSnapshotMeta {
    return this.snapshot.meta
  }

  get data(): HeapTimelineResult {
    if (!this._data) {
      this._data = this.parseFull()
    }
    return this._data
  }

  private parseFull(): HeapTimelineResult {
    const raw = JSON.parse(fs.readFileSync(this.filePath, 'utf8')) as {
      snapshot: HeapSnapshotMeta
      nodes: number[]
      strings: string[]
      timeline?: unknown[]
    }

    const meta = raw.snapshot
    const nodeFields = meta.meta.node_fields
    const nodeTypes = meta.meta.node_types[0]!
    const nodeFieldCount = nodeFields.length

    const nodeTypeIdx = nodeFields.indexOf('type')
    const nodeNameIdx = nodeFields.indexOf('name')
    const nodeSelfSizeIdx = nodeFields.indexOf('self_size')
    const nodeIdIdx = nodeFields.indexOf('id')

    const nodes: TimelineNode[] = []
    for (let base = 0; base < raw.nodes.length; base += nodeFieldCount) {
      nodes.push({
        type: nodeTypes[raw.nodes[base + nodeTypeIdx]!] ?? String(raw.nodes[base + nodeTypeIdx]),
        name:
          raw.strings[raw.nodes[base + nodeNameIdx]!] ??
          `<string#${raw.nodes[base + nodeNameIdx]}>`,
        selfSize: raw.nodes[base + nodeSelfSizeIdx]!,
        id: raw.nodes[base + nodeIdIdx]!,
      })
    }

    const timeline: TimelineEntry[] = []
    if (raw.timeline && Array.isArray(raw.timeline)) {
      for (const entry of raw.timeline) {
        if (typeof entry === 'object' && entry !== null) {
          const e = entry as Record<string, unknown>
          timeline.push({
            type: (e.type as string) === 'Relocation' ? 'Relocation' : 'Allocation',
            timestamp: (e.timestamp as number) ?? 0,
            nodeId: (e.nodeId as number) ?? 0,
            size: (e.size as number) ?? 0,
          })
        }
      }
    }

    return { meta, nodes, strings: raw.strings, timeline }
  }

  async streamSummary(options?: {
    top?: number
    filter?: string
    onProgress?: (phase: string, pct: number) => void
  }): Promise<HeapTimelineSummary> {
    const snapshot = this.snapshot
    const snapshotMeta = snapshot.meta
    const nodeFields = snapshotMeta.meta.node_fields
    const nodeTypes = snapshotMeta.meta.node_types[0]!
    const nodeFieldCount = nodeFields.length
    const typeOffset = nodeFields.indexOf('type')
    const nameOffset = nodeFields.indexOf('name')
    const selfSizeOffset = nodeFields.indexOf('self_size')

    if (typeOffset < 0 || nameOffset < 0 || selfSizeOffset < 0) {
      throw new Error('Unsupported node field layout')
    }

    const byTypeIndex = new Map<number, { allocated: number; freed: number; count: number }>()
    let totalAllocated = 0
    const onProgress = options?.onProgress
    const fileSize = fs.statSync(this.filePath).size

    let mode: 'seekNodes' | 'parseNodes' | 'done' = 'seekNodes'
    let record: number[] = []
    let currentNumber = ''

    const stream = fs.createReadStream(this.filePath, { encoding: 'utf8' })
    let bytesRead = 0
    let lastProgressPct = -1

    for await (const chunk of stream) {
      bytesRead += chunk.length
      const pct = Math.floor((bytesRead / fileSize) * 100)
      if (onProgress && pct !== lastProgressPct) {
        lastProgressPct = pct
        onProgress('nodes', pct)
      }

      let i = 0
      while (i < chunk.length) {
        if (mode === 'seekNodes') {
          const idx = chunk.indexOf('"nodes":[', i)
          if (idx === -1) break
          i = idx + '"nodes":['.length
          mode = 'parseNodes'
          continue
        }

        if (mode === 'parseNodes') {
          const ch = chunk[i]!
          if (ch >= '0' && ch <= '9') {
            currentNumber += ch
          } else if (ch === '-') {
            currentNumber += ch
          } else if (ch === ',' || ch === ']') {
            if (currentNumber) {
              record.push(Number(currentNumber))
              currentNumber = ''
            }

            if (record.length === nodeFieldCount) {
              const typeIdx = record[typeOffset]!
              const selfSize = record[selfSizeOffset]!
              totalAllocated += selfSize

              if (selfSize > 0) {
                const prev = byTypeIndex.get(typeIdx) ?? {
                  allocated: 0,
                  freed: 0,
                  count: 0,
                }
                prev.allocated += selfSize
                prev.count += 1
                byTypeIndex.set(typeIdx, prev)
              }

              record = []
            }

            if (ch === ']') {
              mode = 'done'
              break
            }
          }

          i += 1
          continue
        }

        if (mode === 'done') break
      }

      if (mode === 'done') break
    }

    if (onProgress && lastProgressPct < 100) {
      onProgress('done', 100)
    }

    const byType = new Map<string, { allocated: number; freed: number; count: number }>()
    for (const [typeIdx, info] of byTypeIndex) {
      const typeName = nodeTypes[typeIdx] ?? String(typeIdx)
      byType.set(typeName, info)
    }

    const top = options?.top ?? 30
    const sorted = [...byType.entries()]
      .sort((a, b) => b[1].allocated - a[1].allocated)
      .slice(0, top)

    return {
      totalAllocated,
      totalFreed: 0,
      byType: new Map(sorted),
      intervals: [],
    }
  }

  async rawData(): Promise<HeapTimelineRawData> {
    if (this._rawData) return this._rawData
    if (!this._rawDataPromise) {
      this._rawDataPromise = Bun.file(this.filePath)
        .json()
        .then((raw) => {
          const data = raw as {
            strings?: string[]
            timeline?: unknown[]
          }

          const timeline: TimelineEntry[] = []
          if (Array.isArray(data.timeline)) {
            for (const entry of data.timeline) {
              if (typeof entry !== 'object' || entry === null) continue
              const e = entry as Record<string, unknown>
              timeline.push({
                type: e.type === 'Relocation' ? 'Relocation' : 'Allocation',
                timestamp: typeof e.timestamp === 'number' ? e.timestamp : 0,
                nodeId: typeof e.nodeId === 'number' ? e.nodeId : 0,
                size: typeof e.size === 'number' ? e.size : 0,
              })
            }
          }

          this._rawData = {
            strings: Array.isArray(data.strings) ? data.strings : [],
            timeline,
          }

          return this._rawData
        })
        .finally(() => {
          this._rawDataPromise = null
        })
    }

    return this._rawDataPromise
  }

  async getTimelineEntries(): Promise<TimelineEntry[]> {
    const raw = await this.rawData()
    return raw.timeline
  }

  async searchStrings(query: string): Promise<Array<{ index: number; value: string }>> {
    const raw = await this.rawData()
    const re = new RegExp(query, 'i')
    const matches: Array<{ index: number; value: string }> = []
    for (let index = 0; index < raw.strings.length; index++) {
      const value = raw.strings[index]!
      if (!re.test(value)) continue
      matches.push({ index, value })
      if (matches.length >= 100) break
    }
    return matches
  }
}
