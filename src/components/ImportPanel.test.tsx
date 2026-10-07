import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PhysicalPosition } from "@tauri-apps/api/dpi";
import type { Event } from "@tauri-apps/api/event";
import type { DragDropEvent } from "@tauri-apps/api/webview";
import { ImportPanel } from "./ImportPanel";
import App from "../App";
import { api } from "../api";
import type { ComparisonSummary, JobSummary } from "../types";

const native = vi.hoisted(() => ({
  handler: null as ((event: Event<DragDropEvent>) => void) | null,
  progress: null as ((event: { payload: JobSummary }) => void) | null,
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: async (handler: typeof native.handler) => {
      native.handler = handler;
      return () => {};
    },
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (_event: string, handler: typeof native.progress) => {
    native.progress = handler;
    return () => { native.progress = null; };
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("../api", () => ({
  api: {
    startComparison: vi.fn(), getJob: vi.fn(), getComparison: vi.fn(), cancelComparison: vi.fn(),
  },
}));

beforeEach(() => {
  vi.mocked(api.startComparison).mockResolvedValue("pending-import");
  vi.mocked(api.getJob).mockImplementation(() => new Promise(() => {}));
  vi.mocked(api.cancelComparison).mockResolvedValue();
  vi.stubGlobal("devicePixelRatio", 2);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    function (this: HTMLElement) {
      const label = this.getAttribute("aria-label");
      if (label === "选择之前 CSV") return new DOMRect(100, 20, 300, 60);
      if (label === "选择之后 CSV") return new DOMRect(500, 20, 300, 60);
      return new DOMRect();
    },
  );
});
afterEach(() => {
  cleanup();
  native.handler = null;
  native.progress = null;
  vi.restoreAllMocks();
  vi.resetAllMocks();
  vi.unstubAllGlobals();
});
async function mount() {
  render(<ImportPanel onReady={() => {}} />);
  await waitFor(() => expect(native.handler).not.toBeNull());
}
function drop(paths: string[], x: number, y = 100) {
  act(() =>
    native.handler!({
      event: "tauri://drag-drop",
      id: 1,
      payload: { type: "drop", paths, position: new PhysicalPosition(x, y) },
    }),
  );
}
function selected(side: "之前" | "之后") {
  return screen
    .getByRole("button", { name: `选择${side} CSV` })
    .querySelector("span")!.title;
}

it("routes high-DPI drops to the correct side, ignores gaps, and preserves swap behavior", async () => {
  await mount();
  drop(["C:\\snapshots\\after.CSV"], 1200);
  expect(selected("之后")).toBe("C:\\snapshots\\after.CSV");
  expect(selected("之前")).toBe("");
  drop(["C:\\snapshots\\before.csv"], 400);
  drop(["C:\\outside.csv"], 850); // CSS x=425: between the two targets.
  drop(["C:\\edge.csv"], 1600); // Right boundary is outside the second target.
  expect(selected("之前")).toBe("C:\\snapshots\\before.csv");
  expect(selected("之后")).toBe("C:\\snapshots\\after.CSV");
  expect(
    screen.getByRole("button", { name: "开始比较" }).hasAttribute("disabled"),
  ).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "交换之前和之后" }));
  expect(selected("之前")).toBe("C:\\snapshots\\after.CSV");
  expect(selected("之后")).toBe("C:\\snapshots\\before.csv");
});

it("does not replace selected snapshots with non-CSV or ambiguous multi-file drops", async () => {
  await mount();
  drop(["C:\\before.csv"], 400);
  drop(["C:\\after.csv"], 1200);
  for (const paths of [[], ["C:\\wrong.txt"], ["C:\\one.csv", "C:\\two.csv"]]) {
    drop(paths, 400);
    expect(selected("之前")).toBe("C:\\before.csv");
    expect(selected("之后")).toBe("C:\\after.csv");
  }
  drop(["D:\\replacement.csv"], 400);
  expect(selected("之前")).toBe("D:\\replacement.csv");
  expect(selected("之后")).toBe("C:\\after.csv");
});

it("keeps both snapshot choices unchanged while an import is running", async () => {
  await mount();
  drop(["C:\\before.csv"], 400);
  drop(["C:\\after.csv"], 1200);
  await act(async () =>
    fireEvent.click(screen.getByRole("button", { name: "开始比较" })),
  );
  expect(
    screen.getByRole("button", { name: "开始比较" }).hasAttribute("disabled"),
  ).toBe(true);
  drop(["D:\\new-before.csv"], 400);
  drop(["D:\\new-after.csv"], 1200);
  expect(selected("之前")).toBe("C:\\before.csv");
  expect(selected("之后")).toBe("C:\\after.csv");
});

function summary(comparisonId: string): ComparisonSummary {
  const source = {
    path: "C:\\original.csv", description: null, rows: 1, files: 1, folders: 0,
    roots: [], rootCount: 1, rootsTruncated: false, size: "9007199254740993", allocated: "9007199254740993",
  };
  const statuses = { added: 0, removed: 0, modified: 0, unchanged: 1, typeChanged: 0 };
  return { comparisonId, before: source, after: source,
    statuses: { files: statuses, folders: statuses }, warnings: [comparisonId + " scope"] };
}
function job(state: JobSummary["state"], comparisonId?: string): JobSummary {
  return { jobId: "pending-import", state, phase: "finalizing", bytesRead: "100", totalBytes: "100", rows: 1,
    comparisonId, ...(state === "failed" ? { error: { code: "INVALID_CSV", message: "invalid source" } } : {}) };
}
async function start() {
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "开始比较" })));
}

it("starts blank, waits for manual start, reads only the ready ID, and starts blank again after remount", async () => {
  const view = render(<App />);
  await waitFor(() => expect(native.handler).not.toBeNull());
  expect(selected("之前")).toBe("");
  expect(selected("之后")).toBe("");
  expect(screen.getByText("选择两份 WizTree CSV 开始比较")).toBeTruthy();
  expect(api.getComparison).not.toHaveBeenCalled();
  drop(["C:\\before.csv"], 400);
  drop(["C:\\after.csv"], 1200);
  expect(api.startComparison).not.toHaveBeenCalled();
  expect(api.getComparison).not.toHaveBeenCalled();
  vi.mocked(api.getJob).mockResolvedValueOnce(job("ready", "manual-result"));
  vi.mocked(api.getComparison).mockResolvedValueOnce(summary("manual-result"));
  await start();
  expect(screen.getByText(/manual-result scope/)).toBeTruthy();
  expect(api.getComparison).toHaveBeenCalledWith("manual-result");
  expect(selected("之前")).toBe("C:\\before.csv");
  expect(selected("之后")).toBe("C:\\after.csv");
  view.unmount();
  render(<App />);
  await waitFor(() => expect(native.handler).not.toBeNull());
  expect(selected("之前")).toBe("");
  expect(selected("之后")).toBe("");
  expect(screen.queryByText(/manual-result scope/)).toBeNull();
  expect(api.getComparison).toHaveBeenCalledTimes(1);
});

it("keeps the previous comparison on failure, cancellation, and failed summary reads", async () => {
  render(<App />);
  await waitFor(() => expect(native.handler).not.toBeNull());
  drop(["C:\\before.csv"], 400);
  drop(["C:\\after.csv"], 1200);
  vi.mocked(api.getJob).mockResolvedValueOnce(job("ready", "old"));
  vi.mocked(api.getComparison).mockResolvedValueOnce(summary("old"));
  await start();
  expect(screen.getByText(/old scope/)).toBeTruthy();
  for (const state of ["failed", "cancelled"] as const) {
    vi.mocked(api.getJob).mockResolvedValueOnce(job(state));
    await start();
    expect(screen.getByText(/old scope/)).toBeTruthy();
    expect(api.getComparison).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "开始比较" }).hasAttribute("disabled")).toBe(false);
  }
  vi.mocked(api.getJob).mockResolvedValueOnce(job("ready", "new"));
  vi.mocked(api.getComparison).mockRejectedValueOnce(new Error("summary unavailable"));
  await start();
  expect(screen.getByText(/old scope/)).toBeTruthy();
  expect(screen.getByText("summary unavailable")).toBeTruthy();
  vi.mocked(api.getComparison).mockResolvedValueOnce(summary("new"));
  await act(async () => fireEvent.click(screen.getByText("重新读取对比")));
  expect(screen.getByText(/new scope/)).toBeTruthy();
  expect(screen.queryByText(/old scope/)).toBeNull();
});

it("shows finalization progress and accepts completion only once after manual start", async () => {
  const onReady = vi.fn();
  render(<ImportPanel onReady={onReady} />);
  await waitFor(() => expect(native.handler).not.toBeNull());
  drop(["C:\\before.csv"], 400);
  drop(["C:\\after.csv"], 1200);
  vi.mocked(api.getJob).mockResolvedValueOnce(job("running"));
  await start();
  expect(screen.getByText(/完成删除、层级与汇总/)).toBeTruthy();
  const progress = native.progress!;
  act(() => progress({ payload: job("ready", "complete") }));
  act(() => progress({ payload: job("ready", "complete") }));
  expect(onReady).toHaveBeenCalledTimes(1);
  expect(onReady).toHaveBeenCalledWith("complete");
});
