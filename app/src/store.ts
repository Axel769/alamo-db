// Zustand store (§9.2): UI and execution state live here, NEVER the rows.
// The window cache stays outside React (rowCache.ts): putting it in a store
// would mean a cascade of re-renders on every scroll.
//
// M2: execution state is PER TAB (§9.5, single bar).
// M3.5 (DBeaver model): several connections open together, every tab bound
// to its own via `connId`. The "one execution at a time" constraint is PER
// connection (single sticky per session, §3): it is derived from the tabs'
// `running` flags, no global counter.
import { create } from "zustand";
import {
  CellAssign,
  PendingValue,
  emptyTableQuery,
  ErrorOut,
  ExecEvent,
  ColumnOut,
  ConnSecurity,
  ipc,
  OrderBy,
  OutcomeOut,
  Profile,
  TableMoreOut,
  TableQuery,
  TxnState,
} from "./ipc";
import { canSwitchDb } from "./engines";
import { setting } from "./settings";

export type StatementUi = {
  index: number;
  /// Offset (code point) in the executed SQL; null until the statement has
  /// started (Skipped ones never start).
  charOffset: number | null;
  running: boolean;
  outcome: OutcomeOut | null;
  elapsedMs: number;
  notices: ErrorOut[];
  resultId: number | null;
  columns: ColumnOut[] | null;
  rows: number;
  layoutBytes: number;
  budgetPaused: boolean;
  /// The pause is the precautionary one at N rows, not the memory budget.
  budgetByRows: boolean;
  /// State of the result set while it arrives (§5.2: unknown total → "N+").
  streaming: boolean;
};

function emptyStatement(index: number): StatementUi {
  return {
    index,
    charOffset: null,
    running: false,
    outcome: null,
    elapsedMs: 0,
    notices: [],
    resultId: null,
    columns: null,
    rows: 0,
    layoutBytes: 0,
    budgetPaused: false,
    budgetByRows: false,
    streaming: false,
  };
}

/// Table a table tab points to (§9.5).
export type TableRef = { schema: string; name: string };
export type TableSub = "data" | "structure" | "ddl";
export type TabKind = "sql" | "table";
/// Which connection a tab's execution runs on (§3.1): the editor's sticky
/// or the shared one of the table tabs.
export type Lane = "sticky" | "shared";

/// Open transaction on the sticky (§9.7). `openForMs` is the backend's at
/// fetch time: the visible timer adds `Date.now() - fetchedAt`.
export type TxnUi = {
  hasWrites: boolean;
  writes: number;
  openForMs: number;
  /// Since the last statement: automatic ROLLBACK at `idleRollbackMs` (§9.7.2).
  idleForMs: number;
  idleRollbackMs: number;
  fetchedAt: number;
};

/// Open connection (M3.5): the profile lives here, not in a Studio state —
/// the toolbar shows the one of the active tab's connection.
export type ConnUi = {
  connId: number;
  profile: Profile;
  /// Current database of the sticky (on MySQL it changes with USE).
  currentDb: string | null;
  /// Open transaction on the sticky, `null` = none (§9.7).
  txn: TxnUi | null;
  /// Actual encryption and verification of the channel, measured at connect.
  security: ConnSecurity;
  /// PG (§3.3): one database per connection — the tree lists the server's
  /// databases and each one is opened on a connection of its own.
  oneDbPerConn: boolean;
  /// Connection to another database of the profile (§3.3): the profile's
  /// connection it belongs to, which shows it in the tree and closes it.
  /// `null` = the profile's connection.
  parent: number | null;
  /// Manual commit (§9.7): implicit BEGIN, explicit Commit/Rollback. Always
  /// on prod; on the other profiles the toolbar switch turns it on.
  manualCommit: boolean;
  /// Bumped when writes of the transaction are undone (Rollback, idle
  /// rollback, connection lost): the table tabs re-read what they show.
  undone: number;
};

/// The profiles' connections (tree roots, panel, dots): not those to the
/// other databases of a PG server, which live under their profile's.
export function profileConns(conns: ConnUi[]): ConnUi[] {
  return conns.filter((c) => c.parent === null);
}

/// Database of the profile's connection on PG: without one in the profile,
/// PG connects to the user's namesake.
export function profileDatabase(conn: ConnUi): string {
  return conn.profile.database.trim() || conn.profile.username;
}

/// A profile's connection and those to the other databases of its server
/// (§3.3): what closing the profile closes.
export function connFamily(conns: ConnUi[], rootId: number): Set<number> {
  return new Set(conns.filter((c) => c.connId === rootId || c.parent === rootId).map((c) => c.connId));
}

/// Uncommitted writes on a profile's connection or on those to its server's
/// other databases: what disconnecting the profile would roll back.
export function familyWrites(conns: ConnUi[], rootId: number): boolean {
  const family = connFamily(conns, rootId);
  return conns.some((c) => family.has(c.connId) && !!c.txn?.hasWrites);
}

/// The profile hides some databases (`db_include`/`db_exclude`).
export function hasDbFilter(conn: ConnUi): boolean {
  return conn.profile.db_include.trim() !== "" || conn.profile.db_exclude.trim() !== "";
}

/// The profile's connection a connection belongs to (itself for a profile's).
export function rootConnId(conns: ConnUi[], connId: number | null): number | null {
  return conns.find((c) => c.connId === connId)?.parent ?? connId;
}

/// Next pages of the table tab (the grid asks for them on reaching the
/// bottom). Valid only for THAT result set: a reload creates a new one and
/// this state lapses by itself, without scattered resets.
export type TableMoreUi = {
  resultId: number;
  loading: boolean;
  /// The last page was short: the table is over.
  done: boolean;
  /// Page failed: no more automatic attempts until one retries.
  error: ErrorOut | null;
};

/// One stop of the navigation inside a table tab (foreign keys): the table
/// and the query it was being looked at with.
export type TableNav = { table: TableRef; query: TableQuery };

export type TabUi = {
  id: string;
  title: string;
  kind: TabKind;
  /// Connection of the tab (M3.5). `null` = disconnected: the profile is
  /// closed, the tab stays visible but without data until one reconnects.
  connId: number | null;
  /// Origin profile, to reattach the tab on reconnection and for workspace
  /// persistence. `null` only for pre-M3.5 tabs (migration).
  profileId: number | null;
  /// Present only for kind "table".
  table: TableRef | null;
  /// Database of the SQL tab (MySQL): every tab has its own, it is not the
  /// connection's. The `USE` on the sticky starts BEFORE executing, if the
  /// connection's database is another. `null` = whatever is there.
  db: string | null;
  /// Active sub-tab of the table tab (§9.5: Data / Structure / DDL).
  sub: TableSub;
  /// Server-side sort and filters of the table tab (decision 10, second
  /// half): explicit re-query, separate from the local sort on permutation.
  tableQuery: TableQuery;
  /// History of the foreign-key navigation inside the tab (like the
  /// browser's back/forward). In memory only: the workspace saves the
  /// current stop.
  navBack: TableNav[];
  navFwd: TableNav[];
  more: TableMoreUi | null;
  /// Table tab just opened (or reached through a foreign key): before
  /// loading, the default sort saved for the table is read.
  /// `false` for tabs restored from the workspace: their query wins.
  applyDefaultSort: boolean;
  /// Preview tab (§9.5): the next click from the tree replaces it; double
  /// click or an edit make it permanent.
  preview: boolean;
  /// Pin (§9.5): protects from "close others" and from accidental closing
  /// (the × disappears; it only closes from the context menu).
  pinned: boolean;
  execId: number | null;
  execState: string;
  running: boolean;
  /// When the current execution started (Date.now()): the "running for N s"
  /// counter survives the tab switch.
  startedAt: number | null;
  statements: StatementUi[];
  /// Index of the statement selected in the results pane.
  selected: number;
  /// SQL of the current or last execution: the labels of the statement
  /// strip come from here (the editor may have changed in the meantime).
  execSql: string;
  /// Offset (code point) of `execSql` in the editor text: errors arrive
  /// relative to the SQL sent and this is added to highlight them.
  execBase: number;
  /// Cells modified in the grid and not yet saved (table tab): they are all
  /// written together with Save / Cmd+S.
  pending: PendingEdits;
  /// New rows ("+ Row" / "Duplicate", like DBeaver) not yet saved: each
  /// right below the row it was added under (`rowLayout`), they become
  /// INSERTs with Save, after the UPDATEs. In screen order (`after`
  /// non-decreasing).
  newRows: NewRow[];
  /// Rows marked for deletion (Del / "Delete", like DBeaver), not yet
  /// saved: by PK id (`pendingRowId`) → key. They become DELETEs with Save,
  /// before everything else.
  deleted: Record<string, CellAssign[]>;
};

/// Pending new row: one value per column (DEFAULT = the server decides).
/// `id` only serves React and the selection, it does not go to the database.
/// `after`: row of the view it sits below (-1 = at the top).
export type NewRow = { id: string; after: number; values: Record<string, PendingValue> };

/// Pending edits of a row: its PK key and the changed columns (`null` =
/// NULL). One row per UPDATE on save.
export type PendingRow = { key: CellAssign[]; set: Record<string, PendingValue> };
/// Per row, indexed by the serialised PK values (`pendingRowId`) and not by
/// the row index: sort and next pages move the rows.
export type PendingEdits = Record<string, PendingRow>;

export function pendingRowId(keyValues: (string | null)[]): string {
  return JSON.stringify(keyValues);
}

export function pendingCount(p: PendingEdits): number {
  let n = 0;
  for (const r of Object.values(p)) n += Object.keys(r.set).length;
  return n;
}

/// Everything Save would write: modified cells + new rows. It is the number
/// that decides "are there unsaved changes?" (bar, dialog, closing gate).
export function unsavedCount(tab: Pick<TabUi, "pending" | "newRows" | "deleted">): number {
  // Modified cells of a row to delete are not written: they do not count.
  let cells = 0;
  for (const [id, r] of Object.entries(tab.pending))
    if (!(id in tab.deleted)) cells += Object.keys(r.set).length;
  return cells + tab.newRows.length + Object.keys(tab.deleted).length;
}

export function emptyTab(
  id: string,
  title: string,
  preview = false,
  kind: TabKind = "sql",
  table: TableRef | null = null,
  connId: number | null = null,
  profileId: number | null = null,
  db: string | null = null
): TabUi {
  return {
    id,
    title,
    kind,
    connId,
    profileId,
    table,
    db,
    sub: "data",
    tableQuery: emptyTableQuery(),
    navBack: [],
    navFwd: [],
    more: null,
    applyDefaultSort: false,
    preview,
    pinned: false,
    execId: null,
    execState: "idle",
    running: false,
    startedAt: null,
    statements: [],
    selected: 0,
    execSql: "",
    execBase: 0,
    pending: {},
    newRows: [],
    deleted: {},
  };
}

type Store = {
  /// Open connections (M3.5), in opening order.
  connections: ConnUi[];
  /// "Current" connection for new tabs: the last connected or that of the
  /// most recently activated tab.
  activeConnId: number | null;
  connectError: ErrorOut | null;
  tabs: TabUi[];
  /// Order of the groups in the tab bar (profile ids): stable, a group does
  /// not change place when it opens the first tab or closes the last.
  groupOrder: number[];
  activeTabId: string;
  /// Connection page in the foreground (connId), in place of the active
  /// tab: it is not a tab — it has no editor, results nor persistence — but
  /// in the bar it lives on the group label. `null` = the tab is shown.
  connPage: number | null;
  /// Bumped when something outside the page (the tab bar's database
  /// marker) sets the database it shows: the mounted page re-reads it.
  connPageNav: number;
  /// MRU order (ids, most recent first) for Ctrl+Tab (§9.5).
  mru: string[];
  nextTabId: number;

  /// Registers the connection and reattaches the orphan tabs of its profile
  /// (and the pre-M3.5 ones without a profile: migration).
  /// `manualCommit` starts as the profile says (on for prod).
  addConnection: (conn: Omit<ConnUi, "manualCommit" | "undone">) => void;
  setManualCommit: (connId: number, on: boolean) => void;
  noteUndone: (connId: number) => void;
  /// Removes the connection: its tabs remain, disconnected and without
  /// execution state — the results must be released BEFORE by the caller.
  removeConnection: (connId: number) => void;
  setCurrentDb: (connId: number, db: string | null) => void;
  /// Database of ONE SQL tab (MySQL): from the toolbar picker or from the
  /// executed `USE`. Does not touch the connection: the real `USE` starts at
  /// execution.
  setTabDb: (id: string, db: string | null) => void;
  /// Updates the transaction state from the backend (§9.7).
  setTxnState: (connId: number, st: TxnState) => void;
  setConnectError: (err: ErrorOut | null) => void;
  /// Shows the page of an open connection (which becomes the current one).
  openConnPage: (connId: number) => void;
  hydrate: (tabs: TabUi[], activeTabId: string, nextTabId: number) => void;
  addTab: (tab: TabUi) => void;
  activateTab: (id: string) => void;
  closeTab: (id: string) => void;
  makePermanent: (id: string) => void;
  retitleTab: (id: string, title: string) => void;
  setSub: (id: string, sub: TableSub) => void;
  setPending: (id: string, pending: PendingEdits) => void;
  setNewRows: (id: string, newRows: NewRow[]) => void;
  setDeleted: (id: string, deleted: Record<string, CellAssign[]>) => void;
  /// Result set rows grown outside a load (inserted rows appended by the
  /// save): the grid gets longer without reloading.
  setStatementRows: (tabId: string, resultId: number, rows: number) => void;
  setTableQuery: (id: string, query: TableQuery) => void;
  /// Outcome of reading the default (`applyDefaultSort`): `order` sets it,
  /// `null` = none. If in the meantime the user has already sorted, their
  /// sort wins.
  resolveDefaultSort: (id: string, order: OrderBy[] | null) => void;
  /// The table tab moves to another table (or another filter) keeping the
  /// current stop in the history. The results must be released BEFORE by
  /// the caller; the view reloads by itself (empty statements).
  navigateTable: (id: string, table: TableRef, query: TableQuery) => void;
  /// Back (-1) / forward (+1) in the tab's history.
  navStep: (id: string, dir: -1 | 1) => void;
  setPinned: (id: string, pinned: boolean) => void;
  /// Drag and drop in the bar: `id` lands right before/after `target`.
  /// Both must be in the same group, otherwise nothing moves — groups
  /// stay contiguous whatever the drop.
  moveTab: (id: string, target: string, side: "before" | "after") => void;
  /// Next page of the table tab: start and outcome (`out` with the new
  /// rows, or `error`). An outcome for a result set the tab no longer shows
  /// (reloaded in the meantime) is discarded.
  tableMoreStart: (tabId: string, resultId: number) => void;
  tableMoreEnd: (
    tabId: string,
    resultId: number,
    out: TableMoreOut | null,
    error: ErrorOut | null
  ) => void;
  startExecution: (tabId: string, execId: number, lane?: Lane, sql?: string, base?: number) => void;
  handleEvent: (connId: number, ev: ExecEvent) => void;
  select: (tabId: string, i: number) => void;
};

/// Database (or schema) opened in each connection's page: the page unmounts
/// when a tab is activated, but coming back one must find the table list
/// one started from; and a new SQL tab opened from the page is born on that
/// database. In memory only, outside the reactive store.
export const openedDbByConn = new Map<number, string>();

/// PG (§3.3): profile's connections whose page shows the schemas of their
/// own database instead of the server's databases (the user entered it).
/// The page of another database's connection always shows its schemas.
export const pgDbEntered = new Set<number>();

/// The same, remembered per profile across startups: on reconnecting, the
/// page restarts from the database that was being looked at. Local
/// preference, like `alamo.lastDb`.
const OPENED_DB_KEY = "alamo.openedDb";

function openedDbMap(): Record<string, string> {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(OPENED_DB_KEY) ?? "{}");
    return v && typeof v === "object" ? (v as Record<string, string>) : {};
  } catch {
    return {};
  }
}

export function savedOpenedDb(profileId: number): string | null {
  const v = openedDbMap()[String(profileId)];
  return typeof v === "string" && v !== "" ? v : null;
}

export function rememberOpenedDb(profileId: number, name: string | null): void {
  try {
    const m = openedDbMap();
    if (name === null) delete m[String(profileId)];
    else m[String(profileId)] = name;
    localStorage.setItem(OPENED_DB_KEY, JSON.stringify(m));
  } catch {
    // Storage unavailable: only the memory is lost.
  }
}

/// Filter of the connection page, per level (database list or tables of a
/// database): survives the tab switch like `openedDbByConn`. Key:
/// `pageFilterKey`.
export const pageFilterByConn = new Map<string, string>();

export function pageFilterKey(connId: number, opened: string | null): string {
  return `${connId}\u0000${opened ?? ""}`;
}

/// Group of a tab in the bar: the profile of its connection. Pre-M3.5 tabs
/// without a profile stay together until they are reattached.
export function tabGroup(t: TabUi): number {
  return t.profileId ?? -1;
}

/// Database a tab works on (§9.5). MySQL: a table's schema (the schema IS
/// the database), an SQL tab's own database. PG: the database of the tab's
/// connection (§3.3), kept in `tab.db` since tabs of older versions have
/// none. "" = unknown.
export function tabDatabase(tab: TabUi, switchDb: boolean, connDb: string | null | undefined): string {
  if (!switchDb) return tab.db ?? connDb ?? "";
  if (tab.kind === "table" && tab.table) return tab.table.schema;
  return tab.db ?? "";
}

/// Bar order: the tabs of a connection are contiguous, groups in the order
/// of `groupOrder` (§9.5). Stable: inside the group the order stays as it is.
function regroup(tabs: TabUi[], groupOrder: number[]): TabUi[] {
  const order = [...groupOrder];
  const by = new Map<number, TabUi[]>();
  for (const t of tabs) {
    const k = tabGroup(t);
    if (!by.has(k)) {
      by.set(k, []);
      if (!order.includes(k)) order.push(k);
    }
    by.get(k)!.push(t);
  }
  return order.flatMap((k) => by.get(k) ?? []);
}

/// Bar with `tab` added (§9.5): inside its connection's group, in the block
/// of its database — an SQL tab after the block's last SQL tab or at its
/// start, a table after the block's last tab. A database without tabs
/// starts a block at the end of the group. No open tab moves.
export function placeTab(
  s: Pick<Store, "connections" | "groupOrder">,
  tabs: TabUi[],
  tab: TabUi
): { tabs: TabUi[]; groupOrder: number[] } {
  const groupOrder = withGroup(s.groupOrder, tabGroup(tab));
  const conn = s.connections.find((c) => c.connId === tab.connId);
  const switchDb = canSwitchDb(conn?.profile.dialect);
  const db = tabDatabase(tab, switchDb, conn?.currentDb);
  const block = db
    ? tabs.flatMap((t, i) =>
        tabGroup(t) === tabGroup(tab) && tabDatabase(t, switchDb, conn?.currentDb) === db ? [i] : []
      )
    : [];
  const sql = block.filter((i) => tabs[i].kind === "sql");
  const at =
    block.length === 0
      ? tabs.length
      : tab.kind === "sql"
        ? sql.length > 0
          ? sql[sql.length - 1] + 1
          : block[0]
        : block[block.length - 1] + 1;
  return { tabs: regroup([...tabs.slice(0, at), tab, ...tabs.slice(at)], groupOrder), groupOrder };
}

/// The group joins the end of the order the first time it appears (as a
/// connection or as a tab) and from there it no longer moves.
function withGroup(order: number[], gid: number): number[] {
  return order.includes(gid) ? order : [...order, gid];
}

/// The tab looks at another table: title, query and execution state start
/// over; whoever navigates cares about it, so it is no longer a preview.
function resetTableView(t: TabUi, table: TableRef, query: TableQuery): TabUi {
  return {
    ...t,
    table,
    title: table.name,
    tableQuery: query,
    sub: "data",
    preview: false,
    execId: null,
    execState: "idle",
    running: false,
    statements: [],
    selected: 0,
    // Another table: the pending edits belonged to the previous one.
    pending: {},
    newRows: [],
    deleted: {},
  };
}

function touchMru(mru: string[], id: string): string[] {
  return [id, ...mru.filter((x) => x !== id)];
}

// invoke/channel race: events travel on a channel separate from the
// `invoke` response, so for instantaneous queries (e.g. immediate error)
// they can arrive BEFORE startExecution registers the execId on the tab.
// Without a buffer `finished` would be lost too and the tab would stay
// "running" forever — which on the shared lane blocks all the table tabs.
// Composite key connId:execId: exec_ids are per-session counters.
const pendingEvents = new Map<string, ExecEvent[]>();
const PENDING_CAP = 8;

export const useStore = create<Store>((set, get) => ({
  connections: [],
  activeConnId: null,
  connectError: null,
  tabs: [],
  groupOrder: [],
  activeTabId: "",
  connPage: null,
  connPageNav: 0,
  mru: [],
  nextTabId: 1,

  addConnection: (fresh) =>
    set((s) => {
      const conn: ConnUi = { ...fresh, manualCommit: fresh.profile.is_production, undone: 0 };
      // Another database of an open profile (§3.3): its detached tabs (after
      // a restart or a reconnection) come back on it; the current
      // connection stays the same.
      if (conn.parent !== null)
        return {
          connections: [...s.connections, conn],
          tabs: s.tabs.map((t) =>
            t.connId === null && t.profileId === conn.profile.id && t.db === conn.profile.database
              ? { ...t, connId: conn.connId }
              : t
          ),
        };
      const groupOrder =
        conn.profile.id !== null ? withGroup(s.groupOrder, conn.profile.id) : s.groupOrder;
      return {
        connections: [...s.connections, conn],
        activeConnId: conn.connId,
        connectError: null,
        groupOrder,
        // Reattached tabs may change group (pre-M3.5 migration).
        // On PG only the tabs of the profile's database: the others belong to
        // a connection of their own (§3.3).
        tabs: regroup(
          s.tabs.map((t) =>
            t.connId === null &&
            (t.profileId === conn.profile.id || t.profileId === null) &&
            (!conn.oneDbPerConn || t.db === null || t.db === profileDatabase(conn))
              ? { ...t, connId: conn.connId, profileId: conn.profile.id }
              : t
          ),
          groupOrder
        ),
      };
    }),

  removeConnection: (connId) =>
    set((s) => {
      // The connections to the profile's other databases close with it
      // (§3.3, the backend closes them too).
      const gone = connFamily(s.connections, connId);
      const connections = s.connections.filter((c) => !gone.has(c.connId));
      const roots = profileConns(connections);
      return {
        connections,
        connPage: s.connPage !== null && gone.has(s.connPage) ? null : s.connPage,
        activeConnId:
          s.activeConnId !== null && gone.has(s.activeConnId)
            ? roots[roots.length - 1]?.connId ?? null
            : s.activeConnId,
        // The old connection's data dies with it: statements zeroed, the
        // tab remains as a disconnected placeholder.
        tabs: s.tabs.map((t) =>
          t.connId !== null && gone.has(t.connId)
            ? {
                ...t,
                connId: null,
                execId: null,
                execState: "idle",
                running: false,
                statements: [],
                selected: 0,
              }
            : t
        ),
      };
    }),

  setCurrentDb: (connId, db) =>
    set((s) => {
      const conn = s.connections.find((c) => c.connId === connId);
      if (conn && db !== null && conn.profile.id !== null && canSwitchDb(conn.profile.dialect)) {
        rememberDb(conn.profile.id, db);
      }
      return {
        connections: s.connections.map((c) => (c.connId === connId ? { ...c, currentDb: db } : c)),
      };
    }),

  setTabDb: (id, db) =>
    set((s) => {
      const tab = s.tabs.find((t) => t.id === id);
      if (!tab || tab.db === db) return {};
      const conn = s.connections.find((c) => c.connId === tab.connId);
      if (conn && db !== null && conn.profile.id !== null && canSwitchDb(conn.profile.dialect)) {
        rememberDb(conn.profile.id, db);
      }
      return { tabs: s.tabs.map((t) => (t.id === id ? { ...t, db } : t)) };
    }),

  setManualCommit: (connId, on) =>
    set((s) => ({
      connections: s.connections.map((c) => (c.connId === connId ? { ...c, manualCommit: on } : c)),
    })),

  noteUndone: (connId) =>
    set((s) => ({
      connections: s.connections.map((c) => (c.connId === connId ? { ...c, undone: c.undone + 1 } : c)),
    })),

  setTxnState: (connId, st) =>
    set((s) => ({
      connections: s.connections.map((c) =>
        c.connId === connId
          ? {
              ...c,
              txn: st.open
                ? {
                    hasWrites: st.has_writes,
                    writes: st.writes,
                    openForMs: st.open_for_ms ?? 0,
                    idleForMs: st.idle_for_ms ?? 0,
                    idleRollbackMs: st.idle_rollback_ms,
                    fetchedAt: Date.now(),
                  }
                : null,
            }
          : c
      ),
    })),

  setConnectError: (err) => set({ connectError: err }),

  openConnPage: (connId) =>
    set((s) => (s.connections.some((c) => c.connId === connId) ? { connPage: connId, activeConnId: connId } : {})),

  hydrate: (tabs, activeTabId, nextTabId) =>
    set({
      // At startup the group order is that of the saved tabs.
      groupOrder: tabs.reduce<number[]>((o, t) => withGroup(o, tabGroup(t)), []),
      tabs: regroup(tabs, []),
      activeTabId,
      nextTabId,
      mru: touchMru(tabs.map((t) => t.id), activeTabId),
    }),

  addTab: (tab) =>
    set((s) => {
      // The group keeps the place it has in `groupOrder`: a connection
      // without tabs does not jump to the front when it opens the first.
      const { tabs, groupOrder } = placeTab(s, s.tabs, tab);
      return {
        tabs,
        groupOrder,
        activeTabId: tab.id,
        connPage: null,
        mru: touchMru(s.mru, tab.id),
        // The "current" connection follows the active tab, the new one too:
        // "+" from a table opened from the tree is born on its connection.
        activeConnId: tab.connId ?? s.activeConnId,
        nextTabId: Math.max(s.nextTabId, Number(tab.id) + 1 || s.nextTabId),
      };
    }),

  activateTab: (id) =>
    set((s) => {
      const tab = s.tabs.find((t) => t.id === id);
      if (!tab) return {};
      return {
        activeTabId: id,
        connPage: null,
        mru: touchMru(s.mru, id),
        // The "current" connection follows the active tab (M3.5).
        activeConnId: tab.connId ?? s.activeConnId,
      };
    }),

  closeTab: (id) =>
    set((s) => {
      const tabs = s.tabs.filter((t) => t.id !== id);
      const mru = s.mru.filter((x) => x !== id);
      let activeTabId = s.activeTabId;
      if (activeTabId === id) {
        // MRU, not positional: back to where one was working (§9.5).
        activeTabId = mru.find((x) => tabs.some((t) => t.id === x)) ?? tabs[0]?.id ?? "";
      }
      return { tabs, mru, activeTabId };
    }),

  makePermanent: (id) =>
    set((s) => ({
      tabs: s.tabs.map((t) => (t.id === id && t.preview ? { ...t, preview: false } : t)),
    })),

  retitleTab: (id, title) =>
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? { ...t, title } : t)) })),

  setPending: (id, pending) =>
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? { ...t, pending } : t)) })),

  setNewRows: (id, newRows) =>
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? { ...t, newRows } : t)) })),

  setDeleted: (id, deleted) =>
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? { ...t, deleted } : t)) })),

  setStatementRows: (tabId, resultId, rows) =>
    set((s) => ({
      tabs: s.tabs.map((t) => {
        if (t.id !== tabId) return t;
        const st = t.statements[0];
        if (!st || st.resultId !== resultId) return t;
        return { ...t, statements: [{ ...st, rows }] };
      }),
    })),

  setSub: (id, sub) =>
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? { ...t, sub } : t)) })),

  setTableQuery: (id, query) =>
    set((s) => ({
      tabs: s.tabs.map((t) => (t.id === id ? { ...t, tableQuery: query } : t)),
    })),

  resolveDefaultSort: (id, order) =>
    set((s) => ({
      tabs: s.tabs.map((t) =>
        t.id === id && t.applyDefaultSort
          ? {
              ...t,
              applyDefaultSort: false,
              tableQuery:
                order && order.length > 0 && t.tableQuery.order_by.length === 0
                  ? { ...t.tableQuery, order_by: order }
                  : t.tableQuery,
            }
          : t
      ),
    })),

  navigateTable: (id, table, query) =>
    set((s) => ({
      tabs: s.tabs.map((t) =>
        t.id === id && t.kind === "table" && t.table
          ? {
              ...resetTableView(t, table, query),
              // Another table: its default sort, if any.
              applyDefaultSort: true,
              navBack: [...t.navBack, { table: t.table, query: t.tableQuery }],
              navFwd: [],
            }
          : t
      ),
    })),

  navStep: (id, dir) =>
    set((s) => ({
      tabs: s.tabs.map((t) => {
        if (t.id !== id || t.kind !== "table" || !t.table) return t;
        const from = dir < 0 ? t.navBack : t.navFwd;
        const to = from[from.length - 1];
        if (!to) return t;
        const here = { table: t.table, query: t.tableQuery };
        return {
          ...resetTableView(t, to.table, to.query),
          // Back/forward: the stop already has its query, no default.
          applyDefaultSort: false,
          navBack: dir < 0 ? t.navBack.slice(0, -1) : [...t.navBack, here],
          navFwd: dir < 0 ? [...t.navFwd, here] : t.navFwd.slice(0, -1),
        };
      }),
    })),

  moveTab: (id, target, side) =>
    set((s) => {
      const from = s.tabs.find((t) => t.id === id);
      const to = s.tabs.find((t) => t.id === target);
      if (!from || !to || from === to || tabGroup(from) !== tabGroup(to)) return {};
      const tabs = s.tabs.filter((t) => t.id !== id);
      const at = tabs.findIndex((t) => t.id === target) + (side === "after" ? 1 : 0);
      tabs.splice(at, 0, from);
      return { tabs: regroup(tabs, s.groupOrder) };
    }),
  setPinned: (id, pinned) =>
    set((s) => ({
      // A pinned tab is no longer a preview: there is the intent to keep it.
      tabs: s.tabs.map((t) =>
        t.id === id ? { ...t, pinned, preview: pinned ? false : t.preview } : t
      ),
    })),

  tableMoreStart: (tabId, resultId) =>
    set((s) => ({
      tabs: s.tabs.map((t) =>
        t.id === tabId ? { ...t, more: { resultId, loading: true, done: false, error: null } } : t
      ),
    })),

  tableMoreEnd: (tabId, resultId, out, error) =>
    set((s) => ({
      tabs: s.tabs.map((t) => {
        if (t.id !== tabId || t.more?.resultId !== resultId) return t;
        const st = t.statements[0];
        if (!st || st.resultId !== resultId) return { ...t, more: null };
        return {
          ...t,
          more: { resultId, loading: false, done: out !== null && !out.more, error },
          statements: out
            ? [{ ...st, rows: out.rows, layoutBytes: st.layoutBytes + out.layout_bytes }]
            : t.statements,
        };
      }),
    })),

  startExecution: (tabId, execId, _lane = "sticky", sql = "", base = 0) => {
    set((s) => ({
      tabs: s.tabs.map((t) =>
        t.id === tabId
          ? {
              ...t,
              execId,
              execState: "running",
              running: true,
              startedAt: Date.now(),
              statements: [],
              selected: 0,
              execSql: sql,
              execBase: base,
            }
          : t
      ),
    }));
    // Replays the events that arrived before the execId was registered.
    const connId = get().tabs.find((t) => t.id === tabId)?.connId;
    if (connId === null || connId === undefined) return;
    const key = `${connId}:${execId}`;
    const queued = pendingEvents.get(key);
    pendingEvents.delete(key);
    queued?.forEach((ev) => get().handleEvent(connId, ev));
  },

  select: (tabId, i) =>
    set((s) => ({
      tabs: s.tabs.map((t) => (t.id === tabId ? { ...t, selected: i } : t)),
    })),

  handleEvent: (connId, ev) => {
    // §9.3: events of past executions or of closed tabs are ignored — but
    // those of an execution not yet registered must be queued (see
    // pendingEvents): they might belong to an execId whose invoke has not
    // resolved yet.
    const tab = get().tabs.find((t) => t.connId === connId && t.execId === ev.exec_id);
    if (!tab) {
      const key = `${connId}:${ev.exec_id}`;
      const q = pendingEvents.get(key) ?? [];
      q.push(ev);
      pendingEvents.set(key, q);
      // The execIds of already closed tabs will never be replayed: the cap
      // throws away the oldest and keeps the map small.
      while (pendingEvents.size > PENDING_CAP) {
        const oldest = pendingEvents.keys().next().value;
        if (oldest === undefined) break;
        pendingEvents.delete(oldest);
      }
      return;
    }
    const tabId = tab.id;
    set((s) => {
      const idx = s.tabs.findIndex((t) => t.id === tabId);
      if (idx < 0) return {};
      const t = { ...s.tabs[idx] };
      const statements = [...t.statements];
      t.statements = statements;
      const at = (i: number): StatementUi => {
        while (statements.length <= i) statements.push(emptyStatement(statements.length));
        statements[i] = { ...statements[i] };
        return statements[i];
      };
      const byResult = (rid: number): StatementUi | undefined => {
        const i = statements.findIndex((st) => st.resultId === rid);
        return i >= 0 ? at(i) : undefined;
      };

      const patch: Partial<Store> = {};
      switch (ev.type) {
        case "started": {
          for (let i = 0; i < ev.statement_count; i++) at(i);
          break;
        }
        case "statement_started": {
          const st = at(ev.index);
          st.running = true;
          st.charOffset = ev.char_offset;
          t.selected = ev.index;
          break;
        }
        case "schema": {
          const st = at(ev.index);
          st.resultId = ev.result_id;
          st.columns = ev.columns;
          st.streaming = true;
          break;
        }
        case "rows": {
          const st = byResult(ev.result_id);
          if (st) {
            st.rows = ev.rows;
            st.layoutBytes = ev.layout_bytes;
            // New rows = the fetch restarted ("Continue"): banner gone.
            st.budgetPaused = false;
          }
          break;
        }
        case "budget_paused": {
          const st = byResult(ev.result_id);
          if (st) {
            st.rows = ev.rows;
            st.layoutBytes = ev.layout_bytes;
            st.budgetPaused = true;
            st.budgetByRows = ev.by_rows;
          }
          break;
        }
        case "notice": {
          at(ev.index).notices.push(ev.notice);
          break;
        }
        case "statement_finished": {
          const st = at(ev.index);
          st.running = false;
          st.streaming = false;
          st.budgetPaused = false;
          st.outcome = ev.outcome;
          st.elapsedMs = ev.elapsed_ms;
          if (ev.outcome.kind === "row_set") st.rows = ev.outcome.rows;
          if (ev.outcome.kind === "cancelled") st.rows = ev.outcome.rows;
          if (ev.outcome.kind === "failed") t.selected = ev.index;
          break;
        }
        case "finished": {
          t.running = false;
          t.execState = ev.state;
          break;
        }
      }
      const tabs = [...s.tabs];
      tabs[idx] = t;
      return { ...patch, tabs };
    });
    // §9.7: at the end of an execution on the sticky (SQL tab) the
    // transaction state may have changed — asked of the backend, never polled.
    if (ev.type === "finished" && tab.kind === "sql") {
      void ipc
        .txnState(connId)
        .then((st) => get().setTxnState(connId, st))
        .catch(() => {});
      // On MySQL a `USE` in the text changes the sticky's database: it is
      // re-read and becomes the database of the tab that executed (and of
      // the connection, for new tabs).
      const conn = get().connections.find((c) => c.connId === connId);
      if (canSwitchDb(conn?.profile.dialect)) {
        void ipc
          .currentDatabase(connId)
          .then((db) => {
            get().setCurrentDb(connId, db);
            get().setTabDb(tab.id, db);
          })
          .catch(() => {});
      }
    }
  },
}));

/// Last database used per profile (MySQL): on reconnection one restarts
/// from there instead of from "no database". Local preference, like the theme.
const LAST_DB_KEY = "alamo.lastDb";

function lastDbMap(): Record<string, string> {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(LAST_DB_KEY) ?? "{}");
    return v && typeof v === "object" ? (v as Record<string, string>) : {};
  } catch {
    return {};
  }
}

export function lastDb(profileId: number): string | null {
  const v = lastDbMap()[String(profileId)];
  return typeof v === "string" && v !== "" ? v : null;
}

function rememberDb(profileId: number, db: string): void {
  try {
    const m = lastDbMap();
    if (m[String(profileId)] === db) return;
    m[String(profileId)] = db;
    localStorage.setItem(LAST_DB_KEY, JSON.stringify(m));
  } catch {
    // Storage unavailable: only the memory is lost.
  }
}

/// Convenience selectors.
export function activeTab(s: { tabs: TabUi[]; activeTabId: string }): TabUi | undefined {
  return s.tabs.find((t) => t.id === s.activeTabId);
}

export function connById(
  s: { connections: ConnUi[] },
  connId: number | null | undefined
): ConnUi | undefined {
  return connId === null || connId === undefined
    ? undefined
    : s.connections.find((c) => c.connId === connId);
}

/// SQL tab occupying the sticky of a connection, if any (§3: one editor
/// execution at a time PER connection).
export function stickyRunningTab(s: { tabs: TabUi[] }, connId: number): TabUi | undefined {
  return s.tabs.find((t) => t.connId === connId && t.kind === "sql" && t.running);
}

/// Table tab occupying the shared lane of a connection, if any (§3.1: one
/// table load at a time per connection).
export function sharedBusyTab(s: { tabs: TabUi[] }, connId: number): TabUi | undefined {
  return s.tabs.find(
    (t) => t.connId === connId && t.kind === "table" && (t.running || t.more?.loading === true)
  );
}

/// The table tab may have more rows beyond those loaded: first page full
/// and no short page seen so far. The total is unknown (§5.2: no automatic
/// COUNT) — in the status bar it is "N+".
export function tableMayHaveMore(tab: TabUi): boolean {
  const st = tab.statements[0];
  if (tab.kind !== "table" || !st || st.resultId === null) return false;
  if (st.outcome?.kind !== "row_set" || st.outcome.state !== "complete") return false;
  // A first page shorter than the page size (a setting, §16) is the whole table.
  if (st.rows < setting("execution.table_page_rows")) return false;
  return !(tab.more?.resultId === st.resultId && tab.more.done);
}
