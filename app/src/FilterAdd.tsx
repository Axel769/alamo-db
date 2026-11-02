// "+ Filter" in the table tab bar: a button that opens the list of the
// columns WITH A FILTER and with their type (like DbPicker). A native
// select with 40 columns cannot be scrolled and does not say the type;
// here you type a piece of the name and press Enter.
import { forwardRef, useEffect, useId, useImperativeHandle, useMemo, useRef, useState } from "react";
import { t } from "./i18n";
import PickerSearch from "./PickerSearch";
import type { WhereColumn } from "./WhereInput";

/// For the parent: ⇧⌘F opens the list without the mouse.
export type FilterAddHandle = { open: () => void };

type FilterAddProps = {
  columns: WhereColumn[];
  disabled: boolean;
  onPick: (column: string) => void;
};

const FilterAdd = forwardRef<FilterAddHandle, FilterAddProps>(function FilterAdd(
  { columns, disabled, onPick },
  ref
) {
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const [sel, setSel] = useState(0);
  const rootRef = useRef<HTMLSpanElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  // Highlight moved by the mouse: the list does not scroll by itself (see DbPicker).
  const byMouse = useRef(false);
  const id = useId();

  const list = useMemo(() => {
    const words = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
    return columns.filter((c) => {
      const name = c.name.toLowerCase();
      return words.every((w) => name.includes(w));
    });
  }, [columns, q]);

  useEffect(() => {
    if (!open) return;
    setQ("");
    setSel(0);
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

  useEffect(() => {
    if (byMouse.current) {
      byMouse.current = false;
      return;
    }
    listRef.current?.querySelector(".db-picker-item.sel")?.scrollIntoView({ block: "nearest" });
  }, [sel, open, list]);

  useImperativeHandle(ref, () => ({
    open: () => {
      if (!disabled && columns.length > 0) setOpen(true);
    },
  }));

  const pick = (c: WhereColumn | undefined) => {
    if (c === undefined) return;
    setOpen(false);
    onPick(c.name);
  };

  return (
    <span className="db-picker" ref={rootRef}>
      <button
        className="filter-add"
        disabled={disabled || columns.length === 0}
        title={t("Add a server-side filter on a column (⇧⌘F)")}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        {t("+ Filter")}
      </button>
      {open && (
        <div className="db-picker-pop">
          <PickerSearch
            value={q}
            onChange={(v) => {
              setQ(v);
              setSel(0);
            }}
            placeholder={t("filter {n} columns…", { n: columns.length })}
            role="combobox"
            aria-expanded
            aria-controls={`${id}-list`}
            aria-activedescendant={list[sel] ? `${id}-${sel}` : undefined}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setSel((i) => Math.min(list.length - 1, i + 1));
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                setSel((i) => Math.max(0, i - 1));
              } else if (e.key === "Enter") {
                e.preventDefault();
                pick(list[sel]);
              } else if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                setOpen(false);
              }
            }}
          />
          <div className="db-picker-list" id={`${id}-list`} role="listbox" ref={listRef}>
            {list.map((c, i) => (
              <button
                key={c.name}
                id={`${id}-${i}`}
                role="option"
                aria-selected={i === sel}
                tabIndex={-1}
                className={"db-picker-item filter-add-item" + (i === sel ? " sel" : "")}
                onMouseEnter={() => {
                  if (i === sel) return;
                  byMouse.current = true;
                  setSel(i);
                }}
                onClick={() => pick(c)}
              >
                <span className="filter-add-name">{c.name}</span>
                <span className="filter-add-type">{c.type_display}</span>
              </button>
            ))}
            {list.length === 0 && <div className="tree-note">{t("no results")}</div>}
          </div>
        </div>
      )}
    </span>
  );
});

export default FilterAdd;
