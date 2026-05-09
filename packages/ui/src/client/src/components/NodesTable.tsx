import { useEffect, useState, useCallback } from "react";
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

interface NodeEntry {
  type: string;
  name: string;
  selfSize: number;
  id: number;
  edgeCount: number;
  retentionSize?: number;
}

interface NodesResponse {
  total: number;
  page: number;
  pageSize: number;
  nodes: NodeEntry[];
}

export default function NodesTable({ base }: { base: string }) {
  const [data, setData] = useState<NodesResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [page, setPage] = useState(0);
  const [pageSize] = useState(100);
  const [sort, setSort] = useState("selfSize");
  const [dir, setDir] = useState<"asc" | "desc">("desc");
  const [filterType, setFilterType] = useState("");
  const [search, setSearch] = useState("");

  const fetchNodes = useCallback(() => {
    setError(null);
    const params = new URLSearchParams({
      page: String(page),
      pageSize: String(pageSize),
      sort,
      dir,
    });
    if (filterType) params.set("type", filterType);
    if (search) params.set("q", search);

    fetchJson<NodesResponse>(`${base}/nodes?${params}`)
      .then(setData)
      .catch((e) => setError(e.message));
  }, [base, page, pageSize, sort, dir, filterType, search]);

  useEffect(() => {
    fetchNodes();
  }, [fetchNodes]);

  const totalPages = data ? Math.ceil(data.total / pageSize) : 0;

  const toggleSort = (col: string) => {
    if (sort === col) {
      setDir((d) => (d === "desc" ? "asc" : "desc"));
    } else {
      setSort(col);
      setDir("desc");
    }
    setPage(0);
  };

  const SortIcon = ({ col }: { col: string }) => {
    if (sort !== col) return <span className="text-gray-600 ml-1">↕</span>;
    return <span className="text-indigo-400 ml-1">{dir === "desc" ? "↓" : "↑"}</span>;
  };

  if (error) {
    return <p className="text-red-400 text-sm">Failed to load nodes: {error}</p>;
  }

  return (
    <div className="space-y-4">
      <div className="flex gap-3 items-center">
        <input
          type="text"
          placeholder="Search by name..."
          value={search}
          onChange={(e) => { setSearch(e.target.value); setPage(0); }}
          className="bg-gray-900 border border-gray-700 rounded px-3 py-1.5 text-sm w-64 focus:border-indigo-500 outline-none"
        />
        <input
          type="text"
          placeholder="Filter by type..."
          value={filterType}
          onChange={(e) => { setFilterType(e.target.value); setPage(0); }}
          className="bg-gray-900 border border-gray-700 rounded px-3 py-1.5 text-sm w-48 focus:border-indigo-500 outline-none"
        />
        {data && (
          <span className="text-xs text-gray-500">
            {data.total.toLocaleString()} nodes
          </span>
        )}
      </div>

      <div className="bg-gray-900 rounded-lg overflow-hidden">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-gray-800 text-gray-400">
              <th className="text-left px-4 py-2 cursor-pointer select-none" onClick={() => toggleSort("id")}># <SortIcon col="id" /></th>
              <th className="text-left px-4 py-2 cursor-pointer select-none" onClick={() => toggleSort("type")}>Type <SortIcon col="type" /></th>
              <th className="text-left px-4 py-2 cursor-pointer select-none" onClick={() => toggleSort("name")}>Name <SortIcon col="name" /></th>
              <th className="text-right px-4 py-2 cursor-pointer select-none" onClick={() => toggleSort("selfSize")}>Self Size <SortIcon col="selfSize" /></th>
              <th className="text-right px-4 py-2 cursor-pointer select-none" onClick={() => toggleSort("edgeCount")}>Edges <SortIcon col="edgeCount" /></th>
            </tr>
          </thead>
          <tbody>
            {data?.nodes.map((node, i) => (
              <tr key={node.id} className="border-b border-gray-800/50 hover:bg-gray-800/30">
                <td className="px-4 py-1.5 text-gray-500 font-mono text-xs">{page * pageSize + i}</td>
                <td className="px-4 py-1.5">
                  <span className="px-1.5 py-0.5 rounded text-xs bg-gray-800 text-gray-300">{node.type}</span>
                </td>
                <td className="px-4 py-1.5 font-mono text-xs text-indigo-400 truncate max-w-md" title={node.name}>
                  {node.name}
                </td>
                <td className="px-4 py-1.5 text-right font-mono text-xs">{formatBytes(node.selfSize)}</td>
                <td className="px-4 py-1.5 text-right font-mono text-xs text-gray-400">{node.edgeCount}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {totalPages > 1 && (
        <div className="flex items-center gap-2 text-sm">
          <button
            disabled={page === 0}
            onClick={() => setPage((p) => p - 1)}
            className="px-3 py-1 bg-gray-800 rounded disabled:opacity-30 hover:bg-gray-700"
          >
            Prev
          </button>
          <span className="text-gray-400">
            Page {page + 1} of {totalPages}
          </span>
          <button
            disabled={page >= totalPages - 1}
            onClick={() => setPage((p) => p + 1)}
            className="px-3 py-1 bg-gray-800 rounded disabled:opacity-30 hover:bg-gray-700"
          >
            Next
          </button>
        </div>
      )}
    </div>
  );
}
