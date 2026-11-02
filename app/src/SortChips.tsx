// Chips of the server-side sort in the table tab bar: one per column, in
// priority order. The sign is the same as the header's (`↓1`): the number
// appears only with several columns, clicking reverses.
//
// With two or more columns the priority is changed by dragging a chip (or
// with ⌥←/⌥→ on the focused chip), see useSortDrag: the query runs once
// only, on release.
import { t } from "./i18n";
import { OrderBy } from "./ipc";
import { IconX } from "./icons";
import { sortFlip, sortRemove } from "./tableSort";
import { useSortDrag } from "./useSortDrag";

export default function SortChips({
  order,
  disabled,
  onChange,
}: {
  order: OrderBy[];
  disabled: boolean;
  onChange: (order: OrderBy[]) => void;
}) {
  const { shown, dragged, setRef, startDrag, moveBy } = useSortDrag(order, "x", onChange);
  const movable = order.length > 1 && !disabled;

  return (
    <>
      {shown.map((o, i) => (
        <span
          key={o.column}
          ref={setRef(o.column)}
          className={
            "query-chip sort-chip" + (movable ? " movable" : "") + (o.column === dragged ? " dragging" : "")
          }
          title={movable ? t("Drag to change the priority") : undefined}
          onPointerDown={(e) => {
            if (!movable || e.button !== 0 || (e.target as HTMLElement).closest("button")) return;
            startDrag(i, e);
          }}
          onKeyDown={(e) => {
            // ⌥←/⌥→ on the chip: moves the column by one place. stopPropagation:
            // outside the chips ⌥←/⌥→ is Back/Forward of the tab.
            if (!movable || !e.altKey || e.metaKey || e.ctrlKey) return;
            if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
            e.preventDefault();
            e.stopPropagation();
            moveBy(i, e.key === "ArrowLeft" ? -1 : 1, ".chip-dir");
          }}
        >
          <span className="mono">{o.column}</span>
          <button
            className="chip-dir"
            title={t("Reverse the direction")}
            aria-label={t("Reverse the direction of {col}", { col: o.column })}
            disabled={disabled}
            onClick={() => onChange(sortFlip(order, o.column))}
          >
            {o.desc ? "↓" : "↑"}
            {shown.length > 1 && i + 1}
          </button>
          <button
            className="chip-x"
            title={t("Remove from sort")}
            aria-label={t("Remove {col} from sort", { col: o.column })}
            disabled={disabled}
            onClick={() => onChange(sortRemove(order, o.column))}
          >
            <IconX />
          </button>
        </span>
      ))}
    </>
  );
}
