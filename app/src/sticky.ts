// Freeing the connection's sticky (§3) before a command that runs on it.
import { asErrorOut } from "./errors";
import { t } from "./i18n";
import { ipc } from "./ipc";
import { connById, stickyRunningTab, useStore } from "./store";

/// Is the connection's sticky free for a command (Run, Commit, Rollback,
/// database switch, grid save in manual commit)? A query stopped in pause
/// (row pages or memory budget) stops by itself — the rows that arrived
/// stay — instead of blocking the command. Otherwise the reason it is not,
/// in the user's words: the command does not start, and saying nothing
/// looks like a dead shortcut.
export async function freeSticky(connId: number): Promise<string | null> {
  const busy = stickyRunningTab(useStore.getState(), connId);
  if (!busy) return null;
  if (busy.execId === null || !busy.statements.some((x) => x.budgetPaused)) {
    return t("“{tab}” is still running a query on this connection: wait for it or press Cancel.", {
      tab: busy.title,
    });
  }
  void ipc.budgetStop(connId, busy.execId);
  if (await storeUntil((st) => !stickyRunningTab(st, connId), 5000)) return null;
  return t("The paused query in “{tab}” is still stopping on the server: try again in a moment.", {
    tab: busy.title,
  });
}

/// Before a grid save: in manual commit (always on prod, §9.7) the batch
/// runs in the sticky's transaction, so a SELECT paused in a SQL tab of the
/// same connection must stop first. Throws the reason as an `ErrorOut` when
/// the sticky stays busy; in auto-commit the batch uses the shared lane.
export async function freeStickyForGridSave(connId: number): Promise<void> {
  if (!connById(useStore.getState(), connId)?.manualCommit) return;
  const busy = await freeSticky(connId);
  if (busy !== null) throw asErrorOut(busy);
}

/// Waits for the store to satisfy `pred` (at most `ms`): `false` = timed out.
function storeUntil(pred: (s: ReturnType<typeof useStore.getState>) => boolean, ms: number): Promise<boolean> {
  if (pred(useStore.getState())) return Promise.resolve(true);
  return new Promise((resolve) => {
    const timer = window.setTimeout(() => {
      unsub();
      resolve(false);
    }, ms);
    const unsub = useStore.subscribe((st) => {
      if (!pred(st)) return;
      window.clearTimeout(timer);
      unsub();
      resolve(true);
    });
  });
}
