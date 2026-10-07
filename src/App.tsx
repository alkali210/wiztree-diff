import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "./api";
import { bytes, errorText, signed } from "./format";
import type {
  ChartMode,
  ComparisonSummary,
  Metric,
  SnapshotSide,
  Status,
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
import "./App.css";
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
  return (
    <main>
      <ImportPanel onReady={refresh} />
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
          <div className="workspace">
            <aside className="left-pane">
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
            <aside className="right-pane">
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
            {allowed && (
              <OccupancyCharts
                metric={metric}
                mode={mode}
                onMetricChange={setMetric}
                onModeChange={setMode}
                selectedId={selected}
                comparisonId={comparison.comparisonId}
                onSelect={setSelected}
              />
            )}
          </div>
        </>
      )}
    </main>
  );
}
