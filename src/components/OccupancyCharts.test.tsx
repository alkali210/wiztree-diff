import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { api } from "../api";
import type { ChartMode, FullTreemapData } from "../types";
import { OccupancyCharts } from "./OccupancyCharts";

vi.mock("../api", () => ({ api: { getFullTreemap: vi.fn() } }));
beforeEach(() => {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
});
afterEach(() => {
  cleanup();
  vi.resetAllMocks();
  vi.unstubAllGlobals();
});
function deferred() {
  let resolve!: (frame: FullTreemapData) => void;
  const promise = new Promise<FullTreemapData>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}
function frame(fileCount: number): FullTreemapData {
  return {
    imageDataUrl: "data:image/png;base64,iVBORw0KGgo=",
    atlasWidth: 4096,
    atlasHeight: 1024,
    fileCount,
    visibleFileCount: fileCount,
    renderedBlockCount: fileCount,
    maxDepth: 0,
    addedFileCount: 0,
    weightTotal: "9007199254740993",
    positiveTotal: "0",
    negativeTotal: "0",
    netDelta: "0",
    exportedTotal: "9007199254740993",
    labels: [],
    warnings: [],
  };
}
function chart(comparisonId: string, mode: ChartMode) {
  return (
    <OccupancyCharts
      comparisonId={comparisonId}
      metric="size"
      mode={mode}
      onMetricChange={() => {}}
      onModeChange={() => {}}
      selectedId={null}
      onSelect={() => {}}
    />
  );
}
it("keeps the active mode when an old frame arrives, then displays the completed cached mode", async () => {
  const before = deferred(),
    after = deferred();
  vi.mocked(api.getFullTreemap)
    .mockReturnValueOnce(before.promise)
    .mockReturnValueOnce(after.promise)
    .mockImplementation(() => new Promise(() => {}));
  const view = render(chart("one", "before"));
  view.rerender(chart("one", "after"));
  await act(async () => after.resolve(frame(222)));
  await act(async () => before.resolve(frame(111)));
  expect(screen.getByText("222 文件")).toBeTruthy();
  expect(screen.queryByText("111 文件")).toBeNull();
  view.rerender(chart("one", "before"));
  expect(screen.getByText("111 文件")).toBeTruthy();
  view.rerender(chart("one", "after"));
  expect(screen.getByText("222 文件")).toBeTruthy();
});
it("never reuses frames from a replaced comparison, even when its pending response finishes late", async () => {
  const oldAfter = deferred(),
    newBefore = deferred(),
    newAfter = deferred();
  vi.mocked(api.getFullTreemap)
    .mockResolvedValueOnce(frame(111))
    .mockReturnValueOnce(oldAfter.promise)
    .mockReturnValueOnce(newBefore.promise)
    .mockReturnValueOnce(newAfter.promise);
  const view = render(chart("old", "before"));
  await act(async () => {});
  expect(screen.getByText("111 文件")).toBeTruthy();
  view.rerender(chart("old", "after"));
  view.rerender(chart("new", "before"));
  expect(screen.queryByText("111 文件")).toBeNull();
  await act(async () => newBefore.resolve(frame(333)));
  await act(async () => oldAfter.resolve(frame(222)));
  expect(screen.getByText("333 文件")).toBeTruthy();
  view.rerender(chart("new", "after"));
  expect(screen.queryByText("222 文件")).toBeNull();
  await act(async () => newAfter.resolve(frame(444)));
  expect(screen.getByText("444 文件")).toBeTruthy();
});
