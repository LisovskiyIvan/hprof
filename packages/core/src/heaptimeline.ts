import fs from "fs";
import { parseSnapshotMeta } from "./heapsnapshot.ts";
import type { HeapSnapshotMeta } from "./heapsnapshot.ts";

export interface HeapTimelineResult {
  meta: HeapSnapshotMeta;
  nodes: TimelineNode[];
  strings: string[];
  timeline: TimelineEntry[];
}

export interface TimelineNode {
  type: string;
  name: string;
  selfSize: number;
  id: number;
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
  intervals: TimeInterval[];
}

export interface TimeInterval {
  start: number;
  end: number;
  allocated: number;
  freed: number;
}

export async function parseHeapTimeline(
  filePath: string,
): Promise<HeapTimelineResult> {
  const raw = JSON.parse(fs.readFileSync(filePath, "utf8")) as {
    snapshot: HeapSnapshotMeta;
    nodes: number[];
    strings: string[];
    timeline?: unknown[];
  };

  const meta = raw.snapshot;
  const nodeFields = meta.meta.node_fields;
  const nodeTypes = meta.meta.node_types[0]!;
  const nodeFieldCount = nodeFields.length;

  const nodeTypeIdx = nodeFields.indexOf("type");
  const nodeNameIdx = nodeFields.indexOf("name");
  const nodeSelfSizeIdx = nodeFields.indexOf("self_size");
  const nodeIdIdx = nodeFields.indexOf("id");

  const nodes: TimelineNode[] = [];
  for (let i = 0; i < raw.nodes.length; i += nodeFieldCount) {
    const slice = raw.nodes.slice(i, i + nodeFieldCount);
    nodes.push({
      type: nodeTypes[slice[nodeTypeIdx]!] ?? String(slice[nodeTypeIdx]),
      name: raw.strings[slice[nodeNameIdx]!] ?? `<string#${slice[nodeNameIdx]}>`,
      selfSize: slice[nodeSelfSizeIdx]!,
      id: slice[nodeIdIdx]!,
    });
  }

  const timeline: TimelineEntry[] = [];
  if (raw.timeline && Array.isArray(raw.timeline)) {
    for (const entry of raw.timeline) {
      if (typeof entry === "object" && entry !== null) {
        const e = entry as Record<string, unknown>;
        timeline.push({
          type: (e.type as string) === "Relocation" ? "Relocation" : "Allocation",
          timestamp: (e.timestamp as number) ?? 0,
          nodeId: (e.nodeId as number) ?? 0,
          size: (e.size as number) ?? 0,
        });
      }
    }
  }

  return { meta, nodes, strings: raw.strings, timeline };
}

export async function streamHeapTimelineSummary(
  filePath: string,
  options?: { top?: number; filter?: string },
): Promise<HeapTimelineSummary> {
  const snapshot = parseSnapshotMeta(filePath);
  const nodeFields = snapshot.meta.node_fields;
  const nodeTypes = snapshot.meta.node_types[0]!;
  const nodeFieldCount = nodeFields.length;
  const typeOffset = nodeFields.indexOf("type");
  const nameOffset = nodeFields.indexOf("name");
  const selfSizeOffset = nodeFields.indexOf("self_size");

  if (typeOffset < 0 || nameOffset < 0 || selfSizeOffset < 0) {
    throw new Error("Unsupported node field layout");
  }

  const byTypeIndex = new Map<number, { allocated: number; freed: number; count: number }>();
  let totalAllocated = 0;

  let mode: "seekNodes" | "parseNodes" | "done" = "seekNodes";
  let record: number[] = [];
  let currentNumber = "";

  const stream = fs.createReadStream(filePath, { encoding: "utf8" });

  for await (const chunk of stream) {
    let i = 0;
    while (i < chunk.length) {
      if (mode === "seekNodes") {
        const idx = chunk.indexOf('"nodes":[', i);
        if (idx === -1) break;
        i = idx + '"nodes":['.length;
        mode = "parseNodes";
        continue;
      }

      if (mode === "parseNodes") {
        const ch = chunk[i]!;
        if (ch >= "0" && ch <= "9") {
          currentNumber += ch;
        } else if (ch === "-") {
          currentNumber += ch;
        } else if (ch === "," || ch === "]") {
          if (currentNumber) {
            record.push(Number(currentNumber));
            currentNumber = "";
          }

          if (record.length === nodeFieldCount) {
            const typeIdx = record[typeOffset]!;
            const selfSize = record[selfSizeOffset]!;
            totalAllocated += selfSize;

            if (selfSize > 0) {
              const prev = byTypeIndex.get(typeIdx) ?? {
                allocated: 0,
                freed: 0,
                count: 0,
              };
              prev.allocated += selfSize;
              prev.count += 1;
              byTypeIndex.set(typeIdx, prev);
            }

            record = [];
          }

          if (ch === "]") {
            mode = "done";
            break;
          }
        }

        i += 1;
        continue;
      }

      if (mode === "done") break;
    }

    if (mode === "done") break;
  }

  const byType = new Map<string, { allocated: number; freed: number; count: number }>();
  for (const [typeIdx, info] of byTypeIndex) {
    const typeName = nodeTypes[typeIdx] ?? String(typeIdx);
    byType.set(typeName, info);
  }

  const top = options?.top ?? 30;
  const sorted = [...byType.entries()]
    .sort((a, b) => b[1].allocated - a[1].allocated)
    .slice(0, top);

  return {
    totalAllocated,
    totalFreed: 0,
    byType: new Map(sorted),
    intervals: [],
  };
}
