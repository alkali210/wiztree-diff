import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { bytes, errorText } from "../format";
import type { Root, RootPage, SnapshotSide } from "../types";

interface Props {
  comparisonId: string;
  side: SnapshotSide;
  totalRoots: number;
  truncated: boolean;
  canNavigate: boolean;
  onClose: () => void;
  onSelect: (root: Root) => void;
}

export function RootList({
  comparisonId,
  side,
  totalRoots,
  truncated,
  canNavigate,
  onClose,
  onSelect,
}: Props) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [position, setPosition] = useState<{
    cursor: string | null;
    offset: number;
  }>({ cursor: null, offset: 0 });
  const [page, setPage] = useState<RootPage | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [retry, setRetry] = useState(0);
  const role = side === "before" ? "之前" : "之后";
  useEffect(() => {
    const previousFocus = document.activeElement;
    dialog.current?.showModal();
    return () => {
      dialog.current?.close();
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected)
        previousFocus.focus();
    };
  }, []);
  useEffect(() => {
    let active = true;
    setPage(null);
    setError("");
    setLoading(true);
    void api
      .listRoots(comparisonId, side, position.cursor)
      .then(
        (result) => {
          if (active) setPage(result);
        },
        (reason) => {
          if (active) setError(errorText(reason));
        },
      )
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [comparisonId, side, position, retry]);
  const total = page?.totalRoots ?? totalRoots;
  return (
    <dialog
      ref={dialog}
      className="root-dialog"
      aria-labelledby="root-list-title"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
    >
      <header className="root-list-heading">
        <h2 id="root-list-title">{role} · 导出根</h2>
        <button
          type="button"
          autoFocus
          aria-label="关闭导出根列表"
          onClick={onClose}
        >
          关闭
        </button>
      </header>
      <p className="muted" aria-live="polite">
        {page
          ? page.rows.length
            ? `${position.offset + 1}–${position.offset + page.rows.length}`
            : "0"
          : "—"}{" "}
        / {total.toLocaleString()} 个导出根 · 每页最多 200 条
      </p>
      {truncated && (
        <p className="warning">
          摘要预览仅保留前 200
          个根；此列表按页读取完整导出根，不代表额外磁盘扫描。
        </p>
      )}
      {!canNavigate && (
        <p className="warning">
          可核对导出根；确认范围限制后才能定位目录或查看文件详情。
        </p>
      )}
      {loading && <p role="status">正在读取导出根…</p>}
      {error && (
        <div className="error" role="alert">
          {error}{" "}
          <button type="button" onClick={() => setRetry((value) => value + 1)}>
            重试读取导出根
          </button>
        </div>
      )}
      <div className="root-list-scroll" aria-busy={loading}>
        <table className="root-list-table">
          <thead>
            <tr>
              <th scope="col">原始路径</th>
              <th scope="col">类型</th>
              <th scope="col">逻辑大小</th>
              <th scope="col">分配大小</th>
            </tr>
          </thead>
          <tbody>
            {page?.rows.map((root) => (
              <tr key={root.nodeId}>
                <td>
                  <button
                    type="button"
                    disabled={!canNavigate}
                    title={root.path}
                    aria-label={`${root.kind === "directory" ? "定位目录" : "查看文件详情"} ${root.path}`}
                    onClick={() => onSelect(root)}
                  >
                    {root.path}
                  </button>
                </td>
                <td title={root.kind}>
                  {root.kind === "directory" ? "目录" : "文件"}
                </td>
                <td title={`${root.size} B`}>{bytes(root.size)}</td>
                <td title={`${root.allocated} B`}>{bytes(root.allocated)}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {page && !page.rows.length && <p className="muted">没有导出根。</p>}
      </div>
      <nav className="root-list-paging" aria-label="导出根分页">
        <button
          type="button"
          disabled={loading || position.cursor === null}
          onClick={() => setPosition({ cursor: null, offset: 0 })}
        >
          第一页
        </button>
        <button
          type="button"
          disabled={loading || !page?.nextCursor}
          onClick={() => {
            if (page?.nextCursor)
              setPosition({
                cursor: page.nextCursor,
                offset: position.offset + page.rows.length,
              });
          }}
        >
          下一页
        </button>
      </nav>
    </dialog>
  );
}
