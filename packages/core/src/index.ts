export type ProfileType = 'heapsnapshot' | 'heapprofile' | 'heaptimeline'

export interface ProfileMeta {
  fileName: string
  fileSize: number
  type: ProfileType
}

export { HeapProfile } from './heapprofile.ts'
export type {
  HeapProfileResult,
  HeapProfileSummary,
  FlatCallFrame,
  FilterOptions,
  CumulativeEntry,
  CumulativeSummary,
  FlamegraphFrame,
  TreemapNode,
  DiffEntry,
  ProfileDiff,
} from './heapprofile.ts'

export { HeapSnapshot } from './heapsnapshot.ts'
export type {
  HeapSnapshotMeta,
  HeapSnapshotNode,
  HeapSnapshotEdge,
  HeapSnapshotResult,
  HeapSnapshotSummary,
  HeapSnapshotNodePage,
  HeapSnapshotNodePageOptions,
  HeapSnapshotSearchMatch,
  HeapSnapshotRetainedEntry,
  HeapSnapshotNameMatch,
  HeapSnapshotProperty,
  HeapSnapshotRetainer,
  HeapSnapshotRetainerChainNode,
  HeapSnapshotOwnerGroup,
  HeapSnapshotOwnerAnalysis,
  SnapshotDiff,
  SnapshotObject,
  SnapshotObjectChange,
  SnapshotObjectDiff,
  DetachedNode,
  DetachedSummary,
  SizeHistogram,
  StringStats,
  HeapSnapshotEdgeMatch,
} from './heapsnapshot.ts'

export { HeapTimeline } from './heaptimeline.ts'
export type { HeapTimelineResult, TimelineEntry, HeapTimelineSummary } from './heaptimeline.ts'

export { detectType as detectProfileType, formatBytesNative as formatBytes } from './ffi.ts'
