import { useEffect, useState } from 'react'
import { fetchJson } from '../lib/api'

interface Insights {
  detached?: { totalCount: number; totalSize: number; entries: { node: { index: number; name: string; type: string; selfSize: number }; ownerChain: string }[] }
  histogram?: { buckets: { minSize: number; maxSize: number; count: number; totalSize: number }[] }
  strings?: { entries: { value: string; references: number; referencedBytes: number }[] }
}

interface EdgeMatch {
  sourceIndex: number
  sourceName: string
  sourceType: string
  edgeType: string
  name: string
  targetIndex: number
  targetName: string
}

function formatBytes(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB']
  let value = Math.abs(bytes)
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${bytes < 0 ? '-' : ''}${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`
}

export default function SnapshotInsights({ base }: { base: string }) {
  const [data, setData] = useState<Insights>({})
  const [edgeQuery, setEdgeQuery] = useState('')
  const [edges, setEdges] = useState<EdgeMatch[]>([])
  const [loading, setLoading] = useState(false)

  useEffect(() => {
    let cancelled = false
    Promise.all([
      fetchJson<Insights['detached']>(`${base}/detached?top=20&depth=4`),
      fetchJson<Insights['histogram']>(`${base}/histogram`),
      fetchJson<Insights['strings']>(`${base}/strings?top=20`),
    ]).then(([detached, histogram, strings]) => {
      if (!cancelled) setData({ detached, histogram, strings })
    }).catch(() => {
      if (!cancelled) setData({})
    })
    return () => { cancelled = true }
  }, [base])

  const searchEdges = () => {
    if (!edgeQuery) return
    setLoading(true)
    fetchJson<EdgeMatch[]>(`${base}/edge-search?name=${encodeURIComponent(edgeQuery)}&top=50`, { cache: false })
      .then(setEdges)
      .finally(() => setLoading(false))
  }

  return (
    <div className="space-y-6">
      <div className="grid gap-4 lg:grid-cols-3">
        <section className="bg-gray-900 rounded-lg p-4">
          <h3 className="font-semibold mb-2">Detached nodes</h3>
          <p className="text-xs text-gray-400 mb-2">
            {data.detached?.totalCount ?? 0} nodes · {formatBytes(data.detached?.totalSize ?? 0)}
          </p>
          <div className="space-y-1 text-xs">
            {(data.detached?.entries ?? []).slice(0, 8).map((entry) => (
              <div key={entry.node.index} className="border-b border-gray-800 py-1">
                <span className="text-indigo-400">#{entry.node.index}</span> {entry.node.name || '(anonymous)'}
                <span className="text-gray-500"> · {formatBytes(entry.node.selfSize)}</span>
                {entry.ownerChain && <div className="text-gray-500 truncate">↳ {entry.ownerChain}</div>}
              </div>
            ))}
          </div>
        </section>

        <section className="bg-gray-900 rounded-lg p-4">
          <h3 className="font-semibold mb-2">Size histogram</h3>
          <div className="space-y-1 text-xs">
            {(data.histogram?.buckets ?? []).slice(-10).map((bucket) => (
              <div key={bucket.minSize} className="flex justify-between border-b border-gray-800 py-1">
                <span>{formatBytes(bucket.minSize)}–{formatBytes(bucket.maxSize)}</span>
                <span className="text-gray-400">{bucket.count.toLocaleString()} · {formatBytes(bucket.totalSize)}</span>
              </div>
            ))}
          </div>
        </section>

        <section className="bg-gray-900 rounded-lg p-4">
          <h3 className="font-semibold mb-2">Repeated strings</h3>
          <div className="space-y-1 text-xs">
            {(data.strings?.entries ?? []).slice(0, 10).map((entry) => (
              <div key={entry.value} className="border-b border-gray-800 py-1 truncate" title={entry.value}>
                <span className="text-indigo-400">{entry.references.toLocaleString()}×</span> {entry.value || '(empty)'}
                <span className="text-gray-500"> · {formatBytes(entry.referencedBytes)}</span>
              </div>
            ))}
          </div>
        </section>
      </div>

      <section className="bg-gray-900 rounded-lg p-4">
        <h3 className="font-semibold mb-2">Search property / edge names</h3>
        <div className="flex gap-2">
          <input value={edgeQuery} onChange={(e) => setEdgeQuery(e.target.value)} placeholder="cache, parent, [0]…" className="flex-1 bg-gray-950 border border-gray-700 rounded px-3 py-1.5 text-sm" />
          <button onClick={searchEdges} disabled={loading || !edgeQuery} className="px-3 py-1.5 bg-indigo-600 rounded text-sm disabled:opacity-50">{loading ? 'Searching…' : 'Search'}</button>
        </div>
        {edges.length > 0 && (
          <div className="mt-3 overflow-auto">
            <table className="w-full text-xs">
              <thead><tr className="text-gray-400 border-b border-gray-800"><th className="text-left py-1">Source</th><th className="text-left">Edge</th><th className="text-left">Target</th></tr></thead>
              <tbody>{edges.map((edge, index) => <tr key={`${edge.sourceIndex}-${edge.targetIndex}-${index}`} className="border-b border-gray-800/50"><td className="py-1">#{edge.sourceIndex} {edge.sourceName} <span className="text-gray-500">({edge.sourceType})</span></td><td>{edge.edgeType}:{edge.name}</td><td>#{edge.targetIndex} {edge.targetName}</td></tr>)}</tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  )
}
