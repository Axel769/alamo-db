// ⌘K command palette (interface plan, step 9): one box to open tables,
// tabs, connections and actions by typing a few letters. An extra shortcut,
// not a replacement for the panels.
//
// Tables come from the autocomplete catalogue of the current connection
// (same cache: no extra query); profiles from the connections panel's
// cache.
import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { t } from "./i18n";
import { ipc } from "./ipc";
import { activeTab, profileConns, useStore } from "./store";
import { activateTab, makePermanent, newSqlTab, openTablePreview } from "./tabs";
import { IconDatabase, IconExport, IconHistory, IconImport, IconPlug, IconPlus, IconSettings, IconSql, IconTable } from "./icons";
import type { TransferKind } from "./ConnectionsTransfer";
import { canSwitchDb } from "./engines";

type Item = { group: string; icon: React.ReactNode; title: string; sub?: string; run: () => void };

type Props = {
  onClose: () => void;
  onOpenDialog: (profileId?: number | null) => void;
  onPanel: (id: "schema" | "connections" | "history") => void;
  onSettings: () => void;
  onTransfer: (kind: TransferKind) => void;
};

/// Every word of the search must appear; higher score if the name starts
/// with the search. `null` = no match.
function score(text: string, words: string[]): number | null {
  const s = text.toLowerCase();
  let sc = 0;
  for (const w of words) {
    const i = s.indexOf(w);
    if (i < 0) return null;
    sc += i === 0 ? 3 : s[i - 1] === "_" || s[i - 1] === "." || s[i - 1] === " " ? 2 : 1;
  }
  return sc;
}

const MAX_PER_GROUP = 40;

export default function CommandPalette({ onClose, onOpenDialog, onPanel, onSettings, onTransfer }: Props) {
  const store = useStore();
  const [q, setQ] = useState("");
  const [sel, setSel] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);

  // "Current" connection: connection page, then active tab, then the last
  // used — the same rule as the toolbar.
  const connId = store.connPage ?? activeTab(store)?.connId ?? store.activeConnId;
  const conn = store.connections.find((c) => c.connId === connId);

  const catalog = useQuery({
    queryKey: ["autocomplete", conn?.connId ?? null],
    queryFn: () => ipc.autocompleteCatalog(conn!.connId),
    enabled: conn !== undefined,
    staleTime: Infinity,
  });
  // Where the database is the tab's (MySQL) it has its own query (same key as Studio):
  // the whole catalogue may be truncated by the cap and leave it out.
  const curTab = store.tabs.find((x) => x.id === store.activeTabId);
  const curDb =
    conn && canSwitchDb(conn.profile.dialect)
      ? (curTab?.kind === "sql" && curTab.connId === conn.connId ? curTab.db : null) ?? conn.currentDb
      : null;
  const dbCatalog = useQuery({
    queryKey: ["autocomplete", conn?.connId ?? null, curDb],
    queryFn: () => ipc.autocompleteCatalog(conn!.connId, curDb),
    enabled: conn !== undefined && curDb !== null && curDb !== undefined,
    staleTime: Infinity,
  });
  const profiles = useQuery({ queryKey: ["profiles"], queryFn: ipc.profilesList });

  const tables = useMemo(() => {
    const seen = new Set<string>();
    const out: { schema: string; table: string }[] = [];
    // Current database first: with equal score it stays on top.
    for (const c of [...(dbCatalog.data ?? []), ...(catalog.data ?? [])]) {
      const k = c.schema + "\u0000" + c.table;
      if (seen.has(k)) continue;
      seen.add(k);
      out.push({ schema: c.schema, table: c.table });
    }
    return out;
  }, [catalog.data, dbCatalog.data]);

  const items = useMemo(() => {
    const words = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const out: Item[] = [];
    const add = (group: string, list: { item: Item; key: string }[]) => {
      const ranked = list
        .map((x) => ({ item: x.item, sc: words.length ? score(x.key, words) : 0 }))
        .filter((x): x is { item: Item; sc: number } => x.sc !== null)
        .sort((a, b) => b.sc - a.sc)
        .slice(0, MAX_PER_GROUP);
      for (const r of ranked) out.push({ ...r.item, group });
    };
    const act = (run: () => void) => () => {
      onClose();
      run();
    };

    if (conn) {
      // No search, no list: on a big schema it would be noise.
      if (words.length > 0) {
        add(
          t("Tables · {conn}", { conn: conn.profile.name }),
          tables.map((x) => ({
            key: x.table + " " + x.schema,
            item: {
              group: "",
              icon: <IconTable />,
              title: x.table,
              sub: x.schema,
              run: act(() => makePermanent(openTablePreview(conn.connId, x.schema, x.table))),
            },
          }))
        );
      }
    }
    add(
      t("Open tabs"),
      store.tabs.map((tb) => {
        const c = store.connections.find((x) => x.connId === tb.connId);
        const db = tb.kind === "table" ? tb.table?.schema : c?.currentDb;
        const where = [c?.profile.name, db].filter(Boolean).join(" · ");
        return {
          // The connection name does not enter the tab search: typing it
          // means one wants the connection, not all its tabs.
          key: tb.title + " " + (db ?? ""),
          item: { group: "", icon: tb.kind === "table" ? <IconTable /> : <IconSql />, title: tb.title, sub: where, run: act(() => activateTab(tb.id)) },
        };
      })
    );
    const roots = profileConns(store.connections);
    const open = new Set(roots.map((c) => c.profile.id));
    add(t("Connections"), [
      ...roots.map((c) => ({
        key: c.profile.name,
        item: {
          group: "",
          icon: <IconPlug size={14} />,
          title: c.profile.name,
          sub: t("open — databases and tables"),
          run: act(() => store.openConnPage(c.connId)),
        },
      })),
      ...(profiles.data ?? [])
        .filter((p) => !open.has(p.id))
        .map((p) => ({
          key: p.name,
          item: { group: "", icon: <IconPlug size={14} />, title: p.name, sub: t("connect…"), run: act(() => onOpenDialog(p.id)) },
        })),
    ]);
    add(t("Actions"), [
      { key: t("New query"), item: { group: "", icon: <IconPlus />, title: t("New query"), sub: conn?.profile.name, run: act(() => newSqlTab()) } },
      { key: t("Show schema"), item: { group: "", icon: <IconDatabase size={14} />, title: t("Show schema"), sub: "⌘B", run: act(() => onPanel("schema")) } },
      { key: t("Query history"), item: { group: "", icon: <IconHistory size={14} />, title: t("Query history"), run: act(() => onPanel("history")) } },
      { key: t("New connection…"), item: { group: "", icon: <IconPlus />, title: t("New connection…"), run: act(() => onOpenDialog(null)) } },
      { key: t("Export connections…"), item: { group: "", icon: <IconExport />, title: t("Export connections…"), run: act(() => onTransfer("export")) } },
      { key: t("Import connections…"), item: { group: "", icon: <IconImport />, title: t("Import connections…"), run: act(() => onTransfer("import")) } },
      { key: t("Settings"), item: { group: "", icon: <IconSettings size={14} />, title: t("Settings"), run: act(onSettings) } },
    ]);
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [q, tables, store.tabs, store.connections, profiles.data, conn?.connId]);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);
  useEffect(() => setSel(0), [q]);
  useEffect(() => {
    listRef.current?.querySelector(".cmdk-item.on")?.scrollIntoView({ block: "nearest" });
  }, [sel]);

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSel((s) => Math.min(items.length - 1, s + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSel((s) => Math.max(0, s - 1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      items[sel]?.run();
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  };

  let lastGroup = "";
  return (
    <div className="cmdk-overlay" onMouseDown={onClose}>
      <div className="cmdk" onMouseDown={(e) => e.stopPropagation()} onKeyDown={onKey}>
        <input
          ref={inputRef}
          className="cmdk-input"
          value={q}
          placeholder={
            conn
              ? t("Table, tab, connection or action… ({conn})", { conn: conn.profile.name })
              : t("Tab, connection or action…")
          }
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          onChange={(e) => setQ(e.target.value)}
        />
        <div className="cmdk-list" ref={listRef}>
          {items.map((it, i) => {
            const head = it.group !== lastGroup;
            lastGroup = it.group;
            return (
              <div key={i}>
                {head && <div className="panel-section">{it.group}</div>}
                <button
                  className={"cmdk-item" + (i === sel ? " on" : "")}
                  onMouseEnter={() => setSel(i)}
                  onClick={it.run}
                >
                  <span className="cmdk-icon">{it.icon}</span>
                  <span className="cmdk-title">{it.title}</span>
                  {it.sub && <span className="cmdk-sub">{it.sub}</span>}
                </button>
              </div>
            );
          })}
          {items.length === 0 && <div className="tree-note">{t("no results")}</div>}
        </div>
        <div className="cmdk-foot">
          <span>↑↓ {t("select")}</span>
          <span>↵ {t("open")}</span>
          <span>Esc {t("close")}</span>
        </div>
      </div>
    </div>
  );
}
