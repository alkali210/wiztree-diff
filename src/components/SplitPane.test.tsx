import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useState } from "react";
import { SplitPane } from "./SplitPane";

let width = 1008, height = 608;
let resize: () => void;
let captured = new Set<number>();
beforeEach(() => {
  width = 1008; height = 608; captured = new Set();
  vi.stubGlobal("ResizeObserver", class {
    constructor(callback: () => void) { resize = callback; }
    observe() {} disconnect() {}
  });
  vi.stubGlobal("PointerEvent", class extends MouseEvent {
    pointerId: number; isPrimary: boolean;
    constructor(type: string, init: PointerEventInit) {
      super(type, init); this.pointerId = init.pointerId ?? 1; this.isPrimary = init.isPrimary ?? true;
    }
  });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    const horizontal = this.closest(".split-pane")?.classList.contains("horizontal");
    const size = parseFloat(this.style.flexBasis) || (horizontal ? 250 : 200);
    if (this.classList.contains("splitter")) return new DOMRect(0, 0, horizontal ? 8 : width, horizontal ? height : 8);
    if (this.classList.contains("split-pane-second")) return new DOMRect(0, 0, horizontal ? size : width, horizontal ? height : size);
    return new DOMRect(0, 0, width, height);
  });
  Object.defineProperties(HTMLElement.prototype, {
    setPointerCapture: { configurable: true, value: (id: number) => captured.add(id) },
    hasPointerCapture: { configurable: true, value: (id: number) => captured.has(id) },
    releasePointerCapture: { configurable: true, value: (id: number) => captured.delete(id) },
  });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
function mount(axis: "horizontal" | "vertical" = "horizontal", first = <div>first</div>) {
  const view = render(<SplitPane axis={axis} label="resize" first={first} second={<div>second</div>} defaultRatio={0.3} minFirst={300} minSecond={200} />);
  const handle = screen.getByRole("separator");
  const size = () => parseFloat(view.container.querySelector<HTMLElement>(".split-pane-second")!.style.flexBasis);
  return { ...view, handle, size };
}
it.each(["horizontal", "vertical"] as const)("drags %s panels, clamps both limits and releases capture", (axis) => {
  const { handle, size } = mount(axis);
  const coordinate = axis === "horizontal" ? "clientX" : "clientY";
  const initial = size();
  fireEvent.pointerDown(handle, { pointerId: 7, button: 0, [coordinate]: 400 });
  fireEvent.pointerMove(handle, { pointerId: 8, [coordinate]: 300 });
  expect(size()).toBe(initial);
  fireEvent.pointerMove(handle, { pointerId: 7, [coordinate]: 350 });
  expect(size()).toBe(initial + 50);
  fireEvent.pointerMove(handle, { pointerId: 7, [coordinate]: 9000 });
  expect(size()).toBe(200);
  fireEvent.pointerMove(handle, { pointerId: 7, [coordinate]: -9000 });
  expect(size()).toBe((axis === "horizontal" ? width : height) - 8 - 300);
  fireEvent.pointerUp(handle, { pointerId: 7 });
  expect(captured.size).toBe(0);
  expect(document.body.classList.contains("resizing")).toBe(false);
});
it("restores canceled drags and clears pointer capture and cursor on unmount", () => {
  const { handle, size, unmount } = mount();
  fireEvent.pointerDown(handle, { pointerId: 1, clientX: 400 });
  fireEvent.pointerMove(handle, { pointerId: 1, clientX: 300 });
  fireEvent.keyDown(handle, { key: "Escape" });
  expect(size()).toBe(300);
  fireEvent.pointerDown(handle, { pointerId: 2, clientX: 400 });
  fireEvent.pointerMove(handle, { pointerId: 2, clientX: 300 });
  fireEvent.pointerCancel(handle, { pointerId: 2 });
  expect(size()).toBe(300);
  fireEvent.pointerDown(handle, { pointerId: 3, clientX: 400 });
  unmount();
  expect(captured.size).toBe(0);
  expect(document.body.classList.contains("resizing")).toBe(false);
  expect(document.body.style.getPropertyValue("--resize-cursor")).toBe("");
});
it("supports keyboard sizing, reset and proportional resizing without losing child state", () => {
  function Child() { const [count, setCount] = useState(0); return <button onClick={() => setCount(count + 1)}>{count}</button>; }
  const { handle, size } = mount("horizontal", <Child />);
  fireEvent.click(screen.getByRole("button"));
  fireEvent.keyDown(handle, { key: "ArrowLeft", shiftKey: true });
  expect(size()).toBe(350);
  width = 508; resize();
  expect(size()).toBe(200);
  width = 1008; resize();
  expect(size()).toBe(350);
  fireEvent.keyDown(handle, { key: "Home" }); expect(size()).toBe(700);
  fireEvent.keyDown(handle, { key: "End" }); expect(size()).toBe(200);
  fireEvent.doubleClick(handle); expect(size()).toBe(300);
  expect(handle.getAttribute("aria-valuenow")).toBe("70");
  expect(screen.getByRole("button").textContent).toBe("1");
});
