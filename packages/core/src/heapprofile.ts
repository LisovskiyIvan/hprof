export interface CallFrame {
  functionName: string;
  scriptId: string;
  url: string;
  lineNumber: number;
  columnNumber: number;
}

export interface HeapProfileNode {
  callFrame: CallFrame;
  selfSize: number;
  children: HeapProfileNode[];
}

export interface HeapProfileResult {
  head: HeapProfileNode;
  startTime: number;
  endTime: number;
}

export interface HeapProfileSummary {
  totalSize: number;
  byFrame: Map<string, number>;
  byUrl: Map<string, number>;
  byFunction: Map<string, number>;
}

export function parseHeapProfile(filePath: string): HeapProfileResult {
  throw new Error("TODO: implement");
}

export function summarizeHeapProfile(
  data: HeapProfileResult,
  options?: { top?: number; filter?: string },
): HeapProfileSummary {
  throw new Error("TODO: implement");
}
