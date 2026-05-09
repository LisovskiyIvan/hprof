import { useEffect, useState } from 'react'
import { fetchJson } from '../lib/api'

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`
}

interface RetainedEntry {
  nodeIndex: number
  name: string
  type: string
  selfSize: number
  retainedSize: number
  approximate: boolean
}

export default function RetainedSize({ base }: { base: string }) {
  const [data, setData] = useState<RetainedEntry[] | null>(null)
  const [approximate, setApproximate] = useState(false)
  const [top, setTop] = useState(50)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    setLoading(true)
    setError(null)
    fetchJson<{ retained: RetainedEntry[]; approximate?: boolean }>(`${base}/retained?top=${top}`)
      .then((d) => {
        setData(d.retained)
        setApproximate(Boolean(d.approximate))
        setLoading(false)
      })
      .catch((e) => {
        setLoading(false)
        setError(e.message)
      })
  }, [base, top])

  if (loading)
    return (
      <p className="text-gray-500">
        Computing retained sizes (this may take a moment for large snapshots)...
      </p>
    )
  if (error) return <p className="text-red-400 text-sm">Failed to load retained sizes: {error}</p>
  if (!data) return null

  const maxRetained = data[0]?.retainedSize ?? 1

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-3">
        <label className="text-xs text-gray-400">Show top:</label>
        <select
          value={top}
          onChange={(e) => setTop(Number(e.target.value))}
          className="bg-gray-900 border border-gray-700 rounded px-2 py-1 text-sm"
        >
          <option value={20}>20</option>
          <option value={50}>50</option>
          <option value={100}>100</option>
          <option value={200}>200</option>
        </select>
        <span className="text-xs text-gray-500">
          Retained size = self size + size of objects exclusively retained
        </span>
      </div>

      {approximate && (
        <p className="text-xs text-amber-300">
          Large snapshot mode: retained sizes are approximated from top self-size nodes to keep the
          view responsive under Bun.
        </p>
      )}

      <div className="bg-gray-900 rounded-lg overflow-hidden">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-gray-800 text-gray-400">
              <th className="text-left px-4 py-2">#</th>
              <th className="text-left px-4 py-2">Name</th>
              <th className="text-left px-4 py-2">Type</th>
              <th className="text-right px-4 py-2">Self</th>
              <th className="text-right px-4 py-2">Retained</th>
              <th className="px-4 py-2 w-32"></th>
            </tr>
          </thead>
          <tbody>
            {data.map((entry, i) => {
              const pct = (entry.retainedSize / maxRetained) * 100
              const selfPct = maxRetained > 0 ? (entry.selfSize / entry.retainedSize) * 100 : 0
              return (
                <tr
                  key={entry.nodeIndex}
                  className="border-b border-gray-800/50 hover:bg-gray-800/30"
                >
                  <td className="px-4 py-1.5 text-gray-500 font-mono text-xs">{i + 1}</td>
                  <td
                    className="px-4 py-1.5 font-mono text-xs text-indigo-400 truncate max-w-xs"
                    title={entry.name}
                  >
                    {entry.name}
                  </td>
                  <td className="px-4 py-1.5">
                    <span className="px-1.5 py-0.5 rounded text-xs bg-gray-800 text-gray-300">
                      {entry.type}
                    </span>
                  </td>
                  <td className="px-4 py-1.5 text-right font-mono text-xs">
                    {formatBytes(entry.selfSize)}
                  </td>
                  <td className="px-4 py-1.5 text-right font-mono text-xs font-semibold">
                    {formatBytes(entry.retainedSize)}
                  </td>
                  <td className="px-4 py-1.5">
                    <div className="h-2 bg-gray-800 rounded-full overflow-hidden relative">
                      <div
                        className="h-full bg-indigo-500/40 rounded-full absolute top-0 left-0"
                        style={{ width: `${pct}%` }}
                      />
                      <div
                        className="h-full bg-indigo-500 rounded-full absolute top-0 left-0"
                        style={{ width: `${(pct * selfPct) / 100}%` }}
                      />
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
