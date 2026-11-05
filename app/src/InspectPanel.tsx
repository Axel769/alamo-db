// Cell inspection panel (§4.7, §9.6.2): the full value is read here, not
// in the grid — the grid truncates to 4KB on the wire and clamps the draw.
// In an editable grid it is also where the long values are edited (like
// DBeaver's value panel): every keystroke goes to pending, as the overlay.
import { useEffect, useRef, useState } from "react";
import { locale, t } from "./i18n";
import { ColumnOut, DEFAULT_CELL, PendingValue } from "./ipc";
import { cacheFor, useFullValue } from "./rowCache";
import { IconX } from "./icons";
import { useDragSize } from "./dragSize";
import { ValueEditor, ValueEditorOptions } from "./valueEditor";

type Props = {
  /// Connection of the result set (M3.5).
  connId: number;
  resultId: number;
  columns: ColumnOut[];
  row: number;
  col: number;
  onClose: () => void;
  /// Switches to the row card, to the right of the grid.
  onRowMode?: () => void;
  /// The cell can be edited here (table tab with a PK, row not deleted).
  editable?: boolean;
  /// New row, not saved yet: nothing read from the server, the value is
  /// the pending one.
  fresh?: boolean;
  /// Pending value of the cell (edited, not saved), `undefined` = none.
  pending?: PendingValue;
  /// Text typed in the panel, or the value restored by Esc.
  onEdit?: (value: PendingValue) => void;
  /// Move the focus into the editor (double click on a long value,
  /// Shift+Enter) as soon as it can be edited; `onFocusTaken` drops it.
  focusRequest?: boolean;
  onFocusTaken?: () => void;
  /// ⌘↵ or Esc: the focus goes back to the grid.
  onDone?: () => void;
};

export default function InspectPanel({
  connId,
  resultId,
  columns,
  row,
  col,
  onClose,
  onRowMode,
  editable = false,
  fresh = false,
  pending,
  onEdit,
  focusRequest = false,
  onFocusTaken,
  onDone,
}: Props) {
  const meta = columns[col];
  const cache = cacheFor(connId, resultId, columns.length);
  const cached = fresh ? undefined : cache.cell(row, col);
  const isNull = cached?.isNull ?? false;
  const needsFull = !fresh && (cached === undefined || cached.truncated);
  const [copied, setCopied] = useState(false);
  // Height chosen by dragging the top edge: preference (localStorage),
  // null = CSS default. To read a long JSON the panel is raised.
  const panel = useRef<HTMLDivElement | null>(null);
  const drag = useDragSize("alamo.inspectH");
  const height = drag.size;

  // `get_cell_full` resolves the LOBs from the arena (§4.7); for values
  // already whole in the cache the call is avoided.
  const full = useFullValue(cache, row, col, !isNull && needsFull);
  useEffect(() => setCopied(false), [connId, resultId, row, col]);

  const original = isNull ? null : needsFull ? full : (cached?.v ?? null);
  const loading = !isNull && needsFull && full === null;
  // What the cell shows: the pending value wins over the read one.
  const shown: PendingValue = pending !== undefined ? pending : original;
  const value = typeof shown === "string" ? shown : null;
  const shownNull = shown === null;
  const shownDefault = shown === DEFAULT_CELL;
  const canEdit = editable && !loading && onEdit !== undefined;

  // The editor lives outside React (like the SQL one): created once, its
  // document replaced when the cell changes or the value changes elsewhere.
  const host = useRef<HTMLDivElement | null>(null);
  const editor = useRef<ValueEditor | null>(null);
  // Value of the cell when the focus entered the editor: Esc restores it.
  const entry = useRef<PendingValue | undefined>(undefined);
  const cb = useRef({ onEdit, onDone, original });
  cb.current = { onEdit, onDone, original };
  const cellKey = `${resultId}:${row}:${col}`;
  const lastKey = useRef<string | null>(null);
  const opts: ValueEditorOptions = {
    readOnly: !canEdit,
    placeholder: shownDefault ? "DEFAULT" : shownNull ? "NULL" : "",
    onChange: (text) => cb.current.onEdit?.(text),
    onEscape: () => {
      const e = entry.current;
      // Back to the value on entry: equal to the read one = no change left.
      if (e !== undefined && editor.current && editor.current.text !== (typeof e === "string" ? e : "")) {
        cb.current.onEdit?.(e);
      }
      cb.current.onDone?.();
    },
    onDone: () => cb.current.onDone?.(),
  };
  const optsRef = useRef(opts);
  optsRef.current = opts;

  const pendingRef = useRef(pending);
  pendingRef.current = pending;
  useEffect(() => {
    if (!host.current) return;
    const ed = new ValueEditor(host.current, "", optsRef.current);
    editor.current = ed;
    const dom = ed.view.contentDOM;
    const onFocus = () => {
      entry.current = pendingRef.current !== undefined ? pendingRef.current : cb.current.original;
    };
    dom.addEventListener("focus", onFocus);
    return () => {
      dom.removeEventListener("focus", onFocus);
      ed.destroy();
      editor.current = null;
    };
  }, []);

  // New cell, or the value changed outside the editor (Set NULL from the
  // grid, save): the document follows. While the user types in it, the
  // value coming back from pending is their own text: left alone.
  const doc = value ?? "";
  useEffect(() => {
    const ed = editor.current;
    if (!ed) return;
    ed.configure(optsRef.current);
    const moved = lastKey.current !== cellKey;
    lastKey.current = cellKey;
    if (moved || (ed.text !== doc && !ed.view.hasFocus)) ed.reset(doc);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cellKey, doc, canEdit, shownNull, shownDefault]);

  // Request to edit here (double click on a long value, Shift+Enter).
  useEffect(() => {
    const ed = editor.current;
    if (!focusRequest || !ed || !canEdit) return;
    ed.view.focus();
    onFocusTaken?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusRequest, canEdit, cellKey]);

  // Same scheme as the editor/results handle (Studio): height = distance
  // from the bottom.
  const startDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    const el = panel.current;
    // Above is the grid (`.grid-row` in ResultPane): the panel grows at its
    // expense, always leaving it 120px; in a short window pulling up does
    // not shrink it.
    const above = el?.previousElementSibling;
    if (!el || !above) return;
    const bottom = el.getBoundingClientRect().bottom;
    const maxH = el.offsetHeight + Math.max(0, above.clientHeight - 120);
    drag.startDrag(e, (ev) => Math.max(80, Math.min(maxH, Math.round(bottom - ev.clientY))));
  };

  return (
    <div className="inspect-panel" ref={panel} style={height ? { height } : undefined}>
      <div
        className="inspect-grip"
        onPointerDown={startDrag}
        onDoubleClick={drag.reset}
        title={t("Drag to resize, double-click for the default height")}
      />
      <div className="inspect-head">
        <span className="inspect-col" title={meta?.name}>
          {meta?.name ?? t("column {n}", { n: col })}
        </span>
        <span className="inspect-type">{meta?.type_display}</span>
        <span className="inspect-pos">
          {fresh ? t("new row") : t("row {n}", { n: (row + 1).toLocaleString(locale()) })}
        </span>
        {value !== null && (
          <span className="inspect-len">
            {t("{n} characters", { n: value.length.toLocaleString(locale()) })}
          </span>
        )}
        <div className="spacer" />
        {canEdit && <span className="inspect-keys">{t("⌘↵ back to the grid · Esc undoes")}</span>}
        <button
          className="btn small"
          disabled={value === null}
          onClick={() => {
            if (value === null) return;
            void navigator.clipboard.writeText(value).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            });
          }}
        >
          {copied ? t("Copied") : t("Copy")}
        </button>
        {onRowMode && (
          <button
            className="btn small"
            onClick={onRowMode}
            title={t("Show all the fields of the row, to the right of the grid")}
          >
            {t("Row card")}
          </button>
        )}
        <button className="btn small" onClick={onClose} title={t("Close")}>
          <IconX />
        </button>
      </div>
      <div className="inspect-body">
        {!canEdit && (shownNull || shownDefault) && (
          <span className="inspect-null">{shownNull ? "NULL" : "DEFAULT"}</span>
        )}
        {!shownNull && !shownDefault && loading && <span className="hint">{t("loading…")}</span>}
        <div
          className="inspect-editor"
          ref={host}
          hidden={loading || (!canEdit && (shownNull || shownDefault))}
        />
      </div>
    </div>
  );
}
