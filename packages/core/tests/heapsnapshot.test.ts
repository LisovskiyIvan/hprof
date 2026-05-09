import { describe, test, expect } from "bun:test";
import {
  HeapSnapshot,
} from "../src/heapsnapshot.ts";
import path from "path";

const SNAPSHOTS = path.resolve(import.meta.dir, "../../../snapshots");const HEAP_SNAPSHOT = path.join(SNAPSHOTS, "Heap-20260508T151623.heapsnapshot");

describe("HeapSnapshot.meta", () => {
  test("extracts meta from heapsnapshot header", () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT);
    const meta = snapshot.meta;
    expect(meta.node_count).toBeGreaterThan(0);
    expect(meta.edge_count).toBeGreaterThan(0);
    expect(meta.meta.node_fields).toContain("type");
    expect(meta.meta.node_fields).toContain("name");
    expect(meta.meta.node_fields).toContain("self_size");
    expect(meta.meta.node_types).toBeInstanceOf(Array);
    expect(meta.meta.edge_fields).toBeInstanceOf(Array);
  });
});

describe("HeapSnapshot.streamSummary", () => {
  test("produces summary with top-N nodes", async () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT);
    const summary = await snapshot.streamSummary({ top: 10 });
    expect(summary.totalSize).toBeGreaterThan(0);
    expect(summary.totalCount).toBeGreaterThan(0);
    expect(summary.byNodeName.size).toBeGreaterThan(0);
    expect(summary.byNodeName.size).toBeLessThanOrEqual(10);
    expect(summary.byNodeType.size).toBeGreaterThan(0);
  }, 60000);

  test("respects filter", async () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT);
    const summaryFiltered = await snapshot.streamSummary({
      top: 100,
      filter: "xyznonexistent",
    });
    expect(summaryFiltered.byNodeName.size).toBe(0);
  }, 60000);

  test("calls onProgress callback", async () => {
    const snapshot = new HeapSnapshot(HEAP_SNAPSHOT);
    const phases: string[] = [];
    await snapshot.streamSummary({
      top: 5,
      onProgress: (phase) => {
        if (!phases.includes(phase)) phases.push(phase);
      },
    });
    expect(phases).toContain("nodes");
    expect(phases).toContain("strings");
  }, 60000);
});

describe("HeapSnapshot.retainedSizes", () => {
  test("computes retained sizes for a parsed snapshot", () => {
    const snapshot = new HeapSnapshot("");
    (snapshot as any)._data = {
      meta: {
        node_count: 3,
        edge_count: 2,
        meta: {
          node_fields: ["type", "name", "self_size", "id", "edge_count"],
          node_types: [["hidden", "object", "string"]],
          edge_fields: ["type", "name_or_index", "to_node"],
          edge_types: [["internal", "property"]],
        },
      },
      nodes: [
        { type: "hidden", name: "root", selfSize: 0, id: 1, edgeCount: 1 },
        { type: "object", name: "ObjA", selfSize: 100, id: 2, edgeCount: 1 },
        { type: "string", name: "str", selfSize: 50, id: 3, edgeCount: 0 },
      ],
      edges: [
        { type: "internal", nameOrIndex: "map", toNode: 1 },
        { type: "property", nameOrIndex: "val", toNode: 2 },
      ],
      strings: ["root", "ObjA", "str"],
    };

    const retained = snapshot.retainedSizes;
    expect(retained.length).toBe(3);
    expect(retained[0]).toBeGreaterThan(0);
    expect(retained[0]).toBe(150);
  });
});
