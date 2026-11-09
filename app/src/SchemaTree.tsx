// Schema tree (§10): lazy per node, cache and invalidation in TanStack Query
// (§9.2 — introspection does NOT live in the Zustand store). The queries run
// on the dedicated connection (§3): the tree responds even while an
// execution occupies the sticky one.
//
// M3.5 (DBeaver model): multi-root — one root node per open connection,
// with the profile's color and name. Every query key carries the connId.
//
// Interaction (§9.5): click on a table = preview tab, double click =
// permanent. System schemas stay visible, collapsed at the bottom.
//
// Favorites (like HeidiSQL): star on databases/schemas and tables, per
// profile (not per session: the connId changes at every connection).
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ProdTag } from "./ProdTag";
import { memo, useDeferredValue, useMemo, useState } from "react";
import CatalogError from "./CatalogError";
import { t, useLang } from "./i18n";
import { ColumnInfoOut, ipc, RelationKind, RelationOut } from "./ipc";
import { ConnUi, hasDbFilter, profileConns, profileDatabase, rootConnId, useStore } from "./store";
import { connForDatabase, openConnFor } from "./databases";
import { makePermanent, openTablePreview } from "./tabs";
import { IconChevronDown, IconChevronRight, IconRefresh, IconStar, IconX, RelIcon } from "./icons";
import { connColor } from "./connColor";
import { dbHasFavs, Favs, FavStar, scopeFavs, useFavorites } from "./favorites";
import { VirtualRows } from "./virtualRows";

/// Height of a tree row before it is measured (`.tree-node`: 12.5 px font,
/// 2 px of padding each side).
const ROW_HEIGHT = 19;
const relKey = (r: RelationOut) => r.name;

/// "Favorites only": local preference, remembered across launches.
const FAV_ONLY_KEY = "alamo.treeFavOnly";

function savedFavOnly(): boolean {
  try {
    return localStorage.getItem(FAV_ONLY_KEY) === "1";
  } catch {
    return false;
  }
}

function rememberFavOnly(on: boolean): void {
  try {
    localStorage.setItem(FAV_ONLY_KEY, on ? "1" : "0");
  } catch {
    // Storage unavailable: only the memory of it is lost.
  }
}

function fmtRows(n: number | null): string {
  if (n === null) return "";
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}k`;
  return String(n);
}

export default function SchemaTree() {
  const connections = profileConns(useStore((s) => s.connections));
  const qc = useQueryClient();
  const [filter, setFilter] = useState("");
  const [favOnly, setFavOnly] = useState(savedFavOnly);
  // The input follows the keys right away; the tree (thousands of rows
  // with a big schema) catches up at low priority and skips the steps in
  // between when typing is fast.
  const f = useDeferredValue(filter.trim().toLowerCase());

  if (connections.length === 0) return <aside className="schema-tree" />;

  return (
    <aside className="schema-tree">
      <div className="tree-head">
        <span>{t("Schema")}</span>
        <button
          className="btn small"
          title={t("Reload the tree (the cache is invalidated, nodes reload on demand)")}
          onClick={() => void qc.invalidateQueries()}
        >
          <IconRefresh />
        </button>
      </div>
      <div className="tree-filter">
        <button
          className={"tree-fav-only" + (favOnly ? " on" : "")}
          title={favOnly ? t("Show everything") : t("Show favorites only")}
          aria-pressed={favOnly}
          onClick={() => {
            rememberFavOnly(!favOnly);
            setFavOnly(!favOnly);
          }}
        >
          <IconStar fill={favOnly ? "currentColor" : "none"} />
        </button>
        <input
          value={filter}
          placeholder={t("filter schemas and tables…")}
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          onChange={(e) => setFilter(e.target.value)}
          onKeyDown={(e) => e.key === "Escape" && setFilter("")}
        />
        {filter !== "" && (
          <button className="tree-filter-clear" title={t("Clear the filter")} onClick={() => setFilter("")}>
            <IconX />
          </button>
        )}
      </div>
      <div className="tree-body">
        {connections.map((c) => (
          <ConnRoot
            key={c.connId}
            conn={c}
            filter={f}
            favOnly={favOnly}
            single={connections.length === 1}
          />
        ))}
      </div>
    </aside>
  );
}

/// Root of a connection (M3.5): name and color of the profile, lazy children:
/// the schemas (MySQL: the databases), or on PG the server's databases (§3.3).
function ConnRoot({
  conn,
  filter,
  favOnly,
  single,
}: {
  conn: ConnUi;
  filter: string;
  favOnly: boolean;
  single: boolean;
}) {
  const [open, setOpen] = useState(true);
  const conns = useStore((s) => s.connections);
  const activeConnId = useStore((s) => s.activeConnId);
  const favs = useFavorites(conn.profile.id);
  return (
    <>
      <button
        className={
          "tree-node conn-root" +
          (conn.connId === rootConnId(conns, activeConnId) ? " active" : "") +
          (conn.profile.is_production ? " prod" : "")
        }
        onClick={() => setOpen(!open)}
        title={`${conn.profile.username}@${conn.profile.host}:${conn.profile.port}`}
      >
        <span className="tree-caret">{open ? <IconChevronDown /> : <IconChevronRight />}</span>
        <span className="swatch" style={{ background: connColor(conn.profile) }} />
        <span className="tree-label" dir="auto">
          {conn.profile.name}
        </span>
        {conn.profile.is_production && <ProdTag />}
      </button>
      {open &&
        (conn.oneDbPerConn ? (
          <DatabaseList root={conn} filter={filter} favOnly={favOnly} favs={favs} />
        ) : (
          <SchemaList connId={conn.connId} filter={filter} favOnly={favOnly} single={single} favs={favs} />
        ))}
    </>
  );
}

/// PG (§3.3): the server's databases, like DBeaver. The profile's one is
/// open from the start; the others open their connection when expanded.
function DatabaseList({
  root,
  filter,
  favOnly,
  favs,
}: {
  root: ConnUi;
  filter: string;
  favOnly: boolean;
  favs: Favs | null;
}) {
  const dbs = useQuery({
    queryKey: ["databases", root.connId],
    queryFn: () => ipc.serverDatabases(root.connId),
    staleTime: Infinity,
  });
  const mine = profileDatabase(root);
  return (
    <>
      {dbs.isLoading && <div className="tree-note nested">{t("loading…")}</div>}
      {dbs.data?.length === 0 && hasDbFilter(root) && (
        <div className="tree-note nested">{t("The profile's filter hides every database: change it in Profile…")}</div>
      )}
      {dbs.isError && (
        <CatalogError
          className="nested"
          label={t("databases unavailable")}
          error={dbs.error}
          retrying={dbs.isFetching}
          onRetry={() => void dbs.refetch()}
        />
      )}
      {dbs.data?.map((d) => (
        <DatabaseNode
          key={d.name}
          root={root}
          name={d.name}
          mine={d.name === mine}
          filter={filter}
          favOnly={favOnly}
          favs={favs}
        />
      ))}
    </>
  );
}

function DatabaseNode({
  root,
  name,
  mine,
  filter,
  favOnly,
  favs,
}: {
  root: ConnUi;
  name: string;
  /// The profile's database: on the profile's connection, open at start.
  mine: boolean;
  filter: string;
  favOnly: boolean;
  favs: Favs | null;
}) {
  const [open, setOpen] = useState(mine);
  const connId = useStore((s) => openConnFor(s.connections, root.connId, name));
  const [opening, setOpening] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const dbFavs = useMemo(() => scopeFavs(favs, mine ? null : name), [favs, mine, name]);
  const connect = () => {
    setOpening(true);
    setError(null);
    connForDatabase(root.connId, name)
      .catch(setError)
      .finally(() => setOpening(false));
  };
  // With a filter or "favorites only" a database counts if its name matches,
  // or if it is open (its schemas and tables are searched); the closed ones
  // are never opened by a search: on a server with dozens of databases it
  // would open dozens of connections.
  const live = connId !== undefined;
  if (favOnly && !dbHasFavs(favs, mine ? null : name)) return null;
  if (filter !== "" && !name.toLowerCase().includes(filter) && !live) return null;
  const effOpen = open || ((filter !== "" || favOnly) && live);
  return (
    <>
      <button
        className="tree-node database"
        title={name}
        onClick={() => {
          if (!open && !live) connect();
          setOpen(!open);
        }}
      >
        <span className="tree-caret">{effOpen ? <IconChevronDown /> : <IconChevronRight />}</span>
        <span className="tree-label" dir="auto">
          {name}
        </span>
      </button>
      {effOpen && (
        <div className="tree-db">
          {opening && <div className="tree-note nested">{t("connecting…")}</div>}
          {error !== null && !opening && (
            <CatalogError
              className="nested"
              label={t("database unavailable")}
              error={error}
              retrying={false}
              onRetry={connect}
            />
          )}
          {live && <SchemaList connId={connId} filter={filter} favOnly={favOnly} single={mine} favs={dbFavs} />}
        </div>
      )}
    </>
  );
}

/// Schemas of a connection (MySQL: the databases), with the system ones
/// folded at the bottom.
function SchemaList({
  connId,
  filter,
  favOnly,
  single,
  favs,
}: {
  connId: number;
  filter: string;
  favOnly: boolean;
  /// The only open connection: a lone user schema opens by itself.
  single: boolean;
  favs: Favs | null;
}) {
  const [systemOpen, setSystemOpen] = useState(false);
  const schemas = useQuery({
    queryKey: ["schemas", connId],
    queryFn: () => ipc.schemaList(connId),
    staleTime: Infinity,
  });

  const user = schemas.data?.filter((s) => !s.is_system) ?? [];
  const system = schemas.data?.filter((s) => s.is_system) ?? [];
  const filtering = filter !== "";
  // With a filter the table names of every schema are searched: one round
  // trip for the whole server (names and kinds only, cheap for the server)
  // instead of one query per schema serialized on the introspection
  // connection — tens of seconds over an SSH tunnel with dozens of
  // databases. Kept apart from the per-schema cache, which holds the full
  // rows (estimates, comments) loaded when a schema is opened. If this
  // query fails, the nodes fall back to their own query.
  const all = useQuery({
    queryKey: ["relations-all", connId],
    queryFn: () => ipc.schemaRelationsAll(connId),
    enabled: filtering,
    staleTime: Infinity,
  });
  const search = useMemo(() => {
    if (!all.data) return undefined;
    const bySchema = new Map<string, RelationOut[]>();
    for (const r of all.data) {
      const list = bySchema.get(r.schema);
      if (list) list.push(r);
      else bySchema.set(r.schema, [r]);
    }
    return bySchema;
  }, [all.data]);
  const searching = filtering && all.isLoading;
  const EMPTY: RelationOut[] = useMemo(() => [], []);
  /// What the node searches in: the whole-server names once loaded (an
  /// empty list for a schema without tables); nothing while loading or
  /// after a failure (then the node loads its own relations).
  const searchOf = (name: string): RelationOut[] | undefined =>
    search ? (search.get(name) ?? EMPTY) : undefined;
  // Favorites only: the favorite schemas or those with favorite tables,
  // system ones included (no "system" group). Known from the favorites,
  // without loading the relations of all the schemas.
  const favSchemas = favOnly ? [...user, ...system].filter((s) => favs?.touches(s.name)) : [];

  return (
    <>
      {schemas.isLoading && <div className="tree-note nested">{t("loading…")}</div>}
      {schemas.isError && (
        <CatalogError
          className="nested"
          label={t("tree unavailable")}
          error={schemas.error}
          retrying={schemas.isFetching}
          onRetry={() => void schemas.refetch()}
        />
      )}
      {favOnly && schemas.isSuccess && favSchemas.length === 0 && (
        <div className="tree-note nested">{t("no favorites")}</div>
      )}
      {searching && <div className="tree-note nested">{t("searching the tables…")}</div>}
      {favOnly &&
        favSchemas.map((s) => (
          <SchemaNode
            key={s.name}
            connId={connId}
            name={s.name}
            filter={filter}
            searching={searching}
            searchRels={searchOf(s.name)}
            favs={favs}
            favOnly
          />
        ))}
      {!favOnly &&
        user.map((s) => (
          <SchemaNode
            key={s.name}
            connId={connId}
            name={s.name}
            defaultOpen={single && user.length === 1}
            filter={filter}
            searching={searching}
            searchRels={searchOf(s.name)}
            favs={favs}
          />
        ))}
      {system.length > 0 && !filtering && !favOnly && (
        <>
          <button className="tree-node group" onClick={() => setSystemOpen(!systemOpen)}>
            <span className="tree-caret">{systemOpen ? <IconChevronDown /> : <IconChevronRight />}</span>
            <span className="tree-label muted">{t("system")}</span>
          </button>
          {systemOpen &&
            system.map((s) => <SchemaNode key={s.name} connId={connId} name={s.name} filter="" favs={favs} />)}
        </>
      )}
      {filtering &&
        !favOnly &&
        system.map((s) => (
          <SchemaNode
            key={s.name}
            connId={connId}
            name={s.name}
            filter={filter}
            searching={searching}
            searchRels={searchOf(s.name)}
            favs={favs}
          />
        ))}
    </>
  );
}

const SchemaNode = memo(function SchemaNode({
  connId,
  name,
  filter,
  searching = false,
  searchRels,
  favs,
  favOnly = false,
  defaultOpen = false,
}: {
  connId: number;
  name: string;
  filter: string;
  /// The whole-server names query of the root is in flight: the node
  /// waits for it instead of loading its own relations.
  searching?: boolean;
  /// Names of this schema's relations from the whole-server query (no
  /// estimates): searched when the full relations are not loaded.
  searchRels?: RelationOut[];
  favs: Favs | null;
  favOnly?: boolean;
  defaultOpen?: boolean;
}) {
  useLang();
  const [open, setOpen] = useState(defaultOpen);
  const filtering = filter !== "";
  // Favorites only: favorite tables win — a schema with favorite tables
  // shows only those (open as with the text filter), even if it is a
  // favorite itself. Whole only if favorite without favorite tables.
  const favLimited = favOnly && !(favs?.has(name) && !favs.hasTables(name));
  const rels = useQuery({
    queryKey: ["relations", connId, name],
    queryFn: () => ipc.schemaRelations(connId, name),
    // With an active filter the table names come from the whole-server
    // query of the root (one round trip); this one runs only if that failed.
    enabled: open || favLimited || (filtering && !searching && searchRels === undefined),
    staleTime: Infinity,
  });
  const loading = rels.isLoading || (searching && rels.data === undefined);

  // Full rows when loaded (opened schema), else the searched names.
  const data = rels.data ?? (filtering ? searchRels : undefined);
  const base = favLimited ? data?.filter((r) => favs?.has(name, r.name)) : data;
  const selfMatch = name.toLowerCase().includes(filter);
  // If the schema matches, everything inside is shown; otherwise only the
  // matching tables. No match and nothing loading → node hidden.
  const shown =
    filtering && !selfMatch ? base?.filter((r) => r.name.toLowerCase().includes(filter)) : base;
  if (((filtering && !selfMatch) || favLimited) && !loading && (shown?.length ?? 0) === 0)
    return null;

  const effOpen = open || filtering || favLimited;
  return (
    <>
      <button className="tree-node schema" title={name} onClick={() => setOpen(!open)}>
        <span className="tree-caret">{effOpen ? <IconChevronDown /> : <IconChevronRight />}</span>
        <span className="tree-label" dir="auto">
          {name}
        </span>
        {shown && <span className="tree-count">{shown.length}</span>}
        <FavStar favs={favs} schema={name} />
      </button>
      {effOpen && loading && <div className="tree-note nested">…</div>}
      {effOpen && shown?.length === 0 && !loading && (
        <div className="tree-note nested">{t("empty")}</div>
      )}
      {effOpen && shown && (
        <VirtualRows
          items={shown}
          keyOf={relKey}
          rowHeight={ROW_HEIGHT}
          render={(r) => <RelationNode key={r.name} connId={connId} rel={r} favs={favs} />}
        />
      )}
    </>
  );
});

/// Memoized: with a filter that matches thousands of tables the rows whose
/// props did not change are not rendered again at every keystroke.
const RelationNode = memo(function RelationNode({
  connId,
  rel,
  favs,
}: {
  connId: number;
  rel: RelationOut;
  favs: Favs | null;
}) {
  useLang();
  const [open, setOpen] = useState(false);
  return (
    <>
      <div
        className="tree-node rel"
        // A 64-character name is cut by the panel: whole here, then the comment.
        title={rel.comment ? `${rel.name}\n${rel.comment}` : rel.name}
        onClick={() => openTablePreview(connId, rel.schema, rel.name)}
        onDoubleClick={() => {
          const id = openTablePreview(connId, rel.schema, rel.name);
          makePermanent(id);
        }}
      >
        <span
          className="tree-caret"
          onClick={(e) => {
            e.stopPropagation();
            setOpen(!open);
          }}
        >
          {open ? <IconChevronDown /> : <IconChevronRight />}
        </span>
        <span className={"rel-icon " + rel.kind}><RelIcon kind={rel.kind} /></span>
        <span className="tree-label" dir="auto">
          {rel.name}
        </span>
        <span className="tree-count">{fmtRows(rel.estimated_rows)}</span>
        <FavStar favs={favs} schema={rel.schema} table={rel.name} />
      </div>
      {open && <RelationColumns connId={connId} rel={rel} />}
    </>
  );
});

/// Tooltip of a column row: the name and the type are cut by the panel (a
/// 450-character enum, a 64-character name), the default only if short — a
/// 2 KB base64 default is not a tooltip.
function columnTitle(c: ColumnInfoOut): string {
  const head = `${c.name} · ${c.type_display}${c.nullable ? "?" : ""}`;
  if (c.default === null) return head;
  if (c.default.length <= 80) return `${head}\n${t("default: {v}", { v: c.default })}`;
  return `${head}\n${t("default: {n} characters", { n: c.default.length.toLocaleString() })}`;
}

/// Columns of an open table. Separate component: the query (and its
/// observer in the TanStack cache) exists only for the open tables, not
/// one per row of the tree.
function RelationColumns({ connId, rel }: { connId: number; rel: RelationOut }) {
  const cols = useQuery({
    queryKey: ["columns", connId, rel.schema, rel.name],
    queryFn: () => ipc.schemaColumns(connId, rel.schema, rel.name),
    staleTime: Infinity,
  });
  return (
    <>
      {cols.isLoading && <div className="tree-note nested">…</div>}
      {cols.data?.map((c) => (
        <div className="tree-node col" key={c.name} title={columnTitle(c)}>
          <span className="col-pk">{c.is_primary_key ? "⚿" : ""}</span>
          <span className="tree-label mono" dir="auto">
            {c.name}
          </span>
          <span className="col-type">
            {c.type_display}
            {c.nullable ? "?" : ""}
          </span>
        </div>
      ))}
    </>
  );
}
