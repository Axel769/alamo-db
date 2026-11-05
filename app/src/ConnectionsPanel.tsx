// Connections panel (M4): the open connections (M3.5) and the saved
// profiles, persistent view next to the tree. Connecting requires the
// password (never on disk, §9.1): clicking a profile opens the dialog
// preselected — no direct connect from here.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { t } from "./i18n";
import { ipc } from "./ipc";
import { familyWrites, profileConns, rootConnId, useStore } from "./store";
import { disconnectConn } from "./tabs";
import { dirtyTabIds, guardUnsaved } from "./pending";
import { IconMore, IconX } from "./icons";
import type { TransferKind } from "./ConnectionsTransfer";
import { connColor } from "./connColor";

type Props = {
  /// Opens ConnectDialog, preselecting the profile if given.
  onOpenDialog: (profileId?: number | null) => void;
  /// Opens the export or import dialog of the saved connections.
  onTransfer: (kind: TransferKind) => void;
};

export default function ConnectionsPanel({ onOpenDialog, onTransfer }: Props) {
  const allConns = useStore((s) => s.connections);
  const connections = profileConns(allConns);
  // The connections to a PG server's other databases count as their profile's (§3.3).
  const activeConnId = rootConnId(allConns, useStore((s) => s.activeConnId));
  const tabs = useStore((s) => s.tabs);
  const qc = useQueryClient();
  // Same query key invalidated by Studio when the dialog closes: profiles
  // created/deleted there show up here without a manual refetch.
  const profiles = useQuery({ queryKey: ["profiles"], queryFn: ipc.profilesList });
  // Disconnecting with pending writes: the first click arms, the second
  // executes (ConnectDialog pattern — no window.confirm). §9.7.2.
  const [armedId, setArmedId] = useState<number | null>(null);
  // Context menu on the open connection: the ✕ only appears on mouse
  // hover, the right click is the second way to disconnect.
  const [menu, setMenu] = useState<{ connId: number; x: number; y: number } | null>(null);
  // ⋯ menu of the header: actions on all profiles (export/import).
  const [headMenu, setHeadMenu] = useState<{ x: number; y: number } | null>(null);

  useEffect(() => {
    if (!menu && !headMenu) return;
    const close = () => {
      setMenu(null);
      setHeadMenu(null);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
    };
  }, [menu, headMenu]);

  /// `false` = only armed (pending writes, the second click is needed).
  const requestDisconnect = async (connId: number, hasWrites: boolean): Promise<boolean> => {
    // §9.7.2: disconnecting with pending writes = lost work. It must be
    // said, not discovered.
    if (hasWrites && armedId !== connId) {
      setArmedId(connId);
      window.setTimeout(() => setArmedId(null), 4000);
      return false;
    }
    setArmedId(null);
    // Unsaved grid edits on its tabs: the question comes first.
    if (!(await guardUnsaved(dirtyTabIds((t) => t.connId === connId)))) return true;
    if (!disconnectConn(connId)) return true;
    // Drop the introspection cache of THIS connection (keys
    // ["...", connId, ...]) — as in Studio.
    qc.removeQueries({ predicate: (q) => q.queryKey[1] === connId });
    if (useStore.getState().connections.length === 0) onOpenDialog(null);
    return true;
  };

  const openProfileIds = new Set(connections.map((c) => c.profile.id));
  const closedProfiles = (profiles.data ?? []).filter((p) => !openProfileIds.has(p.id));

  return (
    <aside className="conn-panel">
      <div className="tree-head">
        <span>{t("Connections")}</span>
        <div className="tree-head-actions">
          <button
            className="btn small"
            title={t("More actions")}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              if (headMenu) return setHeadMenu(null);
              const r = e.currentTarget.getBoundingClientRect();
              setHeadMenu({ x: r.left, y: r.bottom + 4 });
            }}
          >
            <IconMore />
          </button>
          <button className="btn small" title={t("New connection…")} onClick={() => onOpenDialog(null)}>
            +
          </button>
        </div>
      </div>
      <div className="tree-body">
        {connections.length > 0 && <div className="panel-section">{t("open connections")}</div>}
        {connections.map((c) => {
          const running = tabs.some((t) => t.running && rootConnId(allConns, t.connId) === c.connId);
          const armed = armedId === c.connId;
          return (
            <div
              key={c.connId}
              className={
                "conn-item" +
                (c.connId === activeConnId ? " active" : "") +
                (c.profile.is_production ? " prod" : "")
              }
              onContextMenu={(e) => {
                e.preventDefault();
                setMenu({ connId: c.connId, x: e.clientX, y: e.clientY });
              }}
            >
              <button
                className="conn-main"
                // The name is cut by the panel: whole on the first line.
                title={`${c.profile.name}\n${c.profile.username}@${c.profile.host}:${c.profile.port}${
                  c.currentDb ? " · " + c.currentDb : ""
                }`}
                onClick={() => useStore.setState({ activeConnId: c.connId })}
              >
                <span className="swatch" style={{ background: connColor(c.profile) }} />
                <span className="tree-label" dir="auto">
                  {c.profile.name}
                </span>
                {c.profile.is_production && <span className="prod-tag">prod</span>}
                {c.txn && (
                  <span
                    className={"conn-txn" + (c.txn.hasWrites ? " writes" : "")}
                    title={
                      c.txn.hasWrites
                        ? t("Open transaction with uncommitted writes")
                        : t("Open transaction")
                    }
                  />
                )}
              </button>
              <button
                className={"conn-x" + (armed ? " danger" : "")}
                disabled={running}
                title={
                  running
                    ? t("Query running on this connection: cancel it first")
                    : armed
                      ? t("Uncommitted writes — confirm? (disconnecting = ROLLBACK)")
                      : t("Closes the connection \"{name}\"; its tabs remain, detached", {
                          name: c.profile.name,
                        })
                }
                onClick={() => void requestDisconnect(c.connId, familyWrites(allConns, c.connId))}
              >
                <IconX />
              </button>
            </div>
          );
        })}
        <div className="panel-section">{t("profiles")}</div>
        {profiles.isLoading && <div className="tree-note">{t("loading…")}</div>}
        {profiles.isError && <div className="tree-note error">{t("profiles unavailable")}</div>}
        {closedProfiles.map((p) => (
          <div key={p.id} className={"conn-item" + (p.is_production ? " prod" : "")}>
            <button
              className="conn-main"
              title={`${p.name}\n${t("{dest} — connect…", { dest: `${p.username}@${p.host}:${p.port}` })}`}
              onClick={() => onOpenDialog(p.id)}
            >
              <span className="swatch" style={{ background: connColor(p) }} />
              <span className="tree-label" dir="auto">
                {p.name}
              </span>
              {p.is_production && <span className="prod-tag">prod</span>}
            </button>
          </div>
        ))}
        {profiles.data && closedProfiles.length === 0 && (
          <div className="tree-note">
            {connections.length > 0 ? t("all profiles are connected") : t("no saved profiles")}
          </div>
        )}
      </div>
      {headMenu && (
        <div
          className="tab-menu"
          style={{ left: headMenu.x, top: headMenu.y }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <button
            className="tab-menu-item"
            onClick={() => {
              setHeadMenu(null);
              onTransfer("export");
            }}
          >
            {t("Export connections…")}
          </button>
          <button
            className="tab-menu-item"
            onClick={() => {
              setHeadMenu(null);
              onTransfer("import");
            }}
          >
            {t("Import connections…")}
          </button>
        </div>
      )}
      {menu &&
        (() => {
          const c = connections.find((x) => x.connId === menu.connId);
          if (!c) return null;
          const running = tabs.some((t) => t.running && rootConnId(allConns, t.connId) === c.connId);
          return (
            <div
              className="tab-menu"
              style={{ left: menu.x, top: menu.y }}
              onMouseDown={(e) => e.stopPropagation()}
            >
              <button
                className="tab-menu-item"
                disabled={running}
                onClick={() => {
                  void requestDisconnect(c.connId, familyWrites(allConns, c.connId)).then((done) => done && setMenu(null));
                }}
              >
                {armedId === c.connId ? t("Uncommitted writes — confirm?") : t("Disconnect")}
              </button>
            </div>
          );
        })()}
    </aside>
  );
}
