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

const native = vi.hoisted(() => ({
  handler: null as ((event: Event<DragDropEvent>) => void) | null,
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: async (handler: typeof native.handler) => {
      native.handler = handler;
      return () => {};
    },
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: async () => () => {} }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("../api", () => ({
  api: {
    startComparison: async () => "pending-import",
    getJob: () => new Promise(() => {}),
  },
}));

beforeEach(() => {
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
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
async function mount() {
  render(<ImportPanel comparison={null} onReady={() => {}} />);
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
