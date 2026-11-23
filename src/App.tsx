import { useCallback, useEffect, useRef, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { WindowControls } from "./components/WindowControls";
import { api } from "./api";
import { bytes, errorText, signed } from "./format";
import type {
  ChartMode,
  ComparisonSummary,
  Metric,
  SnapshotSide,
  Status,
  TreemapHit,
} from "./types";
import { ImportPanel } from "./components/ImportPanel";
import {
  DiffTree,
  type Breadcrumb,
  type TreeHandle,
} from "./components/DiffTree";
import { DetailsPanel } from "./components/DetailsPanel";
import { OccupancyCharts } from "./components/OccupancyCharts";
import { RootList } from "./components/RootList";
import { FileTypePanel } from "./components/FileTypePanel";
import { CategoryPanel } from "./components/CategoryPanel";
import { SplitPane } from "./components/SplitPane";
import { PanelToggle } from "./components/PanelToggle";
import "./App.css";
// Record count bounds layout work better than the disk sizes represented by CSV rows.
const DEFER_TREEMAP_ROWS = 1_000_000;
const statuses: Status[] = [
  "added",
  "removed",
  "modified",
  "typeChanged",
  "unchanged",
];
const labels: Record<Status, string> = {
  added: "新增",
  removed: "删除",
  modified: "修改",
  typeChanged: "类型替换",
  unchanged: "不变",
};
export default function App() {
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [extensionsCollapsed, setExtensionsCollapsed] = useState(false);
  const [treemapPreference, setTreemapPreference] = useState<{
    comparisonId: string;
    collapsed: boolean;
  } | null>(null);
  const [metric, setMetric] = useState<Metric>("size"),
    [mode, setMode] = useState<ChartMode>("after");
  const [selectedName, setSelectedName] = useState("");
  const [rootReview, setRootReview] = useState<SnapshotSide | null>(null);
  const [comparison, setComparison] = useState<ComparisonSummary | null>(null),
    [error, setError] = useState(""),
    [loading, setLoading] = useState(false),
    [confirmed, setConfirmed] = useState<string | null>(null),
    [selected, setSelected] = useState<string | null>(null),
    [changesOnly, setChangesOnly] = useState(false),
    [path, setPath] = useState<Breadcrumb[]>([]);
  const generation = useRef(0),
    tree = useRef<TreeHandle>(null),
    requestedComparison = useRef<string | null>(null);
  const [reveal, setReveal] = useState<{
    comparisonId: string;
    path: Breadcrumb[];
    nodeId: string;
    expand?: boolean;
  } | null>(null);
  const refresh = useCallback((comparisonId: string) => {
    requestedComparison.current = comparisonId;
    const token = ++generation.current;
    setLoading(true);
    setRootReview(null);
    setError("");
    void api
      .getComparison(comparisonId)
      .then(
        (c) => {
          if (token !== generation.current) return;
          setComparison(c);
          setSelected(null);
          setPath([]);
          setChangesOnly(false);
          setReveal(null);
          setConfirmed(null);
        },
        (e) => {
          if (token === generation.current) setError(errorText(e));
        },
      )
      .finally(() => {
        if (token === generation.current) setLoading(false);
      });
  }, []);
  useEffect(() => {
    return () => {
      generation.current++;
    };
  }, []);
  useEffect(() => {
    let live = true;
    setSelectedName("");
    if (comparison && selected)
      void api.getDetails(comparison.comparisonId, selected).then(
        (data) => {
          if (live)
            setSelectedName(
              (data.after ?? data.before)?.path
                .replace(/[\\/]+$/, "")
                .split(/[\\/]/)
                .pop() ?? "",
            );
        },
        () => {},
      );
    return () => {
      live = false;
    };
  }, [comparison, selected]);
  const allowed =
    comparison &&
    (!comparison.warnings.length || confirmed === comparison.comparisonId);
  const handleTreemapSelect = useCallback(
    (nodeId: string, hit?: TreemapHit) => {
      if (!comparison || !allowed) return;
      setSelected(nodeId);
      const expandTarget = hit ? hit.kind === "directory" : false;
      if (changesOnly && hit?.status === "unchanged") {
        setReveal({
          comparisonId: comparison.comparisonId,
          path: [],
          nodeId,
          expand: expandTarget,
        });
        setChangesOnly(false);
      } else {
        tree.current?.reveal(nodeId, expandTarget);
      }
    },
    [allowed, changesOnly, comparison],
  );
  const largeComparison = !!comparison &&
    comparison.before.rows + comparison.after.rows >= DEFER_TREEMAP_ROWS;
  const treemapCollapsed = treemapPreference && treemapPreference.comparisonId === comparison?.comparisonId
    ? treemapPreference.collapsed : largeComparison;
  return (
    <main>
      {isTauri() && (
        <header className="window-header" data-tauri-drag-region>
          <WindowControls />
        </header>
      )}
      <ImportPanel onReady={refresh} />
      {comparison?.orderWarning && (
        <div className="warning" role="status">
          当前比较的“之前” CSV 导出时间比“之后”更新（之前：
          {comparison.orderWarning.beforeTime}；之后：
          {comparison.orderWarning.afterTime}）。请确认是否选反；可交换两侧后重新比较。
          结果仍按您选择的顺序显示。
        </div>
      )}
      {error && (
        <div className="error app-error">
          {error}{" "}
          <button onClick={() => {
            if (requestedComparison.current) refresh(requestedComparison.current);
          }}>重新读取对比</button>
        </div>
      )}
      {!comparison && (
        <div className="empty">
          {loading ? "正在读取比较结果…" : "选择两份 WizTree CSV 开始比较"}
        </div>
      )}
      {comparison && (
        <>
          <div className="workspace-bar">
            <div className="workspace-view-controls">
              <div className="segmented" role="group" aria-label="视图模式">
                {(["before", "after", "delta"] as const).map((m, i) => (
                  <button
                    key={m}
                    type="button"
                    aria-label={["之前视图", "之后视图", "差异视图"][i]}
                    aria-pressed={mode === m}
                    className={mode === m ? "active" : ""}
                    onClick={() => setMode(m)}
                  >
                    {["之前", "之后", "差异"][i]}
                  </button>
                ))}
              </div>
              <div className="segmented" role="group" aria-label="统计指标">
                <button
                  type="button"
                  aria-label="逻辑大小"
                  aria-pressed={metric === "size"}
                  className={metric === "size" ? "active" : ""}
                  onClick={() => setMetric("size")}
                >
                  大小
                </button>
                <button
                  type="button"
                  aria-label="分配大小"
                  aria-pressed={metric === "allocated"}
                  className={metric === "allocated" ? "active" : ""}
                  onClick={() => setMetric("allocated")}
                >
                  分配
                </button>
              </div>
            </div>
            <div className="workspace-controls">
              <PanelToggle panel="left" label="左侧栏" expanded={!sidebarCollapsed} controls="workspace-sidebar"
                onClick={() => setSidebarCollapsed((value) => !value)} />
              <PanelToggle panel="bottom" label="占用图" expanded={!treemapCollapsed} controls="workspace-treemap" disabled={!allowed}
                onClick={() => setTreemapPreference({ comparisonId: comparison.comparisonId, collapsed: !treemapCollapsed })} />
              <PanelToggle panel="right" label="右侧栏" expanded={!extensionsCollapsed} controls="workspace-extensions"
                onClick={() => setExtensionsCollapsed((value) => !value)} />
            </div>
          </div>
          <div className="workspace">
            <aside id="workspace-sidebar" className="left-pane" hidden={sidebarCollapsed}>
              <section className="global-totals">
                <h2>工作区</h2>
                {["before", "after"].map((side, i) => {
                  const source = i ? comparison.after : comparison.before;
                  return (
                    <div className="global-total" key={side}>
                      <strong title={source[metric] + " B"}>
                        {bytes(source[metric])}
                      </strong>
                      <span>{i ? "之后" : "之前"}</span>
                      <button
                        aria-label={i ? "查看之后导出根" : "查看之前导出根"}
                        disabled={loading}
                        onClick={() => setRootReview(i ? "after" : "before")}
                      >
                        {source.rootCount.toLocaleString()} 个根 ›
                      </button>
                    </div>
                  );
                })}
                <strong
                  className={
                    BigInt(comparison.after[metric]) -
                      BigInt(comparison.before[metric]) >
                    0n
                      ? "positive"
                      : BigInt(comparison.after[metric]) <
                          BigInt(comparison.before[metric])
                        ? "negative"
                        : ""
                  }
                >
                  {signed(
                    (
                      BigInt(comparison.after[metric]) -
                      BigInt(comparison.before[metric])
                    ).toString(),
                  )}
                </strong>
                <label className="changes-only">
                  <input
                    type="checkbox"
                    checked={changesOnly}
                    onChange={(e) => {
                      setReveal(null);
                      setSelected(null);
                      setChangesOnly(e.target.checked);
                    }}
                  />
                  仅变化
                </label>
                <details className="status-stats">
                  <summary>变更统计</summary>
                  <div className="stats-popover">
                    {(["files", "folders"] as const).map((kind) => (
                      <div key={kind}>
                        <b>{kind === "files" ? "文件" : "目录"}</b>
                        {statuses.map((status) => (
                          <span key={status} className={status}>
                            {labels[status]}{" "}
                            {comparison.statuses[kind][status].toLocaleString()}
                          </span>
                        ))}
                      </div>
                    ))}
                  </div>
                </details>
              </section>
              {allowed && (
                <CategoryPanel
                  comparisonId={comparison.comparisonId}
                  metric={metric}
                  mode={mode}
                />
              )}
              {allowed && (
                <DetailsPanel
                  comparisonId={comparison.comparisonId}
                  nodeId={selected}
                  metric={metric}
                />
              )}
            </aside>
            <SplitPane
              axis="vertical"
              label="调整目录树与占用图高度"
              defaultRatio={0.34}
              minFirst={180}
              minSecond={200}
              secondHidden={treemapCollapsed}
              first={
                <SplitPane
                  axis="horizontal"
                  label="调整目录树与扩展名宽度"
                  defaultRatio={0.25}
                  minFirst={300}
                  minSecond={250}
                  secondHidden={extensionsCollapsed}
                  first={
                    <div className="center-pane">
                      {rootReview && (
                        <RootList
                          key={`${comparison.comparisonId}:${rootReview}`}
                          comparisonId={comparison.comparisonId}
                          side={rootReview}
                          totalRoots={comparison[rootReview].rootCount}
                          truncated={comparison[rootReview].rootsTruncated}
                          canNavigate={Boolean(allowed) && !loading}
                          onClose={() => setRootReview(null)}
                          onSelect={(root) => {
                            if (!allowed || loading) return;
                            setRootReview(null);
                            if (root.kind === "file") setSelected(root.nodeId);
                            else if (changesOnly) {
                              setReveal({
                                comparisonId: comparison.comparisonId,
                                path: [],
                                nodeId: root.nodeId,
                              });
                              setChangesOnly(false);
                            } else tree.current?.navigate(root.nodeId, true);
                          }}
                        />
                      )}
                      {!allowed ? (
                        <section className="scope-warning">
                          <h2>请确认导出范围</h2>
                          {comparison.warnings.map((w, i) => (
                            <p key={i}>⚠ {w}</p>
                          ))}
                          <p>
                            这些差异仅来自 CSV 记录。范围不一致或记录不完整时，新增 /
                            删除不一定代表磁盘实际变化。
                          </p>
                          <button
                            className="primary"
                            onClick={() => setConfirmed(comparison.comparisonId)}
                          >
                            我理解范围限制，继续查看
                          </button>
                        </section>
                      ) : (
                        <>
                          <DiffTree
                            ref={tree}
                            comparisonId={comparison.comparisonId}
                            metric={metric}
                            changesOnly={changesOnly}
                            selected={selected}
                            onSelect={setSelected}
                            onScope={setPath}
                            reveal={reveal}
                          />
                        </>
                      )}
                    </div>
                  }
                  second={
                    <aside id="workspace-extensions" className="right-pane">
                      {allowed && (
                        <FileTypePanel
                          comparisonId={comparison.comparisonId}
                          parentId={selected ?? path.at(-1)?.nodeId ?? null}
                          metric={metric}
                          mode={mode}
                          name={
                            selected
                              ? selectedName || "所选项目"
                              : (path.at(-1)?.name ?? "工作区")
                          }
                          showFiles
                        />
                      )}
                    </aside>
                  }
                />
              }
              second={allowed ? (
                <div id="workspace-treemap" className="treemap-panel">
                  <OccupancyCharts
                    active={!treemapCollapsed}
                    metric={metric}
                    mode={mode}
                    selectedId={selected}
                    comparisonId={comparison.comparisonId}
                    onSelect={handleTreemapSelect}
                  />
                </div>
              ) : null}
            />
          </div>
        </>
      )}
    </main>
  );
}
