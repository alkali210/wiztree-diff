import { useEffect, useState } from "react";
import { api } from "../api";
import { bytes, errorText, signed } from "../format";
import type { Entry, Metric, NodeDetails } from "../types";
const fields: readonly [keyof Entry, string, boolean?][] = [
  ["path", "原始完整路径"],
  ["kind", "类型"],
  ["size", "逻辑大小", true],
  ["allocated", "分配大小", true],
  ["attributes", "属性"],
  ["modified", "修改时间"],
  ["created", "创建时间"],
  ["accessed", "访问时间"],
  ["mft", "MFT 记录号"],
  ["parentMft", "父 MFT"],
  ["files", "导出文件数"],
  ["folders", "导出目录数"],
  ["directSize", "直接文件大小", true],
  ["directAllocated", "直接文件分配", true],
  ["driveCapacity", "驱动器容量", true],
  ["freeSpace", "可用空间", true],
  ["usedSpace", "已用空间", true],
  ["reservedSpace", "保留空间", true],
  ["hardlinkCount", "同卷关联文件路径数"],
];
export function DetailsPanel({
  comparisonId,
  nodeId,
  metric = "size",
}: {
  comparisonId: string;
  nodeId: string | null;
  metric?: Metric;
}) {
  const [expanded, setExpanded] = useState(false);
  const [result, setResult] = useState<{
      id: string;
      comparison: string;
      data: NodeDetails;
    } | null>(null),
    [error, setError] = useState("");
  useEffect(() => {
    let live = true;
    setExpanded(false);
    setResult(null);
    setError("");
    if (nodeId)
      void api.getDetails(comparisonId, nodeId).then(
        (data) => {
          if (live) setResult({ id: nodeId, comparison: comparisonId, data });
        },
        (e) => {
          if (live) setError(errorText(e));
        },
      );
    return () => {
      live = false;
    };
  }, [comparisonId, nodeId]);
  const data =
    result?.id === nodeId && result.comparison === comparisonId
      ? result.data
      : null;
  return (
    <section className="details-panel">
      <h2>所选项目</h2>
      {!nodeId ? (
        <p className="empty">未选择</p>
      ) : error ? (
        <p className="error">{error}</p>
      ) : !data ? (
        <p className="empty">读取详情…</p>
      ) : (
        <>
          <p className="selected-name">
            {(data.after ?? data.before)?.path
              .replace(/[\\/]+$/, "")
              .split(/[\\/]/)
              .pop()}
          </p>
          <p
            className="selected-path"
            title={(data.after ?? data.before)?.path}
          >
            {(data.after ?? data.before)?.path}
          </p>
          <dl className="selected-summary">
            <dt>{metric === "size" ? "大小" : "分配"}（之前）</dt>
            <dd>{bytes(data.before?.[metric])}</dd>
            <dt>{metric === "size" ? "大小" : "分配"}（之后）</dt>
            <dd>{bytes(data.after?.[metric])}</dd>
            <dt>Δ {metric === "size" ? "大小" : "分配"}</dt>
            <dd
              className={
                BigInt(
                  metric === "size" ? data.sizeDelta : data.allocatedDelta,
                ) > 0n
                  ? "positive"
                  : BigInt(
                        metric === "size"
                          ? data.sizeDelta
                          : data.allocatedDelta,
                      ) < 0n
                    ? "negative"
                    : ""
              }
            >
              {signed(metric === "size" ? data.sizeDelta : data.allocatedDelta)}
            </dd>
            <dt>文件</dt>
            <dd>{(data.after ?? data.before)?.files ?? "—"}</dd>
            <dt>目录</dt>
            <dd>{(data.after ?? data.before)?.folders ?? "—"}</dd>
            <dt>修改时间</dt>
            <dd>{(data.after ?? data.before)?.modified ?? "—"}</dd>
            <dt>属性</dt>
            <dd>{(data.after ?? data.before)?.attributes ?? "—"}</dd>
          </dl>
          <details
            className="export-details"
            onToggle={(event) => setExpanded(event.currentTarget.open)}
          >
            <summary>完整导出字段</summary>
            {expanded && (
              <div className="export-popover">
                <table>
                  <thead>
                    <tr>
                      <th>字段</th>
                      <th>之前</th>
                      <th>之后</th>
                    </tr>
                  </thead>
                  <tbody>
                    {fields.map(([key, label, byte]) => (
                      <tr key={key}>
                        <th>{label}</th>
                        {[data.before, data.after].map((entry, i) => {
                          const value = entry?.[key];
                          return (
                            <td
                              key={i}
                              title={value == null ? undefined : String(value)}
                            >
                              {!entry ? (
                                "—（该侧不存在）"
                              ) : value == null ? (
                                "未导出"
                              ) : byte ? (
                                <>
                                  {bytes(String(value))}
                                  <small className="raw-bytes">
                                    {String(value)} B
                                  </small>
                                </>
                              ) : key === "kind" ? (
                                value === "directory" ? (
                                  "目录"
                                ) : (
                                  "文件"
                                )
                              ) : (
                                String(value)
                              )}
                            </td>
                          );
                        })}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </details>
        </>
      )}
    </section>
  );
}
