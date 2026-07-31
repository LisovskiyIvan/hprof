import { useEffect, useState } from 'react'
import { fetchJson } from '../lib/api'

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes)) return String(bytes)
  const units = ['B', 'KB', 'MB', 'GB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`
}

interface SummaryData {
  totalSize?: number
  totalCount?: number
  totalAllocated?: number
  totalFreed?: number
  byFrame?: [string, number][]
  byUrl?: [string, number][]
  byFunction?: [string, number][]
  byNodeName?: [string, { size: number; count: number }][]
  byNodeType?: [string, { size: number; count: number }][]
  byType?: [string, { allocated: number; freed: number; count: number }][]
}

interface CumulativeData {
  totalSize: number
  byFrame?: {
    name: string
    selfSize: number
    cumulativeSize: number
    selfPct: number
    cumulativePct: number
  }[]
  byUrl?: {
    name: string
    selfSize: number
    cumulativeSize: number
    selfPct: number
    cumulativePct: number
  }[]
  byFunction?: {
    name: string
    selfSize: number
    cumulativeSize: number
    selfPct: number
    cumulativePct: number
  }[]
}

export default function Summary({ base, type }: { base: string; type: string }) {
  const [data, setData] = useState<SummaryData | null>(null)
  const [cumData, setCumData] = useState<CumulativeData | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [view, setView] = useState<'flat' | 'cumulative'>('flat')
  const [focus, setFocus] = useState('')
  const [ignore, setIgnore] = useState('')
  const [hide, setHide] = useState('')
  const isHeapProfile = type === 'heapprofile'

  const fetchFlat = () => {
    setError(null)
    fetchJson<SummaryData>(`${base}/summary`)
      .then(setData)
      .catch((e) => setError(e.message))
  }

  const fetchCum = () => {
    setError(null)
    const params = new URLSearchParams()
    if (focus) params.set('focus', focus)
    if (ignore) params.set('ignore', ignore)
    if (hide) params.set('hide', hide)
    const qs = params.toString()
    fetchJson<CumulativeData>(`${base}/cumulative${qs ? `?${qs}` : ''}`, { cache: false })
      .then(setCumData)
      .catch((e) => setError(e.message))
  }

  useEffect(() => {
    fetchFlat()
    if (isHeapProfile) fetchCum()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [base])

  // Re-fetch cumulative when filters change.
  useEffect(() => {
    if (isHeapProfile && view === 'cumulative') fetchCum()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focus, ignore, hide, view])

  if (error) return <p className="text-red-400 text-sm">Failed to load summary: {error}</p>
  if (!data) return <p className="text-gray-500">Loading summary...</p>

  if (type === 'heapprofile' && view === 'cumulative' && cumData) {
    return (
      <div className="space-y-6">
        <FilterBar
          focus={focus}
          ignore={ignore}
          hide={hide}
          setFocus={setFocus}
          setIgnore={setIgnore}
          setHide={setHide}
          onApply={fetchCum}
        />
        <ViewToggle view={view} setView={setView} />
        <div className="text-sm text-gray-400">
          Total: <span className="text-white font-semibold">{formatBytes(cumData.totalSize)}</span>
        </div>
        <CumulativeTable title="Top Frames (cumulative)" entries={cumData.byFrame ?? []} />
        <CumulativeTable title="Top Functions (cumulative)" entries={cumData.byFunction ?? []} />
      </div>
    )
  }

  if (type === 'heapprofile') {
    return (
      <div className="space-y-6">
        <FilterBar
          focus={focus}
          ignore={ignore}
          hide={hide}
          setFocus={setFocus}
          setIgnore={setIgnore}
          setHide={setHide}
          onApply={() => {
            setView('cumulative')
            fetchCum()
          }}
        />
        <ViewToggle view={view} setView={setView} />
        <div className="space-y-8">
          <div className="text-sm text-gray-400">
            Total sampled:{' '}
            <span className="text-white font-semibold">{formatBytes(data.totalSize ?? 0)}</span>
          </div>
          <Table
            title="Top Frames"
            rows={data.byFrame ?? []}
            format={(v) => formatBytes(v)}
            total={data.totalSize}
          />
          <Table
            title="Top URLs"
            rows={data.byUrl ?? []}
            format={(v) => formatBytes(v)}
            total={data.totalSize}
          />
          <Table
            title="Top Functions"
            rows={data.byFunction ?? []}
            format={(v) => formatBytes(v)}
            total={data.totalSize}
          />
        </div>
      </div>
    )
  }

  if (type === 'heapsnapshot') {
    const typeRows = data.byNodeType ?? []
    const totalTypeSize = typeRows.reduce((s, [, v]) => s + v.size, 0)
    return (
      <div className="space-y-8">
        <div className="text-sm text-gray-400">
          Total self size:{' '}
          <span className="text-white font-semibold">{formatBytes(data.totalSize ?? 0)}</span>
          {' | '}Nodes: <span className="text-white">{data.totalCount?.toLocaleString()}</span>
        </div>
        <SizeBarChart rows={typeRows} total={totalTypeSize} />
        <Table
          title="Top Node Names"
          rows={(data.byNodeName ?? []).map(
            ([name, info]) => [name, info.size] as [string, number],
          )}
          format={(v) => formatBytes(v)}
          total={data.totalSize}
        />
      </div>
    )
  }

  if (type === 'heaptimeline') {
    return (
      <div className="space-y-8">
        <div className="text-sm text-gray-400">
          Total allocated:{' '}
          <span className="text-white font-semibold">{formatBytes(data.totalAllocated ?? 0)}</span>
        </div>
        <Table
          title="Allocations By Type"
          rows={(data.byType ?? []).map(([t, info]) => [t, info.allocated] as [string, number])}
          format={(v) => formatBytes(v)}
          total={data.totalAllocated}
        />
      </div>
    )
  }

  return null
}

function FilterBar({
  focus,
  ignore,
  hide,
  setFocus,
  setIgnore,
  setHide,
  onApply,
}: {
  focus: string
  ignore: string
  hide: string
  setFocus: (v: string) => void
  setIgnore: (v: string) => void
  setHide: (v: string) => void
  onApply: () => void
}) {
  return (
    <div className="flex gap-3 items-center flex-wrap text-sm">
      <input
        type="text"
        placeholder="focus: regex"
        value={focus}
        onChange={(e) => setFocus(e.target.value)}
        onKeyDown={(e) => e.key === 'Enter' && onApply()}
        className="bg-gray-900 border border-gray-700 rounded px-3 py-1.5 text-xs w-44 focus:border-indigo-500 outline-none"
      />
      <input
        type="text"
        placeholder="ignore: drop flat"
        value={ignore}
        onChange={(e) => setIgnore(e.target.value)}
        onKeyDown={(e) => e.key === 'Enter' && onApply()}
        className="bg-gray-900 border border-gray-700 rounded px-3 py-1.5 text-xs w-44 focus:border-indigo-500 outline-none"
      />
      <input
        type="text"
        placeholder="hide: drop from view"
        value={hide}
        onChange={(e) => setHide(e.target.value)}
        onKeyDown={(e) => e.key === 'Enter' && onApply()}
        className="bg-gray-900 border border-gray-700 rounded px-3 py-1.5 text-xs w-44 focus:border-indigo-500 outline-none"
      />
      <button
        onClick={onApply}
        className="px-3 py-1.5 bg-indigo-600 text-white text-xs rounded hover:bg-indigo-700"
      >
        Apply (cumulative)
      </button>
      <span className="text-xs text-gray-500">
        Filters apply to cumulative view. See Go pprof docs for semantics.
      </span>
    </div>
  )
}

function ViewToggle({
  view,
  setView,
}: {
  view: 'flat' | 'cumulative'
  setView: (v: 'flat' | 'cumulative') => void
}) {
  return (
    <div className="inline-flex border border-gray-700 rounded overflow-hidden text-xs">
      <button
        onClick={() => setView('flat')}
        className={`px-3 py-1.5 ${view === 'flat' ? 'bg-indigo-600 text-white' : 'bg-gray-900 text-gray-300'}`}
      >
        Flat
      </button>
      <button
        onClick={() => setView('cumulative')}
        className={`px-3 py-1.5 ${view === 'cumulative' ? 'bg-indigo-600 text-white' : 'bg-gray-900 text-gray-300'}`}
      >
        Cumulative
      </button>
    </div>
  )
}

function CumulativeTable({
  title,
  entries,
}: {
  title: string
  entries: {
    name: string
    selfSize: number
    cumulativeSize: number
    selfPct: number
    cumulativePct: number
  }[]
}) {
  if (!entries.length) return null
  return (
    <div>
      <h3 className="text-sm font-semibold text-gray-300 mb-2">{title}</h3>
      <div className="bg-gray-900 rounded-lg overflow-hidden">
        <div className="grid grid-cols-[1fr_80px_60px_100px_60px] gap-2 px-4 py-2 border-b border-gray-800 text-xs text-gray-400">
          <span>Name</span>
          <span className="text-right">Self</span>
          <span className="text-right">Self %</span>
          <span className="text-right">Cumulative</span>
          <span className="text-right">Cum %</span>
        </div>
        {entries.slice(0, 30).map((e, i) => (
          <div
            key={i}
            className="grid grid-cols-[1fr_80px_60px_100px_60px] gap-2 px-4 py-1.5 border-b border-gray-800/50 last:border-0 text-xs"
          >
            <span className="font-mono text-indigo-400 truncate" title={e.name}>
              {e.name}
            </span>
            <span className="text-right text-gray-300 font-mono">{formatBytes(e.selfSize)}</span>
            <span className="text-right text-gray-500 font-mono">{e.selfPct.toFixed(1)}%</span>
            <span className="text-right text-white font-mono">{formatBytes(e.cumulativeSize)}</span>
            <span className="text-right text-indigo-400 font-mono">
              {e.cumulativePct.toFixed(1)}%
            </span>
          </div>
        ))}
      </div>
    </div>
  )
}

function Table({
  title,
  rows,
  format,
  total,
}: {
  title: string
  rows: [string, number][]
  format: (v: number) => string
  total?: number
}) {
  if (!rows.length) return null
  return (
    <div>
      <h3 className="text-sm font-semibold text-gray-300 mb-2">{title}</h3>
      <div className="bg-gray-900 rounded-lg overflow-hidden">
        {rows.slice(0, 30).map(([key, value], i) => (
          <div
            key={i}
            className="flex items-center px-4 py-2 border-b border-gray-800 last:border-0 text-sm"
          >
            <span className="text-indigo-400 font-mono flex-1 truncate" title={key}>
              {key}
            </span>
            {total !== undefined && total > 0 && (
              <span className="text-gray-600 text-xs ml-3 mr-3 w-12 text-right font-mono">
                {((value / total) * 100).toFixed(1)}%
              </span>
            )}
            <span className="text-gray-300 ml-4 whitespace-nowrap">{format(value)}</span>
          </div>
        ))}
      </div>
    </div>
  )
}

function SizeBarChart({
  rows,
  total,
}: {
  rows: [string, { size: number; count: number }][]
  total: number
}) {
  if (!rows.length) return null
  const chartRows = rows.map(([type, info], i) => ({
    type,
    info,
    color: `hsl(${(i * 47) % 360} 78% 58%)`,
  }))

  return (
    <div>
      <h3 className="text-sm font-semibold text-gray-300 mb-2">Size by Node Type</h3>
      <div className="bg-gray-900 rounded-lg p-4">
        <div className="flex h-8 rounded overflow-hidden mb-3">
          {chartRows.map(({ type, info, color }) => {
            const pct = total > 0 ? (info.size / total) * 100 : 0
            if (pct < 0.5) return null
            return (
              <div
                key={type}
                className="h-full border-r border-gray-950/80 last:border-r-0"
                style={{ width: `${pct}%`, backgroundColor: color }}
                title={`${type}: ${formatBytes(info.size)} (${pct.toFixed(1)}%)`}
              />
            )
          })}
        </div>
        <div className="grid grid-cols-2 gap-x-6 gap-y-1">
          {chartRows.map(({ type, info, color }) => (
            <div key={type} className="flex items-center text-xs gap-2">
              <span
                className="w-2.5 h-2.5 rounded-sm shrink-0"
                style={{ backgroundColor: color }}
              />
              <span className="text-gray-400 truncate">{type}</span>
              <span className="text-gray-300 ml-auto whitespace-nowrap">
                {formatBytes(info.size)}
              </span>
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}
