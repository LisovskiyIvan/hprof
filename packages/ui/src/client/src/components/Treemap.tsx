import { useEffect, useState, useRef } from 'react'
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

interface TreemapNode {
  name: string
  size: number
  children: TreemapNode[]
}

interface TreemapRect {
  x: number
  y: number
  w: number
  h: number
  node: TreemapNode
  depth: number
}

const PALETTE = [
  '#6366f1',
  '#8b5cf6',
  '#ec4899',
  '#ef4444',
  '#f97316',
  '#eab308',
  '#84cc16',
  '#22c55e',
  '#14b8a6',
  '#06b6d4',
  '#3b82f6',
]

function colorFor(depth: number, idx: number): string {
  if (depth === 0) return '#1f2937'
  return PALETTE[(idx * 7 + depth * 3) % PALETTE.length]
}

/// Squarified treemap layout. Implements the Bruls-Huijsen-van Wijk squarify
/// algorithm to produce aspect-ratio-friendly rectangles.
function squarify(
  node: TreemapNode,
  x: number,
  y: number,
  w: number,
  h: number,
  depth: number,
  out: TreemapRect[],
) {
  out.push({ x, y, w, h, node, depth })

  if (node.children.length === 0 || w < 2 || h < 2) return

  // Children to layout: only the ones with size > 0.
  const items = node.children
    .filter((c) => c.size > 0)
    .map((c) => ({ node: c, area: c.size }))
    .sort((a, b) => b.area - a.area)

  if (items.length === 0) return

  const totalArea = w * h
  const totalSize = items.reduce((s, i) => s + i.area, 0) || 1

  // Convert to scaled areas.
  const areas = items.map((i) => ({ ...i, area: (i.area / totalSize) * totalArea }))

  // Squarify algorithm.
  layoutRow(areas, x, y, w, h, depth + 1, out)
}

function layoutRow(
  items: { node: TreemapNode; area: number }[],
  x: number,
  y: number,
  w: number,
  h: number,
  depth: number,
  out: TreemapRect[],
) {
  if (items.length === 0) return
  const isHorizontal = w >= h
  const total = items.reduce((s, i) => s + i.area, 0)

  if (isHorizontal) {
    // Lay out along the left edge (vertical column of width colW).
    const colW = total / h
    let cy = y
    for (const it of items) {
      const itemH = it.area / colW
      out.push({ x, y: cy, w: colW, h: itemH, node: it.node, depth })
      // Recurse into the item.
      squarify(it.node, x, cy, colW, itemH, depth, out)
      cy += itemH
    }
    // Remaining area is to the right.
    const restW = w - colW
    if (restW > 0.5 && items.length > 0) {
      // No more items — done.
    }
  } else {
    const colH = total / w
    let cx = x
    for (const it of items) {
      const itemW = it.area / colH
      out.push({ x: cx, y, w: itemW, h: colH, node: it.node, depth })
      squarify(it.node, cx, y, itemW, colH, depth, out)
      cx += itemW
    }
  }
}

export default function Treemap({ base }: { base: string }) {
  const [data, setData] = useState<TreemapNode | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [focus, setFocus] = useState('')
  const [ignore, setIgnore] = useState('')
  const [hide, setHide] = useState('')
  const [hover, setHover] = useState<{ node: TreemapNode; left: number; top: number } | null>(null)
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const containerRef = useRef<HTMLDivElement>(null)

  const fetchData = () => {
    setError(null)
    const params = new URLSearchParams()
    if (focus) params.set('focus', focus)
    if (ignore) params.set('ignore', ignore)
    if (hide) params.set('hide', hide)
    const qs = params.toString()
    fetchJson<TreemapNode>(`${base}/treemap${qs ? `?${qs}` : ''}`, { cache: false })
      .then(setData)
      .catch((e) => setError(e.message))
  }

  useEffect(() => {
    fetchData()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [base])

  useEffect(() => {
    const canvas = canvasRef.current
    const container = containerRef.current
    if (!canvas || !container || !data) return

    const w = container.clientWidth
    const h = container.clientHeight
    const dpr = window.devicePixelRatio || 1
    canvas.width = w * dpr
    canvas.height = h * dpr
    canvas.style.width = `${w}px`
    canvas.style.height = `${h}px`

    const ctx = canvas.getContext('2d')
    if (!ctx) return
    ctx.scale(dpr, dpr)
    ctx.font = '11px ui-monospace, monospace'
    ctx.textBaseline = 'top'

    ctx.fillStyle = '#030712'
    ctx.fillRect(0, 0, w, h)

    const rects: TreemapRect[] = []
    squarify(data, 0, 0, w, h, 0, rects)

    let colorIdx = 0
    for (const r of rects) {
      if (r.w < 1 || r.h < 1) continue
      ctx.fillStyle = r.depth === 0 ? '#1f2937' : colorFor(r.depth, colorIdx++ % PALETTE.length)
      ctx.fillRect(r.x + 1, r.y + 1, Math.max(0, r.w - 2), Math.max(0, r.h - 2))

      // Border.
      ctx.strokeStyle = '#030712'
      ctx.lineWidth = 1
      ctx.strokeRect(r.x + 1, r.y + 1, Math.max(0, r.w - 2), Math.max(0, r.h - 2))

      if (r.w > 50 && r.h > 20) {
        ctx.fillStyle = 'rgba(255,255,255,0.95)'
        const label = shortLabel(r.node.name, r.w - 8, ctx)
        ctx.fillText(label, r.x + 4, r.y + 4)
        if (r.h > 40) {
          ctx.fillStyle = 'rgba(255,255,255,0.7)'
          ctx.fillText(formatBytes(r.node.size), r.x + 4, r.y + 18)
        }
      }
    }
  }, [data])

  if (error) return <p className="text-red-400 text-sm">Failed to load treemap: {error}</p>
  if (!data) return <p className="text-gray-500">Loading treemap...</p>

  return (
    <div className="space-y-3">
      <div className="flex gap-3 items-center flex-wrap text-sm">
        <input
          type="text"
          placeholder="focus: regex"
          value={focus}
          onChange={(e) => setFocus(e.target.value)}
          className="bg-gray-900 border border-gray-700 rounded px-3 py-1.5 text-xs w-48 focus:border-indigo-500 outline-none"
        />
        <input
          type="text"
          placeholder="ignore: drop flat attribution"
          value={ignore}
          onChange={(e) => setIgnore(e.target.value)}
          className="bg-gray-900 border border-gray-700 rounded px-3 py-1.5 text-xs w-48 focus:border-indigo-500 outline-none"
        />
        <input
          type="text"
          placeholder="hide: drop frames from view"
          value={hide}
          onChange={(e) => setHide(e.target.value)}
          className="bg-gray-900 border border-gray-700 rounded px-3 py-1.5 text-xs w-48 focus:border-indigo-500 outline-none"
        />
        <button
          onClick={fetchData}
          className="px-3 py-1.5 bg-indigo-600 text-white text-xs rounded hover:bg-indigo-700"
        >
          Apply
        </button>
        <span className="ml-auto text-xs text-gray-500">Total: {formatBytes(data.size)}</span>
      </div>

      <div className="bg-gray-900 rounded-lg p-2">
        <div ref={containerRef} className="relative" style={{ height: 600 }}>
          <canvas
            ref={canvasRef}
            onMouseMove={(e) => {
              const canvas = canvasRef.current
              if (!canvas) return
              const rect = canvas.getBoundingClientRect()
              const x = e.clientX - rect.left
              const y = e.clientY - rect.top
              const container = containerRef.current
              if (!container || !data) return
              const rects: TreemapRect[] = []
              squarify(data, 0, 0, container.clientWidth, container.clientHeight, 0, rects)
              // Find the smallest rect that contains the cursor.
              const hit = rects
                .filter((r) => x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
                .sort((a, b) => b.depth - a.depth)[0]
              if (hit) {
                setHover({ node: hit.node, left: e.clientX, top: e.clientY })
              } else {
                setHover(null)
              }
            }}
            onMouseLeave={() => setHover(null)}
            className="block"
          />
        </div>
      </div>

      {hover && (
        <div
          className="fixed z-50 pointer-events-none bg-gray-950 border border-gray-700 rounded shadow-lg p-3 text-xs"
          style={{ left: hover.left + 12, top: hover.top + 12, maxWidth: 400 }}
        >
          <div className="font-mono text-indigo-400 truncate" title={hover.node.name}>
            {hover.node.name}
          </div>
          <div className="text-gray-300 mt-1">
            Size: <span className="text-white">{formatBytes(hover.node.size)}</span>
          </div>
        </div>
      )}
    </div>
  )
}

function shortLabel(name: string, maxWidth: number, ctx: CanvasRenderingContext2D): string {
  if (ctx.measureText(name).width <= maxWidth) return name
  let truncated = name
  while (truncated.length > 3 && ctx.measureText(`${truncated}…`).width > maxWidth) {
    truncated = truncated.slice(0, -1)
  }
  return truncated.length > 3 ? `${truncated}…` : ''
}
