// "Default ▾" in the table tab bar: ONE sort per table (per profile +
// schema + table), without a name, applied when the table opens. The
// current sort is built by clicking the headers or with the chips; here it
// is saved as the default, restored or removed. The button says which
// state we are in: no default, default applied (✓), sort changed (•).
import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { t, tMsg } from "./i18n";
import { ipc, OrderBy } from "./ipc";
import { IconCheck, IconChevronDown } from "./icons";

export function defaultSortKey(profileId: number | null, schema: string, table: string) {
  return ["tableDefaultSort", profileId, schema, table] as const;
}

/// "date ↓, id ↑": summary of a sort.
export function sortSummary(order: OrderBy[]): string {
  return order.map((o) => `${o.column} ${o.desc ? "↓" : "↑"}`).join(", ");
}

function sameOrder(a: OrderBy[], b: OrderBy[]): boolean {
  return a.length === b.length && a.every((o, i) => o.column === b[i].column && o.desc === b[i].desc);
}

export default function SortMenu({
  profileId,
  schema,
  table,
  order,
  columns,
  disabled,
  onApply,
}: {
  /// Without a profile (tab never saved) there is nowhere to save: no menu.
  profileId: number | null;
  schema: string;
  table: string;
  order: OrderBy[];
  /// Columns of the table: when applying the default, those that no longer
  /// exist are skipped.
  columns: string[];
  disabled: boolean;
  onApply: (order: OrderBy[]) => void;
}) {
  const [open, setOpen] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const rootRef = useRef<HTMLSpanElement | null>(null);
  const qc = useQueryClient();
  const key = defaultSortKey(profileId, schema, table);
  const saved = useQuery({
    queryKey: key,
    queryFn: () => ipc.tableDefaultSortGet(profileId!, schema, table),
    enabled: profileId !== null,
    staleTime: Infinity,
  });
  const def = saved.data ?? null;
  const isDefault = def !== null && sameOrder(def, order);
  const modified = def !== null && !isDefault;

  useEffect(() => {
    if (!open) return;
    setNote(null);
    const close = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const blur = () => setOpen(false);
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", blur);
    };
  }, [open]);

  if (profileId === null) return null;

  const store = async (o: OrderBy[]) => {
    setNote(null);
    try {
      await ipc.tableDefaultSortSet(profileId, schema, table, o);
      setOpen(false);
    } catch (e) {
      setNote(tMsg(String(e)));
    }
    await qc.invalidateQueries({ queryKey: key });
  };

  const applyDefault = () => {
    if (!def) return;
    const known = new Set(columns);
    const kept = columns.length === 0 ? def : def.filter((c) => known.has(c.column));
    const dropped = def.filter((c) => !kept.includes(c));
    onApply(kept);
    if (dropped.length > 0) {
      setNote(
        t("columns no longer present, skipped: {cols}", { cols: dropped.map((c) => c.column).join(", ") })
      );
    } else {
      setOpen(false);
    }
  };

  return (
    <span className="db-picker sort-menu" ref={rootRef}>
      <button
        className={"filter-add sort-menu-btn" + (isDefault ? " active" : "") + (modified ? " modified" : "")}
        disabled={disabled}
        aria-haspopup="menu"
        aria-expanded={open}
        title={
          def === null
            ? t("No default sort for this table")
            : isDefault
              ? t("Default sort of the table: {s}", { s: sortSummary(def) })
              : t("Sort changed from the default: {s}", { s: sortSummary(def) })
        }
        onClick={() => setOpen((v) => !v)}
      >
        {isDefault && <IconCheck />}
        {t("Default")}
        {modified && (
          <span className="sort-menu-dot" role="img" aria-label={t("(modified)")} />
        )}
        <IconChevronDown />
      </button>
      {open && (
        <div className="db-picker-pop sort-menu-pop" onMouseDown={(e) => e.stopPropagation()}>
          <div className="sort-menu-current">
            {def ? t("Default: {s}", { s: sortSummary(def) }) : t("No default sort")}
          </div>
          <button
            className="tab-menu-item"
            disabled={order.length === 0 || isDefault}
            onClick={() => void store(order)}
          >
            {order.length === 0
              ? t("Save the current sort as default")
              : t("Save as default: {s}", { s: sortSummary(order) })}
          </button>
          {modified && (
            <button className="tab-menu-item" onClick={applyDefault}>
              {t("Restore the default")}
            </button>
          )}
          {def && (
            <button className="tab-menu-item" onClick={() => void store([])}>
              {t("Remove the default")}
            </button>
          )}
          {order.length === 0 && (
            <div className="sort-menu-note">{t("Sort the table by clicking the headers, then save it here.")}</div>
          )}
          {note && <div className="sort-menu-note">{note}</div>}
        </div>
      )}
    </span>
  );
}
