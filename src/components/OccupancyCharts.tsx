import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { bytes, errorText, signed } from "../format";
import type {
  ChartMode,
  FullTreemapData,
  Metric,
  TreemapHit,
  TreemapRect,
} from "../types";
interface Props {
  comparisonId: string;
  metric: Metric;
  mode: ChartMode;
  onMetricChange: (v: Metric) => void;
  onModeChange: (v: ChartMode) => void;
  selectedId: string | null;
  onSelect: (id: string) => void;
}
function fitCaption(
  ctx: CanvasRenderingContext2D,
  name: string,
  suffix: string,
  width: number,
) {
  if (ctx.measureText(name + suffix).width <= width) return name + suffix;
  const tail = ctx.measureText("…" + suffix).width <= width ? suffix : "";
  let low = 0,
    high = name.length;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    if (ctx.measureText(name.slice(0, middle) + "…" + tail).width <= width)
      low = middle;
    else high = middle - 1;
  }
  // Do not cut a supplementary Unicode character between its surrogate pair.
  const last = name.charCodeAt(low - 1);
  if (last >= 0xd800 && last <= 0xdbff) low--;
  return name.slice(0, low) + "…" + tail;
}
export function OccupancyCharts({
  comparisonId,
  metric,
  mode,
  onMetricChange,
  onModeChange,
  selectedId,
  onSelect,
}: Props) {
  const [maxDepth, setMaxDepth] = useState(0);
  const [depthInput, setDepthInput] = useState(String(maxDepth));
  const key = JSON.stringify([comparisonId, metric, mode, maxDepth]);
  const frameCache = useRef({
    comparisonId,
    maxDepth,
    frames: new Map<string, FullTreemapData>(),
  });
  if (
    frameCache.current.comparisonId !== comparisonId ||
    frameCache.current.maxDepth !== maxDepth
  ) {
    frameCache.current = { comparisonId, maxDepth, frames: new Map() };
  }
  const [frame, setFrame] = useState<{
      key: string;
      data: FullTreemapData;
    } | null>(null),
    [error, setError] = useState(""),
    [retry, setRetry] = useState(0),
    [bounds, setBounds] = useState<{
      key: string;
      nodeId: string;
      rect: TreemapRect | null;
    } | null>(null),
    [hover, setHover] = useState<{
      key: string;
      hit: TreemapHit;
      x: number;
      y: number;
    } | null>(null);
  const canvas = useRef<HTMLCanvasElement>(null),
    image = useRef<HTMLImageElement | null>(null),
    pointerSeq = useRef(0),
    clickSeq = useRef(0),
    pending = useRef<ReturnType<typeof setTimeout> | null>(null);
  const data = frame?.key === key ? frame.data : null,
    highlight =
      bounds?.key === key && bounds.nodeId === selectedId ? bounds.rect : null;
  useEffect(() => {
    let live = true;
    const cache = frameCache.current;
    const cached = cache.frames.get(key);
    setFrame(cached ? { key, data: cached } : null);
    setError("");
    setHover(null);
    pointerSeq.current++;
    clickSeq.current++;
    if (!cached)
      void api.getFullTreemap(comparisonId, metric, mode, maxDepth).then(
        (result) => {
          // Keep only the six metric/mode frames for this comparison and depth.
          // An old response may finish after switching mode, but cannot enter a
          // new comparison/depth cache or replace the currently displayed frame.
          if (frameCache.current === cache) cache.frames.set(key, result);
          if (live) setFrame({ key, data: result });
        },
        (e) => {
          if (live) setError(errorText(e));
        },
      );
    return () => {
      live = false;
      clickSeq.current++;
      pointerSeq.current++;
      if (pending.current) clearTimeout(pending.current);
    };
  }, [comparisonId, metric, mode, maxDepth, retry]);
  useEffect(() => {
    let live = true;
    setBounds(null);
    if (selectedId)
      void api
        .getTreemapBounds(comparisonId, metric, mode, selectedId, maxDepth)
        .then(
          (rect) => {
            if (live) setBounds({ key, nodeId: selectedId, rect });
          },
          (e) => {
            if (live) setError(errorText(e));
          },
        );
    return () => {
      live = false;
    };
  }, [comparisonId, metric, mode, maxDepth, selectedId]);
  useEffect(() => {
    if (!data) return;
    const element = canvas.current!;
    let live = true;
    const bitmap = new Image();
    image.current = bitmap;
    const draw = () => {
      if (!live || !bitmap.complete || !bitmap.naturalWidth) return;
      const b = element.getBoundingClientRect(),
        dpr = window.devicePixelRatio || 1;
      element.width = Math.round(b.width * dpr);
      element.height = Math.round(b.height * dpr);
      const ctx = element.getContext("2d");
      if (!ctx) return;
      ctx.scale(dpr, dpr);
      ctx.clearRect(0, 0, b.width, b.height);
      ctx.drawImage(bitmap, 0, 0, b.width, b.height);
      for (const label of data.labels) {
        const x = label.x * b.width,
          y = label.y * b.height,
          w = label.width * b.width,
          h = label.height * b.height;
        const directory = label.kind === "directory";
        if (w < 65 || h < (directory ? 10 : 25)) continue;
        ctx.save();
        ctx.beginPath();
        ctx.rect(x, y, w, h);
        ctx.clip();
        ctx.font = `${directory ? Math.min(11, h - 2) : 11}px Segoe UI, Microsoft YaHei, sans-serif`;
        const text = fitCaption(
          ctx,
          label.name,
          directory ? ` (${bytes(label.weight)})` : "",
          w - 8,
        );
        const baseline = directory
          ? y + h / 2 + Math.min(11, h - 2) * 0.35
          : y + 14;
        ctx.lineWidth = directory ? 2 : 3;
        ctx.strokeStyle = "rgba(0,0,0,.65)";
        ctx.fillStyle = directory ? "#e1e3e5" : "#eef2f4";
        ctx.strokeText(text, x + 4, baseline);
        ctx.fillText(text, x + 4, baseline);
        ctx.restore();
      }
      if (highlight) {
        ctx.strokeStyle = "#ffdf49";
        ctx.lineWidth = 2;
        ctx.strokeRect(
          highlight.x * b.width + 1,
          highlight.y * b.height + 1,
          Math.max(0, highlight.width * b.width - 2),
          Math.max(0, highlight.height * b.height - 2),
        );
      }
    };
    bitmap.onload = draw;
    bitmap.onerror = () => {
      if (live) setError("占用图图像无法读取");
    };
    bitmap.src = data.imageDataUrl;
    const observer = new ResizeObserver(draw);
    observer.observe(element);
    draw();
    return () => {
      live = false;
      observer.disconnect();
      bitmap.onload = null;
      bitmap.onerror = null;
      image.current = null;
    };
  }, [data, highlight]);
  function point(event: React.MouseEvent<HTMLCanvasElement>) {
    const b = event.currentTarget.getBoundingClientRect();
    return {
      x: Math.min(0.999999999, Math.max(0, (event.clientX - b.left) / b.width)),
      y: Math.min(0.999999999, Math.max(0, (event.clientY - b.top) / b.height)),
      px: event.clientX - b.left,
      py: event.clientY - b.top,
    };
  }
  function move(event: React.MouseEvent<HTMLCanvasElement>) {
    const p = point(event),
      seq = ++pointerSeq.current;
    if (pending.current) clearTimeout(pending.current);
    pending.current = setTimeout(() => {
      void api
        .hitTestTreemap(comparisonId, metric, mode, p.x, p.y, maxDepth)
        .then(
          (hit) => {
            if (seq === pointerSeq.current)
              setHover(hit ? { key, hit, x: p.px, y: p.py } : null);
          },
          (e) => {
            if (seq === pointerSeq.current) setError(errorText(e));
          },
        );
    }, 65);
  }
  function click(event: React.MouseEvent<HTMLCanvasElement>) {
    const p = point(event),
      seq = ++clickSeq.current;
    void api
      .hitTestTreemap(comparisonId, metric, mode, p.x, p.y, maxDepth)
      .then(
        (hit) => {
          if (seq === clickSeq.current && hit) onSelect(hit.nodeId);
        },
        (e) => {
          if (seq === clickSeq.current) setError(errorText(e));
        },
      );
  }
  const shownHover = hover?.key === key ? hover : null;
  return (
    <section className="charts-panel">
      <header className="charts-heading">
        <span>全部文件</span>
        <div className="chart-controls">
          <form
            className="treemap-depth"
            onSubmit={(e) => {
              e.preventDefault();
              const value = Number(depthInput);
              if (
                !depthInput.trim() ||
                !Number.isInteger(value) ||
                value < 0 ||
                value > 4294967295
              )
                return;
              setMaxDepth(value);
            }}
          >
            <label title="导出根目录为第 1 层；达到上限的目录合并成块，0 为无限制">
              最大目录深度
              <input
                aria-label="最大目录深度"
                type="number"
                min="0"
                max="4294967295"
                step="1"
                required
                value={depthInput}
                onChange={(e) => setDepthInput(e.target.value)}
              />
            </label>
            <button type="submit" disabled={depthInput === String(maxDepth)}>
              应用
            </button>
            <small>0 = 无限制</small>
          </form>
          <div className="segmented">
            {(["before", "after", "delta"] as const).map((m, i) => (
              <button
                key={m}
                aria-label={["之前占用图", "之后占用图", "差异占用图"][i]}
                aria-pressed={mode === m}
                className={mode === m ? "active" : ""}
                onClick={() => onModeChange(m)}
              >
                {["之前", "之后", "差异"][i]}
              </button>
            ))}
          </div>
          <div className="segmented">
            <button
              aria-label="逻辑大小图表"
              aria-pressed={metric === "size"}
              className={metric === "size" ? "active" : ""}
              onClick={() => onMetricChange("size")}
            >
              大小
            </button>
            <button
              aria-label="分配大小图表"
              aria-pressed={metric === "allocated"}
              className={metric === "allocated" ? "active" : ""}
              onClick={() => onMetricChange("allocated")}
            >
              分配
            </button>
          </div>
        </div>
      </header>
      {error ? (
        <div className="empty error">
          {error}
          <button onClick={() => setRetry((v) => v + 1)}>重试</button>
        </div>
      ) : !data ? (
        <div className="empty">读取中…</div>
      ) : (
        <div className="chart-viewport global-treemap">
          <canvas
            ref={canvas}
            data-chart-mode={mode}
            data-map-scope="all-files"
            data-max-depth={maxDepth}
            aria-label="全部文件层级占用图"
            onMouseMove={move}
            onMouseLeave={() => {
              pointerSeq.current++;
              if (pending.current) clearTimeout(pending.current);
              setHover(null);
            }}
            onClick={click}
          />
          {BigInt(data.weightTotal) === 0n && (
            <div className="canvas-empty">
              {mode === "delta"
                ? "之前无非零占用；新增项目见之后视图"
                : "无非零占用"}
            </div>
          )}
          {shownHover && (
            <aside
              className="treemap-tooltip"
              style={{
                left: Math.max(
                  4,
                  Math.min(
                    shownHover.x + 12,
                    (canvas.current?.clientWidth || 300) - 290,
                  ),
                ),
                top: Math.max(
                  4,
                  Math.min(
                    shownHover.y + 14,
                    (canvas.current?.clientHeight || 100) - 85,
                  ),
                ),
              }}
            >
              <b>{shownHover.hit.path}</b>
              <span>
                {mode === "delta"
                  ? signed(shownHover.hit.value)
                  : bytes(shownHover.hit.value)}
                {mode === "delta" && (
                  <> · 之前 {bytes(shownHover.hit.weight)}</>
                )}
              </span>
              <small>
                {shownHover.hit.value} B ·{" "}
                {shownHover.hit.kind === "directory"
                  ? shownHover.hit.collapsed
                    ? "目录汇总（已达深度上限）"
                    : "目录汇总"
                  : shownHover.hit.extension || "(无扩展名)"}
              </small>
            </aside>
          )}
        </div>
      )}
      {data && (
        <div className="chart-status">
          <span>{data.fileCount.toLocaleString()} 文件</span>
          <span>
            {data.renderedBlockCount.toLocaleString()} 图块 · 深度{" "}
            {maxDepth === 0 ? "无限制" : maxDepth}
          </span>
          <span title={data.weightTotal + " B"}>
            {mode === "delta" ? "之前文件路径合计" : "文件路径合计"}{" "}
            {bytes(data.weightTotal)}
          </span>
          {mode === "delta" && (
            <>
              <span className="scope-note">之前布局 · 仅差异块</span>
              {data.addedFileCount > 0 && (
                <span title="新增文件没有之前面积；通过祖先边框提示，完整新增项目可在之后视图或目录树查看">
                  新增 {data.addedFileCount.toLocaleString()} 文件（见之后视图）
                </span>
              )}
              <span className="positive">
                增长 +{bytes(data.positiveTotal)}
              </span>
              <span className="negative">
                减少 −{bytes(data.negativeTotal)}
              </span>
              <span>净值 {signed(data.netDelta)}</span>
            </>
          )}
          {metric === "allocated" && (
            <span className="scope-note" title="按路径分配值，不按 MFT 去重">
              非去重物理空间
            </span>
          )}
          {data.warnings.length > 0 && (
            <details className="chart-warnings">
              <summary>范围提示</summary>
              <div>
                {data.warnings.map((w, i) => (
                  <p key={i}>{w}</p>
                ))}
              </div>
            </details>
          )}
        </div>
      )}
    </section>
  );
}
