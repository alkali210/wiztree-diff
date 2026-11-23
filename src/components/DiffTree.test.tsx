import { useRef, useState } from "react";
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
import { resetSystemIconCacheForTest } from "../systemIcons";
import type { ChildPage, ChildRow, ChildSort, LocatedChildPage, Metric, NodeDetails } from "../types";
import { DiffTree, type TreeHandle } from "./DiffTree";

const virtualMocks = vi.hoisted(() => ({
  scrollToIndex: vi.fn(),
  scrollToOffset: vi.fn(),
  maxItems: Infinity,
}));
vi.mock("../api", () => ({
  api: {
    listChildren: vi.fn(),
    locateChildPage: vi.fn(),
    getSystemIcons: vi.fn(),
    getDetails: vi.fn(),
  },
}));
// Keep every row in this small viewport; exercise the real cache/navigation state.
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getVirtualItems: () =>
      Array.from({ length: Math.min(count, virtualMocks.maxItems) }, (_, index) => ({
        index,
        start: index * 28,
      })),
    getTotalSize: () => count * 28,
    scrollToIndex: virtualMocks.scrollToIndex,
    scrollToOffset: virtualMocks.scrollToOffset,
  }),
}));
afterEach(() => {
  cleanup();
  virtualMocks.maxItems = Infinity;
  resetSystemIconCacheForTest();
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
function Tree({
  comparisonId = "c-one",
  metric = "size",
}: {
  comparisonId?: string;
  metric?: Metric;
}) {
  const [selected, onSelect] = useState<string | null>(null);
  const [scope, setScope] = useState("");
  return (
    <>
      <output aria-label="目录范围">{scope}</output>
      <DiffTree
        comparisonId={comparisonId}
        metric={metric}
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

it("sorts by clicked column headers, toggles direction, preserves expanded folders, and follows metric changes", async () => {
  const calls: Array<{ parent: string | null; sort: ChildSort }> = [];
  vi.mocked(api.listChildren).mockImplementation(async (_id, parent, _changes, sort) => {
    calls.push({ parent, sort });
    if (!parent) {
      const items = [row("n1", "Alpha"), row("n2", "Beta")];
      return page(...(sort.direction === "desc" ? items.reverse() : items));
    }
    const children = [row("n3", "a.dll", false), row("n4", "b.dll", false)];
    return page(...(sort.direction === "desc" ? children.reverse() : children));
  });

  const view = render(<Tree metric="size" />);
  await screen.findByText("Alpha");
  expect(calls.at(-1)).toEqual({
    parent: null,
    sort: { by: "name", direction: "asc", metric: "size" },
  });

  fireEvent.click(disclosure("Alpha"));
  await screen.findByText("a.dll");

  // Clicking a size column defaults to descending and reloads expanded folder too.
  fireEvent.click(screen.getByRole("button", { name: "大小（之后）" }));
  await waitFor(() =>
    expect(calls.some((c) => c.parent === "n1" && c.sort.by === "after" && c.sort.direction === "desc")).toBe(true),
  );
  expect(screen.getByText("a.dll")).toBeTruthy();

  // Clicking the active column again flips to ascending.
  fireEvent.click(screen.getByRole("button", { name: "大小（之后）" }));
  await waitFor(() =>
    expect(calls.some((c) => c.parent === null && c.sort.by === "after" && c.sort.direction === "asc")).toBe(true),
  );

  // Switching metric re-sorts when sorting by a metric column.
  view.rerender(<Tree metric="allocated" />);
  await waitFor(() =>
    expect(
      calls.some(
        (c) => c.parent === null && c.sort.by === "after" && c.sort.direction === "asc" && c.sort.metric === "allocated",
      ),
    ).toBe(true),
  );

  // Switching to a non-metric column does not trigger an extra reload on metric change.
  fireEvent.click(screen.getByRole("button", { name: "名称" }));
  await waitFor(() =>
    expect(calls.some((c) => c.parent === null && c.sort.by === "name" && c.sort.direction === "asc")).toBe(true),
  );
  const countBefore = calls.length;
  view.rerender(<Tree metric="size" />);
  await act(async () => {});
  expect(calls.length).toBe(countBefore);
});

it("discards stale sort responses when the user rapidly changes sort order", async () => {
  const firstSort = pending();
  const secondSort = pending();
  vi.mocked(api.listChildren)
    .mockResolvedValueOnce(page(row("n1", "Initial")))
    .mockReturnValueOnce(firstSort.promise)
    .mockReturnValueOnce(secondSort.promise);

  render(<Tree />);
  await screen.findByText("Initial");

  fireEvent.click(screen.getByRole("button", { name: "Δ 大小" }));
  fireEvent.click(screen.getByRole("button", { name: "Δ 大小" }));

  await act(async () => {
    secondSort.resolve(page(row("n3", "LatestSort")));
  });
  await act(async () => {
    firstSort.resolve(page(row("n2", "StaleSort")));
  });

  expect(screen.getByText("LatestSort")).toBeTruthy();
  expect(screen.queryByText("StaleSort")).toBeNull();
});

it("loads batched Windows system icons for folders and file extensions and falls back cleanly", async () => {
  vi.mocked(api.listChildren).mockResolvedValueOnce(
    page(
      row("n1", "Windows", true),
      row("n2", "bun.ps1", false),
      row("n3", "other.PS1", false),
      row("n4", "bun.", false),
    ),
  );
  vi.mocked(api.getSystemIcons).mockResolvedValueOnce({
    __dir__: "data:image/png;base64,DIR",
    ".ps1": "data:image/png;base64,PS1",
  });

  render(<Tree />);
  await screen.findByText("bun.ps1");

  await waitFor(() => {
    const dirImg = screen
      .getByText("Windows")
      .closest('[role="treeitem"]')!
      .querySelector("img.node-icon");
    expect(dirImg?.getAttribute("src")).toBe("data:image/png;base64,DIR");
  });

  // Deduplicated keys: __dir__, .ps1, "" fetched in one batched call.
  expect(api.getSystemIcons).toHaveBeenCalledTimes(1);
  expect(new Set(vi.mocked(api.getSystemIcons).mock.calls[0][0])).toEqual(
    new Set(["__dir__", ".ps1", ""]),
  );

  // Both .ps1 and .PS1 reuse the same icon; missing "" falls back to .file-glyph.
  expect(
    screen
      .getByText("other.PS1")
      .closest('[role="treeitem"]')!
      .querySelector("img.node-icon")
      ?.getAttribute("src"),
  ).toBe("data:image/png;base64,PS1");
  expect(
    screen
      .getByText("bun.")
      .closest('[role="treeitem"]')!
      .querySelector("i.file-glyph"),
  ).not.toBeNull();
});

it("toggles folder expansion on row double-click and prevents rapid disclosure clicks from bouncing open", async () => {
  vi.mocked(api.listChildren).mockImplementation(async (_id, parent) => {
    if (!parent) return page(row("n1", "BDalSystemData", true));
    return page(row("n2", "BuildingBlocks", true));
  });

  render(<Tree />);
  const folderText = await screen.findByText("BDalSystemData");
  const folderRow = folderText.closest('[role="treeitem"]')!;

  // Double-clicking the row expands the directory.
  fireEvent.doubleClick(folderRow);
  await screen.findByText("BuildingBlocks");
  expect(folderRow.getAttribute("aria-expanded")).toBe("true");

  // Double-clicking the row again collapses the directory.
  fireEvent.doubleClick(folderRow);
  expect(screen.queryByText("BuildingBlocks")).toBeNull();
  expect(folderRow.getAttribute("aria-expanded")).toBe("false");

  // Rapidly clicking the disclosure button twice (click, click, dblclick) must end collapsed without bouncing back open.
  const toggleBtn = disclosure("BDalSystemData");
  fireEvent.click(toggleBtn);
  await screen.findByText("BuildingBlocks");
  fireEvent.click(toggleBtn);
  fireEvent.doubleClick(toggleBtn);
  await act(async () => {});
  expect(screen.queryByText("BuildingBlocks")).toBeNull();
  expect(folderRow.getAttribute("aria-expanded")).toBe("false");
});

function details(nodeId: string, parentId: string | null): NodeDetails {
  return {
    nodeId,
    parentId,
    before: null,
    after: null,
    sizeDelta: "0",
    allocatedDelta: "0",
  };
}
function located(start: number, totalChildren: number, ...rows: ChildRow[]): LocatedChildPage {
  return { start, rows, nextCursor: null, totalChildren };
}

it("reveals and scrolls to nested files without expanding them, and expands directories when requested", async () => {
  vi.mocked(api.listChildren).mockImplementation(async (_id, parent) => {
    if (!parent) return page(row("n1", "Root", true));
    if (parent === "n2") return page(row("n3", "target.bin", false));
    return page();
  });
  vi.mocked(api.locateChildPage).mockImplementation(async (_id, parent, target) => {
    if (parent === "n1" && target === "n2") return located(0, 1, row("n2", "Sub", true));
    if (parent === "n2" && target === "n3") return located(0, 1, row("n3", "target.bin", false));
    return null;
  });
  vi.mocked(api.getDetails).mockImplementation(async (_id, nodeId) => {
    if (nodeId === "n3") return details("n3", "n2");
    if (nodeId === "n2") return details("n2", "n1");
    return details(nodeId, null);
  });

  let handle: TreeHandle | null = null;
  function Host() {
    const ref = useRef<TreeHandle>(null);
    const [selected, onSelect] = useState<string | null>(null);
    const [scope, setScope] = useState("");
    handle = ref.current;
    return (
      <>
        <output aria-label="目录范围">{scope}</output>
        <output aria-label="当前选中">{selected ?? ""}</output>
        <DiffTree
          ref={(v) => {
            ref.current = v;
            handle = v;
          }}
          comparisonId="c-one"
          metric="size"
          changesOnly={false}
          selected={selected}
          onSelect={onSelect}
          onScope={(path) => setScope(path.map((item) => item.nodeId).join("/"))}
        />
      </>
    );
  }

  render(<Host />);
  await screen.findByText("Root");

  await act(async () => {
    handle!.reveal("n3", false);
  });
  await screen.findByText("target.bin");
  expect(screen.getByLabelText("当前选中").textContent).toBe("n3");
  expect(screen.getByLabelText("目录范围").textContent).toBe("n1/n2");
  expect(virtualMocks.scrollToIndex).toHaveBeenCalledWith(2, { align: "center" });

  virtualMocks.scrollToIndex.mockClear();
  await act(async () => {
    handle!.reveal("n2", true);
  });
  expect(screen.getByLabelText("当前选中").textContent).toBe("n2");
  expect(screen.getByLabelText("目录范围").textContent).toBe("n1/n2");
  expect(virtualMocks.scrollToIndex).toHaveBeenCalledWith(1, { align: "center" });
});

it("cancels an in-flight reveal when superseded by a newer reveal", async () => {
  const firstDetails = (() => {
    let resolve!: (value: NodeDetails) => void;
    const promise = new Promise<NodeDetails>((r) => {
      resolve = r;
    });
    return { promise, resolve };
  })();
  vi.mocked(api.listChildren).mockResolvedValueOnce(
    page(row("n1", "Alpha", true), row("n2", "Beta", true)),
  );
  vi.mocked(api.locateChildPage).mockImplementation(async (_id, parent, target) => {
    if (parent === "n2" && target === "n4") return located(0, 1, row("n4", "new.txt", false));
    if (parent === "n1" && target === "n3") return located(0, 1, row("n3", "old.txt", false));
    return null;
  });
  vi.mocked(api.getDetails).mockImplementation(async (_id, nodeId) => {
    if (nodeId === "n3") return firstDetails.promise;
    if (nodeId === "n4") return details("n4", "n2");
    return details(nodeId, null);
  });

  let handle: TreeHandle | null = null;
  function Host() {
    const [selected, onSelect] = useState<string | null>(null);
    return (
      <DiffTree
        ref={(v) => {
          handle = v;
        }}
        comparisonId="c-one"
        metric="size"
        changesOnly={false}
        selected={selected}
        onSelect={onSelect}
        onScope={() => {}}
      />
    );
  }

  render(<Host />);
  await screen.findByText("Alpha");

  act(() => {
    handle!.reveal("n3", false);
    handle!.reveal("n4", false);
  });
  await screen.findByText("new.txt");
  await act(async () => {
    firstDetails.resolve(details("n3", "n1"));
  });
  expect(screen.queryByText("old.txt")).toBeNull();
  expect(screen.getByText("new.txt")).toBeTruthy();
});

it("jumps directly to the target page via locateChildPage without sequential scanning", async () => {
  vi.mocked(api.listChildren).mockResolvedValueOnce(page(row("n1", "Wide", true)));
  vi.mocked(api.getDetails).mockResolvedValueOnce(details("n420", "n1"));
  vi.mocked(api.locateChildPage).mockResolvedValueOnce(
    located(400, 450, row("n420", "item_420.bin", false)),
  );

  let handle: TreeHandle | null = null;
  function Host() {
    const [selected, onSelect] = useState<string | null>(null);
    return (
      <DiffTree
        ref={(v) => {
          handle = v;
        }}
        comparisonId="c-one"
        metric="size"
        changesOnly={false}
        selected={selected}
        onSelect={onSelect}
        onScope={() => {}}
      />
    );
  }

  render(<Host />);
  await screen.findByText("Wide");
  const initialListCalls = vi.mocked(api.listChildren).mock.calls.length;

  await act(async () => {
    handle!.reveal("n420", false);
  });
  await screen.findByText("item_420.bin");
  expect(api.locateChildPage).toHaveBeenCalledTimes(1);
  expect(api.locateChildPage).toHaveBeenCalledWith(
    "c-one",
    "n1",
    "n420",
    false,
    { by: "name", direction: "asc", metric: "size" },
  );
  expect(api.listChildren).toHaveBeenCalledTimes(initialListCalls);
  expect(virtualMocks.scrollToIndex).toHaveBeenCalled();
});

it("waits for an in-flight child page when revealing a node under an already-expanding folder", async () => {
  const inflight = pending();
  vi.mocked(api.listChildren)
    .mockResolvedValueOnce(page(row("n1", "Folder", true)))
    .mockReturnValueOnce(inflight.promise);
  vi.mocked(api.getDetails).mockResolvedValueOnce(details("n2", "n1"));

  let handle: TreeHandle | null = null;
  function Host() {
    const [selected, onSelect] = useState<string | null>(null);
    return (
      <DiffTree
        ref={(v) => {
          handle = v;
        }}
        comparisonId="c-one"
        metric="size"
        changesOnly={false}
        selected={selected}
        onSelect={onSelect}
        onScope={() => {}}
      />
    );
  }

  render(<Host />);
  await screen.findByText("Folder");

  fireEvent.click(disclosure("Folder"));
  await act(async () => {
    handle!.reveal("n2", false);
  });
  expect(api.locateChildPage).not.toHaveBeenCalled();

  await act(async () => {
    inflight.resolve(page(row("n2", "arrived.txt", false)));
  });
  await screen.findByText("arrived.txt");
  expect(virtualMocks.scrollToIndex).toHaveBeenCalledWith(1, { align: "center" });
});

it("cancels a deferred reveal if a newer reveal occurs while reloading the unfiltered root", async () => {
  const rootReload = pending();
  vi.mocked(api.listChildren)
    .mockResolvedValueOnce(page(row("n1", "Changed", true)))
    .mockReturnValueOnce(rootReload.promise);
  vi.mocked(api.getDetails).mockResolvedValue(details("n2", null));
  vi.mocked(api.locateChildPage).mockResolvedValue(located(0, 2, row("n2", "NewRoot", false)));

  let handle: TreeHandle | null = null;
  function Host({ changesOnly }: { changesOnly: boolean }) {
    const [selected, onSelect] = useState<string | null>(null);
    return (
      <>
        <output aria-label="选中">{selected ?? ""}</output>
        <DiffTree
          ref={(v) => {
            handle = v;
          }}
          comparisonId="c-one"
          metric="size"
          changesOnly={changesOnly}
          selected={selected}
          onSelect={onSelect}
          onScope={() => {}}
          reveal={{ comparisonId: "c-one", path: [], nodeId: "n_old", expand: false }}
        />
      </>
    );
  }

  const view = render(<Host changesOnly={true} />);
  await screen.findByText("Changed");
  view.rerender(<Host changesOnly={false} />);
  await act(async () => {
    handle!.reveal("n2", false);
  });
  await act(async () => {
    rootReload.resolve(page(row("n1", "Changed", true), row("n_old", "OldRoot", false)));
  });
  await act(async () => {});
  expect(screen.getByLabelText("选中").textContent).toBe("n2");
});

it("prioritizes reveal scrollToIndex over cache-eviction anchor restoration", async () => {
  virtualMocks.maxItems = 20;
  const bulk = Array.from({ length: 9900 }, (_, i) => row(`b${i}`, `bulk_${i}`, false));
  const extra = Array.from({ length: 200 }, (_, i) => row(`t${i}`, `target_${i}`, false));
  vi.mocked(api.listChildren).mockImplementation(async (_id, parent) => {
    if (!parent) return page(row("n_old", "Old", true), row("n_new", "Target", true));
    if (parent === "n_old") return page(...bulk);
    return page();
  });
  vi.mocked(api.getDetails).mockResolvedValueOnce(details("t150", "n_new"));
  vi.mocked(api.locateChildPage).mockResolvedValueOnce(located(0, 200, ...extra));

  let handle: TreeHandle | null = null;
  function Host() {
    const [selected, onSelect] = useState<string | null>(null);
    return (
      <DiffTree
        ref={(v) => {
          handle = v;
        }}
        comparisonId="c-one"
        metric="size"
        changesOnly={false}
        selected={selected}
        onSelect={onSelect}
        onScope={() => {}}
      />
    );
  }

  render(<Host />);
  await screen.findByText("Old");
  fireEvent.click(disclosure("Old"));
  await screen.findByText("bulk_0");

  virtualMocks.scrollToIndex.mockClear();
  virtualMocks.scrollToOffset.mockClear();
  await act(async () => {
    handle!.reveal("t150", false);
  });
  await waitFor(() =>
    expect(virtualMocks.scrollToIndex).toHaveBeenCalledWith(9951, {
      align: "center",
    }),
  );
  const lastOffsetOrder = virtualMocks.scrollToOffset.mock.invocationCallOrder.at(-1) ?? 0;
  expect(lastOffsetOrder).toBeLessThan(virtualMocks.scrollToIndex.mock.invocationCallOrder[0]);
});
