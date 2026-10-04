import { useEffect, useState } from "react";
import { api } from "../api";
import { bytes, errorText } from "../format";
import { extensionColor, extensionLabel } from "../extensions";
import type { ChartMode, ExtensionPage, Metric } from "../types";
export function FileTypePanel({
  comparisonId,
  parentId,
  metric,
  mode,
  name = "全部",
  showFiles = false,
}: {
  comparisonId: string;
  parentId: string | null;
  metric: Metric;
  mode: ChartMode;
  name?: string;
  showFiles?: boolean;
}) {
  const side = mode === "before" ? "before" : "after",
    scope = JSON.stringify([comparisonId, parentId, side, metric]);
  const [position, setPosition] = useState<{
      scope: string;
      cursor: string | null;
      offset: number;
    }>({ scope, cursor: null, offset: 0 }),
    [result, setResult] = useState<{ key: string; data: ExtensionPage } | null>(
      null,
    ),
    [error, setError] = useState(""),
    [retry, setRetry] = useState(0);
  const current =
      position.scope === scope ? position : { scope, cursor: null, offset: 0 },
    key = JSON.stringify([scope, current.cursor]);
  useEffect(() => {
    let live = true;
    setResult(null);
    setError("");
    void api
      .listExtensions(comparisonId, parentId, side, metric, current.cursor)
      .then(
        (data) => {
          if (live) setResult({ key, data });
        },
        (e) => {
          if (live) setError(errorText(e));
        },
      );
    return () => {
      live = false;
    };
  }, [comparisonId, parentId, side, metric, current.cursor, retry]);
  const data = result?.key === key ? result.data : null,
    total = data ? BigInt(data.total[metric]) : 0n;
  return (
    <section className={"file-types " + (showFiles ? "with-files" : "")}>
      <h2 title={name}>扩展名（{name}）</h2>
      <div className="type-heading">
        <span>扩展名</span>
        <span>
          {metric === "size" ? "大小" : "分配"}（
          {side === "before" ? "之前" : "之后"}）
        </span>
        {showFiles && <span>文件</span>}
      </div>
      {error ? (
        <p className="error">
          {error}
          <button onClick={() => setRetry((v) => v + 1)}>重试</button>
        </p>
      ) : !data ? (
        <p className="muted">读取中…</p>
      ) : (
        <div className="extension-rows">
          {data.rows.map((item) => {
            const color = extensionColor(item.extension),
              percent =
                total > 0n
                  ? Number((BigInt(item[metric]) * 10000n) / total) / 100
                  : 0;
            return (
              <div className="type-row" key={item.extension}>
                <span
                  className="type-name"
                  title={extensionLabel(item.extension)}
                >
                  <i style={{ background: color }} />
                  {extensionLabel(item.extension)}
                </span>
                <span className="type-value">
                  <i className="type-bar">
                    <i style={{ width: percent + "%", background: color }} />
                  </i>
                  <span title={item[metric] + " B"}>{bytes(item[metric])}</span>
                </span>
                {showFiles && <span>{item.files.toLocaleString()}</span>}
              </div>
            );
          })}
          {!data.rows.length && <p className="muted">无文件</p>}
        </div>
      )}
      {data && (
        <>
          <p className="type-total" title={data.warnings.join("；")}>
            文件路径合计 {bytes(data.total[metric])}
          </p>
          {(current.offset > 0 || data.nextCursor) && (
            <div className="extension-paging">
              <span>
                {current.offset + 1}–{current.offset + data.rows.length} /{" "}
                {data.totalExtensions}
              </span>
              <button
                disabled={!current.offset}
                onClick={() => setPosition({ scope, cursor: null, offset: 0 })}
              >
                首页
              </button>
              <button
                disabled={!data.nextCursor}
                onClick={() =>
                  setPosition({
                    scope,
                    cursor: data.nextCursor,
                    offset: current.offset + data.rows.length,
                  })
                }
              >
                下一页
              </button>
            </div>
          )}
        </>
      )}
    </section>
  );
}
