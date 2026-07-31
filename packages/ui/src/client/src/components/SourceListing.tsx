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

interface FlatFrame {
  functionName: string
  url: string
  lineNumber: number
  columnNumber: number
  selfSize: number
  stack: string[]
}

interface GroupedLine {
  url: string
  lineNumber: number
  functionNames: Set<string>
  selfSize: number
  count: number
  stacks: string[][]
}

interface UrlGroup {
  url: string
  totalSize: number
  lines: GroupedLine[]
}

/// Source/Location listing for heapprofile. Mimics pprof's `list` command in
/// spirit: samples are aggregated by (url, line number) so you can see which
/// lines of a file are responsible for allocations. Actual source code is not
/// shown unless the URL is a local file:// path that exists on disk — this
/// avoids needing source-map support for browser-served bundles.
export default function SourceListing({ base }: { base: string }) {
  const [data, setData] = useState<UrlGroup[] | null>(null)
  const [filter, setFilter] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [expanded, setExpanded] = useState<Set<string>>(new Set())

  useEffect(() => {
    setError(null)
    fetchJson<FlatFrame[]>(`${base}/flat`, { cache: false })
      .then((d) => {
        // Group by url then by lineNumber.
        const byUrl = new Map<string, UrlGroup>()
        for (const frame of d) {
          if (filter) {
            const re = new RegExp(filter, 'i')
            if (!re.test(frame.url) && !re.test(frame.functionName)) continue
          }
          let g = byUrl.get(frame.url)
          if (!g) {
            g = { url: frame.url, totalSize: 0, lines: [] }
            byUrl.set(frame.url, g)
          }
          g.totalSize += frame.selfSize
          // Find existing line entry.
          let line = g.lines.find((l) => l.lineNumber === frame.lineNumber)
          if (!line) {
            line = {
              url: frame.url,
              lineNumber: frame.lineNumber,
              functionNames: new Set(),
              selfSize: 0,
              count: 0,
              stacks: [],
            }
            g.lines.push(line)
          }
          line.functionNames.add(frame.functionName)
          line.selfSize += frame.selfSize
          line.count += 1
          if (line.stacks.length < 5) line.stacks.push(frame.stack)
        }

        const groups = [...byUrl.values()].sort((a, b) => b.totalSize - a.totalSize)
        for (const g of groups) {
          g.lines.sort((a, b) => b.selfSize - a.selfSize)
        }
        setData(groups)
      })
      .catch((e) => setError(e.message))
  }, [base, filter])

  if (error) return <p className="text-red-400 text-sm">Failed to load: {error}</p>
  if (!data) return <p className="text-gray-500">Loading...</p>

  const toggle = (url: string) => {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(url)) next.delete(url)
      else next.add(url)
      return next
    })
  }

  return (
    <div className="space-y-4">
      <div className="flex gap-3 items-center text-sm">
        <input
          type="text"
          placeholder="filter by url or function..."
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          className="bg-gray-900 border border-gray-700 rounded px-3 py-1.5 text-xs w-64 focus:border-indigo-500 outline-none"
        />
        <span className="text-xs text-gray-500">
          Locations aggregated by file:line. Click a URL to expand its lines.
        </span>
      </div>

      {data.length === 0 ? (
        <p className="text-gray-500 text-sm">No locations match the filter.</p>
      ) : (
        <div className="space-y-2">
          {data.slice(0, 100).map((g) => {
            const isOpen = expanded.has(g.url)
            return (
              <div key={g.url} className="bg-gray-900 rounded-lg overflow-hidden">
                <button
                  onClick={() => toggle(g.url)}
                  className="w-full text-left flex items-center px-4 py-2 text-sm hover:bg-gray-800/50"
                >
                  <span className="text-gray-600 text-xs w-3 mr-2">{isOpen ? '▼' : '▶'}</span>
                  <span className="text-indigo-400 font-mono text-xs truncate flex-1" title={g.url}>
                    {g.url}
                  </span>
                  <span className="text-gray-300 text-xs ml-4 whitespace-nowrap">
                    {formatBytes(g.totalSize)}
                  </span>
                </button>
                {isOpen && (
                  <div>
                    {g.lines.map((l, i) => (
                      <div
                        key={i}
                        className="flex items-center px-4 py-1.5 border-t border-gray-800/50 text-xs hover:bg-gray-800/30"
                        style={{ paddingLeft: '40px' }}
                      >
                        <span className="text-gray-600 font-mono w-16">:{l.lineNumber + 1}</span>
                        <span
                          className="text-gray-400 font-mono flex-1 truncate"
                          title={[...l.functionNames].join(', ')}
                        >
                          {[...l.functionNames].join(', ')}
                        </span>
                        <span className="text-gray-300 ml-4 whitespace-nowrap font-mono">
                          {formatBytes(l.selfSize)}
                        </span>
                        <span className="text-gray-600 ml-3 whitespace-nowrap">×{l.count}</span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )
          })}
        </div>
      )}
    </div>
  )
}
