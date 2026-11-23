import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import App from "./App";
import { api } from "./api";
import type { ComparisonSummary } from "./types";

vi.mock("./api", () => ({api: {getComparison: vi.fn(), getFullTreemap: vi.fn(() => new Promise(() => {})), releaseTreemap: vi.fn(() => Promise.resolve())}}));
vi.mock("./components/ImportPanel", () => ({ImportPanel: ({onReady}: {onReady: (id: string) => void}) => <>
  <button onClick={() => onReady("small")}>small</button><button onClick={() => onReady("large")}>large</button>
</>}));
vi.mock("./components/CategoryPanel", () => ({CategoryPanel: () => null}));
vi.mock("./components/DetailsPanel", () => ({DetailsPanel: () => null}));
vi.mock("./components/FileTypePanel", () => ({FileTypePanel: () => null}));
vi.mock("./components/DiffTree", () => ({DiffTree: () => <div>tree</div>}));

function summary(comparisonId: string, rows: number, warnings: string[] = []): ComparisonSummary {
  const source = {path: "fixture.csv", description: null, rows, files: rows, folders: 0, roots: [], rootCount: 0, rootsTruncated: false, size: "0", allocated: "0"};
  const counts = {added: 0, removed: 0, modified: 0, unchanged: 0, typeChanged: 0};
  return {comparisonId, before: {...source, rows: 499999}, after: source, statuses: {files: counts, folders: counts}, warnings};
}
async function choose(id: string) { fireEvent.click(screen.getByText(id)); await act(async () => {}); }
afterEach(async () => { cleanup(); await act(async () => {}); vi.clearAllMocks(); });

it("defers at the combined record threshold, loads on expand, and resets for a new comparison", async () => {
  vi.mocked(api.getComparison).mockImplementation(async id => summary(id, id === "large" ? 500001 : 500000));
  render(<App />);
  await choose("large");
  expect(api.getFullTreemap).not.toHaveBeenCalled();
  expect(screen.getByText("tree")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", {name: "展开占用图"}));
  await act(async () => {});
  expect(api.getFullTreemap).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", {name: "收起占用图"}));
  await act(async () => {});
  expect(api.releaseTreemap).not.toHaveBeenCalled();
  await choose("small");
  expect(api.getFullTreemap).toHaveBeenCalledTimes(2);
  expect(screen.getByRole("button", {name: "收起占用图"}).getAttribute("aria-expanded")).toBe("true");
  await choose("large");
  expect(api.getFullTreemap).toHaveBeenCalledTimes(2);
  expect(screen.getByRole("button", {name: "展开占用图"})).toBeTruthy();
});

it("keeps sidebar controls and warning confirmation usable while collapsed", async () => {
  vi.mocked(api.getComparison).mockResolvedValue(summary("large", 500001, ["scope warning"]));
  const view = render(<App />);
  await choose("large");
  expect((screen.getByRole("button", {name: "展开占用图"}) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", {name: "收起左侧栏"}));
  expect(view.container.querySelector("#workspace-sidebar")?.hasAttribute("hidden")).toBe(true);
  fireEvent.click(screen.getByRole("button", {name: "我理解范围限制，继续查看"}));
  await act(async () => {});
  expect(api.getFullTreemap).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", {name: "展开左侧栏"}));
  expect(view.container.querySelector("#workspace-sidebar")?.hasAttribute("hidden")).toBe(false);
  fireEvent.click(screen.getByRole("button", {name: "展开占用图"}));
  await act(async () => {});
  expect(api.getFullTreemap).toHaveBeenCalledTimes(1);
});

it("uses icon-only independent panel toggles without a collapse notice", async () => {
  vi.mocked(api.getComparison).mockResolvedValue(summary("large", 500001));
  const view = render(<App />);
  await choose("large");
  const controls = view.container.querySelector(".workspace-controls")!;
  expect(controls.textContent).toBe("");
  expect(controls.querySelectorAll("svg")).toHaveLength(3);
  fireEvent.click(screen.getByRole("button", {name: "收起右侧栏"}));
  const right = view.container.querySelector("#workspace-extensions")!.parentElement!;
  expect(right.style.display).toBe("none");
  expect(screen.queryByRole("separator", {name: "调整目录树与扩展名宽度"})).toBeNull();
  expect(screen.getByRole("button", {name: "收起左侧栏"})).toBeTruthy();
  fireEvent.click(screen.getByRole("button", {name: "展开右侧栏"}));
  expect(right.style.display).toBe("");
  expect(screen.getByRole("separator", {name: "调整目录树与扩展名宽度"})).toBeTruthy();
  expect(api.getFullTreemap).not.toHaveBeenCalled();
});

it("allows switching global mode and metric from the top toolbar even while treemap is collapsed", async () => {
  const largeSummary = summary("large", 500001);
  largeSummary.before = { ...largeSummary.before, size: "1024", allocated: "4096" };
  largeSummary.after = { ...largeSummary.after, size: "2048", allocated: "8192" };
  vi.mocked(api.getComparison).mockResolvedValue(largeSummary);
  render(<App />);
  await choose("large");
  expect(api.getFullTreemap).not.toHaveBeenCalled();
  expect(screen.getByTitle("2048 B").textContent).toBe("2.00 KiB");
  fireEvent.click(screen.getByRole("button", { name: "分配大小" }));
  fireEvent.click(screen.getByRole("button", { name: "差异视图" }));
  await act(async () => {});
  expect(api.getFullTreemap).not.toHaveBeenCalled();
  expect(screen.getByTitle("8192 B").textContent).toBe("8.00 KiB");
  fireEvent.click(screen.getByRole("button", { name: "展开占用图" }));
  await act(async () => {});
  expect(api.getFullTreemap).toHaveBeenCalledTimes(1);
  expect(api.getFullTreemap).toHaveBeenLastCalledWith("large", "allocated", "delta", 0);
});
