import { useEffect, useMemo, useRef, useState } from 'react'
import uPlot from 'uplot'
import { fetchJson } from '../lib/api'

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit++
  }
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`
}

interface Growth {
  spanUs: number
  objectsStart: number
  objectsEnd: number
  samples: [number, number][]
}

interface NameType {
  name: string
  size: number
  count: number
}

interface NameEntry {
  name: string
  size: number
  count: number
  types: NameType[]
}

interface NamesResult {
  totalSize: number
  totalCount: number
  entries: NameEntry[]
}

interface StackFrame {
  name: string
  script: string
  line: number
  column: number
}

interface StackEntry {
  size: number
  count: number
  stack: StackFrame[]
}

interface StacksResult {
  totalSize: number
  totalCount: number
  entries: StackEntry[]
}

interface NameStacksResult {
  name: string
  totalSize: number
  totalCount: number
  entries: StackEntry[]
}

function formatStack(stack: StackFrame[]): string {
  return stack
    .map((f) => f.name)
    .filter((n) => n !== '(root)' && n !== '')
    .join(' <- ')
}

function RateChart({ growth }: { growth: Growth }) {
  const chartRef = useRef<HTMLDivElement>(null)
  const plotRef = useRef<uPlot | null>(null)

  useEffect(() => {
    if (!chartRef.current) return
    const samples = growth.samples
    if (samples.length < 2) return

    // cumulative objects over time
    const t = samples.map((s) => s[0] / 1e6)
    const objects = samples.map((s) => s[1])

    if (plotRef.current) {
      plotRef.current.destroy()
    }
    plotRef.current = new uPlot(
      {
        width: chartRef.current.clientWidth,
        height: 260,
        cursor: { drag: { x: true, y: true } },
        scales: { x: { time: false } },
        axes: [
          {
            stroke: '#6b7280',
            grid: { stroke: '#374151' },
            ticks: { stroke: '#374151' },
            values: (_, ticks) => ticks.map((v) => `${v.toFixed(0)}s`),
          },
          {
            stroke: '#6b7280',
            grid: { stroke: '#374151' },
            ticks: { stroke: '#374151' },
            values: (_, ticks) => ticks.map((v) => `${(v / 1e6).toFixed(0)}M`),
          },
        ],
        series: [
          {},
          {
            label: 'Objects allocated (cumulative)',
            stroke: '#22d3ee',
            width: 2,
          },
        ],
        legend: { live: true },
      },
      [t, objects],
      chartRef.current,
    )

    const onResize = () => {
      if (plotRef.current && chartRef.current) {
        plotRef.current.setSize({ width: chartRef.current.clientWidth, height: 260 })
      }
    }
    window.addEventListener('resize', onResize)
    return () => {
      window.removeEventListener('resize', onResize)
      if (plotRef.current) {
        plotRef.current.destroy()
        plotRef.current = null
      }
    }
  }, [growth])

  return <div ref={chartRef} className="h-[260px]" />
}

export default function Timeline({ base }: { base: string }) {
  const [growth, setGrowth] = useState<Growth | null>(null)
  const [names, setNames] = useState<NamesResult | null>(null)
  const [stacks, setStacks] = useState<StacksResult | null>(null)
  const [nameStacks, setNameStacks] = useState<NameStacksResult | null>(null)
  const [filter, setFilter] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    setError(null)
    Promise.all([
      fetchJson<Growth>(`${base}/growth`),
      fetchJson<NamesResult>(`${base}/names?top=100`),
      fetchJson<StacksResult>(`${base}/stacks?top=40`),
    ])
      .then(([g, n, s]) => {
        setGrowth(g)
        setNames(n)
        setStacks(s)
      })
      .catch((e) => {
        setError(e.message)
      })
  }, [base])

  const filteredNames = useMemo(() => {
    if (!names) return []
    if (!filter) return names.entries
    const re = new RegExp(filter, 'i')
    return names.entries.filter((e) => re.test(e.name))
  }, [names, filter])

  const onNameClick = (name: string) => {
    setBusy(true)
    fetchJson<NameStacksResult>(`${base}/stacks?top=10&name=${encodeURIComponent(name)}`)
      .then((s) => setNameStacks(s))
      .catch((e) => setError(e.message))
      .finally(() => setBusy(false))
  }

  if (error) {
    return <p className="text-red-400 text-sm">Failed to load timeline: {error}</p>
  }

  if (!growth || !names) {
    return <p className="text-gray-500">Loading timeline...</p>
  }

  const spanS = (growth.spanUs / 1e6).toFixed(1)
  const totalObjects = growth.objectsEnd - growth.objectsStart

  return (
    <div className="space-y-6">
      {/* growth */}
      <section>
        <div className="mb-2 flex items-baseline justify-between">
          <h3 className="text-sm font-semibold text-gray-300">Object growth</h3>
          <span className="text-xs text-gray-500">
            {spanS}s · +{totalObjects.toLocaleString()} objects
          </span>
        </div>
        <RateChart growth={growth} />
      </section>

      {/* names */}
      <section>
        <div className="mb-2 flex items-center justify-between gap-4">
          <h3 className="text-sm font-semibold text-gray-300">
            Top allocations by name{' '}
            <span className="font-normal text-gray-500">
              ({formatBytes(names.totalSize)} total)
            </span>
          </h3>
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="filter names…"
            className="rounded border border-gray-700 bg-gray-800 px-2 py-1 text-xs text-gray-200 outline-none focus:border-cyan-600"
          />
        </div>
        <div className="overflow-auto rounded border border-gray-800">
          <table className="w-full text-left text-xs">
            <thead className="bg-gray-800/60 text-gray-400">
              <tr>
                <th className="px-2 py-1.5">Size</th>
                <th className="px-2 py-1.5">%</th>
                <th className="px-2 py-1.5">Count</th>
                <th className="px-2 py-1.5">Name</th>
                <th className="px-2 py-1.5">By type</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-800/60">
              {filteredNames.map((e) => (
                <tr
                  key={e.name}
                  onClick={() => onNameClick(e.name)}
                  className="cursor-pointer hover:bg-gray-800/50"
                >
                  <td className="px-2 py-1 font-mono text-cyan-400">{formatBytes(e.size)}</td>
                  <td className="px-2 py-1 text-gray-500">
                    {((e.size / names.totalSize) * 100).toFixed(1)}%
                  </td>
                  <td className="px-2 py-1 text-gray-500">{e.count.toLocaleString()}</td>
                  <td className="max-w-[30rem] truncate px-2 py-1 text-gray-200" title={e.name}>
                    {e.name}
                  </td>
                  <td className="px-2 py-1 text-gray-500">
                    {e.types
                      .slice(0, 3)
                      .map((t) => `${t.name} ${((t.size / e.size) * 100).toFixed(0)}%`)
                      .join(' · ')}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>

      {/* name stacks */}
      {nameStacks && (
        <section>
          <h3 className="mb-2 text-sm font-semibold text-gray-300">
            Where <span className="text-cyan-400">{nameStacks.name}</span> is allocated
            <span className="ml-2 font-normal text-gray-500">
              ({formatBytes(nameStacks.totalSize)}, {nameStacks.totalCount.toLocaleString()} allocs)
            </span>
            <button
              onClick={() => setNameStacks(null)}
              className="ml-3 rounded border border-gray-700 px-1.5 text-gray-400 hover:text-gray-200"
            >
              ✕
            </button>
          </h3>
          <div className="rounded border border-gray-800">
            <table className="w-full text-left text-xs">
              <thead className="bg-gray-800/60 text-gray-400">
                <tr>
                  <th className="px-2 py-1.5">Size</th>
                  <th className="px-2 py-1.5">Count</th>
                  <th className="px-2 py-1.5">Stack</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-800/60">
                {nameStacks.entries.map((e, i) => (
                  <tr key={i}>
                    <td className="px-2 py-1 font-mono text-cyan-400">{formatBytes(e.size)}</td>
                    <td className="px-2 py-1 text-gray-500">{e.count.toLocaleString()}</td>
                    <td className="px-2 py-1 font-mono text-gray-300">{formatStack(e.stack)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}

      {/* stacks */}
      <section>
        <div className="mb-2 flex items-baseline justify-between">
          <h3 className="text-sm font-semibold text-gray-300">
            Top allocation sites (stack traces){' '}
            <span className="font-normal text-gray-500">
              ({formatBytes(stacks?.totalSize ?? 0)} tracked)
            </span>
          </h3>
          <span className="text-xs text-gray-600">{busy ? 'loading…' : 'leaf <- caller'}</span>
        </div>
        <div className="rounded border border-gray-800">
          <table className="w-full text-left text-xs">
            <thead className="bg-gray-800/60 text-gray-400">
              <tr>
                <th className="px-2 py-1.5">Size</th>
                <th className="px-2 py-1.5">Count</th>
                <th className="px-2 py-1.5">Stack</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-800/60">
              {(stacks?.entries ?? []).map((e, i) => (
                <tr key={i}>
                  <td className="px-2 py-1 font-mono text-cyan-400">{formatBytes(e.size)}</td>
                  <td className="px-2 py-1 text-gray-500">{e.count.toLocaleString()}</td>
                  <td
                    className="max-w-[60rem] truncate px-2 py-1 font-mono text-gray-300"
                    title={formatStack(e.stack)}
                  >
                    {formatStack(e.stack)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>
    </div>
  )
}
