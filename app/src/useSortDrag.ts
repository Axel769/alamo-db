// Sort priority changed by dragging: used by the chips of the bar
// (horizontally) and by the rows of the "Sort" popover (vertically).
// While dragging the elements move as a preview (`shown`); `onDrop` arrives
// once only, on release, and Esc cancels. From the keyboard `moveBy` moves
// by one place and gives the focus back to the moved element.
//
// Pointer events and not HTML5 drag-and-drop: in the Tauri webview that one
// is tied to file drop. Move and release are listened to on window: the
// preview moves the nodes in the DOM, the pointer capture would be lost.
import { PointerEvent as ReactPointerEvent, useEffect, useRef, useState } from "react";
import { OrderBy } from "./ipc";
import { sortMove } from "./tableSort";

/// Beyond this distance (px) the press becomes a drag.
const DRAG_THRESHOLD = 4;

export function useSortDrag(order: OrderBy[], axis: "x" | "y", onDrop: (next: OrderBy[]) => void) {
  const [preview, setPreview] = useState<OrderBy[] | null>(null);
  const [dragged, setDragged] = useState<string | null>(null);
  const els = useRef(new Map<string, HTMLElement>());
  // Element moved from the keyboard: it moves in the DOM and loses the focus,
  // which is given back after the render, on the control named by the selector.
  const refocus = useRef<{ column: string; selector: string } | null>(null);

  useEffect(() => {
    const r = refocus.current;
    refocus.current = null;
    if (r) els.current.get(r.column)?.querySelector<HTMLElement>(r.selector)?.focus();
  }, [order]);

  const setRef = (column: string) => (el: HTMLElement | null) => {
    if (el) els.current.set(column, el);
    else els.current.delete(column);
  };

  const pos = (e: { clientX: number; clientY: number }) => (axis === "x" ? e.clientX : e.clientY);

  const startDrag = (from: number, e: ReactPointerEvent) => {
    e.preventDefault(); // no text selection while dragging
    const start = pos(e);
    // Centers of the elements at the start, in the original order.
    const centers = order.map((c) => {
      const r = els.current.get(c.column)?.getBoundingClientRect();
      if (!r) return 0;
      return axis === "x" ? r.left + r.width / 2 : r.top + r.height / 2;
    });
    let next: OrderBy[] | null = null;
    const move = (ev: PointerEvent) => {
      const p = pos(ev);
      if (next === null && Math.abs(p - start) < DRAG_THRESHOLD) return;
      const to = centers.filter((c, i) => i !== from && c < p).length;
      next = sortMove(order, from, to);
      setPreview(next);
      setDragged(order[from].column);
    };
    const end = (commit: boolean) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
      window.removeEventListener("keydown", esc, true);
      setPreview(null);
      setDragged(null);
      if (commit && next && next.some((o, i) => o.column !== order[i].column)) onDrop(next);
    };
    const up = () => end(true);
    const cancel = () => end(false);
    const esc = (ev: KeyboardEvent) => {
      if (ev.key !== "Escape") return;
      ev.preventDefault();
      ev.stopPropagation();
      end(false);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    window.addEventListener("keydown", esc, true);
  };

  /// Moves the element at `index` by `delta` places; `selector` = the
  /// control inside the element that takes the focus back.
  const moveBy = (index: number, delta: number, selector: string) => {
    const to = index + delta;
    if (to < 0 || to >= order.length) return;
    refocus.current = { column: order[index].column, selector };
    onDrop(sortMove(order, index, to));
  };

  return { shown: preview ?? order, dragged, setRef, startDrag, moveBy };
}
