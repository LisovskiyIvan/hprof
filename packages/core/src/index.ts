export interface ProfileMeta {
  fileName: string;
  fileSize: number;
  type: ProfileType;
}

export type ProfileType = "heapsnapshot" | "heapprofile" | "heaptimeline";

export function detectProfileType(filePath: string): ProfileType {
  const ext = filePath.split(".").pop()?.toLowerCase();
  switch (ext) {
    case "heapsnapshot":
      return "heapsnapshot";
    case "heapprofile":
      return "heapprofile";
    case "heaptimeline":
      return "heaptimeline";
    default:
      throw new Error(`Unsupported file type: .${ext}`);
  }
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes)) return String(bytes);
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`;
}

export { HeapProfile } from "./heapprofile.ts";
export type {
  CallFrame,
  HeapProfileNode,
  HeapProfileResult,
  HeapProfileSummary,
  FlatCallFrame,
} from "./heapprofile.ts";

export { HeapSnapshot } from "./heapsnapshot.ts";
export type {
  HeapSnapshotMeta,
  HeapSnapshotNode,
  HeapSnapshotEdge,
  HeapSnapshotResult,
  HeapSnapshotSummary,
} from "./heapsnapshot.ts";

export { HeapTimeline } from "./heaptimeline.ts";
export type {
  HeapTimelineResult,
  TimelineEntry,
  HeapTimelineSummary,
  TimeInterval,
} from "./heaptimeline.ts";
