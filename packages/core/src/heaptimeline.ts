export interface HeapTimelineResult {
  // Similar to heapsnapshot but with timeline entries
  entries: TimelineEntry[];
}

export interface TimelineEntry {
  type: "Allocation" | "Relocation";
  timestamp: number;
  nodeId: number;
  size: number;
}

export interface HeapTimelineSummary {
  totalAllocated: number;
  totalFreed: number;
  byType: Map<string, { allocated: number; freed: number; count: number }>;
}

export async function parseHeapTimeline(
  filePath: string,
): Promise<HeapTimelineResult> {
  throw new Error("TODO: implement");
}

export async function streamHeapTimelineSummary(
  filePath: string,
  options?: { top?: number; filter?: string },
): Promise<HeapTimelineSummary> {
  throw new Error("TODO: implement");
}
