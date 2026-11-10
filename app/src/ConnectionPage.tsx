// Connection page (interface plan, step 4): the "home" of an open
// connection, reached from the label of its tab group or from its
// dot. Lists the databases (on PostgreSQL: the schemas of the connected
// database) with filter, numbers and "in use", and from there opens tables and queries.
//
// Names come from the tree's list (same cache, immediate); the
// numbers from `schema_stats`, as in DBeaver only ON REQUEST (the
// "Compute sizes" button or sorting by size): on servers with many
// databases it costs tens of seconds, and on a connection of its own (backend), so
// list and tree never wait for it.
import { useIsFetching, useQuery, useQueryClient } from "@tanstack/react-query";
import { ProdTag } from "./ProdTag";
import { useEffect, useMemo, useRef, useState } from "react";
import CatalogError from "./CatalogError";
import { locale, t } from "./i18n";
import { tlsMode } from "./tlsModes";
import { tlsSummary } from "./TlsFacts";
import { ipc, RelationOut, SchemaStatsOut } from "./ipc";
import {
  ConnUi,
  openedDbByConn,
  pageFilterByConn,
  pageFilterKey,
  hasDbFilter,
  pgDbEntered,
  profileDatabase,
  rememberOpenedDb,
  rootConnId,
  tabDatabase,
  useStore,
} from "./store";
import { openConnFor, openDatabasePageOn } from "./databases";
import { makePermanent, newSqlTab, openTablePreview } from "./tabs";
import { IconChevronLeft, IconKey, IconRefresh, IconStar, RelIcon } from "./icons";
import { connColor } from "./connColor";
import DbPicker from "./DbPicker";
import { FavStar, useFavorites } from "./favorites";
import { canSwitchDb, engineName } from "./engines";

type SortKey = "name" | "size" | "use";

/// Filter level of the server's databases (PG, §3.3) in `pageFilterByConn`:
/// no schema or database name contains it.
const DBS_LEVEL = "\u0001databases";

/// Connections for which the user asked for sizes: the page unmounts
/// when switching tabs, "already computed" must survive (like `openedDbByConn`).
const statsWantedByConn = new Set<number>();

function fmtSize(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined) return "";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = bytes;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return `${v >= 100 || u === 0 ? Math.round(v) : v.toFixed(1)} ${units[u]}`;
}

export function connInitials(name: string): string {
  const parts = name.split(/[^\p{L}\p{N}]+/u).filter(Boolean);
  const s = parts.length > 1 ? parts[0][0] + parts[1][0] : (parts[0] ?? "?").slice(0, 2);
  return s.toUpperCase();
}

type Props = {
  conn: ConnUi;
  onEditProfile: (profileId: number | null) => void;
  /// Extra actions in the header (Disconnect: the logic with the
  /// pending-writes confirmation lives in the Studio).
  actions?: React.ReactNode;
};

export default function ConnectionPage({ conn, onEditProfile, actions }: Props) {
  const connId = conn.connId;
  // MySQL: the list shows the databases the tabs switch between; PG: the
  // schemas of the connection's database.
  const switchDb = canSwitchDb(conn.profile.dialect);
  const tabs = useStore((s) => s.tabs);
  const nav = useStore((s) => s.connPageNav);
  const [sort, setSort] = useState<SortKey>("name");
  const [showSystem, setShowSystem] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  /// Database (or schema) "opened" with a double click: the main list
  /// shows its tables instead of the list of databases.
  const [opened, setOpened] = useState<string | null>(() => openedDbByConn.get(conn.connId) ?? null);
  const [wantStats, setWantStats] = useState(() => statsWantedByConn.has(conn.connId));
  /// PG (§3.3): the page of a profile's connection starts from the server's
  /// databases; each one is entered on its own connection, whose page
  /// shows the schemas. `rootId`: the profile's connection.
  const rootId = rootConnId(useStore((s) => s.connections), connId) ?? connId;
  const dbsLevel = (id: number) => conn.oneDbPerConn && conn.parent === null && !pgDbEntered.has(id);
  const [dbList, setDbList] = useState(() => dbsLevel(conn.connId));
  const [dbError, setDbError] = useState<{ db: string; error: unknown } | null>(null);
  /// Favorites only: in the list the databases that are starred or hold
  /// starred tables (as in the tree), in the tables view the starred
  /// tables. Same stars as the tree (per profile); the switch is this
  /// page's own, not the tree's.
  const [favOnly, setFavOnly] = useState(false);
  const favs = useFavorites(conn.profile.id);
  const askStats = () => {
    statsWantedByConn.add(connId);
    setWantStats(true);
  };
  // The filter comes back as it was when re-entering the page (or the level).
  const [filter, setFilterState] = useState(
    () =>
      pageFilterByConn.get(
        pageFilterKey(conn.connId, dbsLevel(conn.connId) ? DBS_LEVEL : (openedDbByConn.get(conn.connId) ?? null))
      ) ?? ""
  );
  const setFilter = (v: string, level: string | null = dbList ? DBS_LEVEL : opened) => {
    if (v === "") pageFilterByConn.delete(pageFilterKey(connId, level));
    else pageFilterByConn.set(pageFilterKey(connId, level), v);
    setFilterState(v);
  };
  const [error, setError] = useState<string | null>(null);
  const filterRef = useRef<HTMLInputElement | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);

  // Databases, tables, columns and numbers load once and stay in
  // cache: a table created by a query does not show up on its own. "Refresh"
  // (button or right click) asks the server for them for THIS connection;
  // the cache is the same as the tree's, which updates along.
  const qc = useQueryClient();
  const ofConn = (q: { queryKey: readonly unknown[] }) => q.queryKey[1] === connId;
  const refreshing = useIsFetching({ predicate: ofConn }) > 0;
  const refresh = () => {
    setMenu(null);
    void qc.invalidateQueries({ predicate: ofConn });
  };

  // The right-click menu closes with a click elsewhere or Esc.
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  // Same query key as the tree: names already in cache, no wait.
  const schemas = useQuery({ queryKey: ["schemas", connId], queryFn: () => ipc.schemaList(connId) });
  // Off until asked: it does not start on its own at opening. Computed
  // once, they stay (a disabled query still shows the cache);
  // "Refresh" recomputes them only if they had been asked for.
  const stats = useQuery({
    queryKey: ["schemaStats", connId],
    queryFn: () => ipc.schemaStats(connId),
    enabled: wantStats,
    staleTime: Infinity,
  });

  useEffect(() => {
    const level = openedDbByConn.get(connId) ?? null;
    const dbs = dbsLevel(connId);
    setDbList(dbs);
    setDbError(null);
    setFilterState(pageFilterByConn.get(pageFilterKey(connId, dbs ? DBS_LEVEL : level)) ?? "");
    setSelected(null);
    setOpened(level);
    setWantStats(statsWantedByConn.has(connId));
    setError(null);
    filterRef.current?.focus();
  }, [connId, nav]);

  // ⌘F goes to the filter (list of databases, or tables of the open database),
  // with the text selected to rewrite it. The page is mounted only when
  // it is in the foreground: no conflict with the ⌘F of the SQL tabs.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.altKey || e.shiftKey || e.key.toLowerCase() !== "f") return;
      if (e.defaultPrevented || !filterRef.current) return;
      e.preventDefault();
      filterRef.current.focus();
      filterRef.current.select();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const enter = (name: string | null) => {
    if (name === null) openedDbByConn.delete(connId);
    else openedDbByConn.set(connId, name);
    // Remembered per profile: only the profile's own database, the schemas
    // of the others do not exist there (§3.3).
    if (conn.profile.id !== null && conn.parent === null) rememberOpenedDb(conn.profile.id, name);
    setOpened(name);
    setFilterState(pageFilterByConn.get(pageFilterKey(connId, name)) ?? "");
    setError(null);
    filterRef.current?.focus();
  };

  /// Tabs open on that database (MySQL) or schema (PG, its tables).
  const inUse = (name: string): number =>
    tabs.filter((tb) =>
      tb.connId === connId && (switchDb ? tabDatabase(tb, switchDb, conn.currentDb) === name : tb.table?.schema === name)
    ).length;

  /// PG: into a database of the server, on its connection (§3.3).
  const enterDb = (db: string) => {
    setDbError(null);
    openDatabasePageOn(rootId, db).catch((error: unknown) => setDbError({ db, error }));
  };
  /// PG: back to the server's databases, on the profile's connection's page.
  const backToDbs = () => {
    pgDbEntered.delete(rootId);
    useStore.setState((s) => ({ connPage: rootId, activeConnId: rootId, connPageNav: s.connPageNav + 1 }));
  };
  // Server's databases (PG): same query key as the tree.
  const dbs = useQuery({
    queryKey: ["databases", rootId],
    queryFn: () => ipc.serverDatabases(rootId),
    enabled: conn.oneDbPerConn,
    staleTime: Infinity,
  });
  const dbNames = (dbs.data ?? []).map((d) => d.name);

  const rows = useMemo(() => {
    const byName = new Map<string, SchemaStatsOut>((stats.data ?? []).map((s) => [s.name, s]));
    const f = filter.trim().toLowerCase();
    const all = (schemas.data ?? []).map((s) => ({ ...s, stats: byName.get(s.name), use: inUse(s.name) }));
    const visible = all.filter(
      (s) =>
        (showSystem || !s.is_system) &&
        (!f || s.name.toLowerCase().includes(f)) &&
        (!favOnly || favs?.touches(s.name)),
    );
    visible.sort((a, b) =>
      sort === "size"
        ? (b.stats?.size_bytes ?? -1) - (a.stats?.size_bytes ?? -1) || a.name.localeCompare(b.name)
        : sort === "use"
          ? b.use - a.use || a.name.localeCompare(b.name)
          : a.name.localeCompare(b.name)
    );
    return { visible, total: all.filter((s) => showSystem || !s.is_system).length, system: all.filter((s) => s.is_system).length };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [schemas.data, stats.data, filter, sort, showSystem, tabs, conn.currentDb, favOnly, favs]);

  const sel = selected !== null ? rows.visible.find((r) => r.name === selected) ?? null : null;

  /// New SQL tab on that database. On MySQL the database belongs to the tab: the
  /// `USE` runs at the first execution. On PostgreSQL the schema is not
  /// "opened": the tab is just born.
  const newQueryOn = (name: string | null) => {
    setError(null);
    if (!switchDb || name === null) {
      newSqlTab();
      return;
    }
    newSqlTab("", false, undefined, name);
  };

  const noun = switchDb ? t("databases") : t("schemas");

  return (
    <div
      className="conn-page"
      style={{ "--conn": connColor(conn.profile) } as React.CSSProperties}
      onContextMenu={(e) => {
        // In text fields the system menu stays (copy/paste).
        if (e.target instanceof Element && e.target.closest("input")) return;
        e.preventDefault();
        setMenu({ x: e.clientX, y: e.clientY });
      }}
    >
      <div className="conn-page-head">
        <div className="conn-page-av">{connInitials(conn.profile.name)}</div>
        <div className="conn-page-title">
          <h2>
            {conn.profile.name}
            {conn.profile.is_production && <ProdTag />}
            {conn.profile.read_only && <span className="conn-page-ro">{t("read-only")}</span>}
            <SecurityTag conn={conn} />
          </h2>
          <div className="conn-page-sub">
            {engineName(conn.profile.dialect)} · {conn.profile.username}@{conn.profile.host}:
            {conn.profile.port}
            {!switchDb && !dbList && conn.profile.database ? ` · ${conn.profile.database}` : ""}
            {conn.profile.sslmode !== "disable" ? ` · TLS ${tlsMode(conn.profile.sslmode).short}` : ""}
            {conn.profile.ssh_profile_id !== null ? " · SSH" : ""}
          </div>
          {/* What the channel really negotiated (§12.2), in plain sight. */}
          {tlsSummary(conn.security) && <div className="conn-page-sub conn-page-tls">{tlsSummary(conn.security)}</div>}
        </div>
        <div className="conn-page-actions">
          {/* With an open database the query is born there, like "New query here". */}
          <button className="btn small" onClick={() => newQueryOn(opened)}>
            {t("New query")}
          </button>
          <button className="btn small" onClick={() => onEditProfile(conn.profile.id)}>
            {t("Profile…")}
          </button>
          {actions}
        </div>
      </div>

      {dbList ? (
        <DatabasesView
          root={conn}
          names={dbNames}
          query={dbs}
          filter={filter}
          setFilter={(v) => setFilter(v)}
          filterRef={filterRef}
          refreshing={refreshing}
          onRefresh={refresh}
          onEnter={enterDb}
          error={dbError}
        />
      ) : (
      <>
      <div className="conn-page-tool">
        {conn.oneDbPerConn && (
          <span className="conn-page-crumb">
            <button className="btn small" onClick={backToDbs} title={t("Back to the databases")}>
              <IconChevronLeft /> {t("Databases")}
            </button>
            {/* Quick switch to another database of the server, on its connection. */}
            <DbPicker
              value={profileDatabase(conn)}
              options={dbNames}
              disabled={false}
              title={t("Switch to another database")}
              onPick={enterDb}
            />
          </span>
        )}
        {opened !== null && (
          <span className="conn-page-crumb">
            <button className="btn small" onClick={() => enter(null)} title={t("Back to the list (Esc)")}>
              <IconChevronLeft /> {switchDb ? t("Database") : t("Schemas")}
            </button>
            {/* The name is also the quick switch: another database of the
                same connection without going back to the list. */}
            <DbPicker
              value={opened}
              options={(schemas.data ?? [])
                .filter((s) => showSystem || !s.is_system || s.name === opened)
                .map((s) => s.name)
                .sort((a, b) => a.localeCompare(b))}
              disabled={false}
              title={switchDb ? t("Switch to another database") : t("Switch to another schema")}
              onPick={(name) => enter(name)}
            />
          </span>
        )}
        {favs && (
          <button
            className={"tree-fav-only" + (favOnly ? " on" : "")}
            title={favOnly ? t("Show everything") : t("Show favorites only")}
            aria-pressed={favOnly}
            onClick={() => setFavOnly(!favOnly)}
          >
            <IconStar fill={favOnly ? "currentColor" : "none"} />
          </button>
        )}
        <input
          ref={filterRef}
          className="conn-page-filter"
          value={filter}
          placeholder={
            opened !== null ? t("filter tables…") : t("filter {n} {what}…", { n: rows.total, what: noun })
          }
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          onChange={(e) => setFilter(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              // Esc clears the filter; with an empty filter goes back to the list.
              if (filter !== "") setFilter("");
              else if (opened !== null) enter(null);
              else if (conn.oneDbPerConn) backToDbs();
            }
            if (e.key === "Enter" && opened === null && rows.visible.length > 0) {
              // A single result: Enter goes straight into it.
              if (rows.visible.length === 1) enter(rows.visible[0].name);
              else setSelected(rows.visible[0].name);
            }
          }}
        />
        <span className="conn-page-count">
          {opened === null && filter.trim() !== ""
            ? t("{n} of {tot}", { n: rows.visible.length, tot: rows.total })
            : ""}
          {stats.isFetching ? " " + t("computing sizes…") : ""}
          {stats.isError ? " " + t("sizes unavailable") : ""}
        </span>
        <div className="spacer" />
        <button
          className="btn small"
          onClick={refresh}
          disabled={refreshing}
          title={t("Reload databases and tables from the server")}
        >
          {refreshing ? <span className="spinner" aria-hidden /> : <IconRefresh />} {t("Refresh")}
        </button>
        {opened === null && !wantStats && (
          <button
            className="btn small"
            onClick={askStats}
            title={t("Reads table counts and sizes of every database from the server: it can take a while")}
          >
            {t("Compute sizes")}
          </button>
        )}
        {opened !== null && (
          <button className="btn small" onClick={() => newQueryOn(opened)}>
            {switchDb ? t("New query here") : t("New query")}
          </button>
        )}
        {opened === null && rows.system > 0 && (
          <label className="check conn-page-sys">
            <input type="checkbox" checked={showSystem} onChange={(e) => setShowSystem(e.target.checked)} />
            {t("system ({n})", { n: rows.system })}
          </label>
        )}
        {opened === null && (
          <div className="seg">
            {(["name", "size", "use"] as const).map((k) => (
              <button
                key={k}
                className={sort === k ? "on" : ""}
                onClick={() => {
                  // Sorting by size without sizes would make no sense.
                  if (k === "size") askStats();
                  setSort(k);
                }}
              >
                {k === "name" ? t("Name") : k === "size" ? t("Size") : t("In use")}
              </button>
            ))}
          </div>
        )}
      </div>

      {error && <div className="banner warn">{error}</div>}
      {dbError && (
        <CatalogError
          className="page"
          label={t("database unavailable")}
          error={dbError.error}
          retrying={false}
          onRetry={() => enterDb(dbError.db)}
        />
      )}
      {schemas.isError && (
        <CatalogError
          className="page"
          label={t("list unavailable")}
          error={schemas.error}
          retrying={schemas.isFetching}
          onRetry={() => void schemas.refetch()}
        />
      )}

      {opened !== null ? (
        <TablesView
          connId={connId}
          profileId={conn.profile.id}
          schema={opened}
          filter={filter}
          favOnly={favOnly}
        />
      ) : (
      <div className="conn-page-body">
        <div className="conn-page-list">
          <table className="conn-page-table">
            <thead>
              <tr>
                {favs && <th className="fav" aria-label={t("Favorite")} />}
                <th>{switchDb ? t("Database") : t("Schema")}</th>
                <th>{t("Charset")}</th>
                <th>{t("Collation")}</th>
                <th className="num">{t("Tables")}</th>
                <th className="num">{t("Size")}</th>
                <th>{t("In use")}</th>
              </tr>
            </thead>
            <tbody>
              {rows.visible.map((r) => (
                <tr
                  key={r.name}
                  className={(r.name === selected ? "sel" : "") + (r.is_system ? " system" : "")}
                  onClick={() => setSelected(r.name)}
                  onDoubleClick={() => enter(r.name)}
                >
                  {favs && (
                    <td className="fav">
                      <FavStar favs={favs} schema={r.name} />
                    </td>
                  )}
                  <td className="name">
                    {r.name}
                    {r.name === conn.currentDb && <span className="conn-page-cur">{t("current")}</span>}
                  </td>
                  <td>{r.stats?.charset ?? ""}</td>
                  <td>{r.stats?.collation ?? ""}</td>
                  <td className="num">{r.stats ? r.stats.tables.toLocaleString(locale()) : ""}</td>
                  <td className="num">{fmtSize(r.stats?.size_bytes)}</td>
                  <td>{r.use > 0 && <span className="conn-page-use">{t("{n} tabs", { n: r.use })}</span>}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {schemas.isLoading && <div className="tree-note">{t("loading…")}</div>}
          {!schemas.isLoading && rows.visible.length === 0 && (
            <div className="tree-note">{favOnly && filter.trim() === "" ? t("no favorites") : t("no results")}</div>
          )}
        </div>
        <aside className="conn-page-side">
          {sel ? (
            <SchemaSide
              connId={connId}
              schema={sel.name}
              stats={sel.stats}
              switchDb={switchDb}
              onNewQuery={() => newQueryOn(sel.name)}
            />
          ) : (
            <div className="conn-page-hint">
              <p>
                <b>{t("Click")}</b> {t("a row to see its tables and open them.")}
              </p>
              <p>
                <b>{t("Double click")}</b> = {t("opens the list of its tables.")}
              </p>
              <p>{t("With many databases: type in the filter and press ↵ to select the first one.")}</p>
              {!wantStats && (
                <p>
                  <b>{t("Tables and sizes")}</b>: {t("press")} <b>{t("Compute sizes")}</b>{" "}
                  {t("(on large servers it can take a while).")}
                </p>
              )}
            </div>
          )}
        </aside>
      </div>
      )}

      </>
      )}

      {menu && (
        <div
          className="tab-menu"
          style={{ left: menu.x, top: menu.y }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <button className="tab-menu-item" onClick={refresh} disabled={refreshing}>
            {t("Refresh")}
          </button>
        </div>
      )}
    </div>
  );
}

/// Tables of the open database, in the main list: double click (or
/// Enter from the filter, if one is left) opens the table; a single click
/// selects it and shows its columns on the side.
function TablesView({
  connId,
  profileId,
  schema,
  filter,
  favOnly,
}: {
  connId: number;
  profileId: number | null;
  schema: string;
  filter: string;
  favOnly: boolean;
}) {
  const [selected, setSelected] = useState<string | null>(null);
  const favs = useFavorites(profileId);
  // Same query keys as the tree: shared cache.
  const rels = useQuery({
    queryKey: ["relations", connId, schema],
    queryFn: () => ipc.schemaRelations(connId, schema),
    staleTime: Infinity,
  });
  useEffect(() => setSelected(null), [schema]);
  const f = filter.trim().toLowerCase();
  const list = (rels.data ?? []).filter(
    (r) => (!f || r.name.toLowerCase().includes(f)) && (!favOnly || favs?.has(schema, r.name)),
  );
  const open = (name: string) => makePermanent(openTablePreview(connId, schema, name));
  // Enter in the filter (which sits in the toolbar above): opens the table if the
  // filter leaves only one.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Enter" || list.length !== 1) return;
      if (!(e.target as Element | null)?.classList.contains("conn-page-filter")) return;
      open(list[0].name);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [list.length, list[0]?.name, connId, schema]);
  const cols = useQuery({
    queryKey: ["columns", connId, schema, selected],
    queryFn: () => ipc.schemaColumns(connId, schema, selected!),
    staleTime: Infinity,
    enabled: selected !== null,
  });
  return (
    <div className="conn-page-body">
      <div className="conn-page-list">
        <table className="conn-page-table">
          <thead>
            <tr>
              {/* Star column: no heading, it is only the toggle. */}
              {favs && <th className="fav" aria-label={t("Favorite")} />}
              <th>{t("Table")}</th>
              <th>{t("Type")}</th>
              <th className="num">{t("Rows (estimate)")}</th>
              <th>{t("Comment")}</th>
            </tr>
          </thead>
          <tbody>
            {list.map((r) => (
              <tr
                key={r.name}
                className={r.name === selected ? "sel" : ""}
                onClick={() => setSelected(r.name)}
                onDoubleClick={() => open(r.name)}
              >
                {favs && (
                  <td className="fav">
                    <FavStar favs={favs} schema={schema} table={r.name} />
                  </td>
                )}
                <td className="name">{r.name}</td>
                <td>{r.kind === "table" ? t("table") : r.kind === "view" ? t("view") : r.kind.replace("_", " ")}</td>
                <td className="num">
                  {r.estimated_rows !== null ? r.estimated_rows.toLocaleString(locale()) : ""}
                </td>
                <td>{r.comment ?? ""}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {rels.isLoading && <div className="tree-note">{t("loading…")}</div>}
        {rels.isError && (
          <CatalogError
            label={t("tables unavailable")}
            error={rels.error}
            retrying={rels.isFetching}
            onRetry={() => void rels.refetch()}
          />
        )}
        {!rels.isLoading && !rels.isError && list.length === 0 && (
          <div className="tree-note">{favOnly && !f ? t("no favorites") : t("no results")}</div>
        )}
      </div>
      <aside className="conn-page-side">
        {selected !== null ? (
          <>
            <h3>{selected}</h3>
            <div className="conn-page-meta">{schema}</div>
            <div className="conn-page-side-actions">
              <button className="btn small primary" onClick={() => open(selected)}>
                {t("Open table")}
              </button>
            </div>
            <div className="conn-page-rels">
              {(cols.data ?? []).map((c) => (
                <div key={c.name} className="conn-page-col">
                  {/* The key's space is always there: the names stay
                      aligned, with or without a key. */}
                  <span className="conn-page-col-key">{c.is_primary_key && <IconKey />}</span>
                  <span className="conn-page-col-name">{c.name}</span>
                  <span className="conn-page-col-type">{c.type_display}</span>
                </div>
              ))}
            </div>
          </>
        ) : (
          <div className="conn-page-hint">
            <p>
              <b>{t("Double click")}</b> = {t("opens the table.")}
            </p>
            <p>
              <b>{t("Click")}</b> = {t("shows its columns here.")}
            </p>
          </div>
        )}
      </aside>
    </div>
  );
}

function SchemaSide({
  connId,
  schema,
  stats,
  switchDb,
  onNewQuery,
}: {
  connId: number;
  schema: string;
  stats: SchemaStatsOut | undefined;
  switchDb: boolean;
  onNewQuery: () => void;
}) {
  const [filter, setFilter] = useState("");
  // Same query key as the tree.
  const rels = useQuery({
    queryKey: ["relations", connId, schema],
    queryFn: () => ipc.schemaRelations(connId, schema),
    staleTime: Infinity,
  });
  useEffect(() => setFilter(""), [schema]);
  const f = filter.trim().toLowerCase();
  const list = (rels.data ?? []).filter((r: RelationOut) => !f || r.name.toLowerCase().includes(f));
  return (
    <>
      <h3>{schema}</h3>
      <div className="conn-page-meta">
        {stats
          ? [
              t("{n} tables", { n: stats.tables.toLocaleString(locale()) }),
              fmtSize(stats.size_bytes),
              stats.charset,
            ]
              .filter(Boolean)
              .join(" · ")
          : ""}
      </div>
      <div className="conn-page-side-actions">
        <button className="btn small primary" onClick={onNewQuery}>
          {switchDb ? t("New query here") : t("New query")}
        </button>
      </div>
      <input
        className="conn-page-filter small"
        value={filter}
        placeholder={t("filter tables…")}
        autoCapitalize="none"
        autoCorrect="off"
        spellCheck={false}
        onChange={(e) => setFilter(e.target.value)}
      />
      <div className="conn-page-rels">
        {rels.isLoading && <div className="tree-note">{t("loading…")}</div>}
        {rels.isError && (
          <CatalogError
            label={t("tables unavailable")}
            error={rels.error}
            retrying={rels.isFetching}
            onRetry={() => void rels.refetch()}
          />
        )}
        {list.map((r) => (
          <button
            key={r.name}
            className="conn-page-rel"
            title={r.comment ?? undefined}
            // Permanent tab: from here it is opened to work on it, not to peek
            // (the preview remains the single click on the tree).
            onClick={() => makePermanent(openTablePreview(connId, r.schema, r.name))}
          >
            <span className={"rel-icon " + r.kind}><RelIcon kind={r.kind} /></span>
            <span className="tree-label">{r.name}</span>
            <span className="tree-count">
              {r.estimated_rows !== null ? r.estimated_rows.toLocaleString(locale()) : ""}
            </span>
          </button>
        ))}
      </div>
    </>
  );
}

/// What really protects the connection: padlock only if the certificate
/// was verified. `prefer` fallen back to plaintext and `require` (encrypts
/// without knowing whom it talks to) stay visible, not hidden in a tooltip.
function SecurityTag({ conn }: { conn: ConnUi }) {
  const { encrypted, verified } = conn.security;
  const ssh = conn.profile.ssh_profile_id !== null;
  if (verified)
    return (
      <span className="conn-sec ok" title={t("Encrypted channel, server certificate verified")}>
        {t("TLS verified")}
      </span>
    );
  if (encrypted)
    return (
      <span
        className="conn-sec warn"
        title={t("Encrypted channel, but the server is not verified: whoever controls the network can impersonate it. Use verify-full.")}
      >
        {t("TLS not verified")}
      </span>
    );
  return (
    <span
      className="conn-sec warn"
      title={
        ssh
          ? t("Encrypted only up to the SSH bastion: from there to the database the traffic, password included, travels in plaintext.")
          : t("Plaintext traffic, password included.")
      }
    >
      {ssh ? t("plaintext after the bastion") : t("plaintext")}
    </span>
  );
}

/// PG (§3.3): the server's databases. Double click (or ↵ in the filter with
/// one left) enters one, on a connection of its own.
function DatabasesView({
  root,
  names,
  query,
  filter,
  setFilter,
  filterRef,
  refreshing,
  onRefresh,
  onEnter,
  error,
}: {
  root: ConnUi;
  names: string[];
  query: { isLoading: boolean; isError: boolean; isFetching: boolean; error: unknown; refetch: () => unknown };
  filter: string;
  setFilter: (v: string) => void;
  filterRef: React.RefObject<HTMLInputElement>;
  refreshing: boolean;
  onRefresh: () => void;
  onEnter: (db: string) => void;
  error: { db: string; error: unknown } | null;
}) {
  const [selected, setSelected] = useState<string | null>(null);
  const conns = useStore((s) => s.connections);
  const tabs = useStore((s) => s.tabs);
  const f = filter.trim().toLowerCase();
  const visible = names.filter((n) => !f || n.toLowerCase().includes(f));
  const mine = profileDatabase(root);
  /// Tabs open on that database, on its connection.
  const inUse = (db: string) => {
    const id = openConnFor(conns, root.connId, db);
    return id === undefined ? 0 : tabs.filter((tb) => tb.connId === id).length;
  };
  return (
    <>
      <div className="conn-page-tool">
        <input
          ref={filterRef}
          className="conn-page-filter"
          value={filter}
          placeholder={t("filter {n} {what}…", { n: names.length, what: t("databases") })}
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          onChange={(e) => setFilter(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && filter !== "") setFilter("");
            if (e.key === "Enter" && visible.length > 0) {
              if (visible.length === 1) onEnter(visible[0]);
              else setSelected(visible[0]);
            }
          }}
        />
        <span className="conn-page-count">
          {f !== "" ? t("{n} of {tot}", { n: visible.length, tot: names.length }) : ""}
        </span>
        <div className="spacer" />
        <button
          className="btn small"
          onClick={onRefresh}
          disabled={refreshing}
          title={t("Reload databases and tables from the server")}
        >
          {refreshing ? <span className="spinner" aria-hidden /> : <IconRefresh />} {t("Refresh")}
        </button>
      </div>
      {error && (
        <CatalogError
          className="page"
          label={t("database unavailable")}
          error={error.error}
          retrying={false}
          onRetry={() => onEnter(error.db)}
        />
      )}
      {query.isError && (
        <CatalogError
          className="page"
          label={t("list unavailable")}
          error={query.error}
          retrying={query.isFetching}
          onRetry={() => void query.refetch()}
        />
      )}
      <div className="conn-page-body">
        <div className="conn-page-list">
          <table className="conn-page-table">
            <thead>
              <tr>
                <th>{t("Database")}</th>
                <th>{t("In use")}</th>
              </tr>
            </thead>
            <tbody>
              {visible.map((n) => {
                const use = inUse(n);
                return (
                  <tr
                    key={n}
                    className={n === selected ? "sel" : ""}
                    onClick={() => setSelected(n)}
                    onDoubleClick={() => onEnter(n)}
                  >
                    <td className="name">
                      {n}
                      {n === mine && <span className="conn-page-cur">{t("profile")}</span>}
                    </td>
                    <td>{use > 0 && <span className="conn-page-use">{t("{n} tabs", { n: use })}</span>}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          {query.isLoading && <div className="tree-note">{t("loading…")}</div>}
          {!query.isLoading && visible.length === 0 && (
            <div className="tree-note">
              {names.length === 0 && hasDbFilter(root)
                ? t("The profile's filter hides every database: change it in Profile…")
                : t("no results")}
            </div>
          )}
        </div>
        <aside className="conn-page-side">
          {selected !== null ? (
            <>
              <h3>{selected}</h3>
              <div className="conn-page-side-actions">
                <button className="btn small primary" onClick={() => onEnter(selected)}>
                  {t("Open database")}
                </button>
              </div>
            </>
          ) : (
            <div className="conn-page-hint">
              <p>
                <b>{t("Double click")}</b> = {t("opens the database: its schemas and tables, on a connection of its own.")}
              </p>
              <p>{t("With many databases: type in the filter and press ↵ to select the first one.")}</p>
            </div>
          )}
        </aside>
      </div>
    </>
  );
}
