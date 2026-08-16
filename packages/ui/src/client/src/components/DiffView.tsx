import { useEffect, useState } from 'react'
import { fetchJson } from '../lib/api'

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes)) return String(bytes)
  const sign = bytes < 0 ? '-' : ''
  const abs = Math.abs(bytes)
  const units = ['B', 'KB', 'MB', 'GB']
  let value = abs
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${sign}${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`
}

interface DiffEntry {
  name: string
  baselineSize: number
  profileSize: number
  delta: number
  deltaPct: number | null
}

interface DiffResult {
  baselineTotal: number
  profileTotal: number
  deltaTotal: number
  byFrame?: DiffEntry[]
  byUrl?: DiffEntry[]
  byFunction?: DiffEntry[]
  byNodeName?: DiffEntry[]
  byNodeType?: DiffEntry[]
  objects?: {
    matchedCount: number
    newCount: number
    deletedCount: number
    newSize: number
    deletedSize: number
    deltaSize: number
    newObjects: { id: number; index: number; name: string; type: string; selfSize: number }[]
    deletedObjects: { id: number; index: number; name: string; type: string; selfSize: number }[]
    grownObjects: { id: number; profileIndex: number; name: string; delta: number }[]
  }
}

interface ProfileEntry {
  filePath: string
  fileName: string
  type: string
}

export default function DiffView({
  base,
  currentPath,
  currentType,
}: {
  base: string
  currentPath: string
  currentType: string
}) {
  const [profiles, setProfiles] = useState<ProfileEntry[]>([])
  const [baseline, setBaseline] = useState<string>('')
  const [data, setData] = useState<DiffResult | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    fetchJson<ProfileEntry[]>('/api/profiles', { cache: false })
      .then((p) => setProfiles(p.filter((x) => x.type === currentType)))
      .catch(() => setProfiles([]))
  }, [currentType])

  const runDiff = () => {
    if (!baseline) return
    setLoading(true)
    setError(null)
    fetchJson<DiffResult>(`${base}/diff?baseline=${encodeURIComponent(baseline)}`, { cache: false })
      .then((d) => {
        setData(d)
        setLoading(false)
      })
      .catch((e) => {
        setError(e.message)
        setLoading(false)
      })
  }

  return (
    <div className="space-y-4">
      <div className="flex gap-3 items-center flex-wrap text-sm">
        <span className="text-xs text-gray-400">Compare against baseline:</span>
        <select
          value={baseline}
          onChange={(e) => setBaseline(e.target.value)}
          className="bg-gray-900 border border-gray-700 rounded px-3 py-1.5 text-xs"
        >
          <option value="">— select baseline —</option>
          {profiles
            .filter((p) => p.filePath !== currentPath)
            .map((p) => (
              <option key={p.filePath} value={p.filePath}>
                {p.fileName}
              </option>
            ))}
        </select>
        <button
          onClick={runDiff}
          disabled={!baseline || loading}
          className="px-3 py-1.5 bg-indigo-600 text-white text-xs rounded hover:bg-indigo-700 disabled:opacity-50"
        >
          {loading ? 'Diffing...' : 'Diff'}
        </button>
        {data && (
          <span className="ml-auto text-xs text-gray-400">
            Baseline: {formatBytes(data.baselineTotal)} → Profile:{' '}
            <span className="text-white">{formatBytes(data.profileTotal)}</span> | Δ{' '}
            <span className={data.deltaTotal > 0 ? 'text-red-400' : 'text-emerald-400'}>
              {data.deltaTotal > 0 ? '+' : ''}
              {formatBytes(data.deltaTotal)}
            </span>
          </span>
        )}
      </div>

      {error && <p className="text-red-400 text-sm">Diff failed: {error}</p>}

      {data && (
        <div className="space-y-6">
          {data.byFunction && data.byFunction.length > 0 && (
            <DiffTable title="Function Δ" entries={data.byFunction} />
          )}
          {data.byFrame && data.byFrame.length > 0 && (
            <DiffTable title="Frame Δ" entries={data.byFrame} />
          )}
          {data.byNodeName && data.byNodeName.length > 0 && (
            <DiffTable title="Node Name Δ" entries={data.byNodeName} />
          )}
          {data.byNodeType && data.byNodeType.length > 0 && (
            <DiffTable title="Node Type Δ" entries={data.byNodeType} />
          )}
          {data.objects && <ObjectDiff data={data.objects} />}
        </div>
      )}

      {profiles.length === 0 && (
        <p className="text-xs text-gray-500">
          No other {currentType} profiles loaded. Start the server with multiple files to compare.
        </p>
      )}
    </div>
  )
}

function ObjectDiff({ data }: { data: NonNullable<DiffResult['objects']> }) {
  return (
    <div className="space-y-4">
      <h3 className="text-sm font-semibold text-gray-300">Object identity diff</h3>
      <p className="text-xs text-gray-400">
        {data.matchedCount.toLocaleString()} matched · {data.newCount.toLocaleString()} new ({formatBytes(data.newSize)}) · {data.deletedCount.toLocaleString()} deleted ({formatBytes(data.deletedSize)})
      </p>
      {data.grownObjects.length > 0 && (
        <SimpleObjectTable title="Growing objects" entries={data.grownObjects.map((x) => ({ id: x.id, index: x.profileIndex, name: x.name, type: '', size: x.delta }))} />
      )}
      {data.newObjects.length > 0 && (
        <SimpleObjectTable title="New objects" entries={data.newObjects.map((x) => ({ id: x.id, index: x.index, name: x.name, type: x.type, size: x.selfSize }))} />
      )}
    </div>
  )
}

function SimpleObjectTable({ title, entries }: { title: string; entries: { id: number; index: number; name: string; type: string; size: number }[] }) {
  return (
    <div>
      <h4 className="text-xs text-gray-400 mb-1">{title}</h4>
      <div className="bg-gray-900 rounded-lg overflow-hidden">
        <table className="w-full text-xs">
          <thead><tr className="border-b border-gray-800 text-gray-400"><th className="text-left px-3 py-1">ID</th><th className="text-left">Index</th><th className="text-left">Name</th><th className="text-left">Type</th><th className="text-right px-3">Size/Δ</th></tr></thead>
          <tbody>{entries.slice(0, 50).map((entry) => <tr key={`${entry.id}-${entry.index}`} className="border-b border-gray-800/50"><td className="px-3 py-1">{entry.id}</td><td>{entry.index}</td><td>{entry.name}</td><td>{entry.type}</td><td className={entry.size > 0 ? 'text-red-400 text-right px-3' : 'text-right px-3'}>{formatBytes(entry.size)}</td></tr>)}</tbody>
        </table>
      </div>
    </div>
  )
}

function DiffTable({ title, entries }: { title: string; entries: DiffEntry[] }) {
  const maxAbsDelta = Math.max(...entries.map((e) => Math.abs(e.delta)), 1)
  // Show biggest movers first.
  const sorted = [...entries].sort((a, b) => Math.abs(b.delta) - Math.abs(a.delta)).slice(0, 100)
  return (
    <div>
      <h3 className="text-sm font-semibold text-gray-300 mb-2">{title}</h3>
      <div className="bg-gray-900 rounded-lg overflow-hidden">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-gray-800 text-gray-400">
              <th className="text-left px-4 py-2">Name</th>
              <th className="text-right px-4 py-2">Baseline</th>
              <th className="text-right px-4 py-2">Profile</th>
              <th className="text-right px-4 py-2">Δ</th>
              <th className="text-right px-4 py-2">%</th>
              <th className="px-4 py-2 w-32"></th>
            </tr>
          </thead>
          <tbody>
            {sorted.map((e, i) => {
              const pct = (Math.abs(e.delta) / maxAbsDelta) * 100
              return (
                <tr key={i} className="border-b border-gray-800/50 hover:bg-gray-800/30">
                  <td
                    className="px-4 py-1.5 font-mono text-xs text-indigo-400 truncate max-w-xs"
                    title={e.name}
                  >
                    {e.name}
                  </td>
                  <td className="px-4 py-1.5 text-right font-mono text-xs text-gray-400">
                    {formatBytes(e.baselineSize)}
                  </td>
                  <td className="px-4 py-1.5 text-right font-mono text-xs">
                    {formatBytes(e.profileSize)}
                  </td>
                  <td
                    className={`px-4 py-1.5 text-right font-mono text-xs ${
                      e.delta > 0
                        ? 'text-red-400'
                        : e.delta < 0
                          ? 'text-emerald-400'
                          : 'text-gray-500'
                    }`}
                  >
                    {e.delta > 0 ? '+' : ''}
                    {formatBytes(e.delta)}
                  </td>
                  <td
                    className={`px-4 py-1.5 text-right font-mono text-xs ${
                      e.delta > 0
                        ? 'text-red-400'
                        : e.delta < 0
                          ? 'text-emerald-400'
                          : 'text-gray-500'
                    }`}
                  >
                    {e.deltaPct === null
                      ? 'new'
                      : `${e.deltaPct > 0 ? '+' : ''}${(e.deltaPct * 100).toFixed(1)}%`}
                  </td>
                  <td className="px-4 py-1.5">
                    <div className="h-2 bg-gray-800 rounded-full overflow-hidden relative">
                      <div
                        className={`h-full rounded-full absolute top-0 ${
                          e.delta >= 0 ? 'bg-red-500 left-1/2' : 'bg-emerald-500 right-1/2'
                        }`}
                        style={{ width: `${Math.min(pct / 2, 50)}%` }}
                      />
                      <div className="absolute top-0 left-1/2 w-px h-full bg-gray-700" />
                    </div>
                  </td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>
    </div>
  )
}
