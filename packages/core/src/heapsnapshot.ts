import * as ffi from "./ffi.ts";
import type { NativeHandle } from "./ffi.ts";

export interface HeapSnapshotMeta {
  node_count: number;
  edge_count: number;
  extra_native_bytes?: number;
  meta: {
    node_fields: string[];
    node_types: string[][];
    edge_fields: string[];
    edge_types: string[][];
  };
}

export interface HeapSnapshotNode {
  type: string;
  name: string;
  selfSize: number;
  retentionSize?: number;
  id: number;
  edgeCount: number;
}

export interface HeapSnapshotEdge {
  type: string;
  nameOrIndex: string | number;
  toNode: number;
}

export interface HeapSnapshotResult {
  meta: HeapSnapshotMeta;
  nodes: HeapSnapshotNode[];
  edges: HeapSnapshotEdge[];
  strings: string[];
}

export interface HeapSnapshotSummary {
  totalSize: number;
  totalCount: number;
  byNodeType: Map<string, { size: number; count: number }>;
  byNodeName: Map<string, { size: number; count: number }>;
}

export interface HeapSnapshotNodePageOptions {
  page?: number;
  pageSize?: number;
  type?: string | null;
  q?: string | null;
  sort?: "id" | "type" | "name" | "selfSize" | "edgeCount";
  dir?: "asc" | "desc";
}

export interface HeapSnapshotNodePage {
  total: number;
  page: number;
  pageSize: number;
  nodes: HeapSnapshotNode[];
}

export interface HeapSnapshotSearchMatch {
  index: number;
  value: string;
}

export interface HeapSnapshotRetainedEntry {
  nodeIndex: number;
  name: string;
  type: string;
  selfSize: number;
  retainedSize: number;
  approximate: boolean;
}

export class HeapSnapshot {
  readonly filePath: string;
  private _handle: NativeHandle | null = null;
  private _meta: HeapSnapshotMeta | null = null;

  constructor(filePath: string) {
    this.filePath = filePath;
  }

  private get handle(): NativeHandle {
    if (!this._handle) {
      this._handle = ffi.snapshotOpen(this.filePath);
    }
    return this._handle;
  }

  get meta(): HeapSnapshotMeta {
    if (!this._meta) {
      this._meta = ffi.snapshotMeta(this.handle) as HeapSnapshotMeta;
    }
    return this._meta;
  }

  async streamSummary(options?: {
    top?: number;
    filter?: string;
    onProgress?: (phase: string, pct: number) => void;
  }): Promise<HeapSnapshotSummary> {
    const raw = ffi.snapshotSummary(this.handle, options?.top ?? 30, options?.filter);
    const byNodeName = new Map<string, { size: number; count: number }>();
    for (const [name, info] of Object.entries(raw.by_node_name)) {
      byNodeName.set(name, info as { size: number; count: number });
    }
    const byNodeType = new Map<string, { size: number; count: number }>();
    for (const [type, info] of Object.entries(raw.by_node_type)) {
      byNodeType.set(type, info as { size: number; count: number });
    }
    return {
      totalSize: raw.total_size,
      totalCount: raw.total_count,
      byNodeName,
      byNodeType,
    };
  }

  async getNodePage(options?: HeapSnapshotNodePageOptions): Promise<HeapSnapshotNodePage> {
    const raw = ffi.snapshotNodePage(this.handle, options);
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
    };
  }

  async getNodeEdges(nodeIndex: number): Promise<{ node: HeapSnapshotNode; edges: HeapSnapshotEdge[] }> {
    const raw = ffi.snapshotEdges(this.handle, nodeIndex);
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
    };
  }

  async searchStrings(query: string): Promise<HeapSnapshotSearchMatch[]> {
    const raw = ffi.snapshotSearch(this.handle, query);
    return raw;
  }

  async getRetainedEntries(topN = 30): Promise<{ approximate: boolean; retained: HeapSnapshotRetainedEntry[] }> {
    const raw = ffi.snapshotRetained(this.handle, topN);
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
    };
  }

  destroy(): void {
    if (this._handle) {
      ffi.snapshotDestroy(this._handle);
      this._handle = null;
    }
  }
}
