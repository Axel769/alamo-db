// Windowed rendering of a long run of rows in a scrolling panel: a schema
// with 3000 tables put 3000 rows in the DOM (1.2 s to open it, §10). Only
// the rows in view, plus a margin, are rendered; two spacers keep the
// scroll height. Rows have an estimated height; every rendered row is
// measured, so an expanded row (a table with its columns) does not shift
// the slice. Short runs (under `threshold`) render as before: no wrappers.
import { ReactNode, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

const OVERSCAN = 8;

type Props<T> = {
  items: readonly T[];
  keyOf: (item: T) => string;
  /// Height of a row before it is measured, in px.
  rowHeight: number;
  render: (item: T) => ReactNode;
  threshold?: number;
};

export function VirtualRows<T>({ items, keyOf, rowHeight, render, threshold = 200 }: Props<T>) {
  const host = useRef<HTMLDivElement | null>(null);
  const heights = useRef(new Map<string, number>());
  const [range, setRange] = useState<[number, number]>([0, Math.min(items.length, 40)]);
  const windowed = items.length >= threshold;

  // Offset of every row from the top of the run, from the measured heights.
  const offsets = useMemo(() => {
    const out = new Float64Array(items.length + 1);
    for (let i = 0; i < items.length; i++) {
      out[i + 1] = out[i] + (heights.current.get(keyOf(items[i])) ?? rowHeight);
    }
    return out;
    // `range` is a dependency on purpose: a measurement changes the map and
    // then the range, which recomputes the offsets.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [items, keyOf, rowHeight, range]);

  useLayoutEffect(() => {
    const el = host.current;
    if (!windowed || !el) return;
    const parent = scrollParent(el);
    if (!parent) return;
    const compute = () => {
      const top = el.getBoundingClientRect().top - parent.getBoundingClientRect().top + parent.scrollTop;
      const from = parent.scrollTop - top;
      const to = from + parent.clientHeight;
      let start = lowerBound(offsets, from) - OVERSCAN;
      let end = lowerBound(offsets, to) + OVERSCAN;
      start = Math.max(0, Math.min(start, items.length));
      end = Math.max(start, Math.min(end, items.length));
      setRange((r) => (r[0] === start && r[1] === end ? r : [start, end]));
    };
    compute();
    parent.addEventListener("scroll", compute, { passive: true });
    const ro = new ResizeObserver(compute);
    ro.observe(parent);
    return () => {
      parent.removeEventListener("scroll", compute);
      ro.disconnect();
    };
  }, [windowed, items.length, offsets]);

  // Measured heights of the rendered rows: a change re-renders (the
  // offsets follow `range`) so the spacers stay exact.
  const rowsRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const rows = rowsRef.current;
    if (!windowed || !rows) return;
    const ro = new ResizeObserver((entries) => {
      let changed = false;
      for (const e of entries) {
        const key = (e.target as HTMLElement).dataset.vkey;
        if (key === undefined) continue;
        const h = e.contentRect.height;
        if (h > 0 && heights.current.get(key) !== h) {
          heights.current.set(key, h);
          changed = true;
        }
      }
      if (changed) setRange((r) => [...r] as [number, number]);
    });
    for (const child of rows.children) ro.observe(child);
    return () => ro.disconnect();
  }, [windowed, range, items]);

  if (!windowed) return <>{items.map(render)}</>;

  const [start, end] = range;
  return (
    <div ref={host}>
      <div style={{ height: offsets[start] }} />
      <div ref={rowsRef}>
        {items.slice(start, end).map((item) => (
          <div key={keyOf(item)} data-vkey={keyOf(item)}>
            {render(item)}
          </div>
        ))}
      </div>
      <div style={{ height: offsets[items.length] - offsets[end] }} />
    </div>
  );
}

/// Nearest ancestor that scrolls vertically.
function scrollParent(el: HTMLElement): HTMLElement | null {
  for (let p = el.parentElement; p; p = p.parentElement) {
    const o = getComputedStyle(p).overflowY;
    if (o === "auto" || o === "scroll") return p;
  }
  return null;
}

/// Index of the first row whose bottom is past `y`.
function lowerBound(offsets: Float64Array, y: number): number {
  let lo = 0;
  let hi = offsets.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (offsets[mid + 1] <= y) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}
