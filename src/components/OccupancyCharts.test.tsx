import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { api } from "../api";
import type { ChartMode, FullTreemapData, TreemapHit } from "../types";
import { OccupancyCharts } from "./OccupancyCharts";

vi.mock("../api", () => ({ api: {
  getFullTreemap: vi.fn(), releaseTreemap: vi.fn(),
  hitTestTreemap: vi.fn(), getTreemapBounds: vi.fn(),
} }));
class Bitmap {
  static instances: Bitmap[] = [];
  complete = false;
  naturalWidth = 0;
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  src = "";
  constructor() { Bitmap.instances.push(this); }
  load() { this.complete = true; this.naturalWidth = 4096; this.onload?.(); }
}
let resizeChart: () => void;
beforeEach(() => {
  Bitmap.instances = [];
  vi.stubGlobal("Image", Bitmap);
  vi.stubGlobal("ResizeObserver", class { constructor(callback: () => void) { resizeChart = callback; } observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, 800, 400));
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
    scale: vi.fn(), clearRect: vi.fn(), drawImage: vi.fn(), strokeRect: vi.fn(),
  } as unknown as CanvasRenderingContext2D);
  vi.mocked(api.releaseTreemap).mockResolvedValue();
  vi.mocked(api.hitTestTreemap).mockResolvedValue(null);
  vi.mocked(api.getTreemapBounds).mockResolvedValue(null);
});
afterEach(async () => {
  cleanup();
  await flush();
  vi.restoreAllMocks();
  vi.resetAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => { resolve = r; });
  return { promise, resolve };
}
function frame(layoutId: string, fileCount: number, comparisonId = "one"): FullTreemapData {
  return {
    layoutId, comparisonId,
    imageDataUrl: "data:image/png;base64,iVBORw0KGgo=",
    atlasWidth: 4096, atlasHeight: 1024, fileCount, visibleFileCount: fileCount,
    renderedBlockCount: fileCount, maxDepth: 0, addedFileCount: 0,
    weightTotal: "9007199254740993", positiveTotal: "0", negativeTotal: "0",
    netDelta: "0", exportedTotal: "9007199254740993", labels: [], warnings: [],
  };
}
function chart(comparisonId: string, mode: ChartMode, selectedId: string | null = null, onSelect = (_id: string) => {}, active = true) {
  return <OccupancyCharts comparisonId={comparisonId} metric="size" mode={mode}
    active={active} selectedId={selectedId} onSelect={onSelect} />;
}
async function flush() { await act(async () => {}); }
async function load() { await act(async () => Bitmap.instances.at(-1)!.load()); }
function canvas() { return screen.getByLabelText("全部文件层级占用图"); }

it("defers first load and reuses layout, image and canvas pixels across visibility toggles", async () => {
  vi.mocked(api.getFullTreemap).mockResolvedValue(frame("first", 10));
  const view = render(chart("one", "after", null, undefined, false));
  await flush();
  expect(api.getFullTreemap).not.toHaveBeenCalled();
  view.rerender(chart("one", "after"));
  await flush(); await load();
  const element = canvas();
  const context = (element as HTMLCanvasElement).getContext("2d")!;
  const draws = vi.mocked(context.drawImage).mock.calls.length;
  for (let i = 0; i < 3; i++) {
    view.rerender(chart("one", "after", null, undefined, false));
    await flush();
    act(() => resizeChart());
    expect(api.releaseTreemap).not.toHaveBeenCalled();
    view.rerender(chart("one", "after"));
    await flush();
    act(() => resizeChart());
    expect(canvas()).toBe(element);
  }
  expect(api.getFullTreemap).toHaveBeenCalledTimes(1);
  expect(Bitmap.instances).toHaveLength(1);
  expect(context.drawImage).toHaveBeenCalledTimes(draws);
  expect(canvas().getAttribute("data-layout-id")).toBe("first");
  vi.mocked(HTMLCanvasElement.prototype.getBoundingClientRect).mockReturnValue(new DOMRect(0, 0, 900, 400));
  act(() => resizeChart());
  expect(context.drawImage).toHaveBeenCalledTimes(draws + 1);
  view.unmount();
  expect(api.releaseTreemap).toHaveBeenCalledWith("one", "first");
});

it("finishes a hidden in-flight view and draws it on reopen without requesting twice", async () => {
  const late = deferred<FullTreemapData>();
  vi.mocked(api.getFullTreemap).mockReturnValue(late.promise);
  const view = render(chart("one", "after"));
  await flush();
  view.rerender(chart("one", "after", null, undefined, false));
  await act(async () => late.resolve(frame("late", 99)));
  await load();
  expect(api.releaseTreemap).not.toHaveBeenCalled();
  expect(canvas().getAttribute("data-layout-id")).toBeNull();
  view.rerender(chart("one", "after"));
  await flush();
  act(() => resizeChart());
  expect(api.getFullTreemap).toHaveBeenCalledTimes(1);
  expect(canvas().getAttribute("data-layout-id")).toBe("late");
});

it("invalidates retained hidden views when the comparison or view changes", async () => {
  vi.mocked(api.getFullTreemap).mockResolvedValueOnce(frame("old", 10)).mockResolvedValueOnce(frame("new", 20, "two"));
  const view = render(chart("one", "after"));
  await flush(); await load();
  view.rerender(chart("one", "after", null, undefined, false));
  await flush();
  view.rerender(chart("two", "before", null, undefined, false));
  await flush();
  expect(api.releaseTreemap).toHaveBeenCalledWith("one", "old");
  expect(api.getFullTreemap).toHaveBeenCalledTimes(1);
  view.rerender(chart("two", "before"));
  await flush(); await load();
  expect(api.getFullTreemap).toHaveBeenLastCalledWith("two", "size", "before", 0);
  expect(canvas().getAttribute("data-layout-id")).toBe("new");
});

it("requests only the current view, discards late frames, and recomputes revisited modes", async () => {
  const before = deferred<FullTreemapData>(), after = deferred<FullTreemapData>();
  vi.mocked(api.getFullTreemap).mockReturnValueOnce(before.promise)
    .mockReturnValueOnce(after.promise).mockImplementation(() => new Promise(() => {}));
  const view = render(chart("one", "before"));
  await flush();
  expect(api.getFullTreemap).toHaveBeenCalledTimes(1);
  view.rerender(chart("one", "after"));
  await flush();
  await act(async () => after.resolve(frame("after", 222)));
  await load();
  await act(async () => before.resolve(frame("late-before", 111)));
  expect(screen.getByText("222 文件")).toBeTruthy();
  expect(screen.queryByText("111 文件")).toBeNull();
  expect(api.releaseTreemap).toHaveBeenCalledWith("one", "late-before");
  view.rerender(chart("one", "before"));
  await flush();
  expect(screen.queryByText("111 文件")).toBeNull();
  expect(api.getFullTreemap).toHaveBeenCalledTimes(3);
});

it("replaces and releases the previous handle only after drawing, with no interaction during loading", async () => {
  const next = deferred<FullTreemapData>();
  vi.mocked(api.getFullTreemap).mockResolvedValueOnce(frame("old", 111)).mockReturnValueOnce(next.promise);
  const view = render(chart("one", "before", "node"));
  await flush();
  fireEvent.click(canvas());
  expect(api.hitTestTreemap).not.toHaveBeenCalled();
  expect(api.getTreemapBounds).not.toHaveBeenCalled();
  await load();
  expect(canvas().getAttribute("data-layout-id")).toBe("old");
  view.rerender(chart("one", "after", "node"));
  await flush();
  expect(screen.queryByLabelText("全部文件层级占用图")).toBeNull();
  await act(async () => next.resolve(frame("new", 222)));
  fireEvent.click(canvas());
  expect(api.hitTestTreemap).not.toHaveBeenCalled();
  expect(api.releaseTreemap).not.toHaveBeenCalledWith("one", "old");
  expect(canvas().getAttribute("data-layout-id")).toBeNull();
  await load();
  expect(api.releaseTreemap).toHaveBeenCalledWith("one", "old");
  expect(canvas().getAttribute("data-layout-id")).toBe("new");
  fireEvent.click(canvas(), { clientX: 400, clientY: 200 });
  expect(api.hitTestTreemap).toHaveBeenCalledWith("one", "new", 0.5, 0.5);
  expect(api.getTreemapBounds).toHaveBeenLastCalledWith("one", "new", "node");
  view.unmount();
  expect(api.releaseTreemap).toHaveBeenCalledWith("one", "new");
});

it("rejects stale click, hover, and selection bounds responses when the displayed layout changes", async () => {
  const hit = deferred<TreemapHit | null>(), bounds = deferred<{ x: number; y: number; width: number; height: number } | null>();
  const onSelect = vi.fn();
  vi.mocked(api.getFullTreemap).mockResolvedValueOnce(frame("old", 111)).mockResolvedValueOnce(frame("new", 222));
  vi.mocked(api.hitTestTreemap).mockReturnValue(hit.promise);
  vi.mocked(api.getTreemapBounds).mockReturnValueOnce(bounds.promise);
  const view = render(chart("one", "before", "node", onSelect));
  await flush(); await load();
  vi.useFakeTimers();
  fireEvent.mouseMove(canvas(), { clientX: 200, clientY: 100 });
  await act(async () => vi.advanceTimersByTime(65));
  fireEvent.click(canvas(), { clientX: 200, clientY: 100 });
  view.rerender(chart("one", "after", "node", onSelect));
  await flush(); await load();
  await act(async () => {
    hit.resolve({ nodeId: "old-node", name: "old", path: "C:\\old", extension: ".txt", weight: "1", value: "1",
      status: "unchanged", hasChanges: false, kind: "file", collapsed: false, rect: { x: 0, y: 0, width: 1, height: 1 } });
    bounds.resolve({ x: 0, y: 0, width: 1, height: 1 });
  });
  expect(onSelect).not.toHaveBeenCalled();
  expect(screen.queryByText("C:\\old")).toBeNull();
});

it("never adopts an old comparison response and releases pending results after unmount", async () => {
  const old = deferred<FullTreemapData>(), late = deferred<FullTreemapData>();
  vi.mocked(api.getFullTreemap).mockReturnValueOnce(old.promise).mockReturnValueOnce(late.promise);
  const view = render(chart("old", "before"));
  await flush();
  view.rerender(chart("new", "after"));
  await flush();
  await act(async () => old.resolve(frame("old-layout", 111, "old")));
  expect(screen.queryByText("111 文件")).toBeNull();
  expect(api.releaseTreemap).toHaveBeenCalledWith("old", "old-layout");
  view.unmount();
  await act(async () => late.resolve(frame("late", 222, "new")));
  expect(api.releaseTreemap).toHaveBeenCalledWith("new", "late");
});

it("releases a failed image and allows retry without reusing its handle", async () => {
  vi.mocked(api.getFullTreemap).mockResolvedValueOnce(frame("broken", 111)).mockResolvedValueOnce(frame("retry", 222));
  render(chart("one", "after"));
  await flush();
  act(() => Bitmap.instances.at(-1)!.onerror?.());
  expect(screen.getByText("占用图图像无法读取")).toBeTruthy();
  expect(api.releaseTreemap).toHaveBeenCalledWith("one", "broken");
  fireEvent.click(screen.getByText("重试"));
  await flush(); await load();
  expect(canvas().getAttribute("data-layout-id")).toBe("retry");
});

it("updates selection overlay without redrawing the bitmap and ignores late selection bounds", async () => {
  vi.mocked(api.getFullTreemap).mockResolvedValue(frame("layout", 10));
  const late = deferred<{ x: number; y: number; width: number; height: number } | null>();
  vi.mocked(api.getTreemapBounds)
    .mockResolvedValueOnce({ x: 0.1, y: 0.2, width: 0.3, height: 0.4 })
    .mockReturnValueOnce(late.promise)
    .mockResolvedValueOnce({ x: 0.5, y: 0.6, width: 0.2, height: 0.1 });
  const view = render(chart("one", "before", "first"));
  await flush(); await load();
  const context = (canvas() as HTMLCanvasElement).getContext("2d")!;
  const draws = vi.mocked(context.drawImage).mock.calls.length;
  const images = Bitmap.instances.length;
  expect((view.container.querySelector(".treemap-highlight") as HTMLElement).style.left).toBe("10%");
  view.rerender(chart("one", "before", "second"));
  await flush();
  expect(view.container.querySelector(".treemap-highlight")).toBeNull();
  view.rerender(chart("one", "before", "third"));
  await flush();
  await act(async () => late.resolve({ x: 0, y: 0, width: 1, height: 1 }));
  expect((view.container.querySelector(".treemap-highlight") as HTMLElement).style.left).toBe("50%");
  expect(Bitmap.instances.length).toBe(images);
  expect(context.drawImage).toHaveBeenCalledTimes(draws);
  view.rerender(chart("one", "before"));
  await flush();
  expect(view.container.querySelector(".treemap-highlight")).toBeNull();
});

it("positions the treemap tooltip inside bounds when hovering near edges and renders warnings in chart-warnings", async () => {
  const frameWithWarnings: FullTreemapData = {
    ...frame("tip", 5),
    warnings: ["测试范围提示"],
  };
  vi.mocked(api.getFullTreemap).mockResolvedValue(frameWithWarnings);
  vi.mocked(api.hitTestTreemap).mockResolvedValue({
    nodeId: "n5",
    name: "big.bin",
    path: "C:\\data\\big.bin",
    extension: ".bin",
    weight: "1024",
    value: "1024",
    status: "unchanged",
    hasChanges: false,
    kind: "file",
    collapsed: false,
    rect: { x: 0.8, y: 0.8, width: 0.2, height: 0.2 },
  });
  const view = render(chart("one", "after"));
  await flush();
  await load();
  const c = canvas() as HTMLCanvasElement;
  Object.defineProperty(c, "clientWidth", { configurable: true, value: 400 });
  Object.defineProperty(c, "clientHeight", { configurable: true, value: 200 });
  const offsetWidthSpy = vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(220);
  const offsetHeightSpy = vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(64);
  vi.useFakeTimers();
  fireEvent.mouseMove(c, { clientX: 390, clientY: 190 });
  await act(async () => vi.advanceTimersByTime(65));
  const tip = view.container.querySelector(".treemap-tooltip") as HTMLElement;
  expect(tip).toBeTruthy();
  expect(tip.textContent).toContain("C:\\data\\big.bin");
  expect(tip.textContent).toContain("1.00 KiB");
  expect(tip.style.left).toBe("158px");
  expect(tip.style.top).toBe("114px");
  const warnings = view.container.querySelector(".chart-warnings");
  expect(warnings).toBeTruthy();
  expect(warnings!.textContent).toContain("测试范围提示");
  offsetWidthSpy.mockRestore();
  offsetHeightSpy.mockRestore();
});

it("invalidates pending treemap clicks when changesOnly changes while hitTestTreemap is in flight", async () => {
  const pendingHit = deferred<TreemapHit | null>();
  const onSelect = vi.fn();
  vi.mocked(api.getFullTreemap).mockResolvedValue(frame("layout", 10));
  vi.mocked(api.hitTestTreemap).mockReturnValueOnce(pendingHit.promise);

  const view = render(
    <OccupancyCharts
      comparisonId="one"
      metric="size"
      mode="after"
      changesOnly={false}
      selectedId={null}
      onSelect={onSelect}
    />,
  );
  await flush();
  await load();

  fireEvent.click(canvas(), { clientX: 200, clientY: 100 });
  view.rerender(
    <OccupancyCharts
      comparisonId="one"
      metric="size"
      mode="after"
      changesOnly={true}
      selectedId={null}
      onSelect={onSelect}
    />,
  );
  await act(async () => {
    pendingHit.resolve({
      nodeId: "n1",
      name: "same.txt",
      path: "C:\\same.txt",
      extension: ".txt",
      weight: "10",
      value: "10",
      status: "unchanged",
      hasChanges: false,
      kind: "file",
      collapsed: false,
      rect: { x: 0, y: 0, width: 1, height: 1 },
    });
  });
  expect(onSelect).not.toHaveBeenCalled();
});
