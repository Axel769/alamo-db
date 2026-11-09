// Connection dialog (§9.1): profiles persisted in SQLite, password never on
// disk in cleartext — asked here or, if saved, it comes from the system
// keychain straight into the backend (§12).
import { useCallback, useEffect, useRef, useState } from "react";
import { ProdTag } from "./ProdTag";
import { t, tMsg } from "./i18n";
import { CertOut, ConnectOut, ErrorOut, ipc, Profile, SshProfile, SshProfileOut, TestOut } from "./ipc";
import { SshPick, sshAuthLabel, sshSummary } from "./SshProfilesDialog";
import { connInitials } from "./ConnectionPage";
import { IconLock, IconLockOpen, IconPlus, IconSettings, IconShieldCheck, IconTrash, IconX } from "./icons";
import logoUrl from "../src-tauri/icons/icon.png";
import { TLS_MODE_ORDER, TlsLevel, tlsMode, verifiesChain } from "./tlsModes";
import { CertFacts, TlsFacts } from "./TlsFacts";
import { useStore } from "./store";
import { connColor } from "./connColor";
import { SETTING_CATEGORIES, SETTING_DEFS, SettingDef, SettingValues } from "./settingsSchema";
import { applyEffective, setting, settingOf, useSettings } from "./settings";

/// The settings a connection profile may override (§16): the overridable
/// categories, in schema order. Phase 1 is the Execution ones.
const OVERRIDABLE_DEFS = SETTING_DEFS.filter((d) =>
  SETTING_CATEGORIES.find((c) => c.id === d.category)?.overridable
);
import { engineName, engineOf, engines, engineShortName } from "./engines";
import type { TransferKind } from "./ConnectionsTransfer";

type Section = "general" | "access" | "security" | "ssh" | "settings";
const SECTIONS: Section[] = ["general", "access", "security", "ssh", "settings"];

/// The list filter matches name or host, case-insensitive.
const matchesFilter = (p: Profile, filter: string): boolean => {
  const f = filter.trim().toLowerCase();
  return !f || p.name.toLowerCase().includes(f) || p.host.toLowerCase().includes(f);
};

/// On the first engine of the build, with its defaults.
const newProfile = (): Profile => ({
  id: null,
  name: t("new profile"),
  dialect: engines()[0].id,
  host: "localhost",
  port: engines()[0].default_port,
  database: "",
  username: engines()[0].default_user,
  sslmode: "prefer",
  ssl_root_cert: null,
  color: null,
  is_production: false,
  read_only: false,
  ssh_profile_id: null,
  db_include: "",
  db_exclude: "",
  settings: {},
});

// Fixed palette, not a color picker: the color is there to tell the
// environments apart at a glance (§9.1), not to decorate.
const COLORS = ["#2563eb", "#16a34a", "#d97706", "#dc2626", "#a02a83", "#6b7280"];

type Props = {
  open: boolean;
  connected: boolean;
  /// Profile to preselect on opening (from the connections panel):
  /// `null` = the first in the list, as always.
  initialProfileId?: number | null;
  onClose: () => void;
  /// M3.5: connecting ADDS a connection, it does not replace the current one.
  onConnected: (out: ConnectOut) => void;
  /// Export/import of the connections: with no open connections this
  /// dialog is the only way, the connections panel cannot be reached.
  onTransfer: (kind: TransferKind) => void;
  /// An export/import dialog is open above: Esc belongs to it, not to us.
  covered: boolean;
  /// Grows at every successful import: the profile list must be re-read.
  profilesVersion: number;
  /// The SSH profiles manager (§12): bastions are created, edited and
  /// deleted there; the SSH tab here only picks one.
  onManageSsh: (id: SshPick) => void;
  /// Settings from here too: at the first start this dialog covers the
  /// gear of the activity bar, and language or theme are wanted before any
  /// connection.
  onSettings: () => void;
  /// The profile picked in the manager at its closing (`v` grows every
  /// time): it goes into the tab if the profile is being edited.
  sshPick: { id: number | null; v: number } | null;
};

/// Longest profile name (the same limit as `storage::profile_save`): a
/// name is a label in a 240 px panel, not a description.
const PROFILE_NAME_MAX = 120;

export default function ConnectDialog({
  open,
  connected,
  initialProfileId = null,
  onClose,
  onConnected,
  onTransfer,
  covered,
  profilesVersion,
  onManageSsh,
  onSettings,
  sshPick,
}: Props) {
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [draft, setDraft] = useState<Profile>(newProfile);
  /// Saved SSH profiles (reusable bastions): the draft references one by
  /// id, null = direct connection.
  const [sshProfiles, setSshProfiles] = useState<SshProfileOut[]>([]);
  const [password, setPassword] = useState("");
  /// Bastion password or key passphrase (depends on ssh_auth):
  /// like the db password, never persisted outside the keychain.
  const [sshSecret, setSshSecret] = useState("");
  const [savePassword, setSavePassword] = useState(false);
  /// There is a keychain entry for the selected profile (§12): drives the
  /// placeholder and the initial state of the checkbox. The password never arrives.
  const [passwordSaved, setPasswordSaved] = useState(false);
  const [error, setError] = useState<ErrorOut | null>(null);
  /// Action to repeat after confirming the fingerprint of a never-seen
  /// bastion (§12): connect or test, on the saved profile `id`.
  const [retry, setRetry] = useState<{ id: number; action: "connect" | "test" } | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [section, setSection] = useState<Section>("general");
  /// A saved profile opens read-only (summary + password +
  /// Connect): to change it you need "Edit profile". So a field
  /// touched by mistake does not end up saved — Connect saves nothing.
  /// The new profile is always in edit mode.
  const [editing, setEditing] = useState(false);
  /// Text typed in the delete confirmation: it must match the
  /// profile name (like deleting a repository).
  const [deleteText, setDeleteText] = useState("");
  /// Filter of the profile list (appears when there are many).
  const [listFilter, setListFilter] = useState("");
  const dialogRef = useRef<HTMLDivElement>(null);
  const filterRef = useRef<HTMLInputElement>(null);
  const itemsRef = useRef<HTMLDivElement>(null);
  /// Keyboard of the profile list, rebuilt at every render (it reads the
  /// current list and draft); the listener below only forwards to it.
  const listKeyRef = useRef<(e: KeyboardEvent) => void>(() => {});
  // Profiles already connected: the list says so, to avoid opening a duplicate.
  // (The selector returns the store's array, stable: a `.map` in
  // here would create a new array on every read and a render loop.)
  const connections = useStore((s) => s.connections);
  const openIds = connections.map((c) => c.profile.id);
  /// Result of the last successful "Test connection"; disappears at the first
  /// change to the profile.
  const [testOk, setTestOk] = useState<TestOut | null>(null);
  /// The root CA as the backend reads it (§12.2), for the path it was read
  /// from: a result for a path that has since changed is not shown.
  const [caInfo, setCaInfo] = useState<{ path: string; cert: CertOut | null; error: string | null } | null>(null);
  const caPath = verifiesChain(draft.sslmode) ? (draft.ssl_root_cert ?? "").trim() : "";
  useEffect(() => {
    if (!caPath) {
      setCaInfo(null);
      return;
    }
    let live = true;
    // Debounced: the path is also typed by hand.
    const timer = setTimeout(() => {
      ipc.inspectRootCert(caPath).then(
        (cert) => live && setCaInfo({ path: caPath, cert, error: null }),
        (e) => live && setCaInfo({ path: caPath, cert: null, error: String(e) }),
      );
    }, 300);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [caPath]);

  const reload = useCallback(async (selectId?: number | null) => {
    const [list, sshList] = await Promise.all([ipc.profilesList(), ipc.sshProfilesList()]);
    setProfiles(list);
    setSshProfiles(sshList);
    if (selectId != null) {
      const p = list.find((x) => x.id === selectId);
      if (p) setDraft({ ...p });
    } else if (list.length > 0) {
      setDraft({ ...list[0] });
    }
  }, []);

  /// `v` of the last `sshPick` applied: each closing of the manager counts once.
  const pickApplied = useRef(0);

  // On profile change and at every opening of the dialog: the checkbox reflects
  // the real state of the keychain — ticking saves, unticking forgets
  // (at the next connect). Reopening on the same profile after a connect that
  // saved or forgot must re-read, not show the old state.
  useEffect(() => {
    let stale = false;
    if (!open) return;
    if (draft.id === null) {
      setPasswordSaved(false);
      setSavePassword(false);
      return;
    }
    ipc
      .profilePasswordPresent(draft.id)
      .then((present) => {
        if (stale) return;
        setPasswordSaved(present.db || present.ssh);
        setSavePassword(present.db || present.ssh);
      })
      .catch(() => {});
    return () => {
      stale = true;
    };
  }, [draft.id, open]);

  // Saved profile: reopened to connect, so on Access (where the
  // password is). New profile: start from the server details.
  useEffect(() => {
    if (open) setSection(draft.id === null ? "general" : "access");
    setTestOk(null);
    setEditing(draft.id === null);
  }, [draft.id, open]);

  useEffect(() => {
    if (!open) return;
    setError(null);
    setPassword("");
    setSshSecret("");
    setConfirmDelete(false);
    void reload(initialProfileId ?? undefined);
  }, [open, initialProfileId, reload]);

  // After an import: lists re-read without touching the profile being edited. At
  // first start (list empty before) the first imported one is selected.
  useEffect(() => {
    if (!open || profilesVersion === 0) return;
    void Promise.all([ipc.profilesList(), ipc.sshProfilesList()]).then(([list, sshList]) => {
      setProfiles(list);
      setSshProfiles(sshList);
      if (draft.id === null && profiles.length === 0 && list.length > 0) setDraft({ ...list[0] });
      // A deleted bastion unlinks the saved profiles in the backend: the
      // one shown read-only must say so too.
      const current = !editing && draft.id !== null ? list.find((p) => p.id === draft.id) : undefined;
      if (current) setDraft({ ...current });
      // Back from the SSH profiles manager: the profile chosen there goes
      // into the tab — only while editing, a look must not change a saved
      // profile. Nothing chosen there (null) leaves the tunnel as it is,
      // unless its bastion is gone.
      if (editing && sshPick && sshPick.v !== pickApplied.current) {
        pickApplied.current = sshPick.v;
        setDraft((d) => {
          const exists = sshList.some((s) => s.id === d.ssh_profile_id);
          return sshPick.id === null && exists ? d : { ...d, ssh_profile_id: sshPick.id };
        });
        setTestOk(null);
      }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [profilesVersion]);

  // Esc closes, like the ✕ — so only once a connection is up: with no
  // connections the dialog is the only way in and does not close.
  useEffect(() => {
    // (The delete confirmation closes with Esc even without connections.)
    if (!open || covered || (!connected && !confirmDelete)) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        // With the delete confirmation open, Esc closes only that.
        if (confirmDelete) setConfirmDelete(false);
        else onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, connected, onClose, confirmDelete, covered]);

  // ⌘F, arrows and Enter on the profile list. Capture: ⌘F must not reach
  // the pages under the dialog (ConnectionPage, SQL tab search).
  useEffect(() => {
    if (!open || covered) return;
    const onKey = (e: KeyboardEvent) => listKeyRef.current(e);
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, covered]);

  // The selected profile stays visible while moving with the arrows.
  useEffect(() => {
    itemsRef.current?.querySelector(".profile-item.active")?.scrollIntoView({ block: "nearest" });
  }, [draft.id]);

  if (!open) return null;

  /// The bastion of the draft, if it still exists.
  const ssh = sshProfiles.find((s) => s.id === draft.ssh_profile_id) ?? null;
  // The database is mandatory only where the engine says so (PG: every
  // connection is born inside a db; MySQL connects without a default schema).
  const engine = engineOf(draft.dialect);
  const dbMissing = engine?.database_required !== false && draft.database.trim() === "";
  const valid =
    draft.name.trim() !== "" &&
    draft.host.trim() !== "" &&
    !dbMissing &&
    draft.username.trim() !== "" &&
    draft.port > 0 &&
    draft.port <= 65535 &&
    (draft.ssh_profile_id === null || ssh !== null);

  const set = (patch: Partial<Profile>) => {
    setDraft((d) => ({ ...d, ...patch }));
    setConfirmDelete(false);
    setTestOk(null);
  };

  // Form sections: the profile has too many fields for a single column, and
  // the advanced ones (TLS, tunnel) must not get in the way of whoever
  // wants only host and password. Label + summary of the state.
  const sectionLabel = (sec: Section): string =>
    sec === "general"
      ? t("General")
      : sec === "access"
        ? t("Access")
        : sec === "security"
          ? t("Security")
          : sec === "ssh"
            ? t("SSH tunnel")
            : t("Settings");
  const sectionBadge = (sec: Section): string | null =>
    sec === "settings"
      ? Object.keys(draft.settings).length > 0
        ? t("on")
        : null
    : sec === "security"
      ? tlsMode(draft.sslmode).short
      : sec === "ssh"
        ? draft.ssh_profile_id !== null
          ? t("on")
          : null
        : sec === "access" && passwordSaved
          ? t("saved")
          : null;
  const invalidIn: Record<Section, boolean> = {
    general:
      draft.name.trim() === "" ||
      draft.host.trim() === "" ||
      dbMissing ||
      !(draft.port > 0 && draft.port <= 65535),
    access: draft.username.trim() === "",
    security: false,
    // Bastion deleted in the manager while this profile pointed at it.
    ssh: draft.ssh_profile_id !== null && ssh === null,
    // Overrides are validated by the backend; the ranges match the window.
    settings: false,
  };
  const friendly = error ? friendlyHint(error.message) : null;

  const save = async (): Promise<number> => {
    const { id, effective } = await ipc.profileSave(draft);
    applyEffective(effective);
    await reload(id);
    return id;
  };

  /// Connect/test error; the never-seen bastion remembers what to repeat.
  const failed = (e: unknown, id: number | null, action: "connect" | "test") => {
    const err = asError(e);
    setError(err);
    setRetry(err.code === "ssh_host_key_unknown" && id !== null ? { id, action } : null);
  };

  const connect = async () => {
    if (!valid || busy) return;
    setBusy(true);
    setError(null);
    let id: number | null = null;
    try {
      // Outside edit mode the SAVED profile is connected, as is.
      id = editing || draft.id === null ? await save() : draft.id;
      const p = await ipc.connectProfile(id, password || null, savePassword && keychainAllowed(), sshSecret || null);
      onConnected(p);
    } catch (e) {
      failed(e, id, "connect");
    } finally {
      setBusy(false);
    }
  };

  /// Tests the connection without opening it in the Studio nor touching the
  /// keychain. The profile is saved first, as for Connect: the backend
  /// works on the saved profile.
  const test = async () => {
    if (!valid || busy) return;
    setBusy(true);
    setError(null);
    setTestOk(null);
    let id: number | null = null;
    try {
      id = editing || draft.id === null ? await save() : draft.id;
      setTestOk(await ipc.testProfile(id, password || null, sshSecret || null));
    } catch (e) {
      failed(e, id, "test");
    } finally {
      setBusy(false);
    }
  };

  /// Leaves edit mode discarding the changes: back to the saved profile.
  const cancelEdit = () => {
    const saved = profiles.find((p) => p.id === draft.id);
    if (saved) setDraft({ ...saved });
    setError(null);
    setTestOk(null);
    setConfirmDelete(false);
    setEditing(false);
  };

  const saveEdit = async () => {
    if (!valid || busy) return;
    setBusy(true);
    setError(null);
    try {
      await save();
      setEditing(false);
    } catch (e) {
      setError(asError(e));
    } finally {
      setBusy(false);
    }
  };

  /// "Trust and save": the backend saves the bastion key and the
  /// action that met it is repeated.
  const trust = async () => {
    if (!retry || !error?.detail || busy) return;
    const { id, action } = retry;
    setRetry(null);
    try {
      await ipc.sshHostKeyTrust(id, error.detail);
    } catch (e) {
      setError(asError(e));
      return;
    }
    if (action === "connect") await connect();
    else await test();
  };

  /// Name of the SAVED profile (not the one being edited in the form): it is
  /// the one that gets deleted, and the one to type to confirm.
  const savedName = profiles.find((p) => p.id === draft.id)?.name ?? draft.name;
  /// How many OTHER saved profiles go through the draft's bastion: the
  /// manager edits it for all of them.
  const sshOthers = ssh ? ssh.used_by.filter((n) => draft.id === null || n !== savedName).length : 0;
  const deleteReady = deleteText.trim() === savedName.trim() && savedName.trim() !== "";

  /// The Delete button only opens the confirmation; this one deletes, and
  /// only with the name typed right.
  const deleteProfile = async () => {
    if (draft.id === null || !deleteReady) return;
    await ipc.profileDelete(draft.id);
    setConfirmDelete(false);
    setDraft(newProfile());
    const list = await ipc.profilesList();
    setProfiles(list);
    if (list.length > 0) setDraft({ ...list[0] });
  };

  const visible = profiles.filter((p) => matchesFilter(p, listFilter));

  const pick = (p: Profile) => {
    setDraft({ ...p });
    setPassword("");
    setError(null);
    setConfirmDelete(false);
  };

  listKeyRef.current = (e) => {
    const target = e.target as HTMLElement;
    if (target !== document.body && !dialogRef.current?.contains(target)) return;
    const mod = e.metaKey || e.ctrlKey;
    if (mod && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "f") {
      if (!filterRef.current) return;
      e.preventDefault();
      e.stopPropagation();
      filterRef.current.focus();
      filterRef.current.select();
      return;
    }
    if (mod || e.altKey || e.shiftKey || confirmDelete) return;
    const inList = target === filterRef.current || target.closest(".profile-item") !== null;
    if (e.key === "Enter" && inList) {
      e.preventDefault();
      if (visible.some((p) => p.id === draft.id)) void connect();
      return;
    }
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    // While editing a profile the arrows belong to its fields (port, menus).
    if (!inList && editing) return;
    const at = visible.findIndex((p) => p.id === draft.id);
    const next =
      e.key === "ArrowDown"
        ? Math.min(at + 1, visible.length - 1)
        : at < 0
          ? visible.length - 1
          : Math.max(at - 1, 0);
    e.preventDefault();
    if (next < 0 || next === at) return;
    pick(visible[next]);
    // Focus on a list item follows the selection.
    if (target.closest(".profile-item")) {
      itemsRef.current?.querySelector<HTMLElement>(`[data-profile-id="${visible[next].id}"]`)?.focus();
    }
  };

  return (
    <div className="dialog-overlay">
      <div className="dialog connect-dialog" ref={dialogRef}>
        <div className="dialog-title">
          <span>
            {t("Connections")}
            <small className="dialog-sub">
              {connected ? t("open another connection or edit a profile") : t("pick a profile to start")}
            </small>
          </span>
          <div className="dialog-title-actions">
            <button className="btn small" title={t("Settings")} onClick={onSettings}>
              <IconSettings />
            </button>
            {connected && (
              <button className="btn small dialog-close" onClick={onClose}>
                <IconX />
              </button>
            )}
          </div>
        </div>
        <div className="dialog-body">
          <div className="profile-list">
            {profiles.length > 6 && (
              <input
                ref={filterRef}
                className="profile-filter"
                value={listFilter}
                placeholder={t("filter {n} profiles…", { n: profiles.length })}
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
                onChange={(e) => {
                  const f = e.target.value;
                  setListFilter(f);
                  // Typing selects the first match, so Enter connects it. Not
                  // while editing: the edits in progress would be lost.
                  if (!editing && !matchesFilter(draft, f)) {
                    const first = profiles.find((p) => matchesFilter(p, f));
                    if (first) pick(first);
                  }
                }}
              />
            )}
            <div className="profile-items" ref={itemsRef}>
              {visible.map((p) => (
                <button
                  key={p.id}
                  data-profile-id={p.id}
                  className={
                    "profile-item" +
                    (p.id === draft.id ? " active" : "") +
                    (p.is_production ? " prod" : "")
                  }
                  style={{ "--conn": connColor(p) } as React.CSSProperties}
                  title={t("Double click: connect")}
                  onClick={() => pick(p)}
                  // Double click = connect: the first click already
                  // selected the profile (password from the keychain, or
                  // the error will say it is missing).
                  onDoubleClick={() => void connect()}
                >
                  <span className="profile-av">{connInitials(p.name)}</span>
                  <span className="profile-text">
                    <span className="profile-name">{p.name}</span>
                    <span className="profile-host">
                      {engineShortName(p.dialect)} · {p.host}:{p.port}
                    </span>
                  </span>
                  {p.is_production && <ProdTag />}
                  {openIds.includes(p.id) && <span className="profile-open" title={t("connection open")} />}
                </button>
              ))}
              {profiles.length === 0 && (
                // First start: the one time the app introduces itself. Three
                // pieces in with a 50 ms stagger, the form on the right is
                // usable at once.
                <div className="welcome">
                  <img className="welcome-logo" src={logoUrl} alt="" width={56} height={56} />
                  <div className="welcome-name">Alamo DB</div>
                  <div className="welcome-text">
                    {t("No saved profile: fill in the server details on the right and press Connect.")}
                  </div>
                </div>
              )}
            </div>
            <button
              className={"profile-new" + (draft.id === null ? " active" : "")}
              onClick={() => {
                setDraft(newProfile());
                setPassword("");
                setError(null);
                setConfirmDelete(false);
              }}
            >
              <IconPlus /> {t("New profile")}
            </button>
            <div className="profile-transfer">
              <button onClick={() => onTransfer("import")}>{t("Import…")}</button>
              <button disabled={profiles.length === 0} onClick={() => onTransfer("export")}>
                {t("Export…")}
              </button>
            </div>
          </div>

          <div className="profile-form">
            {/* What is being connected to, always visible above the sections. */}
            <div
              className="form-head"
              style={{ "--conn": connColor(draft) } as React.CSSProperties}
            >
              <span className="profile-av big">{draft.name.trim() ? connInitials(draft.name) : "+"}</span>
              <span className="form-head-text">
                <span className="form-head-name">
                  {draft.name.trim() || t("New profile")}
                  {draft.is_production && <ProdTag />}
                  {draft.read_only && <span className="ro-tag">{t("read-only")}</span>}
                </span>
                <span className="profile-host">
                  {engineName(draft.dialect)}
                  {draft.host.trim() ? ` · ${draft.username || "?"}@${draft.host}:${draft.port}` : ""}
                  {draft.database.trim() ? ` · ${draft.database}` : ""}
                  {draft.ssh_profile_id !== null ? " · SSH" : ""}
                </span>
              </span>
            </div>
            {editing && (
            <div className="form-sections" role="tablist">
              {SECTIONS.map((sec) => (
                <button
                  key={sec}
                  role="tab"
                  aria-selected={section === sec}
                  className={"form-section" + (section === sec ? " active" : "")}
                  onClick={() => setSection(sec)}
                >
                  {sectionLabel(sec)}
                  {sectionBadge(sec) && <span className="form-section-badge">{sectionBadge(sec)}</span>}
                  {invalidIn[sec] && <span className="form-section-warn" title={t("required fields missing")} />}
                </button>
              ))}
            </div>
            )}
            <div className="form-scroll">
            {!editing && (
              <ProfileSummary
                draft={draft}
                ssh={ssh}
                password={password}
                onPassword={setPassword}
                sshSecret={sshSecret}
                onSshSecret={setSshSecret}
                passwordSaved={passwordSaved}
                savePassword={savePassword}
                onSavePassword={setSavePassword}
                onEnter={() => void connect()}
              />
            )}
            {editing && (
            <>
            {section === "general" && (
              <>
            <label>
              {t("Name")}
              <input
                value={draft.name}
                maxLength={PROFILE_NAME_MAX}
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
                onChange={(e) => set({ name: e.target.value })}
              />
            </label>
            <label>
              {t("Engine")}
              <select
                value={draft.dialect}
                onChange={(e) => {
                  const next = engineOf(e.target.value);
                  if (!next) return;
                  // Port and user follow the engine default only if
                  // the user had not already touched them.
                  const patch: Partial<Profile> = { dialect: next.id };
                  if (draft.port === engine?.default_port) patch.port = next.default_port;
                  if (draft.username === engine?.default_user) patch.username = next.default_user;
                  set(patch);
                }}
              >
                {engines().map((e) => (
                  <option key={e.id} value={e.id}>
                    {e.name}
                  </option>
                ))}
                {/* Engine missing from this build: the profile keeps it. */}
                {!engine && <option value={draft.dialect}>{draft.dialect}</option>}
              </select>
            </label>
            <div className="form-row">
              <label className="grow">
                {t("Host")}
                <input
                  value={draft.host}
                  autoCapitalize="none"
                  autoCorrect="off"
                  spellCheck={false}
                  onChange={(e) => set({ host: e.target.value })}
                />
              </label>
              <label className="port">
                {t("Port")}
                <input
                  type="text"
                  inputMode="numeric"
                  value={draft.port}
                  onChange={(e) => set({ port: Number(e.target.value.replace(/\D/g, "")) })}
                />
              </label>
            </div>
            <label>
              {t("Database")}
              <input
                value={draft.database}
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
                placeholder={engine?.database_required === false ? t("optional — no default schema") : ""}
                onChange={(e) => set({ database: e.target.value })}
              />
            </label>
            <div className="form-row">
              <label className="grow">
                {t("Show only databases")}
                <input
                  value={draft.db_include}
                  autoCapitalize="none"
                  autoCorrect="off"
                  spellCheck={false}
                  placeholder={t("all — e.g. *_test*, *_demo*")}
                  onChange={(e) => set({ db_include: e.target.value })}
                />
              </label>
              <label className="grow">
                {t("Hide databases")}
                <input
                  value={draft.db_exclude}
                  autoCapitalize="none"
                  autoCorrect="off"
                  spellCheck={false}
                  placeholder={t("none — e.g. *_test*, *_demo*")}
                  onChange={(e) => set({ db_exclude: e.target.value })}
                />
              </label>
            </div>
            <div className="form-hint">
              {t(
                "To split the same server into several profiles (e.g. prod and test). Comma-separated, * = any text. It only hides: they stay reachable from SQL. Applies from the next connection."
              )}
            </div>
            <div className="form-row">
              <label className="check">
                <input
                  type="checkbox"
                  checked={draft.is_production}
                  onChange={(e) => {
                    const on = e.target.checked;
                    // A new production profile starts from the strongest
                    // mode the engine has (§12.2); a saved one keeps its
                    // choice, the Security banner says what it costs.
                    const upgrade =
                      on &&
                      draft.id === null &&
                      !verifiesChain(draft.sslmode) &&
                      (engine?.tls_modes.includes("verify-full") ?? false);
                    set({ is_production: on, ...(upgrade ? { sslmode: "verify-full" } : {}) });
                  }}
                />
                {t("production")}
              </label>
              <label
                className="check"
                title={t("Writes are rejected (by the server too) and grid editing is disabled")}
              >
                <input
                  type="checkbox"
                  checked={draft.read_only}
                  onChange={(e) => set({ read_only: e.target.checked })}
                />
                {t("read-only")}
              </label>
            </div>
            <div className="form-row swatches">
              <span>{t("Color")}</span>
              <button
                className={"swatch none" + (draft.color === null ? " active" : "")}
                title={t("none")}
                onClick={() => set({ color: null })}
              />
              {COLORS.map((c) => (
                <button
                  key={c}
                  className={"swatch" + (draft.color === c ? " active" : "")}
                  style={{ background: c }}
                  onClick={() => set({ color: c })}
                />
              ))}
            </div>
              </>
            )}
            {section === "access" && (
              <>
            <label>
              {t("User")}
              <input
                value={draft.username}
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
                onChange={(e) => set({ username: e.target.value })}
              />
            </label>
            <label>
              {t("Password")}
              <input
                type="password"
                value={password}
                autoComplete="off"
                placeholder={
                  passwordSaved
                    ? t("saved in the keychain — empty = use it")
                    : t("never on disk: system keychain only")
                }
                onChange={(e) => setPassword(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && void connect()}
              />
            </label>
            {keychainAllowed() && (
              <label
                className="check"
                title={t("In the system keychain (Keychain Access/Credential Manager), never in cleartext on disk")}
              >
                <input
                  type="checkbox"
                  checked={savePassword}
                  onChange={(e) => setSavePassword(e.target.checked)}
                />
                {t("save password in the keychain")}
              </label>
            )}
              </>
            )}
            {section === "security" && (
              <>
                {/* The modes the engine honours (§2.1), weakest first; a
                    mode it does not honour stays visible only if the
                    profile already has it, marked as such. */}
                <div className="tls-modes" role="radiogroup" aria-label={t("Encryption")}>
                  {[
                    ...TLS_MODE_ORDER.filter((m) => engine?.tls_modes.includes(m) ?? true),
                    ...(engine && !engine.tls_modes.includes(draft.sslmode) ? [draft.sslmode] : []),
                  ].map((id) => {
                    const m = tlsMode(id);
                    const supported = engine?.tls_modes.includes(id) ?? true;
                    return (
                      <label
                        key={id}
                        className={"tls-mode level-" + m.level + (draft.sslmode === id ? " selected" : "")}
                      >
                        <input
                          type="radio"
                          name="sslmode"
                          value={id}
                          checked={draft.sslmode === id}
                          onChange={() => set({ sslmode: id })}
                        />
                        <span className="tls-mode-icon">
                          <TlsLevelIcon level={m.level} />
                        </span>
                        <span className="tls-mode-text">
                          <span className="tls-mode-label">
                            {m.label}
                            <code>{id}</code>
                            {m.recommended && <span className="tls-mode-tag">{t("recommended")}</span>}
                            {!supported && (
                              <span className="tls-mode-tag warn">{t("not supported by this engine")}</span>
                            )}
                          </span>
                          {m.description && <span className="tls-mode-desc">{m.description}</span>}
                        </span>
                      </label>
                    );
                  })}
                </div>
                {draft.is_production && !verifiesChain(draft.sslmode) && (
                  <div className="banner warn tls-banner">
                    <span>
                      {t(
                        "Production profile: with \u201c{mode}\u201d the server is not authenticated. Whoever controls the network can impersonate it and read the password.",
                        { mode: tlsMode(draft.sslmode).label },
                      )}
                    </span>
                    {engine?.tls_modes.includes("verify-full") && (
                      <button className="btn small" onClick={() => set({ sslmode: "verify-full" })}>
                        {t("Use \u201cVerify certificate and host name\u201d")}
                      </button>
                    )}
                  </div>
                )}
                {verifiesChain(draft.sslmode) && (
                  <>
                    <div className="form-row">
                      <label className="grow">
                        {t("Root CA (PEM)")}
                        <input
                          value={draft.ssl_root_cert ?? ""}
                          autoCapitalize="none"
                          autoCorrect="off"
                          spellCheck={false}
                          placeholder={draft.sslmode === "verify-ca" ? t("required") : t("optional")}
                          onChange={(e) => set({ ssl_root_cert: e.target.value || null })}
                        />
                      </label>
                      <button
                        className="btn"
                        disabled={busy}
                        onClick={() =>
                          void ipc.pickRootCert().then((p) => {
                            if (p) set({ ssl_root_cert: p });
                          })
                        }
                      >
                        {t("Browse…")}
                      </button>
                    </div>
                    <div className="form-hint">
                      {draft.sslmode === "verify-ca"
                        ? t("Required: the certificate is checked against this CA only, the host name is not checked.")
                        : t("Optional: empty = the roots the system trusts. With a company CA, pick its certificate here.")}
                    </div>
                    {caInfo &&
                      caInfo.path === caPath &&
                      (caInfo.error ? (
                        <div className="form-error">{tMsg(caInfo.error)}</div>
                      ) : (
                        caInfo.cert && <CertFacts cert={caInfo.cert} />
                      ))}
                  </>
                )}
              </>
            )}
            {section === "ssh" && (
              <>
            <label
              className="check"
              title={t("The database connection goes through a local port-forward over SSH")}
            >
              <input
                type="checkbox"
                checked={draft.ssh_profile_id !== null}
                onChange={(e) => {
                  if (!e.target.checked) {
                    set({ ssh_profile_id: null });
                    return;
                  }
                  // No bastion yet: created in the manager, which hands
                  // it back here at closing.
                  const first = sshProfiles[0];
                  if (first) set({ ssh_profile_id: first.id });
                  else onManageSsh("new");
                }}
              />
              {t("SSH tunnel")}
            </label>
            {draft.ssh_profile_id !== null && (
              <>
                <div className="form-row">
                  <label className="grow">
                    {t("SSH profile")}
                    <select
                      value={ssh ? String(ssh.id) : "missing"}
                      onChange={(e) => {
                        if (e.target.value === "new") onManageSsh("new");
                        else set({ ssh_profile_id: Number(e.target.value) });
                      }}
                    >
                      {ssh === null && <option value="missing">{t("(deleted profile: pick another)")}</option>}
                      {sshProfiles.map((s) => (
                        <option key={s.id} value={String(s.id)}>
                          {s.name}
                        </option>
                      ))}
                      <option value="new">{t("new SSH profile…")}</option>
                    </select>
                  </label>
                  <button
                    className="btn"
                    disabled={ssh === null}
                    title={t("Host, user and key of this bastion, for every connection that uses it")}
                    onClick={() => onManageSsh(draft.ssh_profile_id)}
                  >
                    {t("Edit…")}
                  </button>
                  <button className="btn" title={t("All the SSH profiles: create, duplicate, delete, test")} onClick={() => onManageSsh(null)}>
                    {t("Manage…")}
                  </button>
                </div>
                {ssh && (
                  <div className="form-hint">
                    {sshSummary(ssh)} · {sshAuthLabel(ssh)} ·{" "}
                    {ssh.host_fingerprint ? t("host key confirmed") : t("host key never seen")}
                    {sshOthers === 1 && " · " + t("shared with 1 other connection")}
                    {sshOthers > 1 && " · " + t("shared with {n} other connections", { n: sshOthers })}
                  </div>
                )}
                {ssh && ssh.auth !== "agent" && (
                  <label>
                    {ssh.auth === "key" ? t("Key passphrase") : t("SSH password")}
                    <input
                      type="password"
                      value={sshSecret}
                      autoComplete="off"
                      placeholder={
                        ssh.secret_saved
                          ? t("saved in the keychain — empty = use it")
                          : ssh.auth === "key"
                            ? t("empty if the key is not encrypted")
                            : ""
                      }
                      onChange={(e) => setSshSecret(e.target.value)}
                    />
                  </label>
                )}
              </>
            )}
              </>
            )}
            {section === "settings" && (
              <>
                <div className="form-hint">
                  {t("Overrides for this connection only. Empty = the global settings (Settings → Execution).")}
                </div>
                {OVERRIDABLE_DEFS.map((def) => (
                  <OverrideRow
                    key={def.key}
                    def={def}
                    value={draft.settings[def.key]}
                    onChange={(v) => {
                      const next = { ...draft.settings };
                      if (v === undefined) delete next[def.key];
                      else next[def.key] = v;
                      set({ settings: next });
                    }}
                  />
                ))}
              </>
            )}
            </>
            )}
            {testOk && (
              <>
                <div className="banner ok">
                  {t("Connection succeeded in {ms} ms", { ms: testOk.elapsed_ms })}
                  {testOk.database ? " · " + testOk.database : ""}
                </div>
                <TlsFacts security={testOk.security} ssh={draft.ssh_profile_id !== null} />
              </>
            )}
            {error && error.code === "ssh_host_key_unknown" && (
              <div className="error-panel host-key-panel">
                <div className="error-head">
                  <span className="error-message">{t("New SSH server")}</span>
                </div>
                <div className="error-detail">{error.context}</div>
                <div className="error-detail">
                  {t("Never seen before from this Mac. Compare the fingerprint with the one given by whoever runs the server: if it differs, don't trust it.")}
                </div>
                <code className="host-key-fingerprint">{error.detail}</code>
                <div className="host-key-actions">
                  <button
                    className="btn"
                    onClick={() => {
                      setError(null);
                      setRetry(null);
                    }}
                  >
                    {t("Cancel")}
                  </button>
                  <button className="btn primary" disabled={!retry || busy} onClick={() => void trust()}>
                    {t("Trust and save")}
                  </button>
                </div>
              </div>
            )}
            {error && error.code !== "ssh_host_key_unknown" && (
              <div className="error-panel">
                <div className="error-head">
                  {error.code && <span className="error-code">{error.code}</span>}
                  <span className="error-message">{tMsg(error.message)}</span>
                </div>
                {error.detail && <div className="error-detail">{error.detail}</div>}
                {error.hint && (
                  <div className="error-hint">{t("Hint: {hint}", { hint: tMsg(error.hint) })}</div>
                )}
                {friendly && (
                  <div className="error-hint">
                    {friendly.text}{" "}
                    {friendly.section && friendly.section !== section && (
                      <button className="btn small" onClick={() => setSection(friendly.section!)}>
                        {t("Go to {section}", { section: sectionLabel(friendly.section) })}
                      </button>
                    )}
                  </div>
                )}
              </div>
            )}
            </div>

            <div className="form-actions">
              {editing && draft.id !== null && (
                <button
                  className="btn danger strong"
                  disabled={busy}
                  title={t("Delete the saved profile (asks for confirmation)")}
                  onClick={() => {
                    setDeleteText("");
                    setConfirmDelete(true);
                  }}
                >
                  <IconTrash /> {t("Delete")}
                </button>
              )}
              <button
                className="btn"
                disabled={!valid || busy}
                title={
                  editing
                    ? t("Saves the profile, opens the connection, runs a query and closes it")
                    : t("Opens the connection, runs a query and closes it: it does not save credentials")
                }
                onClick={() => void test()}
              >
                {t("Test connection")}
              </button>
              <div className="spacer" />
              {!editing && (
                <button className="btn" disabled={busy} onClick={() => setEditing(true)}>
                  {t("Edit profile")}
                </button>
              )}
              {editing && draft.id !== null && (
                <>
                  <button className="btn" disabled={busy} onClick={cancelEdit}>
                    {t("Cancel")}
                  </button>
                  <button className="btn primary" disabled={!valid || busy} onClick={() => void saveEdit()}>
                    {t("Save")}
                  </button>
                </>
              )}
              {editing && draft.id === null && (
                <button className="btn" disabled={!valid || busy} onClick={() => void saveEdit()}>
                  {t("Save")}
                </button>
              )}
              {(!editing || draft.id === null) && (
                <button className="btn primary" disabled={!valid || busy} onClick={() => void connect()}>
                  {busy ? t("connecting…") : t("Connect")}
                </button>
              )}
            </div>
          </div>
        </div>
        {confirmDelete && draft.id !== null && (
          // Delete confirmation: above the modal, with the name to type.
          // Deleting a profile also throws the password out of the keychain:
          // it must not happen because of one click too many.
          <div className="confirm-overlay" onMouseDown={() => setConfirmDelete(false)}>
            <div className="confirm-box" onMouseDown={(e) => e.stopPropagation()}>
              <div className="confirm-title">
                <IconTrash /> {t("Delete the profile “{name}”?", { name: savedName })}
              </div>
              <ul className="confirm-list">
                <li>{t("The profile and its password in the keychain are deleted.")}</li>
                <li>{t("A connection that is already open stays open until you close it; the linked SSH profile is not touched.")}</li>
                <li>{t("The database is not touched. This cannot be undone.")}</li>
              </ul>
              <label className="confirm-label">
                {t("To confirm, type the profile name:")} <b>{savedName}</b>
                <input
                  autoFocus
                  value={deleteText}
                  autoCapitalize="none"
                  autoCorrect="off"
                  spellCheck={false}
                  placeholder={savedName}
                  onChange={(e) => setDeleteText(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && void deleteProfile()}
                />
              </label>
              <div className="confirm-actions">
                <button className="btn" onClick={() => setConfirmDelete(false)}>
                  {t("Cancel")}
                </button>
                <button className="btn danger solid" disabled={!deleteReady} onClick={() => void deleteProfile()}>
                  <IconTrash /> {t("Delete the profile")}
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

/// From the driver's error message to practical advice, with the form
/// section where it gets fixed. The messages remain the driver's (in
/// English): only the common cases are recognized here.
function friendlyHint(message: string): { text: string; section: Section | null } | null {
  const m = message.toLowerCase();
  if (m.includes("unknownissuer"))
    return {
      text: t("The server certificate is signed by a CA the system does not know: set the root CA."),
      section: "security",
    };
  // The given CA did not sign the server certificate (another server's CA,
  // or a CA that was re-issued).
  if (m.includes("badsignature"))
    return {
      text: t("The server certificate is not signed by the given root CA: check the file, it may belong to another server."),
      section: "security",
    };
  // rustls >= 0.23.18 writes "certificate not valid for name …".
  if (m.includes("notvalidforname") || m.includes("not valid for name"))
    return {
      text: t("The certificate is not valid for this host name: use the right name (on PostgreSQL also verify-ca)."),
      section: "security",
    };
  if (m.includes("does not support tls"))
    return {
      text: t(
        "The server does not offer TLS. If you expect it (local server) use prefer or disable; otherwise someone may be in the middle: do not lower security."
      ),
      section: "security",
    };
  if (m.includes("password missing") || m.includes("password authentication failed") || m.includes("access denied"))
    return { text: t("Check user and password."), section: "access" };
  if (m.includes("failed to lookup address"))
    return {
      text: t("Host not found: check the name, or enable the SSH tunnel if the server is behind a bastion."),
      section: "general",
    };
  if (m.includes("connection refused"))
    return { text: t("No server is listening on that host and port."), section: "general" };
  if (m.includes("ssh"))
    return { text: t("The problem is in the SSH tunnel."), section: "ssh" };
  return null;
}

function asError(e: unknown): ErrorOut {
  if (typeof e === "object" && e !== null && "message" in e) return e as ErrorOut;
  return {
    severity: "ERROR",
    category: "internal",
    code: null,
    message: String(e),
    detail: null,
    hint: null,
    context: null,
    position_char: null,
    internal_query: null,
    statement_char_offset: null,
    statement_index: null,
  };
}

/// The padlock of a TLS level: open for plaintext (or possibly so), closed
/// for an encrypted channel, shield when the server is authenticated too.
function TlsLevelIcon({ level }: { level: TlsLevel }) {
  if (level === "plain" || level === "maybe") return <IconLockOpen />;
  if (level === "encrypted") return <IconLock />;
  return <IconShieldCheck />;
}

/// Saved profile in read-only: what is about to be opened, and only the fields
/// needed to connect (password, if not in the keychain).
function ProfileSummary({
  draft,
  ssh,
  password,
  onPassword,
  sshSecret,
  onSshSecret,
  passwordSaved,
  savePassword,
  onSavePassword,
  onEnter,
}: {
  draft: Profile;
  ssh: SshProfile | null;
  password: string;
  onPassword: (v: string) => void;
  sshSecret: string;
  onSshSecret: (v: string) => void;
  passwordSaved: boolean;
  savePassword: boolean;
  onSavePassword: (v: boolean) => void;
  onEnter: () => void;
}) {
  const mode = [
    draft.is_production ? t("production (manual commit)") : null,
    draft.read_only ? t("read-only") : null,
  ].filter(Boolean);
  return (
    <>
      <dl className="profile-summary">
        <dt>{t("Engine")}</dt>
        <dd>{engineName(draft.dialect)}</dd>
        <dt>{t("Server")}</dt>
        <dd className="mono">
          {draft.host}:{draft.port}
        </dd>
        {draft.database.trim() !== "" && (
          <>
            <dt>{t("Database")}</dt>
            <dd className="mono">{draft.database}</dd>
          </>
        )}
        {(draft.db_include.trim() !== "" || draft.db_exclude.trim() !== "") && (
          <>
            <dt>{t("Filter")}</dt>
            <dd className="mono">
              {[
                draft.db_include.trim() ? `${t("only")} ${draft.db_include.trim()}` : null,
                draft.db_exclude.trim() ? `${t("except")} ${draft.db_exclude.trim()}` : null,
              ]
                .filter(Boolean)
                .join(" · ")}
            </dd>
          </>
        )}
        <dt>{t("User")}</dt>
        <dd className="mono">{draft.username}</dd>
        <dt>TLS</dt>
        <dd>
          {tlsMode(draft.sslmode).label} <code className="tls-id">{draft.sslmode}</code>
        </dd>
        <dt>{t("SSH tunnel")}</dt>
        <dd className={ssh ? "mono" : undefined}>{ssh ? `${ssh.username}@${ssh.host}:${ssh.port}` : t("no")}</dd>
        {mode.length > 0 && (
          <>
            <dt>{t("Mode")}</dt>
            <dd>{mode.join(" · ")}</dd>
          </>
        )}
      </dl>
      <label>
        {t("Password")}
        <input
          type="password"
          value={password}
          autoComplete="off"
          autoFocus
          placeholder={
            passwordSaved
              ? t("saved in the keychain — empty = use it")
              : t("never on disk: system keychain only")
          }
          onChange={(e) => onPassword(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && onEnter()}
        />
      </label>
      {ssh && ssh.auth !== "agent" && (
        <label>
          {ssh.auth === "key" ? t("Key passphrase") : t("SSH password")}
          <input
            type="password"
            value={sshSecret}
            autoComplete="off"
            placeholder={passwordSaved ? t("saved in the keychain — empty = use it") : ""}
            onChange={(e) => onSshSecret(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && onEnter()}
          />
        </label>
      )}
      {keychainAllowed() && (
        <label
          className="check"
          title={t("In the system keychain (Keychain Access/Credential Manager), never in cleartext on disk")}
        >
          <input type="checkbox" checked={savePassword} onChange={(e) => onSavePassword(e.target.checked)} />
          {t("save password in the keychain")}
        </label>
      )}
    </>
  );
}

/// §16 "Passwords": with "always ask" nothing is offered for the keychain
/// and a connect clears what was saved (the backend's "do not save").
function keychainAllowed(): boolean {
  return setting("connections.password_policy") === "keychain";
}

/// One overridable setting in the profile dialog (§16): a checkbox to keep
/// the global value (showing it, as set now in the settings window), or a
/// control for this connection's own value. `value` undefined = the global
/// one is in use.
function OverrideRow({ def, value, onChange }: { def: SettingDef; value: unknown; onChange: (v: unknown) => void }) {
  const global = useSettings((s) => s.settings);
  const dflt = settingOf(global, def.key as keyof SettingValues) as unknown;
  const overridden = value !== undefined;
  const current = overridden ? value : dflt;
  const defaultLabel = (): string => {
    if (def.kind.kind === "enum") {
      const i = def.kind.values.indexOf(String(dflt));
      return i >= 0 ? t(def.kind.labels[i]) : String(dflt);
    }
    if (def.kind.kind === "bool") return dflt ? t("Yes") : t("No");
    return String(dflt);
  };
  return (
    <div className="override-row" data-key={def.key}>
      <div className="override-head">
        <span>{t(def.label)}</span>
        <label className="check override-default">
          <input
            type="checkbox"
            checked={!overridden}
            onChange={(e) => onChange(e.target.checked ? undefined : dflt)}
          />
          {t("Use the default ({value})", { value: defaultLabel() })}
        </label>
      </div>
      {overridden && <OverrideControl def={def} value={current} onChange={onChange} />}
    </div>
  );
}

function OverrideControl({ def, value, onChange }: { def: SettingDef; value: unknown; onChange: (v: unknown) => void }) {
  switch (def.kind.kind) {
    case "enum": {
      const { values, labels } = def.kind;
      return (
        <select value={String(value)} onChange={(e) => onChange(e.target.value)}>
          {values.map((v, i) => (
            <option key={v} value={v}>
              {t(labels[i])}
            </option>
          ))}
        </select>
      );
    }
    case "bool":
      return (
        <select value={value ? "true" : "false"} onChange={(e) => onChange(e.target.value === "true")}>
          <option value="true">{t("Yes")}</option>
          <option value="false">{t("No")}</option>
        </select>
      );
    case "int":
      return (
        <input
          type="number"
          min={def.kind.min}
          max={def.kind.max}
          step={1}
          value={Number(value)}
          onChange={(e) => {
            const n = Number(e.target.value);
            if (Number.isInteger(n)) onChange(n);
          }}
        />
      );
    case "str":
      return <input type="text" value={String(value)} onChange={(e) => onChange(e.target.value)} />;
  }
}
