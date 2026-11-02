// "Sort ▾" in the table tab bar, like DBeaver's Sort/Filter panel: all
// the columns in a list, the sorted ones with their priority at the top,
// the others below to add with ↑ or ↓. The priority is changed by dragging
// the sorted rows (or ⌥↑/⌥↓ from the keyboard). The choices stay a draft
// until Apply is pressed (a single query, like DBeaver's OK); Esc or Cancel
// leave the sort as it was.
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { t } from "./i18n";
import PickerSearch from "./PickerSearch";
import { OrderBy } from "./ipc";
import { IconChevronDown, IconGrip, IconX } from "./icons";
import { sortAdd, sortRemove } from "./tableSort";
import { useSortDrag } from "./useSortDrag";
import type { WhereColumn } from "./WhereInput";

function sameOrder(a: OrderBy[], b: OrderBy[]): boolean {
  return a.length === b.length && a.every((o, i) => o.column === b[i].column && o.desc === b[i].desc);
}

/// ↑ and ↓ of a row: pressed = current direction of the column.
function DirButtons({
  column,
  desc,
  onPick,
}: {
  column: string;
  /// `null` = column not sorted (neither one pressed).
  desc: boolean | null;
  onPick: (desc: boolean) => void;
}) {
  return (
    <span className="sort-panel-dirs">
      <button
        className={"sort-panel-dir" + (desc === false ? " on" : "")}
        aria-pressed={desc === false}
        aria-label={t("{col} ascending", { col: column })}
        title={t("Ascending")}
        onClick={() => onPick(false)}
      >
        ↑
      </button>
      <button
        className={"sort-panel-dir" + (desc === true ? " on" : "")}
        aria-pressed={desc === true}
        aria-label={t("{col} descending", { col: column })}
        title={t("Descending")}
        onClick={() => onPick(true)}
      >
        ↓
      </button>
    </span>
  );
}

export default function SortPanel({
  order,
  columns,
  disabled,
  onApply,
}: {
  order: OrderBy[];
  columns: WhereColumn[];
  disabled: boolean;
  onApply: (order: OrderBy[]) => void;
}) {
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState<OrderBy[]>(order);
  const [q, setQ] = useState("");
  const rootRef = useRef<HTMLSpanElement | null>(null);
  const id = useId();

  const drag = useSortDrag(draft, "y", setDraft);
  const movable = draft.length > 1;

  const types = useMemo(() => new Map(columns.map((c) => [c.name, c.type_display])), [columns]);
  const others = useMemo(() => {
    const sorted = new Set(draft.map((o) => o.column));
    const words = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
    return columns.filter(
      (c) => !sorted.has(c.name) && words.every((w) => c.name.toLowerCase().includes(w))
    );
  }, [columns, draft, q]);

  // On opening the draft restarts from the sort in use.
  useEffect(() => {
    if (!open) return;
    setDraft(order);
    setQ("");
    const close = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      setOpen(false);
    };
    const blur = () => setOpen(false);
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", esc);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", esc);
      window.removeEventListener("blur", blur);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const changed = !sameOrder(draft, order);
  const apply = () => {
    setOpen(false);
    if (changed) onApply(draft);
  };

  return (
    <span className="db-picker sort-panel" ref={rootRef}>
      <button
        className="filter-add sort-panel-btn"
        disabled={disabled || columns.length === 0}
        title={t("Choose the columns to sort by and their priority")}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        {t("Sort")}
        <IconChevronDown />
      </button>
      {open && (
        <div className="db-picker-pop sort-panel-pop" role="dialog" aria-label={t("Sorting")}>
          <PickerSearch
            value={q}
            onChange={setQ}
            placeholder={t("filter {n} columns…", { n: columns.length })}
            aria-controls={`${id}-others`}
          />
          <div className="sort-panel-list">
            <div className="sort-panel-head">{t("Sorted")}</div>
            {draft.length === 0 && <div className="tree-note">{t("none: pick ↑ or ↓ below")}</div>}
            {drag.shown.map((o, i) => (
              <div
                key={o.column}
                ref={drag.setRef(o.column)}
                className={
                  "sort-panel-row sorted" +
                  (movable ? " movable" : "") +
                  (o.column === drag.dragged ? " dragging" : "")
                }
                onPointerDown={(e) => {
                  if (!movable || e.button !== 0 || (e.target as HTMLElement).closest("button")) return;
                  drag.startDrag(i, e);
                }}
                onKeyDown={(e) => {
                  // ⌥↑/⌥↓ on a button of the row: moves it by one place.
                  if (!movable || !e.altKey || e.metaKey || e.ctrlKey) return;
                  if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return;
                  e.preventDefault();
                  drag.moveBy(i, e.key === "ArrowUp" ? -1 : 1, ".sort-panel-dir.on");
                }}
              >
                <span
                  className="sort-panel-grip"
                  title={movable ? t("Drag to change the priority") : undefined}
                >
                  {movable && <IconGrip />}
                </span>
                <span className="sort-panel-prio">{i + 1}</span>
                <span className="sort-panel-name">{o.column}</span>
                <span className="sort-panel-type">{types.get(o.column) ?? ""}</span>
                <DirButtons
                  column={o.column}
                  desc={o.desc}
                  onPick={(desc) => setDraft(sortAdd(draft, o.column, desc))}
                />
                <button
                  className="chip-x"
                  title={t("Remove from sort")}
                  aria-label={t("Remove {col} from sort", { col: o.column })}
                  onClick={() => setDraft(sortRemove(draft, o.column))}
                >
                  <IconX />
                </button>
              </div>
            ))}
            <div className="sort-panel-head">{t("Other columns")}</div>
            <div id={`${id}-others`}>
              {others.map((c) => (
                <div key={c.name} className="sort-panel-row">
                  <span className="sort-panel-grip" />
                  <span className="sort-panel-prio" />
                  <span className="sort-panel-name">{c.name}</span>
                  <span className="sort-panel-type">{c.type_display}</span>
                  <DirButtons
                    column={c.name}
                    desc={null}
                    onPick={(desc) => setDraft(sortAdd(draft, c.name, desc))}
                  />
                  <span className="sort-panel-x-gap" />
                </div>
              ))}
              {others.length === 0 && <div className="tree-note">{t("no results")}</div>}
            </div>
          </div>
          <div className="sort-panel-foot">
            <button className="query-clear" disabled={draft.length === 0} onClick={() => setDraft([])}>
              {t("Clear")}
            </button>
            <span className="spacer" />
            <button className="btn small" onClick={() => setOpen(false)}>
              {t("Cancel")}
            </button>
            <button className="btn small primary" disabled={!changed} onClick={apply}>
              {t("Apply")}
            </button>
          </div>
        </div>
      )}
    </span>
  );
}
