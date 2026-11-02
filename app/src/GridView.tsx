// Read-only grid on a result set of the registry (§8, §9.2, §9.6).
import { MutableRefObject, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  CellArray,
  CompactSelection,
  DataEditor,
  DataEditorRef,
  EditableGridCell,
  GridCell,
  GridCellKind,
  GridColumn,
  GridKeyEventArgs,
  GridSelection,
  CellClickedEventArgs,
  HeaderClickedEventArgs,
  Item,
  Rectangle,
  Theme,
} from "@glideapps/glide-data-grid";
import "@glideapps/glide-data-grid/dist/index.css";
import { ColumnOut, DEFAULT_CELL, ExportFormat, ipc, OrderBy, PendingValue, WindowOut } from "./ipc";
import { locale, t } from "./i18n";
import { BLOCK, cacheFor } from "./rowCache";
import { NewRowSlot, rowLayout } from "./rowLayout";
import { pasteValues, toTsv } from "./tsv";
import { tokens } from "./theme";
import { ExportDialog } from "./ExportDialog";

const ROW_HEIGHT = 26; // compact density §9.6.2
const HEADER_HEIGHT = 28;
/// Right end of the header that sorts (like DBeaver's arrows): a click
/// there sorts, a click on the rest of the header selects the column.
const SORT_ZONE = 24;
/// Right end of a foreign-key cell with the link arrow: a click there opens
/// the linked record, a click on the rest of the cell selects it.
const LINK_ZONE = 18;

type Props = {
  /// Connection of the result set (M3.5): result_id is a per-session counter.
  connId: number;
  resultId: number;
  columns: ColumnOut[];
  rows: number;
  width: number;
  height: number;
  /// §7.3: the content does not represent the whole result (streaming,
  /// truncated, cancelled) — the sort must say so with the badge.
  incomplete?: boolean;
  /// Current cell changed (click or arrows) → inspection panel.
  onInspect?: (row: number, col: number) => void;
  /// Right click on the header (decision 10): the server-side actions live
  /// in the context menu, never on the local sort click. Table tab only.
  onHeaderContext?: (col: number, x: number, y: number) => void;
  /// Suggested name for export and INSERT: "schema.table" for the table
  /// tabs, a generic name for SQL result sets.
  exportName?: string;
  /// Key for the memory of the manually resized column widths: per column
  /// name, survives reloads and tab switches (in memory only, not persisted
  /// with the workspace). Table tab: "schema.table"; SQL tab: the tab id.
  widthKey?: string;
  /// M4: cell editing — only the table tab with a PK passes it (decision 6).
  /// Truncated cells (>4KB, LOB) never open the overlay: it would show a
  /// partial value and saving would write it maimed.
  editable?: boolean;
  /// Edit the cell in the cell panel (whole value, room to write): double
  /// click / Enter on a long value (see `isLong`) and Shift+Enter on any
  /// cell, new rows included. With it, long values never open the overlay.
  onEditInPanel?: (row: number, col: number) => void;
  /// The cell was edited in the overlay (`value` string) or must be set to
  /// NULL (`value === null`, from the context menu or Delete): the caller
  /// puts it in pending and the grid shows it through `pendingAt`.
  onEditCell?: (row: number, col: number, value: PendingValue) => void;
  /// Pending value (edited, not yet saved) of the cell, or `undefined` if
  /// there is none. The cell shows it instead of the read one, highlighted.
  /// New identity = changes changed: everything is redrawn.
  pendingAt?: (row: number, col: number) => PendingValue | undefined;
  /// The column accepts NULL (from introspection): decides whether "Set NULL"
  /// can be offered. Absent = offered anyway, the server decides.
  canSetNull?: (column: string) => boolean;
  /// Multi-cell (decision 12, single rectangle): same value on the whole
  /// selected rectangle (`value === null` = SET NULL from Delete/menu).
  /// The cells end up pending as with `onEditCell`.
  onEditCells?: (rect: Rectangle, value: PendingValue) => void;
  /// Default of the column from introspection (`null` = none): the "Set
  /// DEFAULT" menu shows it in the hint.
  columnDefault?: (column: string) => string | null;
  /// "Filter by this value" from the cell menu (table tab only):
  /// `value === null` = the cell is NULL, `exclude` = negated filter.
  onFilterCell?: (column: string, value: string | null, exclude: boolean) => void;
  /// Extra actions for the cell, at the top of the context menu (table tab:
  /// "go to the linked record" on foreign keys). `[]` = none.
  cellActions?: (row: number, col: number) => { label: string; onClick: () => void }[];
  /// Columns with an outgoing foreign key (table tab), each with the
  /// columns of its key: the cell shows the link arrow when the whole key
  /// has a saved value (not NULL, not edited). New identity = redraw.
  linkCols?: ReadonlyMap<number, readonly number[]>;
  /// Click on the link arrow (row in the caller's coordinates); ⌘/Ctrl+click
  /// = new tab, like the row card.
  onFollowLink?: (row: number, col: number, newTab: boolean) => void;
  /// Paste (Cmd+V): rectangle of values from the clipboard anchored at the
  /// cell `[col, row]`. Raw strings: '' stays an empty string, never NULL.
  onPasteCells?: (row: number, col: number, values: string[][]) => void;
  /// The scroll got near the last row (table tab: loads the next page).
  /// Does not fire with the local sort active: the new rows would mix with
  /// the ones under the user's eyes.
  onNearEnd?: () => void;
  /// Table tab: the header click sorts on the server (like DBeaver), the
  /// local sort stays in the menu. Absent = SQL tab, where the click is the
  /// local sort (the user's SQL is not rewritten, §5.2).
  serverSort?: ServerSortProps;
  /// New rows not yet saved (table tab, "+ Row"), on a "new" background:
  /// the i-th right below view row `after[i]` (`rowLayout`), `cell(i, col)`
  /// the value. For the caller it is row `rows + i`, wherever it shows.
  /// New identity = rows changed: everything is redrawn.
  extraRows?: { after: readonly number[]; cell: (i: number, col: number) => PendingValue };
  /// Brings on screen and selects a cell (row `rows + i` for the new ones):
  /// the row just added. Different `nonce` = new request.
  focusCell?: { col: number; row: number; nonce: number };
  /// Row actions of the table tab, like DBeaver: rows in the caller's
  /// coordinates (view for the real ones, `rows + i` for the new ones).
  /// Keys: ⌥Insert adds, ⌃⌥Insert duplicates, Delete (or ⌥Delete) deletes;
  /// ⇧Delete sets NULL, the old Delete.
  rowActions?: RowActions;
};

export type RowActions = {
  onAdd: () => void;
  onDuplicate: (rows: number[]) => void;
  onDelete: (rows: number[]) => void;
  /// Real row marked for deletion (not yet saved): red and struck through.
  /// New identity = marks changed: everything is redrawn.
  deletedAt?: (row: number) => boolean;
  /// Rows touched by the selection, for the buttons of the bar.
  onSelectedRows: (rows: number[]) => void;
  /// Set by the grid at every render, for the table tab.
  handleRef: MutableRefObject<RowsHandle | null>;
};

export type RowsHandle = {
  /// Where "+ Row" and "Duplicate" put the new rows: below the last row of
  /// the selection, else above the first row on screen.
  slot(): NewRowSlot;
  /// Screen row of the current cell once the save has written everything:
  /// the rows to delete above it leave, the new ones become real rows in
  /// place. `null` = no current cell.
  cursorAfterSave(): Item | null;
};

export type SortSpec = { col: number; desc: boolean };

export type ServerSortProps = {
  /// Current server-side sort: arrows (and priority) in the headers.
  order: OrderBy[];
  /// Header click; `additive` = ⌘/Ctrl or Shift (adds the column).
  onHeaderSort: (col: number, additive: boolean) => void;
  /// Local sort requested from the header menu: every request has a new
  /// `nonce`; `desc: null` removes it.
  /// Valid only for the given result set: an old request is not reapplied
  /// to a new result (remount, reload).
  localRequest: { resultId: number; col: number; desc: boolean | null; nonce: number } | null;
  /// The local sort changed (also reset by a new result).
  onLocalSortChange: (s: SortSpec | null) => void;
};

/// Rows from the end within which the next page is requested: early enough
/// to find it ready with a normal scroll.
const NEAR_END = 100;

// Glide's copy, cut and paste turned off: the text-only handler inside
// GridView replaces them.
const GRID_KEYBINDINGS = { copy: false, cut: false, paste: false };

/// Cells taken out of the whole columns selected from the header: rows
/// (screen) by column. The columns stay columns: Delete does not touch the
/// rows and the rows loaded later are included.
type ColumnCuts = ReadonlyMap<number, ReadonlySet<number>>;
const NO_CUTS: ColumnCuts = new Map();

/// The key that adds to a selection, the same one Glide reads.
const isMultiKey = (e: { metaKey: boolean; ctrlKey: boolean }) =>
  /Mac/.test(navigator.platform) ? e.metaKey : e.ctrlKey;

const EMPTY_SELECTION: GridSelection = {
  columns: CompactSelection.empty(),
  rows: CompactSelection.empty(),
};

export default function GridView({
  connId,
  resultId,
  columns,
  rows,
  width,
  height,
  incomplete = false,
  onInspect,
  onHeaderContext,
  exportName,
  widthKey,
  editable = false,
  onEditInPanel,
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
  onNearEnd,
  serverSort,
  extraRows,
  focusCell,
  rowActions,
}: Props) {
  const gridRef = useRef<DataEditorRef | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  // Controlled selection: needed to intercept the current cell. It changes
  // at the user's pace, not the scroll's: the re-render costs nothing.
  const [selection, setSelection] = useState<GridSelection>(EMPTY_SELECTION);
  useEffect(() => setSelection(EMPTY_SELECTION), [resultId]);
  const [cuts, setCuts] = useState<ColumnCuts>(NO_CUTS);
  useEffect(() => {
    if (selection.columns.length === 0) setCuts(NO_CUTS);
  }, [selection.columns]);
  /// ⌘/Ctrl held on the press in progress. Read from the capture phase:
  /// Glide changes the selection on mouse down, on `window`, afterwards.
  const multiPress = useRef(false);
  // On the next tick: Glide's click, on the same mouse up, reads the flag.
  useEffect(() => {
    const end = () => window.setTimeout(() => (multiPress.current = false));
    window.addEventListener("mouseup", end);
    return () => window.removeEventListener("mouseup", end);
  }, []);
  const regionRef = useRef({ y: 0, height: 60 });
  // Cell of the last editing commit: the selection must not advance (the
  // columns selected from the header stay selected).
  const keepSelection = useRef<{ cell: Item; columns: CompactSelection } | null>(null);
  const tok = useMemo(tokens, []);

  // Local sort (decision 10): the permutation lives in Rust, here only the
  // user's choice. `viewRows` is how many rows the view covers — on a still
  // open stream it may lag behind until recomputed.
  const [sort, setSort] = useState<SortSpec | null>(null);
  const [viewRows, setViewRows] = useState<number | null>(null);
  const [sortEpoch, setSortEpoch] = useState(0);
  useEffect(() => {
    setSort(null);
    setViewRows(null);
  }, [resultId]);

  // §9.6: the type color is resolved once per column, never per cell; the
  // overrides are frozen objects, never allocated in getCellContent.
  const colThemes = useMemo(() => {
    const tint: Record<string, string | undefined> = {
      numeric: tok.typeNumeric,
      temporal: tok.typeTemporal,
      boolean: tok.typeBoolean,
      structured: tok.typeStructured,
      text: undefined,
    };
    return columns.map((c) => {
      const color = tint[c.semantic_class];
      return color ? Object.freeze({ textDark: color }) : undefined;
    });
  }, [columns, tok]);
  const nullTheme = useMemo(() => Object.freeze({ textDark: tok.cellNullFg }), [tok]);
  const pendingTheme = useMemo(() => Object.freeze({ bgCell: tok.stateModified }), [tok]);
  const pendingNullTheme = useMemo(
    () => Object.freeze({ bgCell: tok.stateModified, textDark: tok.cellNullFg }),
    [tok]
  );
  const newTheme = useMemo(() => Object.freeze({ bgCell: tok.stateNew }), [tok]);
  // Only the function, not the actions object (new at every render of the
  // caller): getCellContent must change only when the marks change.
  const deletedAt = rowActions?.deletedAt;
  const deletedTheme = useMemo(
    () => Object.freeze({ bgCell: tok.stateDeleted, textDark: tok.cellMutedFg }),
    [tok]
  );
  const newNullTheme = useMemo(
    () => Object.freeze({ bgCell: tok.stateNew, textDark: tok.cellNullFg }),
    [tok]
  );

  // Manually resized widths: per column name, in a map outside React that
  // survives reloads (new resultId) and tab switches — a widened UUID stays
  // widened after the UPDATE that reloads the grid.
  const savedWidths = widthsFor(widthKey ?? `conn:${connId}`);
  // Content-fitted widths (first rows): like the manual ones, per column
  // name and outside React — a reload does not make the grid jump.
  const fitWidths = fitWidthsFor(widthKey ?? `conn:${connId}`);
  const [widthEpoch, setWidthEpoch] = useState(0);
  const gridColumns = useMemo<GridColumn[]>(
    () =>
      columns.map((c) => ({
        title: c.name,
        width:
          savedWidths.get(c.name) ??
          fitWidths.get(c.name) ??
          Math.min(320, Math.max(70, 24 + c.name.length * 8, widthHint(c))),
      })),
    // widthEpoch: bumped on resize — the map is mutable, an identity is needed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [columns, savedWidths, fitWidths, widthEpoch]
  );

  // Sort mark drawn in the sort zone of the header (not in the title: the
  // zone is also the click target); on hover an empty "⇅" shows where to
  // click. Default drawing first, then the zone is painted over so a long
  // name never runs under the mark.
  const serverOrder = serverSort?.order;
  const drawHeader = useCallback(
    (
      args: {
        ctx: CanvasRenderingContext2D;
        columnIndex: number;
        theme: Theme;
        rect: Rectangle;
        isHovered: boolean;
      },
      drawContent: () => void
    ) => {
      drawContent();
      const { ctx, rect, theme, columnIndex, isHovered } = args;
      const name = columns[columnIndex]?.name;
      if (name === undefined) return;
      const mark = headerMark(name, columnIndex, sort, serverOrder).trim();
      if (mark === "" && !isHovered) return;
      ctx.save();
      ctx.fillStyle = isHovered ? theme.bgHeaderHovered : theme.bgHeader;
      ctx.fillRect(rect.x + rect.width - SORT_ZONE, rect.y, SORT_ZONE, rect.height);
      ctx.font = `${theme.headerFontStyle} ${theme.fontFamily}`;
      ctx.textAlign = "right";
      ctx.textBaseline = "middle";
      ctx.fillStyle = theme.textHeader;
      if (mark === "") ctx.globalAlpha = 0.45;
      ctx.fillText(mark === "" ? "⇅" : mark, rect.x + rect.width - 6, rect.y + rect.height / 2 + 1);
      ctx.restore();
    },
    [columns, sort, serverOrder]
  );

  const onColumnResize = useCallback(
    (_column: GridColumn, newSize: number, colIndex: number) => {
      const name = columns[colIndex]?.name;
      if (!name) return;
      savedWidths.set(name, newSize);
      setWidthEpoch((e) => e + 1);
    },
    [columns, savedWidths]
  );

  const cache = cacheFor(connId, resultId, columns.length);

  // Cell memo: rebuilt DURING the render when the result set changes, the
  // rows grow, the sort changes or the PK arrives (`editable`). Clearing it
  // in a useEffect was too late: Glide (child) paints before the parent's
  // effect runs, and the first frame showed the cells of the previous result set.
  const cellMemo = useMemo(
    () => new Map<number, GridCell>(),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [cache, columns.length, rows, sortEpoch, editable, pendingAt, extraRows, deletedAt]
  );

  const applySort = useCallback(
    async (next: SortSpec | null) => {
      try {
        const out = await ipc.sortResult(connId, resultId, next?.col ?? null, next?.desc ?? false);
        cache.clear();
        setSort(next);
        setViewRows(next ? out.rows : null);
        setSortEpoch((e) => e + 1);
      } catch (e) {
        console.error(e);
      }
    },
    [connId, resultId, cache]
  );

  // Header click, like DBeaver: on the name the column is selected (Glide's
  // selection, ⌘/Ctrl adds, Shift extends); on the sort zone at the right
  // end it sorts (decision 10). Table tab: server-side sort, ⌘/Ctrl+click or
  // Shift+click adds the column. SQL tab: local sort, ascending → descending →
  // natural.
  const onHeaderClicked = useCallback(
    (col: number, e: HeaderClickedEventArgs) => {
      if (e.localEventX < e.bounds.width - SORT_ZONE) return;
      e.preventDefault();
      if (serverSort) {
        serverSort.onHeaderSort(col, e.shiftKey || e.metaKey || e.ctrlKey);
        return;
      }
      if (sort?.col !== col) void applySort({ col, desc: false });
      else if (!sort.desc) void applySort({ col, desc: true });
      else void applySort(null);
    },
    [sort, applySort, serverSort]
  );

  // Local sort requested from the header menu (table tab).
  const localNonce = serverSort?.localRequest?.nonce;
  useEffect(() => {
    const r = serverSort?.localRequest;
    if (!r || r.resultId !== resultId) return;
    void applySort(r.desc === null ? null : { col: r.col, desc: r.desc });
    // Only on a new request (nonce), not at every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [localNonce]);
  const onLocalSortChange = serverSort?.onLocalSortChange;
  useEffect(() => {
    onLocalSortChange?.(sort);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sort]);

  // Stream grown with the view active: the permutation covers only the rows
  // of that time, it is recomputed on the current ones.
  const sortRef = useRef(sort);
  sortRef.current = sort;
  useEffect(() => {
    if (sortRef.current) void applySort(sortRef.current);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rows]);

  // Rows of the result set on screen (with the local sort they may be
  // fewer), with the new rows among them: screen rows ≠ view rows.
  const baseRows = viewRows ?? rows;
  const layout = useMemo(() => rowLayout(baseRows, extraRows?.after ?? []), [baseRows, extraRows]);
  const displayRows = layout.count;
  const isExtra = useCallback((row: number) => layout.extraAt(row) >= 0, [layout]);
  /// Row for the caller: the view's, or `rows + i` for the new ones
  /// regardless of the sort.
  const toModelRow = useCallback(
    (row: number) => {
      const i = layout.extraAt(row);
      return i >= 0 ? rows + i : layout.baseAt(row);
    },
    [layout, rows]
  );
  const fromModelRow = useCallback(
    (row: number) => (row >= rows ? layout.rowOfExtra(row - rows) : layout.rowOfBase(row)),
    [layout, rows]
  );
  const extraAt = useCallback(
    (row: number, col: number): PendingValue | undefined => {
      const i = layout.extraAt(row);
      return i < 0 ? undefined : extraRows?.cell(i, col);
    },
    [extraRows, layout]
  );
  /// Cell as the grid sees it: from the cache for the real rows, from the
  /// pending value for the new ones (never truncated).
  const cellAt = useCallback(
    (row: number, col: number) => {
      if (!isExtra(row)) return cache.cell(layout.baseAt(row), col);
      const p = extraAt(row, col);
      if (p === undefined) return undefined;
      return { v: typeof p === "string" ? p : "", isNull: typeof p !== "string", truncated: false };
    },
    [cache, layout, isExtra, extraAt]
  );
  const requestRow = useCallback(
    (row: number) => {
      if (!isExtra(row)) cache.request(layout.baseAt(row));
    },
    [cache, layout, isExtra]
  );
  /// Pending value: of the edited cell, or of the new row.
  const pendingValue = useCallback(
    (row: number, col: number): PendingValue | undefined =>
      isExtra(row) ? extraAt(row, col) : pendingAt?.(layout.baseAt(row), col),
    [layout, isExtra, extraAt, pendingAt]
  );
  /// Screen rows `y..y + height` in the caller's rows: runs contiguous
  /// for the caller, view rows and new rows apart. `offset` = first
  /// screen row of the run minus `y`.
  const modelRuns = useCallback(
    (y: number, height: number) => {
      const runs: { offset: number; y: number; height: number }[] = [];
      const end = Math.min(y + height, displayRows);
      for (let d = y; d < end; d++) {
        const m = toModelRow(d);
        const last = runs[runs.length - 1];
        if (last && m === last.y + last.height && (m < rows) === (last.y < rows)) last.height++;
        else runs.push({ offset: d - y, y: m, height: 1 });
      }
      return runs;
    },
    [displayRows, toModelRow, rows]
  );
  const editRect = useCallback(
    (r: Rectangle, value: PendingValue) => {
      if (!onEditCells) return;
      for (const run of modelRuns(r.y, r.height))
        onEditCells({ x: r.x, y: run.y, width: r.width, height: run.height }, value);
    },
    [onEditCells, modelRuns]
  );
  /// Paste of a block of values from screen row `y`, split like editRect.
  const pasteAt = useCallback(
    (y: number, x: number, values: string[][]) => {
      if (!onPasteCells) return;
      for (const run of modelRuns(y, values.length))
        onPasteCells(run.y, x, values.slice(run.offset, run.offset + run.height));
    },
    [onPasteCells, modelRuns]
  );
  /// Columns selected from the header (like DBeaver): the same value on
  /// every row on screen of each one, new rows included.
  const editColumns = useCallback(
    (cols: CompactSelection, value: PendingValue) => {
      for (const x of cols) {
        let y = 0;
        for (const cut of [...(cuts.get(x) ?? [])].sort((a, b) => a - b)) {
          if (cut > y) editRect({ x, y, width: 1, height: cut - y }, value);
          y = cut + 1;
        }
        if (displayRows > y) editRect({ x, y, width: 1, height: displayRows - y }, value);
      }
    },
    [editRect, displayRows, cuts]
  );
  // Row just added: on screen, selected and with the keyboard focus on the
  // grid (the click on the button had taken it there): you can type right
  // away in the first cell. The focus must be given AFTER Glide received
  // the selection: a focused grid without a current cell picks the first
  // visible row by itself, and it would win.
  const focusAfterSelect = useRef(false);
  useEffect(() => {
    if (!focusCell) return;
    const cell: Item = [focusCell.col, fromModelRow(focusCell.row)];
    gridRef.current?.scrollTo(cell[0], cell[1]);
    focusAfterSelect.current = true;
    setSelection({
      columns: CompactSelection.empty(),
      rows: CompactSelection.empty(),
      current: { cell, range: { x: cell[0], y: cell[1], width: 1, height: 1 }, rangeStack: [] },
    });
    // The panels follow, as for a selection made by the user: the row card
    // open while adding a row shows the new one.
    onInspect?.(focusCell.row, focusCell.col);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusCell?.nonce]);
  useEffect(() => {
    if (!focusAfterSelect.current || !selection.current) return;
    focusAfterSelect.current = false;
    gridRef.current?.focus();
  }, [selection]);
  // First block arrived (or already cached on remount): the columns that do
  // not yet have a fitted width are measured.
  useEffect(() => {
    const w = cache.peekBlock(0);
    if (w && fitColumns(columns, w, fitWidths)) setWidthEpoch((e) => e + 1);
  }, [cache, columns, fitWidths]);

  useEffect(() => {
    cache.onBlock = (_b, w) => {
      if (w.row_start === 0 && fitColumns(columns, w, fitWidths)) setWidthEpoch((e) => e + 1);
      // A block may arrive again with changed values (rows re-read after a
      // save): the memo of those rows is no longer valid.
      // Memo and Glide count screen rows, the block view rows.
      const vis = regionRef.current;
      const cells: { cell: Item }[] = [];
      for (let r = w.row_start; r < w.row_end; r++) {
        const d = layout.rowOfBase(r);
        for (let c = 0; c < columns.length; c++) cellMemo.delete(d * columns.length + c);
        if (d >= vis.y - 30 && d < vis.y + vis.height + 30)
          for (let c = 0; c < columns.length; c++) cells.push({ cell: [c, d] });
      }
      if (cells.length > 0) gridRef.current?.updateCells(cells);
    };
  }, [cache, cellMemo, columns, fitWidths, layout]);

  // The grid got shorter (the cell panel opens under it, the window
  // shrinks): the selected cell, on screen before, must stay on screen —
  // a click on one of the last rows hid it under the panel. The top row
  // does not move, the old height says what was visible.
  const prevHeight = useRef(height);
  useEffect(() => {
    const was = prevHeight.current;
    prevHeight.current = height;
    const cell = selection.current?.cell;
    if (height >= was || !cell) return;
    const top = regionRef.current.y;
    const rowsBefore = Math.floor((was - HEADER_HEIGHT) / ROW_HEIGHT);
    if (cell[1] < top || cell[1] >= top + rowsBefore) return;
    requestAnimationFrame(() => gridRef.current?.scrollTo(cell[0], cell[1], "vertical"));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [height]);

  // The stream grew: the partial blocks must be redone and so must the memo
  // of the cells at the end.
  useEffect(() => {
    cache.invalidatePartial(rows);
  }, [cache, rows]);

  const glideTheme = useMemo<Partial<Theme>>(
    () => ({
      accentColor: tok.cellSelectionBorder,
      accentLight: tok.stateSelection,
      bgCell: tok.stateNormal,
      bgCellMedium: tok.stateAlt,
      textDark: tok.cellFg,
      textMedium: tok.cellMutedFg,
      textLight: tok.cellMutedFg,
      bgHeader: tok.cellHeaderBg,
      bgHeaderHasFocus: tok.cellHeaderBg,
      bgHeaderHovered: tok.stateAlt,
      textHeader: tok.cellFg,
      borderColor: tok.cellBorder,
      horizontalBorderColor: tok.cellBorder,
      fontFamily: "ui-monospace, SFMono-Regular, monospace",
      baseFontStyle: "13px",
      headerFontStyle: "500 13px",
      cellVerticalPadding: 3,
    }),
    [tok]
  );

  const getCellContent = useCallback(
    (cell: Item): GridCell => {
      const [col, row] = cell;
      // Unique key per cell: with a fixed `* 64`, beyond 64 columns the cell
      // (row, 64) collided with (row + 1, 0).
      const key = row * columns.length + col;
      const memo = cellMemo.get(key);
      if (memo !== undefined) return memo;
      const meta = columns[col];
      const alignRight = meta.semantic_class === "numeric" || meta.numeric_text;
      if (isExtra(row)) {
        // New row: only pending values, on a "new" (green) background.
        const p = extraAt(row, col);
        if (p === undefined) return LOADING;
        const isText = typeof p === "string";
        const out: GridCell = {
          kind: GridCellKind.Text,
          data: isText ? p : "",
          displayData: isText ? clamp(p) : p === null ? "NULL" : "DEFAULT",
          allowOverlay: editable && !(onEditInPanel && isText && isLong(p)),
          contentAlign: alignRight ? "right" : "left",
          themeOverride: isText ? newTheme : newNullTheme,
        };
        cellMemo.set(key, out);
        return out;
      }
      const base = layout.baseAt(row);
      const v = cache.cell(base, col);
      if (v === undefined) {
        cache.request(base);
        return LOADING;
      }
      if (deletedAt?.(base)) {
        // To delete on save: read value, red and struck through (the line
        // is drawn by drawCell), not editable.
        const out: GridCell = {
          kind: GridCellKind.Text,
          data: v.isNull ? "" : v.v,
          displayData: v.isNull ? "NULL" : clamp(v.v),
          allowOverlay: false,
          contentAlign: alignRight ? "right" : "left",
          themeOverride: deletedTheme,
        };
        cellMemo.set(key, out);
        return out;
      }
      const p = pendingAt?.(base, col);
      if (p !== undefined) {
        // Edited and not saved: the new value, on a "modified" background.
        // NULL and DEFAULT are words, not values: dimmed rendering.
        const isText = typeof p === "string";
        const out: GridCell = {
          kind: GridCellKind.Text,
          data: isText ? p : "",
          displayData: isText ? clamp(p) : p === null ? "NULL" : "DEFAULT",
          allowOverlay: editable && !(onEditInPanel && isText && isLong(p)),
          contentAlign: alignRight ? "right" : "left",
          themeOverride: isText ? pendingTheme : pendingNullTheme,
        };
        cellMemo.set(key, out);
        return out;
      }
      // NULL: state, not type — identical rendering in every column (§9.6 rule 2).
      // Truncated values (4KB) are longer than CLAMP: `clamp` adds the "…"
      // and turns the newlines into "↵" (raw, Glide stopped at the first one).
      // Long values are edited in the cell panel (see onEditInPanel).
      const out: GridCell = {
        kind: GridCellKind.Text,
        data: v.isNull ? "" : v.v,
        displayData: v.isNull ? "NULL" : clamp(v.v),
        allowOverlay: editable && !v.truncated && !(onEditInPanel && !v.isNull && isLong(v.v)),
        contentAlign: alignRight ? "right" : "left",
        themeOverride: v.isNull ? nullTheme : colThemes[col],
      };
      cellMemo.set(key, out);
      return out;
    },
    // New memo → new identity → Glide invalidates all the rendered cells.
    [cache, cellMemo, columns, colThemes, nullTheme, pendingAt, pendingTheme, pendingNullTheme, editable, layout, isExtra, extraAt, newTheme, newNullTheme, deletedAt, deletedTheme, onEditInPanel]
  );

  /// Row that can be edited in the panel (not deleted; new ones too).
  const panelEditable = useCallback(
    (row: number) => editable && !!onEditInPanel && (isExtra(row) || !deletedAt?.(layout.baseAt(row))),
    [editable, onEditInPanel, deletedAt, isExtra, layout]
  );

  // Double click / Enter: Glide calls this also when the overlay stays
  // closed. Long value → edited in the panel.
  const onCellActivated = useCallback(
    ([col, row]: Item) => {
      if (!panelEditable(row)) return;
      const v = cellAt(row, col);
      if (v === undefined) return;
      const p = pendingValue(row, col);
      const long = typeof p === "string" ? isLong(p) : p === undefined && !v.isNull && (v.truncated || isLong(v.v));
      if (long) onEditInPanel!(toModelRow(row), col);
    },
    [panelEditable, cellAt, pendingValue, toModelRow, onEditInPanel]
  );

  // Shift+Enter (like DBeaver's value editor): any cell in the panel.
  const onGridKeyDown = useCallback(
    (e: GridKeyEventArgs) => {
      if (e.key !== "Enter" || !e.shiftKey || e.metaKey || e.ctrlKey || e.altKey) return;
      const cur = selection.current;
      if (!cur || cur.range.width !== 1 || cur.range.height !== 1) return;
      const [col, row] = cur.cell;
      if (!panelEditable(row)) return;
      e.cancel();
      onEditInPanel!(toModelRow(row), col);
    },
    [selection, panelEditable, toModelRow, onEditInPanel]
  );

  // Save from Glide's overlay: the cell goes pending (no write to the
  // database until saving).
  // NULL → "" is the overlay's identity: leaving a NULL cell without
  // touching it must not propose an UPDATE to an empty string.
  const onCellEdited = useCallback(
    (cell: Item, newValue: EditableGridCell) => {
      if (!onEditCell || newValue.kind !== GridCellKind.Text) return;
      const [col, row] = cell;
      const cur = cellAt(row, col);
      // After the commit Glide moves the selection one row down (Excel
      // style): the selection sliding silently opens the next editor on
      // the wrong row. It is kept on the edited cell — also on commits
      // without changes, which would advance anyway. The flag dies at the
      // next frame so as not to capture normal navigation.
      keepSelection.current = { cell, columns: selection.columns };
      requestAnimationFrame(() => (keepSelection.current = null));
      // Multi-cell (decision 12): overlay commit with a rectangle larger
      // than the cell selected → same value on the whole rectangle. The
      // edited cell must be inside the
      // rectangle: an overlay opened elsewhere has nothing to do with the selection.
      if (onEditCells && selection.columns.hasIndex(col)) {
        editColumns(selection.columns, newValue.data);
        return;
      }
      const r = selection.current?.range;
      if (
        onEditCells &&
        r &&
        (r.width > 1 || r.height > 1) &&
        col >= r.x &&
        col < r.x + r.width &&
        row >= r.y &&
        row < r.y + r.height
      ) {
        editRect(r, newValue.data);
        return;
      }
      if (cur === undefined) return;
      const next = newValue.data;
      // Compared with what the cell shows: the pending value, if any —
      // rewriting the original must be able to undo the change.
      // Pending DEFAULT: the overlay starts empty, leaving without typing
      // does not turn it into an empty string.
      const p = pendingValue(row, col);
      const shownNull = p !== undefined ? p === null || p === DEFAULT_CELL : cur.isNull;
      const shown = typeof p === "string" ? p : p !== undefined ? "" : cur.v;
      if (shownNull ? next === "" : next === shown) return;
      onEditCell(toModelRow(row), col, next);
    },
    [cellAt, onEditCell, onEditCells, editRect, editColumns, pendingValue, toModelRow, selection]
  );

  /// Single selected cell (and editable grid): target of "Set NULL" —
  /// context menu and Delete key.
  const nullTarget = useCallback((): Item | null => {
    if (!editable || !onEditCell) return null;
    const cur = selection.current;
    if (!cur || cur.rangeStack.length > 0) return null;
    if (cur.range.width !== 1 || cur.range.height !== 1) return null;
    if (selection.rows.length > 0 || selection.columns.length > 0) return null;
    return cur.cell;
  }, [editable, onEditCell, selection]);

  const proposeNull = useCallback(() => {
    const cell = nullTarget();
    if (!cell || !onEditCell) return;
    const [col, row] = cell;
    const v = cellAt(row, col);
    if (v === undefined) return;
    const p = pendingValue(row, col);
    if (p !== undefined ? p === null : v.isNull) return;
    if (canSetNull && !canSetNull(columns[col]?.name ?? "")) return;
    onEditCell(toModelRow(row), col, null);
  }, [nullTarget, onEditCell, pendingValue, cellAt, toModelRow, canSetNull, columns]);

  // Delete/Backspace: Glide by itself would empty the selected cells
  // (onCellEdited with an empty string, without going through the overlay).
  // Always blocked: '' is not NULL. Single cell → pending NULL; rectangle
  // → pending NULL on all of it (decision 12), same deal.
  const onDelete = useCallback(
    (sel: GridSelection): boolean => {
      if (onEditCells && editable && sel.columns.length > 0) {
        editColumns(sel.columns, null);
        return false;
      }
      const r = sel.current?.range;
      if (onEditCells && editable && r && (r.width > 1 || r.height > 1)) {
        editRect(r, null);
        return false;
      }
      proposeNull();
      return false;
    },
    [proposeNull, onEditCells, editRect, editColumns, editable]
  );

  /// Rows touched by the selection (whole rows, rectangles), in the
  /// caller's coordinates, sorted and without duplicates.
  const selectedRows = useCallback(
    (sel: GridSelection): number[] => {
      const set = new Set<number>();
      for (const r of sel.rows) set.add(r);
      // Whole columns: their current cell (see onGridSelectionChange) does
      // not make its row a target — Delete would delete it.
      if (sel.current && sel.columns.length === 0) {
        for (const rect of [sel.current.range, ...sel.current.rangeStack])
          for (let r = rect.y; r < rect.y + rect.height; r++) set.add(r);
      }
      return [...set]
        .filter((r) => r < displayRows)
        .sort((a, b) => a - b)
        .map(toModelRow);
    },
    [displayRows, toModelRow]
  );
  const onSelectedRows = rowActions?.onSelectedRows;
  if (rowActions)
    rowActions.handleRef.current = {
      slot: () => {
        let last = -1;
        for (const r of selection.rows) last = Math.max(last, r);
        const cur = selection.current;
        if (cur) for (const rect of [cur.range, ...cur.rangeStack]) last = Math.max(last, rect.y + rect.height - 1);
        return layout.below(last >= 0 ? Math.min(last, displayRows - 1) : regionRef.current.y - 1);
      },
      cursorAfterSave: () => {
        const cell = selection.current?.cell;
        if (!cell) return null;
        let gone = 0;
        for (let v = 0; v < layout.baseAt(cell[1]); v++) if (rowActions.deletedAt?.(v)) gone++;
        return [cell[0], cell[1] - gone];
      },
    };
  useEffect(() => {
    onSelectedRows?.(selectedRows(selection));
  }, [selection, selectedRows, onSelectedRows]);

  // Row keys like DBeaver, before Glide (capture): Delete deletes the rows
  // of the selection instead of setting NULL; NULL stays on ⇧Delete.
  // Mac without an Insert key: "Help" is the same key on extended keyboards.
  const rowKeysRef = useRef<(e: KeyboardEvent) => void>(() => {});
  rowKeysRef.current = (e: KeyboardEvent) => {
    if (!rowActions || !editable) return;
    if (!wrapRef.current?.contains(document.activeElement)) return;
    const insert = e.key === "Insert" || e.key === "Help";
    const del = e.key === "Delete" || e.key === "Backspace";
    const handled = () => {
      e.preventDefault();
      e.stopPropagation();
    };
    if (insert && e.altKey && e.ctrlKey && !e.metaKey && !e.shiftKey) {
      handled();
      const rows = selectedRows(selection);
      if (rows.length > 0) rowActions.onDuplicate(rows);
    } else if (insert && e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey) {
      handled();
      rowActions.onAdd();
    } else if (del && e.shiftKey && !e.altKey && !e.ctrlKey && !e.metaKey) {
      handled();
      onDelete(selection);
    } else if (del && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
      handled();
      const rows = selectedRows(selection);
      if (rows.length > 0) rowActions.onDelete(rows);
    }
  };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => rowKeysRef.current(e);
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  /// The cell (screen row) shows the link arrow: real row, not to delete,
  /// every column of the key read, not NULL, not truncated, not edited.
  const linkAt = useCallback(
    (row: number, col: number) => {
      const key = linkCols?.get(col);
      if (!key || isExtra(row)) return false;
      const base = layout.baseAt(row);
      if (deletedAt?.(base)) return false;
      for (const k of key) {
        const v = cache.cell(base, k);
        if (v === undefined || v.isNull || v.truncated || pendingAt?.(base, k) !== undefined) return false;
      }
      return true;
    },
    [linkCols, isExtra, layout, deletedAt, cache, pendingAt]
  );

  /// ⌘/Ctrl+click on a cell of a selected column: out of the selection, or
  /// back in. The current cell does not stay on a cell taken out.
  const toggleColumnCell = useCallback(
    ([col, row]: Item) => {
      multiPress.current = false;
      const out = new Set(cuts.get(col));
      if (!out.delete(row)) out.add(row);
      const next = new Map(cuts);
      if (out.size > 0) next.set(col, out);
      else next.delete(col);
      setCuts(next);
      const cur = selection.current?.cell;
      if (!cur || cur[0] !== col || cur[1] !== row || !out.has(row)) return;
      let to = row + 1;
      while (out.has(to)) to++;
      if (to >= displayRows) {
        to = row - 1;
        while (out.has(to)) to--;
      }
      if (to < 0) return;
      setSelection({
        ...selection,
        current: { cell: [col, to], range: { x: col, y: to, width: 1, height: 1 }, rangeStack: [] },
      });
      onInspect?.(toModelRow(to), col);
    },
    [cuts, selection, displayRows, onInspect, toModelRow]
  );

  /// Foreign keys: the text is laid out in the cell minus the link zone (a
  /// right-aligned id stays whole), then the zone is repainted with the
  /// cell's fill (a long text never runs under the arrow) and gets "↗".
  /// Rows to delete: after the text, a horizontal line at mid cell.
  /// A cell taken out of a selected column loses the selection's fill.
  const drawCell = useMemo(() => {
    if (!deletedAt && !linkCols && cuts.size === 0) return undefined;
    return (
      args: { ctx: CanvasRenderingContext2D; rect: Rectangle; row: number; col: number; theme: Theme },
      drawContent: () => void
    ) => {
      const { ctx, rect, theme } = args;
      const cut = cuts.get(args.col)?.has(args.row) === true;
      if (cut) {
        ctx.fillStyle = theme.bgCell ?? tok.stateNormal;
        ctx.fillRect(rect.x + 1, rect.y + 1, rect.width - 1, rect.height - 1);
      }
      // `rect` is the same object Glide's renderer draws with (6.0.3):
      // narrowing it here narrows the text.
      const link = linkAt(args.row, args.col) && rect.width > LINK_ZONE * 2;
      if (link) rect.width -= LINK_ZONE;
      drawContent();
      if (link) {
        rect.width += LINK_ZONE;
        const x = rect.x + rect.width - LINK_ZONE;
        // Glide passes the fill of the cell (selection included), absent from its types.
        const fill = (args as { cellFillColor?: string }).cellFillColor;
        ctx.save();
        ctx.fillStyle = theme.bgCell ?? tok.stateNormal;
        ctx.fillRect(x, rect.y + 1, LINK_ZONE - 1, rect.height - 1);
        if (fill && !cut) {
          ctx.fillStyle = fill;
          ctx.fillRect(x, rect.y + 1, LINK_ZONE - 1, rect.height - 1);
        }
        ctx.font = `${theme.baseFontStyle} ${theme.fontFamily}`;
        ctx.textAlign = "center";
        ctx.textBaseline = "middle";
        ctx.fillStyle = tok.cellSelectionBorder;
        ctx.fillText("↗", x + LINK_ZONE / 2, rect.y + rect.height / 2 + 1);
        ctx.restore();
        return;
      }
      if (!deletedAt || isExtra(args.row) || !deletedAt(layout.baseAt(args.row))) return;
      const y = Math.round(rect.y + rect.height / 2) + 0.5;
      ctx.save();
      ctx.strokeStyle = tok.cellMutedFg;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(rect.x + 4, y);
      ctx.lineTo(rect.x + rect.width - 4, y);
      ctx.stroke();
      ctx.restore();
    };
  }, [deletedAt, linkCols, linkAt, isExtra, layout, tok, cuts]);

  // Click on the link arrow: opens the linked record instead of starting
  // the editing (the selection already moved there on mouse down).
  const onCellClicked = useCallback(
    ([col, row]: Item, e: CellClickedEventArgs) => {
      if (!onFollowLink || e.localEventX < e.bounds.width - LINK_ZONE || !linkAt(row, col)) {
        // ⌘+click on the current cell: Glide does not report a change.
        if (multiPress.current && selection.columns.hasIndex(col)) toggleColumnCell([col, row]);
        return;
      }
      e.preventDefault();
      onFollowLink(layout.baseAt(row), col, e.metaKey || e.ctrlKey);
    },
    [onFollowLink, linkAt, layout, selection.columns, toggleColumnCell]
  );

  // Context menu on the grid (decision 11): formatted copy of the selection
  // and export to file. Closing with the TabBar/TableTab convention.
  const [cellMenu, setCellMenu] = useState<{ x: number; y: number } | null>(null);
  const [exportOpen, setExportOpen] = useState(false);
  useEffect(() => {
    if (!cellMenu) return;
    const close = () => setCellMenu(null);
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
    };
  }, [cellMenu]);

  const onCellContextMenu = useCallback(
    (cell: Item, e: { preventDefault: () => void; bounds: Rectangle; localEventX?: number; localEventY?: number }) => {
      e.preventDefault();
      const [col, row] = cell;
      const cur = selection.current;
      // Inside the selection also whole rows or columns (row number, "select
      // all" box): the right click must not lose them.
      const inSel =
        (cur &&
          col >= cur.range.x &&
          col < cur.range.x + cur.range.width &&
          row >= cur.range.y &&
          row < cur.range.y + cur.range.height) ||
        selection.rows.hasIndex(row) ||
        (selection.columns.hasIndex(col) && !cuts.get(col)?.has(row));
      // Right click outside the selection: the selection becomes the clicked
      // cell — the copy always works on what is seen selected.
      if (!inSel) {
        setSelection({
          columns: CompactSelection.empty(),
          rows: CompactSelection.empty(),
          current: {
            cell: [col, row],
            range: { x: col, y: row, width: 1, height: 1 },
            rangeStack: [],
          },
        });
      }
      setCellMenu({
        x: e.bounds.x + (e.localEventX ?? 0),
        y: e.bounds.y + (e.localEventY ?? 0),
      });
    },
    [selection, cuts]
  );

  /// Rectangle for "Copy selection as": cells, or whole rows or columns if
  /// contiguous (the backend copies a single rectangle). null = sparse
  /// rows/columns (or columns with cells taken out), not copyable this way.
  const copyRect = useCallback((): Rectangle | null => {
    const cur = selection.current;
    if (cur && selection.columns.length === 0) return cur.range;
    const span = (sel: CompactSelection): [number, number] | null => {
      const first = sel.first();
      const last = sel.last();
      if (first === undefined || last === undefined || last - first + 1 !== sel.length) return null;
      return [first, sel.length];
    };
    if (selection.rows.length > 0) {
      const sp = span(selection.rows);
      return sp && { x: 0, y: sp[0], width: columns.length, height: sp[1] };
    }
    if (selection.columns.length > 0) {
      const sp = cuts.size === 0 ? span(selection.columns) : null;
      return sp && { x: sp[0], y: 0, width: sp[1], height: displayRows };
    }
    return { x: 0, y: 0, width: columns.length, height: displayRows };
  }, [selection, columns.length, displayRows, cuts]);

  const copyAs = useCallback(
    (format: ExportFormat) => {
      setCellMenu(null);
      const r = copyRect();
      if (!r) return;
      // The new rows are not on the server: only the real ones are copied,
      // the view rows between the rectangle's first and last.
      const from = layout.baseAt(r.y);
      const to = layout.baseAt(Math.min(r.y + r.height, displayRows));
      if (from >= to) return;
      void ipc
        .copyResultCells(
          connId,
          resultId,
          {
            row_start: from,
            row_end: to,
            col_start: r.x,
            col_end: r.x + r.width,
          },
          { format, headers: false, table_name: exportName ?? "export" }
        )
        .catch((e) => console.error(e));
    },
    [connId, resultId, copyRect, layout, displayRows, exportName]
  );

  // Text of the cells of a rectangle, SYNCHRONOUS from the cache: the
  // clipboard is written inside the copy event (setData), navigator.clipboard
  // in the WKWebView dies silently. The cells of the selection are already
  // in cache (prefetch on selection change, below); a missing cell comes out
  // empty and is requested — the next Cmd+C finds it. NULL copies empty.
  // Beyond 4 KB the window is truncated (§4.7): the full value is copied
  // from the inspection panel. Formats and headers with the export
  // (decision 11, M4).
  const cellStrings = useCallback(
    (sel: Rectangle): string[][] => {
      const out: string[][] = [];
      for (let r = sel.y; r < sel.y + sel.height; r++) {
        const line: string[] = [];
        for (let c = sel.x; c < sel.x + sel.width; c++) {
          const v = cellAt(r, c);
          if (v === undefined) requestRow(r);
          line.push(v === undefined || v.isNull ? "" : v.v);
        }
        out.push(line);
      }
      return out;
    },
    [cellAt, requestRow]
  );
  const getCellsForSelection = useCallback(
    (sel: Rectangle): CellArray =>
      cellStrings(sel).map((line) =>
        line.map(
          (s): GridCell => ({ kind: GridCellKind.Text, data: s, displayData: s, allowOverlay: false })
        )
      ),
    [cellStrings]
  );

  // Copy (Cmd+C) and paste (Cmd+V) are handled by us, with plain text
  // ONLY: Glide's (turned off in GRID_KEYBINDINGS) also put in the
  // clipboard an HTML <table> with the database values not neutralized,
  // and on paste they read the HTML back with innerHTML. A cell
  // `x</td><td …>` pasted in Outlook or Excel would inject markup; pasted
  // here it would write into the neighbouring columns. Only with the focus
  // in the grid: the cell editor (Glide's portal) and the rest of the app
  // keep the native behaviour. The pasted values go pending and the grid
  // shows them through `pendingAt`.
  useEffect(() => {
    const focused = () => wrapRef.current?.contains(document.activeElement) === true;
    const onCopy = (e: ClipboardEvent) => {
      const cur = selection.current;
      if (!focused() || e.clipboardData === null) return;
      // Whole rows selected from the number (no current cell): all the
      // columns of each one, one TSV line per row.
      if (!cur && selection.rows.length > 0) {
        const lines: string[][] = [];
        for (const row of selection.rows) {
          lines.push(...cellStrings({ x: 0, y: row, width: columns.length, height: 1 }));
        }
        lastSingleCopy = null;
        e.clipboardData.setData("text/plain", toTsv(lines));
        e.preventDefault();
        return;
      }
      // Whole columns selected from the header: every downloaded row, the
      // selected columns side by side (also non-adjacent ones). A cell taken
      // out copies empty; a row with nothing left is skipped.
      if (selection.columns.length > 0) {
        const cols = [...selection.columns];
        const lines: string[][] = [];
        for (let r = 0; r < displayRows; r++) {
          const kept = cols.map((c) => !cuts.get(c)?.has(r));
          if (!kept.includes(true)) continue;
          lines.push(cols.map((c, i) => (kept[i] ? cellStrings({ x: c, y: r, width: 1, height: 1 })[0][0] : "")));
        }
        lastSingleCopy = null;
        e.clipboardData.setData("text/plain", toTsv(lines));
        e.preventDefault();
        return;
      }
      if (!cur) return;
      const cells = cellStrings(cur.range);
      // A single cell: the value as it is, without the TSV quotes (a
      // multi-line JSON must be pasted elsewhere identical). TSV is only for
      // rectangles, where tabs and newlines separate the cells.
      const single = cells.length === 1 && cells[0].length === 1;
      const text = single ? cells[0][0] : toTsv(cells);
      lastSingleCopy = single ? text : null;
      e.clipboardData.setData("text/plain", text);
      e.preventDefault();
    };
    const canPaste = () =>
      editable && onPasteCells !== undefined && focused() && selection.current !== undefined && selection.columns.length === 0;
    // WebKit (macOS) enables Paste on a non-editable element only if the page
    // takes `beforepaste`: without it ⌘V on a cell did nothing at all.
    const onBeforePaste = (e: Event) => {
      if (canPaste()) e.preventDefault();
    };
    const onPaste = (e: ClipboardEvent) => {
      const cur = selection.current;
      if (!canPaste() || !cur || e.clipboardData === null) return;
      const text = e.clipboardData.getData("text/plain");
      // The value of a cell copied from here comes back as a single cell,
      // even with newlines or tabs inside (read as TSV it would become a rectangle).
      const values =
        text === lastSingleCopy ? [[text]] : pasteValues(text, cur.range.width * cur.range.height);
      e.preventDefault();
      if (values.length > 0) pasteAt(cur.range.y, cur.range.x, values);
    };
    window.addEventListener("copy", onCopy);
    window.addEventListener("beforepaste", onBeforePaste);
    window.addEventListener("paste", onPaste);
    return () => {
      window.removeEventListener("copy", onCopy);
      window.removeEventListener("beforepaste", onBeforePaste);
      window.removeEventListener("paste", onPaste);
    };
  }, [selection, cellStrings, editable, onPasteCells, pasteAt, columns.length, displayRows, cuts]);

  return (
    <>
      {sort && incomplete && (
        <div className="sort-badge" role="status">
          {t("only the {n} fetched rows are sorted", { n: baseRows.toLocaleString(locale()) })}
        </div>
      )}
      <div
        ref={wrapRef}
        style={{ display: "contents" }}
        onMouseDownCapture={(e) => (multiPress.current = e.button === 0 && isMultiKey(e))}
      >
      <DataEditor
        ref={gridRef}
        columns={gridColumns}
        rows={displayRows}
        getCellContent={getCellContent}
        drawCell={drawCell}
        drawHeader={drawHeader}
        getCellsForSelection={getCellsForSelection}
        keybindings={GRID_KEYBINDINGS}
        onCellEdited={editable ? onCellEdited : undefined}
        onCellActivated={onCellActivated}
        onCellClicked={onCellClicked}
        onKeyDown={onGridKeyDown}
        onDelete={onDelete}
        onCellContextMenu={onCellContextMenu}
        onColumnResize={onColumnResize}
        maxColumnWidth={2000}
        onHeaderClicked={onHeaderClicked}
        onHeaderContextMenu={
          onHeaderContext &&
          ((col, e) => {
            e.preventDefault();
            onHeaderContext(col, e.bounds.x, e.bounds.y + e.bounds.height);
          })
        }
        rowHeight={ROW_HEIGHT}
        headerHeight={HEADER_HEIGHT}
        smoothScrollX
        smoothScrollY
        // "clickable-number": same row number, but the click selects the
        // whole row (with "number" Glide ignores the click). Cmd+click adds,
        // Shift+click extends, like in DBeaver.
        rowMarkers="clickable-number"
        width={width}
        height={height}
        theme={glideTheme}
        gridSelection={selection}
        onGridSelectionChange={(s) => {
          // ⌘/Ctrl+click on a cell of a selected column: the selection stays
          // the column, without that cell (or with it again).
          if (multiPress.current && s.current && selection.columns.hasIndex(s.current.cell[0])) {
            toggleColumnCell(s.current.cell);
            return;
          }
          // Post-commit auto-advance: the selection is brought back to the
          // cell just edited (see onCellEdited).
          const keep = keepSelection.current;
          if (
            keep &&
            s.current &&
            (s.current.cell[0] !== keep.cell[0] || s.current.cell[1] !== keep.cell[1])
          ) {
            keepSelection.current = null;
            setSelection({
              columns: keep.columns,
              rows: CompactSelection.empty(),
              current: {
                cell: keep.cell,
                range: { x: keep.cell[0], y: keep.cell[1], width: 1, height: 1 },
                rangeStack: [],
              },
            });
            return;
          }
          if (multiPress.current) {
            setCuts(new Map([...cuts].filter(([col]) => s.columns.hasIndex(col))));
          } else if (cuts.size > 0) {
            setCuts(NO_CUTS);
          }
          // Column selected from the header, like DBeaver: a current cell
          // in it, on the row the cursor was on (if on screen) — typing
          // opens the editor there and the value goes to the whole column
          // (onCellEdited). The clicked column, else the old one if still in.
          if (!s.current && s.columns.length > 0 && s.rows.length === 0 && editable && onEditCells && displayRows > 0) {
            const prev = selection.current?.cell;
            const added = [...s.columns].find((c) => !selection.columns.hasIndex(c));
            const col = added ?? (prev && s.columns.hasIndex(prev[0]) ? prev[0] : s.columns.first()!);
            const vis = regionRef.current;
            const onScreen = prev && prev[1] >= vis.y && prev[1] < Math.min(vis.y + vis.height, displayRows);
            const row = onScreen ? prev[1] : Math.min(vis.y, displayRows - 1);
            s = { ...s, current: { cell: [col, row], range: { x: col, y: row, width: 1, height: 1 }, rangeStack: [] } };
          }
          setSelection(s);
          if (s.current) {
            // The new rows too: the cell panel shows (and edits) their
            // pending value.
            onInspect?.(toModelRow(s.current.cell[1]), s.current.cell[0]);
            // Prefetch of the selection: so the synchronous Cmd+C finds everything.
            const r = s.current.range;
            for (let row = r.y; row < r.y + r.height; row += BLOCK) requestRow(row);
            requestRow(r.y + r.height - 1);
          }
          // Whole rows: same prefetch, for Cmd+C.
          for (const row of s.rows) requestRow(row);
        }}
        onVisibleRegionChanged={(r) => {
          regionRef.current = { y: r.y, height: r.height };
          const from = Math.max(0, r.y - BLOCK);
          const to = Math.min(displayRows, r.y + r.height + BLOCK);
          for (let row = from; row < to; row += BLOCK) requestRow(row);
          if (onNearEnd && !sort && r.y + r.height >= displayRows - NEAR_END) onNearEnd();
        }}
      />
      </div>
      {cellMenu && (
        <div
          className="tab-menu"
          style={{ position: "fixed", left: cellMenu.x, top: cellMenu.y }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          {(() => {
            // Changes from the menu (they go pending like the others): single
            // cell or rectangle, only in an editable grid.
            const cur = selection.current;
            if (!editable || !onEditCell || !cur || cur.rangeStack.length > 0) return null;
            if (selection.rows.length > 0 || selection.columns.length > 0) return null;
            const r = cur.range;
            const single = r.width === 1 && r.height === 1;
            if (!single && (!onEditCells || !onPasteCells)) return null;
            let nullDisabled = false;
            let nullTitle = t("Sets the selected cells to NULL; NOT NULL columns are left as they are (⇧Del)");
            if (single) {
              const [col, row] = cur.cell;
              const v = cellAt(row, col);
              const p = pendingValue(row, col);
              const colName = columns[col]?.name ?? "";
              const isNull = p !== undefined ? p === null : v?.isNull;
              const notNullable = canSetNull !== undefined && !canSetNull(colName);
              nullDisabled = v === undefined || !!isNull || notNullable;
              nullTitle =
                v === undefined
                  ? t("Cell not loaded yet")
                  : isNull
                    ? t("The cell is already NULL")
                    : notNullable
                      ? t("Column {col} is NOT NULL", { col: colName })
                      : t("Sets the cell to NULL (⇧Del)");
            }
            return (
              <>
                <div className="tab-menu-label">{single ? t("Cell") : t("Selected cells")}</div>
                <button
                  className="tab-menu-item"
                  disabled={nullDisabled}
                  title={nullTitle}
                  onClick={() => {
                    setCellMenu(null);
                    if (single) proposeNull();
                    else editRect(r, null);
                  }}
                >
                  {t("Set NULL")}
                </button>
                {(() => {
                  // Default known only on a single column: on a rectangle
                  // each column has its own, the server decides for each.
                  const def =
                    r.width === 1 ? columnDefault?.(columns[r.x]?.name ?? "") : undefined;
                  return (
                    <button
                      className="tab-menu-item"
                      title={
                        def === undefined
                          ? t("Sets each column's default value (chosen by the database on save)")
                          : def === null
                            ? t("The column has no default: it becomes NULL, or an error if NOT NULL")
                            : t("Column default: {def}", { def })
                      }
                      onClick={() => {
                        setCellMenu(null);
                        if (single) onEditCell(toModelRow(cur.cell[1]), cur.cell[0], DEFAULT_CELL);
                        else editRect(r, DEFAULT_CELL);
                      }}
                    >
                      {t("Set DEFAULT")}
                    </button>
                  );
                })()}
                <button
                  className="tab-menu-item"
                  title={single ? undefined : t("A different UUID for each cell")}
                  onClick={() => {
                    setCellMenu(null);
                    if (single) onEditCell(toModelRow(cur.cell[1]), cur.cell[0], crypto.randomUUID());
                    else
                      pasteAt(
                        r.y,
                        r.x,
                        Array.from({ length: r.height }, () =>
                          Array.from({ length: r.width }, () => crypto.randomUUID())
                        )
                      );
                  }}
                >
                  {t("Generate UUID v4")}
                </button>
              </>
            );
          })()}
          {rowActions && editable && (() => {
            const rows = selectedRows(selection);
            const allDeleted =
              rows.length > 0 &&
              rows.every((r) => r < baseRows && rowActions.deletedAt?.(r) === true);
            const close = (f: () => void) => () => {
              setCellMenu(null);
              f();
            };
            return (
              <>
                <div className="tab-menu-label">{rows.length > 1 ? t("Rows") : t("Row")}</div>
                <button className="tab-menu-item" onClick={close(rowActions.onAdd)}>
                  {t("Add row")} <span className="tab-menu-key">⌥Insert</span>
                </button>
                <button
                  className="tab-menu-item"
                  disabled={rows.length === 0}
                  onClick={close(() => rowActions.onDuplicate(rows))}
                >
                  {rows.length > 1 ? t("Duplicate rows") : t("Duplicate row")}{" "}
                  <span className="tab-menu-key">⌃⌥Insert</span>
                </button>
                <button
                  className="tab-menu-item"
                  disabled={rows.length === 0}
                  onClick={close(() => rowActions.onDelete(rows))}
                >
                  {allDeleted
                    ? t("Restore")
                    : rows.length > 1
                      ? t("Delete rows")
                      : t("Delete row")}{" "}
                  <span className="tab-menu-key">{t("Del")}</span>
                </button>
              </>
            );
          })()}
          {(() => {
            const cur = selection.current;
            if (!cellActions || !cur || cur.range.width !== 1 || cur.range.height !== 1) return null;
            if (isExtra(cur.cell[1])) return null;
            const actions = cellActions(layout.baseAt(cur.cell[1]), cur.cell[0]);
            if (actions.length === 0) return null;
            return (
              <>
                <div className="tab-menu-label">{t("Go to")}</div>
                {actions.map((a, i) => (
                  <button
                    key={i}
                    className="tab-menu-item"
                    onClick={() => {
                      setCellMenu(null);
                      a.onClick();
                    }}
                  >
                    {a.label}
                  </button>
                ))}
              </>
            );
          })()}
          {(() => {
            // Server-side filter from the cell value: single cell and whole
            // value only (a truncated one would filter on the visible piece).
            const cur = selection.current;
            if (!onFilterCell || !cur || cur.range.width !== 1 || cur.range.height !== 1) return null;
            const [col, row] = cur.cell;
            const v = cellAt(row, col);
            const colName = columns[col]?.name;
            if (v === undefined || !colName || isExtra(row)) return null;
            const item = (exclude: boolean, label: string) => (
              <button
                className="tab-menu-item"
                disabled={v.truncated}
                title={v.truncated ? t("Truncated value: filter from the column header") : undefined}
                onClick={() => {
                  setCellMenu(null);
                  onFilterCell(colName, v.isNull ? null : v.v, exclude);
                }}
              >
                {label}
              </button>
            );
            return (
              <>
                <div className="tab-menu-label">{t("Filter")}</div>
                {item(false, t("Only this value"))}
                {item(true, t("Exclude this value"))}
              </>
            );
          })()}
          <div className="tab-menu-label">{t("Copy selection as")}</div>
          {(() => {
            const sparse = copyRect() === null;
            const item = (format: ExportFormat, label: string) => (
              <button
                className="tab-menu-item"
                disabled={sparse}
                title={sparse ? t("Rows or columns not contiguous: select a single block") : undefined}
                onClick={() => copyAs(format)}
              >
                {label}
              </button>
            );
            return (
              <>
                {item("tsv", "TSV")}
                {item("csv", "CSV")}
                {item("json", "JSON")}
                {item("insert", "INSERT")}
              </>
            );
          })()}
          <div className="tab-menu-label">Result set</div>
          <button
            className="tab-menu-item"
            onClick={() => {
              setCellMenu(null);
              setExportOpen(true);
            }}
          >
            {t("Export to file…")} {incomplete ? t("(fetched rows)") : ""}
          </button>
        </div>
      )}
      {exportOpen && (
        <ExportDialog
          title={t("Export result set")}
          subject={exportName ?? "export"}
          detail={
            incomplete
              ? t("{n} rows fetched (partial result)", { n: baseRows.toLocaleString(locale()) })
              : t("{n} rows", { n: baseRows.toLocaleString(locale()) })
          }
          tableName={exportName ?? "export"}
          script={false}
          onExport={(opts) => {
            const ext = opts.format === "insert" ? "sql" : opts.format;
            return ipc.exportResult(connId, resultId, opts, `${exportName ?? "export"}.${ext}`);
          }}
          onClose={() => setExportOpen(false)}
        />
      )}
    </>
  );
}

const LOADING: GridCell = { kind: GridCellKind.Loading, allowOverlay: false };

/// Last value copied from a single cell (see onCopy/onPaste).
let lastSingleCopy: string | null = null;

// Widths per key (table or SQL tab) → column name → px. In memory only:
// when the app is reopened it starts again from the heuristics.
const columnWidths = new Map<string, Map<string, number>>();

function widthsFor(key: string): Map<string, number> {
  let m = columnWidths.get(key);
  if (!m) {
    m = new Map();
    columnWidths.set(key, m);
  }
  return m;
}

/// Text drawn in a cell: beyond CLAMP characters no column would show it
/// anyway, and drawing it all costs at every frame.
const CLAMP = 500;

/// Text to draw: the grid shows a single line, so newlines become "↵" and
/// the indentation that follows them a space — of a multi-line JSON the
/// start of the content is seen, not just the "{". Tabs become spaces.
/// Rendering only: the value (copy, editing) stays the real one.
/// Value edited in the cell panel rather than in the overlay: what the cell
/// cannot show whole (beyond CLAMP, or on more lines).
export function isLong(s: string): boolean {
  return s.length > CLAMP || /[\r\n]/.test(s);
}

function clamp(s: string): string {
  let out = s.length > CLAMP ? s.slice(0, CLAMP) + "…" : s;
  if (/[\n\r\t]/.test(out)) out = out.replace(/[ \t]*\r?\n[ \t]*/g, " ↵ ").replace(/[\r\t]/g, " ");
  return out;
}

// Fitted width: the column widens up to the longest value of the first
// rows (and the name), but not beyond FIT_MAX — from there the text is cut.
const FIT_MIN = 60;
const FIT_MAX = 420;

const fitWidthsByKey = new Map<string, Map<string, number>>();

function fitWidthsFor(key: string): Map<string, number> {
  let m = fitWidthsByKey.get(key);
  if (!m) {
    m = new Map();
    fitWidthsByKey.set(key, m);
  }
  return m;
}

/// Width of one character of the grid font (monospace: the same for all).
/// Measured once, in bold like the header.
let charWidth: number | null = null;

function monoCharWidth(): number {
  if (charWidth === null) {
    const ctx = document.createElement("canvas").getContext("2d");
    if (ctx) {
      ctx.font = "500 13px ui-monospace, SFMono-Regular, monospace";
      charWidth = ctx.measureText("0".repeat(20)).width / 20;
    } else {
      charWidth = 8;
    }
  }
  return charWidth;
}

/// Measures the columns without a fitted width from block `w`. Returns
/// whether it added any (a re-render is needed).
function fitColumns(columns: ColumnOut[], w: WindowOut, into: Map<string, number>): boolean {
  if (w.row_end <= w.row_start) return false;
  const cw = monoCharWidth();
  let added = false;
  for (const col of w.columns) {
    const meta = columns[col.index];
    if (!meta || into.has(meta.name)) continue;
    // +2: room for the sort arrow in the header. The text is measured as it
    // is drawn (newlines rendered with "↵", see `clamp`).
    let chars = meta.name.length + 2;
    for (let i = 0; i < col.values.length; i++) {
      const n = col.nulls[i] ? 4 : clamp(col.values[i]).length + (col.truncated[i] ? 1 : 0);
      if (n > chars) chars = n;
    }
    into.set(meta.name, Math.round(Math.min(FIT_MAX, Math.max(FIT_MIN, chars * cw + 24))));
    added = true;
  }
  return added;
}

/// Mark in the header. Local sort: ↑/↓ in the SQL tabs; in the table tab
/// ⇡/⇣, so as not to confuse it with the server's. Server: ↑/↓ with the
/// priority when there is more than one column (`date ↓1`, `id ↑2`).
function headerMark(name: string, i: number, local: SortSpec | null, server: OrderBy[] | undefined): string {
  if (local?.col === i) {
    if (server === undefined) return local.desc ? " ↓" : " ↑";
    return local.desc ? " ⇣" : " ⇡";
  }
  const k = server?.findIndex((o) => o.column === name) ?? -1;
  if (k < 0 || !server) return "";
  return (server[k].desc ? " ↓" : " ↑") + (server.length > 1 ? String(k + 1) : "");
}

function widthHint(c: ColumnOut): number {
  switch (c.semantic_class) {
    case "temporal":
      return 210; // timestamp with microseconds: 26 chars (§9.6.2)
    case "numeric":
      return 110;
    case "boolean":
      return 90;
    case "structured":
      return 280;
    default:
      return 160;
  }
}
