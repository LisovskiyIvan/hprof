import { useEffect, useState, useRef, useCallback } from 'react'
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

interface FlamegraphFrame {
  name: string
  selfSize: number
  totalSize: number
  children: FlamegraphFrame[]
}

interface Rect {
  x: number
  y: number
  w: number
  h: number
  frame: FlamegraphFrame
  depth: number
}

interface Tooltip {
  frame: FlamegraphFrame
  pct: number
  left: number
  top: number
}

const ROW_HEIGHT = 22
const MIN_WIDTH = 1

// Hot -> cold color palette based on percentage of total.
function colorFor(pct: number): string {
  if (pct > 50) return '#ef4444'
  if (pct > 25) return '#f97316'
  if (pct > 10) return '#eab308'
  if (pct > 5) return '#84cc16'
  if (pct > 1) return '#22c55e'
  return '#3b82f6'
}

/// Flatten the flamegraph tree into a list of rectangles suitable for canvas
/// drawing. Each frame occupies one row at depth `depth`; its width is
/// proportional to its `totalSize` within its parent's span.
function flatten(
  frame: FlamegraphFrame,
  x: number,
  y: number,
  width: number,
  depth: number,
  out: Rect[],
) {
  if (width < MIN_WIDTH) return
  out.push({ x, y, w: width, h: ROW_HEIGHT - 1, frame, depth })

  let cursor = x
  for (const child of frame.children) {
    // Proportional width inside parent's span.
    const proportionalWidth = frame.totalSize > 0 ? (child.totalSize / frame.totalSize) * width : 0
    flatten(child, cursor, y + ROW_HEIGHT, proportionalWidth, depth + 1, out)
    cursor += proportionalWidth
  }
}

export default function Flamegraph({ base }: { base: string }) {
  const [data, setData] = useState<FlamegraphFrame | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [focus, setFocus] = useState('')
  const [ignore, setIgnore] = useState('')
  const [hide, setHide] = useState('')
  const [zoomFrame, setZoomFrame] = useState<FlamegraphFrame | null>(null)
  const [hover, setHover] = useState<Tooltip | null>(null)
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const containerRef = useRef<HTMLDivElement>(null)

  const fetchData = useCallback(() => {
    setError(null)
    const params = new URLSearchParams()
    if (focus) params.set('focus', focus)
    if (ignore) params.set('ignore', ignore)
    if (hide) params.set('hide', hide)
    const qs = params.toString()
    fetchJson<FlamegraphFrame>(`${base}/flamegraph${qs ? `?${qs}` : ''}`, { cache: false })
      .then((d) => {
        setData(d)
        setZoomFrame(null)
      })
      .catch((e) => setError(e.message))
  }, [base, focus, ignore, hide])

  useEffect(() => {
    fetchData()
  }, [fetchData])

  // Draw canvas.
  useEffect(() => {
    const canvas = canvasRef.current
    const container = containerRef.current
    if (!canvas || !container || !data) return

    const root = zoomFrame ?? data
    const rects: Rect[] = []
    flatten(root, 0, 0, container.clientWidth, 0, rects)

    const containerWidth = container.clientWidth
    const containerHeight = container.clientHeight

    const dpr = window.devicePixelRatio || 1
    canvas.width = containerWidth * dpr
    canvas.height = containerHeight * dpr
    canvas.style.width = `${containerWidth}px`
    canvas.style.height = `${containerHeight}px`

    const ctx = canvas.getContext('2d')
    if (!ctx) return
    ctx.scale(dpr, dpr)
    ctx.font = '11px ui-monospace, monospace'
    ctx.textBaseline = 'middle'

    // Clear.
    ctx.fillStyle = '#030712'
    ctx.fillRect(0, 0, containerWidth, containerHeight)

    for (const r of rects) {
      if (r.w < MIN_WIDTH) continue
      const pctVal = (r.frame.totalSize / (root.totalSize || 1)) * 100
      ctx.fillStyle = colorFor(pctVal)
      ctx.fillRect(r.x, r.y, r.w - 1, r.h)

      // Label.
      if (r.w > 30) {
        ctx.fillStyle = 'rgba(0,0,0,0.85)'
        const label = shortLabel(r.frame.name, r.w - 8, ctx)
        ctx.fillText(label, r.x + 4, r.y + r.h / 2)
      }
    }
  }, [data, zoomFrame])

  const handleMove = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const canvas = canvasRef.current
    const container = containerRef.current
    if (!canvas || !container || !data) return
    const rect = canvas.getBoundingClientRect()
    const x = e.clientX - rect.left
    const y = e.clientY - rect.top

    const root = zoomFrame ?? data
    const total = root.totalSize || 1
    const rects: Rect[] = []
    flatten(root, 0, 0, container.clientWidth, 0, rects)

    const hit = rects.find((r) => x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
    if (hit) {
      const pct = (hit.frame.totalSize / total) * 100
      setHover({
        frame: hit.frame,
        pct,
        left: e.clientX,
        top: e.clientY,
      })
      canvas.style.cursor = 'pointer'
    } else {
      setHover(null)
      canvas.style.cursor = 'default'
    }
  }

  const handleClick = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const canvas = canvasRef.current
    const container = containerRef.current
    if (!canvas || !container || !data) return
    const rect = canvas.getBoundingClientRect()
    const x = e.clientX - rect.left
    const y = e.clientY - rect.top

    const root = zoomFrame ?? data
    const rects: Rect[] = []
    flatten(root, 0, 0, container.clientWidth, 0, rects)

    const hit = rects.find((r) => x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
    if (hit) {
      // Left click: zoom into this frame.
      setZoomFrame(hit.frame)
      setHover(null)
    }
  }

  if (error) return <p className="text-red-400 text-sm">Failed to load flamegraph: {error}</p>
  if (!data) return <p className="text-gray-500">Loading flamegraph...</p>

  return (
    <div className="space-y-3">
      <div className="flex gap-3 items-center flex-wrap text-sm">
        <input
          type="text"
          placeholder="focus: regex (e.g. render)"
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
        <div className="ml-auto flex items-center gap-3">
          {zoomFrame && (
            <button
              onClick={() => setZoomFrame(null)}
              className="px-3 py-1.5 bg-gray-800 text-xs rounded hover:bg-gray-700"
            >
              ↶ Reset zoom
            </button>
          )}
          <span className="text-xs text-gray-500">
            Total: {formatBytes((zoomFrame ?? data).totalSize)}
          </span>
        </div>
      </div>

      <div className="bg-gray-900 rounded-lg p-2">
        <div className="text-xs text-gray-500 px-2 py-1">
          Click a frame to zoom in. Hover for details.
        </div>
        <div ref={containerRef} className="relative" style={{ height: 600 }}>
          <canvas
            ref={canvasRef}
            onMouseMove={handleMove}
            onMouseLeave={() => setHover(null)}
            onClick={handleClick}
            className="block"
          />
        </div>
      </div>

      {hover && (
        <div
          className="fixed z-50 pointer-events-none bg-gray-950 border border-gray-700 rounded shadow-lg p-3 text-xs"
          style={{
            left: hover.left + 12,
            top: hover.top + 12,
            maxWidth: 400,
          }}
        >
          <div className="font-mono text-indigo-400 truncate" title={hover.frame.name}>
            {hover.frame.name}
          </div>
          <div className="text-gray-300 mt-1">
            Total: <span className="text-white">{formatBytes(hover.frame.totalSize)}</span>
            <span className="text-gray-500 ml-2">({hover.pct.toFixed(2)}%)</span>
          </div>
          <div className="text-gray-300">
            Self: <span className="text-white">{formatBytes(hover.frame.selfSize)}</span>
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
