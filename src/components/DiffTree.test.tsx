import { useState } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "../api";
import type { ChildPage, ChildRow } from "../types";
import { DiffTree } from "./DiffTree";

vi.mock("../api", () => ({ api: { listChildren: vi.fn() } }));
// Keep every row in this small viewport; exercise the real cache/navigation state.
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getVirtualItems: () =>
      Array.from({ length: count }, (_, index) => ({
        index,
        start: index * 28,
      })),
    getTotalSize: () => count * 28,
    scrollToIndex: vi.fn(),
    scrollToOffset: vi.fn(),
  }),
}));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
function row(nodeId: string, name: string, expandable = true): ChildRow {
  return {
    nodeId,
    name,
    expandable,
    status: "unchanged",
    hasChanges: false,
    changedDescendantCount: 0,
    childCount: expandable ? 1 : 0,
    before: {
      kind: expandable ? "directory" : "file",
      size: "1",
      allocated: "1",
      modified: null,
      attributes: null,
    },
    after: null,
    sizeDelta: "0",
    allocatedDelta: "0",
  };
}
function page(...rows: ChildRow[]): ChildPage {
  return { rows, nextCursor: null, totalChildren: rows.length };
}
function pending() {
  let resolve!: (value: ChildPage) => void;
  const promise = new Promise<ChildPage>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}
function Tree({ comparisonId = "c-one" }: { comparisonId?: string }) {
  const [selected, onSelect] = useState<string | null>(null);
  const [scope, setScope] = useState("");
  return (
    <>
      <output aria-label="目录范围">{scope}</output>
      <DiffTree
        comparisonId={comparisonId}
        metric="size"
        changesOnly={false}
        selected={selected}
        onSelect={onSelect}
        onScope={(path) => setScope(path.map((item) => item.nodeId).join("/"))}
      />
    </>
  );
}
function disclosure(name: string) {
  return screen
    .getByText(name)
    .closest('[role="treeitem"]')!
    .querySelector("button")!;
}
it("retains both directory responses when expansions overlap and selection changes", async () => {
  const first = pending(),
    second = pending();
  vi.mocked(api.listChildren).mockImplementation(async (_id, parent) => {
    if (!parent) return page(row("n1", "System32"), row("n2", "Drivers"));
    return parent === "n1" ? first.promise : second.promise;
  });
  render(<Tree />);
  await screen.findByText("System32");
  fireEvent.click(disclosure("System32"));
  fireEvent.click(disclosure("Drivers"));
  fireEvent.click(screen.getByText("System32"));
  await act(async () => {
    second.resolve(page(row("n4", "driver.sys", false)));
  });
  await act(async () => {
    first.resolve(page(row("n3", "kernel.dll", false)));
  });
  expect(screen.getByText("kernel.dll")).toBeTruthy();
  expect(screen.getByText("driver.sys")).toBeTruthy();
  expect(screen.getByLabelText("目录范围").textContent).toBe("n1");
});
it("ignores a collapsed request without discarding the reopened directory response", async () => {
  const old = pending(),
    fresh = pending();
  vi.mocked(api.listChildren)
    .mockResolvedValueOnce(page(row("n1", "System32")))
    .mockReturnValueOnce(old.promise)
    .mockReturnValueOnce(fresh.promise);
  render(<Tree />);
  await screen.findByText("System32");
  fireEvent.click(disclosure("System32"));
  fireEvent.click(disclosure("System32"));
  fireEvent.click(disclosure("System32"));
  await act(async () => {
    old.resolve(page(row("n2", "obsolete.dll", false)));
  });
  expect(screen.queryByText("obsolete.dll")).toBeNull();
  await act(async () => {
    fresh.resolve(page(row("n3", "current.dll", false)));
  });
  expect(screen.getByText("current.dll")).toBeTruthy();
});
it("rejects pending children from a replaced comparison", async () => {
  const old = pending();
  vi.mocked(api.listChildren).mockImplementation(async (id, parent) => {
    if (id === "c-one")
      return parent ? old.promise : page(row("n1", "System32"));
    return page(row("n5", "New root"));
  });
  const view = render(<Tree />);
  await screen.findByText("System32");
  fireEvent.click(disclosure("System32"));
  view.rerender(<Tree comparisonId="c-two" />);
  await screen.findByText("New root");
  await act(async () => {
    old.resolve(page(row("n2", "obsolete.dll", false)));
  });
  await waitFor(() => expect(screen.queryByText("obsolete.dll")).toBeNull());
  expect(screen.queryByText("System32")).toBeNull();
});
