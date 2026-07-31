import { dlopen, FFIType, ptr, read, type Pointer, CString } from 'bun:ffi'
import { existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

function getLibFilename(): string {
  switch (process.platform) {
    case 'darwin':
      return 'libhprof_c.dylib'
    case 'win32':
      return 'hprof_c.dll'
    default:
      return 'libhprof_c.so'
  }
}

function getNpmPackageName(): string {
  const platform = process.platform
  const arch = process.arch
  if (platform === 'darwin') return `@hprof/hprof-bin-darwin-${arch === 'arm64' ? 'arm64' : 'x64'}`
  if (platform === 'linux')
    return `@hprof/hprof-bin-linux-${arch === 'arm64' ? 'arm64' : 'x64'}-gnu`
  if (platform === 'win32') return `@hprof/hprof-bin-win32-${arch === 'arm64' ? 'arm64' : 'x64'}`
  throw new Error(`Unsupported platform: ${platform}`)
}

function findBinary(): string | null {
  const libName = getLibFilename()
  const pkgDir = dirname(fileURLToPath(import.meta.url))

  const devPaths = [
    join(pkgDir, '..', '..', '..', 'target', 'release', libName),
    join(pkgDir, '..', '..', '..', 'target', 'debug', libName),
  ]
  for (const p of devPaths) {
    if (existsSync(p)) return p
  }

  try {
    const require = createRequire(join(pkgDir, 'package.json'))
    const packageName = getNpmPackageName()
    const packageJsonPath = require.resolve(`${packageName}/package.json`)
    const binaryPath = join(dirname(packageJsonPath), libName)
    if (existsSync(binaryPath)) return binaryPath
  } catch {}

  return null
}

const ffiDefinition = {
  hprof_snapshot_open: { args: [FFIType.cstring], returns: FFIType.ptr },
  hprof_snapshot_meta: { args: [FFIType.ptr], returns: FFIType.ptr },
  hprof_snapshot_summary: {
    args: [FFIType.ptr, FFIType.u32, FFIType.cstring],
    returns: FFIType.ptr,
  },
  hprof_snapshot_node_page: {
    args: [
      FFIType.ptr,
      FFIType.u32,
      FFIType.u32,
      FFIType.cstring,
      FFIType.cstring,
      FFIType.u8,
      FFIType.u8,
    ],
    returns: FFIType.ptr,
  },
  hprof_snapshot_edges: { args: [FFIType.ptr, FFIType.u32], returns: FFIType.ptr },
  hprof_snapshot_search: { args: [FFIType.ptr, FFIType.cstring], returns: FFIType.ptr },
  hprof_snapshot_retained: { args: [FFIType.ptr, FFIType.u32], returns: FFIType.ptr },
  hprof_snapshot_flamegraph: {
    args: [FFIType.ptr, FFIType.u32, FFIType.cstring],
    returns: FFIType.ptr,
  },
  hprof_snapshot_treemap: {
    args: [FFIType.ptr, FFIType.u32, FFIType.cstring],
    returns: FFIType.ptr,
  },
  hprof_snapshot_diff: { args: [FFIType.ptr, FFIType.ptr], returns: FFIType.ptr },
  hprof_snapshot_destroy: { args: [FFIType.ptr], returns: FFIType.void },

  hprof_profile_open: { args: [FFIType.cstring], returns: FFIType.ptr },
  hprof_profile_data: { args: [FFIType.ptr], returns: FFIType.ptr },
  hprof_profile_summarize: {
    args: [FFIType.ptr, FFIType.u32, FFIType.cstring],
    returns: FFIType.ptr,
  },
  hprof_profile_summarize_cumulative: {
    args: [FFIType.ptr, FFIType.u32, FFIType.cstring, FFIType.cstring, FFIType.cstring],
    returns: FFIType.ptr,
  },
  hprof_profile_flatten: { args: [FFIType.ptr], returns: FFIType.ptr },
  hprof_profile_flamegraph: {
    args: [FFIType.ptr, FFIType.cstring, FFIType.cstring, FFIType.cstring],
    returns: FFIType.ptr,
  },
  hprof_profile_dot: {
    args: [FFIType.ptr, FFIType.u32, FFIType.cstring, FFIType.cstring, FFIType.cstring],
    returns: FFIType.ptr,
  },
  hprof_profile_treemap: {
    args: [FFIType.ptr, FFIType.cstring, FFIType.cstring, FFIType.cstring],
    returns: FFIType.ptr,
  },
  hprof_profile_diff: { args: [FFIType.ptr, FFIType.ptr], returns: FFIType.ptr },
  hprof_profile_destroy: { args: [FFIType.ptr], returns: FFIType.void },

  hprof_timeline_open: { args: [FFIType.cstring], returns: FFIType.ptr },
  hprof_timeline_meta: { args: [FFIType.ptr], returns: FFIType.ptr },
  hprof_timeline_summary: {
    args: [FFIType.ptr, FFIType.u32, FFIType.cstring],
    returns: FFIType.ptr,
  },
  hprof_timeline_destroy: { args: [FFIType.ptr], returns: FFIType.void },

  hprof_detect_type: { args: [FFIType.cstring], returns: FFIType.ptr },
  hprof_format_bytes: { args: [FFIType.u64], returns: FFIType.ptr },
  hprof_free_result: { args: [FFIType.ptr], returns: FFIType.void },
  hprof_free_string: { args: [FFIType.ptr], returns: FFIType.void },
} as const

type Lib = ReturnType<typeof dlopen<typeof ffiDefinition>>
let lib: Lib | null = null

function loadLib(): Lib {
  if (lib) return lib
  const binaryPath = findBinary()
  if (!binaryPath)
    throw new Error(
      'hprof native library not found. Build from source with `cargo build --release -p hprof-c`',
    )
  lib = dlopen(binaryPath, ffiDefinition)
  return lib
}

const RES_SUCCESS = 0
const RES_ERROR = 8
const RES_HANDLE = 16

function encode(s: string): Uint8Array {
  return new TextEncoder().encode(`${s}\0`)
}

function readCString(p: Pointer | number | null): string | null {
  if (!p || p === 0) return null
  return new CString(p as unknown as Pointer).toString()
}

function readResult(
  p: Pointer | null,
): { ok: true; handle: number } | { ok: false; error: string } {
  if (!p) return { ok: false, error: 'FFI returned null' }
  const success = read.u8(p, RES_SUCCESS) !== 0
  const library = loadLib()
  if (!success) {
    const errorPtr = read.ptr(p, RES_ERROR)
    const error = readCString(errorPtr) || 'Unknown error'
    library.symbols.hprof_free_result(p)
    return { ok: false, error }
  }
  const handle = read.ptr(p, RES_HANDLE)
  library.symbols.hprof_free_result(p)
  return { ok: true, handle }
}

function callString(p: Pointer | null): string {
  const r = readResult(p)
  if (!r.ok) throw new Error(r.error)
  const str = readCString(r.handle)
  loadLib().symbols.hprof_free_string(r.handle as unknown as Pointer)
  return str ?? ''
}

export type NativeHandle = Pointer

export function snapshotOpen(path: string): NativeHandle {
  const r = readResult(loadLib().symbols.hprof_snapshot_open(ptr(encode(path))))
  if (!r.ok) throw new Error(r.error)
  return r.handle as unknown as Pointer
}

export function snapshotMeta(handle: NativeHandle): any {
  return JSON.parse(callString(loadLib().symbols.hprof_snapshot_meta(handle)))
}

export function snapshotSummary(handle: NativeHandle, top = 30, filter?: string): any {
  return JSON.parse(
    callString(loadLib().symbols.hprof_snapshot_summary(handle, top, ptr(encode(filter ?? '')))),
  )
}

export function snapshotNodePage(
  handle: NativeHandle,
  options: {
    page?: number
    pageSize?: number
    type?: string | null
    q?: string | null
    sort?: 'id' | 'type' | 'name' | 'selfSize' | 'edgeCount'
    dir?: 'asc' | 'desc'
  } = {},
): any {
  const sortMap: Record<string, number> = { selfSize: 0, id: 1, type: 2, name: 3, edgeCount: 4 }
  const sortVal = sortMap[options.sort ?? 'selfSize'] ?? 0
  const dirVal = options.dir === 'asc' ? 1 : 0
  return JSON.parse(
    callString(
      loadLib().symbols.hprof_snapshot_node_page(
        handle,
        options.page ?? 0,
        options.pageSize ?? 100,
        ptr(encode(options.type ?? '')),
        ptr(encode(options.q ?? '')),
        sortVal,
        dirVal,
      ),
    ),
  )
}

export function snapshotEdges(handle: NativeHandle, nodeIndex: number): any {
  return JSON.parse(callString(loadLib().symbols.hprof_snapshot_edges(handle, nodeIndex)))
}

export function snapshotSearch(handle: NativeHandle, query: string): any {
  return JSON.parse(callString(loadLib().symbols.hprof_snapshot_search(handle, ptr(encode(query)))))
}

export function snapshotRetained(handle: NativeHandle, topN = 30): any {
  return JSON.parse(callString(loadLib().symbols.hprof_snapshot_retained(handle, topN)))
}

export function snapshotFlamegraph(handle: NativeHandle, top?: number, filter?: string): any {
  return JSON.parse(
    callString(
      loadLib().symbols.hprof_snapshot_flamegraph(handle, top ?? 0, ptr(encode(filter ?? ''))),
    ),
  )
}

export function snapshotTreemap(handle: NativeHandle, top?: number, filter?: string): any {
  return JSON.parse(
    callString(
      loadLib().symbols.hprof_snapshot_treemap(handle, top ?? 0, ptr(encode(filter ?? ''))),
    ),
  )
}

export function snapshotDiff(handle: NativeHandle, baselineHandle: NativeHandle): any {
  return JSON.parse(callString(loadLib().symbols.hprof_snapshot_diff(handle, baselineHandle)))
}

export function snapshotDestroy(handle: NativeHandle): void {
  loadLib().symbols.hprof_snapshot_destroy(handle)
}

export function profileOpen(path: string): NativeHandle {
  const r = readResult(loadLib().symbols.hprof_profile_open(ptr(encode(path))))
  if (!r.ok) throw new Error(r.error)
  return r.handle as unknown as Pointer
}

export function profileData(handle: NativeHandle): any {
  return JSON.parse(callString(loadLib().symbols.hprof_profile_data(handle)))
}

export function profileSummarize(handle: NativeHandle, top?: number, filter?: string): any {
  return JSON.parse(
    callString(
      loadLib().symbols.hprof_profile_summarize(handle, top ?? 0, ptr(encode(filter ?? ''))),
    ),
  )
}

export function profileFlatten(handle: NativeHandle): any {
  return JSON.parse(callString(loadLib().symbols.hprof_profile_flatten(handle)))
}

export function profileSummarizeCumulative(
  handle: NativeHandle,
  options?: { top?: number; focus?: string; ignore?: string; hide?: string },
): any {
  return JSON.parse(
    callString(
      loadLib().symbols.hprof_profile_summarize_cumulative(
        handle,
        options?.top ?? 0,
        ptr(encode(options?.focus ?? '')),
        ptr(encode(options?.ignore ?? '')),
        ptr(encode(options?.hide ?? '')),
      ),
    ),
  )
}

export function profileFlamegraph(
  handle: NativeHandle,
  options?: { focus?: string; ignore?: string; hide?: string },
): any {
  return JSON.parse(
    callString(
      loadLib().symbols.hprof_profile_flamegraph(
        handle,
        ptr(encode(options?.focus ?? '')),
        ptr(encode(options?.ignore ?? '')),
        ptr(encode(options?.hide ?? '')),
      ),
    ),
  )
}

export function profileDot(
  handle: NativeHandle,
  options?: { top?: number; focus?: string; ignore?: string; hide?: string },
): string {
  // Return raw DOT text (not JSON).
  return callString(
    loadLib().symbols.hprof_profile_dot(
      handle,
      options?.top ?? 0,
      ptr(encode(options?.focus ?? '')),
      ptr(encode(options?.ignore ?? '')),
      ptr(encode(options?.hide ?? '')),
    ),
  )
}

export function profileTreemap(
  handle: NativeHandle,
  options?: { focus?: string; ignore?: string; hide?: string },
): any {
  return JSON.parse(
    callString(
      loadLib().symbols.hprof_profile_treemap(
        handle,
        ptr(encode(options?.focus ?? '')),
        ptr(encode(options?.ignore ?? '')),
        ptr(encode(options?.hide ?? '')),
      ),
    ),
  )
}

export function profileDiff(handle: NativeHandle, baselineHandle: NativeHandle): any {
  return JSON.parse(callString(loadLib().symbols.hprof_profile_diff(handle, baselineHandle)))
}

export function profileDestroy(handle: NativeHandle): void {
  loadLib().symbols.hprof_profile_destroy(handle)
}

export function timelineOpen(path: string): NativeHandle {
  const r = readResult(loadLib().symbols.hprof_timeline_open(ptr(encode(path))))
  if (!r.ok) throw new Error(r.error)
  return r.handle as unknown as Pointer
}

export function timelineMeta(handle: NativeHandle): any {
  return JSON.parse(callString(loadLib().symbols.hprof_timeline_meta(handle)))
}

export function timelineSummary(handle: NativeHandle, top?: number, filter?: string): any {
  return JSON.parse(
    callString(
      loadLib().symbols.hprof_timeline_summary(handle, top ?? 0, ptr(encode(filter ?? ''))),
    ),
  )
}

export function timelineDestroy(handle: NativeHandle): void {
  loadLib().symbols.hprof_timeline_destroy(handle)
}

export function detectType(path: string): string {
  return callString(loadLib().symbols.hprof_detect_type(ptr(encode(path))))
}

export function formatBytesNative(bytes: number): string {
  return callString(loadLib().symbols.hprof_format_bytes(BigInt(bytes)))
}

export function isAvailable(): boolean {
  try {
    loadLib()
    return true
  } catch {
    return false
  }
}
