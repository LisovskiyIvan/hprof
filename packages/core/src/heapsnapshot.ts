import * as ffi from './ffi.ts'
import type { NativeHandle } from './ffi.ts'

export interface HeapSnapshotMeta {
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

export interface HeapSnapshotNode {
  type: string
  name: string
  selfSize: number
  retentionSize?: number
  id: number
  edgeCount: number
}

export interface HeapSnapshotEdge {
  type: string
  nameOrIndex: string | number
  toNode: number
}

export interface HeapSnapshotResult {
  meta: HeapSnapshotMeta
  nodes: HeapSnapshotNode[]
  edges: HeapSnapshotEdge[]
  strings: string[]
}

export interface HeapSnapshotSummary {
  totalSize: number
  totalCount: number
  byNodeType: Map<string, { size: number; count: number }>
  byNodeName: Map<string, { size: number; count: number }>
}

export interface HeapSnapshotNodePageOptions {
  page?: number
  pageSize?: number
  type?: string | null
  q?: string | null
  sort?: 'id' | 'type' | 'name' | 'selfSize' | 'edgeCount'
  dir?: 'asc' | 'desc'
}

export interface HeapSnapshotNodePage {
  total: number
  page: number
  pageSize: number
  nodes: HeapSnapshotNode[]
}

export interface HeapSnapshotSearchMatch {
  index: number
  value: string
}

export interface HeapSnapshotNameMatch {
  nodeIndex: number
  id: number
  name: string
  type: string
  selfSize: number
  edgeCount: number
}

/** Value of a node property, resolved by the native side. `kind` mirrors the
 * serde tag: `number`, `string`, or `ref` (object). The element index of an
 * `element` edge is carried in `name` ("[i]"). */
export interface HeapSnapshotProperty {
  name: string
  edgeType: string
  kind: 'number' | 'string' | 'ref'
  value: number | string | { index: number; id: number; type: string; name: string }
}

export interface HeapSnapshotRetainer {
  source: number
  edgeType: string
  name: string
}

export interface HeapSnapshotRetainerChainNode {
  nodeIndex: number
  id: number
  name: string
  type: string
  selfSize: number
  edgeCount: number
  edgeType: string
  edgeName: string
  cycle: boolean
}

export interface HeapSnapshotOwnerGroup {
  chain: string
  count: number
  selfSize: number
}

export interface HeapSnapshotOwnerAnalysis {
  name: string
  totalNodes: number
  totalSelf: number
  groups: HeapSnapshotOwnerGroup[]
}

export interface HeapSnapshotRetainedEntry {
  nodeIndex: number
  name: string
  type: string
  selfSize: number
  retainedSize: number
  approximate: boolean
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

export interface SnapshotDiff {
  baselineTotal: number
  profileTotal: number
  deltaTotal: number
  byNodeName: DiffEntry[]
  byNodeType: DiffEntry[]
  objects?: SnapshotObjectDiff
}

export interface SnapshotObject {
  index: number
  id: number
  name: string
  type: string
  selfSize: number
  edgeCount: number
}

export interface SnapshotObjectChange {
  id: number
  baselineIndex: number
  profileIndex: number
  name: string
  type: string
  baselineSize: number
  profileSize: number
  delta: number
}

export interface SnapshotObjectDiff {
  matchedCount: number
  newCount: number
  deletedCount: number
  newSize: number
  deletedSize: number
  deltaSize: number
  newObjects: SnapshotObject[]
  deletedObjects: SnapshotObject[]
  grownObjects: SnapshotObjectChange[]
}

export interface DetachedNode {
  node: SnapshotObject
  detachedness: number
  ownerChain: string
}

export interface DetachedSummary {
  totalCount: number
  totalSize: number
  entries: DetachedNode[]
}

export interface SizeHistogram {
  totalCount: number
  totalSize: number
  buckets: { minSize: number; maxSize: number; count: number; totalSize: number }[]
}

export interface StringStats {
  totalStrings: number
  totalBytes: number
  referencedStrings: number
  referencedBytes: number
  entries: { value: string; references: number; byteLength: number; referencedBytes: number }[]
}

export interface HeapSnapshotEdgeMatch {
  sourceIndex: number
  sourceId: number
  sourceName: string
  sourceType: string
  edgeType: string
  name: string
  targetIndex: number
  targetId: number
  targetName: string
  targetType: string
}

export class HeapSnapshot {
  readonly filePath: string
  private _handle: NativeHandle | null = null
  private _meta: HeapSnapshotMeta | null = null

  constructor(filePath: string) {
    this.filePath = filePath
  }

  private get handle(): NativeHandle {
    if (!this._handle) {
      this._handle = ffi.snapshotOpen(this.filePath)
    }
    return this._handle
  }

  get meta(): HeapSnapshotMeta {
    if (!this._meta) {
      this._meta = ffi.snapshotMeta(this.handle) as HeapSnapshotMeta
    }
    return this._meta
  }

  async streamSummary(options?: {
    top?: number
    filter?: string
    onProgress?: (phase: string, pct: number) => void
  }): Promise<HeapSnapshotSummary> {
    const raw = ffi.snapshotSummary(this.handle, options?.top ?? 30, options?.filter)
    const byNodeName = new Map<string, { size: number; count: number }>()
    for (const [name, info] of Object.entries(raw.by_node_name)) {
      byNodeName.set(name, info as { size: number; count: number })
    }
    const byNodeType = new Map<string, { size: number; count: number }>()
    for (const [type, info] of Object.entries(raw.by_node_type)) {
      byNodeType.set(type, info as { size: number; count: number })
    }
    return {
      totalSize: raw.total_size,
      totalCount: raw.total_count,
      byNodeName,
      byNodeType,
    }
  }

  async getNodePage(options?: HeapSnapshotNodePageOptions): Promise<HeapSnapshotNodePage> {
    const raw = ffi.snapshotNodePage(this.handle, options)
    return {
      total: raw.total,
      page: raw.page,
      pageSize: raw.page_size,
      nodes: raw.nodes.map((n: any) => ({
        type: n.type,
        name: n.name,
        selfSize: n.self_size,
        id: n.id,
        edgeCount: n.edge_count,
      })),
    }
  }

  async getNodeEdges(
    nodeIndex: number,
  ): Promise<{ node: HeapSnapshotNode; edges: HeapSnapshotEdge[] }> {
    const raw = ffi.snapshotEdges(this.handle, nodeIndex)
    return {
      node: {
        type: raw.node.type,
        name: raw.node.name,
        selfSize: raw.node.self_size,
        id: raw.node.id,
        edgeCount: raw.node.edge_count,
      },
      edges: raw.edges.map((e: any) => ({
        type: e.type,
        nameOrIndex: e.name_or_index,
        toNode: e.to_node,
      })),
    }
  }

  async searchStrings(query: string): Promise<HeapSnapshotSearchMatch[]> {
    const raw = ffi.snapshotSearch(this.handle, query)
    return raw as HeapSnapshotSearchMatch[]
  }

  /** Find nodes by name (substring by default, exact with `exact: true`),
   * optionally filtered by minimum self size and node type, ranked by self
   * size. Unlike `getRetainedEntries` this needs no dominator analysis. */
  findNodes(options: {
    exact?: boolean
    name: string
    minSelf?: number
    type?: string
    limit?: number
  }): HeapSnapshotNameMatch[] {
    // FFI JSON shape is our own serde contract, covered by tests
    return ffi.snapshotFind(this.handle, options) as HeapSnapshotNameMatch[]
  }

  /** All edges of a node with values resolved: numbers/strings inlined,
   * objects as `{index, id, type, name}` references. */
  getNodeProperties(
    nodeIndex: number,
  ): { node: HeapSnapshotNode; properties: HeapSnapshotProperty[] } {
    const raw = ffi.snapshotProperties(this.handle, nodeIndex) as {
      node: { type: string; name: string; self_size: number; id: number; edge_count: number }
      properties: HeapSnapshotProperty[]
    }
    return {
      node: {
        type: raw.node.type,
        name: raw.node.name,
        selfSize: raw.node.self_size,
        id: raw.node.id,
        edgeCount: raw.node.edge_count,
      },
      properties: raw.properties,
    }
  }

  /** All incoming edges of a node: who retains it and how. */
  getRetainers(nodeIndex: number): HeapSnapshotRetainer[] {
    // FFI JSON shape is our own serde contract, covered by tests
    return ffi.snapshotRetainers(this.handle, nodeIndex) as HeapSnapshotRetainer[]
  }

  /** Walk the first-parent (owner) chain up to `maxDepth` hops, target
   * first. `cycle` on the last hop means the walk hit an already-seen node. */
  getRetainerChain(nodeIndex: number, maxDepth = 8): HeapSnapshotRetainerChainNode[] {
    // FFI JSON shape is our own serde contract, covered by tests
    return ffi.snapshotChain(this.handle, nodeIndex, maxDepth) as HeapSnapshotRetainerChainNode[]
  }

  /** Classify nodes matching `name` into owner groups: each match is walked
   * up its first-parent chain (`depth` hops) and grouped by the resulting
   * "owner -> parent -> ..." chain, summed by self size. */
  ownerGroups(options: {
    exact?: boolean
    name: string
    minSelf?: number
    depth?: number
    top?: number
  }): HeapSnapshotOwnerAnalysis {
    // FFI JSON shape is our own serde contract, covered by tests
    return ffi.snapshotOwners(this.handle, options) as HeapSnapshotOwnerAnalysis
  }

  async getRetainedEntries(
    topN = 30,
    exact = false,
  ): Promise<{ approximate: boolean; retained: HeapSnapshotRetainedEntry[] }> {
    const raw = ffi.snapshotRetainedMode(this.handle, topN, exact)
    return {
      approximate: raw.approximate,
      retained: raw.retained.map((e: any) => ({
        nodeIndex: e.node_index,
        name: e.name,
        type: e.type,
        selfSize: e.self_size,
        retainedSize: e.retained_size,
        approximate: e.approximate,
      })),
    }
  }

  async detached(limit = 30, depth = 0): Promise<DetachedSummary> {
    return ffi.snapshotDetached(this.handle, limit, depth) as DetachedSummary
  }

  async sizeHistogram(): Promise<SizeHistogram> {
    return ffi.snapshotSizeHistogram(this.handle) as SizeHistogram
  }

  async stringStats(limit = 30): Promise<StringStats> {
    return ffi.snapshotStringStats(this.handle, limit) as StringStats
  }

  findEdges(options: {
    exact?: boolean
    name: string
    type?: string
    edgeType?: string
    limit?: number
  }): HeapSnapshotEdgeMatch[] {
    return ffi.snapshotFindEdges(this.handle, options) as HeapSnapshotEdgeMatch[]
  }

  dot(nodeIndex: number, depth = 2, maxNodes = 1000): string {
    return ffi.snapshotDot(this.handle, nodeIndex, depth, maxNodes)
  }

  async flamegraph(options?: { top?: number; filter?: string }): Promise<FlamegraphFrame> {
    return ffi.snapshotFlamegraph(this.handle, options?.top, options?.filter)
  }

  async treemap(options?: { top?: number; filter?: string }): Promise<TreemapNode> {
    return ffi.snapshotTreemap(this.handle, options?.top, options?.filter)
  }

  async diff(baseline: HeapSnapshot): Promise<SnapshotDiff> {
    const raw = ffi.snapshotDiff(this.handle, baseline.handle)
    return {
      baselineTotal: raw.baselineTotal,
      profileTotal: raw.profileTotal,
      deltaTotal: raw.deltaTotal,
      byNodeName: raw.byNodeName,
      byNodeType: raw.byNodeType,
      objects: raw.objects,
    }
  }

  destroy(): void {
    if (this._handle) {
      ffi.snapshotDestroy(this._handle)
      this._handle = null
    }
  }
}
