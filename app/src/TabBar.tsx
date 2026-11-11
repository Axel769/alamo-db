// Single tab bar (§9.5): heterogeneous tabs (SQL, table) in the same
// strip, grouped by connection (profile label and colour); inside the group
// a block per database, its SQL on the left and its tables on the right with
// a separator. The label
// stays at the left edge while the strip scrolls; a marker with the
// database/schema opens every run of adjacent tabs that work on it, and stays
// beside the label while its run passes under it; a click on it opens the
// connection page on that database. Preview in
// italics, double click = permanent, middle click = close. Overflow: the
// strip scrolls and the ▾ button lists all the tabs; pin and "close
// others" from the context menu. Drag to reorder, inside the connection
// group.
import { useQuery } from "@tanstack/react-query";
import { CSSProperties, Fragment, useEffect, useLayoutEffect, useRef, useState } from "react";
import { t } from "./i18n";
import { keyHint, withKey } from "./keymap";
import { canSwitchDb, engineShortName } from "./engines";
import { ipc } from "./ipc";
import { ConnUi, tabDatabase, tabGroup, TabUi, useStore } from "./store";
import {
  activateTab,
  closeGroupTabs,
  closeOthers,
  closeTabsGuarded,
  closeToSide,
  countToSide,
  groupSideIds,
  makePermanent,
  moveTab,
  newSqlTabFromContext,
  newSqlTabOn,
  openDatabasePage,
  togglePin,
} from "./tabs";
import { IconChevronDown, IconDb, IconPin, IconPlus, IconSql, IconTable, IconX } from "./icons";
import { connColor } from "./connColor";

// Context menu: on a tab (close / close others / to the right / to the
// left / pin), on the connection label, which behaves like a tab (close
// its tabs / the others / to the right / to the left), or on a database
// marker (new query on it / close its tabs).
type Menu = { x: number; y: number } & ({ tabId: string } | { gid: number } | { dbGid: number; db: string });

function menuAt(e: React.MouseEvent): { x: number; y: number } {
  e.preventDefault();
  e.stopPropagation();
  return { x: e.clientX, y: e.clientY };
}

// Drag to reorder: pointer events, not HTML5 drag and drop — the webview's
// native drop handling (files) would swallow it. `target` is the tab the
// indicator sits next to; empty when the pointer is over no valid slot.
// `same` = the slot is where the tab already is: nothing to show.
type Drag = { id: string; target: string; side: "before" | "after"; same: boolean };
const DRAG_THRESHOLD_PX = 4;

/// Where a tab dragged to `x` would land: among the tabs of its group only,
/// so groups stay contiguous. Across blocks too: an SQL tab moved to another
/// database goes back to that database's block by hand.
/// Before the first tab whose middle is past the pointer, otherwise after
/// the last one.
function dropSlot(strip: HTMLElement, id: string, x: number): Drag {
  const tabs = useStore.getState().tabs;
  const me = tabs.find((t) => t.id === id);
  const none: Drag = { id, target: "", side: "before", same: true };
  if (!me) return none;
  const peers = tabs.filter((t) => tabGroup(t) === tabGroup(me));
  const others = peers.filter((t) => t.id !== id);
  if (others.length === 0) return none;
  const rect = (t: TabUi) =>
    strip.querySelector(`[data-tab-id="${CSS.escape(t.id)}"]`)?.getBoundingClientRect();
  let slot = others.length;
  for (let i = 0; i < others.length; i++) {
    const r = rect(others[i]);
    if (r && x < r.left + r.width / 2) {
      slot = i;
      break;
    }
  }
  // `slot` = index among the others the tab would take; it already has it
  // when it is the one just before that index in the current order.
  const same = peers.indexOf(me) === slot;
  return slot < others.length
    ? { id, target: others[slot].id, side: "before", same }
    : { id, target: others[others.length - 1].id, side: "after", same };
}

/// Sticky by hand for the database markers: the marker of the run passing
/// under the connection label stays beside it until the run's last tab
/// pushes it away. CSS sticky would need a wrapper per run, and a wrapper
/// stops its tabs from shrinking. Measures layout positions, so the
/// marker's own shift is taken out.
function pinMarkers(strip: HTMLElement): void {
  let labelRight = -Infinity;
  let marker: HTMLElement | null = null;
  let markerLeft = 0;
  let runRight = 0;
  const place = () => {
    if (!marker) return;
    const dx = Math.max(0, Math.min(labelRight, runRight - marker.offsetWidth) - markerLeft);
    marker.style.translate = dx > 0 ? `${dx}px` : "";
    marker.classList.toggle("pinned", dx > 0);
    marker = null;
  };
  for (const el of strip.children) {
    if (!(el instanceof HTMLElement) || el.classList.contains("tab-sep")) continue;
    const r = el.getBoundingClientRect();
    if (el.classList.contains("tab")) {
      runRight = r.right;
      continue;
    }
    place();
    if (el.classList.contains("tab-group")) labelRight = r.right;
    if (el.classList.contains("tab-db")) {
      marker = el;
      markerLeft = r.left - (parseFloat(el.style.translate) || 0);
      runRight = r.right;
    }
  }
  place();
}

/// What covers the left part of the strip for a tab: its group's label and,
/// if it is pinned beside it, the marker of the tab's run.
function coverRight(tab: HTMLElement): number {
  let marker: HTMLElement | null = null;
  let el = tab.previousElementSibling;
  while (el && !el.classList.contains("tab-group")) {
    if (!marker && el.classList.contains("tab-db")) marker = el as HTMLElement;
    el = el.previousElementSibling;
  }
  if (!el) return -Infinity;
  const labelRight = el.getBoundingClientRect().right;
  if (!marker) return labelRight;
  const left = marker.getBoundingClientRect().left - (parseFloat(marker.style.translate) || 0);
  return left < labelRight ? labelRight + marker.offsetWidth : labelRight;
}

export default function TabBar() {
  const tabs = useStore((s) => s.tabs);
  const active = useStore((s) => s.activeTabId);
  const connections = useStore((s) => s.connections);
  const connPage = useStore((s) => s.connPage);
  // Same query key as the connections panel: the profile name (and colour)
  // is needed for the disconnected groups too.
  const profiles = useQuery({ queryKey: ["profiles"], queryFn: ipc.profilesList });
  // Groups in the order they appeared in the bar (store.groupOrder): a
  // connection keeps its place even when it opens the first tab or closes
  // the last — no reordering under the mouse.
  const groupOrder = useStore((s) => s.groupOrder);
  const groups = groupOrder
    .map((gid) => ({
      gid,
      conn: connections.find((c) => c.parent === null && c.profile.id === gid) as ConnUi | undefined,
      tabs: tabs.filter((tab) => tabGroup(tab) === gid),
    }))
    .filter((g) => g.conn !== undefined || g.tabs.length > 0)
    .map((g) => {
      const profile = g.conn?.profile ?? profiles.data?.find((p) => p.id === g.gid);
      const subs = g.tabs.map((tab) => tabDatabase(tab, canSwitchDb(profile?.dialect), g.conn?.currentDb));
      return { ...g, profile, subs, short: shortNames(subs) };
    });
  // Database of every tab, for the ▾ list: two "gt_events" on different
  // databases must be told apart there too.
  const contextOf = new Map(groups.flatMap((g) => g.tabs.map((tab, i) => [tab.id, g.subs[i]] as const)));
  const stripRef = useRef<HTMLDivElement | null>(null);
  const [menu, setMenu] = useState<Menu | null>(null);
  const [listOpen, setListOpen] = useState(false);
  const [overflowing, setOverflowing] = useState(false);
  const [drag, setDrag] = useState<Drag | null>(null);
  // Mirror of `drag` for the pointerup handler (closures see stale state).
  const dragRef = useRef<Drag | null>(null);
  // A drag ends with a click on the dragged tab (the pointer is captured):
  // that click must not activate it.
  const justDragged = useRef(false);

  const startDrag = (e: React.PointerEvent, id: string) => {
    const strip = stripRef.current;
    if (e.button !== 0 || !strip || (e.target as HTMLElement).closest(".tab-close")) return;
    const el = e.currentTarget as HTMLElement;
    const { pointerId, clientX: x0, clientY: y0 } = e;
    let moving = false;
    const update = (d: Drag | null) => {
      dragRef.current = d;
      setDrag(d);
    };
    const onMove = (ev: PointerEvent) => {
      if (!moving) {
        if (Math.hypot(ev.clientX - x0, ev.clientY - y0) < DRAG_THRESHOLD_PX) return;
        moving = true;
        justDragged.current = true;
        // Capture: the move goes on even outside the window. Synthetic
        // events (tests) have no active pointer: the capture fails, the
        // drag works anyway.
        try {
          el.setPointerCapture(pointerId);
        } catch {}
      }
      update(dropSlot(strip, id, ev.clientX));
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onUp);
      const d = dragRef.current;
      if (d && d.target && !d.same) moveTab(d.id, d.target, d.side);
      update(null);
      // The click fires right after the pointerup, before any timer.
      window.setTimeout(() => (justDragged.current = false), 0);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onUp);
  };

  // The overflow button only appears when really needed (§9.5): the strip
  // scrolls, or a tab shrank enough to cut its title — the ▾ list is where
  // the full names are.
  useEffect(() => {
    const el = stripRef.current;
    if (!el) return;
    const check = () =>
      setOverflowing(
        el.scrollWidth > el.clientWidth + 1 ||
          [...el.querySelectorAll<HTMLElement>(".tab .tab-title > span")].some((t) => t.scrollWidth > t.clientWidth + 1)
      );
    check();
    const ro = new ResizeObserver(check);
    ro.observe(el);
    return () => ro.disconnect();
  }, [tabs]);

  // The activated tab (from MRU, dropdown, closing) must be visible, and
  // not under its group's label, which stays at the left edge (sticky), nor
  // under the database marker pinned beside it.
  useLayoutEffect(() => {
    const strip = stripRef.current;
    const el = strip?.querySelector<HTMLElement>(`[data-tab-id="${CSS.escape(active)}"]`);
    if (!strip || !el) return;
    el.scrollIntoView({ inline: "nearest", block: "nearest" });
    const covered = coverRight(el) - el.getBoundingClientRect().left;
    if (covered > 0) strip.scrollLeft -= covered;
  }, [active]);

  // Markers follow every scroll and every change of the bar.
  useLayoutEffect(() => {
    if (stripRef.current) pinMarkers(stripRef.current);
  });
  useEffect(() => {
    const strip = stripRef.current;
    if (!strip) return;
    const pin = () => pinMarkers(strip);
    strip.addEventListener("scroll", pin, { passive: true });
    const ro = new ResizeObserver(pin);
    ro.observe(strip);
    return () => {
      strip.removeEventListener("scroll", pin);
      ro.disconnect();
    };
  }, []);

  // Any click closes menu and dropdown.
  useEffect(() => {
    if (!menu && !listOpen) return;
    const close = () => {
      setMenu(null);
      setListOpen(false);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
    };
  }, [menu, listOpen]);

  const menuTab = menu && "tabId" in menu ? tabs.find((t) => t.id === menu.tabId) : undefined;
  const menuGroup = menu && "gid" in menu ? groups.find((g) => g.gid === menu.gid) : undefined;
  const groupClosable = menuGroup ? menuGroup.tabs.filter((t) => !t.pinned && !t.running).length : 0;
  const menuDb = menu && "dbGid" in menu ? menu : undefined;
  const dbGroup = menuDb && groups.find((g) => g.gid === menuDb.dbGid);
  const dbClosable = dbGroup
    ? dbGroup.tabs.filter((tab, i) => dbGroup.subs[i] === menuDb.db && !tab.pinned && !tab.running).map((tab) => tab.id)
    : [];

  return (
    // On the bar the webview's native menu makes no sense: either ours,
    // or nothing.
    <div className="tabbar-row" onContextMenu={(e) => e.preventDefault()}>
      <div className={"tabbar" + (drag ? " dragging" : "")} role="tablist" ref={stripRef}>
        {groups.map((g) => {
          const { conn, profile, subs } = g;
          const style = { "--conn": connColor(profile) } as CSSProperties;
          return (
            <Fragment key={g.gid}>
              {/* The label is there even without tabs: it is the way to the
                  connection page, and says it is open. */}
              <button
                className={
                  "tab-group" +
                  (conn ? "" : " off") +
                  (profile?.is_production ? " prod" : "") +
                  (conn && connPage === conn.connId ? " active" : "")
                }
                style={style}
                // The label is cut at 260 px: the whole name first.
                title={`${profile?.name ?? t("no profile")}\n${
                  conn
                    ? `${profile?.username}@${profile?.host}:${profile?.port} — ${t("databases and tables of the connection")}`
                    : t("Connection closed: its tabs remain, disconnected")
                }`}
                onClick={() =>
                  conn ? useStore.getState().openConnPage(conn.connId) : activateTab(g.tabs[0].id)
                }
                onContextMenu={(e) => setMenu({ gid: g.gid, ...menuAt(e) })}
              >
                <span className="tab-group-dot" />
                <span className="tab-text">
                  <span className="tab-title" dir="auto">
                    {profile?.name ?? t("no profile")}
                  </span>
                  <span className="tab-sub">
                    {profile ? engineShortName(profile.dialect) : ""}
                    {profile?.is_production ? " · PROD" : ""}
                    {!conn ? " · " + t("disconnected") : ""}
                  </span>
                </span>
              </button>
              {g.tabs.map((tab, i) => (
                <Fragment key={tab.id}>
                  {i > 0 && tab.kind === "table" && g.tabs[i - 1].kind === "sql" && (
                    <span className="tab-sep" aria-hidden />
                  )}
                  {subs[i] && subs[i] !== subs[i - 1] && (
                    // Like the label one level down: opens the connection
                    // page on this database's tables.
                    <button
                      className="tab-db"
                      style={style}
                      title={`${subs[i]} — ${t("tables of the database")}`}
                      onClick={() => (conn ? openDatabasePage(conn.connId, subs[i]) : activateTab(tab.id))}
                      onContextMenu={(e) => setMenu({ dbGid: g.gid, db: subs[i], ...menuAt(e) })}
                    >
                      <IconDb />
                      <span className="tab-db-name" dir="auto">
                        {g.short.get(subs[i])}
                      </span>
                    </button>
                  )}
                  <Tab
                    tab={tab}
                    style={style}
                    context={subs[i]}
                    active={connPage === null && tab.id === active}
                    dragging={drag?.id === tab.id}
                    drop={drag && !drag.same && drag.target === tab.id ? drag.side : null}
                    onMenu={(e) => setMenu({ tabId: tab.id, ...menuAt(e) })}
                    onPointerDown={(e) => startDrag(e, tab.id)}
                    onActivate={() => {
                      if (!justDragged.current) activateTab(tab.id);
                    }}
                  />
                </Fragment>
              ))}
            </Fragment>
          );
        })}
        <button
          className="tab-new"
          title={withKey(t("New SQL tab (from a table tab it inherits the database)"), "tab.new_query")}
          onClick={() => newSqlTabFromContext()}
        >
          <IconPlus />
        </button>
      </div>
      {overflowing && (
        <button
          className="tab-overflow"
          title={t("All tabs")}
          onMouseDown={(e) => e.stopPropagation()}
          onClick={() => setListOpen((v) => !v)}
        >
          <IconChevronDown />
        </button>
      )}
      {listOpen && (
        <div className="tab-list" onMouseDown={(e) => e.stopPropagation()}>
          {tabs.map((t) => (
            <button
              key={t.id}
              className={"tab-list-item" + (t.id === active ? " active" : "")}
              onClick={() => {
                activateTab(t.id);
                setListOpen(false);
              }}
            >
              <span className="tab-list-kind">{t.kind === "table" ? <IconTable /> : <IconSql />}</span>
              <span className={"tab-list-title" + (t.preview ? " preview" : "")} dir="auto">
                {t.title}
              </span>
              {contextOf.get(t.id) && <span className="tab-list-db">{contextOf.get(t.id)}</span>}
              {t.pinned && <span className="tab-pin"><IconPin /></span>}
              {t.running && <span className="tab-running">●</span>}
            </button>
          ))}
        </div>
      )}
      {menu && menuTab && (
        <div
          className="tab-menu"
          style={{ left: menu.x, top: menu.y }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <button
            className="tab-menu-item"
            onClick={() => {
              togglePin(menuTab.id);
              setMenu(null);
            }}
          >
            {menuTab.pinned ? t("Unpin") : t("Pin")}
          </button>
          <div className="tab-menu-sep" />
          <button
            className="tab-menu-item"
            disabled={menuTab.running}
            onClick={() => {
              void closeTabsGuarded([menuTab.id]);
              setMenu(null);
            }}
          >
            {t("Close")}
            {!menuTab.pinned && <span className="tab-menu-key">{keyHint("tab.close")}</span>}
          </button>
          <button
            className="tab-menu-item"
            disabled={tabs.filter((x) => x.id !== menuTab.id && !x.pinned && !x.running).length === 0}
            onClick={() => {
              closeOthers(menuTab.id);
              setMenu(null);
            }}
          >
            {t("Close others")}
          </button>
          <button
            className="tab-menu-item"
            disabled={countToSide(menuTab.id, "right") === 0}
            onClick={() => {
              closeToSide(menuTab.id, "right");
              setMenu(null);
            }}
          >
            {t("Close tabs to the right")}
          </button>
          <button
            className="tab-menu-item"
            disabled={countToSide(menuTab.id, "left") === 0}
            onClick={() => {
              closeToSide(menuTab.id, "left");
              setMenu(null);
            }}
          >
            {t("Close tabs to the left")}
          </button>
        </div>
      )}
      {menu && menuGroup && (
        <div
          className="tab-menu"
          style={{ left: menu.x, top: menu.y }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <button
            className="tab-menu-item"
            disabled={groupClosable === 0}
            onClick={() => {
              closeGroupTabs(menuGroup.gid);
              setMenu(null);
            }}
          >
            {t("Close the connection's tabs")}
          </button>
          <div className="tab-menu-sep" />
          {(
            [
              ["others", t("Close others")],
              ["right", t("Close tabs to the right")],
              ["left", t("Close tabs to the left")],
            ] as const
          ).map(([side, label]) => {
            const ids = groupSideIds(menuGroup.gid, side);
            return (
              <button
                key={side}
                className="tab-menu-item"
                disabled={ids.length === 0}
                onClick={() => {
                  void closeTabsGuarded(ids);
                  setMenu(null);
                }}
              >
                {label}
              </button>
            );
          })}
        </div>
      )}
      {menuDb && dbGroup && (
        <div
          className="tab-menu"
          style={{ left: menuDb.x, top: menuDb.y }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <button
            className="tab-menu-item"
            disabled={!dbGroup.conn}
            onClick={() => {
              if (dbGroup.conn) newSqlTabOn(dbGroup.conn.connId, menuDb.db);
              setMenu(null);
            }}
          >
            {t("New query on this database")}
          </button>
          <div className="tab-menu-sep" />
          <button
            className="tab-menu-item"
            disabled={dbClosable.length === 0}
            onClick={() => {
              void closeTabsGuarded(dbClosable);
              setMenu(null);
            }}
          >
            {t("Close the database's tabs")}
          </button>
        </div>
      )}
    </div>
  );
}

/// Name the marker shows: the "_"-separated words that all the databases of
/// the group share at the start and at the end are dropped
/// (g_transport_brivioevigano_test, g_transport_sacchi_test → brivioevigano,
/// sacchi). With a single database there is nothing to tell apart: full
/// name. The tooltip and the connection page keep the full one.
function shortNames(contexts: string[]): Map<string, string> {
  const uniq = [...new Set(contexts.filter((c) => c))];
  const out = new Map(uniq.map((c) => [c, c]));
  if (uniq.length < 2) return out;
  const words = uniq.map((c) => c.split("_"));
  const min = Math.min(...words.map((w) => w.length));
  const same = (at: (w: string[]) => string) => words.every((w) => at(w) === at(words[0]));
  let pre = 0;
  while (pre < min - 1 && same((w) => w[pre])) pre++;
  let suf = 0;
  while (pre + suf < min - 1 && same((w) => w[w.length - 1 - suf])) suf++;
  uniq.forEach((c, i) => out.set(c, words[i].slice(pre, words[i].length - suf).join("_")));
  return out;
}

// `tab` is NOT destructured as `t`: it would shadow i18n's `t` with the
// tab object — t("…") would become a call on an object and the render crash
// leaves the window black (it really happened).
/// A shrunk tab cuts its title in the middle (gt_…routes): the tables of a
/// database often share a prefix, the end is what tells them apart. The
/// tail is the last "_" word when short, otherwise the last 6 characters —
/// counted in widths: a CJK ideogram is two, and six of them would not fit
/// the tab floor; a head shorter than 3 characters is not worth a cut.
function titleParts(title: string): [string, string] {
  const i = title.lastIndexOf("_");
  const cut = i > 0 && title.length - i - 1 <= 10 ? i + 1 : title.length - tailLength(title);
  return cut < 3 ? ["", title] : [title.slice(0, cut), title.slice(cut)];
}

const WIDE = /[\u1100-\u115f\u2e80-\ua4cf\uac00-\ud7a3\uf900-\ufaff\ufe30-\ufe4f\uff00-\uff60\uffe0-\uffe6]/;

/// How many trailing characters make about six narrow ones.
function tailLength(title: string): number {
  let width = 0;
  let n = 0;
  while (n < title.length && width < 6) {
    width += WIDE.test(title[title.length - 1 - n]) ? 2 : 1;
    n++;
  }
  return n;
}

function Tab({
  tab,
  active,
  context,
  style,
  dragging,
  drop,
  onMenu,
  onPointerDown,
  onActivate,
}: {
  tab: TabUi;
  active: boolean;
  /// Database/schema of the tab, for the tooltip (the marker before the run
  /// shows it, maybe shortened).
  context: string;
  style: CSSProperties;
  dragging: boolean;
  drop: "before" | "after" | null;
  onMenu: (e: React.MouseEvent) => void;
  onPointerDown: (e: React.PointerEvent) => void;
  onActivate: () => void;
}) {
  const [head, tail] = titleParts(tab.title);
  // A title shorter than the floor of a full bar ("…services"): the tab
  // fits its text instead of leaving slack between the title and the ×.
  const short = tab.title.length <= 8 && !WIDE.test(tab.title);
  return (
    <div
      role="tab"
      aria-selected={active}
      data-tab-id={tab.id}
      className={
        "tab" +
        (active ? " active" : "") +
        (short ? " short" : "") +
        (tab.preview ? " preview" : "") +
        (tab.connId === null ? " off" : "") +
        (dragging ? " dragging" : "") +
        (drop ? ` drop-${drop}` : "")
      }
      style={style}
      onPointerDown={onPointerDown}
      onClick={onActivate}
      onDoubleClick={() => makePermanent(tab.id)}
      onContextMenu={onMenu}
      onAuxClick={(e) => {
        if (e.button === 1 && !tab.pinned) void closeTabsGuarded([tab.id]);
      }}
      title={
        tab.kind === "table" && tab.table
          ? `${tab.table.schema}.${tab.table.name}`
          : context
            ? `${tab.title} · ${context}`
            : tab.title
      }
    >
      <span className="tab-icon">{tab.kind === "table" ? <IconTable /> : <IconSql />}</span>
      <span className="tab-text">
        <span className="tab-title" dir="auto">
          {head && <span className="tab-title-head">{head}</span>}
          <span>{tail}</span>
        </span>
      </span>
      {tab.running ? (
        <span className="tab-running" title={t("running")}>
          ●
        </span>
      ) : tab.pinned ? (
        <span className="tab-pin" title={t("pinned (close from the context menu)")}>
          <IconPin />
        </span>
      ) : (
        <button
          className="tab-close"
          title={withKey(t("Close"), "tab.close")}
          onClick={(e) => {
            e.stopPropagation();
            void closeTabsGuarded([tab.id]);
          }}
        >
          <IconX />
        </button>
      )}
    </div>
  );
}
