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

export async function parseHeapSnapshot(
  filePath: string,
): Promise<HeapSnapshotResult> {
  throw new Error("TODO: implement");
}

export async function streamHeapSnapshotSummary(
  filePath: string,
  options?: { top?: number; filter?: string },
): Promise<HeapSnapshotSummary> {
  throw new Error("TODO: implement");
}
