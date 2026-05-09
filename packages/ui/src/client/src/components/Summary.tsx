import { useEffect, useState } from "react";
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

interface SummaryData {
  totalSize?: number;
  totalCount?: number;
  totalAllocated?: number;
  totalFreed?: number;
  byFrame?: [string, number][];
  byUrl?: [string, number][];
  byFunction?: [string, number][];
  byNodeName?: [string, { size: number; count: number }][];
  byNodeType?: [string, { size: number; count: number }][];
  byType?: [string, { allocated: number; freed: number; count: number }][];
}

export default function Summary({ base, type }: { base: string; type: string }) {
  const [data, setData] = useState<SummaryData | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setError(null);
    fetchJson<SummaryData>(`${base}/summary`)
      .then(setData)
      .catch((e) => setError(e.message));
  }, [base]);

  if (error) return <p className="text-red-400 text-sm">Failed to load summary: {error}</p>;
  if (!data) return <p className="text-gray-500">Loading summary...</p>;

  if (type === "heapprofile") {
    return (
      <div className="space-y-8">
        <div className="text-sm text-gray-400">
          Total sampled size: <span className="text-white font-semibold">{formatBytes(data.totalSize ?? 0)}</span>
        </div>
        <Table title="Top Frames" rows={data.byFrame ?? []} format={(v) => formatBytes(v)} />
        <Table title="Top URLs" rows={data.byUrl ?? []} format={(v) => formatBytes(v)} />
        <Table title="Top Functions" rows={data.byFunction ?? []} format={(v) => formatBytes(v)} />
      </div>
    );
  }

  if (type === "heapsnapshot") {
    const typeRows = data.byNodeType ?? [];
    const totalTypeSize = typeRows.reduce((s, [, v]) => s + v.size, 0);
    return (
      <div className="space-y-8">
        <div className="text-sm text-gray-400">
          Total self size: <span className="text-white font-semibold">{formatBytes(data.totalSize ?? 0)}</span>
          {" | "}Nodes: <span className="text-white">{data.totalCount?.toLocaleString()}</span>
        </div>
        <SizeBarChart rows={typeRows} total={totalTypeSize} />
        <Table
          title="Top Node Names"
          rows={(data.byNodeName ?? []).map(([name, info]) => [name, info.size] as [string, number])}
          format={(v) => formatBytes(v)}
        />
      </div>
    );
  }

  if (type === "heaptimeline") {
    return (
      <div className="space-y-8">
        <div className="text-sm text-gray-400">
          Total allocated: <span className="text-white font-semibold">{formatBytes(data.totalAllocated ?? 0)}</span>
        </div>
        <Table
          title="Allocations By Type"
          rows={(data.byType ?? []).map(([t, info]) => [t, info.allocated] as [string, number])}
          format={(v) => formatBytes(v)}
        />
      </div>
    );
  }

  return null;
}

function Table({
  title,
  rows,
  format,
}: {
  title: string;
  rows: [string, number][];
  format: (v: number) => string;
}) {
  if (!rows.length) return null;
  return (
    <div>
      <h3 className="text-sm font-semibold text-gray-300 mb-2">{title}</h3>
      <div className="bg-gray-900 rounded-lg overflow-hidden">
        {rows.slice(0, 30).map(([key, value], i) => (
          <div key={i} className="flex items-center px-4 py-2 border-b border-gray-800 last:border-0 text-sm">
            <span className="text-indigo-400 font-mono flex-1 truncate" title={key}>
              {key}
            </span>
            <span className="text-gray-300 ml-4 whitespace-nowrap">{format(value)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function SizeBarChart({
  rows,
  total,
}: {
  rows: [string, { size: number; count: number }][];
  total: number;
}) {
  if (!rows.length) return null;
  const chartRows = rows.map(([type, info], i) => ({
    type,
    info,
    color: `hsl(${(i * 47) % 360} 78% 58%)`,
  }));

  return (
    <div>
      <h3 className="text-sm font-semibold text-gray-300 mb-2">Size by Node Type</h3>
      <div className="bg-gray-900 rounded-lg p-4">
        <div className="flex h-8 rounded overflow-hidden mb-3">
          {chartRows.map(({ type, info, color }) => {
            const pct = total > 0 ? (info.size / total) * 100 : 0;
            if (pct < 0.5) return null;
            return (
              <div
                key={type}
                className="h-full border-r border-gray-950/80 last:border-r-0"
                style={{ width: `${pct}%`, backgroundColor: color }}
                title={`${type}: ${formatBytes(info.size)} (${pct.toFixed(1)}%)`}
              />
            );
          })}
        </div>
        <div className="grid grid-cols-2 gap-x-6 gap-y-1">
          {chartRows.map(({ type, info, color }) => (
            <div key={type} className="flex items-center text-xs gap-2">
              <span className="w-2.5 h-2.5 rounded-sm shrink-0" style={{ backgroundColor: color }} />
              <span className="text-gray-400 truncate">{type}</span>
              <span className="text-gray-300 ml-auto whitespace-nowrap">{formatBytes(info.size)}</span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
