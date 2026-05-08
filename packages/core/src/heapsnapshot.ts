import fs from "fs";

export interface HeapSnapshotMeta {
  node_count: number;
  edge_count: number;
  extra_native_bytes?: number;
  meta: {
    node_fields: string[];
    node_types: string[][];
    edge_fields: string[];
    edge_types: string[][];
  };
}

export interface HeapSnapshotNode {
  type: string;
  name: string;
  selfSize: number;
  retentionSize?: number;
  id: number;
  edgeCount: number;
}

export interface HeapSnapshotEdge {
  type: string;
  nameOrIndex: string | number;
  toNode: number;
}

export interface HeapSnapshotResult {
  meta: HeapSnapshotMeta;
  nodes: HeapSnapshotNode[];
  edges: HeapSnapshotEdge[];
  strings: string[];
}

export interface HeapSnapshotSummary {
  totalSize: number;
  totalCount: number;
  byNodeType: Map<string, { size: number; count: number }>;
  byNodeName: Map<string, { size: number; count: number }>;
}

export function parseSnapshotMeta(filePath: string): HeapSnapshotMeta {
  const fd = fs.openSync(filePath, "r");
  try {
    let chunkSize = 2 * 1024 * 1024;
    const maxChunkSize = 64 * 1024 * 1024;

    while (chunkSize <= maxChunkSize) {
      const buffer = Buffer.alloc(chunkSize);
      const bytesRead = fs.readSync(fd, buffer, 0, chunkSize, 0);
      const prefix = buffer.subarray(0, bytesRead).toString("utf8");
      const snapshotMarker = '"snapshot":';
      const snapshotIndex = prefix.indexOf(snapshotMarker);
      const nodesIndex = prefix.indexOf('"nodes":[');
      if (snapshotIndex !== -1 && nodesIndex !== -1) {
        const objectStart = prefix.indexOf(
          "{",
          snapshotIndex + snapshotMarker.length,
        );
        if (objectStart !== -1 && objectStart < nodesIndex) {
          let depth = 0;
          let inString = false;
          let escaped = false;
          for (let i = objectStart; i < prefix.length; i++) {
            const ch = prefix[i]!;
            if (inString) {
              if (escaped) {
                escaped = false;
              } else if (ch === "\\") {
                escaped = true;
              } else if (ch === '"') {
                inString = false;
              }
              continue;
            }

            if (ch === '"') {
              inString = true;
              continue;
            }

            if (ch === "{") {
              depth += 1;
            } else if (ch === "}") {
              depth -= 1;
              if (depth === 0) {
                return JSON.parse(
                  prefix.slice(objectStart, i + 1),
                ) as HeapSnapshotMeta;
              }
            }
          }
        }
      }

      if (!prefix.includes('"nodes":[')) {
        chunkSize *= 2;
        continue;
      }

      throw new Error(
        "Cannot parse snapshot header even though nodes marker exists",
      );
    }

    throw new Error("Cannot parse snapshot header: prefix limit exceeded");
  } finally {
    fs.closeSync(fd);
  }
}

export async function streamHeapSnapshotSummary(
  filePath: string,
  options?: { top?: number; filter?: string },
): Promise<HeapSnapshotSummary> {
  const top = options?.top ?? 30;
  const filterRe = options?.filter ? new RegExp(options.filter, "i") : null;
  const snapshot = parseSnapshotMeta(filePath);

  const nodeFields = snapshot.meta.node_fields;
  const nodeTypes = snapshot.meta.node_types[0]!;
  const nodeFieldCount = nodeFields.length;
  const typeOffset = nodeFields.indexOf("type");
  const nameOffset = nodeFields.indexOf("name");
  const selfSizeOffset = nodeFields.indexOf("self_size");

  if (typeOffset < 0 || nameOffset < 0 || selfSizeOffset < 0) {
    throw new Error("Unsupported node field layout");
  }

  const byNameIndex = new Map<number, { size: number; count: number; typeIndex: number }>();
  const byTypeIndex = new Map<number, { size: number; count: number }>();
  let totalCount = 0;
  let totalSize = 0;

  let mode: "seekNodes" | "parseNodes" | "seekStrings" | "parseStrings" | "done" =
    "seekNodes";
  let record: number[] = [];
  let currentNumber = "";
  let currentString = "";
  let stringEscape = false;
  let stringIndex = -1;

  let topNameIndices: Set<number> | null = null;
  const topNames = new Map<number, string>();

  const stream = fs.createReadStream(filePath, { encoding: "utf8" });

  for await (const chunk of stream) {
    let i = 0;
    while (i < chunk.length) {
      if (mode === "seekNodes") {
        const idx = chunk.indexOf('"nodes":[', i);
        if (idx === -1) break;
        i = idx + '"nodes":['.length;
        mode = "parseNodes";
        continue;
      }

      if (mode === "parseNodes") {
        const ch = chunk[i]!;
        if (ch >= "0" && ch <= "9") {
          currentNumber += ch;
        } else if (ch === "-") {
          currentNumber += ch;
        } else if (ch === "," || ch === "]") {
          if (currentNumber) {
            record.push(Number(currentNumber));
            currentNumber = "";
          }

          if (record.length === nodeFieldCount) {
            const typeIdx = record[typeOffset]!;
            const nameIdx = record[nameOffset]!;
            const selfSize = record[selfSizeOffset]!;
            totalCount += 1;
            totalSize += selfSize;

            if (selfSize > 0) {
              const prev = byNameIndex.get(nameIdx) ?? {
                size: 0,
                count: 0,
                typeIndex: typeIdx,
              };
              prev.size += selfSize;
              prev.count += 1;
              byNameIndex.set(nameIdx, prev);

              const typePrev = byTypeIndex.get(typeIdx) ?? {
                size: 0,
                count: 0,
              };
              typePrev.size += selfSize;
              typePrev.count += 1;
              byTypeIndex.set(typeIdx, typePrev);
            }

            record = [];
          }

          if (ch === "]") {
            mode = "seekStrings";
          }
        }

        i += 1;
        continue;
      }

      if (mode === "seekStrings") {
        const idx = chunk.indexOf('"strings":[', i);
        if (idx === -1) break;
        i = idx + '"strings":['.length;
        mode = "parseStrings";

        const indices = [...byNameIndex.entries()]
          .sort((a, b) => b[1].size - a[1].size)
          .slice(0, top * 5)
          .map(([index]) => index);
        topNameIndices = new Set(indices);
        continue;
      }

      if (mode === "parseStrings") {
        const ch = chunk[i]!;
        if (currentString === "" && ch === '"') {
          currentString = '"';
        } else if (currentString !== "") {
          currentString += ch;

          if (stringEscape) {
            stringEscape = false;
          } else if (ch === "\\") {
            stringEscape = true;
          } else if (ch === '"') {
            stringIndex += 1;
            if (topNameIndices?.has(stringIndex)) {
              topNames.set(stringIndex, JSON.parse(currentString));
            }
            currentString = "";
          }
        } else if (ch === "]") {
          mode = "done";
          break;
        }

        i += 1;
        continue;
      }

      if (mode === "done") break;
    }

    if (mode === "done") break;
  }

  const byNodeName = new Map<string, { size: number; count: number }>();
  for (const [nameIdx, info] of byNameIndex) {
    const name = topNames.get(nameIdx) ?? `<string#${nameIdx}>`;
    if (filterRe && !filterRe.test(`${name} ${nodeTypes[info.typeIndex] ?? info.typeIndex}`)) {
      continue;
    }
    const prev = byNodeName.get(name) ?? { size: 0, count: 0 };
    prev.size += info.size;
    prev.count += info.count;
    byNodeName.set(name, prev);
  }

  const sortedNames = [...byNodeName.entries()]
    .sort((a, b) => b[1].size - a[1].size)
    .slice(0, top);
  const byNodeType = new Map<string, { size: number; count: number }>();
  for (const [typeIdx, info] of byTypeIndex) {
    const typeName = nodeTypes[typeIdx] ?? String(typeIdx);
    byNodeType.set(typeName, info);
  }

  return {
    totalSize,
    totalCount,
    byNodeName: new Map(sortedNames),
    byNodeType,
  };
}

export async function parseHeapSnapshot(
  filePath: string,
): Promise<HeapSnapshotResult> {
  const raw = JSON.parse(fs.readFileSync(filePath, "utf8")) as {
    snapshot: HeapSnapshotMeta;
    nodes: number[];
    edges: number[];
    strings: string[];
  };

  const meta = raw.snapshot;
  const nodeFields = meta.meta.node_fields;
  const nodeTypes = meta.meta.node_types[0]!;
  const edgeFields = meta.meta.edge_fields;
  const edgeTypes = meta.meta.edge_types[0]!;
  const nodeFieldCount = nodeFields.length;
  const edgeFieldCount = edgeFields.length;

  const nodeTypeIdx = nodeFields.indexOf("type");
  const nodeNameIdx = nodeFields.indexOf("name");
  const nodeSelfSizeIdx = nodeFields.indexOf("self_size");
  const nodeIdIdx = nodeFields.indexOf("id");
  const nodeEdgeCountIdx = nodeFields.indexOf("edge_count");

  const edgeTypeIdx = edgeFields.indexOf("type");
  const edgeNameOrIndexIdx = edgeFields.indexOf("name_or_index");
  const edgeToNodeIdx = edgeFields.indexOf("to_node");

  const nodes: HeapSnapshotNode[] = [];
  for (let i = 0; i < raw.nodes.length; i += nodeFieldCount) {
    const slice = raw.nodes.slice(i, i + nodeFieldCount);
    nodes.push({
      type: nodeTypes[slice[nodeTypeIdx]!] ?? String(slice[nodeTypeIdx]),
      name: raw.strings[slice[nodeNameIdx]!] ?? `<string#${slice[nodeNameIdx]}>`,
      selfSize: slice[nodeSelfSizeIdx]!,
      id: slice[nodeIdIdx]!,
      edgeCount: slice[nodeEdgeCountIdx]!,
    });
  }

  const edges: HeapSnapshotEdge[] = [];
  for (let i = 0; i < raw.edges.length; i += edgeFieldCount) {
    const slice = raw.edges.slice(i, i + edgeFieldCount);
    const nameOrIndexVal = slice[edgeNameOrIndexIdx]!;
    const edgeType = edgeTypes[slice[edgeTypeIdx]!] ?? String(slice[edgeTypeIdx]);
    edges.push({
      type: edgeType,
      nameOrIndex: edgeType === "element" || typeof nameOrIndexVal === "number"
        ? nameOrIndexVal
        : raw.strings[nameOrIndexVal] ?? String(nameOrIndexVal),
      toNode: Math.floor(slice[edgeToNodeIdx]! / nodeFieldCount),
    });
  }

  return { meta, nodes, edges, strings: raw.strings };
}
