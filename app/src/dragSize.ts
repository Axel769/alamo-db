// Sizes the user picks by dragging an edge (side panel, results, cell panel,
// row card): a preference kept in localStorage, `null` = the CSS default.
import { useCallback, useState } from "react";

export function useDragSize(key: string) {
  const [size, setSize] = useState<number | null>(() => {
    const v = Number(localStorage.getItem(key));
    return v > 0 ? v : null;
  });

  /// Double click on the handle: back to the CSS default.
  const reset = useCallback(() => {
    setSize(null);
    localStorage.removeItem(key);
  }, [key]);

  /// Drag on the handle, with pointer capture so that it goes on over the
  /// grid. `measure` turns the pointer into the size, already clamped; the
  /// last one is saved on release.
  const startDrag = useCallback(
    (e: React.PointerEvent<HTMLElement>, measure: (ev: PointerEvent) => number) => {
      if (e.button !== 0) return;
      e.preventDefault();
      const el = e.currentTarget;
      el.setPointerCapture(e.pointerId);
      let last = 0;
      const move = (ev: PointerEvent) => {
        last = measure(ev);
        setSize(last);
      };
      const up = () => {
        el.removeEventListener("pointermove", move);
        el.removeEventListener("pointerup", up);
        el.removeEventListener("pointercancel", up);
        if (last > 0) localStorage.setItem(key, String(last));
      };
      el.addEventListener("pointermove", move);
      el.addEventListener("pointerup", up);
      el.addEventListener("pointercancel", up);
    },
    [key],
  );

  return { size, startDrag, reset };
}
