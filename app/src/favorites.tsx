// Favorites (like HeidiSQL): star on databases/schemas and tables, per
// profile (not per session: the connId changes at every connection).
// Shared by the schema tree and the connection page: same query key,
// same optimistic toggle, so a star set in one place shows in the other.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo } from "react";
import { t } from "./i18n";
import { Favorite, ipc } from "./ipc";
import { IconStar } from "./icons";

/// Favorites of a profile: which is on and how to turn it on/off.
export type Favs = {
  has: (schema: string, table?: string) => boolean;
  /// The schema is a favorite or contains favorite tables.
  touches: (schema: string) => boolean;
  /// The schema contains at least one favorite table.
  hasTables: (schema: string) => boolean;
  toggle: (schema: string, table?: string) => void;
  /// Some favorite (schema or table) lies in a schema matching `pred`.
  some: (pred: (schema: string) => boolean) => boolean;
};

/// Separator of `database` and `schema` in the favorites of a PG server's
/// other databases (§3.3): a character no identifier contains in practice.
const DB_SEP = "\u001f";

/// Favorites of one database of a PG server (§3.3). The profile's database
/// (`db` null) keeps the plain schema names it had before there were
/// several; the others are stored as `database␟schema`, so `public.orders`
/// starred in one database is not starred in all of them.
export function scopeFavs(favs: Favs | null, db: string | null): Favs | null {
  if (!favs || db === null) return favs;
  const prefix = db + DB_SEP;
  return {
    has: (schema, table) => favs.has(prefix + schema, table),
    touches: (schema) => favs.touches(prefix + schema),
    hasTables: (schema) => favs.hasTables(prefix + schema),
    toggle: (schema, table) => favs.toggle(prefix + schema, table),
    some: (pred) => favs.some((s) => s.startsWith(prefix) && pred(s.slice(prefix.length))),
  };
}

/// The database holds favorites ("favorites only" in the tree).
export function dbHasFavs(favs: Favs | null, db: string | null): boolean {
  if (!favs) return false;
  return db === null ? favs.some((s) => !s.includes(DB_SEP)) : favs.some((s) => s.startsWith(db + DB_SEP));
}

const favKey = (schema: string, table?: string | null) =>
  table ? `${schema}\0${table}` : schema;

/// Without a saved profile (null id) there is nowhere to save: no stars.
export function useFavorites(profileId: number | null): Favs | null {
  const qc = useQueryClient();
  const key = ["favorites", profileId];
  const q = useQuery({
    queryKey: key,
    queryFn: () => ipc.favoritesList(profileId!),
    enabled: profileId !== null,
    staleTime: Infinity,
  });
  const set = useMemo(() => new Set(q.data?.map((f) => favKey(f.schema, f.table))), [q.data]);
  const schemas = useMemo(() => new Set(q.data?.map((f) => f.schema)), [q.data]);
  const withTables = useMemo(
    () => new Set(q.data?.filter((f) => f.table !== null).map((f) => f.schema)),
    [q.data],
  );
  // One object per favorites snapshot: the (memoized) nodes of the tree
  // re-render only when a star changes, not at every keystroke.
  return useMemo<Favs | null>(() => {
    if (profileId === null) return null;
    const key = ["favorites", profileId];
    return {
      has: (schema, table) => set.has(favKey(schema, table)),
      touches: (schema) => schemas.has(schema),
      hasTables: (schema) => withTables.has(schema),
      some: (pred) => [...schemas].some(pred),
      toggle: (schema, table) => {
        const on = !set.has(favKey(schema, table));
        const fav: Favorite = { schema, table: table ?? null };
        // Optimistic: the star changes right away; then it is re-read from
        // the DB, which has the last word even if the save fails.
        qc.setQueryData<Favorite[]>(key, (old = []) =>
          on
            ? [...old, fav]
            : old.filter((f) => favKey(f.schema, f.table) !== favKey(schema, table)),
        );
        void ipc
          .favoriteSet(profileId, schema, fav.table, on)
          .catch(() => {})
          .finally(() => void qc.invalidateQueries({ queryKey: key }));
      },
    };
  }, [profileId, qc, set, schemas, withTables]);
}

/// Star of a schema or table. A schema that is not a favorite but holds
/// favorite tables shows an empty star anyway ("partial"): it is listed
/// under "favorites only", and the reader must see why.
export function FavStar({ favs, schema, table }: { favs: Favs | null; schema: string; table?: string }) {
  if (!favs) return null;
  const on = favs.has(schema, table);
  const partial = !on && table === undefined && favs.hasTables(schema);
  return (
    <span
      className={"fav-star" + (on ? " on" : partial ? " partial" : "")}
      title={
        on
          ? t("Remove from favorites")
          : partial
            ? t("Contains favorite tables — click to add the whole one")
            : t("Add to favorites")
      }
      onClick={(e) => {
        e.stopPropagation();
        favs.toggle(schema, table);
      }}
      onDoubleClick={(e) => e.stopPropagation()}
    >
      <IconStar fill={on ? "currentColor" : "none"} />
    </span>
  );
}

