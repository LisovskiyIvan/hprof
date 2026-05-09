import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router'
import { fetchJson } from '../lib/api'

interface ProfileEntry {
  filePath: string
  fileName: string
  fileSize: number
  type: string
  meta?: { node_count?: number; edge_count?: number }
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

export default function Home() {
  const [profiles, setProfiles] = useState<ProfileEntry[]>([])
  const navigate = useNavigate()

  useEffect(() => {
    let cancelled = false

    fetchJson<ProfileEntry[]>('/api/profiles', { cache: false })
      .then((data) => {
        if (!cancelled) {
          setProfiles(data)
        }
      })
      .catch(() => {
        if (!cancelled) {
          setProfiles([])
        }
      })

    return () => {
      cancelled = true
    }
  }, [])

  return (
    <div className="min-h-screen bg-gray-950 text-gray-100">
      <header className="border-b border-gray-800 px-6 py-4">
        <h1 className="text-xl font-bold tracking-tight">hprof</h1>
        <p className="text-sm text-gray-400">V8 Memory Profile Analyzer</p>
      </header>

      <main className="max-w-4xl mx-auto px-6 py-8">
        <h2 className="text-lg font-semibold mb-4">Loaded Profiles</h2>

        {profiles.length === 0 ? (
          <p className="text-gray-500">No profiles loaded.</p>
        ) : (
          <div className="space-y-2">
            {profiles.map((p) => {
              const id = encodeURIComponent(p.filePath)
              return (
                <button
                  key={p.filePath}
                  onClick={() => navigate(`/profile/${id}`)}
                  className="w-full text-left bg-gray-900 border border-gray-800 rounded-lg px-5 py-4 hover:border-indigo-500 transition-colors"
                >
                  <div className="flex items-center justify-between">
                    <div>
                      <span className="font-mono text-sm text-indigo-400">{p.fileName}</span>
                      <span className="ml-3 text-xs text-gray-500 uppercase">{p.type}</span>
                    </div>
                    <div className="text-sm text-gray-400">
                      {formatBytes(p.fileSize)}
                      {typeof p.meta?.node_count === 'number' && (
                        <span className="ml-3">{p.meta.node_count.toLocaleString()} nodes</span>
                      )}
                    </div>
                  </div>
                </button>
              )
            })}
          </div>
        )}
      </main>
    </div>
  )
}
