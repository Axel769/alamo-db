// Results pane of a single statement: grid, banners, errors. Extracted
// from Studio because both the SQL tabs and the Data sub-tab of the table
// tab use it (§9.5) — same events, same rendering.
import { useRef, useState } from "react";
import { ipc, ColumnOut, ErrorOut, PendingValue } from "./ipc";
import { StatementUi, tableMayHaveMore, TabUi } from "./store";
import GridView, { RowActions, ServerSortProps } from "./GridView";
import InspectPanel from "./InspectPanel";
import RowPanel, { RowLinks } from "./RowPanel";
import { locale, t, tMsg } from "./i18n";

export function StatementPane({
  tab,
  st,
  gridHost,
  gridSize,
  onHeaderContext,
  editable,
  cellEditable,
  onEditCell,
  pendingAt,
  canSetNull,
  columnDefault,
  onEditCells,
  onPasteCells,
  onFilterCell,
  cellActions,
  linkCols,
  onFollowLink,
  rowLinks,
  onNearEnd,
  serverSort,
  extraRows,
  focusCell,
  rowActions,
}: {
  tab: TabUi;
  st: StatementUi;
  gridHost: React.RefObject<HTMLDivElement>;
  gridSize: { w: number; h: number };
  /// Context menu on the header (decision 10): only the table tab passes
  /// it — on SQL tabs the right click has no server actions.
  onHeaderContext?: (col: number, x: number, y: number) => void;
  /// M4: cell editing — the table tab with a PK (decision 6) and the SQL
  /// tab when the result comes from one table with its key (§9.5.1).
  editable?: boolean;
  /// Per column (position): computed columns stay read-only.
  cellEditable?: (col: number) => boolean;
  /// `value === null` = set NULL (context menu / Delete).
  onEditCell?: (row: number, col: number, value: PendingValue) => void;
  /// Pending value of the cell, see GridView.
  pendingAt?: (row: number, col: number) => PendingValue | undefined;
  canSetNull?: (col: number) => boolean;
  /// Default of the column (introspection) for "Set DEFAULT".
  columnDefault?: (col: number) => string | null;
  /// Multi-cell and paste (decision 12): only the table tab passes them.
  onEditCells?: (rect: { x: number; y: number; width: number; height: number }, value: PendingValue) => void;
  onPasteCells?: (row: number, col: number, values: string[][]) => void;
  onFilterCell?: (column: string, value: string | null, exclude: boolean) => void;
  cellActions?: (row: number, col: number) => { label: string; onClick: () => void }[];
  /// Foreign keys drawn as links in the grid (table tab), see GridView.
  linkCols?: ReadonlyMap<number, readonly number[]>;
  onFollowLink?: (row: number, col: number, newTab: boolean) => void;
  /// Records linked to the row (foreign keys), for the row card.
  rowLinks?: (row: number) => RowLinks;
  /// Scroll near the last row: the table tab loads more rows.
  onNearEnd?: () => void;
  /// Table tab: header click = server-side sort.
  serverSort?: ServerSortProps;
  /// Pending new rows and cell to bring on screen, see GridView.
  extraRows?: { after: readonly number[]; cell: (i: number, col: number) => PendingValue };
  focusCell?: { col: number; row: number; nonce: number };
  /// Add / Duplicate / Delete row (table tab), see GridView.
  rowActions?: RowActions;
}) {
  const failed = st.outcome?.kind === "failed" ? st.outcome.error : null;
  // Inspected cell, tied to the result set: if the result changes (new
  // execution, another statement) the panel disappears by itself.
  const [inspect, setInspect] = useState<{ resultId: number; row: number; col: number } | null>(
    null
  );
  // M3.5: the result sets live in the tab's session. Disconnected tab = no
  // data (statements cleared on disconnect), here connId is needed only
  // when a resultId exists — so it is never null in those branches.
  const connId = tab.connId;
  // Where to inspect: "cell" = value of the cell at the bottom (InspectPanel),
  // "row" = row card on the right (RowPanel). User preference.
  const [inspectMode, setInspectMode] = useState<"cell" | "row">(() =>
    localStorage.getItem("alamo.inspect") === "row" ? "row" : "cell"
  );
  // With the row card, the cell panel opens below it to edit a long value
  // (the card stays): the result set it was opened for.
  const [cardPanel, setCardPanel] = useState<number | null>(null);
  const switchMode = (m: "cell" | "row") => {
    localStorage.setItem("alamo.inspect", m);
    setInspectMode(m);
    setCardPanel(null);
  };
  // Edit in the cell panel (long value, Shift+Enter): the panel opens on
  // the cell and takes the focus (once: the request is dropped as soon as
  // the panel has taken it).
  const [panelFocus, setPanelFocus] = useState(false);
  const editInPanel = (row: number, col: number) => {
    if (st.resultId === null) return;
    setInspect({ resultId: st.resultId, row, col });
    if (inspectMode === "row") setCardPanel(st.resultId);
    setPanelFocus(true);
  };
  const cellPanel = inspectMode === "cell" || (cardPanel !== null && cardPanel === st.resultId);
  // Back from the panel (⌘↵, Esc): the focus returns to the grid, on the
  // same cell.
  const focusGrid = () =>
    gridHost.current?.querySelector<HTMLElement>('canvas[tabindex="0"]')?.focus();
  // Paged SQL tab (row pause, like DBeaver): scrolling to the bottom asks
  // for the next page. One request per page: `asked` remembers at how many
  // rows it was already asked.
  const asked = useRef<number | null>(null);
  const execId = tab.execId;
  const sqlNextPage =
    !onNearEnd && st.budgetPaused && st.budgetByRows && connId !== null && execId !== null
      ? () => {
          if (asked.current === st.rows) return;
          asked.current = st.rows;
          void ipc.budgetMore(connId, execId);
        }
      : undefined;
  // "Load all" clicked for this execution: the pages bar disappears right
  // away, without waiting for the download to finish.
  const [allFor, setAllFor] = useState<number | null>(null);
  // When paged the bar stays even between one page and the next: appearing
  // and disappearing at every page would make the grid jump while scrolling.
  const paged = st.budgetByRows && (st.budgetPaused || st.streaming) && allFor !== execId;
  // New rows are `st.rows + i`: both panels show their pending values.
  const extraCount = extraRows?.after.length ?? 0;
  const inspecting =
    connId !== null &&
    inspect &&
    inspect.resultId === st.resultId &&
    st.columns &&
    inspect.row < st.rows + extraCount
      ? inspect
      : null;
  const fresh = inspecting !== null && inspecting.row >= st.rows;
  const pendingOf = (row: number, col: number) =>
    row >= st.rows ? extraRows?.cell(row - st.rows, col) : pendingAt?.(row, col);
  const rowEditable = (row: number) => !!editable && !!onEditCell && !rowActions?.deletedAt?.(row);
  return (
    <>
      {st.budgetByRows
        ? paged && <PagesBar tab={tab} st={st} onAll={() => setAllFor(execId)} />
        : st.budgetPaused && <BudgetBanner tab={tab} st={st} />}
      {failed && <ErrorPanel error={failed} />}
      {st.outcome?.kind === "cancelled" && (
        <div className="banner neutral">
          {t("Cancelled by the user.")}{" "}
          {st.rows > 0 && (
            <>
              {t("The {n} rows already received remain readable (partial).", {
                n: st.rows.toLocaleString(locale()),
              })}
            </>
          )}
        </div>
      )}
      {st.outcome?.kind === "affected" && (
        <div className="banner ok">
          {t("{n} rows affected.", { n: st.outcome.rows.toLocaleString(locale()) })}
        </div>
      )}
      {st.outcome?.kind === "void" && <div className="banner ok">{t("Done.")}</div>}
      {st.outcome?.kind === "skipped" && (
        <div className="banner neutral">
          {t("Not executed: a previous statement failed or was cancelled.")}
        </div>
      )}
      {st.notices.length > 0 && (
        <div className="notices">
          {st.notices.map((n, i) => (
            <div key={i} className={"notice " + n.severity}>
              <span className="sev">{n.severity.toUpperCase()}</span> {tMsg(n.message)}
            </div>
          ))}
        </div>
      )}
      <div className="grid-row">
      <div className="grid-host" ref={gridHost}>
        {connId !== null && st.resultId !== null && st.columns && st.rows + extraCount > 0 && (
          <GridView
            connId={connId}
            resultId={st.resultId}
            columns={st.columns}
            rows={st.rows}
            width={gridSize.w}
            height={gridSize.h}
            // Table tab with more pages on the server: the first is "complete"
            // but the local sort sees only the downloaded rows — the badge is needed.
            incomplete={resultIncomplete(st) || tableMayHaveMore(tab)}
            onInspect={(row, col) => {
              if (st.resultId === null) return;
              setInspect({ resultId: st.resultId, row, col });
              setPanelFocus(false);
            }}
            onHeaderContext={onHeaderContext}
            exportName={
              tab.kind === "table" && tab.table
                ? `${tab.table.schema}.${tab.table.name}`
                : (singleSourceTable(st.columns) ?? st.source?.table ?? t("result"))
            }
            widthKey={
              tab.kind === "table" && tab.table
                ? `t:${tab.table.schema}.${tab.table.name}`
                : `s:${tab.id}`
            }
            editable={editable}
            cellEditable={cellEditable}
            onEditInPanel={editable && onEditCell ? editInPanel : undefined}
            onEditCell={onEditCell}
            pendingAt={pendingAt}
            canSetNull={canSetNull}
            columnDefault={columnDefault}
            onEditCells={onEditCells}
            onPasteCells={onPasteCells}
            onFilterCell={onFilterCell}
            cellActions={cellActions}
            linkCols={linkCols}
            onFollowLink={onFollowLink}
            onNearEnd={onNearEnd ?? sqlNextPage}
            serverSort={serverSort}
            extraRows={extraRows}
            focusCell={focusCell}
            rowActions={rowActions}
          />
        )}
        {st.resultId !== null && st.rows === 0 && st.streaming && (
          <div className="hint">{t("waiting for the first rows…")}</div>
        )}
        {st.resultId !== null && st.rows === 0 && extraCount === 0 && !st.streaming && !failed && (
          <div className="hint">{t("0 rows.")}</div>
        )}
      </div>
      {inspecting && inspectMode === "row" && (
        <RowPanel
          connId={connId!}
          resultId={inspecting.resultId}
          columns={st.columns!}
          row={inspecting.row}
          col={inspecting.col}
          onClose={() => {
            setInspect(null);
            setCardPanel(null);
          }}
          onCellMode={() => switchMode("cell")}
          onSelectCol={(col) => setInspect({ ...inspecting, col })}
          links={fresh ? undefined : rowLinks?.(inspecting.row)}
          pendingAt={(col) => pendingOf(inspecting.row, col)}
          fresh={fresh}
          edit={
            rowEditable(inspecting.row)
              ? {
                  onEdit: (col, value) => onEditCell!(inspecting.row, col, value),
                  onEditInPanel: (col) => editInPanel(inspecting.row, col),
                  canSetNull: (col) => canSetNull?.(col) ?? true,
                }
              : undefined
          }
        />
      )}
      </div>
      {inspecting && cellPanel && (
        <InspectPanel
          connId={connId!}
          resultId={inspecting.resultId}
          columns={st.columns!}
          row={inspecting.row}
          col={inspecting.col}
          onClose={inspectMode === "row" ? () => setCardPanel(null) : () => setInspect(null)}
          onRowMode={inspectMode === "cell" ? () => switchMode("row") : undefined}
          editable={rowEditable(inspecting.row)}
          fresh={fresh}
          pending={pendingOf(inspecting.row, inspecting.col)}
          onEdit={onEditCell && ((value) => onEditCell(inspecting.row, inspecting.col, value))}
          focusRequest={panelFocus}
          onFocusTaken={() => setPanelFocus(false)}
          onDone={() => {
            if (inspectMode === "row") setCardPanel(null);
            focusGrid();
          }}
        />
      )}
    </>
  );
}

/// Paged SQL tab (row pause): thin bar, the next rows arrive by scrolling
/// to the bottom; "Load all" removes the pages (export, sort and copy on
/// the whole result). No "Stop": the pause closes by itself with Run,
/// Commit, Rollback, database change or after the timeout.
function PagesBar({ tab, st, onAll }: { tab: TabUi; st: StatementUi; onAll: () => void }) {
  const execId = tab.execId;
  const connId = tab.connId;
  const ok = connId !== null && execId !== null;
  return (
    <div className="banner neutral pages-bar">
      <span>
        {t("First {n} rows — scroll to the bottom to load more.", {
          n: st.rows.toLocaleString(locale()),
        })}
      </span>
      <button
        className="btn small"
        onClick={() => {
          if (!ok) return;
          onAll();
          void ipc.budgetResume(connId, execId);
        }}
      >
        {t("Load all")}
      </button>
    </div>
  );
}

/// Banner of §5: budget reached, the user decides. "Export to file" comes
/// with the export (M4).
function BudgetBanner({ tab, st }: { tab: TabUi; st: StatementUi }) {
  const execId = tab.execId;
  const connId = tab.connId;
  const ok = connId !== null && execId !== null;
  return (
    <div className="banner warn">
      {t("{n} rows loaded, {mb} MB — the soft memory budget has been reached.", {
        n: st.rows.toLocaleString(locale()),
        mb: (st.layoutBytes / 1048576).toFixed(0),
      })}
      <button className="btn" onClick={() => ok && void ipc.budgetResume(connId, execId)}>
        {t("Continue")}
      </button>
      <button className="btn" onClick={() => ok && void ipc.budgetStop(connId, execId)}>
        {t("Stop")}
      </button>
    </div>
  );
}

/// Hierarchical, never concatenated into one line (§6): message, detail,
/// hint, context kept distinct. Click → position in the editor (§6.1).
export function ErrorPanel({ error }: { error: ErrorOut }) {
  const [copied, setCopied] = useState(false);
  // Full text for the Copy button: one line per part, so it can be pasted
  // in a ticket or a chat and read.
  const text = [
    [error.code, tMsg(error.message)].filter(Boolean).join(" "),
    error.detail && tMsg(error.detail),
    error.hint ? `${t("Hint:")} ${tMsg(error.hint)}` : null,
    error.context,
  ]
    .filter(Boolean)
    .join("\n");
  return (
    <div
      className="error-panel"
      onClick={() => {
        // The message can be selected with the mouse: releasing a selection
        // must not move the cursor in the editor.
        if (window.getSelection()?.toString()) return;
        if (error.statement_char_offset !== null) {
          // The editor is a singleton in the Studio module: reach it via an event.
          window.dispatchEvent(
            new CustomEvent("alamo:show-error", {
              detail: { offset: error.statement_char_offset, pos: error.position_char },
            })
          );
        }
      }}
      title={error.statement_char_offset !== null ? t("Click to highlight in the editor") : undefined}
    >
      <div className="error-head">
        {error.code && <span className="error-code">{error.code}</span>}
        <span className="error-message">{tMsg(error.message)}</span>
        <div className="spacer" />
        <button
          className="btn small error-copy"
          title={t("Copy the error message")}
          onClick={(e) => {
            e.stopPropagation();
            void navigator.clipboard.writeText(text).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            });
          }}
        >
          {copied ? t("Copied") : t("Copy")}
        </button>
      </div>
      {error.detail && <div className="error-detail">{tMsg(error.detail)}</div>}
      {error.hint && <div className="error-hint">{t("Hint:")} {tMsg(error.hint)}</div>}
      {error.context && <pre className="error-context">{error.context}</pre>}
    </div>
  );
}

/// §7.3: the incompleteness flag propagates to the local sort — the badge
/// "only the N downloaded rows sorted" comes from here.
export function resultIncomplete(st: StatementUi): boolean {
  if (st.streaming || st.budgetPaused) return true;
  if (st.outcome?.kind === "cancelled") return true;
  if (st.outcome?.kind === "row_set") return st.outcome.state !== "complete";
  return false;
}

/// Name for the INSERT export of a query: the source table if ALL the
/// columns come from it (`SELECT * FROM t`); with joins, computed columns
/// or unknown source (PG) `null` and it stays "result".
export function singleSourceTable(columns: ColumnOut[] | null | undefined): string | null {
  const first = columns?.[0]?.source_table ?? null;
  if (first === null || !columns!.every((c) => c.source_table === first)) return null;
  return first;
}
