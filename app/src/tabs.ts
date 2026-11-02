// Tab orchestration (§9.5) — outside React (§9.2): the per-tab
// `EditorState`s and the workspace persistence must not go through the store
// on every keystroke or switch.
//
// The backend stays at one execution at a time (single sticky, §3): tabs
// share the connection, they do not multiply it.
import { EditorState } from "@codemirror/state";
import { guardUnsaved } from "./pending";
import { RunMode, SqlEditor } from "./editor";
import { t } from "./i18n";
import { ipc, normalizeTableQuery, TableQuery } from "./ipc";
import { dropCache } from "./rowCache";
import { connForDatabase, openDatabasePageOn } from "./databases";
import {
  activeTab,
  connFamily,
  emptyTab,
  openedDbByConn,
  placeTab,
  profileDatabase,
  rememberOpenedDb,
  tabGroup,
  TableRef,
  TabUi,
  useStore,
} from "./store";
import { canSwitchDb } from "./engines";

type PersistedTab = {
  id: string;
  title: string;
  preview: boolean;
  // Absent in workspaces written before table tabs: default "sql".
  kind?: "sql" | "table";
  table?: TableRef | null;
  pinned?: boolean;
  // Absent in workspaces written before the re-query (decision 10).
  tableQuery?: TableQuery;
  // M3.5: origin profile of the tab. Tabs are reborn disconnected
  // (connId null) and reattach when their profile reconnects.
  // Absent in pre-M3.5 workspaces: reattaches to the first connection.
  profileId?: number | null;
  /// Database of the SQL tab (MySQL). Absent in earlier workspaces: null,
  /// the tab takes the connection's at the first execution.
  db?: string | null;
};
type Workspace = { tabs: PersistedTab[]; active: string; next: number };

const states = new Map<string, EditorState>();
let editor: SqlEditor | null = null;
let onRun: (mode: RunMode) => void = () => {};
let onDocChanged: (() => void) | undefined;

const DEFAULT_SQL = "SELECT * FROM bench_wide LIMIT 100;";

export function registerEditor(e: SqlEditor, run: (mode: RunMode) => void, docChanged?: () => void): void {
  editor = e;
  onRun = run;
  onDocChanged = docChanged;
}

export function unregisterEditor(): void {
  // The active tab's state survives the unmounting of the view.
  saveActiveState();
  editor = null;
}

function saveActiveState(): void {
  const s = useStore.getState();
  const id = s.activeTabId;
  // Only SQL tabs have an editor state: with a table tab active the view
  // shows (hidden) the doc of another tab — saving it here would attribute
  // it to the wrong tab.
  const tab = s.tabs.find((t) => t.id === id);
  if (editor && id && tab?.kind === "sql") states.set(id, editor.getState());
}

/// Current SQL of a tab, mounted or not.
export function tabSql(id: string): string {
  const s = useStore.getState();
  if (editor && id === s.activeTabId) return editor.sql();
  return states.get(id)?.doc.toString() ?? "";
}

// ------------------------------------------------------------------ autosave

// §9.4: per-tab debounce, key `editor:<id>`. The id is captured WHEN the
// save is scheduled: if the timer fires after a tab switch, the right
// content comes from the states map, not from the view.
const AUTOSAVE_DEBOUNCE_MS = 800;
let saveTimer: number | undefined;

export function scheduleAutosave(): void {
  const s = useStore.getState();
  const id = s.activeTabId;
  if (!id) return;
  // Editing a preview makes it permanent (§9.5): there is work inside.
  const tab = s.tabs.find((t) => t.id === id);
  if (tab?.preview) makePermanent(id);
  window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => flushAutosave(id), AUTOSAVE_DEBOUNCE_MS);
}

export function flushAutosave(id?: string): void {
  window.clearTimeout(saveTimer);
  const s = useStore.getState();
  const tabId = id ?? s.activeTabId;
  if (!tabId || s.tabs.find((t) => t.id === tabId)?.kind !== "sql") return;
  saveActiveState();
  void ipc.autosaveSet(tabId, tabSql(tabId));
}

// ------------------------------------------------------------------ workspace

/// Tabs must survive a crash (§9.4): structure on SQLite via kv, SQL
/// content in the per-tab autosave (`editor:<id>`).
let persistTimer: number | undefined;

export function persistWorkspace(): void {
  window.clearTimeout(persistTimer);
  persistTimer = window.setTimeout(() => {
    // The empty workspace is saved too: "all closed" must survive the
    // restart, not resurrect the previous tabs.
    const s = useStore.getState();
    const ws: Workspace = {
      tabs: s.tabs.map((t) => ({
        id: t.id,
        title: t.title,
        preview: t.preview,
        kind: t.kind,
        table: t.table,
        pinned: t.pinned,
        tableQuery: t.tableQuery,
        profileId: t.profileId,
        db: t.db,
      })),
      active: s.activeTabId,
      next: s.nextTabId,
    };
    void ipc.workspaceSet(JSON.stringify(ws));
  }, 400);
}

/// Startup: rebuilds the tabs from the saved workspace. Without a workspace
/// (first start) it restarts from M1's single tab (id "0", same autosave
/// buffer: free migration); a saved **empty** workspace stays empty — the
/// user had closed everything.
export async function loadWorkspace(): Promise<void> {
  let ws: Workspace | null = null;
  try {
    const raw = await ipc.workspaceGet();
    if (raw) ws = JSON.parse(raw) as Workspace;
  } catch {
    ws = null;
  }
  if (!ws) {
    ws = { tabs: [{ id: "0", title: t("SQL {n}", { n: 1 }), preview: false }], active: "0", next: 1 };
  }
  const docs = await Promise.all(
    ws.tabs.map((t) =>
      (t.kind ?? "sql") === "sql" ? ipc.autosaveGet(t.id).catch(() => null) : Promise.resolve(null)
    )
  );
  if (!editor) return;
  ws.tabs.forEach((t, i) => {
    if ((t.kind ?? "sql") !== "sql") return;
    states.set(t.id, editor!.newState(docs[i] ?? (t.id === "0" ? DEFAULT_SQL : ""), onRun, onDocChanged));
  });
  const active = ws.tabs.some((t) => t.id === ws!.active)
    ? ws.active
    : ws.tabs[0]?.id ?? "";
  useStore
    .getState()
    .hydrate(
      ws.tabs.map((t) => {
        // connId null: tabs are reborn disconnected, they reattach on the
        // connect of their profile (addConnection).
        const tab = emptyTab(
          t.id,
          // Old workspaces have "schema.table" titles: recomputed.
          t.kind === "table" && t.table ? tableTitle(t.table.schema, t.table.name) : t.title,
          t.preview,
          t.kind ?? "sql",
          t.table ?? null,
          null,
          t.profileId ?? null,
          // Table tabs: PG only, the database of their connection (§3.3).
          t.db ?? null
        );
        // Workspaces saved before the WHERE bar or the multi-column sort.
        const tableQuery = t.tableQuery ? normalizeTableQuery(t.tableQuery) : tab.tableQuery;
        return { ...tab, pinned: t.pinned ?? false, tableQuery };
      }),
      active,
      ws.tabs.length === 0 ? 0 : ws.next
    );
  const st = states.get(active);
  if (st) editor.setState(st);
}

// ------------------------------------------------------------------ actions

/// `db`: database of the new tab (MySQL). Without it, inherits the active
/// SQL tab's if it is on the same connection, otherwise the connection's
/// (the last used).
export function newSqlTab(initialSql = "", preview = false, title?: string, db?: string | null): string {
  saveActiveState();
  const s = useStore.getState();
  const id = String(s.nextTabId);
  // The new tab is born on the current connection (M3.5).
  const conn = s.connections.find((c) => c.connId === s.activeConnId);
  const cur = activeTab(s);
  const inherited =
    cur?.kind === "sql" && conn && cur.connId === conn.connId && cur.db !== null
      ? cur.db
      : conn?.currentDb ?? null;
  useStore.setState({ nextTabId: s.nextTabId + 1 });
  useStore
    .getState()
    .addTab(
      emptyTab(
        id,
        title ?? t("SQL {n}", { n: Number(id) + 1 }),
        preview,
        "sql",
        null,
        conn?.connId ?? null,
        conn?.profile.id ?? null,
        canSwitchDb(conn?.profile.dialect) ? (db === undefined ? inherited : db) : pgDbOf(conn?.connId ?? null)
      )
    );
  const st = editor!.newState(initialSql, onRun, onDocChanged);
  states.set(id, st);
  editor!.setState(st);
  // New tab = the query is written right away. On the next frame: coming
  // from the connection page the editor was hidden and must be shown first,
  // and the click on "+" or the closing palette are done with the focus.
  requestAnimationFrame(() => {
    if (useStore.getState().activeTabId === id) editor?.focus();
  });
  if (initialSql) void ipc.autosaveSet(id, initialSql);
  persistWorkspace();
  return id;
}

/// Database marker of the tab bar: the connection page opened on that
/// database's (schema's) tables, as if entered from its list.
export function openDatabasePage(connId: number, db: string): void {
  const conn = useStore.getState().connections.find((c) => c.connId === connId);
  if (!conn) return;
  // PG (§3.3): the database has a connection of its own. If it cannot be
  // opened, the page of the server's databases says why on a double click.
  if (conn.oneDbPerConn) {
    void openDatabasePageOn(connId, db).catch(() => useStore.getState().openConnPage(connId));
    return;
  }
  openedDbByConn.set(connId, db);
  if (conn.profile.id !== null) rememberOpenedDb(conn.profile.id, db);
  useStore.setState((s) => ({ connPage: connId, activeConnId: connId, connPageNav: s.connPageNav + 1 }));
}

/// "New query on this database" from the marker's menu: a SQL tab on that
/// connection, on that database. PG (§3.3): on that database's connection.
export function newSqlTabOn(connId: number, db: string): void {
  const conn = useStore.getState().connections.find((c) => c.connId === connId);
  if (conn?.oneDbPerConn) {
    void connForDatabase(connId, db).then((id) => {
      useStore.setState({ activeConnId: id });
      newSqlTab();
    });
    return;
  }
  useStore.setState({ activeConnId: connId });
  newSqlTab("", false, undefined, db);
}

/// "+" of the tabbar: like newSqlTab, but while looking at a MySQL table the
/// new tab is born on that table's database — the unqualified query hits
/// the table in front of you right away. The real `USE` starts at the first
/// execution (see `run` in Studio). MySQL only (on PG the database is fixed
/// at connect).
export function newSqlTabFromContext(): string {
  const s = useStore.getState();
  const tab = activeTab(s);
  // Connection page in the foreground: the context is the database opened
  // there (if any), not the table tab underneath.
  const ctx =
    s.connPage !== null
      ? { connId: s.connPage, db: openedDbByConn.get(s.connPage) ?? null }
      : tab?.kind === "table" && tab.table && tab.connId !== null
        ? { connId: tab.connId, db: tab.table.schema }
        : null;
  const conn = ctx ? s.connections.find((c) => c.connId === ctx.connId) : undefined;
  if (ctx && ctx.db !== null && canSwitchDb(conn?.profile.dialect)) return newSqlTab("", false, undefined, ctx.db);
  return newSqlTab();
}

export function activateTab(id: string): void {
  const s = useStore.getState();
  if (!s.tabs.some((t) => t.id === id)) return;
  if (id === s.activeTabId) {
    // Already the active tab, but covered by the connection page: the
    // click must bring it back to the foreground, not be ignored.
    if (s.connPage !== null) useStore.setState({ connPage: null });
    return;
  }
  // The pending save of the current tab starts NOW, before the view
  // mounts the other tab.
  flushAutosave();
  s.activateTab(id);
  const st = states.get(id);
  if (st && editor) editor.setState(st);
  persistWorkspace();
}

/// Closing: releases the result sets in Rust too (§8) and the autosave
/// buffer — without that, orphans pile up. A tab that is executing does not
/// close: its execution occupies the sticky and must be cancelled first.
///
/// The last tab really closes too: the "no tabs" state is legitimate (the
/// view shows the empty state, one restarts from the +) and the title
/// counter restarts — closing everything must not leave an "SQL 7" around.
export function closeTab(id: string): void {
  const s = useStore.getState();
  const tab = s.tabs.find((t) => t.id === id);
  if (!tab || tab.running) return;
  releaseTabResults(tab);
  states.delete(id);
  void ipc.autosaveDelete(id);
  s.closeTab(id);
  const cur = useStore.getState();
  if (cur.tabs.length === 0) {
    useStore.setState({ nextTabId: 0 });
    // Last tab closed: instead of an empty page we go back to the
    // connection page (the closed tab's, or the first open one).
    const back = cur.connections.find((c) => c.connId === tab.connId) ?? cur.connections[0];
    if (back && cur.connPage === null) cur.openConnPage(back.connId);
  } else {
    const st = states.get(cur.activeTabId);
    if (st && editor) editor.setState(st);
  }
  persistWorkspace();
}

export function releaseTabResults(tab: TabUi): void {
  // Disconnected tab (connId null): the session is already dead and the
  // result sets with it — nothing to release.
  if (tab.connId === null) return;
  for (const stmt of tab.statements) {
    if (stmt.resultId !== null) {
      void ipc.releaseResult(tab.connId, stmt.resultId);
      dropCache(tab.connId, stmt.resultId);
    }
  }
}

// Name only: the schema/database prefix made the tab unreadable.
// The qualified name stays in the tooltip (TabBar).
function tableTitle(_schema: string, table: string): string {
  return table;
}

function profileIdOf(connId: number): number | null {
  const s = useStore.getState();
  return s.connections.find((c) => c.connId === connId)?.profile.id ?? null;
}

/// PG (§3.3): the database of the tab's connection, kept on the tab to put
/// it back on that database after a restart. `null` elsewhere.
function pgDbOf(connId: number | null): string | null {
  const conn = useStore.getState().connections.find((c) => c.connId === connId);
  return conn?.oneDbPerConn ? (conn.currentDb ?? profileDatabase(conn)) : null;
}

export function newTableTab(connId: number, schema: string, name: string, preview = false): string {
  saveActiveState();
  const s = useStore.getState();
  const id = String(s.nextTabId);
  useStore.setState({ nextTabId: s.nextTabId + 1 });
  useStore
    .getState()
    .addTab({
      ...emptyTab(
        id,
        tableTitle(schema, name),
        preview,
        "table",
        { schema, name },
        connId,
        profileIdOf(connId),
        pgDbOf(connId)
      ),
      applyDefaultSort: true,
    });
  persistWorkspace();
  return id;
}

/// Click from the tree (§9.5): preview table tab, only one — the next click
/// replaces it. Double click makes it permanent. The data is loaded by the
/// view (shared lane of ITS connection, §3.1).
export function openTablePreview(connId: number, schema: string, table: string): string {
  const s = useStore.getState();
  // The same table on the same connection is already open: activate it.
  // Same name on another connection = distinct tab (M3.5).
  const same = s.tabs.find(
    (t) =>
      t.kind === "table" &&
      t.connId === connId &&
      t.table?.schema === schema &&
      t.table?.name === table
  );
  if (same) {
    activateTab(same.id);
    return same.id;
  }
  const existing = s.tabs.find((t) => t.preview);
  // A preview that is executing is not replaced: it would lose the
  // execution (and the Cancel button). It is promoted and a new one opens.
  if (existing?.running) {
    makePermanent(existing.id);
    return newTableTab(connId, schema, table, true);
  }
  if (existing) {
    saveActiveState();
    releaseTabResults(existing);
    // The preview could be an SQL tab (old workspaces): drop the editor
    // state and the autosave buffer, the replacement makes it a table tab.
    states.delete(existing.id);
    void ipc.autosaveDelete(existing.id);
    const fresh = {
      ...emptyTab(
        existing.id,
        tableTitle(schema, table),
        true,
        "table",
        { schema, name: table },
        connId,
        profileIdOf(connId),
        pgDbOf(connId)
      ),
      applyDefaultSort: true,
    };
    // Replacement + repositioning: into the block of its database (§9.5).
    useStore.setState((prev) =>
      placeTab(prev, prev.tabs.filter((t) => t.id !== existing.id), fresh)
    );
    useStore.getState().activateTab(existing.id);
    persistWorkspace();
    return existing.id;
  }
  return newTableTab(connId, schema, table, true);
}

export function makePermanent(id: string): void {
  useStore.getState().makePermanent(id);
  persistWorkspace();
}

export function togglePin(id: string): void {
  const s = useStore.getState();
  const tab = s.tabs.find((t) => t.id === id);
  if (!tab) return;
  s.setPinned(id, !tab.pinned);
  persistWorkspace();
}

/// Drop of a dragged tab (TabBar): new position, then the workspace is
/// saved — the order of the bar is part of it.
export function moveTab(id: string, target: string, side: "before" | "after"): void {
  useStore.getState().moveTab(id, target, side);
  persistWorkspace();
}

/// "Close others" (§9.5): spares the pinned ones and those executing — an
/// execution occupies its connection and must be cancelled first.
export function closeOthers(id: string): void {
  const s = useStore.getState();
  void closeTabsGuarded(s.tabs.filter((t) => t.id !== id && !t.pinned && !t.running).map((t) => t.id));
}

/// Closes the tabs after the Save / Discard / Cancel question, if any has
/// unsaved grid edits. Cancel = none of them closes.
export async function closeTabsGuarded(ids: string[]): Promise<void> {
  if (ids.length === 0 || !(await guardUnsaved(ids))) return;
  for (const id of ids) closeTab(id);
}

/// "Close tabs to the right / to the left" (§9.5): `tabs` in the store is
/// already in bar order (groups per connection), so the index is enough.
/// Same exemptions as "Close others": pinned and executing ones stay.
export function closeToSide(id: string, side: "left" | "right"): void {
  const s = useStore.getState();
  const i = s.tabs.findIndex((t) => t.id === id);
  if (i < 0) return;
  const victims = side === "right" ? s.tabs.slice(i + 1) : s.tabs.slice(0, i);
  void closeTabsGuarded(victims.filter((t) => !t.pinned && !t.running).map((t) => t.id));
}

/// How many tabs `closeToSide` would close: the menu disables the entry
/// when there is nothing to close on that side.
export function countToSide(id: string, side: "left" | "right"): number {
  const s = useStore.getState();
  const i = s.tabs.findIndex((t) => t.id === id);
  if (i < 0) return 0;
  const victims = side === "right" ? s.tabs.slice(i + 1) : s.tabs.slice(0, i);
  return victims.filter((t) => !t.pinned && !t.running).length;
}

/// From the context menu of the connection label: closes all the tabs of
/// the group (same exemptions). The connection stays open.
export function closeGroupTabs(gid: number): void {
  const s = useStore.getState();
  void closeTabsGuarded(
    s.tabs.filter((t) => tabGroup(t) === gid && !t.pinned && !t.running).map((t) => t.id)
  );
}

/// From the menu of the connection label, which in the bar behaves like a
/// tab (as on DBeaver): "others" are all the tabs, "to the right" its own
/// and those of the connections after, "to the left" those of the
/// connections before. The position is that of the groups (`groupOrder`),
/// as the bar draws them. Same exemptions: pinned and executing ones stay.
export function groupSideIds(gid: number, side: "others" | "left" | "right"): string[] {
  const s = useStore.getState();
  const order = s.groupOrder;
  const i = order.indexOf(gid);
  const pick = (g: number) => {
    if (side === "others") return true;
    const j = order.indexOf(g);
    return side === "right" ? j >= i : j >= 0 && j < i;
  };
  return s.tabs.filter((t) => pick(tabGroup(t)) && !t.pinned && !t.running).map((t) => t.id);
}

/// Ctrl+Tab: MRU, not positional (§9.5). `Ctrl+PgUp/PgDn` do the
/// positional cycle.
export function cycleMru(): void {
  const s = useStore.getState();
  const next = s.mru.find((id) => id !== s.activeTabId && s.tabs.some((t) => t.id === id));
  if (next) activateTab(next);
}

/// Disconnection of ONE connection (M3.5): releases the results of its tabs
/// (which remain, disconnected) and closes the session in Rust. Refused if
/// one of its tabs is executing: cancel first.
/// Closes a profile's connection, with those to its server's other
/// databases (§3.3).
export function disconnectConn(connId: number): boolean {
  const s = useStore.getState();
  const family = connFamily(s.connections, connId);
  if (s.tabs.some((t) => t.connId !== null && family.has(t.connId) && t.running)) return false;
  for (const t of s.tabs) {
    if (t.connId !== null && family.has(t.connId)) releaseTabResults(t);
  }
  s.removeConnection(connId);
  void ipc.disconnect(connId);
  persistWorkspace();
  return true;
}

export function cyclePositional(delta: 1 | -1): void {
  const s = useStore.getState();
  const i = s.tabs.findIndex((t) => t.id === s.activeTabId);
  if (i < 0 || s.tabs.length < 2) return;
  const j = (i + delta + s.tabs.length) % s.tabs.length;
  activateTab(s.tabs[j].id);
}
