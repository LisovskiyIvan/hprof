import { useEffect, useState, useRef } from 'react'

interface CallGraphProps {
  base: string
}

/// Renders the call graph. Uses the server-side `?format=svg` endpoint which
/// shells out to `dot` if Graphviz is installed; otherwise shows the raw DOT
/// in a code block with a download link and usage instructions.
export default function CallGraph({ base }: CallGraphProps) {
  const [focus, setFocus] = useState('')
  const [ignore, setIgnore] = useState('')
  const [hide, setHide] = useState('')
  const [top, setTop] = useState(20)
  const [svg, setSvg] = useState<string | null>(null)
  const [svgAvailable, setSvgAvailable] = useState<boolean | null>(null)
  const [dot, setDot] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const containerRef = useRef<HTMLDivElement>(null)

  const buildParams = () => {
    const params = new URLSearchParams()
    params.set('top', String(top))
    if (focus) params.set('focus', focus)
    if (ignore) params.set('ignore', ignore)
    if (hide) params.set('hide', hide)
    return params.toString()
  }

  const fetchSvg = async () => {
    setError(null)
    try {
      const res = await fetch(`${base}/graph?format=svg&${buildParams()}`)
      if (!res.ok) {
        const j = await res.json().catch(() => ({ error: `HTTP ${res.status}` }))
        setError(j.error)
        return
      }
      const text = await res.text()
      if (text.includes('<svg') && !text.includes('not available')) {
        setSvg(text)
        setSvgAvailable(true)
      } else {
        setSvg(text)
        setSvgAvailable(false)
      }
    } catch (e) {
      setError((e as Error).message)
    }
  }

  const fetchDot = async () => {
    try {
      const res = await fetch(`${base}/graph?format=dot&${buildParams()}`)
      const text = await res.text()
      setDot(text)
    } catch {
      // Ignore.
    }
  }

  useEffect(() => {
    fetchSvg()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [base, focus, ignore, hide, top])

  useEffect(() => {
    if (svg && containerRef.current) {
      containerRef.current.innerHTML = svg
      // Make the SVG responsive.
      const svgEl = containerRef.current.querySelector('svg')
      if (svgEl) {
        svgEl.setAttribute('width', '100%')
        svgEl.setAttribute('height', 'auto')
        svgEl.style.maxWidth = '100%'
      }
    }
  }, [svg])

  const downloadDot = () => {
    if (!dot) {
      fetchDot().then(() => {
        if (dot) downloadBlob(dot, 'heapprofile.dot', 'text/vnd.graphviz')
      })
      return
    }
    downloadBlob(dot, 'heapprofile.dot', 'text/vnd.graphviz')
  }

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
        <label className="flex items-center gap-1 text-xs text-gray-400">
          top
          <input
            type="number"
            value={top}
            min={1}
            max={200}
            onChange={(e) => setTop(Number(e.target.value) || 20)}
            className="bg-gray-900 border border-gray-700 rounded px-2 py-1 text-xs w-16 focus:border-indigo-500 outline-none"
          />
        </label>
        <button
          onClick={downloadDot}
          className="px-3 py-1.5 bg-gray-800 text-xs rounded hover:bg-gray-700 ml-auto"
        >
          ⬇ Download DOT
        </button>
      </div>

      {error && <p className="text-red-400 text-sm">Failed to load graph: {error}</p>}

      {svgAvailable === false && (
        <div className="text-xs text-amber-300 bg-amber-950/30 border border-amber-800 rounded p-3">
          <strong>Graphviz not detected on server.</strong> The DOT source is ready — install{' '}
          <code className="bg-gray-900 px-1 rounded">graphviz</code> and pipe it through{' '}
          <code className="bg-gray-900 px-1 rounded">dot -Tsvg</code>:
          <pre className="mt-2 text-gray-300 whitespace-pre-wrap">
            {`# from your terminal
hprof dot file.heapprofile | dot -Tsvg -o graph.svg
hprof dot file.heapprofile | dot -Tpng -o graph.png`}
          </pre>
        </div>
      )}

      <div className="bg-gray-900 rounded-lg p-4">
        <div ref={containerRef} className="overflow-auto max-h-[700px]" />
      </div>
    </div>
  )
}

function downloadBlob(content: string, filename: string, mime: string) {
  const blob = new Blob([content], { type: mime })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  document.body.appendChild(a)
  a.click()
  document.body.removeChild(a)
  URL.revokeObjectURL(url)
}
