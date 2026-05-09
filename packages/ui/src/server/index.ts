import {
  detectProfileType,
  formatBytes,
  HeapProfile,
  HeapSnapshot,
  HeapTimeline,
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
import { spawn } from "child_process";
import fs from "fs";
import { createServer } from "http";
import path from "path";
import { fileURLToPath } from "url";

export interface ServerOptions {
  files: string[];
  port: number;
  open: boolean;
}

const FULL_PARSE_NODE_LIMIT = 5_000_000;

type ApiError = Error & { status?: number };

interface ProfileData {
  type: ProfileType;
  filePath: string;
  fileName: string;
  fileSize: number;
  profile?: HeapProfile;
  snapshot?: HeapSnapshot;
  timeline?: HeapTimeline;
  profileResult?: HeapProfileResult;
  snapshotResult?: HeapSnapshotResult;
  timelineResult?: HeapTimelineResult;
  profileResultPromise?: Promise<HeapProfileResult>;
  snapshotResultPromise?: Promise<HeapSnapshotResult>;
  timelineResultPromise?: Promise<HeapTimelineResult>;
  profileSummary?: HeapProfileSummary;
  snapshotSummary?: HeapSnapshotSummary;
  timelineSummary?: HeapTimelineSummary;
  retainedSizes?: number[];
  profileSummaryPromise?: Promise<HeapProfileSummary>;
  snapshotSummaryPromise?: Promise<HeapSnapshotSummary>;
  timelineSummaryPromise?: Promise<HeapTimelineSummary>;
  retainedSizesPromise?: Promise<number[]>;
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

  if (type === "heapprofile") {
    data.profile = new HeapProfile(filePath);
  } else if (type === "heapsnapshot") {
    data.snapshot = new HeapSnapshot(filePath);
  } else if (type === "heaptimeline") {
    data.timeline = new HeapTimeline(filePath);
  }

  profiles.set(filePath, data);
  return data;
}

function getMeta(data: ProfileData): HeapSnapshotMeta | undefined {
  if (data.snapshot) return data.snapshot.meta;
  if (data.timeline) return data.timeline.meta;
  return undefined;
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

function apiError(message: string, status: number): ApiError {
  const error = new Error(message) as ApiError;
  error.status = status;
  return error;
}

function ensureInteractiveParseAllowed(data: ProfileData, feature: string) {
  const meta = getMeta(data);
  if (!meta || meta.node_count <= FULL_PARSE_NODE_LIMIT) return;
  throw apiError(
    `${feature} is disabled for very large profiles (${meta.node_count.toLocaleString()} nodes). Use summary view or CLI output instead.`,
    413,
  );
}

async function ensureProfileSummary(data: ProfileData): Promise<HeapProfileSummary> {
  if (data.profileSummary) return data.profileSummary;
  if (!data.profileSummaryPromise) {
    data.profileSummaryPromise = Promise.resolve().then(() => {
      data.profileSummary = data.profile!.summarize();
      return data.profileSummary;
    }).finally(() => {
      data.profileSummaryPromise = undefined;
    });
  }
  return data.profileSummaryPromise;
}

async function ensureSnapshotSummary(data: ProfileData): Promise<HeapSnapshotSummary> {
  if (data.snapshotSummary) return data.snapshotSummary;
  if (!data.snapshotSummaryPromise) {
    data.snapshotSummaryPromise = data.snapshot!.streamSummary().then((summary) => {
      data.snapshotSummary = summary;
      return summary;
    }).finally(() => {
      data.snapshotSummaryPromise = undefined;
    });
  }
  return data.snapshotSummaryPromise;
}

async function ensureTimelineSummary(data: ProfileData): Promise<HeapTimelineSummary> {
  if (data.timelineSummary) return data.timelineSummary;
  if (!data.timelineSummaryPromise) {
    data.timelineSummaryPromise = data.timeline!.streamSummary().then((summary) => {
      data.timelineSummary = summary;
      return summary;
    }).finally(() => {
      data.timelineSummaryPromise = undefined;
    });
  }
  return data.timelineSummaryPromise;
}

async function ensureSnapshotResult(data: ProfileData): Promise<HeapSnapshotResult> {
  ensureInteractiveParseAllowed(data, "This view");
  if (data.snapshotResult) return data.snapshotResult;
  if (!data.snapshotResultPromise) {
    data.snapshotResultPromise = Promise.resolve().then(() => {
      data.snapshotResult = data.snapshot!.data;
      return data.snapshotResult;
    }).finally(() => {
      data.snapshotResultPromise = undefined;
    });
  }
  return data.snapshotResultPromise;
}

async function ensureTimelineResult(data: ProfileData): Promise<HeapTimelineResult> {
  ensureInteractiveParseAllowed(data, "This view");
  if (data.timelineResult) return data.timelineResult;
  if (!data.timelineResultPromise) {
    data.timelineResultPromise = Promise.resolve().then(() => {
      data.timelineResult = data.timeline!.data;
      return data.timelineResult;
    }).finally(() => {
      data.timelineResultPromise = undefined;
    });
  }
  return data.timelineResultPromise;
}

async function ensureRetainedSizes(data: ProfileData): Promise<number[]> {
  ensureInteractiveParseAllowed(data, "This view");
  if (data.retainedSizes) return data.retainedSizes;
  if (!data.retainedSizesPromise) {
    data.retainedSizesPromise = ensureSnapshotResult(data).then(() => {
      data.retainedSizes = data.snapshot!.retainedSizes;
      return data.retainedSizes;
    }).finally(() => {
      data.retainedSizesPromise = undefined;
    });
  }
  return data.retainedSizesPromise;
}

async function sendResponse(response: Response, res: import("http").ServerResponse) {
  res.statusCode = response.status;
  response.headers.forEach((value, key) => {
    res.setHeader(key, value);
  });
  const body = await response.arrayBuffer();
  res.end(Buffer.from(body));
}

function staticResponse(clientDist: string, pathname: string): Response {
  const relativePath = pathname === "/" ? "index.html" : pathname.replace(/^\//, "");
  const requestedPath = path.resolve(clientDist, relativePath);
  const indexPath = path.join(clientDist, "index.html");

  if (!requestedPath.startsWith(clientDist + path.sep) && requestedPath !== indexPath) {
    return new Response("Not found", { status: 404 });
  }

  let filePath = requestedPath;
  if (!fs.existsSync(filePath) || fs.statSync(filePath).isDirectory()) {
    filePath = indexPath;
  }

  if (!fs.existsSync(filePath)) {
    return new Response("UI bundle not found. Build packages/ui/src/client first.", {
      status: 500,
    });
  }

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

  return new Response(fs.readFileSync(filePath), {
    headers: { "Content-Type": mimeTypes[ext] ?? "application/octet-stream" },
  });
}

function serializeMap<K, V>(map: Map<K, V>): [K, V][] {
  return [...map.entries()];
}

async function handleApiRequest(url: URL): Promise<Response> {
  const pathname = url.pathname;

  if (pathname === "/api/profiles") {
    const result = [];
    for (const [filePath, data] of profiles) {
      const meta = getMeta(data);
      result.push({
        filePath,
        fileName: data.fileName,
        fileSize: data.fileSize,
        type: data.type,
        meta: meta
          ? {
              node_count: meta.node_count,
              edge_count: meta.edge_count,
              extra_native_bytes: meta.extra_native_bytes,
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

    const meta = getMeta(data);
    const metaObj: Record<string, unknown> = {
      fileName: data.fileName,
      fileSize: data.fileSize,
      type: data.type,
    };
    if (meta) {
      metaObj.node_count = meta.node_count;
      metaObj.edge_count = meta.edge_count;
      metaObj.extra_native_bytes = meta.extra_native_bytes;
    }
    return jsonResponse(metaObj);
  }

  const filePath = decodeURIComponent(profileMatch[1]!);
  const action = profileMatch[2]!;
  const data = profiles.get(filePath);
  if (!data) return errorResponse("Profile not found", 404);

  switch (action) {
    case "meta": {
      const meta = getMeta(data);
      const metaObj: Record<string, unknown> = {
        fileName: data.fileName,
        fileSize: data.fileSize,
        type: data.type,
      };
      if (meta) {
        metaObj.node_count = meta.node_count;
        metaObj.edge_count = meta.edge_count;
        metaObj.extra_native_bytes = meta.extra_native_bytes;
      }
      return jsonResponse(metaObj);
    }

    case "summary": {
      if (data.type === "heapprofile") {
        const profileSummary = await ensureProfileSummary(data);
        return jsonResponse({
          totalSize: profileSummary.totalSize,
          byFrame: serializeMap(profileSummary.byFrame),
          byUrl: serializeMap(profileSummary.byUrl),
          byFunction: serializeMap(profileSummary.byFunction),
        });
      }

      if (data.type === "heapsnapshot") {
        const snapshotSummary = await ensureSnapshotSummary(data);
        return jsonResponse({
          totalSize: snapshotSummary.totalSize,
          totalCount: snapshotSummary.totalCount,
          byNodeName: serializeMap(snapshotSummary.byNodeName),
          byNodeType: serializeMap(snapshotSummary.byNodeType),
        });
      }

      if (data.type === "heaptimeline") {
        const timelineSummary = await ensureTimelineSummary(data);
        return jsonResponse({
          totalAllocated: timelineSummary.totalAllocated,
          totalFreed: timelineSummary.totalFreed,
          byType: serializeMap(timelineSummary.byType),
        });
      }

      return errorResponse("Unknown profile type");
    }

    case "nodes": {
      if (data.type !== "heapsnapshot") {
        return errorResponse("Nodes only available for heapsnapshot");
      }
      const snapshotResult = await ensureSnapshotResult(data);
      const page = Number(url.searchParams.get("page") ?? "0");
      const pageSize = Number(url.searchParams.get("pageSize") ?? "100");
      const nodeType = url.searchParams.get("type");
      const search = url.searchParams.get("q");
      const sort = url.searchParams.get("sort") ?? "selfSize";
      const sortDir = url.searchParams.get("dir") ?? "desc";

      let filtered = snapshotResult.nodes;
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
      const snapshotResult = await ensureSnapshotResult(data);
      const nodeId = Number(url.searchParams.get("nodeId"));
      if (!nodeId) return errorResponse("nodeId required");

      const node = snapshotResult.nodes[nodeId];
      if (!node) return errorResponse("Node not found");

      let offset = 0;
      for (let i = 0; i < nodeId; i++) {
        offset += snapshotResult.nodes[i]!.edgeCount;
      }
      const edges = snapshotResult.edges.slice(offset, offset + node.edgeCount);

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
        data.profileResult = data.profile!.data;
      }
      return jsonResponse(data.profileResult.head);
    }

    case "timeline": {
      if (data.type !== "heaptimeline") {
        return errorResponse("Timeline only available for heaptimeline");
      }
      const timelineResult = await ensureTimelineResult(data);
      return jsonResponse({
        timeline: timelineResult.timeline,
      });
    }

    case "search": {
      const q = url.searchParams.get("q");
      if (!q) return errorResponse("q parameter required");

      if (data.type === "heapsnapshot" || data.type === "heaptimeline") {
        if (data.type === "heapsnapshot") {
          const snapshotResult = await ensureSnapshotResult(data);
          const re = new RegExp(q, "i");
          const matches = snapshotResult.strings
            .map((s, i) => ({ index: i, value: s }))
            .filter((s) => re.test(s.value))
            .slice(0, 100);
          return jsonResponse({ matches });
        }

        const timelineResult = await ensureTimelineResult(data);
        const re = new RegExp(q, "i");
        const matches = timelineResult.strings
          .map((s, i) => ({ index: i, value: s }))
          .filter((s) => re.test(s.value))
          .slice(0, 100);
        return jsonResponse({ matches });
      }

      if (data.type === "heapprofile") {
        const profileSummary = await ensureProfileSummary(data);
        const re = new RegExp(q, "i");
        const frames = [...profileSummary.byFrame.entries()]
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
      const snapshotResult = await ensureSnapshotResult(data);

      const retainedSizes = await ensureRetainedSizes(data);
      const topN = Number(url.searchParams.get("top") ?? "30");
      const indexed = retainedSizes
        .map((size, idx) => ({ idx, size }))
        .sort((a, b) => b.size - a.size)
        .slice(0, topN);

      const result = indexed.map(({ idx, size }) => {
        const node = snapshotResult.nodes[idx]!;
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

  const clientDist = fileURLToPath(new URL("../client/dist", import.meta.url));
  const server = createServer(async (req, res) => {
    try {
      const url = new URL(req.url ?? "/", `http://${req.headers.host ?? `localhost:${options.port}`}`);
      const response = url.pathname.startsWith("/api/")
        ? await handleApiRequest(url)
        : staticResponse(clientDist, url.pathname);
      await sendResponse(response, res);
    } catch (error) {
      const apiErr = error as ApiError;
      await sendResponse(errorResponse(apiErr.message, apiErr.status ?? 500), res);
    }
  });

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(options.port, () => resolve());
  });

  const address = server.address();
  const port = typeof address === "object" && address ? address.port : options.port;

  console.log(`\n  hprof UI server running at http://localhost:${port}`);
  console.log(`\n  Profiles:`);
  for (const data of profiles.values()) {
    console.log(`    ${data.fileName} (${data.type}, ${formatBytes(data.fileSize)})`);
  }
  console.log();

  if (options.open) {
    const targetUrl = `http://localhost:${port}`;
    if (process.platform === "win32") {
      spawn("cmd", ["/c", "start", "", targetUrl], {
        detached: true,
        stdio: "ignore",
      }).unref();
    } else {
      const opener = process.platform === "darwin" ? "open" : "xdg-open";
      spawn(opener, [targetUrl], {
        detached: true,
        stdio: "ignore",
      }).unref();
    }
  }
}
