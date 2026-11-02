// "SQL ▾" in the table tab bar: the SELECT the tab is running, in a
// popover on demand instead of truncated at the end of the row (it stole
// space from the chips). Inside: the whole query, highlighted, to copy.
import { useEffect, useMemo, useRef, useState } from "react";
import { sqlPieces } from "./editor";
import { t } from "./i18n";
import { IconChevronDown } from "./icons";

export default function SqlPreview({ sql, dialect }: { sql: string; dialect: string }) {
  const [open, setOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const rootRef = useRef<HTMLSpanElement | null>(null);
  const pieces = useMemo(() => (open ? sqlPieces(sql, dialect) : []), [open, sql, dialect]);

  useEffect(() => {
    if (!open) return;
    setCopied(false);
    const close = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
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
  }, [open]);

  return (
    <span className="db-picker sql-preview" ref={rootRef}>
      <button
        className="filter-add sql-preview-btn"
        title={sql}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        SQL
        <IconChevronDown />
      </button>
      {open && (
        <div className="db-picker-pop sql-preview-pop" role="dialog" aria-label={t("SQL run by the tab")}>
          <pre className="sql-preview-code">
            {pieces.map((p, i) => (
              <span key={i} className={p.cls || undefined}>
                {p.text}
              </span>
            ))}
          </pre>
          <div className="sql-preview-foot">
            <button
              className="btn small"
              autoFocus
              onClick={() =>
                void navigator.clipboard.writeText(sql).then(() => {
                  setCopied(true);
                  setTimeout(() => setCopied(false), 1500);
                })
              }
            >
              {copied ? t("Copied") : t("Copy")}
            </button>
          </div>
        </div>
      )}
    </span>
  );
}
