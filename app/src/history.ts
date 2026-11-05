// Query history (M4): the executions started from the editor, most recent
// first. Lives in localStorage — it is a convenience for looking things up,
// not critical data like workspace/autosave (those live on SQLite, §9.4):
// losing it loses no work. Zustand store separate from store.ts: it changes
// only on each execution, it must not re-render whoever watches the tabs.
import { create } from "zustand";
import { useStore } from "./store";
import { setting } from "./settings";

export type HistoryEntry = {
  sql: string;
  /// Origin profile: id to recognise it, name and colour denormalised —
  /// the entry stays readable even if the profile is deleted.
  profileId: number | null;
  profileName: string;
  color: string | null;
  /// Epoch ms of the execution (of the last one, for consecutive repeats).
  at: number;
};

const KEY = "alamo.history";
/// Huge batches pasted into the editor: a prefix goes into the history,
/// localStorage is no place for megabytes of SQL.
const SQL_CAP = 10_000;

function load(): HistoryEntry[] {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (e): e is HistoryEntry =>
        typeof e === "object" &&
        e !== null &&
        typeof (e as HistoryEntry).sql === "string" &&
        typeof (e as HistoryEntry).at === "number"
    );
  } catch {
    return [];
  }
}

function persist(entries: HistoryEntry[]): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(entries));
  } catch {
    // Quota full or storage denied: the in-memory history stays valid.
  }
}

export const useHistory = create<{ entries: HistoryEntry[] }>(() => ({
  entries: load(),
}));

/// To be called when an editor execution really starts (doExecute, after
/// the successful invoke): attempts rejected by the prod gates do not count.
export function recordHistory(connId: number, sql: string): void {
  const trimmed = sql.trim();
  if (!trimmed) return;
  const conn = useStore.getState().connections.find((c) => c.connId === connId);
  const entry: HistoryEntry = {
    sql: trimmed.slice(0, SQL_CAP),
    profileId: conn?.profile.id ?? null,
    profileName: conn?.profile.name ?? "",
    color: conn?.profile.color ?? null,
    at: Date.now(),
  };
  useHistory.setState((s) => {
    // Re-running the same query must not fill the list: the entry at the
    // head is only updated.
    const head = s.entries[0];
    const entries =
      head && head.sql === entry.sql && head.profileId === entry.profileId
        ? [{ ...head, at: entry.at }, ...s.entries.slice(1)]
        : [entry, ...s.entries].slice(0, setting("editor.history_size"));
    persist(entries);
    return { entries };
  });
}

export function clearHistory(): void {
  useHistory.setState({ entries: [] });
  persist([]);
}
