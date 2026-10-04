import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "../api";
import { DetailsPanel } from "./DetailsPanel";
import type { Entry, NodeDetails } from "../types";
vi.mock("../api", () => ({ api: { getDetails: vi.fn() } }));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}
function detail(id: string, path: string): NodeDetails {
  const entry: Entry = {
    path,
    kind: "file",
    size: "9007199254740993",
    allocated: "0",
    modified: null,
    attributes: null,
    files: null,
    folders: null,
    mft: null,
    parentMft: null,
    accessed: null,
    created: null,
    directSize: null,
    directAllocated: null,
    driveCapacity: null,
    freeSpace: null,
    usedSpace: null,
    reservedSpace: null,
    hardlinkCount: 1,
  };
  return {
    nodeId: id,
    parentId: null,
    before: entry,
    after: null,
    sizeDelta: "-9007199254740993",
    allocatedDelta: "0",
  };
}
it("ignores an old node response after a newer selection resolves", async () => {
  const old = deferred<NodeDetails>(),
    current = deferred<NodeDetails>();
  vi.mocked(api.getDetails)
    .mockReturnValueOnce(old.promise)
    .mockReturnValueOnce(current.promise);
  const view = render(<DetailsPanel comparisonId="one" nodeId="old" />);
  view.rerender(<DetailsPanel comparisonId="one" nodeId="new" />);
  await act(async () => current.resolve(detail("new", "C:\\new.txt")));
  expect(screen.getByText("C:\\new.txt")).toBeTruthy();
  await act(async () => old.resolve(detail("old", "C:\\old.txt")));
  expect(screen.queryByText("C:\\old.txt")).toBeNull();
  expect(screen.getByText("C:\\new.txt")).toBeTruthy();
});
it("ignores an old comparison response when the node ID is reused", async () => {
  const old = deferred<NodeDetails>(),
    current = deferred<NodeDetails>();
  vi.mocked(api.getDetails)
    .mockReturnValueOnce(old.promise)
    .mockReturnValueOnce(current.promise);
  const view = render(
    <DetailsPanel comparisonId="old-comparison" nodeId="same" />,
  );
  view.rerender(<DetailsPanel comparisonId="new-comparison" nodeId="same" />);
  await act(async () => current.resolve(detail("same", "D:\\current.txt")));
  await act(async () => old.resolve(detail("same", "C:\\outdated.txt")));
  expect(screen.queryByText("C:\\outdated.txt")).toBeNull();
  expect(screen.getByText("D:\\current.txt")).toBeTruthy();
});
