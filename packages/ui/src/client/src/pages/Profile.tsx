import { useEffect, useState } from 'react'
import { useParams } from 'react-router'
import Summary from '../components/Summary'
import NodesTable from '../components/NodesTable'
import TreeView from '../components/TreeView'
import Search from '../components/Search'
import Timeline from '../components/Timeline'
import RetainedSize from '../components/RetainedSize'
import Flamegraph from '../components/Flamegraph'
import CallGraph from '../components/CallGraph'
import Treemap from '../components/Treemap'
import DiffView from '../components/DiffView'
import SourceListing from '../components/SourceListing'
import SnapshotInsights from '../components/SnapshotInsights'
import { fetchJson } from '../lib/api'

type Tab =
  | 'summary'
  | 'flamegraph'
  | 'callgraph'
  | 'treemap'
  | 'locations'
  | 'nodes'
  | 'tree'
  | 'timeline'
  | 'retained'
  | 'diff'
  | 'search'
  | 'insights'

interface Meta {
  fileName: string
  fileSize: number
  type: string
  node_count?: number
  edge_count?: number
}

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

export default function Profile() {
  const { id } = useParams<{ id: string }>()
  const filePath = id ? decodeURIComponent(id) : ''
  const [meta, setMeta] = useState<Meta | null>(null)
  const [tab, setTab] = useState<Tab>('summary')
  const [error, setError] = useState<string | null>(null)

  const base = `/api/profile/${encodeURIComponent(filePath)}`

  useEffect(() => {
    if (!filePath) {
      setMeta(null)
      return
    }

    let cancelled = false

    setMeta(null)
    setError(null)
    fetchJson<Meta>(`${base}/meta`)
      .then((nextMeta) => {
        if (!cancelled) {
          setMeta(nextMeta)
        }
      })
      .catch((e) => {
        if (!cancelled) {
          setError(e.message)
        }
      })

    return () => {
      cancelled = true
    }
  }, [base, filePath])

  const tabs: { key: Tab; label: string; show: boolean }[] = [
    { key: 'summary', label: 'Summary', show: true },
    {
      key: 'flamegraph',
      label: 'Flamegraph',
      show: meta?.type === 'heapprofile' || meta?.type === 'heapsnapshot',
    },
    { key: 'callgraph', label: 'Call Graph', show: meta?.type === 'heapprofile' },
    {
      key: 'treemap',
      label: 'Treemap',
      show: meta?.type === 'heapprofile' || meta?.type === 'heapsnapshot',
    },
    { key: 'locations', label: 'Locations', show: meta?.type === 'heapprofile' },
    { key: 'nodes', label: 'Nodes', show: meta?.type === 'heapsnapshot' },
    { key: 'tree', label: 'Call Tree', show: meta?.type === 'heapprofile' },
    { key: 'timeline', label: 'Timeline', show: meta?.type === 'heaptimeline' },
    { key: 'retained', label: 'Retained', show: meta?.type === 'heapsnapshot' },
    {
      key: 'diff',
      label: 'Diff',
      show: meta?.type === 'heapprofile' || meta?.type === 'heapsnapshot',
    },
    { key: 'search', label: 'Search', show: true },
    { key: 'insights', label: 'Insights', show: meta?.type === 'heapsnapshot' },
  ]

  if (error) {
    return (
      <div className="min-h-screen bg-gray-950 text-red-400 flex items-center justify-center">
        <p>Error: {error}</p>
      </div>
    )
  }

  if (!meta) {
    return (
      <div className="min-h-screen bg-gray-950 text-gray-400 flex items-center justify-center">
        <p>Loading...</p>
      </div>
    )
  }

  return (
    <div className="min-h-screen bg-gray-950 text-gray-100">
      <header className="border-b border-gray-800 px-6 py-3">
        <div className="flex items-center justify-between">
          <div>
            <h1 className="text-lg font-bold tracking-tight">{meta.fileName}</h1>
            <p className="text-xs text-gray-500">
              {meta.type} | {formatBytes(meta.fileSize)}
              {typeof meta.node_count === 'number' &&
                ` | ${meta.node_count.toLocaleString()} nodes`}
              {typeof meta.edge_count === 'number' &&
                ` | ${meta.edge_count.toLocaleString()} edges`}
            </p>
          </div>
          <a href="/" className="text-sm text-gray-400 hover:text-white">
            ← Back
          </a>
        </div>
      </header>

      <nav className="border-b border-gray-800 px-6 flex gap-1 overflow-x-auto">
        {tabs
          .filter((t) => t.show)
          .map((t) => (
            <button
              key={t.key}
              onClick={() => setTab(t.key)}
              className={`px-4 py-2.5 text-sm border-b-2 transition-colors whitespace-nowrap ${
                tab === t.key
                  ? 'border-indigo-500 text-white'
                  : 'border-transparent text-gray-400 hover:text-gray-200'
              }`}
            >
              {t.label}
            </button>
          ))}
      </nav>

      <main className="p-6">
        {tab === 'summary' && <Summary base={base} type={meta.type} />}
        {tab === 'flamegraph' && <Flamegraph base={base} />}
        {tab === 'callgraph' && <CallGraph base={base} />}
        {tab === 'treemap' && <Treemap base={base} />}
        {tab === 'locations' && <SourceListing base={base} />}
        {tab === 'nodes' && <NodesTable base={base} />}
        {tab === 'tree' && <TreeView base={base} />}
        {tab === 'timeline' && <Timeline base={base} />}
        {tab === 'retained' && <RetainedSize base={base} />}
        {tab === 'diff' && <DiffView base={base} currentPath={filePath} currentType={meta.type} />}
        {tab === 'search' && <Search base={base} type={meta.type} />}
        {tab === 'insights' && <SnapshotInsights base={base} />}
      </main>
    </div>
  )
}
