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
  HeapSnapshotSummary,
  HeapTimelineSummary,
  HeapProfileSummary,
  HeapSnapshotMeta,
} from "@hprof/core";
import path from "path";
import { fileURLToPath } from "url";

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
  profile?: HeapProfile;
  snapshot?: HeapSnapshot;
  timeline?: HeapTimeline;
  profileResult?: HeapProfileResult;
  profileResultPromise?: Promise<HeapProfileResult>;
  profileSummary?: HeapProfileSummary;
  snapshotSummary?: HeapSnapshotSummary;
  timelineSummary?: HeapTimelineSummary;
  profileSummaryPromise?: Promise<HeapProfileSummary>;
  snapshotSummaryPromise?: Promise<HeapSnapshotSummary>;
  timelineSummaryPromise?: Promise<HeapTimelineSummary>;
}

const profiles = new Map<string, ProfileData>();

async function loadProfile(filePath: string): Promise<ProfileData> {
  const cached = profiles.get(filePath);
  if (cached) return cached;

  const type = detectProfileType(filePath);
  const file = Bun.file(filePath);
  const data: ProfileData = {
    type,
    filePath,
    fileName: path.basename(filePath),
    fileSize: file.size,
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

async function staticResponse(clientDist: string, pathname: string): Promise<Response> {
  const relativePath = pathname === "/" ? "index.html" : pathname.replace(/^\//, "");
  const requestedPath = path.resolve(clientDist, relativePath);
  const indexPath = path.join(clientDist, "index.html");

  if (!requestedPath.startsWith(clientDist + path.sep) && requestedPath !== indexPath) {
    return new Response("Not found", { status: 404 });
  }

  let filePath = requestedPath;
  const requestedFile = Bun.file(filePath);
  if (!(await requestedFile.exists())) {
    filePath = indexPath;
  }

  const file = Bun.file(filePath);
  if (!(await file.exists())) {
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

  return new Response(file, {
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
      const page = Number(url.searchParams.get("page") ?? "0");
      const pageSize = Number(url.searchParams.get("pageSize") ?? "100");
      const nodeType = url.searchParams.get("type");
      const search = url.searchParams.get("q");
      const sort = (url.searchParams.get("sort") ?? "selfSize") as "id" | "type" | "name" | "selfSize" | "edgeCount";
      const sortDir = (url.searchParams.get("dir") ?? "desc") as "asc" | "desc";

      return jsonResponse(await data.snapshot!.getNodePage({
        page,
        pageSize,
        type: nodeType,
        q: search,
        sort,
        dir: sortDir,
      }));
    }

    case "edges": {
      if (data.type !== "heapsnapshot") {
        return errorResponse("Edges only available for heapsnapshot");
      }
      const nodeId = Number(url.searchParams.get("nodeId"));
      if (!Number.isFinite(nodeId) || nodeId < 0) return errorResponse("nodeId required");
      return jsonResponse(await data.snapshot!.getNodeEdges(nodeId));
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
      return jsonResponse({
        timeline: await data.timeline!.getTimelineEntries(),
      });
    }

    case "search": {
      const q = url.searchParams.get("q");
      if (!q) return errorResponse("q parameter required");

      if (data.type === "heapsnapshot" || data.type === "heaptimeline") {
        if (data.type === "heapsnapshot") {
          return jsonResponse({ matches: await data.snapshot!.searchStrings(q) });
        }

        return jsonResponse({ matches: await data.timeline!.searchStrings(q) });
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
      const topN = Number(url.searchParams.get("top") ?? "30");
      return jsonResponse(await data.snapshot!.getRetainedEntries(topN));
    }

    default:
      return errorResponse(`Unknown action: ${action}`, 404);
  }
}

export async function startServer(options: ServerOptions): Promise<void> {
  for (const file of options.files) {
    const resolved = path.resolve(file);
    if (!(await Bun.file(resolved).exists())) {
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
  const server = Bun.serve({
    port: options.port,
    async fetch(req) {
      try {
        const url = new URL(req.url);
        return url.pathname.startsWith("/api/")
          ? await handleApiRequest(url)
          : await staticResponse(clientDist, url.pathname);
      } catch (error) {
        return errorResponse((error as Error).message, 500);
      }
    }
  });
  const port = server.port;

  console.log(`\n  hprof UI server running at http://localhost:${port}`);
  console.log(`\n  Profiles:`);
  for (const data of profiles.values()) {
    console.log(`    ${data.fileName} (${data.type}, ${formatBytes(data.fileSize)})`);
  }
  console.log();

  if (options.open) {
    const targetUrl = `http://localhost:${port}`;
    const opener = process.platform === "darwin"
      ? ["open", targetUrl]
      : process.platform === "win32"
        ? ["cmd", "/c", "start", "", targetUrl]
        : ["xdg-open", targetUrl];
    Bun.spawn(opener, { stdout: "ignore", stderr: "ignore" });
  }
}
