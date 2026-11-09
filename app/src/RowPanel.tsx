// Row card (interface plan, step 6): all the fields of the selected row,
// to the right of the grid. Alternative to the cell panel at the bottom
// (InspectPanel): that one shows ONE value in full, this one the row — and
// for the field of the current cell it loads the full value anyway.
import { useEffect, useRef, useState } from "react";
import { locale, t } from "./i18n";
import { ColumnOut, ipc, isDefaultCell, PendingValue } from "./ipc";
import { cacheFor, useFullValue } from "./rowCache";
import { IconX } from "./icons";
import { useDragSize } from "./dragSize";
import { isLong, nullText } from "./display";
import { useSettings } from "./settings";

/// A jump to another table: `go(true)` opens it in a new tab.
export type RowLink = { label: string; title: string; go: (newTab: boolean) => void };
/// Links of the row: per column (outgoing foreign key → parent record) and
/// incoming (child tables pointing to this row).
export type RowLinks = { fields: Record<string, RowLink>; related: RowLink[] };

type Props = {
  connId: number;
  resultId: number;
  columns: ColumnOut[];
  row: number;
  /// Column of the current cell: highlighted and shown in full.
  col: number;
  onClose: () => void;
  /// Goes back to the cell panel at the bottom.
  onCellMode: () => void;
  /// Click on a field: it becomes the current cell.
  onSelectCol: (col: number) => void;
  links?: RowLinks;
  /// Pending value of a field (edited, not saved), `undefined` = none.
  pendingAt?: (col: number) => PendingValue | undefined;
  /// New row, not saved yet: nothing read from the server, every field is
  /// its pending value.
  fresh?: boolean;
  /// Editing, like in the grid (table tab with a PK, row not deleted): the
  /// values go to pending and are written with Save.
  edit?: RowEdit;
};

export type RowEdit = {
  onEdit: (col: number, value: PendingValue) => void;
  /// What a single line cannot hold (see `isLong`, truncated values) is
  /// edited in the cell panel at the bottom.
  onEditInPanel: (col: number) => void;
  canSetNull: (col: number) => boolean;
  /// `false` = the column is read-only (computed, §9.5.1).
  cellEditable?: (col: number) => boolean;
};

/// Readable JSON if the value is one; otherwise the text as it is.
function pretty(v: string, structured: boolean): string {
  if (!structured) return v;
  try {
    return JSON.stringify(JSON.parse(v), null, 2);
  } catch {
    return v;
  }
}

export default function RowPanel({
  connId,
  resultId,
  columns,
  row,
  col,
  onClose,
  onCellMode,
  onSelectCol,
  links,
  pendingAt,
  fresh = false,
  edit,
}: Props) {
  const cache = cacheFor(connId, resultId, columns.length);
  const current = fresh ? undefined : cache.cell(row, col);
  const nul = nullText(useSettings((s) => s.settings.data));
  const [copied, setCopied] = useState(false);
  const activeRef = useRef<HTMLDivElement | null>(null);
  // Field being edited in place: `initial` tells an untouched field (blur
  // without typing) from a real edit, so a NULL never becomes "".
  const [editing, setEditing] = useState<{ col: number; text: string; initial: string } | null>(null);
  // The same, readable by the blur that follows Enter/Esc (when the focus
  // goes back to the field) before React re-renders: it must find the
  // edit already closed, or Esc would save.
  const editingRef = useRef(editing);
  editingRef.current = editing;
  useEffect(() => setEditing(null), [row, resultId]);
  // Width chosen by dragging the left edge: long values and column names
  // need more room than the default.
  const panel = useRef<HTMLElement | null>(null);
  const drag = useDragSize("alamo.rowPanelW");
  const startDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    const el = panel.current;
    // On the left is the grid: the card grows at its expense, always
    // leaving it 200px.
    const beside = el?.previousElementSibling;
    if (!el || !beside) return;
    const right = el.getBoundingClientRect().right;
    const maxW = el.offsetWidth + Math.max(0, beside.clientWidth - 200);
    drag.startDrag(e, (ev) => Math.max(240, Math.min(maxW, Math.round(right - ev.clientX))));
  };

  // Only the current field is loaded in full (LOB from the arena, §4.7): the
  // other truncated ones stay truncated and say so.
  const needsFull = current !== undefined && !current.isNull && current.truncated;
  const full = useFullValue(cache, row, col, needsFull);
  useEffect(() => setCopied(false), [connId, resultId, row, col]);

  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest" });
  }, [row, col]);

  /// What the field shows: the pending value if there is one, otherwise the
  /// server's (in full for the current field, once loaded).
  const shownAt = (i: number): PendingValue | undefined => {
    const p = pendingAt?.(i);
    if (p !== undefined || fresh) return p;
    const v = cache.cell(row, i);
    if (v === undefined) return undefined;
    return v.isNull ? null : i === col && full !== null ? full : v.v;
  };

  const startEdit = (i: number) => {
    if (!edit || edit.cellEditable?.(i) === false) return;
    const p = pendingAt?.(i);
    const v = fresh ? undefined : cache.cell(row, i);
    if (p === undefined && v === undefined) return;
    const long =
      typeof p === "string" ? isLong(p) : p === undefined && !!v && !v.isNull && (v.truncated || isLong(v.v));
    if (long) {
      edit.onEditInPanel(i);
      return;
    }
    const s = shownAt(i);
    const text = typeof s === "string" ? s : "";
    setEditing({ col: i, text, initial: text });
  };

  /// Closes the edit in place; `save` = the typed text goes to pending.
  const finish = (save: boolean) => {
    const cur = editingRef.current;
    if (!cur) return;
    editingRef.current = null;
    setEditing(null);
    if (save && edit && cur.text !== cur.initial) edit.onEdit(cur.col, cur.text);
  };

  const copyJson = () => {
    const obj: Record<string, string | null> = {};
    columns.forEach((c, i) => {
      const s = shownAt(i);
      // DEFAULT is decided by the server on Save: the value read so far.
      const v = fresh ? undefined : cache.cell(row, i);
      obj[c.name] = isDefaultCell(s) ? (v === undefined || v.isNull ? null : v.v) : (s ?? null);
    });
    void navigator.clipboard.writeText(JSON.stringify(obj, null, 2)).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  };

  return (
    <aside className="row-panel" ref={panel} style={drag.size ? { width: drag.size } : undefined}>
      <div
        className="panel-resize"
        onPointerDown={startDrag}
        onDoubleClick={drag.reset}
        title={t("Drag to resize, double-click for the default width")}
      />
      <div className="row-panel-head">
        <span className="row-panel-title">
          {fresh ? t("new row") : t("row {n}", { n: (row + 1).toLocaleString(locale()) })}
        </span>
        <div className="spacer" />
        <button className="btn small" onClick={copyJson} title={t("Copy the row as JSON")}>
          {copied ? t("Copied") : "JSON"}
        </button>
        <button
          className="btn small"
          onClick={onCellMode}
          title={t("Show only the selected cell, at the bottom")}
        >
          {t("Cell")}
        </button>
        <button className="btn small" onClick={onClose} title={t("Close")}>
          <IconX />
        </button>
      </div>
      <div className="row-panel-body">
        {columns.map((c, i) => {
          const v = fresh ? undefined : cache.cell(row, i);
          const isCur = i === col;
          const p = pendingAt?.(i);
          const shown = shownAt(i);
          const text = typeof shown === "string" ? shown : null;
          // A new row is all pending: it takes the grid's "new" colour.
          const state = fresh ? " new" : p !== undefined ? " pending" : "";
          return (
            <div
              key={i}
              className={"row-field" + (isCur ? " current" : "") + state}
              ref={isCur ? activeRef : undefined}
              tabIndex={-1}
              title={edit ? t("Double-click or Enter to edit, ⇧Del for NULL") : undefined}
              onClick={() => !isCur && onSelectCol(i)}
              onDoubleClick={() => startEdit(i)}
              onKeyDown={(e) => {
                if (!edit || editing?.col === i || e.target !== e.currentTarget) return;
                if (e.key === "Enter") {
                  e.preventDefault();
                  startEdit(i);
                } else if (
                  e.shiftKey &&
                  (e.key === "Delete" || e.key === "Backspace") &&
                  edit.canSetNull(i)
                ) {
                  e.preventDefault();
                  edit.onEdit(i, null);
                }
              }}
            >
              <div className="row-field-key">
                <span>{c.name}</span>
                <small>{c.type_display}</small>
              </div>
              {editing?.col === i ? (
                <input
                  className="row-field-input"
                  autoFocus
                  autoCorrect="off"
                  spellCheck={false}
                  value={editing.text}
                  onChange={(e) => setEditing({ ...editing, text: e.target.value })}
                  onBlur={() => finish(true)}
                  onKeyDown={(e) => {
                    e.stopPropagation();
                    if (e.key !== "Enter" && e.key !== "Escape") return;
                    e.preventDefault();
                    finish(e.key === "Enter");
                    e.currentTarget.closest<HTMLElement>(".row-field")?.focus();
                  }}
                />
              ) : shown === undefined ? (
                <span className="row-field-val muted">…</span>
              ) : shown === null ? (
                <span className="row-field-val null">{nul}</span>
              ) : isDefaultCell(shown) ? (
                <span className="row-field-val null">DEFAULT</span>
              ) : (
                <div className="row-field-value">
                  <pre className={"row-field-val " + c.semantic_class + (isCur ? " open" : "")}>
                    {pretty(text ?? "", c.semantic_class === "structured")}
                    {p === undefined && v?.truncated && !(isCur && full !== null) && (
                      <span className="row-field-more"> {t("… (truncated: select it to read it in full)")}</span>
                    )}
                  </pre>
                  {links?.fields[c.name] && p === undefined && (
                    // Foreign key: like the ↗ in the grid, only the arrow
                    // leads to the linked record (⌘/Ctrl+click = in a new
                    // tab); the value is edited like the others. Not on a
                    // pending value: the link points to the saved one.
                    <button
                      className="row-link"
                      title={links.fields[c.name].title}
                      onClick={(e) => {
                        e.stopPropagation();
                        links.fields[c.name].go(e.metaKey || e.ctrlKey);
                      }}
                      onDoubleClick={(e) => e.stopPropagation()}
                    >
                      → {links.fields[c.name].label}
                    </button>
                  )}
                </div>
              )}
            </div>
          );
        })}
        {links && links.related.length > 0 && (
          <div className="row-related">
            <div className="panel-section">{t("Related")}</div>
            {links.related.map((l, i) => (
              <button
                key={i}
                className="row-related-item"
                title={l.title}
                onClick={(e) => l.go(e.metaKey || e.ctrlKey)}
              >
                {l.label} <span className="row-link-to">→</span>
              </button>
            ))}
          </div>
        )}
      </div>
    </aside>
  );
}
