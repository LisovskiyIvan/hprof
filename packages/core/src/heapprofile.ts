import fs from 'fs'

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

function add(map: Map<string, number>, key: string, size: number) {
  map.set(key, (map.get(key) ?? 0) + size)
}

export class HeapProfile {
  readonly filePath: string
  private _data: HeapProfileResult | null = null

  constructor(filePath: string) {
    this.filePath = filePath
  }

  get data(): HeapProfileResult {
    if (!this._data) {
      const raw = JSON.parse(fs.readFileSync(this.filePath, 'utf8')) as {
        head: HeapProfileNode
        startTime: number
        endTime: number
      }
      this._data = {
        head: raw.head,
        startTime: raw.startTime,
        endTime: raw.endTime,
      }
    }
    return this._data
  }

  summarize(options?: { top?: number; filter?: string }): HeapProfileSummary {
    const top = options?.top ?? Infinity
    const filterRe = options?.filter ? new RegExp(options.filter, 'i') : null

    const byFrame = new Map<string, number>()
    const byUrl = new Map<string, number>()
    const byFunction = new Map<string, number>()
    let totalSize = 0

    function walk(node: HeapProfileNode, stack: string[]) {
      const cf = node.callFrame
      const fn = cf.functionName || '(anonymous)'
      const url = cf.url || '<no-url>'
      const line = (cf.lineNumber ?? -1) + 1
      const frame = `${fn} @ ${url}:${line}`
      const nextStack = [...stack, frame]
      const selfSize = node.selfSize || 0

      if (selfSize > 0) {
        const hay = `${frame}\n${nextStack.join('\n')}`
        if (!filterRe || filterRe.test(hay)) {
          totalSize += selfSize
          add(byFrame, frame, selfSize)
          add(byUrl, url, selfSize)
          add(byFunction, fn, selfSize)
        }
      }

      for (const child of node.children) {
        walk(child, nextStack)
      }
    }

    walk(this.data.head, [])

    const trim = (map: Map<string, number>) => {
      if (top === Infinity) return map
      const sorted = [...map.entries()].sort((a, b) => b[1] - a[1])
      return new Map(sorted.slice(0, top))
    }

    return {
      totalSize,
      byFrame: trim(byFrame),
      byUrl: trim(byUrl),
      byFunction: trim(byFunction),
    }
  }

  flatten(): FlatCallFrame[] {
    const result: FlatCallFrame[] = []

    function walk(node: HeapProfileNode, stack: string[]) {
      const cf = node.callFrame
      const fn = cf.functionName || '(anonymous)'
      const url = cf.url || '<no-url>'
      const line = (cf.lineNumber ?? -1) + 1
      const frame = `${fn} @ ${url}:${line}`
      const nextStack = [...stack, frame]

      if (node.selfSize > 0) {
        result.push({
          functionName: fn,
          url,
          lineNumber: cf.lineNumber,
          columnNumber: cf.columnNumber,
          selfSize: node.selfSize,
          stack: nextStack,
        })
      }

      for (const child of node.children) {
        walk(child, nextStack)
      }
    }

    walk(this.data.head, [])
    return result
  }
}
