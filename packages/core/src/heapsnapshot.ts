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

export interface HeapSnapshotNodePageOptions {
  page?: number;
  pageSize?: number;
  type?: string | null;
  q?: string | null;
  sort?: "id" | "type" | "name" | "selfSize" | "edgeCount";
  dir?: "asc" | "desc";
}

export interface HeapSnapshotNodePage {
  total: number;
  page: number;
  pageSize: number;
  nodes: HeapSnapshotNode[];
}

export interface HeapSnapshotSearchMatch {
  index: number;
  value: string;
}

export interface HeapSnapshotRetainedEntry {
  nodeIndex: number;
  name: string;
  type: string;
  selfSize: number;
  retainedSize: number;
  approximate: boolean;
}

interface HeapSnapshotRawData {
  meta: HeapSnapshotMeta;
  nodes: number[];
  edges: number[];
  strings: string[];
  nodeFields: string[];
  nodeTypes: string[];
  edgeFields: string[];
  edgeTypes: string[];
  nodeFieldCount: number;
  edgeFieldCount: number;
  nodeOffsets: {
    type: number;
    name: number;
    selfSize: number;
    id: number;
    edgeCount: number;
  };
  edgeOffsets: {
    type: number;
    nameOrIndex: number;
    toNode: number;
  };
}

export class HeapSnapshot {
  readonly filePath: string;
  private _meta: HeapSnapshotMeta | null = null;
  private _data: HeapSnapshotResult | null = null;
  private _retainedSizes: number[] | null = null;
  private _rawData: HeapSnapshotRawData | null = null;
  private _rawDataPromise: Promise<HeapSnapshotRawData> | null = null;
  private _edgeStarts: Uint32Array | null = null;

  constructor(filePath: string) {
    this.filePath = filePath;
  }

  get meta(): HeapSnapshotMeta {
    if (!this._meta) {
      this._meta = this.parseMeta();
    }
    return this._meta;
  }

  private parseMeta(): HeapSnapshotMeta {
    const fd = fs.openSync(this.filePath, "r");
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

  async streamSummary(
    options?: { top?: number; filter?: string; onProgress?: (phase: string, pct: number) => void },
  ): Promise<HeapSnapshotSummary> {
    const top = options?.top ?? 30;
    const filterRe = options?.filter ? new RegExp(options.filter, "i") : null;
    const onProgress = options?.onProgress;
    const snapshot = this.meta;
    const fileSize = fs.statSync(this.filePath).size;

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

    const stream = fs.createReadStream(this.filePath, { encoding: "utf8" });
    let bytesRead = 0;
    let lastProgressPct = -1;

    for await (const chunk of stream) {
      bytesRead += chunk.length;
      const pct = Math.floor((bytesRead / fileSize) * 100);
      if (onProgress && pct !== lastProgressPct) {
        lastProgressPct = pct;
        const phase = mode === "seekNodes" || mode === "parseNodes" ? "nodes" : mode === "seekStrings" || mode === "parseStrings" ? "strings" : "done";
        onProgress(phase, pct);
      }
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

  async rawData(): Promise<HeapSnapshotRawData> {
    if (this._rawData) return this._rawData;
    if (!this._rawDataPromise) {
      this._rawDataPromise = Bun.file(this.filePath).json().then((raw) => {
        const data = raw as {
          snapshot: HeapSnapshotMeta;
          nodes: number[];
          edges: number[];
          strings: string[];
        };

        const meta = data.snapshot;
        const nodeFields = meta.meta.node_fields;
        const nodeTypes = meta.meta.node_types[0]!;
        const edgeFields = meta.meta.edge_fields;
        const edgeTypes = meta.meta.edge_types[0]!;

        const parsed: HeapSnapshotRawData = {
          meta,
          nodes: data.nodes,
          edges: data.edges,
          strings: data.strings,
          nodeFields,
          nodeTypes,
          edgeFields,
          edgeTypes,
          nodeFieldCount: nodeFields.length,
          edgeFieldCount: edgeFields.length,
          nodeOffsets: {
            type: nodeFields.indexOf("type"),
            name: nodeFields.indexOf("name"),
            selfSize: nodeFields.indexOf("self_size"),
            id: nodeFields.indexOf("id"),
            edgeCount: nodeFields.indexOf("edge_count"),
          },
          edgeOffsets: {
            type: edgeFields.indexOf("type"),
            nameOrIndex: edgeFields.indexOf("name_or_index"),
            toNode: edgeFields.indexOf("to_node"),
          },
        };

        this._rawData = parsed;
        return parsed;
      }).finally(() => {
        this._rawDataPromise = null;
      });
    }
    return this._rawDataPromise;
  }

  private createNodeFromRaw(raw: HeapSnapshotRawData, nodeIndex: number): HeapSnapshotNode {
    const base = nodeIndex * raw.nodeFieldCount;
    return {
      type: raw.nodeTypes[raw.nodes[base + raw.nodeOffsets.type]!] ?? String(raw.nodes[base + raw.nodeOffsets.type]),
      name: raw.strings[raw.nodes[base + raw.nodeOffsets.name]!] ?? `<string#${raw.nodes[base + raw.nodeOffsets.name]}>`,
      selfSize: raw.nodes[base + raw.nodeOffsets.selfSize]!,
      id: raw.nodes[base + raw.nodeOffsets.id]!,
      edgeCount: raw.nodes[base + raw.nodeOffsets.edgeCount]!,
    };
  }

  private compareNodes(
    a: { nodeIndex: number; node: HeapSnapshotNode },
    b: { nodeIndex: number; node: HeapSnapshotNode },
    sort: NonNullable<HeapSnapshotNodePageOptions["sort"]>,
    dir: NonNullable<HeapSnapshotNodePageOptions["dir"]>,
  ): number {
    let cmp = 0;
    switch (sort) {
      case "id":
        cmp = a.node.id - b.node.id;
        break;
      case "type":
        cmp = a.node.type.localeCompare(b.node.type);
        break;
      case "name":
        cmp = a.node.name.localeCompare(b.node.name);
        break;
      case "edgeCount":
        cmp = a.node.edgeCount - b.node.edgeCount;
        break;
      case "selfSize":
      default:
        cmp = a.node.selfSize - b.node.selfSize;
        break;
    }

    if (cmp === 0) {
      cmp = a.nodeIndex - b.nodeIndex;
    }

    return dir === "desc" ? -cmp : cmp;
  }

  async getNodePage(options?: HeapSnapshotNodePageOptions): Promise<HeapSnapshotNodePage> {
    const raw = await this.rawData();
    const page = Math.max(0, options?.page ?? 0);
    const pageSize = Math.max(1, options?.pageSize ?? 100);
    const sort = options?.sort ?? "selfSize";
    const dir = options?.dir ?? "desc";
    const filterType = options?.type ?? null;
    const filterRe = options?.q ? new RegExp(options.q, "i") : null;
    const wanted = (page + 1) * pageSize;
    const selected: { nodeIndex: number; node: HeapSnapshotNode }[] = [];
    let total = 0;

    const bubbleWorstToFront = () => {
      let worstIndex = 0;
      for (let i = 1; i < selected.length; i++) {
        if (this.compareNodes(selected[i]!, selected[worstIndex]!, sort, dir) > 0) {
          worstIndex = i;
        }
      }
      if (worstIndex !== 0) {
        const worst = selected[worstIndex]!;
        selected[worstIndex] = selected[0]!;
        selected[0] = worst;
      }
    };

    for (let nodeIndex = 0; nodeIndex < raw.meta.node_count; nodeIndex++) {
      const node = this.createNodeFromRaw(raw, nodeIndex);
      if (filterType && node.type !== filterType) continue;
      if (filterRe && !filterRe.test(node.name)) continue;
      total += 1;

      const candidate = { nodeIndex, node };
      if (selected.length < wanted) {
        selected.push(candidate);
        if (selected.length === wanted) bubbleWorstToFront();
        continue;
      }

      if (this.compareNodes(candidate, selected[0]!, sort, dir) < 0) {
        selected[0] = candidate;
        bubbleWorstToFront();
      }
    }

    selected.sort((a, b) => this.compareNodes(a, b, sort, dir));

    return {
      total,
      page,
      pageSize,
      nodes: selected.slice(page * pageSize, page * pageSize + pageSize).map((entry) => entry.node),
    };
  }

  async getNodeEdges(nodeIndex: number): Promise<{ node: HeapSnapshotNode; edges: HeapSnapshotEdge[] }> {
    const raw = await this.rawData();
    if (nodeIndex < 0 || nodeIndex >= raw.meta.node_count) {
      throw new Error("Node not found");
    }

    if (!this._edgeStarts) {
      const edgeStarts = new Uint32Array(raw.meta.node_count + 1);
      let edgeOffset = 0;
      for (let i = 0; i < raw.meta.node_count; i++) {
        edgeStarts[i] = edgeOffset;
        const base = i * raw.nodeFieldCount;
        edgeOffset += raw.nodes[base + raw.nodeOffsets.edgeCount]!;
      }
      edgeStarts[raw.meta.node_count] = edgeOffset;
      this._edgeStarts = edgeStarts;
    }

    const node = this.createNodeFromRaw(raw, nodeIndex);
    const edgeStart = this._edgeStarts[nodeIndex]!;
    const edgeEnd = this._edgeStarts[nodeIndex + 1]!;
    const edges: HeapSnapshotEdge[] = [];

    for (let edgeIndex = edgeStart; edgeIndex < edgeEnd; edgeIndex++) {
      const base = edgeIndex * raw.edgeFieldCount;
      const edgeType = raw.edgeTypes[raw.edges[base + raw.edgeOffsets.type]!] ?? String(raw.edges[base + raw.edgeOffsets.type]);
      const nameOrIndexValue = raw.edges[base + raw.edgeOffsets.nameOrIndex]!;
      edges.push({
        type: edgeType,
        nameOrIndex: edgeType === "element" || typeof nameOrIndexValue === "number"
          ? nameOrIndexValue
          : raw.strings[nameOrIndexValue] ?? String(nameOrIndexValue),
        toNode: Math.floor(raw.edges[base + raw.edgeOffsets.toNode]! / raw.nodeFieldCount),
      });
    }

    return { node, edges };
  }

  async searchStrings(query: string): Promise<HeapSnapshotSearchMatch[]> {
    const raw = await this.rawData();
    const re = new RegExp(query, "i");
    const matches: HeapSnapshotSearchMatch[] = [];
    for (let index = 0; index < raw.strings.length; index++) {
      const value = raw.strings[index]!;
      if (!re.test(value)) continue;
      matches.push({ index, value });
      if (matches.length >= 100) break;
    }
    return matches;
  }

  async getRetainedEntries(topN = 30): Promise<{ approximate: boolean; retained: HeapSnapshotRetainedEntry[] }> {
    const raw = await this.rawData();

    if (raw.meta.node_count > 5_000_000) {
      const selected: { nodeIndex: number; node: HeapSnapshotNode }[] = [];
      const bubbleWorstToFront = () => {
        let worstIndex = 0;
        for (let i = 1; i < selected.length; i++) {
          if (selected[i]!.node.selfSize < selected[worstIndex]!.node.selfSize) {
            worstIndex = i;
          }
        }
        if (worstIndex !== 0) {
          const worst = selected[worstIndex]!;
          selected[worstIndex] = selected[0]!;
          selected[0] = worst;
        }
      };

      for (let nodeIndex = 0; nodeIndex < raw.meta.node_count; nodeIndex++) {
        const node = this.createNodeFromRaw(raw, nodeIndex);
        if (selected.length < topN) {
          selected.push({ nodeIndex, node });
          if (selected.length === topN) bubbleWorstToFront();
          continue;
        }

        if (node.selfSize > selected[0]!.node.selfSize) {
          selected[0] = { nodeIndex, node };
          bubbleWorstToFront();
        }
      }

      selected.sort((a, b) => b.node.selfSize - a.node.selfSize);
      return {
        approximate: true,
        retained: selected.map(({ nodeIndex, node }) => ({
          nodeIndex,
          name: node.name,
          type: node.type,
          selfSize: node.selfSize,
          retainedSize: node.selfSize,
          approximate: true,
        })),
      };
    }

    const retainedSizes = this.retainedSizes;
    const indexed = retainedSizes
      .map((size, idx) => ({ idx, size }))
      .sort((a, b) => b.size - a.size)
      .slice(0, topN);

    return {
      approximate: false,
      retained: indexed.map(({ idx, size }) => {
        const node = this.createNodeFromRaw(raw, idx);
        return {
          nodeIndex: idx,
          name: node.name,
          type: node.type,
          selfSize: node.selfSize,
          retainedSize: size,
          approximate: false,
        };
      }),
    };
  }

  get data(): HeapSnapshotResult {
    if (!this._data) {
      this._data = this.parseFull();
    }
    return this._data;
  }

  private parseFull(): HeapSnapshotResult {
    const raw = JSON.parse(fs.readFileSync(this.filePath, "utf8")) as {
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

  get retainedSizes(): number[] {
    if (!this._retainedSizes) {
      this._retainedSizes = this.buildRetainedSize();
    }
    return this._retainedSizes;
  }

  private buildRetainedSize(): number[] {
    const { nodes, edges } = this.data;
    const nodeCount = nodes.length;

    const postOrder = new Int32Array(nodeCount);
    const visited = new Uint8Array(nodeCount);
    let postIdx = 0;

    const stack: number[] = [0];
    while (stack.length > 0) {
      const nodeIdx = stack.pop()!;
      if (nodeIdx < 0) {
        postOrder[postIdx++] = ~nodeIdx;
        continue;
      }
      if (visited[nodeIdx]) continue;
      visited[nodeIdx] = 1;
      stack.push(~nodeIdx);
      let edgeOffset = 0;
      for (let i = 0; i < nodeIdx; i++) {
        edgeOffset += nodes[i]!.edgeCount;
      }
      for (let e = 0; e < nodes[nodeIdx]!.edgeCount; e++) {
        const toNode = edges[edgeOffset + e]!.toNode;
        if (toNode >= 0 && toNode < nodeCount && !visited[toNode]) {
          stack.push(toNode);
        }
      }
    }

    const idoms = new Int32Array(nodeCount).fill(-1);
    idoms[0] = 0;

    const succs: number[][] = Array.from({ length: nodeCount }, () => []);
    for (let n = 0; n < nodeCount; n++) {
      let edgeOffset = 0;
      for (let i = 0; i < n; i++) {
        edgeOffset += nodes[i]!.edgeCount;
      }
      for (let e = 0; e < nodes[n]!.edgeCount; e++) {
        const toNode = edges[edgeOffset + e]!.toNode;
        if (toNode >= 0 && toNode < nodeCount) {
          succs[n]!.push(toNode);
        }
      }
    }

    const preds: number[][] = Array.from({ length: nodeCount }, () => []);
    for (let n = 0; n < nodeCount; n++) {
      for (const s of succs[n]!) {
        preds[s]!.push(n);
      }
    }

    function intersect(a: number, b: number): number {
      let finger1 = a;
      let finger2 = b;
      while (finger1 !== finger2) {
        while (finger1 > finger2) finger1 = idoms[finger1]!;
        while (finger2 > finger1) finger2 = idoms[finger2]!;
      }
      return finger1;
    }

    let changed = true;
    while (changed) {
      changed = false;
      for (let i = postIdx - 1; i >= 0; i--) {
        const n = postOrder[i]!;
        if (n === 0) continue;
        const predList = preds[n]!;
        if (!predList || predList.length === 0) continue;

        let newIdom = -1;
        for (const p of predList) {
          if (idoms[p] === -1) continue;
          if (newIdom === -1) {
            newIdom = p;
          } else {
            newIdom = intersect(newIdom, p);
          }
        }
        if (newIdom !== -1 && idoms[n] !== newIdom) {
          idoms[n] = newIdom;
          changed = true;
        }
      }
    }

    const retained = new Float64Array(nodeCount);
    for (let i = 0; i < nodeCount; i++) {
      retained[i] = nodes[i]!.selfSize;
    }

    for (let i = 0; i < postIdx; i++) {
      const n = postOrder[i]!;
      if (n === 0) continue;
      const dom = idoms[n]!;
      if (dom >= 0 && dom < nodeCount) {
        retained[dom]! += retained[n]!;
      }
    }

    return Array.from(retained);
  }
}
