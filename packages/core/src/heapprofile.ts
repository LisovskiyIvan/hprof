import * as ffi from './ffi.ts'
import type { NativeHandle } from './ffi.ts'

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

  summarize(options?: { top?: number; filter?: string }): HeapProfileSummary {
    const raw = ffi.profileSummarize(this.handle, options?.top, options?.filter)
    return {
      totalSize: raw.totalSize,
      byFrame: new Map(Object.entries(raw.byFrame)),
      byUrl: new Map(Object.entries(raw.byUrl)),
      byFunction: new Map(Object.entries(raw.byFunction)),
    }
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

  destroy(): void {
    if (this._handle) {
      ffi.profileDestroy(this._handle)
      this._handle = null
    }
  }
}
