import { useId, useLayoutEffect, useRef, type ReactNode } from "react";

interface Props {
  axis: "horizontal" | "vertical";
  label: string;
  first: ReactNode;
  second?: ReactNode;
  defaultRatio: number;
  minFirst: number;
  minSecond: number;
}

// Only sizes the existing panels. Resizing does not remount children or request data.
export function SplitPane({ axis, label, first, second, defaultRatio, minFirst, minSecond }: Props) {
  const id = useId();
  const container = useRef<HTMLDivElement>(null);
  const end = useRef<HTMLDivElement>(null);
  const separator = useRef<HTMLDivElement>(null);
  const ratio = useRef(defaultRatio);
  const enabled = second != null;
  useLayoutEffect(() => {
    const parent = container.current, target = end.current, handle = separator.current;
    if (!enabled || !parent || !target || !handle) return;
    const dimension = axis === "horizontal" ? "width" : "height";
    const coordinate = axis === "horizontal" ? "clientX" : "clientY";
    let drag: { pointerId: number; origin: number; size: number; ratio: number } | null = null;
    function limits() {
      const available = Math.max(0, parent!.getBoundingClientRect()[dimension] - handle!.getBoundingClientRect()[dimension]);
      return { available, min: minSecond, max: Math.max(minSecond, available - minFirst) };
    }
    function apply() {
      const { available, min, max } = limits();
      if (!available) return;
      const size = Math.min(max, Math.max(min, available * ratio.current));
      target!.style.flexBasis = size + "px";
      handle!.setAttribute("aria-valuemin", String(Math.round((available - max) / available * 100)));
      handle!.setAttribute("aria-valuemax", String(Math.round((available - min) / available * 100)));
      handle!.setAttribute("aria-valuenow", String(Math.round((available - size) / available * 100)));
      handle!.setAttribute("aria-valuetext", Math.round(size) + " 像素");
    }
    function setSize(size: number) {
      const { available, min, max } = limits();
      if (!available) return;
      ratio.current = Math.min(max, Math.max(min, size)) / available;
      apply();
    }
    function finish(event?: PointerEvent, restore = false) {
      if (!drag || (event && event.pointerId !== drag.pointerId)) return;
      if (restore) ratio.current = drag.ratio;
      const pointerId = drag.pointerId;
      drag = null;
      handle!.classList.remove("dragging");
      document.body.classList.remove("resizing");
      document.body.style.removeProperty("--resize-cursor");
      if (handle!.hasPointerCapture(pointerId)) handle!.releasePointerCapture(pointerId);
      apply();
    }
    function down(event: PointerEvent) {
      if (event.button !== 0 || !event.isPrimary || drag) return;
      event.preventDefault();
      handle!.focus();
      drag = { pointerId: event.pointerId, origin: event[coordinate], size: target!.getBoundingClientRect()[dimension], ratio: ratio.current };
      handle!.setPointerCapture(event.pointerId);
      handle!.classList.add("dragging");
      document.body.style.setProperty("--resize-cursor", axis === "horizontal" ? "col-resize" : "row-resize");
      document.body.classList.add("resizing");
    }
    function move(event: PointerEvent) {
      if (drag?.pointerId === event.pointerId) setSize(drag.size - (event[coordinate] - drag.origin));
    }
    const up = (event: PointerEvent) => finish(event);
    const cancel = (event: PointerEvent) => finish(event, true);
    const reset = () => { ratio.current = defaultRatio; apply(); };
    function key(event: KeyboardEvent) {
      if (event.key === "Escape" && drag) { event.preventDefault(); finish(undefined, true); return; }
      const previous = axis === "horizontal" ? "ArrowLeft" : "ArrowUp";
      const next = axis === "horizontal" ? "ArrowRight" : "ArrowDown";
      if (![previous, next, "Home", "End"].includes(event.key)) return;
      event.preventDefault();
      const { min, max } = limits();
      const size = target!.getBoundingClientRect()[dimension];
      setSize(event.key === "Home" ? max : event.key === "End" ? min : size + (event.key === previous ? 1 : -1) * (event.shiftKey ? 50 : 10));
    }
    handle.addEventListener("pointerdown", down);
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
    handle.addEventListener("pointercancel", cancel);
    handle.addEventListener("lostpointercapture", up);
    handle.addEventListener("dblclick", reset);
    handle.addEventListener("keydown", key);
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(apply);
    observer?.observe(parent);
    apply();
    return () => {
      observer?.disconnect();
      finish();
      handle.removeEventListener("pointerdown", down);
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      handle.removeEventListener("pointercancel", cancel);
      handle.removeEventListener("lostpointercapture", up);
      handle.removeEventListener("dblclick", reset);
      handle.removeEventListener("keydown", key);
    };
  }, [axis, enabled, defaultRatio, minFirst, minSecond]);
  const firstMin = axis === "horizontal" ? { minWidth: minFirst } : { minHeight: minFirst };
  const secondMin = axis === "horizontal" ? { minWidth: minSecond } : { minHeight: minSecond };
  return (
    <div ref={container} className={"split-pane " + axis}>
      <div id={id + "-first"} className="split-pane-first" style={enabled ? firstMin : undefined}>{first}</div>
      {enabled && <>
        <div ref={separator} className={"splitter splitter-" + axis} role="separator" tabIndex={0}
          aria-label={label} aria-orientation={axis === "horizontal" ? "vertical" : "horizontal"}
          aria-controls={id + "-first " + id + "-second"}
          title="拖动调整 · 双击恢复 · 方向键微调" />
        <div ref={end} id={id + "-second"} className="split-pane-second" style={secondMin}>{second}</div>
      </>}
    </div>
  );
}
