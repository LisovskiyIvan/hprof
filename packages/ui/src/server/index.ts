import {
  detectProfileType,
  formatBytes,
  parseHeapProfile,
  summarizeHeapProfile,
  parseSnapshotMeta,
  streamHeapSnapshotSummary,
  parseHeapSnapshot,
  buildRetainedSize,
  parseHeapTimeline,
  streamHeapTimelineSummary,
} from "@hprof/core";
import type {
  ProfileType,
  HeapProfileResult,
  HeapSnapshotResult,
  HeapTimelineResult,
  HeapSnapshotSummary,
  HeapTimelineSummary,
  HeapProfileSummary,
  HeapSnapshotMeta,
} from "@hprof/core";
import fs from "fs";
import path from "path";

export interface ServerOptions {
  files: string[];
  port: number;
  open: boolean;
}

interface ProfileData {
  type: ProfileType;
  filePath: string;
  fileName: string;
  fileSize: number;
  meta?: HeapSnapshotMeta;
  profileResult?: HeapProfileResult;
  snapshotResult?: HeapSnapshotResult;
  timelineResult?: HeapTimelineResult;
  profileSummary?: HeapProfileSummary;
  snapshotSummary?: HeapSnapshotSummary;
  timelineSummary?: HeapTimelineSummary;
  retainedSizes?: number[];
}

const profiles = new Map<string, ProfileData>();

async function loadProfile(filePath: string): Promise<ProfileData> {
  const cached = profiles.get(filePath);
  if (cached) return cached;

  const type = detectProfileType(filePath);
  const stat = fs.statSync(filePath);
  const data: ProfileData = {
    type,
    filePath,
    fileName: path.basename(filePath),
    fileSize: stat.size,
  };

  if (type === "heapsnapshot" || type === "heaptimeline") {
    data.meta = parseSnapshotMeta(filePath);
  }

  profiles.set(filePath, data);
  return data;
}

function jsonResponse(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function errorResponse(message: string, status = 400): Response {
  return jsonResponse({ error: message }, status);
}

function serializeMap<K, V>(map: Map<K, V>): [K, V][] {
  return [...map.entries()];
}

async function handleApiRequest(url: URL): Promise<Response> {
  const pathname = url.pathname;

  if (pathname === "/api/profiles") {
    const result = [];
    for (const [filePath, data] of profiles) {
      result.push({
        filePath,
        fileName: data.fileName,
        fileSize: data.fileSize,
        type: data.type,
        meta: data.meta
          ? {
              node_count: data.meta.node_count,
              edge_count: data.meta.edge_count,
              extra_native_bytes: data.meta.extra_native_bytes,
            }
          : undefined,
      });
    }
    return jsonResponse(result);
  }

  const profileMatch = pathname.match(/^\/api\/profile\/([^/]+)\/(.+)$/);
  if (!profileMatch) {
    const singleMatch = pathname.match(/^\/api\/profile\/(.+)$/);
    if (!singleMatch) return errorResponse("Not found", 404);

    const filePath = decodeURIComponent(singleMatch[1]!);
    const data = profiles.get(filePath);
    if (!data) return errorResponse("Profile not found", 404);

    const meta: Record<string, unknown> = {
      fileName: data.fileName,
      fileSize: data.fileSize,
      type: data.type,
    };
    if (data.meta) {
      meta.node_count = data.meta.node_count;
      meta.edge_count = data.meta.edge_count;
      meta.extra_native_bytes = data.meta.extra_native_bytes;
    }
    return jsonResponse(meta);
  }

  const filePath = decodeURIComponent(profileMatch[1]!);
  const action = profileMatch[2]!;
  const data = profiles.get(filePath);
  if (!data) return errorResponse("Profile not found", 404);

  switch (action) {
    case "meta": {
      const meta: Record<string, unknown> = {
        fileName: data.fileName,
        fileSize: data.fileSize,
        type: data.type,
      };
      if (data.meta) {
        meta.node_count = data.meta.node_count;
        meta.edge_count = data.meta.edge_count;
        meta.extra_native_bytes = data.meta.extra_native_bytes;
      }
      return jsonResponse(meta);
    }

    case "summary": {
      if (data.type === "heapprofile") {
        if (!data.profileResult) {
          data.profileResult = parseHeapProfile(filePath);
        }
        if (!data.profileSummary) {
          data.profileSummary = summarizeHeapProfile(data.profileResult);
        }
        return jsonResponse({
          totalSize: data.profileSummary.totalSize,
          byFrame: serializeMap(data.profileSummary.byFrame),
          byUrl: serializeMap(data.profileSummary.byUrl),
          byFunction: serializeMap(data.profileSummary.byFunction),
        });
      }

      if (data.type === "heapsnapshot") {
        if (!data.snapshotSummary) {
          data.snapshotSummary = await streamHeapSnapshotSummary(filePath);
        }
        return jsonResponse({
          totalSize: data.snapshotSummary.totalSize,
          totalCount: data.snapshotSummary.totalCount,
          byNodeName: serializeMap(data.snapshotSummary.byNodeName),
          byNodeType: serializeMap(data.snapshotSummary.byNodeType),
        });
      }

      if (data.type === "heaptimeline") {
        if (!data.timelineSummary) {
          data.timelineSummary = await streamHeapTimelineSummary(filePath);
        }
        return jsonResponse({
          totalAllocated: data.timelineSummary.totalAllocated,
          totalFreed: data.timelineSummary.totalFreed,
          byType: serializeMap(data.timelineSummary.byType),
        });
      }

      return errorResponse("Unknown profile type");
    }

    case "nodes": {
      if (data.type !== "heapsnapshot") {
        return errorResponse("Nodes only available for heapsnapshot");
      }
      if (!data.snapshotResult) {
        data.snapshotResult = await parseHeapSnapshot(filePath);
      }
      const page = Number(url.searchParams.get("page") ?? "0");
      const pageSize = Number(url.searchParams.get("pageSize") ?? "100");
      const nodeType = url.searchParams.get("type");
      const search = url.searchParams.get("q");
      const sort = url.searchParams.get("sort") ?? "selfSize";
      const sortDir = url.searchParams.get("dir") ?? "desc";

      let filtered = data.snapshotResult.nodes;
      if (nodeType) {
        filtered = filtered.filter((n) => n.type === nodeType);
      }
      if (search) {
        const re = new RegExp(search, "i");
        filtered = filtered.filter((n) => re.test(n.name));
      }

      filtered.sort((a, b) => {
        const aVal = a[sort as keyof typeof a] ?? 0;
        const bVal = b[sort as keyof typeof b] ?? 0;
        const cmp = typeof aVal === "number" && typeof bVal === "number" ? aVal - bVal : String(aVal).localeCompare(String(bVal));
        return sortDir === "desc" ? -cmp : cmp;
      });

      const total = filtered.length;
      const start = page * pageSize;
      const nodes = filtered.slice(start, start + pageSize);

      return jsonResponse({
        total,
        page,
        pageSize,
        nodes,
      });
    }

    case "edges": {
      if (data.type !== "heapsnapshot") {
        return errorResponse("Edges only available for heapsnapshot");
      }
      if (!data.snapshotResult) {
        data.snapshotResult = await parseHeapSnapshot(filePath);
      }
      const nodeId = Number(url.searchParams.get("nodeId"));
      if (!nodeId) return errorResponse("nodeId required");

      const node = data.snapshotResult.nodes[nodeId];
      if (!node) return errorResponse("Node not found");

      let offset = 0;
      for (let i = 0; i < nodeId; i++) {
        offset += data.snapshotResult.nodes[i]!.edgeCount;
      }
      const edges = data.snapshotResult.edges.slice(offset, offset + node.edgeCount);

      return jsonResponse({
        node,
        edges,
      });
    }

    case "tree": {
      if (data.type !== "heapprofile") {
        return errorResponse("Tree only available for heapprofile");
      }
      if (!data.profileResult) {
        data.profileResult = parseHeapProfile(filePath);
      }
      return jsonResponse(data.profileResult.head);
    }

    case "timeline": {
      if (data.type !== "heaptimeline") {
        return errorResponse("Timeline only available for heaptimeline");
      }
      if (!data.timelineResult) {
        data.timelineResult = await parseHeapTimeline(filePath);
      }
      return jsonResponse({
        timeline: data.timelineResult.timeline,
      });
    }

    case "search": {
      const q = url.searchParams.get("q");
      if (!q) return errorResponse("q parameter required");

      if (data.type === "heapsnapshot" || data.type === "heaptimeline") {
        if (!data.snapshotResult && data.type === "heapsnapshot") {
          data.snapshotResult = await parseHeapSnapshot(filePath);
        }
        if (!data.timelineResult && data.type === "heaptimeline") {
          data.timelineResult = await parseHeapTimeline(filePath);
        }
        const strings = data.snapshotResult?.strings ?? data.timelineResult?.strings ?? [];
        const re = new RegExp(q, "i");
        const matches = strings
          .map((s, i) => ({ index: i, value: s }))
          .filter((s) => re.test(s.value))
          .slice(0, 100);
        return jsonResponse({ matches });
      }

      if (data.type === "heapprofile") {
        if (!data.profileResult) {
          data.profileResult = parseHeapProfile(filePath);
        }
        if (!data.profileSummary) {
          data.profileSummary = summarizeHeapProfile(data.profileResult);
        }
        const re = new RegExp(q, "i");
        const frames = [...data.profileSummary.byFrame.entries()]
          .filter(([frame]) => re.test(frame))
          .slice(0, 100);
        return jsonResponse({ matches: frames.map(([frame, size]) => ({ frame, size })) });
      }

      return errorResponse("Unknown profile type");
    }

    case "retained": {
      if (data.type !== "heapsnapshot") {
        return errorResponse("Retained size only available for heapsnapshot");
      }
      if (!data.snapshotResult) {
        data.snapshotResult = await parseHeapSnapshot(filePath);
      }
      if (!data.retainedSizes) {
        data.retainedSizes = buildRetainedSize(data.snapshotResult);
      }

      const topN = Number(url.searchParams.get("top") ?? "30");
      const indexed = data.retainedSizes
        .map((size, idx) => ({ idx, size }))
        .sort((a, b) => b.size - a.size)
        .slice(0, topN);

      const result = indexed.map(({ idx, size }) => {
        const node = data.snapshotResult!.nodes[idx]!;
        return {
          nodeIndex: idx,
          name: node.name,
          type: node.type,
          selfSize: node.selfSize,
          retainedSize: size,
        };
      });

      return jsonResponse({ retained: result });
    }

    default:
      return errorResponse(`Unknown action: ${action}`, 404);
  }
}

export async function startServer(options: ServerOptions): Promise<void> {
  for (const file of options.files) {
    const resolved = path.resolve(file);
    if (!fs.existsSync(resolved)) {
      console.error(`File not found: ${resolved}`);
      process.exit(1);
    }
    await loadProfile(resolved);
  }

  if (profiles.size === 0) {
    console.error("No profile files specified");
    process.exit(1);
  }

  const server = Bun.serve({
    port: options.port,
    async fetch(req) {
      const url = new URL(req.url);

      if (url.pathname.startsWith("/api/")) {
        return handleApiRequest(url);
      }

      const clientDist = path.join(import.meta.dir, "client", "dist");
      let filePath = path.join(clientDist, url.pathname === "/" ? "index.html" : url.pathname);

      if (!fs.existsSync(filePath)) {
        filePath = path.join(clientDist, "index.html");
      }

      if (fs.existsSync(filePath)) {
        const ext = path.extname(filePath);
        const mimeTypes: Record<string, string> = {
          ".html": "text/html",
          ".js": "application/javascript",
          ".css": "text/css",
          ".json": "application/json",
          ".png": "image/png",
          ".svg": "image/svg+xml",
          ".ico": "image/x-icon",
        };
        return new Response(Bun.file(filePath), {
          headers: { "Content-Type": mimeTypes[ext] ?? "application/octet-stream" },
        });
      }

      return new Response("Not found", { status: 404 });
    },
  });

  console.log(`\n  hprof UI server running at http://localhost:${server.port}`);
  console.log(`\n  Profiles:`);
  for (const [fp, data] of profiles) {
    console.log(`    ${data.fileName} (${data.type}, ${formatBytes(data.fileSize)})`);
  }
  console.log();

  if (options.open) {
    const opener =
      process.platform === "darwin"
        ? "open"
        : process.platform === "win32"
          ? "start"
          : "xdg-open";
    Bun.spawn([opener, `http://localhost:${server.port}`]);
  }
}
