import * as ffi from './ffi.ts'
import type { NativeHandle } from './ffi.ts'

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

export interface HeapTimelineSummary {
  totalAllocated: number
  totalFreed: number
  byType: Map<string, { allocated: number; freed: number; count: number }>
}

export interface TimelineNameType {
  name: string
  size: number
  count: number
}

export interface TimelineNameEntry {
  name: string
  size: number
  count: number
  types: TimelineNameType[]
}

export interface TimelineNamesResult {
  totalSize: number
  totalCount: number
  entries: TimelineNameEntry[]
}

export interface TimelineStackFrame {
  name: string
  script: string
  line: number
  column: number
}

export interface TimelineStackEntry {
  size: number
  count: number
  stack: TimelineStackFrame[]
}

export interface TimelineStacksResult {
  totalSize: number
  totalCount: number
  entries: TimelineStackEntry[]
}

export interface TimelineNameStacksResult {
  name: string
  totalSize: number
  totalCount: number
  entries: TimelineStackEntry[]
}

export interface TimelineGrowth {
  spanUs: number
  objectsStart: number
  objectsEnd: number
  samples: [number, number][]
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

  /** Top allocation names by total self-size, with per-type split. */
  async topNames(options?: { top?: number; filter?: string }): Promise<TimelineNamesResult> {
    return ffi.timelineTopNames(this.handle, options?.top, options?.filter)
  }

  /** Top allocation sites (stack traces from the trace tree). */
  async topStacks(options?: { top?: number; filter?: string }): Promise<TimelineStacksResult> {
    return ffi.timelineTopStacks(this.handle, options?.top, options?.filter)
  }

  /** Stack distribution for nodes whose name matches `nameFilter` (regex). */
  async nameStacks(nameFilter: string, top?: number): Promise<TimelineNameStacksResult> {
    return ffi.timelineNameStacks(this.handle, nameFilter, top)
  }

  /** Object-growth profile derived from the samples array. */
  async growth(): Promise<TimelineGrowth> {
    return ffi.timelineGrowth(this.handle)
  }

  /** Node names containing `query`, ranked by allocated size. */
  async searchStrings(query: string): Promise<TimelineNameEntry[]> {
    return ffi.timelineSearch(this.handle, query)
  }

  destroy(): void {
    if (this._handle) {
      ffi.timelineDestroy(this._handle)
      this._handle = null
    }
  }
}
