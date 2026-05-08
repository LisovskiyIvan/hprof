import { useEffect, useState, useRef } from "react";
import uPlot from "uplot";

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`;
}

interface TimelineEntry {
  type: "Allocation" | "Relocation";
  timestamp: number;
  nodeId: number;
  size: number;
}

interface TimelineData {
  timeline: TimelineEntry[];
}

interface Bucket {
  time: number;
  allocated: number;
  freed: number;
}

export default function Timeline({ base }: { base: string }) {
  const [data, setData] = useState<TimelineData | null>(null);
  const [bucketCount, setBucketCount] = useState(50);
  const chartRef = useRef<HTMLDivElement>(null);
  const plotRef = useRef<uPlot | null>(null);

  useEffect(() => {
    fetch(`${base}/timeline`)
      .then((r) => r.json())
      .then(setData);
  }, [base]);

  useEffect(() => {
    if (!data || !chartRef.current) return;

    const entries = data.timeline;
    if (!entries.length) return;

    const minTime = Math.min(...entries.map((e) => e.timestamp));
    const maxTime = Math.max(...entries.map((e) => e.timestamp));
    const span = maxTime - minTime || 1;
    const bucketSize = span / bucketCount;

    const buckets: Bucket[] = Array.from({ length: bucketCount }, (_, i) => ({
      time: minTime + i * bucketSize,
      allocated: 0,
      freed: 0,
    }));

    for (const entry of entries) {
      const idx = Math.min(
        Math.floor((entry.timestamp - minTime) / bucketSize),
        bucketCount - 1,
      );
      if (entry.type === "Allocation") {
        buckets[idx]!.allocated += entry.size;
      } else {
        buckets[idx]!.freed += entry.size;
      }
    }

    const tSeries = buckets.map((b) => b.time);
    const allocSeries = buckets.map((b) => b.allocated);
    const freedSeries = buckets.map((b) => b.freed);

    if (plotRef.current) {
      plotRef.current.destroy();
    }

    plotRef.current = new uPlot(
      {
        width: chartRef.current.clientWidth,
        height: 300,
        cursor: { drag: { x: true, y: true } },
        scales: {
          x: { time: false },
          y: { auto: true },
        },
        axes: [
          {
            stroke: "#6b7280",
            grid: { stroke: "#374151" },
            ticks: { stroke: "#374151" },
            values: (_, ticks) =>
              ticks.map((t) => {
                const idx = Math.round(t);
                const b = buckets[idx];
                return b ? `${(b.time - minTime).toFixed(0)}ms` : "";
              }),
          },
          {
            stroke: "#6b7280",
            grid: { stroke: "#374151" },
            ticks: { stroke: "#374151" },
            values: (_, ticks) => ticks.map((t) => formatBytes(t)),
          },
        ],
        series: [
          {},
          {
            label: "Allocated",
            stroke: "#6366f1",
            fill: "rgba(99,102,241,0.3)",
            width: 2,
          },
          {
            label: "Freed",
            stroke: "#ef4444",
            fill: "rgba(239,68,68,0.2)",
            width: 2,
          },
        ],
        legend: {
          live: true,
        },
      },
      [tSeries, allocSeries, freedSeries],
      chartRef.current,
    );

    const onResize = () => {
      if (plotRef.current && chartRef.current) {
        plotRef.current.setSize({
          width: chartRef.current.clientWidth,
          height: 300,
        });
      }
    };
    window.addEventListener("resize", onResize);

    return () => {
      window.removeEventListener("resize", onResize);
      plotRef.current?.destroy();
      plotRef.current = null;
    };
  }, [data, bucketCount]);

  if (!data) return <p className="text-gray-500">Loading timeline...</p>;

  if (!data.timeline.length) {
    return (
      <div className="space-y-4">
        <p className="text-gray-400 text-sm">No timeline entries in this profile.</p>
        <p className="text-gray-500 text-xs">
          Timeline data is only available for heaptimeline files with explicit allocation records.
          This snapshot-based timeline shows aggregate node self sizes by type instead.
        </p>
      </div>
    );
  }

  const totalAlloc = data.timeline
    .filter((e) => e.type === "Allocation")
    .reduce((s, e) => s + e.size, 0);
  const totalFreed = data.timeline
    .filter((e) => e.type === "Relocation")
    .reduce((s, e) => s + e.size, 0);

  return (
    <div className="space-y-4">
      <div className="flex gap-6 text-sm text-gray-400">
        <span>Entries: <span className="text-white">{data.timeline.length.toLocaleString()}</span></span>
        <span>Allocated: <span className="text-indigo-400">{formatBytes(totalAlloc)}</span></span>
        <span>Freed: <span className="text-red-400">{formatBytes(totalFreed)}</span></span>
      </div>
      <div className="flex items-center gap-3">
        <label className="text-xs text-gray-400">Buckets:</label>
        <select
          value={bucketCount}
          onChange={(e) => setBucketCount(Number(e.target.value))}
          className="bg-gray-900 border border-gray-700 rounded px-2 py-1 text-sm"
        >
          <option value={20}>20</option>
          <option value={50}>50</option>
          <option value={100}>100</option>
          <option value={200}>200</option>
        </select>
      </div>
      <div ref={chartRef} className="bg-gray-900 rounded-lg p-2" />
    </div>
  );
}
