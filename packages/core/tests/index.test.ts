import { describe, test, expect } from "bun:test";
import {
  detectProfileType,
  formatBytes,
  parseHeapProfile,
  summarizeHeapProfile,
  flattenToCallFrames,
} from "../src/index.ts";
import path from "path";

const SNAPSHOTS = path.resolve(import.meta.dir, "../../../snapshots");

describe("detectProfileType", () => {
  test("detects heapsnapshot", () => {
    expect(detectProfileType("file.heapsnapshot")).toBe("heapsnapshot");
  });

  test("detects heapprofile", () => {
    expect(detectProfileType("file.heapprofile")).toBe("heapprofile");
  });

  test("detects heaptimeline", () => {
    expect(detectProfileType("file.heaptimeline")).toBe("heaptimeline");
  });

  test("throws for unknown extension", () => {
    expect(() => detectProfileType("file.json")).toThrow("Unsupported file type");
  });
});

describe("formatBytes", () => {
  test("formats bytes", () => {
    expect(formatBytes(0)).toBe("0.00 B");
  });

  test("formats KB", () => {
    expect(formatBytes(1024)).toBe("1.00 KB");
  });

  test("formats MB", () => {
    expect(formatBytes(1048576)).toBe("1.00 MB");
  });

  test("formats GB", () => {
    expect(formatBytes(1073741824)).toBe("1.00 GB");
  });

  test("handles non-finite", () => {
    expect(formatBytes(Infinity)).toBe("Infinity");
    expect(formatBytes(NaN)).toBe("NaN");
  });
});

describe("heapprofile parser", () => {
  const filePath = path.join(SNAPSHOTS, "Heap-20260508T151711.heapprofile");
  let result: ReturnType<typeof parseHeapProfile>;

  test("parseHeapProfile parses the file", () => {
    result = parseHeapProfile(filePath);
    expect(result.head).toBeDefined();
    expect(result.head.callFrame).toBeDefined();
    expect(result.head.children).toBeInstanceOf(Array);
    expect(typeof result.startTime === "number" || result.startTime === undefined).toBe(true);
  });

  test("summarizeHeapProfile aggregates by frame/url/function", () => {
    const summary = summarizeHeapProfile(result);
    expect(summary.totalSize).toBeGreaterThan(0);
    expect(summary.byFrame.size).toBeGreaterThan(0);
    expect(summary.byUrl.size).toBeGreaterThan(0);
    expect(summary.byFunction.size).toBeGreaterThan(0);
  });

  test("summarizeHeapProfile respects --top", () => {
    const summary = summarizeHeapProfile(result, { top: 5 });
    expect(summary.byFrame.size).toBeLessThanOrEqual(5);
    expect(summary.byUrl.size).toBeLessThanOrEqual(5);
    expect(summary.byFunction.size).toBeLessThanOrEqual(5);
  });

  test("summarizeHeapProfile respects --filter", () => {
    const summaryAll = summarizeHeapProfile(result);
    const summaryFiltered = summarizeHeapProfile(result, { filter: "xyznonexistent" });
    expect(summaryFiltered.totalSize).toBe(0);
    expect(summaryFiltered.byFrame.size).toBe(0);
  });

  test("flattenToCallFrames returns flat array", () => {
    const flat = flattenToCallFrames(result);
    expect(flat.length).toBeGreaterThan(0);
    expect(flat[0]).toHaveProperty("functionName");
    expect(flat[0]).toHaveProperty("selfSize");
    expect(flat[0]).toHaveProperty("stack");
    expect(flat[0]!.stack.length).toBeGreaterThan(0);
  });
});
