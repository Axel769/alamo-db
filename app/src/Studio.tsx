// M2: schema tree + multi-tab SQL + autocomplete (§9.5, §10, §15).
// M3.5: several connections open together (DBeaver model) — the toolbar
// follows the active tab's connection; every connection has its sticky (§3).
import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query";
import { ProdTag } from "./ProdTag";
import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ipc, AutocompleteOut, ErrorOut, Profile, ScriptItem, StatementClassOut } from "./ipc";
import { RunContext, RunMode, SqlEditor } from "./editor";
import { executionFor, setEffective, setting, useSettings } from "./settings";
import { keyHint, useAction, withKey } from "./keymap";
import {
  useStore,
  activeTab,
  connById,
  lastDb,
  openedDbByConn,
  connFamily,
  familyWrites,
  profileConns,
  profileDatabase,
  rootConnId,
  savedOpenedDb,
  unsavedCount,
  stickyRunningTab,
  tableMayHaveMore,
  ConnUi,
  StatementUi,
  TabUi,
  TxnUi,
} from "./store";
import ConnectDialog from "./ConnectDialog";
import ExplainEstimate from "./ExplainEstimate";
import SettingsDialog from "./SettingsDialog";
import SshProfilesDialog, { SshPick } from "./SshProfilesDialog";
import ConnectionsPanel from "./ConnectionsPanel";
import { ExportConnectionsDialog, ImportConnectionsDialog, TransferKind } from "./ConnectionsTransfer";
import ConnectionPage, { connInitials } from "./ConnectionPage";
import CommandPalette from "./CommandPalette";
import SchemaTree from "./SchemaTree";
import { recordHistory } from "./history";
import TabBar from "./TabBar";
import TableTab from "./TableTab";
import { freeSticky } from "./sticky";
import { ErrorPanel, resultIncomplete, singleSourceTable } from "./ResultPane";
import SqlResultPane from "./SqlResultPane";
import { ExportDialog } from "./ExportDialog";
import { dropCache } from "./rowCache";
import { connForDatabase } from "./databases";
import { asErrorOut } from "./errors";
import { locale, t, tMsg, useLang } from "./i18n";
import {
  closeTabsGuarded,
  cycleMru,
  cyclePositional,
  disconnectConn,
  flushAutosave,
  loadWorkspace,
  newSqlTab,
  newSqlTabFromContext,
  persistWorkspace,
  registerEditor,
  scheduleAutosave,
  unregisterEditor,
} from "./tabs";
import { IconChevronDown, IconChevronUp, IconDatabase, IconExport, IconPlay, IconPlug, IconPlus, IconSearch, IconSettings, IconStop, IconX } from "./icons";
import { connColor } from "./connColor";
import logoUrl from "../src-tauri/icons/icon.png";
import DbPicker from "./DbPicker";
import UnsavedDialog from "./UnsavedDialog";
import UpdateDialog from "./UpdateDialog";
import TxnLogDialog from "./TxnLogDialog";
import { dirtyTabIds, guardUnsaved } from "./pending";
import { useDragSize } from "./dragSize";
import { Elapsed, RunningNotice } from "./RunningNotice";
import { canSwitchDb, engineShortName, transactionalDdl } from "./engines";
import { sidePanels } from "./panels";

const queryClient = new QueryClient({
  defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false } },
});

/// Sidebar panels (VSCode style): one icon per panel in the activity bar,
/// content swapped in the same container. Built in: the two that need the
/// Studio's state; the others come from the registry (`panels.ts`).
const BUILTIN_PANELS = ["schema", "connections"];

function savedPanel(): string | null {
  const saved = localStorage.getItem("alamo.panel");
  if (saved !== null) {
    const known = BUILTIN_PANELS.includes(saved) || sidePanels().some((p) => p.id === saved);
    return known ? saved : null;
  }
  // Migration from the old binary toggle of the tree (alamo.tree).
  return localStorage.getItem("alamo.tree") !== "0" ? "schema" : null;
}

export default function Studio() {
  return (
    <QueryClientProvider client={queryClient}>
      <StudioInner />
    </QueryClientProvider>
  );
}

function StudioInner() {
  // Settings version (§16): a language change re-renders the whole tree
  // from here, `t()` reads the language on every call.
  useLang();
  const editorRef = useRef<SqlEditor | null>(null);
  const editorHost = useRef<HTMLDivElement | null>(null);
  const store = useStore();
  const [gridSize, setGridSize] = useState({ w: 800, h: 400 });
  const gridHost = useRef<HTMLDivElement | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  // Profile preselected in the dialog (from the connections panel).
  const [dialogProfileId, setDialogProfileId] = useState<number | null>(null);
  // Export/import of the saved connections (ConnectionsTransfer).
  const [transfer, setTransfer] = useState<TransferKind | null>(null);
  /// SSH profiles manager, above the connections dialog or on its own
  /// (command palette): what it opens on, null = closed.
  const [sshManager, setSshManager] = useState<{ id: SshPick } | null>(null);
  /// The profile picked in the manager, for the SSH tab of the connections
  /// dialog; `v` grows at every closing so the same id is picked again.
  const [sshPick, setSshPick] = useState<{ id: number | null; v: number } | null>(null);
  // Grows on every import: ConnectDialog re-reads the profiles (the panel
  // goes through the ["profiles"] query).
  const [profilesVersion, setProfilesVersion] = useState(0);
  const [workspaceReady, setWorkspaceReady] = useState(false);
  // Panel sidebar (VSCode style): schema, connections, history.
  // The hidden panel is CSS only: it stays mounted and loses neither
  // expansion state nor cache. Persisted preference ("" = sidebar closed).
  // Auto-hide panel: by default it OVERLAYS the content and closes by itself
  // when a table or a query is opened — the data keeps the whole width.
  // "Pinned" brings it back side by side and persistent, as before.
  const [panelPinned, setPanelPinned] = useState(() => localStorage.getItem("alamo.panelPinned") === "1");
  // Results pane of the SQL tab: it does not exist until the tab has ever
  // executed (the editor takes everything), then it folds from the handle
  // and resizes by dragging it. The height is a preference (localStorage);
  // the fold is per tab and per session, and reopens on every execution:
  // whoever launches a query wants to see its outcome.
  const studioMain = useRef<HTMLDivElement | null>(null);
  // Strip of per-statement tabs: with many tabs it scrolls, and the
  // selected tab (click, Script, new execution) must stay in view.
  const stmtTabs = useRef<HTMLDivElement | null>(null);
  const results = useDragSize("alamo.resultsH");
  const resultsH = results.size;
  const [resultsCollapsed, setResultsCollapsed] = useState<Record<string, boolean>>({});
  const [activePanel, setActivePanel] = useState<string | null>(() =>
    // Auto-hide does not reopen at startup: it would cover the restored work.
    localStorage.getItem("alamo.panelPinned") === "1" ? savedPanel() : null
  );
  // ⌘B with the sidebar closed reopens the last used panel, like VSCode.
  const lastPanel = useRef<string>(savedPanel() ?? "schema");
  const togglePanel = useCallback((id: string) => {
    setActivePanel((cur) => {
      const next = cur === id ? null : id;
      if (next) lastPanel.current = next;
      localStorage.setItem("alamo.panel", next ?? "");
      return next;
    });
  }, []);
  const toggleSidebar = useCallback(() => {
    setActivePanel((cur) => {
      const next = cur ? null : lastPanel.current;
      localStorage.setItem("alamo.panel", next ?? "");
      return next;
    });
  }, []);
  const closePanel = useCallback(() => {
    setActivePanel(null);
    localStorage.setItem("alamo.panel", "");
  }, []);
  // Sidebar width: drag the right edge, a single one for pinned and
  // auto-hide.
  const panelSize = useDragSize("alamo.panelW");
  const panelW = panelSize.size;
  const startPanelSizeDrag = panelSize.startDrag;
  const startPanelDrag = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      const panel = e.currentTarget.parentElement;
      if (!panel) return;
      const left = panel.getBoundingClientRect().left;
      // The content keeps at least 300px.
      const maxW = Math.max(180, Math.min(700, window.innerWidth - left - 300));
      startPanelSizeDrag(e, (ev) => Math.max(180, Math.min(maxW, Math.round(ev.clientX - left))));
    },
    [startPanelSizeDrag],
  );
  const panelResize = (
    <div
      className="panel-resize"
      title={t("Drag to resize, double-click for the default width")}
      onPointerDown={startPanelDrag}
      onDoubleClick={panelSize.reset}
    />
  );
  const togglePinned = useCallback(() => {
    setPanelPinned((cur) => {
      localStorage.setItem("alamo.panelPinned", cur ? "0" : "1");
      return !cur;
    });
  }, []);
  // On opening, the panel filter takes the focus: ⌘B, two letters and the
  // table is one click away.
  useEffect(() => {
    if (!activePanel) return;
    const raf = requestAnimationFrame(() =>
      document.querySelector<HTMLInputElement>(".side-panel:not([hidden]) .tree-filter input")?.focus()
    );
    return () => cancelAnimationFrame(raf);
  }, [activePanel]);
  // Auto-hide: click outside or Esc close. Esc with text in the filter only
  // clears it (the panel handles it): capture, to read the value before
  // React zeroes it.
  useEffect(() => {
    if (!activePanel || panelPinned) return;
    const onDown = (e: MouseEvent) => {
      if ((e.target as Element | null)?.closest(".side-panel, .activity-bar")) return;
      closePanel();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      const el = document.activeElement;
      if (!el?.closest(".side-panel")) return;
      if (el instanceof HTMLInputElement && el.value !== "") return;
      closePanel();
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [activePanel, panelPinned, closePanel]);
  // A tab was opened from the panel (tree table, history entry): the
  // floating panel is done. The delay lets the second click of the double
  // click arrive (permanent tab instead of preview).
  const panelCloseTimer = useRef<number | null>(null);
  const panelDone = useCallback(() => {
    if (panelPinned) return;
    if (panelCloseTimer.current !== null) window.clearTimeout(panelCloseTimer.current);
    panelCloseTimer.current = window.setTimeout(closePanel, 220);
  }, [panelPinned, closePanel]);
  const onTreeClick = useCallback(
    (e: React.MouseEvent) => {
      if ((e.target as Element).closest(".tree-node.rel")) panelDone();
    },
    [panelDone]
  );
  const panelClass = "side-panel" + (panelPinned ? "" : " floating");
  const panelStyle = panelW !== null ? { width: panelW } : undefined;
  const panelFoot = (
    <div className="panel-foot">
      <span>{panelPinned ? t("panel pinned") : t("closes on its own")}</span>
      <button
        className={"panel-pin" + (panelPinned ? " on" : "")}
        title={
          panelPinned
            ? t("Back to auto-hide: the panel overlays the content and closes when you open a table")
            : t("Keep the panel always open, next to the content")
        }
        onClick={togglePinned}
      >
        {panelPinned ? t("Unpin panel") : t("Keep open")}
      </button>
    </div>
  );
  const openConnectDialog = useCallback((profileId?: number | null) => {
    setDialogProfileId(profileId ?? null);
    setDialogOpen(true);
  }, []);
  // App settings (language, then theme): gear at the bottom of the
  // activity bar, like VSCode — never inside the connections dialog.
  const [settingsOpen, setSettingsOpen] = useState(false);
  // What ⌘↵ and the Run button execute (§16).
  const runMode = useSettings((s) => s.settings.editor.run_shortcut);
  // Command palette (⌘K).
  const [paletteOpen, setPaletteOpen] = useState(false);
  // App closing blocked by the backend (§9.7.2): uncommitted writes, the
  // red button or Cmd+Q stop and the choice goes through here.
  const [quitBlocked, setQuitBlocked] = useState(false);
  // DOM event, not Tauri's `listen`: without the ACL capability that
  // `listen` is refused and the signal would never arrive (the backend sends
  // it with `eval`, see `signal_quit_blocked`).
  useEffect(() => {
    const onBlocked = () => {
      // First the grid edits (Save / Discard / Cancel), then the
      // transactions with writes (the usual dialog); nothing open = quits.
      void (async () => {
        if (!(await guardUnsaved(dirtyTabIds()))) return;
        // Fresh transaction state from the backend, for every connection:
        // the gate decided on that, and the dialog must say the same.
        const conns = useStore.getState().connections;
        await Promise.all(
          conns.map((c) =>
            ipc
              .txnState(c.connId)
              .then((st) => useStore.getState().setTxnState(c.connId, st))
              .catch(() => {})
          )
        );
        const txnWrites = useStore.getState().connections.some((c) => c.txn?.hasWrites);
        if (txnWrites) setQuitBlocked(true);
        else void ipc.appQuit();
      })();
    };
    window.addEventListener("alamo:quit-blocked", onBlocked);
    return () => window.removeEventListener("alamo:quit-blocked", onBlocked);
  }, []);

  // macOS menu "Settings…" (⌘,): the native item signals the webview (§16).
  useEffect(() => {
    const onOpen = () => setSettingsOpen(true);
    window.addEventListener("alamo:open-settings", onOpen);
    return () => window.removeEventListener("alamo:open-settings", onOpen);
  }, []);

  // §9.7.2: the backend rolled back a transaction left idle.
  useEffect(() => {
    const onRollback = (e: Event) => {
      const { connId, writes } = (e as CustomEvent<{ connId: number; writes: boolean }>).detail;
      if (writes) useStore.getState().noteUndone(connId);
      void ipc
        .txnState(connId)
        .then((st) => useStore.getState().setTxnState(connId, st))
        .catch(() => {});
      const name = connById(useStore.getState(), connId)?.profile.name ?? "";
      const min = executionFor(connId).idle_rollback_minutes;
      setConnNote({
        warn: writes,
        text: writes
          ? t("{name}: transaction idle for {min} minutes, rolled back (ROLLBACK). Its uncommitted writes were lost.", { name, min })
          : t("{name}: transaction idle for {min} minutes, closed (ROLLBACK).", { name, min }),
      });
    };
    window.addEventListener("alamo:txn-idle-rollback", onRollback);
    return () => window.removeEventListener("alamo:txn-idle-rollback", onRollback);
  }, []);

  // The backend must know whether there are unsaved grid edits: the closing
  // gate (red button, Cmd+Q) treats them like the writes.
  useEffect(() => {
    let last = false;
    return useStore.subscribe((s) => {
      const any = s.tabs.some((t) => unsavedCount(t) > 0);
      if (any === last) return;
      last = any;
      void ipc.setUnsavedEdits(any).catch(() => {});
    });
  }, []);

  const connected = store.connections.length > 0;
  // Connection page in the foreground: for the rest of the Studio it is
  // like "no tab" (no editor, no results), but the toolbar follows its
  // connection.
  const page = store.connPage !== null ? connById(store, store.connPage) : undefined;
  // Disconnect and Reconnect on the page act on the profile's connection,
  // also from the page of another database of a PG server (§3.3); the
  // writes that disconnecting would lose are those of all of them.
  const pageRoot = page && connById(store, rootConnId(store.connections, page.connId));
  const pageWrites = pageRoot !== undefined && familyWrites(store.connections, pageRoot.connId);
  const tab = page ? undefined : activeTab(store);
  // The connection "in the toolbar" (M3.5): the active tab's. A
  // disconnected tab does NOT show another connection's profile — it would
  // show a context that is not its own. Without tabs: the last used connection.
  const activeConn = tab ? connById(store, tab.connId) : connById(store, store.activeConnId);
  const activeConnId = activeConn?.connId ?? null;
  // Database the editor works on: on MySQL it is the TAB's (every tab has
  // its own, the `USE` starts at execution); on PG and without tabs, the
  // connection's.
  const currentDb =
    tab?.kind === "sql" && canSwitchDb(activeConn?.profile.dialect) ? tab.db : activeConn?.currentDb ?? null;
  // Sticky of the active connection busy → Run/Disconnect blocked.
  const stickyTab = activeConnId !== null ? stickyRunningTab(store, activeConnId) : undefined;
  const anyRunning = stickyTab !== undefined;
  // Execution stopped on the pause (Continue / Stop): Run stays enabled and
  // stops it itself (see `run`).
  const stickyPaused = stickyTab?.statements.some((x) => x.budgetPaused) ?? false;
  // The active tab is paused (row pages or memory budget): it is not
  // working — no spinner, timer or progress thread.
  const tabPaused = tab?.statements.find((x) => x.budgetPaused);
  const tabWorking = !!tab?.running && !tabPaused;

  // §9.7: pre-execution confirmation on prod (first write / irreversible).
  const [writeGate, setWriteGate] = useState<WriteGate | null>(null);
  // Explicit commit/rollback error, shown as a banner.
  const [txnError, setTxnError] = useState<ErrorOut | null>(null);
  // Transaction log (§9.7.2) open on this connection.
  const [txnLogFor, setTxnLogFor] = useState<number | null>(null);
  // §9.7: manual commit on a non-prod connection (toolbar switch).
  const toggleManualCommit = async (connId: number, on: boolean) => {
    try {
      await ipc.setManualCommit(connId, on);
      useStore.getState().setManualCommit(connId, on);
      setTxnError(null);
    } catch (e) {
      setTxnError(asErrorOut(e));
    }
  };
  // Execute rejected BEFORE starting (read-only, lane busy): no events, so
  // no error pane — it needs a banner of its own.
  const [execError, setExecError] = useState<ErrorOut | null>(null);
  // For the Run button label: selection / current query / everything.
  const [runCtx, setRunCtx] = useState<RunContext>({ hasSelection: false, statements: 0 });
  const [runMenu, setRunMenu] = useState(false);
  // The banner belongs to the connection that rejected: switching
  // connection (or to a tab of another connection) it must not hang over an
  // unrelated context.
  useEffect(() => setExecError(null), [activeConnId]);
  // Disconnect with a transaction with writes: the first click arms, the
  // second executes (ConnectDialog pattern — no window.confirm).
  const [discArmed, setDiscArmed] = useState(false);
  // Right click on a connection's avatar (activity bar): menu with
  // "Disconnect", same two-click arming if there are writes.
  const [avMenu, setAvMenu] = useState<{ connId: number; x: number; y: number } | null>(null);
  const [avArmed, setAvArmed] = useState(false);

  // Escalation §7.2: if ~10s after the cancel the execution is still alive,
  // the server did not take it (pooler, non-interruptible query) → the
  // termination of the connection is offered, destructive and never automatic.
  const [escalate, setEscalate] = useState(false);
  useEffect(() => {
    if (!anyRunning) setEscalate(false);
  }, [anyRunning]);

  // The connection's database (the sticky's real one) is read at connect
  // and at every end of execution (store, `handleEvent` finished): a `USE`
  // in the text changes it. Never during: the sticky is busy.

  // On MySQL the context can also be CHANGED: the toolbar shows a select
  // with the schemas — same query key as the tree, shared cache.
  // The choice is the tab's; the `USE` on the sticky starts when executing.
  // On PG the database is fixed at connect: the badge remains.
  const dbSwitchable = canSwitchDb(activeConn?.profile.dialect);
  const schemas = useQuery({
    queryKey: ["schemas", activeConnId],
    queryFn: () => ipc.schemaList(activeConnId!),
    staleTime: Infinity,
    enabled: dbSwitchable && activeConnId !== null,
  });
  // Selectable databases: the non-system ones, plus the current one if it
  // is not in the list (system, or list not arrived yet).
  const dbOptions = useMemo(() => {
    const names = (schemas.data ?? []).filter((x) => !x.is_system).map((x) => x.name);
    return currentDb !== null && !names.includes(currentDb) ? [currentDb, ...names] : names;
  }, [schemas.data, currentDb]);
  // Outcome of the last automatic reconnection (or of its failure), shown
  // as a banner above the editor.
  // `retry`: connection to retry with the banner button, when the automatic
  // reconnection failed (network down at that moment).
  // `busy`: written by `stickyReady`, it goes away by itself once the
  // connection is free again.
  const [connNote, setConnNote] = useState<{
    warn: boolean;
    text: string;
    retry?: number;
    busy?: boolean;
  } | null>(null);
  useEffect(() => {
    if (!connNote || connNote.warn) return;
    const id = window.setTimeout(() => setConnNote(null), 6000);
    return () => window.clearTimeout(id);
  }, [connNote]);

  /// `freeSticky` for a command of the user: if the sticky is busy the
  /// command does not start and the banner says why.
  const stickyReady = useCallback(async (connId: number) => {
    const busy = await freeSticky(connId);
    setConnNote((n) => (busy !== null ? { warn: true, text: busy, busy: true } : n?.busy ? null : n));
    return busy === null;
  }, []);
  // PG (§3.3): the active tab works on another database of a connected
  // profile but is detached (restart, reconnection): its database's
  // connection opens now, and the tabs on it come back with it. Only for
  // the tab in front of the user: restoring a workspace must not open a
  // connection per database at once. One attempt per tab and connection:
  // a failure is said once, the tree or the page retry.
  const reattachTried = useRef(new Set<string>());
  const detached = tab && tab.connId === null && tab.profileId !== null && tab.db !== null ? tab : undefined;
  const detachedRoot = detached
    ? profileConns(store.connections).find((c) => c.profile.id === detached.profileId && c.oneDbPerConn)
    : undefined;
  useEffect(() => {
    if (!detached || !detachedRoot || detached.db === null) return;
    const db = detached.db;
    const key = `${detachedRoot.connId}\u0000${db}`;
    if (db === profileDatabase(detachedRoot) || reattachTried.current.has(key)) return;
    reattachTried.current.add(key);
    connForDatabase(detachedRoot.connId, db).catch((e: unknown) =>
      setConnNote({
        warn: true,
        text: t("The database \"{db}\" could not be opened: {msg}", { db, msg: tMsg(asErrorOut(e).message) }),
      })
    );
  }, [detached?.id, detached?.db, detachedRoot?.connId]);
  // Check or reopening of the connection in progress (`ensureLive`): with
  // the network down it can take long and without signs everything looks
  // stuck. It is shown (banner, status bar, Run button) after a moment, so
  // the normal ping does not flicker. `reconnectSeq`: the last call counts.
  // `force`: "Reconnect" requested by the user (see `reconnectConn`).
  const [reconnecting, setReconnecting] = useState<{ connId: number; startedAt: number; force: boolean } | null>(
    null
  );
  const reconnectSeq = useRef(0);
  const tabConnecting = reconnecting !== null && tab?.connId === reconnecting.connId;

  /// Before using the editor's connection: if it dropped (network, Mac
  /// asleep) the backend reopens it; here the selected database is restored
  /// and what happened is said. `null` = connection not recoverable now, the
  /// action does not start. `force`: reopens without checking (Reconnect).
  const ensureLive = useCallback(async (connId: number, showAfter = 400, force = false) => {
    const seq = ++reconnectSeq.current;
    const startedAt = Date.now();
    const show = window.setTimeout(() => setReconnecting({ connId, startedAt, force }), showAfter);
    try {
      const rc = force ? await ipc.reconnect(connId) : await ipc.ensureConnected(connId);
      if (!force && rc.dead) {
        // §16: automatic reconnection off. Said once, Reconnect in the note.
        setConnNote({ warn: true, text: t("Connection lost. Press Reconnect to reopen it."), retry: connId });
        return null;
      }
      if (!rc.reconnected) return rc;
      // Database and table lists read (or failed) on the old connection:
      // re-read, without having to press "Refresh" too.
      void queryClient.invalidateQueries({ predicate: (q) => q.queryKey[1] === connId });
      const conn = connById(useStore.getState(), connId);
      const db = conn?.currentDb ?? null;
      if (db !== null && canSwitchDb(conn?.profile.dialect)) {
        await ipc.useDatabase(connId, db).catch(() => {});
      }
      const st = await ipc.txnState(connId).catch(() => null);
      if (st) useStore.getState().setTxnState(connId, st);
      if (rc.lost_writes) useStore.getState().noteUndone(connId);
      setConnNote(
        rc.lost_writes
          ? {
              warn: true,
              text: t(
                "Connection restored. The open transaction with uncommitted writes was lost: the server rolled it back."
              ),
            }
          : rc.lost_txn
            ? { warn: true, text: t("Connection restored. The open transaction was closed by the server.") }
            : { warn: false, text: force ? t("Connection reopened.") : t("Connection restored automatically.") }
      );
      return rc;
    } catch (e) {
      setConnNote({
        warn: true,
        text: t("Connection lost and reconnection failed: {msg}. Try again when the network is back.", {
          msg: tMsg(asErrorOut(e).message),
        }),
        retry: connId,
      });
      return null;
    } finally {
      window.clearTimeout(show);
      if (reconnectSeq.current === seq) setReconnecting(null);
    }
  }, []);

  /// "Reconnect" on demand (menu and connection page): tunnel and
  /// connections redone right away, without waiting for the drop to be
  /// noticed. With uncommitted writes the first click arms, like
  /// "Disconnect": reconnecting loses them. `false` = only armed.
  const [recArmed, setRecArmed] = useState<number | null>(null);
  const reconnectConn = (connId: number, hasWrites: boolean): boolean => {
    if (hasWrites && recArmed !== connId) {
      setRecArmed(connId);
      window.setTimeout(() => setRecArmed((a) => (a === connId ? null : a)), 4000);
      return false;
    }
    setRecArmed(null);
    setConnNote(null);
    void stickyReady(connId).then((free) => {
      if (free) void ensureLive(connId, 0, true);
    });
    return true;
  };
  const forcing = (connId: number) => reconnecting?.connId === connId && reconnecting.force;

  /// Toolbar picker: changes the active tab's database, nothing more. The
  /// `USE` starts at execution (`run`), so the other tabs do not see it.
  const switchDb = useCallback((db: string) => {
    const s = useStore.getState();
    const cur = activeTab(s);
    if (cur?.kind === "sql") {
      s.setTabDb(cur.id, db);
      persistWorkspace();
    }
  }, []);

  /// Before executing on MySQL: if the tab's database is not the sticky's,
  /// `USE` on that one. `false` = USE refused (database gone or no
  /// permissions): the execution does not start, the banner says so.
  const syncDb = useCallback(async (connId: number, tabId: string): Promise<boolean> => {
    const s = useStore.getState();
    const cur = s.tabs.find((x) => x.id === tabId);
    const conn = connById(s, connId);
    if (!cur || !conn || !canSwitchDb(conn.profile.dialect) || cur.db === null || cur.db === conn.currentDb) {
      return true;
    }
    try {
      await ipc.useDatabase(connId, cur.db);
      useStore.getState().setCurrentDb(connId, cur.db);
      return true;
    } catch (e) {
      setExecError(asErrorOut(e));
      return false;
    }
  }, []);

  const cancelRun = useCallback(() => {
    const s = useStore.getState();
    const conn = connById(s, activeTab(s)?.connId ?? s.activeConnId);
    if (!conn) return;
    const running = stickyRunningTab(s, conn.connId);
    if (!running || running.execId === null) return;
    const execId = running.execId;
    void ipc.cancel(conn.connId, execId);
    window.setTimeout(() => {
      const cur = useStore.getState();
      const t = cur.tabs.find((x) => x.connId === conn.connId && x.execId === execId);
      if (t?.running) setEscalate(true);
    }, 10_000);
  }, []);

  const terminate = useCallback(async () => {
    setEscalate(false);
    const s = useStore.getState();
    const conn = connById(s, activeTab(s)?.connId ?? s.activeConnId);
    if (!conn) return;
    try {
      await ipc.terminateConnection(conn.connId);
    } catch (e) {
      console.error(e);
    }
    useStore.getState().removeConnection(conn.connId);
    if (useStore.getState().connections.length === 0) openConnectDialog();
  }, []);

  /// Real execution: releases the previous results and fires the execute.
  /// Separate from `run` because on prod profiles the confirmation sits in
  /// between.
  const doExecute = useCallback(async (connId: number, tabId: string, sql: string, base = 0) => {
    const s = useStore.getState();
    const tab = s.tabs.find((t) => t.id === tabId);
    if (!tab || tab.connId !== connId || stickyRunningTab(s, connId)) return;
    if (unsavedCount(tab) > 0) return;
    for (const st of tab.statements) {
      if (st.resultId !== null) {
        void ipc.releaseResult(connId, st.resultId);
        dropCache(connId, st.resultId);
      }
    }
    try {
      setExecError(null);
      const execId = await ipc.execute(connId, sql, (ev) =>
        useStore.getState().handleEvent(connId, ev)
      );
      useStore.getState().startExecution(tabId, execId, "sticky", sql, base);
      setResultsCollapsed((c) => (c[tabId] ? { ...c, [tabId]: false } : c));
      // Only executions that really started go into the history: a rejected
      // execute (or one blocked by the prod gate) is not history.
      recordHistory(connId, sql);
    } catch (e) {
      setExecError(asErrorOut(e));
    }
  }, []);

  /// `mode`: "current" = selection or statement under the cursor,
  /// "selection" = selection or everything, "all" = the whole text (⇧⌘↩ /
  /// "Run all"). Without one, what the setting says ⌘↵ does.
  const run = useCallback(async (mode: RunMode = setting("editor.run_shortcut")) => {
    const s = useStore.getState();
    const tab = activeTab(s);
    // One editor execution at a time PER connection (§3). Table tabs do not
    // execute from the editor; neither does a disconnected tab.
    if (!tab || tab.kind !== "sql" || tab.connId === null) return;
    const connId = tab.connId;
    // §9.5.1: a run releases the results the pending edits belong to.
    if (!(await guardUnsaved([tab.id]))) return;
    if (!(await stickyReady(connId))) return;
    if (!(await ensureLive(connId))) return;
    if (!(await syncDb(connId, tab.id))) return;
    editorRef.current?.clearError();
    flushAutosave();
    const toRun = await editorRef.current?.sqlToRun(mode);
    if (!toRun || !toRun.sql.trim()) return;
    const { sql, offset: base } = toRun;

    // §9.7: on a prod profile, confirmation at the first write of the
    // transaction (decision 8) and ALWAYS on irreversible operations — even
    // with the gate already open there is no safety net for MySQL DDL,
    // TRUNCATE, DROP, UPDATE/DELETE without WHERE. On the other profiles
    // only UPDATE/DELETE without WHERE, as DataGrip and DBeaver do — and
    // only there can the setting turn it off.
    const conn = connById(s, connId);
    if (conn && !conn.profile.is_production && !conn.profile.read_only && setting("editor.confirm_unfiltered_writes")) {
      try {
        const classes = await ipc.classifySql(connId, sql);
        const unfiltered = classes.filter((c) => c.unfiltered);
        if (unfiltered.length > 0) {
          setWriteGate({
            connId,
            tabId: tab.id,
            sql,
            base,
            firstWrite: false,
            irreversible: [],
            ddl: [],
            unknown: [],
            unfiltered: unfiltered.map((c) => statementSnippet(sql, classes, c)),
          });
          return;
        }
      } catch (e) {
        console.error(e);
      }
    }
    if (conn?.profile.is_production) {
      try {
        const classes = await ipc.classifySql(connId, sql);
        if (conn.profile.read_only) {
          // Read-only on prod: certain writes are rejected by the backend
          // with the banner (no double confirmation), but statements OPAQUE
          // to the classifier (unknown) remain a risk to confirm — on prod
          // the gate does not degrade with read-only.
          const unknown = classes.filter((c) => c.class === "unknown");
          if (unknown.length > 0) {
            setWriteGate({
              connId,
              tabId: tab.id,
              sql,
              base,
              firstWrite: false,
              irreversible: [],
              ddl: [],
              unknown: unknown.map((c) => statementSnippet(sql, classes, c)),
              unfiltered: [],
            });
            return;
          }
        } else {
          const irreversible = classes.filter((c) => c.irreversible);
          // DDL a Rollback could undo: confirmed only when the setting asks.
          const ddl = executionFor(connId).confirm_ddl_on_production
            ? classes.filter((c) => c.class === "ddl" && !c.irreversible)
            : [];
          const firstWrite = classes.some((c) => c.is_write) && !conn.txn?.hasWrites;
          if (firstWrite || irreversible.length > 0 || ddl.length > 0) {
            setWriteGate({
              connId,
              tabId: tab.id,
              sql,
              base,
              firstWrite,
              irreversible: irreversible.map((c) => statementSnippet(sql, classes, c)),
              ddl: ddl.map((c) => statementSnippet(sql, classes, c)),
              unknown: [],
              unfiltered: [],
            });
            return;
          }
        }
      } catch (e) {
        // Classification failed (broken connection): the execute below will
        // report the real error.
        console.error(e);
      }
    }
    await doExecute(connId, tab.id, sql, base);
  }, [doExecute, ensureLive, stickyReady, syncDb]);

  // §9.7: explicit commit/rollback on the active connection's sticky.
  const txnAction = useCallback(async (action: "commit" | "rollback") => {
    const s = useStore.getState();
    const conn = connById(s, activeTab(s)?.connId ?? s.activeConnId);
    if (!conn || !(await stickyReady(conn.connId))) return;
    // Connection dropped and reopened: the transaction is gone, there is
    // nothing to commit or roll back (the banner says so).
    const rc = await ensureLive(conn.connId);
    if (!rc || rc.reconnected) return;
    try {
      const st =
        action === "commit"
          ? await ipc.txnCommit(conn.connId)
          : await ipc.txnRollback(conn.connId);
      useStore.getState().setTxnState(conn.connId, st);
      if (action === "rollback" && conn.txn?.hasWrites) useStore.getState().noteUndone(conn.connId);
      setTxnError(null);
    } catch (e) {
      setTxnError(asErrorOut(e));
    }
  }, [ensureLive, stickyReady]);

  // Single editor + workspace: the view is created right away, the tabs
  // come from the persisted workspace (§9.4 — tabs survive a crash).
  useEffect(() => {
    if (!editorHost.current || editorRef.current) return;
    const editor = new SqlEditor(editorHost.current, (mode) => void run(mode), "", scheduleAutosave);
    // Statement boundaries from the Rust splitter, on the active tab's
    // connection: without a connection they are not computed (everything
    // will be executed).
    editor.splitter = async (sql) => {
      const s = useStore.getState();
      const tab = activeTab(s);
      if (!tab || tab.kind !== "sql" || tab.connId === null) return null;
      const chars = Array.from(sql);
      const cps: number[] = [];
      let u16 = 0;
      for (const ch of chars) {
        cps.push(u16);
        u16 += ch.length;
      }
      cps.push(u16);
      const ranges = await ipc.splitSql(tab.connId, sql);
      return ranges.map((r) => ({ from: cps[r.char_offset], to: cps[r.char_offset + r.char_len] }));
    };
    editor.onContext = setRunCtx;
    editorRef.current = editor;
    registerEditor(editor, (mode) => void run(mode), scheduleAutosave);
    void loadWorkspace().then(() => setWorkspaceReady(true));
    return () => {
      unregisterEditor();
      editor.destroy();
      editorRef.current = null;
    };
  }, [run]);

  // The avatar menu closes with a click elsewhere, like the Run one.
  useEffect(() => {
    if (!avMenu) return;
    const close = () => setAvMenu(null);
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
    };
  }, [avMenu]);

  /// Closes the connection; `false` if one of its tabs is executing.
  /// `false` = not closed (execution in progress, or Cancel on the unsaved
  /// changes question).
  const closeConn = async (connId: number): Promise<boolean> => {
    // With the connections to the server's other databases (§3.3).
    const family = connFamily(useStore.getState().connections, connId);
    if (!(await guardUnsaved(dirtyTabIds((tb) => tb.connId !== null && family.has(tb.connId))))) return false;
    if (!disconnectConn(connId)) return false;
    // Drop the introspection cache of THESE connections: the others stay
    // warm (keys ["...", connId, ...]).
    queryClient.removeQueries({ predicate: (q) => family.has(q.queryKey[1] as number) });
    if (useStore.getState().connections.length === 0) openConnectDialog();
    return true;
  };

  // The Run button menu closes with a click elsewhere.
  useEffect(() => {
    if (!runMenu) return;
    const close = () => setRunMenu(false);
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
    };
  }, [runMenu]);

  // Keyboard shortcuts (§16): the global and editor actions of the registry
  // (keymap.ts). The dispatcher skips what CodeMirror or a dialog handled
  // first (Ctrl+B/K inside CodeMirror on mac are emacs cursor keys).
  // Tab navigation (§9.5): MRU order and positional.
  useAction("tab.mru", () => (cycleMru(), true));
  useAction("tab.prev", () => (cyclePositional(-1), true));
  useAction("tab.next", () => (cyclePositional(1), true));
  // Like VSCode: shows/hides the sidebar.
  useAction("sidebar.toggle", () => (toggleSidebar(), true));
  useAction("palette.toggle", () => (setPaletteOpen((v) => !v), true));
  useAction("tab.new_query", () => (newSqlTabFromContext(), true));
  // Search in the text of the active SQL tab. Inside CodeMirror its keymap
  // handles it; here the case where the focus is elsewhere (grid, buttons)
  // but the tab is a query.
  useAction("editor.search", () => {
    const s = useStore.getState();
    if (s.connPage !== null || activeTab(s)?.kind !== "sql" || !editorRef.current) return false;
    editorRef.current.openSearch();
    return true;
  });
  // Closes the active tab, through the unsaved-changes guard. Same
  // exemptions as the middle click (pinned, running). Not while a dialog,
  // the palette or a cell editor (Glide's #portal) is open: the tab behind
  // it would go.
  useAction("tab.close", () => {
    const s = useStore.getState();
    const tab = s.connPage === null ? activeTab(s) : undefined;
    if (!tab || tab.pinned || tab.running || document.querySelector(".dialog-overlay, .cmdk-overlay, #portal textarea")) return true;
    void closeTabsGuarded([tab.id]);
    return true;
  });
  // On macOS ⌘, is the menu accelerator and never reaches the DOM: this
  // serves Windows, Linux and a remapped key.
  useAction("settings.open", () => (setSettingsOpen(true), true));
  // Cancel, Commit and Rollback as the buttons of the query bar allow them.
  useAction("run.cancel", () => tabWorking && (cancelRun(), true));
  useAction("txn.commit", () => {
    if (!activeConn?.txn?.hasWrites || (anyRunning && !stickyPaused)) return false;
    if (activeConn.profile.is_production) setTxnLogFor(activeConn.connId);
    else void txnAction("commit");
    return true;
  });
  useAction("txn.rollback", () => {
    if (!activeConn?.txn || (anyRunning && !stickyPaused)) return false;
    void txnAction("rollback");
    return true;
  });

  // macOS: the native menu has no Undo/Redo (the Cmd+Z fix for CodeMirror,
  // see main.rs) — but input/textarea fields relied on exactly that. Here
  // the fallback; CodeMirror does not come through here (contenteditable,
  // it has its own keymap). Not a shortcut of the registry: it restores a
  // system key, it is not an action of the app.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.metaKey || e.ctrlKey || e.altKey || e.key.toLowerCase() !== "z") return;
      const t = e.target;
      if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement) {
        e.preventDefault();
        document.execCommand(e.shiftKey ? "redo" : "undo");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Autocomplete (§10): flat catalogue of the ACTIVE connection, handed to
  // CodeMirror. Per-connection cache in TanStack: the tree's ⟳ invalidates
  // this too.
  //
  // On MySQL the current database first, alone: on a server with dozens of
  // databases the whole catalogue is slow and exceeds the cap (sorted by
  // name: the last databases were left without suggestions). The whole one
  // starts afterwards, for the qualified names of the other databases.
  const dbCatalogOn = activeConnId !== null && canSwitchDb(activeConn?.profile.dialect) && currentDb !== null;
  const dbCatalog = useQuery({
    queryKey: ["autocomplete", activeConnId, currentDb],
    queryFn: () => ipc.autocompleteCatalog(activeConnId!, currentDb),
    enabled: dbCatalogOn,
    staleTime: Infinity,
  });
  const catalog = useQuery({
    queryKey: ["autocomplete", activeConnId],
    queryFn: () => ipc.autocompleteCatalog(activeConnId!),
    enabled: activeConnId !== null && (!dbCatalogOn || dbCatalog.isFetched),
    staleTime: Infinity,
  });
  const dbCatalogData = dbCatalogOn ? dbCatalog.data : undefined;
  useEffect(() => {
    if (!editorRef.current) return;
    if ((!catalog.data && !dbCatalogData) || activeConnId === null) {
      // Disconnected tab or no connection: the keywords remain.
      editorRef.current.setSchema(undefined);
      return;
    }
    const ns: Record<string, Record<string, string[]>> = {};
    const add = (entries: AutocompleteOut[]) => {
      for (const e of entries) {
        (ns[e.schema] ??= {})[e.table] ??= [];
        ns[e.schema][e.table].push(e.column);
      }
    };
    add(catalog.data ?? []);
    if (dbCatalogData && currentDb !== null) {
      // The current database counts in full from its own query: the one
      // in the general catalogue may be truncated by the cap.
      delete ns[currentDb];
      add(dbCatalogData);
    }
    editorRef.current.setSchema(ns);
  }, [catalog.data, dbCatalogData, currentDb, activeConnId]);

  // Dialect and current database to the autocomplete: on MySQL unqualified
  // names resolve in the sticky's database (USE included), on PG in
  // "public". Without it, table completion does not start (§10).
  const activeDialect = activeConn?.profile.dialect;
  useEffect(() => {
    if (!activeDialect || !editorRef.current) return;
    editorRef.current.setDialectContext(activeDialect, currentDb);
  }, [activeDialect, currentDb, catalog.data, dbCatalogData]);

  // Smoke/bench: automatic connection from ALAMO_BENCH_URL. Normal path:
  // profiles dialog.
  useEffect(() => {
    (async () => {
      if (await invoke<boolean>("m1_smoke")) {
        ipc
          .connect(null)
          .then((connId) =>
            useStore.getState().addConnection({
              connId,
              profile: {
                id: null,
                name: "bench",
                dialect: "postgres",
                settings: {},
                host: "",
                port: 0,
                database: "",
                username: "",
                sslmode: "prefer",
                ssl_root_cert: null,
                ssl_client_cert: null,
                ssl_client_key: null,
                color: null,
                is_production: false,
                read_only: false,
                ssh_profile_id: null,
                db_include: "",
                db_exclude: "",
              },
              currentDb: null,
              txn: null,
              // Bench: plaintext connection from URL, never a real profile.
              security: { encrypted: false, verified: false, protocol: null, cipher: null, cert: null },
              // No profile to reopen on another database.
              oneDbPerConn: false,
              parent: null,
            })
          )
          .catch((e: ErrorOut) => useStore.getState().setConnectError(e));
      } else {
        openConnectDialog();
      }
    })();
  }, []);

  // e2e smoke test (ALAMO_M1_SMOKE=1): same path as the UI, outcomes on
  // stdout, exit. Validates connect→execute→events→registry with no interaction.
  useEffect(() => {
    if (!workspaceReady) return;
    let armed = true;
    (async () => {
      if (!(await invoke<boolean>("m1_smoke"))) return;
      // ONLY the smoke tab is observed: the workspace may hold table tabs
      // that load (and finish) on their own (M3.5).
      let smokeTabId: string | null = null;
      const unsub = useStore.subscribe((s, prev) => {
        if (!armed || smokeTabId === null) return;
        const was = prev.tabs.find((t) => t.id === smokeTabId);
        const now = s.tabs.find((t) => t.id === smokeTabId);
        if (!was?.running || now?.running !== false) return;
        armed = false;
        unsub();
        const tab = now;
        const sts = tab?.statements ?? [];
        const kinds = sts.map((st) => st.outcome?.kind ?? "?");
        const rows = sts.map((st) => st.rows);
        const notices = sts.reduce((a, st) => a + st.notices.length, 0);
        const ok =
          tab?.execState === "completed" &&
          kinds[0] === "row_set" &&
          (kinds[1] === "void" || kinds[1] === "affected") &&
          kinds[2] === "row_set" &&
          rows[2] === 5000 &&
          notices >= 1;
        void invoke("smoke_done", {
          summary: JSON.stringify({ state: tab?.execState, kinds, rows, notices }),
          ok,
        });
      });
      // Wait for the connection, then execute via the same path as the UI.
      const waitConn = () =>
        new Promise<void>((res) => {
          const check = () =>
            useStore.getState().connections.length > 0 ? res() : setTimeout(check, 100);
          check();
        });
      await waitConn();
      // The workspace may have been saved empty (all tabs closed) or with a
      // table tab active: the smoke executes from an SQL tab of its own.
      if (activeTab(useStore.getState())?.kind !== "sql") newSqlTab();
      const connId = useStore.getState().activeConnId!;
      const sql =
        "SELECT 1 AS uno; DO $$ BEGIN RAISE NOTICE 'hello from smoke'; END $$; SELECT * FROM bench_narrow ORDER BY id LIMIT 5000;";
      const execId = await ipc.execute(connId, sql, (ev) =>
        useStore.getState().handleEvent(connId, ev)
      );
      const tabId = useStore.getState().activeTabId;
      // Before startExecution: the replay of the queued events is synchronous
      // and may close the execution in the same tick.
      smokeTabId = tabId;
      useStore.getState().startExecution(tabId, execId);
    })();
    return () => {
      armed = false;
    };
  }, [workspaceReady]);

  // Click on the error → highlight in the editor (§6.1).
  useEffect(() => {
    const onShow = (e: Event) => {
      const { offset, pos } = (e as CustomEvent).detail as {
        offset: number;
        pos: number | null;
      };
      const base = activeTab(useStore.getState())?.execBase ?? 0;
      editorRef.current?.showError(base + offset, pos);
    };
    window.addEventListener("alamo:show-error", onShow);
    return () => window.removeEventListener("alamo:show-error", onShow);
  }, []);

  useEffect(() => {
    const el = gridHost.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      setGridSize({ w: el.clientWidth, h: el.clientHeight });
    });
    ro.observe(el);
    return () => ro.disconnect();
  });

  const sel: StatementUi | undefined = tab?.statements[tab.selected];
  const profile = activeConn?.profile ?? null;

  // The selected per-statement tab stays in view when the strip scrolls
  // (SQL tab switch, click on a tab, selection from "Script").
  useEffect(() => {
    stmtTabs.current
      ?.querySelector(".stmt-tab.active")
      ?.scrollIntoView({ inline: "nearest", block: "nearest" });
  }, [tab?.id, tab?.selected]);

  // SQL tab with its results area (the connection page covers it).
  const sqlTab = tab && tab.kind === "sql" && !page ? tab : null;
  const resultsAvailable =
    !!sqlTab && (sqlTab.statements.length > 0 || sqlTab.running || !!store.connectError);
  const resultsOpen = !!sqlTab && resultsAvailable && !resultsCollapsed[sqlTab.id];
  const mainLayout = !sqlTab
    ? ""
    : !resultsAvailable
      ? " no-results"
      : resultsOpen
        ? " results-open"
        : " results-collapsed";
  const toggleResults = () => {
    if (!sqlTab) return;
    const id = sqlTab.id;
    setResultsCollapsed((c) => ({ ...c, [id]: !c[id] }));
  };
  // Dragging the handle: the height is the distance from the bottom of the area.
  const startResultsDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    const main = studioMain.current;
    if (!resultsOpen || !main) return;
    const bottom = main.getBoundingClientRect().bottom;
    const maxH = Math.max(120, main.clientHeight - 220);
    results.startDrag(e, (ev) => Math.max(80, Math.min(maxH, Math.round(bottom - ev.clientY))));
  };

  return (
    <div
      className={"studio" + (profile?.is_production ? " prod" : "") + (profile ? " tinted" : "")}
      // The frame takes the colour of the profile being worked on (active
      // tab or connection page): with several databases open one knows
      // where one is without reading.
      style={profile ? ({ "--conn": connColor(profile) } as React.CSSProperties) : undefined}
    >
      <div
        className="toolbar"
        // Integrated title bar: the empty areas of the toolbar drag the
        // window, double click maximises it. The controls do not.
        onMouseDown={(e) => {
          if (e.button !== 0) return;
          const el = e.target as HTMLElement;
          if (el !== e.currentTarget && !el.classList.contains("spacer") && !el.closest(".app-brand")) return;
          if (e.detail === 2) void ipc.windowToggleMaximize();
          else void ipc.windowStartDrag();
        }}
      >
        {/* Title bar as in the mock: in the centre the command box (⌘K),
            on the right the transaction state and the connection being
            worked on. Run/Cancel and the database sit in the query bar,
            above the editor. */}
        <span className="app-brand">
          <img src={logoUrl} alt="" />
          Alamo DB
        </span>
        <div className="spacer" />
        <button className="cmd-box" onClick={() => setPaletteOpen(true)}>
          <IconSearch />
          <span>{t("Open table, query or action…")}</span>
          {keyHint("palette.toggle") && <kbd>{keyHint("palette.toggle")}</kbd>}
        </button>
        <div className="spacer" />
        {activeConn?.txn && (
          <TxnBadge txn={activeConn.txn} onOpen={() => setTxnLogFor(activeConn.connId)} />
        )}
        {activeConn && !activeConn.profile.is_production && !activeConn.profile.read_only && (
          <button
            className={"btn small" + (activeConn.manualCommit ? " active" : "")}
            disabled={!!activeConn.txn || anyRunning}
            title={
              activeConn.txn
                ? t("Commit or Rollback the open transaction first")
                : activeConn.manualCommit
                  ? t("Manual commit: every change waits for Commit. Click for auto-commit")
                  : t("Auto-commit: every statement is applied right away. Click for manual commit")
            }
            onClick={() => void toggleManualCommit(activeConn.connId, !activeConn.manualCommit)}
          >
            {activeConn.manualCommit ? t("Manual commit") : t("Auto-commit")}
          </button>
        )}
        {activeConn && (activeConn.profile.is_production || activeConn.manualCommit || activeConn.txn) && (
          <>
            <button
              className="btn small"
              disabled={!activeConn.txn?.hasWrites || (anyRunning && !stickyPaused)}
              title={
                activeConn.txn?.hasWrites
                  ? activeConn.profile.is_production
                    ? t("Lists the writes of the open transaction, then COMMIT")
                    : t("COMMIT the open transaction")
                  : t("Enabled only when the transaction has writes (§9.7)")
              }
              onClick={() =>
                activeConn.profile.is_production
                  ? setTxnLogFor(activeConn.connId)
                  : void txnAction("commit")
              }
            >
              Commit{activeConn.txn?.writes ? ` (${activeConn.txn.writes.toLocaleString(locale())})` : ""}
            </button>
            <button
              className="btn small"
              disabled={!activeConn.txn || (anyRunning && !stickyPaused)}
              title={t("ROLLBACK the open transaction")}
              onClick={() => void txnAction("rollback")}
            >
              Rollback
            </button>
          </>
        )}
        {profile ? (
          <button
            className={"profile-pill" + (profile.is_production ? " prod" : "")}
            title={t("Databases and tables of the connection")}
            onClick={() => activeConn && store.openConnPage(activeConn.connId)}
          >
            <span className="swatch" style={{ background: connColor(profile) }} />
            {profile.name}
            {profile.is_production && <ProdTag />}
            {profile.read_only && <span className="ro-tag">{t("read-only")}</span>}
          </button>
        ) : (
          <>
            <span className={"conn-dot " + (connected ? "on" : "off")} />
            <span className="conn-label">
              {connected
                ? t("connected")
                : tab && tab.profileId !== null
                  ? t("tab disconnected — reconnect the profile")
                  : t("not connected")}
            </span>
            <button className="btn small" onClick={() => openConnectDialog()}>
              {t("Connections…")}
            </button>
          </>
        )}
      </div>

      {txnError && (
        <div className="banner warn">
          {t("Commit/Rollback failed:")} {tMsg(txnError.message)}
          <button className="btn small" onClick={() => setTxnError(null)}>
            <IconX />
          </button>
        </div>
      )}

      {reconnecting && (
        <div className="banner neutral" role="status">
          <span className="spinner" aria-hidden />
          <span>
            {t("Connecting to {name}…", {
              name: connById(store, reconnecting.connId)?.profile.name ?? "",
            })}{" "}
            <Elapsed startedAt={reconnecting.startedAt} />
          </span>
        </div>
      )}

      {connNote && (
        <div className={"banner " + (connNote.warn ? "warn" : "ok")} role="status">
          <span>{connNote.text}</span>
          {connNote.retry !== undefined && (
            <button
              className="btn small primary"
              onClick={() => {
                const id = connNote.retry!;
                setConnNote(null);
                void ensureLive(id, 0, true);
              }}
            >
              {t("Reconnect")}
            </button>
          )}
          <button className="btn small" onClick={() => setConnNote(null)}>
            <IconX />
          </button>
        </div>
      )}

      {execError && (
        <div className="banner warn">
          <span>
            {t("Execution rejected:")} {tMsg(execError.message)}
            {execError.detail && <span className="banner-note">{execError.detail}</span>}
            {execError.hint && (
              <span className="banner-note">{t("Hint:")} {tMsg(execError.hint)}</span>
            )}
          </span>
          <button className="btn small" onClick={() => setExecError(null)}>
            <IconX />
          </button>
        </div>
      )}

      <div className="studio-body">
        <div className="activity-bar">
          <button
            className={"activity-btn" + (activePanel === "schema" ? " active" : "")}
            title={withKey(t("Databases and tables"), "sidebar.toggle")}
            aria-pressed={activePanel === "schema"}
            onClick={() => togglePanel("schema")}
          >
            <IconDatabase />
          </button>
          <button
            className={"activity-btn" + (activePanel === "connections" ? " active" : "")}
            title={t("Connections and profiles")}
            aria-pressed={activePanel === "connections"}
            onClick={() => togglePanel("connections")}
          >
            <IconPlug />
          </button>
          {sidePanels().map((p) => (
            <button
              key={p.id}
              className={"activity-btn" + (activePanel === p.id ? " active" : "")}
              title={p.title()}
              aria-pressed={activePanel === p.id}
              onClick={() => togglePanel(p.id)}
            >
              <p.icon />
            </button>
          ))}
          {/* Open connections, always visible: with several databases at once
              one sees at a glance what is being worked on. */}
          {store.connections.length > 0 && <span className="activity-sep" aria-hidden />}
          {profileConns(store.connections).map((c) => {
            // The tabs on the PG server's other databases count too (§3.3).
            const nTabs = store.tabs.filter((tb) => rootConnId(store.connections, tb.connId) === c.connId).length;
            return (
              <button
                key={c.connId}
                className={
                  "conn-av" +
                  (c.connId === rootConnId(store.connections, activeConnId) ? " current" : "") +
                  (c.profile.is_production ? " prod" : "")
                }
                style={{ "--conn": connColor(c.profile) } as React.CSSProperties}
                title={
                  c.profile.name +
                  (c.profile.is_production ? " · PROD" : "") +
                  (c.currentDb ? " · " + c.currentDb : "") +
                  " · " +
                  t("{n} tabs", { n: nTabs })
                }
                onClick={() => store.openConnPage(c.connId)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setAvArmed(false);
                  setAvMenu({ connId: c.connId, x: e.clientX, y: e.clientY });
                }}
              >
                {connInitials(c.profile.name)}
                {c.txn?.hasWrites && <span className="conn-av-dirty" />}
              </button>
            );
          })}
          {store.connections.length > 0 && (
            <button
              className="conn-av add"
              title={t("Open another connection…")}
              onClick={() => openConnectDialog()}
            >
              <IconPlus />
            </button>
          )}
          <button
            className="activity-btn activity-settings"
            title={t("Settings")}
            onClick={() => setSettingsOpen(true)}
          >
            <IconSettings />
          </button>
        </div>
        {/* All panels stay mounted: the hidden one is CSS only (expansion
            state, cache and filters survive the switch). */}
        <div className={panelClass} hidden={activePanel !== "schema"} style={panelStyle} onClick={onTreeClick}>
          <SchemaTree />
          {panelFoot}
          {panelResize}
        </div>
        <div className={panelClass} hidden={activePanel !== "connections"} style={panelStyle}>
          <ConnectionsPanel onOpenDialog={openConnectDialog} onTransfer={setTransfer} />
          {panelFoot}
          {panelResize}
        </div>
        {sidePanels().map((p) => (
          <div key={p.id} className={panelClass} hidden={activePanel !== p.id} style={panelStyle}>
            <p.component done={panelDone} />
            {panelFoot}
            {panelResize}
          </div>
        ))}
        {/* Without a visible editor (table tab or no tab) the middle row of
            the grid disappears: the content takes the whole height — otherwise
            the table tab would live in the 38% reserved for the editor. */}
        <div
          ref={studioMain}
          className={"studio-main" + (!tab || tab.kind === "table" ? " no-editor" : "") + mainLayout}
          // Height chosen by the user; min(): never beyond the area's space,
          // the editor keeps at least the template's 120px.
          style={
            resultsOpen && resultsH
              ? { gridTemplateRows: `auto auto minmax(120px, 1fr) auto min(${resultsH}px, 100% - 220px)` }
              : undefined
          }
        >
          <TabBar />
          {/* The editor is a singleton (§9.2): with a table tab active or no
              tab it is hidden, never unmount the view. */}
          {/* Query bar (SQL tabs only): what it runs on and the commands. */}
          <div className="query-bar" style={!tab || tab.kind === "table" ? { display: "none" } : undefined}>
            <span
              className="ctx-chip"
              style={{ "--conn": connColor(profile) } as React.CSSProperties}
              title={t("Connection and database the query runs on")}
            >
              <span className="tab-group-dot" />
              <b>{profile?.name ?? t("disconnected")}</b>
              <span className="ctx-sep">›</span>
              {dbSwitchable ? (
                <DbPicker
                  value={currentDb}
                  disabled={!tab || tab.kind !== "sql"}
                  options={dbOptions}
                  onPick={switchDb}
                />
              ) : (
                activeConn && (
                  <span
                    className={"db-badge" + (currentDb === null ? " none" : "")}
                    title={t("Connection database: unqualified statements read from here.")}
                  >
                    {currentDb ?? t("no database")}
                  </span>
                )
              )}
            </span>
            {/* Split button: on the left what ⌘↩ does (selection, query under
                the cursor, or everything if there is a single query); the
                arrow opens "Run all". */}
            <span className="run-group">
              <button
                className="btn primary"
                onClick={() => void run()}
                disabled={
                  !tab || tab.kind === "table" || tab.connId === null || (anyRunning && !stickyPaused) || tabConnecting
                }
                title={
                  stickyPaused
                    ? t("Stops the paused query and runs")
                    : anyRunning && stickyTab?.id !== tab?.id
                    ? t("execution in progress in \"{title}\"", { title: stickyTab?.title ?? "" })
                    : tab && tab.connId === null
                      ? t("tab disconnected: reconnect its profile")
                      : runCtx.hasSelection && runMode !== "all"
                        ? t("Runs only the selected text")
                        : runCtx.statements > 1 && runMode === "current"
                          ? t("Runs the query at the cursor (highlighted)")
                          : undefined
                }
              >
                {tab?.running && !stickyPaused ? (
                  <>
                    <span className="spinner" aria-hidden /> {t("Running…")}
                  </>
                ) : tabConnecting ? (
                  <>
                    <span className="spinner" aria-hidden /> {t("Connecting…")}
                  </>
                ) : (
                  <>
                    <IconPlay />{" "}
                    {runCtx.hasSelection && runMode !== "all"
                      ? t("Run selection")
                      : runCtx.statements > 1
                        ? runMode === "current"
                          ? t("Run query")
                          : t("Run all")
                        : t("Run")}{" "}
                    {keyHint("run") && <kbd>{keyHint("run")}</kbd>}
                  </>
                )}
              </button>
              <button
                className="btn primary run-more"
                disabled={
                  !tab || tab.kind === "table" || tab.connId === null || (anyRunning && !stickyPaused) || tabConnecting
                }
                title={t("Other ways to run")}
                onMouseDown={(e) => e.stopPropagation()}
                onClick={() => setRunMenu((v) => !v)}
              >
                <IconChevronDown />
              </button>
              {runMenu && (
                <div className="run-menu" onMouseDown={(e) => e.stopPropagation()}>
                  <button
                    className="tab-menu-item"
                    onClick={() => {
                      setRunMenu(false);
                      void run();
                    }}
                  >
                    {runCtx.hasSelection && runMode !== "all"
                      ? t("Run selection")
                      : runMode === "current"
                        ? t("Run query at cursor")
                        : t("Run all")}{" "}
                    {keyHint("run") && <kbd>{keyHint("run")}</kbd>}
                  </button>
                  <button
                    className="tab-menu-item"
                    onClick={() => {
                      setRunMenu(false);
                      void run("all");
                    }}
                  >
                    {t("Run all")} {keyHint("run.all") && <kbd>{keyHint("run.all")}</kbd>}
                  </button>
                </div>
              )}
            </span>
            <button
              className={"btn" + (tabWorking ? " cancel-live" : "")}
              onClick={cancelRun}
              disabled={!anyRunning}
            >
              <IconStop /> {t("Cancel")}
            </button>
          </div>
          {/* Thread running under the bars while the active tab works (query
              or table load): visible at a glance too. */}
          {tabWorking && !page && <div className="run-progress" role="progressbar" />}
          <div
            className="editor-pane"
            ref={editorHost}
            style={!tab || tab.kind === "table" ? { display: "none" } : undefined}
          />
          {page ? (
            <ConnectionPage
              conn={page}
              onEditProfile={openConnectDialog}
              actions={
                <>
                <button
                  className={"btn small" + (recArmed === page.connId ? " danger" : "")}
                  disabled={anyRunning || forcing(page.connId)}
                  title={t("Reopens the SSH tunnel and server connections right away, without waiting for the drop to be noticed")}
                  onClick={() => void reconnectConn(page.connId, !!page.txn?.hasWrites)}
                >
                  {recArmed === page.connId ? t("Uncommitted writes — confirm?") : t("Reconnect")}
                </button>
                <button
                  className={"btn small" + (discArmed ? " danger" : "")}
                  disabled={anyRunning}
                  title={
                    pageWrites
                      ? t("The open transaction has uncommitted writes: disconnecting = ROLLBACK (§9.7.2)")
                      : t("Closes the connection \"{name}\"; its tabs remain, detached", {
                          name: page.profile.name,
                        })
                  }
                  onClick={() => {
                    const connId = pageRoot?.connId ?? page.connId;
                    // §9.7.2: disconnecting with pending writes = lost work. It
                    // must be said, not discovered: first click arms, second executes.
                    if (pageWrites && !discArmed) {
                      setDiscArmed(true);
                      window.setTimeout(() => setDiscArmed(false), 4000);
                      return;
                    }
                    setDiscArmed(false);
                    void closeConn(connId);
                  }}
                >
                  {discArmed ? t("Uncommitted writes — confirm?") : t("Disconnect")}
                </button>
                </>
              }
            />
          ) : tab?.kind === "table" ? (
            <TableTab key={tab.id} tab={tab} gridHost={gridHost} gridSize={gridSize} />
          ) : sqlTab && !resultsOpen ? (
            resultsAvailable && (
              <ResultsHandle open={false} summary={sel ? stmtOutcome(sel) : ""} onToggle={toggleResults} />
            )
          ) : (
            <>
              {sqlTab && (
                <ResultsHandle
                  open
                  summary=""
                  onToggle={toggleResults}
                  onPointerDown={startResultsDrag}
                  onDoubleClick={results.reset}
                />
              )}
              <div className="results-pane">
                {escalate && anyRunning && (
                  <div className="banner warn">
                    {t("The server has not responded to the cancel request for 10 seconds.")}
                    <button className="btn danger" onClick={() => void terminate()}>
                      {t("Terminate connection")}
                    </button>
                    <span className="banner-note">
                      {t(
                        "destructive action: closes the session on the server, including open transactions and session state"
                      )}
                    </span>
                  </div>
                )}
                {store.connectError && <ErrorPanel error={store.connectError} />}
                {tab && stmtTabsVisible(tab.statements) && (
                  <div className="stmt-bar">
                  <div
                    className="stmt-tabs"
                    ref={stmtTabs}
                    onWheel={(e) => {
                      // Hidden scrollbar: the vertical wheel scrolls the strip.
                      if (e.deltaY !== 0 && e.deltaX === 0) e.currentTarget.scrollLeft += e.deltaY;
                    }}
                  >
                    {tab.statements.some((st) => !ownsTab(st)) && (
                      <button
                        className={"stmt-tab" + (sel && !ownsTab(sel) ? " active" : "")}
                        onClick={() => store.select(tab.id, tab.statements.findIndex((st) => !ownsTab(st)))}
                        title={t(
                          "Outcome of every statement in the script. The tabs next to it are only the statements with a result set or an error."
                        )}
                      >
                        <span className="stmt-tab-sql">{t("Script ({n})", { n: tab.statements.length })}</span>
                        <span className="stmt-tab-badge">{scriptBadge(tab.statements)}</span>
                      </button>
                    )}
                    {tab.statements.map((st, i) =>
                      ownsTab(st) ? (
                        <button
                          key={i}
                          className={
                            "stmt-tab" +
                            (i === tab.selected ? " active" : "") +
                            badgeClass(st) +
                            (tab.edit !== null && tab.edit.resultId === st.resultId ? " pending" : "")
                          }
                          onClick={() => store.select(tab.id, i)}
                          title={stmtSnippet(tab.execSql, tab.statements, i, 200)}
                        >
                          <span className="stmt-tab-n">{i + 1}</span>
                          <span className="stmt-tab-sql">{stmtSnippet(tab.execSql, tab.statements, i, 36)}</span>
                          <span className="stmt-tab-badge">{stmtBadge(st)}</span>
                        </button>
                      ) : null
                    )}
                  </div>
                  <ScriptExportButton tab={tab} />
                  </div>
                )}
                {tab?.running && (!sel || sel.resultId === null) && (
                  <RunningNotice label={t("Query running…")} startedAt={tab.startedAt} onCancel={cancelRun} />
                )}
                {tab &&
                  sel &&
                  (ownsTab(sel) || tab.statements.length === 1 ? (
                    <SqlResultPane tab={tab} st={sel} gridHost={gridHost} gridSize={gridSize} />
                  ) : (
                    <ScriptPane tab={tab} onSelect={(i) => store.select(tab.id, i)} />
                  ))}
                {!tab && !store.connectError && (
                  <div className="hint">
                    {keyHint("tab.new_query")
                      ? t("No tabs open. Create one with {key} or the + in the bar.", { key: keyHint("tab.new_query") })
                      : t("No tabs open. Create one with the + in the bar.")}
                  </div>
                )}
                {tab && !sel && !store.connectError && (
                  <div className="hint">
                    {keyHint("run") ? t("Write SQL and press {key}. Results appear here.", { key: keyHint("run") }) : t("Write SQL and press Run. Results appear here.")}
                  </div>
                )}
              </div>
            </>
          )}
        </div>
      </div>

      <div className="statusbar">
        <span className={tabWorking || tabConnecting ? "statusbar-running" : undefined}>
          {(tabWorking || tabConnecting) && <span className="spinner" aria-hidden />}
          {tabConnecting && !tabWorking
            ? t("connecting…")
            : !tab || tab.execState === "idle"
            ? t("ready")
            : tabPaused
              ? tabPaused.budgetByRows
                ? t("paused — scroll for more rows")
                : t("paused")
              : stateLabel(tab.execState, tab.running)}
          {tabWorking && <Elapsed startedAt={tab.startedAt} prefix=" · " />}
          {tabConnecting && !tabWorking && <Elapsed startedAt={reconnecting.startedAt} prefix=" · " />}
        </span>
        {sel && sel.resultId !== null && (
          <span>
            {t("{n} rows", { n: sel.rows.toLocaleString(locale()) })}
            {sel.streaming || (tab && tableMayHaveMore(tab)) ? "+" : ""} · {(sel.layoutBytes / 1048576).toFixed(1)} MB
          </span>
        )}
        {sel && sel.elapsedMs > 0 && <span>{(sel.elapsedMs / 1000).toFixed(2)} s</span>}
        {store.connections.length > 0 && (
          <span className="statusbar-conns">
            {t("{n} open connections", { n: profileConns(store.connections).length })}:{" "}
            {profileConns(store.connections)
              .map((c) => c.profile.name)
              .join(", ")}
          </span>
        )}
      </div>

      {avMenu &&
        (() => {
          const c = store.connections.find((x) => x.connId === avMenu.connId);
          if (!c) return null;
          const running = store.tabs.some((tb) => tb.running && rootConnId(store.connections, tb.connId) === c.connId);
          const writes = familyWrites(store.connections, c.connId);
          return (
            <div
              className="tab-menu"
              style={{ left: avMenu.x, top: avMenu.y }}
              onMouseDown={(e) => e.stopPropagation()}
            >
              <div className="tab-menu-label">{c.profile.name}</div>
              <button
                className="tab-menu-item"
                disabled={running || forcing(c.connId)}
                title={
                  running
                    ? t("Query running on this connection: cancel it first")
                    : t("Reopens the SSH tunnel and server connections right away, without waiting for the drop to be noticed")
                }
                onClick={() => {
                  if (reconnectConn(c.connId, !!c.txn?.hasWrites)) setAvMenu(null);
                }}
              >
                {recArmed === c.connId ? t("Uncommitted writes — confirm?") : t("Reconnect")}
              </button>
              <button
                className="tab-menu-item"
                disabled={running}
                title={running ? t("Query running on this connection: cancel it first") : undefined}
                onClick={() => {
                  // §9.7.2: pending writes = lost work, the first click arms.
                  if (writes && !avArmed) {
                    setAvArmed(true);
                    return;
                  }
                  void closeConn(c.connId).then((done) => done && setAvMenu(null));
                }}
              >
                {avArmed ? t("Uncommitted writes — confirm?") : t("Disconnect")}
              </button>
            </div>
          );
        })()}

      <UnsavedDialog />

      {quitBlocked && (
        <QuitDialog
          pending={store.connections.filter((c) => c.txn?.hasWrites)}
          onQuit={() => void ipc.appQuit()}
          onClose={() => setQuitBlocked(false)}
        />
      )}

      {txnLogFor !== null && (
        <TxnLogDialog
          connId={txnLogFor}
          profile={connById(store, txnLogFor)?.profile ?? null}
          busy={anyRunning && !stickyPaused}
          onCommit={() => {
            setTxnLogFor(null);
            void txnAction("commit");
          }}
          onRollback={() => {
            setTxnLogFor(null);
            void txnAction("rollback");
          }}
          onClose={() => setTxnLogFor(null)}
        />
      )}

      {writeGate && (
        <WriteGateDialog
          gate={writeGate}
          profile={connById(store, writeGate.connId)?.profile ?? null}
          onRun={() => {
            const g = writeGate;
            setWriteGate(null);
            void doExecute(g.connId, g.tabId, g.sql, g.base);
          }}
          onClose={() => setWriteGate(null)}
        />
      )}

      {paletteOpen && (
        <CommandPalette
          onClose={() => setPaletteOpen(false)}
          onOpenDialog={openConnectDialog}
          onPanel={(id) => {
            lastPanel.current = id;
            localStorage.setItem("alamo.panel", id);
            setActivePanel(id);
          }}
          onSettings={() => setSettingsOpen(true)}
          onTransfer={setTransfer}
          onSshProfiles={() => setSshManager({ id: null })}
        />
      )}


      <ConnectDialog
        open={dialogOpen}
        connected={connected}
        initialProfileId={dialogProfileId}
        onTransfer={setTransfer}
        onManageSsh={(id) => setSshManager({ id })}
        onSettings={() => setSettingsOpen(true)}
        sshPick={sshPick}
        covered={transfer !== null || sshManager !== null}
        profilesVersion={profilesVersion}
        onClose={() => {
          setDialogOpen(false);
          // Profiles may have changed in the dialog: the connections panel
          // re-reads them.
          void queryClient.invalidateQueries({ queryKey: ["profiles"] });
        }}
        onConnected={(out) => {
          setEffective(out.conn_id, out.execution);
          // M3.5: the connection is ADDED, the others stay open. The
          // introspection queries are keyed per connId: no clear.
          useStore.getState().addConnection({
            connId: out.conn_id,
            profile: out.profile,
            currentDb: null,
            txn: null,
            security: out.security,
            oneDbPerConn: out.one_db_per_conn,
            parent: null,
          });
          // Just connected, one lands on the connection page: the database
          // list is the natural starting point, or the database that was
          // being looked at last time.
          const opened = out.profile.id !== null ? savedOpenedDb(out.profile.id) : null;
          if (opened !== null) openedDbByConn.set(out.conn_id, opened);
          useStore.getState().openConnPage(out.conn_id);
          // §16: per-connection overrides this build could not apply, once.
          if (out.warnings && out.warnings.length > 0) {
            setConnNote({
              warn: true,
              text:
                t("Some overrides of this connection could not be used:") +
                " " +
                out.warnings.map((w) => `${w.key} — ${tMsg(w.message)}`).join("; "),
            });
          }
          // The current database is needed right away: the tabs show it even
          // when the connection is not the active one.
          // On MySQL one restarts from the last database used with this
          // profile (read NOW: the updates below overwrite it).
          const last = canSwitchDb(out.profile.dialect) && out.profile.id !== null ? lastDb(out.profile.id) : null;
          ipc
            .currentDatabase(out.conn_id)
            .then(async (db) => {
              if (last !== null && last !== db) {
                try {
                  await ipc.useDatabase(out.conn_id, last);
                  db = last;
                } catch {
                  // Database gone or no longer accessible: the profile's
                  // remains.
                }
              }
              useStore.getState().setCurrentDb(out.conn_id, db);
            })
            .catch(() => {});
          setDialogOpen(false);
          void queryClient.invalidateQueries({ queryKey: ["profiles"] });
        }}
      />
      {/* After the connections dialog: opened from it, it must sit above it. */}
      {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}

      {/* After ConnectDialog: it opens from there too and must sit above it. */}
      {transfer === "export" && <ExportConnectionsDialog onClose={() => setTransfer(null)} />}
      {transfer === "import" && (
        <ImportConnectionsDialog
          onClose={() => setTransfer(null)}
          onImported={() => {
            void queryClient.invalidateQueries({ queryKey: ["profiles"] });
            setProfilesVersion((v) => v + 1);
          }}
        />
      )}

      {sshManager && (
        <SshProfilesDialog
          initialId={sshManager.id}
          onClose={(picked) => {
            setSshManager(null);
            setSshPick((p) => ({ id: picked, v: (p?.v ?? 0) + 1 }));
            // Profiles may have been unlinked from a deleted bastion: the
            // dialog and the panel re-read them.
            setProfilesVersion((v) => v + 1);
            void queryClient.invalidateQueries({ queryKey: ["profiles"] });
          }}
        />
      )}

      {/* Last: at startup it opens together with Connections and must sit on top. */}
      <UpdateDialog />
    </div>
  );
}

/// App closing with uncommitted writes (§9.7.2): the backend stopped the
/// red button/Cmd+Q, here the explicit choice. Quit anyway = the connections
/// drop with the process and the server performs the ROLLBACK.
function QuitDialog({
  pending,
  onQuit,
  onClose,
}: {
  pending: ConnUi[];
  onQuit: () => void;
  onClose: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Only Esc = cancel: no Enter, no default on the destructive one.
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);
  return (
    <div className="dialog-overlay">
      <div className="dialog update-dialog small-dialog">
        <div className="dialog-title">
          <span>{t("Uncommitted writes")}</span>
          <button className="btn small dialog-close" onClick={onClose}>
            <IconX />
          </button>
        </div>
        <div className="dialog-body">
          <div>
            {t("Closing the app now performs a")} <span className="mono">ROLLBACK</span>{" "}
            {t("of the open transactions on:")}
          </div>
          <ul className="quit-pending">
            {pending.map((c) => (
              <li key={c.connId}>
                <span className="swatch" style={{ background: connColor(c.profile) }} />
                <span>{c.profile.name}</span>
                {c.profile.is_production && <ProdTag />}
              </li>
            ))}
          </ul>
          <div className="form-actions">
            <div className="spacer" />
            <button className="btn" onClick={onClose}>
              {t("Back to work")}
            </button>
            <button className="btn danger" onClick={onQuit}>
              {t("Quit anyway (ROLLBACK)")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

/// Start of the i-th statement, for its tab label: from its offset (in
/// code points, as the backend gives it) to the next one's, without leading
/// comments and with whitespace collapsed.
function stmtSnippet(sql: string, sts: StatementUi[], i: number, max: number): string {
  const cps = Array.from(sql);
  const from = sts[i]?.charOffset ?? null;
  if (from === null) return "";
  const next = sts[i + 1]?.charOffset ?? null;
  let text = cps.slice(from, next ?? cps.length).join("");
  text = text.replace(/^(\s|--[^\n]*\n|\/\*[\s\S]*?\*\/)+/, "").replace(/\s+/g, " ").trim();
  if (text.endsWith(";")) text = text.slice(0, -1).trim();
  return text.length > max ? text.slice(0, max - 1) + "…" : text;
}

/// A statement has a tab of its own only if there is something to look at:
/// a result set (even incoming) or an error. SET, INSERT, UPDATE, DDL and
/// the statements not executed all end up in the "Script" tab (like
/// DBeaver's "Statistics"): a script with twenty SETs must not open twenty
/// empty tabs.
function ownsTab(st: StatementUi): boolean {
  if (st.resultId !== null) return true;
  const k = st.outcome?.kind;
  return k === "failed" || k === "cancelled";
}

/// The tab bar is only useful if there are at least two tabs to choose
/// from: "Script" counts one, plus one for every statement with its own tab.
function stmtTabsVisible(sts: StatementUi[]): boolean {
  const own = sts.filter(ownsTab).length;
  return own + (own < sts.length ? 1 : 0) > 1;
}

/// Badge of the "Script" tab: "…" while one of the grouped statements runs,
/// then the total of affected rows (same ✎ as the single tabs).
function scriptBadge(sts: StatementUi[]): string {
  const grouped = sts.filter((st) => !ownsTab(st));
  if (grouped.some((st) => st.running)) return "…";
  const affected = grouped.reduce((a, st) => a + (st.outcome?.kind === "affected" ? st.outcome.rows : 0), 0);
  return affected > 0 ? `${affected.toLocaleString(locale())}✎` : "";
}

/// Outcome of a statement in words, for the table of the "Script" tab.
function stmtOutcome(st: StatementUi): string {
  if (st.running) return t("running…");
  switch (st.outcome?.kind) {
    case "row_set":
      return t("{n} rows", { n: st.rows.toLocaleString(locale()) });
    case "affected":
      return t("{n} rows affected", { n: st.outcome.rows.toLocaleString(locale()) });
    case "void":
      return t("Done");
    case "failed":
      return t("Error");
    case "cancelled":
      return t("Cancelled");
    case "skipped":
      return t("Not executed");
    default:
      return t("pending");
  }
}

function fmtElapsed(ms: number): string {
  if (ms <= 0) return "";
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(2)} s`;
}

/// "Script" tab: one row per statement with outcome and time (§6.4: every
/// statement has its own outcome, never an aggregate one). The rows of
/// statements with their own tab are clickable and lead to that tab.
function ScriptPane({ tab, onSelect }: { tab: TabUi; onSelect: (i: number) => void }) {
  return (
    <div className="structure-pane">
      <table className="structure-table script-table">
        <thead>
          <tr>
            <th>#</th>
            <th>Statement</th>
            <th>{t("Outcome")}</th>
            <th>{t("Time")}</th>
          </tr>
        </thead>
        <tbody>
          {tab.statements.map((st, i) => {
            const own = ownsTab(st);
            return (
              <tr
                key={i}
                className={(own ? "clickable" : "") + badgeClass(st)}
                onClick={own ? () => onSelect(i) : undefined}
                title={own ? t("Open this statement's tab") : stmtSnippet(tab.execSql, tab.statements, i, 400)}
              >
                <td className="num">{i + 1}</td>
                <td className="mono">{stmtSnippet(tab.execSql, tab.statements, i, 90)}</td>
                <td className={st.outcome || st.running ? "" : "muted"}>{stmtOutcome(st)}</td>
                <td className="num">{fmtElapsed(st.elapsedMs)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

/// "Export all" of a script with at least two result sets: every result in
/// one file or in a folder, in the script's order (§6.4: one result per
/// statement, never merged). Off while the script runs, the results are
/// still arriving.
function ScriptExportButton({ tab }: { tab: TabUi }) {
  const [open, setOpen] = useState(false);
  const results = tab.statements.filter((st) => st.resultId !== null);
  if (results.length < 2 || tab.connId === null) return null;
  const connId = tab.connId;
  const rows = results.reduce((a, st) => a + st.rows, 0);
  const partial = results.some(resultIncomplete);
  const items: ScriptItem[] = results.map((st) => ({
    result_id: st.resultId as number,
    position: st.index + 1,
    sql: stmtSnippet(tab.execSql, tab.statements, st.index, Infinity),
    table_name: singleSourceTable(st.columns) ?? "result",
  }));
  const counts = t("{n} result sets, {rows} rows", {
    n: results.length,
    rows: rows.toLocaleString(locale()),
  });
  return (
    <>
      <button className="btn small stmt-export" disabled={tab.running} onClick={() => setOpen(true)}>
        <IconExport />
        {t("Export all ({n})", { n: results.length })}
      </button>
      {open && (
        <ExportDialog
          title={t("Export all results")}
          subject={tab.title}
          detail={partial ? `${counts} ${t("(some partial: only the fetched rows)")}` : counts}
          tableName={null}
          script={true}
          onExport={(opts, target) => ipc.exportScript(connId, items, opts, target, exportBaseName(tab.title))}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  );
}

/// "Query 1 2026-10-28 1530": the tab and the moment, so two exports of
/// the same script do not end up with the same name.
function exportBaseName(title: string): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${title} ${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}${p(d.getMinutes())}`;
}

/// Handle between editor and results: draggable (open) and with the button
/// that folds/reopens the pane. Folded, a single line remains with the
/// outcome of the selected statement, so one knows what is underneath
/// without opening it.
function ResultsHandle({
  open,
  summary,
  onToggle,
  onPointerDown,
  onDoubleClick,
}: {
  open: boolean;
  summary: string;
  onToggle: () => void;
  onPointerDown?: (e: React.PointerEvent<HTMLDivElement>) => void;
  onDoubleClick?: () => void;
}) {
  return (
    <div
      className={"results-handle" + (open ? " open" : "")}
      onPointerDown={onPointerDown}
      onDoubleClick={onDoubleClick}
      title={open ? t("Drag to resize, double-click for the default height") : undefined}
    >
      <button
        className="results-toggle"
        onPointerDown={(e) => e.stopPropagation()}
        onDoubleClick={(e) => e.stopPropagation()}
        onClick={onToggle}
        title={open ? t("Hide results") : t("Show results")}
      >
        {open ? <IconChevronDown /> : <IconChevronUp />}
        <span>{t("Results")}</span>
        {summary && <span className="results-summary">· {summary}</span>}
      </button>
      {open && <span className="results-grip" aria-hidden />}
    </div>
  );
}

function stmtBadge(st: StatementUi): string {
  if (st.running) return "…";
  switch (st.outcome?.kind) {
    case "row_set":
      return `${st.rows.toLocaleString(locale())}`;
    case "affected":
      return `${st.outcome.rows}✎`;
    case "failed":
      return "✕";
    case "cancelled":
      return "⊘";
    case "skipped":
      return "—";
    default:
      return "";
  }
}

function badgeClass(st: StatementUi): string {
  if (st.outcome?.kind === "failed") return " failed";
  if (st.outcome?.kind === "cancelled") return " cancelled";
  return "";
}

function stateLabel(state: string, running: boolean): string {
  if (running) return t("running…");
  switch (state) {
    case "completed":
      return t("completed");
    case "failed":
      return t("failed");
    case "cancelled":
      return t("cancelled by the user");
    default:
      return state;
  }
}

/// §9.7: execution awaiting confirmation on a prod profile — first write of
/// the transaction (decision 8) or irreversible operations.
type WriteGate = {
  connId: number;
  tabId: string;
  sql: string;
  /// Offset of `sql` in the editor (see TabUi.execBase).
  base: number;
  firstWrite: boolean;
  /// Snippets of the statements that cannot be undone, to list them plainly.
  irreversible: string[];
  /// Prod with "Confirm schema changes" on (§16): the transactional DDL,
  /// listed apart from the irreversible ones because a Rollback undoes it.
  ddl: string[];
  /// Snippets of the statements opaque to the classifier on read-only prod:
  /// the server net should stop them if they write, but on prod the
  /// confirmation remains (§9.7.3).
  unknown: string[];
  /// Non-prod profiles: UPDATE/DELETE without WHERE (on prod they are
  /// listed among the irreversible ones).
  unfiltered: string[];
};

/// Text of statement `c` inside the batch, compacted for the dialog's list.
/// Offsets are in characters (§6.1): cut on code points.
function statementSnippet(sql: string, classes: StatementClassOut[], c: StatementClassOut): string {
  const chars = Array.from(sql);
  const next = classes.find((x) => x.index === c.index + 1);
  const end = next ? next.char_offset : chars.length;
  const text = chars
    .slice(c.char_offset, end)
    .join("")
    .trim()
    .replace(/\s+/g, " ");
  return text.length > 90 ? text.slice(0, 90) + "…" : text;
}

/// Open transaction badge with timer (§9.7.2): progressive warning — on
/// prod even a SELECT opens the transaction and keeping it open blocks the
/// vacuum / holds locks.
function TxnBadge({ txn, onOpen }: { txn: TxnUi; onOpen: () => void }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);
  const min = Math.floor((txn.openForMs + (now - txn.fetchedAt)) / 60_000);
  // The last 2 minutes before the automatic ROLLBACK are counted down.
  const left = txn.idleRollbackMs - (txn.idleForMs + (now - txn.fetchedAt));
  const closing = left < 120_000;
  const level = closing || min >= 10 ? " danger" : min >= 2 ? " warn" : "";
  return (
    <button
      className={"txn-badge" + level}
      onClick={onOpen}
      title={t(
        "Open transaction on the editor connection (§9.7): it closes with Commit or Rollback, or by itself with a ROLLBACK after {min} minutes without statements. Left open for long, it blocks vacuum and holds locks on the server. Click: the writes waiting for Commit.",
        { min: Math.round(txn.idleRollbackMs / 60000) }
      )}
    >
      {closing
        ? t("automatic rollback in {s} s", { s: Math.max(0, Math.ceil(left / 1000)) })
        : min < 1
          ? t("transaction open")
          : t("transaction open for {min} min", { min })}
      {txn.hasWrites ? " · " + t("writes") : ""}
    </button>
  );
}

/// Pre-execution confirmation on prod (§9.7): profile, reason for the gate,
/// irreversible operations in plain sight, full SQL and — if available — the
/// row estimate from EXPLAIN (free, never a COUNT). Enter runs, Esc
/// cancels.
function WriteGateDialog({
  gate,
  profile,
  onRun,
  onClose,
}: {
  gate: WriteGate;
  profile: Profile | null;
  onRun: () => void;
  onClose: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Enter" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault();
        onRun();
      } else if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onRun, onClose]);
  return (
    <div className="dialog-overlay">
      <div className="dialog update-dialog">
        <div className="dialog-title">
          <span>
            {gate.unknown.length > 0 ? t("Confirm execution") : t("Confirm write")}
            {profile?.is_production && <> <ProdTag long /></>}
          </span>
          <button className="btn small dialog-close" onClick={onClose}>
            <IconX />
          </button>
        </div>
        <div className="dialog-body">
          {profile && (
            <div className="update-target">
              <span className="swatch" style={{ background: connColor(profile) }} />{" "}
              <span className="mono">{profile.name}</span>
            </div>
          )}
          {gate.firstWrite && (
            <div className="update-change">
              {t(
                "First write of this transaction: after confirmation, subsequent writes pass without a gate until Commit (decision 8)."
              )}
            </div>
          )}
          {gate.unknown.length > 0 && (
            <div className="banner warn gate-warn">
              {t("Statements that cannot be classified on a read-only prod profile:")}
              <ul className="gate-list">
                {gate.unknown.map((s, i) => (
                  <li key={i} className="mono">
                    {s}
                  </li>
                ))}
              </ul>
              <div className="banner-note">
                {t(
                  "If they really do write, the server's read-only session rejects them; the confirmation remains because the classifier cannot read them (§9.7.3)."
                )}
              </div>
            </div>
          )}
          {gate.irreversible.length > 0 && (
            <div className="banner warn gate-warn">
              {t("Operations that can NOT be undone with Rollback:")}
              <ul className="gate-list">
                {gate.irreversible.map((s, i) => (
                  <li key={i} className="mono">
                    {s}
                  </li>
                ))}
              </ul>
              {profile && !transactionalDdl(profile.dialect) && (
                <div className="banner-note">
                  {t("On {engine}, DDL implicitly commits anything still pending (§9.7.1).", {
                    engine: engineShortName(profile.dialect),
                  })}
                </div>
              )}
            </div>
          )}
          {gate.ddl.length > 0 && (
            <div className="banner warn gate-warn">
              {t("Schema changes, confirmed as asked in Settings → Execution (a Rollback undoes them):")}
              <ul className="gate-list">
                {gate.ddl.map((s, i) => (
                  <li key={i} className="mono">
                    {s}
                  </li>
                ))}
              </ul>
            </div>
          )}
          {gate.unfiltered.length > 0 && (
            <div className="banner warn gate-warn">
              {t("Without WHERE: they change every row of the table:")}
              <ul className="gate-list">
                {gate.unfiltered.map((s, i) => (
                  <li key={i} className="mono">
                    {s}
                  </li>
                ))}
              </ul>
            </div>
          )}
          <pre className="ddl-pane update-sql">{gate.sql}</pre>
          <ExplainEstimate connId={gate.connId} sql={gate.sql} />
          <div className="form-actions">
            <div className="spacer" />
            <button className="btn" onClick={onClose}>
              {t("Cancel")}
            </button>
            <button className="btn primary" onClick={onRun}>
              {t("Run")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
