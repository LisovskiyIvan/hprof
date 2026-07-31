import * as ffi from './ffi.ts'
import type { NativeHandle } from './ffi.ts'
import { readFileSync } from 'node:fs'

export interface CallFrame {
  functionName: string
  scriptId: string
  url: string
  lineNumber: number
  columnNumber: number
}

export interface HeapProfileNode {
  callFrame: CallFrame
  selfSize: number
  children: HeapProfileNode[]
}

export interface HeapProfileResult {
  head: HeapProfileNode
  startTime: number
  endTime: number
}

export interface HeapProfileSummary {
  totalSize: number
  byFrame: Map<string, number>
  byUrl: Map<string, number>
  byFunction: Map<string, number>
}

export interface FlatCallFrame {
  functionName: string
  url: string
  lineNumber: number
  columnNumber: number
  selfSize: number
  stack: string[]
}

export interface FilterOptions {
  focus?: string
  ignore?: string
  hide?: string
}

export interface CumulativeEntry {
  selfSize: number
  cumulativeSize: number
  count: number
  selfPct: number
  cumulativePct: number
}

export interface CumulativeSummary {
  totalSize: number
  byFrame: Map<string, CumulativeEntry>
  byUrl: Map<string, CumulativeEntry>
  byFunction: Map<string, CumulativeEntry>
}

export interface FlamegraphFrame {
  name: string
  selfSize: number
  totalSize: number
  children: FlamegraphFrame[]
}

export interface TreemapNode {
  name: string
  size: number
  children: TreemapNode[]
}

export interface DiffEntry {
  name: string
  baselineSize: number
  profileSize: number
  delta: number
  deltaPct: number | null
}

export interface ProfileDiff {
  baselineTotal: number
  profileTotal: number
  deltaTotal: number
  byFrame: DiffEntry[]
  byUrl: DiffEntry[]
  byFunction: DiffEntry[]
}

function toCumulativeEntry(raw: any): CumulativeEntry {
  return {
    selfSize: raw.selfSize,
    cumulativeSize: raw.cumulativeSize,
    count: raw.count,
    selfPct: raw.selfPct,
    cumulativePct: raw.cumulativePct,
  }
}

export class HeapProfile {
  readonly filePath: string
  private _handle: NativeHandle | null = null

  constructor(filePath: string) {
    this.filePath = filePath
  }

  private get handle(): NativeHandle {
    if (!this._handle) {
      this._handle = ffi.profileOpen(this.filePath)
    }
    return this._handle
  }

  get data(): HeapProfileResult {
    throw new Error(
      'Use profileSummarize() or profileFlatten() instead — data() requires full JSON parse',
    )
  }

  /// Eagerly parse the whole profile JSON into a recursive tree structure.
  /// This is heavier than `summarize()` but is required for the call-tree view
  /// and the source-listing aggregation.
  getFullData(): HeapProfileResult {
    const text = readFileSync(this.filePath, 'utf8')
    const raw = JSON.parse(text)
    return {
      head: convertNode(raw.head),
      startTime: raw.startTime ?? 0,
      endTime: raw.endTime ?? 0,
    }
  }

  summarize(options?: { top?: number; filter?: string }): HeapProfileSummary {
    const raw = ffi.profileSummarize(this.handle, options?.top, options?.filter)
    return {
      totalSize: raw.totalSize,
      byFrame: new Map(Object.entries(raw.byFrame)),
      byUrl: new Map(Object.entries(raw.byUrl)),
      byFunction: new Map(Object.entries(raw.byFunction)),
    }
  }

  summarizeCumulative(options?: { top?: number } & FilterOptions): CumulativeSummary {
    const raw = ffi.profileSummarizeCumulative(this.handle, options)
    const byFrame = new Map<string, CumulativeEntry>()
    const byUrl = new Map<string, CumulativeEntry>()
    const byFunction = new Map<string, CumulativeEntry>()
    for (const [k, v] of Object.entries<any>(raw.byFrame)) byFrame.set(k, toCumulativeEntry(v))
    for (const [k, v] of Object.entries<any>(raw.byUrl)) byUrl.set(k, toCumulativeEntry(v))
    for (const [k, v] of Object.entries<any>(raw.byFunction))
      byFunction.set(k, toCumulativeEntry(v))
    return { totalSize: raw.totalSize, byFrame, byUrl, byFunction }
  }

  flatten(): FlatCallFrame[] {
    const raw = ffi.profileFlatten(this.handle)
    return raw.map((f: any) => ({
      functionName: f.functionName,
      url: f.url,
      lineNumber: f.lineNumber,
      columnNumber: f.columnNumber,
      selfSize: f.selfSize,
      stack: f.stack,
    }))
  }

  flamegraph(options?: FilterOptions): FlamegraphFrame {
    return ffi.profileFlamegraph(this.handle, options)
  }

  dot(options?: { top?: number } & FilterOptions): string {
    return ffi.profileDot(this.handle, options)
  }

  treemap(options?: FilterOptions): TreemapNode {
    return ffi.profileTreemap(this.handle, options)
  }

  diff(baseline: HeapProfile): ProfileDiff {
    const raw = ffi.profileDiff(this.handle, baseline.handle)
    return {
      baselineTotal: raw.baselineTotal,
      profileTotal: raw.profileTotal,
      deltaTotal: raw.deltaTotal,
      byFrame: raw.byFrame,
      byUrl: raw.byUrl,
      byFunction: raw.byFunction,
    }
  }

  destroy(): void {
    if (this._handle) {
      ffi.profileDestroy(this._handle)
      this._handle = null
    }
  }
}

function convertNode(val: any): HeapProfileNode {
  if (!val) {
    return {
      callFrame: { functionName: '', scriptId: '', url: '', lineNumber: 0, columnNumber: 0 },
      selfSize: 0,
      children: [],
    }
  }
  const cf = val.callFrame ?? {}
  return {
    callFrame: {
      functionName: cf.functionName ?? '',
      scriptId: cf.scriptId ?? '',
      url: cf.url ?? '',
      lineNumber: cf.lineNumber ?? 0,
      columnNumber: cf.columnNumber ?? 0,
    },
    selfSize: val.selfSize ?? 0,
    children: Array.isArray(val.children) ? val.children.map(convertNode) : [],
  }
}
