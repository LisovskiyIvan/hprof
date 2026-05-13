import * as ffi from './ffi.ts'
import type { NativeHandle } from './ffi.ts'
import type { HeapSnapshotMeta } from './heapsnapshot.ts'

export interface HeapTimelineMeta {
  node_count: number
  edge_count: number
  extra_native_bytes?: number
  meta: {
    node_fields: string[]
    node_types: string[][]
    edge_fields: string[]
    edge_types: string[][]
  }
}

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
}

export class HeapTimeline {
  readonly filePath: string
  private _handle: NativeHandle | null = null
  private _meta: HeapTimelineMeta | null = null

  constructor(filePath: string) {
    this.filePath = filePath
  }

  private get handle(): NativeHandle {
    if (!this._handle) {
      this._handle = ffi.timelineOpen(this.filePath)
    }
    return this._handle
  }

  get meta(): HeapTimelineMeta {
    if (!this._meta) {
      this._meta = ffi.timelineMeta(this.handle) as HeapTimelineMeta
    }
    return this._meta
  }

  async streamSummary(options?: {
    top?: number
    filter?: string
    onProgress?: (phase: string, pct: number) => void
  }): Promise<HeapTimelineSummary> {
    const raw = ffi.timelineSummary(this.handle, options?.top, options?.filter)
    return {
      totalAllocated: raw.totalAllocated,
      totalFreed: raw.totalFreed,
      byType: new Map(Object.entries(raw.byType)),
    }
  }

  destroy(): void {
    if (this._handle) {
      ffi.timelineDestroy(this._handle)
      this._handle = null
    }
  }
}
