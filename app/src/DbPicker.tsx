// Choice of the TAB's database (MySQL) in the query bar: a button that
// opens a list WITH A FILTER. A native select with dozens of databases
// cannot be scrolled; here you type a piece of the name and press Enter.
import { useEffect, useMemo, useRef, useState } from "react";
import { t } from "./i18n";
import { IconChevronDown } from "./icons";

export default function DbPicker({
  value,
  options,
  disabled,
  onPick,
  title,
}: {
  value: string | null;
  options: string[];
  disabled: boolean;
  onPick: (db: string) => void;
  /// Tooltip of the button; by default the one of the SQL tab bar.
  title?: string;
}) {
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const [sel, setSel] = useState(0);
  const rootRef = useRef<HTMLSpanElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  // Highlight moved by the mouse: the list must NOT scroll by itself — it
  // moves only with the arrows, on opening and when the filter changes.
  const byMouse = useRef(false);

  const list = useMemo(() => {
    const words = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
    return options.filter((o) => {
      const name = o.toLowerCase();
      return words.every((w) => name.includes(w));
    });
  }, [options, q]);

  // On opening: empty filter, current database highlighted.
  useEffect(() => {
    if (!open) return;
    setQ("");
    setSel(Math.max(0, value === null ? 0 : options.indexOf(value)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  useEffect(() => {
    if (!open) return;
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

  const pick = (db: string | undefined) => {
    if (db === undefined) return;
    setOpen(false);
    if (db !== value) onPick(db);
  };

  return (
    <span className="db-picker" ref={rootRef}>
      <button
        className={"db-picker-btn" + (value === null ? " none" : "")}
        disabled={disabled}
        title={title ?? t("This tab's database: unqualified statements read from here (USE before running)")}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="db-picker-value">{value ?? t("no database")}</span>
        <IconChevronDown />
      </button>
      {open && (
        <div className="db-picker-pop">
          <input
            className="db-picker-filter"
            autoFocus
            value={q}
            placeholder={t("filter {n} databases…", { n: options.length })}
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            onChange={(e) => {
              setQ(e.target.value);
              setSel(0);
            }}
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
          <div className="db-picker-list" ref={listRef}>
            {list.map((o, i) => (
              <button
                key={o}
                className={
                  "db-picker-item" + (i === sel ? " sel" : "") + (o === value ? " current" : "")
                }
                onMouseEnter={() => {
                  if (i === sel) return;
                  byMouse.current = true;
                  setSel(i);
                }}
                onClick={() => pick(o)}
              >
                {o}
              </button>
            ))}
            {list.length === 0 && <div className="tree-note">{t("no results")}</div>}
          </div>
        </div>
      )}
    </span>
  );
}
