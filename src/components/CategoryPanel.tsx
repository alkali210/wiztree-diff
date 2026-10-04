import { useEffect, useState } from "react";
import { api } from "../api";
import { categoryInfo } from "../categories";
import { bytes, errorText } from "../format";
import type { ChartMode, FileCategoriesData, Metric } from "../types";

export function CategoryPanel({
  comparisonId,
  metric,
  mode,
}: {
  comparisonId: string;
  metric: Metric;
  mode: ChartMode;
}) {
  const [result, setResult] = useState<{
      id: string;
      data: FileCategoriesData;
    } | null>(null),
    [error, setError] = useState(""),
    [retry, setRetry] = useState(0);
  useEffect(() => {
    let live = true;
    setResult(null);
    setError("");
    void api.getFileCategories(comparisonId).then(
      (data) => {
        if (live) setResult({ id: comparisonId, data });
      },
      (e) => {
        if (live) setError(errorText(e));
      },
    );
    return () => {
      live = false;
    };
  }, [comparisonId, retry]);
  const data = result?.id === comparisonId ? result.data : null,
    side = mode === "before" ? "before" : "after",
    total = data ? BigInt(data[side][metric]) : 0n;
  return (
    <section className="file-types">
      <h2>文件类型（全部）</h2>
      <div className="type-heading">
        <span>类型</span>
        <span>
          {metric === "size" ? "大小" : "分配"}（
          {side === "before" ? "之前" : "之后"}）
        </span>
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
          {data.items.map((item) => {
            const { name, color } = categoryInfo(item.category),
              v = item[side],
              percent =
                total > 0n
                  ? Number((BigInt(v[metric]) * 10000n) / total) / 100
                  : 0;
            return (
              <div className="type-row" key={item.category}>
                <span
                  className="type-name"
                  title={`${name} · ${v.files.toLocaleString()} 个文件`}
                >
                  <i style={{ background: color }} />
                  {name}
                </span>
                <span className="type-value">
                  <i className="type-bar">
                    <i style={{ width: percent + "%", background: color }} />
                  </i>
                  <span title={v[metric] + " B"}>{bytes(v[metric])}</span>
                </span>
              </div>
            );
          })}
        </div>
      )}
      {data && (
        <p className="type-total" title={data.warnings.join("；")}>
          文件路径合计 {bytes(data[side][metric])}
        </p>
      )}
    </section>
  );
}
