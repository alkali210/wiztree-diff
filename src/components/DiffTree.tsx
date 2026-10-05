import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { api } from "../api";
import { bytes, errorText, signed } from "../format";
import type { ChildRow, Metric } from "../types";
export interface Breadcrumb {
  nodeId: string;
  name: string;
}
export interface TreeHandle {
  navigate: (nodeId: string, expandable: boolean) => void;
}
interface Props {
  comparisonId: string;
  metric: Metric;
  changesOnly: boolean;
  selected: string | null;
  onSelect: (id: string) => void;
  onScope: (path: Breadcrumb[]) => void;
  reveal?: { comparisonId: string; path: Breadcrumb[]; nodeId: string } | null;
}
interface Page {
  rows: ChildRow[];
  // Positions exist only for retained rows; earlier pages reload from the first
  // cursor without keeping unbounded cursor history or detached tree records.
  positions: Map<string, number>;
  through: number;
  retryMore: boolean;
  cursor: string | null;
  total: number;
  loading: boolean;
  error: string;
  stamp: number;
}
type Visible =
  | { row: ChildRow; depth: number; parent: string }
  | { parent: string; depth: number; action: true; gap?: [number, number] };
const CACHE_LIMIT = 10000;
function visibleKey(v: Visible) {
  return "row" in v
    ? v.row.nodeId
    : `action:${v.parent}:${v.gap?.[0] ?? "next"}`;
}
export const DiffTree = forwardRef<TreeHandle, Props>(function DiffTree(
  { comparisonId, metric, changesOnly, selected, onSelect, onScope, reveal },
  ref,
) {
  const pages = useRef(new Map<string, Page>()),
    expanded = useRef(new Set<string>()),
    epoch = useRef(0),
    clock = useRef(0),
    scope = useRef<Breadcrumb[]>([]),
    scroll = useRef<HTMLDivElement>(null);
  const navigation = useRef(0),
    currentSelected = useRef(selected),
    viewport = useRef<Visible[]>([]),
    currentVisible = useRef<Visible[]>([]),
    anchor = useRef<{ key: string; offset: number } | null>(null);
  currentSelected.current = selected;
  const [, render] = useState(0),
    [hint, setHint] = useState("");
  const redraw = () => render((v) => v + 1);
  function find(id: string) {
    for (const p of pages.current.values()) {
      const r = p.rows.find((r) => r.nodeId === id);
      if (r) return r;
    }
  }
  function ancestors(id: string): Breadcrumb[] {
    for (const [parent, p] of pages.current) {
      const r = p.rows.find((r) => r.nodeId === id);
      if (r)
        return [
          ...(parent ? ancestors(parent) : []),
          { nodeId: id, name: r.name },
        ];
    }
    return [];
  }
  function drop(id: string) {
    const p = pages.current.get(id);
    if (p) for (const r of p.rows) drop(r.nodeId);
    pages.current.delete(id);
    expanded.current.delete(id);
  }
  function collapse(id: string, invalidate = true) {
    if (invalidate) navigation.current++;
    drop(id);
    const index = scope.current.findIndex((x) => x.nodeId === id);
    if (index >= 0) {
      scope.current = scope.current.slice(0, index);
      onScope(scope.current);
    }
    redraw();
  }
  function preserveAnchor() {
    const first = viewport.current.find((v) => {
      const index = currentVisible.current.indexOf(v);
      return index * 28 + 28 > (scroll.current?.scrollTop ?? 0);
    });
    if (first)
      anchor.current = {
        key: visibleKey(first),
        offset:
          (scroll.current?.scrollTop ?? 0) -
          currentVisible.current.indexOf(first) * 28,
      };
  }
  function protectedRows(parent: string) {
    const ids = new Set<string>();
    const protect = (id: string) => {
      ids.add(id);
      for (const x of ancestors(id)) ids.add(x.nodeId);
    };
    protect(parent);
    if (currentSelected.current) protect(currentSelected.current);
    for (const x of scope.current) protect(x.nodeId);
    for (const v of viewport.current)
      protect("row" in v ? v.row.nodeId : v.parent);
    return ids;
  }
  function makeRoom(add: number, parent: string, incoming: Set<string>) {
    let count = [...pages.current.values()].reduce(
      (n, p) => n + p.rows.length,
      0,
    );
    if (count + add <= CACHE_LIMIT) return true;
    preserveAnchor();
    const protectedIds = protectedRows(parent);
    for (const id of incoming) protectedIds.add(id);
    const oldest = [...pages.current].sort((a, b) => a[1].stamp - b[1].stamp);
    // First release old directory subtrees, except visible/selected/scope paths.
    for (const [id] of oldest) {
      if (count + add <= CACHE_LIMIT) break;
      if (
        id &&
        id !== parent &&
        !protectedIds.has(id) &&
        pages.current.has(id)
      ) {
        const before = [...pages.current.values()].reduce(
          (n, p) => n + p.rows.length,
          0,
        );
        collapse(id, false);
        count -=
          before -
          [...pages.current.values()].reduce((n, p) => n + p.rows.length, 0);
        setHint("缓存达到上限，已收起较久未交互的目录，可重新展开。");
      }
    }
    // A wide single directory cannot be collapsed to make room for itself.
    // Slide its cached sibling pages instead, retaining every protected row and
    // expanded directory link. Position metadata disappears with evicted rows.
    for (const [id, p] of oldest) {
      if (count + add <= CACHE_LIMIT) break;
      if (pages.current.get(id) !== p) continue;
      const retained: ChildRow[] = [];
      for (const row of p.rows) {
        if (
          count + add > CACHE_LIMIT &&
          !protectedIds.has(row.nodeId) &&
          !expanded.current.has(row.nodeId)
        ) {
          p.positions.delete(row.nodeId);
          count--;
        } else retained.push(row);
      }
      p.rows = retained;
    }
    return count + add <= CACHE_LIMIT;
  }
  async function load(parent: string, more = false) {
    const generation = epoch.current;
    let p = pages.current.get(parent);
    if (p?.loading || (more && p && !p.cursor)) return false;
    if (!p) {
      p = {
        rows: [],
        positions: new Map(),
        through: 0,
        retryMore: false,
        cursor: null,
        total: 0,
        loading: false,
        error: "",
        stamp: ++clock.current,
      };
      pages.current.set(parent, p);
    }
    const own = p;
    const start = more ? own.through : 0;
    own.loading = true;
    own.retryMore = more;
    own.error = "";
    own.stamp = ++clock.current;
    redraw();
    try {
      const result = await api.listChildren(
        comparisonId,
        parent || null,
        changesOnly,
        more ? own.cursor : null,
      );
      if (
        generation !== epoch.current ||
        pages.current.get(parent) !== own ||
        (parent && !expanded.current.has(parent))
      )
        return false;
      const incoming = new Set(result.rows.map((row) => row.nodeId));
      const add = result.rows.filter(
        (row) => !own.positions.has(row.nodeId),
      ).length;
      if (!makeRoom(add, parent, incoming)) {
        own.error = "可释放的非可见缓存不足，请收起目录后重试。";
        return false;
      }
      const existing = new Set(own.rows.map((row) => row.nodeId));
      result.rows.forEach((row, index) => {
        own.positions.set(row.nodeId, start + index);
        if (!existing.has(row.nodeId)) own.rows.push(row);
      });
      own.rows.sort(
        (a, b) => own.positions.get(a.nodeId)! - own.positions.get(b.nodeId)!,
      );
      own.through = start + result.rows.length;
      own.cursor = result.nextCursor;
      own.total = result.totalChildren;
      return true;
    } catch (e) {
      if (
        generation === epoch.current &&
        pages.current.get(parent) === own
      )
        own.error = errorText(e);
      return false;
    } finally {
      if (generation === epoch.current && pages.current.get(parent) === own) {
        own.loading = false;
        redraw();
      }
    }
  }
  function select(id: string) {
    navigation.current++;
    currentSelected.current = id;
    onSelect(id);
    const p = pages.current.get(id);
    if (expanded.current.has(id) && p && !p.error) {
      p.stamp = ++clock.current;
      scope.current = ancestors(id);
      onScope(scope.current);
    }
  }
  async function retry(parent: string) {
    const generation = epoch.current,
      nav = navigation.current;
    const more = pages.current.get(parent)?.retryMore ?? false;
    if (
      (await load(parent, more)) &&
      generation === epoch.current &&
      nav === navigation.current &&
      parent &&
      expanded.current.has(parent)
    ) {
      scope.current = ancestors(parent);
      onScope(scope.current);
    }
  }
  async function navigate(id: string, expandable: boolean) {
    currentSelected.current = id;
    onSelect(id);
    const generation = epoch.current,
      nav = ++navigation.current;
    if (!expandable) return;
    const live = () =>
      generation === epoch.current && nav === navigation.current;
    async function expand(node: string) {
      if (!find(node)?.expandable || !live()) return false;
      expanded.current.add(node);
      const existing = pages.current.get(node);
      if (existing) existing.stamp = ++clock.current;
      redraw();
      if (!existing || existing.error) {
        if (!(await load(node, false))) return false;
      }
      return live() && expanded.current.has(node);
    }
    async function locate(node: string, parent: string) {
      let page = pages.current.get(parent);
      if (!page) {
        if (!(await load(parent, false))) return false;
        page = pages.current.get(parent);
      }
      if (!page || page.loading || !live()) return false;
      // Evicted pages can contain the target: restart once, then seek forward.
      if (!find(node) && page.rows.length < page.through) {
        if (!(await load(parent, false))) return false;
      }
      while (!find(node)) {
        const current = pages.current.get(parent);
        if (!current?.cursor || !live()) {
          if (live())
            setHint("此目录不在当前过滤结果中，请关闭“仅变化”后定位。");
          return false;
        }
        if (!(await load(parent, true)) || !live()) return false;
      }
      return true;
    }
    try {
      // A root in one export can have a shared-tree parent present only in the
      // other export. Follow actual parent IDs, not guesses based on chart scope.
      const chain: { node: string; parent: string | null }[] = [];
      let cursor: string | null = id;
      while (cursor && !find(cursor)) {
        const details = await api.getDetails(comparisonId, cursor);
        if (!live()) return;
        chain.push({ node: cursor, parent: details.parentId });
        cursor = details.parentId;
      }
      if (cursor && chain.length && !(await expand(cursor))) return;
      for (const step of chain.reverse()) {
        if (
          !(await locate(step.node, step.parent || "")) ||
          !(await expand(step.node))
        )
          return;
      }
      if (!chain.length && !(await expand(id))) return;
    } catch (error) {
      if (live()) setHint(errorText(error));
      return;
    }
    if (!live()) return;
    scope.current = ancestors(id);
    onScope(scope.current);
    redraw();
  }
  async function reloadFirst(parent: string) {
    const nav = ++navigation.current;
    if (!(await load(parent, false)) || nav !== navigation.current) return;
    const first = pages.current
      .get(parent)
      ?.rows.find(
        (row) => pages.current.get(parent)?.positions.get(row.nodeId) === 0,
      );
    if (first) {
      anchor.current = { key: first.nodeId, offset: 0 };
      redraw();
    }
  }
  useImperativeHandle(ref, () => ({
    navigate: (id, dir) => {
      void navigate(id, dir);
    },
  }));
  useEffect(() => {
    epoch.current++;
    const generation = epoch.current;
    navigation.current++;
    anchor.current = null;
    viewport.current = [];
    pages.current.clear();
    expanded.current.clear();
    scope.current = [];
    onScope([]);
    setHint("");
    void (async () => {
      if (!(await load(""))) return;
      if (!changesOnly && reveal?.comparisonId === comparisonId) {
        setHint("已关闭“仅变化”，以定位占用图中的目录。");
        for (const p of reveal.path) {
          if (generation !== epoch.current) return;
          await navigate(p.nodeId, true);
          if (scope.current.at(-1)?.nodeId !== p.nodeId) return;
        }
        if (generation === epoch.current) await navigate(reveal.nodeId, true);
      }
    })();
    return () => {
      epoch.current++;
    };
  }, [comparisonId, changesOnly]);
  const visible: Visible[] = [];
  function flatten(parent: string, depth: number) {
    const p = pages.current.get(parent);
    if (!p) return;
    let next = 0;
    for (const row of p.rows) {
      const position = p.positions.get(row.nodeId)!;
      if (position > next)
        visible.push({
          parent,
          depth,
          action: true,
          gap: [next + 1, position],
        });
      visible.push({ row, depth, parent });
      if (expanded.current.has(row.nodeId)) flatten(row.nodeId, depth + 1);
      next = position + 1;
    }
    if (next < p.through)
      visible.push({ parent, depth, action: true, gap: [next + 1, p.through] });
    if (p.loading || p.error || p.cursor || !p.rows.length)
      visible.push({ parent, depth, action: true });
  }
  flatten("", 0);
  currentVisible.current = visible;
  const virtual = useVirtualizer({
    count: visible.length,
    getItemKey: (index) => visibleKey(visible[index]),
    getScrollElement: () => scroll.current,
    estimateSize: () => 28,
    overscan: 8,
  });
  const items = virtual.getVirtualItems();
  viewport.current = items.map((item) => visible[item.index]);
  useLayoutEffect(() => {
    const saved = anchor.current;
    if (!saved) return;
    anchor.current = null;
    const generation = epoch.current,
      nav = navigation.current;
    // The virtualizer synchronously notifies React on scroll. Restore outside
    // React’s layout lifecycle, and discard an anchor from a superseded view.
    queueMicrotask(() => {
      if (generation !== epoch.current || nav !== navigation.current) return;
      const index = currentVisible.current.findIndex(
        (v) => visibleKey(v) === saved.key,
      );
      if (index >= 0) virtual.scrollToOffset(index * 28 + saved.offset);
    });
  });
  function key(event: React.KeyboardEvent) {
    if (event.target instanceof HTMLButtonElement) return;
    const nodes = visible.filter(
      (v): v is Extract<Visible, { row: ChildRow }> => "row" in v,
    );
    const index = nodes.findIndex((v) => v.row.nodeId === selected),
      node = nodes[index];
    if (
      ["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Enter"].includes(
        event.key,
      )
    )
      event.preventDefault();
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      const next =
        nodes[
          Math.max(
            0,
            Math.min(
              nodes.length - 1,
              index + (event.key === "ArrowDown" ? 1 : -1),
            ),
          )
        ];
      if (next) {
        select(next.row.nodeId);
        virtual.scrollToIndex(visible.indexOf(next));
      }
    } else if (node) {
      if (event.key === "ArrowRight" && node.row.expandable)
        void navigate(node.row.nodeId, true);
      if (event.key === "ArrowLeft") {
        if (expanded.current.has(node.row.nodeId)) collapse(node.row.nodeId);
        else if (node.parent) select(node.parent);
      }
      if (event.key === "Enter") select(node.row.nodeId);
    }
  }
  const labels = {
    added: "＋ 新增",
    removed: "− 删除",
    modified: "↕ 修改",
    unchanged: "＝ 不变",
    typeChanged: "⇄ 类型替换",
  };
  return (
    <section className="tree-panel">
      {hint && <div className="cache-hint">{hint}</div>}
      <div
        className="tree-scroll"
        ref={scroll}
        tabIndex={0}
        role="tree"
        aria-label="快照差异目录树"
        onKeyDown={key}
      >
        <div className="tree-heading">
          <span>名称</span>
          <span>{metric === "size" ? "大小" : "分配"}（之前）</span>
          <span>{metric === "size" ? "大小" : "分配"}（之后）</span>
          <span>Δ {metric === "size" ? "大小" : "分配"}</span>
          <span>类型</span>
          <span>修改时间（之前）</span>
          <span>修改时间（之后）</span>
          <span>属性</span>
        </div>
        <div style={{ height: virtual.getTotalSize(), position: "relative" }}>
          {items.map((item) => {
            const v = visible[item.index];
            if ("action" in v) {
              const p = pages.current.get(v.parent)!;
              return (
                <div
                  key={visibleKey(v)}
                  className="tree-action"
                  style={{
                    transform: `translateY(${item.start}px)`,
                    paddingLeft: 12 + v.depth * 18,
                  }}
                >
                  {p.loading ? (
                    "读取中…"
                  ) : p.error ? (
                    <button onClick={() => void retry(v.parent)}>
                      {p.error} · 重试
                    </button>
                  ) : v.gap ? (
                    <button
                      aria-label="从首页重新加载子项"
                      data-parent-id={v.parent}
                      onClick={() => void reloadFirst(v.parent)}
                    >
                      第 {v.gap[0]}–{v.gap[1]} 项未载入 · 从首页重载
                    </button>
                  ) : p.cursor ? (
                    <button
                      aria-label="加载更多子项"
                      data-parent-id={v.parent}
                      onClick={() => void load(v.parent, true)}
                    >
                      加载更多 · 当前载入 {p.rows.length} 项 · 已读取至{" "}
                      {p.through} / {p.total}
                    </button>
                  ) : (
                    <span>无内容（仅包含本次导出记录）</span>
                  )}
                </div>
              );
            }
            const r = v.row;
            return (
              <div
                key={r.nodeId}
                data-node-id={r.nodeId}
                role="treeitem"
                aria-level={v.depth + 1}
                aria-selected={selected === r.nodeId}
                aria-expanded={
                  r.expandable ? expanded.current.has(r.nodeId) : undefined
                }
                className={
                  "tree-row " +
                  r.status +
                  (selected === r.nodeId ? " selected" : "")
                }
                style={{ transform: `translateY(${item.start}px)` }}
                onClick={() => select(r.nodeId)}
                onDoubleClick={() =>
                  r.expandable
                    ? void navigate(r.nodeId, true)
                    : onSelect(r.nodeId)
                }
              >
                <span
                  className="node-name"
                  style={{ paddingLeft: 8 + v.depth * 16 }}
                >
                  <button
                    className="disclosure"
                    aria-label={
                      expanded.current.has(r.nodeId) ? "收起目录" : "展开目录"
                    }
                    disabled={!r.expandable}
                    onClick={(e) => {
                      e.stopPropagation();
                      expanded.current.has(r.nodeId)
                        ? collapse(r.nodeId)
                        : void navigate(r.nodeId, true);
                    }}
                  >
                    {r.expandable
                      ? expanded.current.has(r.nodeId)
                        ? "▾"
                        : "▸"
                      : ""}
                  </button>
                  <i
                    className={
                      (r.after ?? r.before)?.kind === "directory"
                        ? "folder-glyph"
                        : "file-glyph"
                    }
                    aria-hidden="true"
                  />
                  <span title={r.name}>{r.name}</span>
                  {r.status === "typeChanged" ||
                  (r.status === "unchanged" && r.hasChanges) ||
                  (r.status === "modified" && r.sizeDelta === "0") ? (
                    <small
                      className="node-badge"
                      title={
                        labels[r.status] +
                        (r.hasChanges
                          ? ` · 内部变化 ${r.changedDescendantCount}`
                          : "")
                      }
                    >
                      {r.status === "typeChanged"
                        ? "类型"
                        : r.status === "unchanged"
                          ? "内部"
                          : "分配"}
                    </small>
                  ) : null}
                </span>
                <span title={r.before?.[metric]}>
                  {bytes(r.before?.[metric])}
                </span>
                <span title={r.after?.[metric]}>
                  {bytes(r.after?.[metric])}
                </span>
                <span
                  className={
                    BigInt(metric === "size" ? r.sizeDelta : r.allocatedDelta) >
                    0n
                      ? "positive"
                      : BigInt(
                            metric === "size" ? r.sizeDelta : r.allocatedDelta,
                          ) < 0n
                        ? "negative"
                        : ""
                  }
                >
                  {signed(metric === "size" ? r.sizeDelta : r.allocatedDelta)}
                </span>
                <span
                  title={
                    r.status === "typeChanged" ? "文件 / 目录类型替换" : r.name
                  }
                >
                  {r.status === "typeChanged"
                    ? r.before?.kind === "directory"
                      ? "目录→文件"
                      : "文件→目录"
                    : (r.after ?? r.before)?.kind === "directory"
                      ? "文件夹"
                      : r.name.lastIndexOf(".") >= 0
                        ? r.name
                            .slice(r.name.lastIndexOf(".") + 1)
                            .toUpperCase() + " 文件"
                        : "文件"}
                </span>
                <span title={r.before?.modified ?? undefined}>
                  {r.before?.modified ?? "—"}
                </span>
                <span title={r.after?.modified ?? undefined}>
                  {r.after?.modified ?? "—"}
                </span>
                <span
                  title={
                    r.after?.attributes ?? r.before?.attributes ?? undefined
                  }
                >
                  {r.after?.attributes ?? r.before?.attributes ?? "—"}
                </span>
              </div>
            );
          })}
        </div>
      </div>
    </section>
  );
});
