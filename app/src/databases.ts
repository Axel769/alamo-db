// The other databases of a PostgreSQL server (§3.3): each one on a
// connection of its own, opened the first time it is used and kept as a
// child of the profile's connection.
import { ipc } from "./ipc";
import { ConnUi, pgDbEntered, profileDatabase, useStore } from "./store";

/// Connection already open on `db` for the profile's connection `rootId`,
/// if any.
export function openConnFor(conns: ConnUi[], rootId: number, db: string): number | undefined {
  return conns.find(
    (c) =>
      (c.connId === rootId && profileDatabase(c) === db) ||
      (c.parent === rootId && c.profile.database === db)
  )?.connId;
}

/// Connection on `db` for the profile's connection `rootId`: the open one,
/// or a new one added to the store.
export async function connForDatabase(rootId: number, db: string): Promise<number> {
  const open = openConnFor(useStore.getState().connections, rootId, db);
  if (open !== undefined) return open;
  const out = await ipc.openDatabase(rootId, db);
  const s = useStore.getState();
  // Opened twice at once (tree and page): the backend gives back the same
  // connection, added once.
  if (!s.connections.some((c) => c.connId === out.conn_id)) {
    s.addConnection({
      connId: out.conn_id,
      profile: out.profile,
      currentDb: db,
      txn: null,
      security: out.security,
      oneDbPerConn: true,
      parent: rootId,
    });
  }
  return out.conn_id;
}

/// Connection page on database `db` of the profile's connection `rootId`:
/// its schemas, on its connection.
export async function openDatabasePageOn(rootId: number, db: string): Promise<void> {
  const id = await connForDatabase(rootId, db);
  if (id === rootId) pgDbEntered.add(rootId);
  useStore.setState((s) => ({ connPage: id, activeConnId: id, connPageNav: s.connPageNav + 1 }));
}
