import { useEffect, useState, useCallback, useRef } from 'react'
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

interface NodeEntry {
  type: string
  name: string
  selfSize: number
  id: number
  edgeCount: number
  retentionSize?: number
}

interface NodesResponse {
  total: number
  page: number
  pageSize: number
  nodes: NodeEntry[]
}

const ROW_HEIGHT = 33
const OVERSCAN = 10

/// Virtualized nodes table. Instead of paginating through "Next/Prev",
/// we fetch larger chunks (e.g. 500 rows) and render only the visible
/// window using transform offsets. Scrolling loads more pages.
export default function NodesTable({ base }: { base: string }) {
  const [allNodes, setAllNodes] = useState<NodeEntry[]>([])
  const [total, setTotal] = useState(0)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [page, setPage] = useState(0)
  const pageSize = 500
  const [sort, setSort] = useState('selfSize')
  const [dir, setDir] = useState<'asc' | 'desc'>('desc')
  const [filterType, setFilterType] = useState('')
  const [search, setSearch] = useState('')
  const [viewportHeight, setViewportHeight] = useState(600)
  const [scrollTop, setScrollTop] = useState(0)
  const containerRef = useRef<HTMLDivElement>(null)

  // Fetch a single page; append to allNodes if continuing same query.
  const fetchPage = useCallback(
    (p: number, replace: boolean) => {
      setLoading(true)
      setError(null)
      const params = new URLSearchParams({
        page: String(p),
        pageSize: String(pageSize),
        sort,
        dir,
      })
      if (filterType) params.set('type', filterType)
      if (search) params.set('q', search)

      fetchJson<NodesResponse>(`${base}/nodes?${params}`, { cache: false })
        .then((data) => {
          setTotal(data.total)
          setAllNodes((prev) => (replace ? data.nodes : [...prev, ...data.nodes]))
          setLoading(false)
        })
        .catch((e) => {
          setError(e.message)
          setLoading(false)
        })
    },
    [base, sort, dir, filterType, search],
  )

  // Reset+fetch when filters/sort change.
  useEffect(() => {
    setPage(0)
    setAllNodes([])
    setScrollTop(0)
    fetchPage(0, true)
  }, [fetchPage])

  // Observe viewport height.
  useEffect(() => {
    const container = containerRef.current
    if (!container) return
    const update = () => setViewportHeight(container.clientHeight)
    update()
    const ro = new ResizeObserver(update)
    ro.observe(container)
    return () => ro.disconnect()
  }, [])

  const totalRows = total
  const startIndex = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN)
  const endIndex = Math.min(
    totalRows,
    Math.ceil((scrollTop + viewportHeight) / ROW_HEIGHT) + OVERSCAN,
  )

  // Load more pages if user scrolled past what we have.
  useEffect(() => {
    const loadedCount = allNodes.length
    const neededIndex = endIndex
    if (neededIndex > loadedCount && !loading && loadedCount < total) {
      const nextPage = Math.floor(loadedCount / pageSize)
      fetchPage(nextPage, false)
      setPage(nextPage)
    }
  }, [endIndex, allNodes.length, loading, total, fetchPage])

  const onScroll = (e: React.UIEvent<HTMLDivElement>) => {
    setScrollTop(e.currentTarget.scrollTop)
  }

  const toggleSort = (col: string) => {
    if (sort === col) {
      setDir((d) => (d === 'desc' ? 'asc' : 'desc'))
    } else {
      setSort(col)
      setDir('desc')
    }
  }

  const SortIcon = ({ col }: { col: string }) => {
    if (sort !== col) return <span className="text-gray-600 ml-1">↕</span>
    return <span className="text-indigo-400 ml-1">{dir === 'desc' ? '↓' : '↑'}</span>
  }

  if (error) {
    return <p className="text-red-400 text-sm">Failed to load nodes: {error}</p>
  }

  const visibleNodes = allNodes.slice(
    Math.max(0, startIndex - page * pageSize),
    endIndex - page * pageSize,
  )

  // Compute absolute indices for visible rows.
  const absoluteIndex = (i: number) => page * pageSize + i

  return (
    <div className="space-y-4">
      <div className="flex gap-3 items-center">
        <input
          type="text"
          placeholder="Search by name..."
          value={search}
          onChange={(e) => {
            setSearch(e.target.value)
          }}
          className="bg-gray-900 border border-gray-700 rounded px-3 py-1.5 text-sm w-64 focus:border-indigo-500 outline-none"
        />
        <input
          type="text"
          placeholder="Filter by type..."
          value={filterType}
          onChange={(e) => {
            setFilterType(e.target.value)
          }}
          className="bg-gray-900 border border-gray-700 rounded px-3 py-1.5 text-sm w-48 focus:border-indigo-500 outline-none"
        />
        <span className="text-xs text-gray-500">
          {total.toLocaleString()} nodes · virtualized scroll
        </span>
      </div>

      <div className="bg-gray-900 rounded-lg overflow-hidden">
        <table className="w-full text-sm table-fixed">
          <colgroup>
            <col className="w-20" />
            <col className="w-32" />
            <col />
            <col className="w-28" />
            <col className="w-20" />
          </colgroup>
          <thead className="sticky top-0 z-10">
            <tr className="border-b border-gray-800 text-gray-400 bg-gray-900">
              <th
                className="text-left px-4 py-2 cursor-pointer select-none"
                onClick={() => toggleSort('id')}
              >
                # <SortIcon col="id" />
              </th>
              <th
                className="text-left px-4 py-2 cursor-pointer select-none"
                onClick={() => toggleSort('type')}
              >
                Type <SortIcon col="type" />
              </th>
              <th
                className="text-left px-4 py-2 cursor-pointer select-none"
                onClick={() => toggleSort('name')}
              >
                Name <SortIcon col="name" />
              </th>
              <th
                className="text-right px-4 py-2 cursor-pointer select-none"
                onClick={() => toggleSort('selfSize')}
              >
                Self Size <SortIcon col="selfSize" />
              </th>
              <th
                className="text-right px-4 py-2 cursor-pointer select-none"
                onClick={() => toggleSort('edgeCount')}
              >
                Edges <SortIcon col="edgeCount" />
              </th>
            </tr>
          </thead>
        </table>
        <div
          ref={containerRef}
          onScroll={onScroll}
          style={{ height: viewportHeight, overflowY: 'auto', position: 'relative' }}
        >
          <div style={{ height: totalRows * ROW_HEIGHT, position: 'relative' }}>
            <table className="w-full text-sm table-fixed">
              <colgroup>
                <col className="w-20" />
                <col className="w-32" />
                <col />
                <col className="w-28" />
                <col className="w-20" />
              </colgroup>
              <tbody>
                {visibleNodes.map((node, i) => {
                  const absIdx = absoluteIndex(i)
                  const offsetTop = (startIndex + i) * ROW_HEIGHT
                  return (
                    <tr
                      key={`${node.id}-${absIdx}`}
                      className="border-b border-gray-800/50 hover:bg-gray-800/30 absolute"
                      style={{
                        height: ROW_HEIGHT,
                        top: offsetTop,
                        width: '100%',
                        display: 'table-row',
                      }}
                    >
                      <td className="px-4 py-1.5 text-gray-500 font-mono text-xs">{absIdx}</td>
                      <td className="px-4 py-1.5">
                        <span className="px-1.5 py-0.5 rounded text-xs bg-gray-800 text-gray-300">
                          {node.type}
                        </span>
                      </td>
                      <td
                        className="px-4 py-1.5 font-mono text-xs text-indigo-400 truncate max-w-md"
                        title={node.name}
                      >
                        {node.name}
                      </td>
                      <td className="px-4 py-1.5 text-right font-mono text-xs">
                        {formatBytes(node.selfSize)}
                      </td>
                      <td className="px-4 py-1.5 text-right font-mono text-xs text-gray-400">
                        {node.edgeCount}
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
          {loading && (
            <div className="absolute inset-0 flex items-center justify-center pointer-events-none">
              <span className="bg-gray-800 text-xs px-3 py-1.5 rounded">Loading more...</span>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
