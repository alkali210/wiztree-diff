import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "../api";
import { resetSystemIconCacheForTest } from "../systemIcons";
import type { ExtensionPage } from "../types";
import { FileTypePanel } from "./FileTypePanel";

vi.mock("../api", () => ({
  api: { listExtensions: vi.fn(), getSystemIcons: vi.fn() },
}));

afterEach(async () => {
  cleanup();
  resetSystemIconCacheForTest();
  await act(async () => {});
  vi.clearAllMocks();
});

it("renders extension labels, file counts with tooltips, and supports paging", async () => {
  const longExt = ".super_long_extension_name_for_overflow_boundary_check";
  const page1: ExtensionPage = {
    rows: [
      { extension: "", size: "1024", allocated: "4096", files: 270604 },
      { extension: longExt, size: "2048", allocated: "4096", files: 1234567 },
    ],
    nextCursor: "cursor-2",
    totalExtensions: 3,
    total: { size: "4096", allocated: "12288", files: 1505172 },
    warnings: [],
  };
  const page2: ExtensionPage = {
    rows: [{ extension: ".dll", size: "1024", allocated: "4096", files: 1 }],
    nextCursor: null,
    totalExtensions: 3,
    total: { size: "4096", allocated: "12288", files: 1505172 },
    warnings: [],
  };
  vi.mocked(api.listExtensions).mockImplementation(async (_id, _p, _s, _m, cursor) =>
    cursor ? page2 : page1,
  );

  const view = render(
    <FileTypePanel
      comparisonId="cmp-1"
      parentId={null}
      metric="size"
      mode="after"
      name="工作区"
      showFiles
    />,
  );
  await act(async () => {});

  const rowsContainer = view.container.querySelector(".extension-rows")!;
  expect(rowsContainer.querySelector(".type-heading")).not.toBeNull();
  const longName = screen.getByTitle(longExt);
  expect(longName.querySelector("span")?.textContent).toBe(longExt);
  expect(screen.getByTitle("(无扩展名)").querySelector("span")?.textContent).toBe("(无扩展名)");
  expect(screen.getByTitle((1234567).toLocaleString()).classList.contains("type-files")).toBe(true);

  fireEvent.click(screen.getByRole("button", { name: "下一页" }));
  await act(async () => {});
  expect(screen.getByTitle(".dll")).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "首页" }));
  await act(async () => {});
  expect(screen.getByTitle(longExt)).toBeTruthy();
});

it("displays system icons alongside color swatches in extension rows", async () => {
  vi.mocked(api.listExtensions).mockResolvedValueOnce({
    rows: [
      { extension: ".dll", size: "2048", allocated: "4096", files: 5 },
      { extension: "", size: "1024", allocated: "4096", files: 2 },
    ],
    nextCursor: null,
    totalExtensions: 2,
    total: { size: "3072", allocated: "8192", files: 7 },
    warnings: [],
  });
  vi.mocked(api.getSystemIcons).mockResolvedValueOnce({
    ".dll": "data:image/png;base64,DLL",
    "": "data:image/png;base64,PLAIN",
  });

  render(
    <FileTypePanel
      comparisonId="cmp-1"
      parentId={null}
      metric="size"
      mode="after"
      name="工作区"
      showFiles
    />,
  );

  await waitFor(() => {
    const dllRow = screen.getByTitle(".dll");
    expect(dllRow.querySelector("img.type-icon")?.getAttribute("src")).toBe(
      "data:image/png;base64,DLL",
    );
    expect(dllRow.querySelector("i.type-swatch.slim")).not.toBeNull();
  });
  expect(
    screen.getByTitle("(无扩展名)").querySelector("img.type-icon")?.getAttribute("src"),
  ).toBe("data:image/png;base64,PLAIN");
});
