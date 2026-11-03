// Unsaved grid edits (table tab): conversion into UPDATE, saving from
// outside the tab and the "Save / Discard / Cancel" question before the
// actions that would lose them (reload, filters, closing, quitting).
import { create } from "zustand";
import { asErrorOut } from "./errors";
import { t } from "./i18n";
import { BatchUpdateOut, CellAssign, ColumnOut, ErrorOut, ipc, isDefaultCell, PendingValue, RowUpdate, TableWrite } from "./ipc";
import { freeStickyForGridSave } from "./sticky";
import { NewRow, PendingEdits, pendingRowId, unsavedCount, useStore } from "./store";

function assign(column: string, value: PendingValue, numeric: Set<string>): CellAssign {
  if (isDefaultCell(value)) return { column, value: null, default: true };
  return numeric.has(column) ? { column, value, numeric: true } : { column, value };
}

/// Columns whose values are written bare, as numbers (`CellAssign.numeric`).
/// Only MySQL BIT today: the integer and float types accept the quoted
/// form everywhere, BIT does not ("Data too long" for '1').
export function numericColumns(columns: ColumnOut[] | null | undefined): Set<string> {
  return new Set((columns ?? []).filter((c) => c.type_display === "bit").map((c) => c.name));
}

/// One UPDATE per row, in the order of the pending rows.
export function pendingToUpdates(p: PendingEdits, numeric: Set<string>): RowUpdate[] {
  return Object.values(p).map((r) => ({
    key: r.key,
    set: Object.entries(r.set).map(([column, value]) => assign(column, value, numeric)),
  }));
}

/// The writes Save would run, in the order the backend runs them: the
/// DELETEs of the deleted rows, the UPDATEs of the cells (not those of the
/// deleted rows), one INSERT per new row. Cells at DEFAULT are not listed:
/// the server fills them anyway, and the preview stays short (all DEFAULT =
/// row with no columns).
export function pendingToWrites(
  tab: {
    pending: PendingEdits;
    newRows: NewRow[];
    deleted: Record<string, CellAssign[]>;
  },
  numeric: Set<string>
): TableWrite[] {
  const deletes: TableWrite[] = Object.values(tab.deleted).map((key) => ({ kind: "delete", key }));
  const alive: PendingEdits = Object.fromEntries(
    Object.entries(tab.pending).filter(([id]) => !(id in tab.deleted))
  );
  const updates: TableWrite[] = pendingToUpdates(alive, numeric).map((u) => ({ kind: "update", ...u }));
  const inserts: TableWrite[] = tab.newRows.map((r) => ({
    kind: "insert",
    values: Object.entries(r.values)
      .filter(([, v]) => !isDefaultCell(v))
      .map(([column, value]) => assign(column, value, numeric)),
  }));
  return [...deletes, ...updates, ...inserts];
}

/// After a save: the writes that succeeded leave the pending set (`writes`
/// in the order of `pendingToWrites`). Error on the shared lane = ROLLBACK,
/// everything stays; on prod (§9.7) the first `applied` are in the open
/// transaction, the rest stays pending.
export function settleWrites(tabId: string, connId: number, writes: TableWrite[], result: BatchUpdateOut): void {
  const done = result.error ? (result.rolled_back ? 0 : result.applied.length) : writes.length;
  const s = useStore.getState();
  const tab = s.tabs.find((x) => x.id === tabId);
  const next: PendingEdits = { ...(tab?.pending ?? {}) };
  const deleted = { ...(tab?.deleted ?? {}) };
  let insertsDone = 0;
  for (const w of writes.slice(0, done)) {
    if (w.kind === "insert") {
      insertsDone++;
      continue;
    }
    const id = pendingRowId(w.key.map((k) => k.value));
    delete next[id];
    // Deleted row: its modified cells go too, they are not written any more.
    if (w.kind === "delete") delete deleted[id];
  }
  s.setPending(tabId, next);
  s.setDeleted(tabId, deleted);
  if (insertsDone > 0) s.setNewRows(tabId, (tab?.newRows ?? []).slice(insertsDone));
  // On prod the batch enters the sticky's transaction: the badge and the
  // Commit/Rollback buttons must notice right away.
  void ipc
    .txnState(connId)
    .then((st) => useStore.getState().setTxnState(connId, st))
    .catch(() => {});
}

/// Saves the pending edits of a tab, even a non-active one. `null` =
/// everything saved; otherwise the error (the unapplied ones stay pending).
export async function savePendingEdits(tabId: string): Promise<ErrorOut | null> {
  const tab = useStore.getState().tabs.find((t) => t.id === tabId);
  if (!tab || !tab.table || unsavedCount(tab) === 0) return null;
  if (tab.connId === null) return asErrorOut(t("tab disconnected: reconnect the profile to save"));
  const connId = tab.connId;
  const writes = pendingToWrites(tab, numericColumns(tab.statements[0]?.columns));
  try {
    await freeStickyForGridSave(connId);
    const result = await ipc.tableWriteBatch(connId, tab.table.schema, tab.table.name, writes);
    settleWrites(tabId, connId, writes, result);
    return result.error;
  } catch (e) {
    return asErrorOut(e);
  }
}

// ------------------------------------------------ Save / Discard / Cancel

type UnsavedRequest = { tabIds: string[]; resolve: (proceed: boolean) => void };

/// Request in progress: `UnsavedDialog` (mounted in Studio) shows it.
export const useUnsaved = create<{ req: UnsavedRequest | null }>(() => ({ req: null }));

/// Before an action that would lose the pending edits of these tabs: if
/// there are any, asks Save / Discard / Cancel. `true` = one can proceed
/// (saved or discarded), `false` = the user cancelled.
export function guardUnsaved(tabIds: string[]): Promise<boolean> {
  const s = useStore.getState();
  const dirty = tabIds.filter((id) => {
    const tab = s.tabs.find((t) => t.id === id);
    return tab !== undefined && unsavedCount(tab) > 0;
  });
  if (dirty.length === 0) return Promise.resolve(true);
  // One question at a time: a second action in the meantime does not start.
  if (useUnsaved.getState().req) return Promise.resolve(false);
  return new Promise((resolve) => useUnsaved.setState({ req: { tabIds: dirty, resolve } }));
}

/// Tabs with pending edits among the given ones (all if omitted).
export function dirtyTabIds(filter: (t: { id: string; connId: number | null }) => boolean = () => true): string[] {
  return useStore
    .getState()
    .tabs.filter((t) => filter(t) && unsavedCount(t) > 0)
    .map((t) => t.id);
}
