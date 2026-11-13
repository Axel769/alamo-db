// Windowed rendering of a long run of rows in a scrolling panel: a schema
// with 3000 tables put 3000 rows in the DOM (1.2 s to open it, §10). Only
// the rows in view, plus a margin, are rendered; two spacers keep the
// scroll height. Rows have an estimated height; every rendered row is
// measured, so an expanded row (a table with its columns) does not shift
// the slice. Short runs (under `threshold`) render as before: no wrappers.
//
// The slice never feeds back into itself: offsets depend on the measured
// heights only, never on the slice, and a slice that still covers the
// view is kept. Otherwise a slice change moved the rows, the move changed
// the slice, and React stopped the loop with "Maximum update depth
// exceeded" (#185): seen on Windows, where the scrollbars take room and
// fractional display scaling rounds row positions differently at every
// pass, with the tree filter opening many runs at once.
import { ReactNode, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

const OVERSCAN = 8;
/// A measured height that moves by less than this is the same height:
/// sub-pixel rounding, not a resized row.
const HEIGHT_EPSILON = 0.5;

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
  /// Bumped when a measured height changes: the offsets follow it.
  const [measured, setMeasured] = useState(0);
  const windowed = items.length >= threshold;

  // Offset of every row from the top of the run, from the measured heights.
  const offsets = useMemo(() => {
    const out = new Float64Array(items.length + 1);
    for (let i = 0; i < items.length; i++) {
      out[i + 1] = out[i] + (heights.current.get(keyOf(items[i])) ?? rowHeight);
    }
    return out;
    // `measured` stands for the heights map, which a ref does not signal.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [items, keyOf, rowHeight, measured]);

  useLayoutEffect(() => {
    const el = host.current;
    if (!windowed || !el) return;
    const parent = scrollParent(el);
    if (!parent) return;
    const compute = () => {
      const top = el.getBoundingClientRect().top - parent.getBoundingClientRect().top + parent.scrollTop;
      const from = parent.scrollTop - top;
      const to = from + parent.clientHeight;
      const clamp = (i: number) => Math.max(0, Math.min(i, items.length));
      // The rows in view, then the slice with its margin around them.
      const first = clamp(lowerBound(offsets, from));
      const last = Math.max(first, clamp(lowerBound(offsets, to) + 1));
      const start = clamp(first - OVERSCAN);
      const end = Math.max(start, clamp(last + OVERSCAN));
      setRange((r) =>
        // Kept while it covers the view and is not much larger than it.
        r[0] <= first && r[1] >= last && r[1] <= items.length && r[1] - r[0] <= end - start + 2 * OVERSCAN
          ? r
          : [start, end]
      );
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
  // offsets follow `measured`) so the spacers stay exact.
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
        const old = heights.current.get(key);
        if (h > 0 && (old === undefined || Math.abs(old - h) >= HEIGHT_EPSILON)) {
          heights.current.set(key, h);
          changed = true;
        }
      }
      if (changed) setMeasured((m) => m + 1);
    });
    for (const child of rows.children) ro.observe(child);
    return () => ro.disconnect();
  }, [windowed, range, items]);

  if (!windowed) return <>{items.map(render)}</>;

  // The run may have shrunk (filter) before the slice follows it.
  const end = Math.min(range[1], items.length);
  const start = Math.min(range[0], end);
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
