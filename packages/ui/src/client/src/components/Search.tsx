import { useState } from "react";
import { fetchJson } from "../lib/api";

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes)) return String(bytes);
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`;
}

interface StringMatch {
  index: number;
  value: string;
}

interface FrameMatch {
  frame: string;
  size: number;
}

export default function Search({ base, type }: { base: string; type: string }) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<StringMatch[] | FrameMatch[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const doSearch = () => {
    if (!query.trim()) return;
    setLoading(true);
    setError(null);
    fetchJson<{ matches: StringMatch[] | FrameMatch[] }>(`${base}/search?q=${encodeURIComponent(query)}`, { cache: false })
      .then((data) => {
        setResults(data.matches);
        setLoading(false);
      })
      .catch((e) => {
        setLoading(false);
        setError(e.message);
      });
  };

  return (
    <div className="space-y-4">
      <div className="flex gap-3 items-center">
        <input
          type="text"
          placeholder="Search strings, names, frames..."
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && doSearch()}
          className="bg-gray-900 border border-gray-700 rounded px-3 py-1.5 text-sm w-96 focus:border-indigo-500 outline-none"
        />
        <button
          onClick={doSearch}
          disabled={loading}
          className="px-4 py-1.5 bg-indigo-600 text-white text-sm rounded hover:bg-indigo-700 disabled:opacity-50"
        >
          Search
        </button>
      </div>

      {error && <p className="text-red-400 text-sm">Failed to search: {error}</p>}

      {results && (
        <div className="bg-gray-900 rounded-lg overflow-hidden">
          {results.length === 0 ? (
            <p className="px-4 py-3 text-gray-500 text-sm">No results found.</p>
          ) : type === "heapprofile" ? (
            (results as FrameMatch[]).map((r, i) => (
              <div key={i} className="flex items-center px-4 py-2 border-b border-gray-800/50 last:border-0 text-sm">
                <span className="font-mono text-xs text-indigo-400 flex-1 truncate">{r.frame}</span>
                <span className="text-gray-300 text-xs ml-4">{formatBytes(r.size)}</span>
              </div>
            ))
          ) : (
            (results as StringMatch[]).map((r, i) => (
              <div key={i} className="flex items-center px-4 py-2 border-b border-gray-800/50 last:border-0 text-sm">
                <span className="text-gray-500 text-xs font-mono w-16">#{r.index}</span>
                <span className="font-mono text-xs text-indigo-400 flex-1 truncate">{r.value}</span>
              </div>
            ))
          )}
        </div>
      )}
    </div>
  );
}
