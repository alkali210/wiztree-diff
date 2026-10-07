import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api } from "../api";
import { bytes, errorText } from "../format";
import type { JobSummary } from "../types";
export function ImportPanel({ onReady }: {
  onReady: (comparisonId: string) => void;
}) {
  const [paths, setPaths] = useState(["", ""]);
  const [job, setJob] = useState<JobSummary | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [dropTarget, setDropTarget] = useState<number | null>(null);
  const pickers = useRef<Array<HTMLButtonElement | null>>([]);
  const busyRef = useRef(busy);
  busyRef.current = busy;
  const active = useRef<string | null>(null),
    generation = useRef(0),
    dispose = useRef<(() => void) | null>(null),
    ready = useRef(onReady);
  ready.current = onReady;
  useEffect(
    () => () => {
      generation.current++;
      dispose.current?.();
    },
    [],
  );
  useEffect(() => {
    let live = true;
    let unlisten: UnlistenFn | undefined;
    void getCurrentWebview()
      .onDragDropEvent(({ payload }) => {
        if (!live) return;
        if (payload.type === "leave" || busyRef.current) {
          setDropTarget(null);
          return;
        }
        // Tauri reports physical webview pixels; DOM bounds use CSS pixels.
        const { x, y } = payload.position.toLogical(window.devicePixelRatio);
        const side = pickers.current.findIndex((picker) => {
          if (!picker) return false;
          const rect = picker.getBoundingClientRect();
          return (
            x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
          );
        });
        setDropTarget(payload.type === "drop" || side < 0 ? null : side);
        if (payload.type !== "drop" || side < 0) return;
        if (payload.paths.length !== 1 || !/\.csv$/i.test(payload.paths[0])) {
          setError("请在每个区域一次拖入一个 CSV 文件。");
          return;
        }
        const path = payload.paths[0];
        setPaths((previous) =>
          previous.map((value, i) => (i === side ? path : value)),
        );
        setError("");
      })
      .then(
        (stop) => {
          if (live) unlisten = stop;
          else stop();
        },
        (e) => {
          if (live) setError(errorText(e));
        },
      );
    return () => {
      live = false;
      unlisten?.();
    };
  }, []);
  async function choose(side: number) {
    try {
      const path = await open({
        multiple: false,
        filters: [{ name: "WizTree CSV", extensions: ["csv"] }],
      });
      if (typeof path === "string") {
        setPaths((p) => p.map((v, i) => (i === side ? path : v)));
        setError("");
      }
    } catch (e) {
      setError(errorText(e));
    }
  }
  async function start() {
    busyRef.current = true;
    setDropTarget(null);
    const token = ++generation.current;
    dispose.current?.();
    active.current = null;
    setBusy(true);
    setError("");
    setJob(null);
    let unlisten: UnlistenFn | undefined;
    let timer: ReturnType<typeof setInterval> | undefined;
    let pending = false;
    let finished = false;
    const cleanup = () => {
      unlisten?.();
      if (timer) clearInterval(timer);
    };
    dispose.current = cleanup;
    const accept = (j: JobSummary) => {
      if (
        token !== generation.current ||
        j.jobId !== active.current ||
        finished
      )
        return;
      setJob(j);
      if (j.state !== "running") {
        finished = true;
        cleanup();
        setBusy(false);
        busyRef.current = false;
        if (j.state === "ready" && j.comparisonId) ready.current(j.comparisonId);
        if (j.error) setError(errorText(j.error));
      }
    };
    const poll = async () => {
      if (pending || finished || !active.current) return;
      pending = true;
      try {
        accept(await api.getJob(active.current));
      } catch (e) {
        if (token === generation.current) setError(errorText(e));
      } finally {
        pending = false;
      }
    };
    try {
      unlisten = await listen<JobSummary>("comparison-progress", (e) =>
        accept(e.payload),
      );
      if (token !== generation.current) {
        cleanup();
        return;
      }
      const id = await api.startComparison(paths[0], paths[1]);
      if (token !== generation.current) {
        cleanup();
        return;
      }
      active.current = id;
      await poll();
      if (!finished) timer = setInterval(() => void poll(), 1000);
    } catch (e) {
      cleanup();
      if (token === generation.current) {
        setError(errorText(e));
        setBusy(false);
      }
    }
  }
  return (
    <section className="import-panel">
      <div className="import-inputs">
        {["之前", "之后"].map((label, i) => (
          <button
            key={label}
            aria-label={"选择" + label + " CSV"}
            title="点击选择，或拖入一个 CSV 文件"
            ref={(element) => {
              pickers.current[i] = element;
            }}
            disabled={busy}
            onClick={() => void choose(i)}
            className={
              "path-button " +
              (i ? "after-picker" : "before-picker") +
              (dropTarget === i ? " drop-active" : "")
            }
          >
            <i className="csv-icon" aria-hidden="true">
              ▧
            </i>
            <b>{label} CSV</b>
            <span title={paths[i]}>
              {dropTarget === i
                ? "松开以选择 CSV"
                : paths[i].split(/[\\/]/).pop() || "选择文件或拖入 CSV…"}
            </span>
          </button>
        ))}
        <button
          className="swap-button"
          aria-label="交换之前和之后"
          disabled={busy}
          onClick={() => setPaths(([before, after]) => [after, before])}
        >
          ⇄
        </button>
        <button
          aria-label="开始比较"
          className="primary"
          disabled={busy || !paths.every(Boolean)}
          onClick={() => void start()}
        >
          开始比较
        </button>
        {busy && (
          <button
            aria-label="取消任务"
            disabled={!active.current}
            onClick={() => {
              if (active.current)
                void api
                  .cancelComparison(active.current)
                  .catch((e) => setError(errorText(e)));
            }}
          >
            取消任务
          </button>
        )}
      </div>
      {job && job.state !== "ready" && (
        <div className="progress-line">
          <span>
            {
              {
                before: "导入之前",
                after: "导入之后",
                finalizing: "完成删除、层级与汇总",
              }[job.phase]
            }{" "}
            ·{" "}
            {
              {
                running: "处理中",
                ready: "已完成",
                cancelled: "已取消",
                failed: "失败",
              }[job.state]
            }
          </span>
          <progress
            aria-label="导入进度"
            max={10000}
            value={
              BigInt(job.totalBytes) > 0n
                ? Number(
                    (BigInt(job.bytesRead) * 10000n) / BigInt(job.totalBytes),
                  )
                : 0
            }
          />
          <span>
            {bytes(job.bytesRead)} / {bytes(job.totalBytes)} ·{" "}
            {job.rows.toLocaleString()} 条
          </span>
        </div>
      )}
      {error && <p className="error">{error}</p>}
    </section>
  );
}
