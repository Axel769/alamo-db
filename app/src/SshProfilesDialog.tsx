// SSH profiles manager (§12): the bastion is an entity shared by several
// database profiles, so it is created, edited and deleted HERE, where one
// sees who goes through it — the connection's SSH tab only picks one
// (DataGrip model). Credentials: typed for "Test tunnel", in the keychain
// afterwards if asked; never on disk.
import { useCallback, useEffect, useState } from "react";
import { t, tMsg } from "./i18n";
import { ErrorOut, ipc, SshProfile, SshProfileOut } from "./ipc";
import { asErrorOut } from "./errors";
import { IconDuplicate, IconPlus, IconTrash, IconX } from "./icons";

/// What to open on: a saved profile, a fresh one, or whatever comes first.
export type SshPick = number | null | "new";

type Props = {
  initialId: SshPick;
  /// The profile selected at closing: the connection's SSH tab picks it.
  onClose: (pickedId: number | null) => void;
};

const NEW_SSH: SshProfile = {
  id: null,
  name: "",
  host: "",
  port: 22,
  username: "",
  auth: "agent",
  key_path: null,
};

/// Only the profile's own fields: `SshProfileOut` carries more.
const strip = ({ id, name, host, port, username, auth, key_path }: SshProfile): SshProfile => ({
  id,
  name,
  host,
  port,
  username,
  auth,
  key_path,
});

const same = (a: SshProfile, b: SshProfile) => JSON.stringify(strip(a)) === JSON.stringify(strip(b));

export const sshSummary = (p: SshProfile) => `${p.username}@${p.host}:${p.port}`;

export const sshAuthLabel = (p: SshProfile) =>
  p.auth === "key" ? t("key {path}", { path: p.key_path ?? "" }) : p.auth === "password" ? t("password") : t("SSH agent");

const isValid = (p: SshProfile) =>
  p.host.trim() !== "" &&
  p.username.trim() !== "" &&
  p.port > 0 &&
  p.port <= 65535 &&
  (p.auth !== "key" || (p.key_path ?? "").trim() !== "");

type Confirm = { kind: "delete" } | { kind: "discard"; then: () => void };

export default function SshProfilesDialog({ initialId, onClose }: Props) {
  const [profiles, setProfiles] = useState<SshProfileOut[]>([]);
  /// null = list not loaded yet.
  const [draft, setDraft] = useState<SshProfile | null>(null);
  /// Bastion password or key passphrase: for "Test tunnel"; saved only if asked.
  const [secret, setSecret] = useState("");
  const [saveSecret, setSaveSecret] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ErrorOut | null>(null);
  const [testOk, setTestOk] = useState<number | null>(null);
  const [confirm, setConfirm] = useState<Confirm | null>(null);
  /// The last SAVED profile shown: what the connection gets at closing
  /// when the draft is a new profile that was never saved.
  const [lastSavedId, setLastSavedId] = useState<number | null>(null);

  const saved = draft?.id != null ? (profiles.find((p) => p.id === draft.id) ?? null) : null;
  const fieldsDirty = draft !== null && !same(draft, saved ?? NEW_SSH);
  /// Also "do not keep the credential": nothing changes in the fields,
  /// but Save has something to do.
  const dirty = fieldsDirty || (saved?.secret_saved === true && !saveSecret);
  const valid = draft !== null && isValid(draft);

  /// Shows a profile. `keep`: the credential typed and the "save" choice
  /// stay — it is the same profile re-read after a save or a test, not
  /// another one.
  const show = useCallback((p: SshProfileOut | null, keep = false) => {
    setDraft(p ? strip(p) : { ...NEW_SSH });
    if (p) setLastSavedId(p.id);
    if (!keep) {
      setSecret("");
      setSaveSecret(p?.secret_saved ?? false);
    }
    setError(null);
    setTestOk(null);
  }, []);

  /// Re-reads the list and shows `selectId` (a fresh profile with `"new"`,
  /// the first one with `null`).
  const reload = useCallback(
    async (selectId: SshPick, keep = false) => {
      const list = await ipc.sshProfilesList();
      setProfiles(list);
      if (selectId === "new") show(null);
      else show(list.find((p) => p.id === selectId) ?? list[0] ?? null, keep);
    },
    [show],
  );

  useEffect(() => {
    void reload(initialId);
  }, [initialId, reload]);

  /// Runs `then` now, or after the user agrees to drop the edits.
  const guarded = (then: () => void) => {
    if (fieldsDirty) setConfirm({ kind: "discard", then });
    else then();
  };

  const close = () => guarded(() => onClose(draft?.id ?? lastSavedId));

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        if (confirm) setConfirm(null);
        else close();
      }
      // Nothing under the dialog gets the keyboard.
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  const set = (patch: Partial<SshProfile>) => {
    setDraft((d) => (d ? { ...d, ...patch } : d));
    setTestOk(null);
  };

  const save = async (): Promise<number> => {
    if (!draft) throw new Error("nothing to save");
    // Empty name = derived from the bastion.
    const name = draft.name.trim() || sshSummary(draft).replace(/:\d+$/, "");
    const id = await ipc.sshProfileSave({ ...draft, name });
    if (saved?.secret_saved && !saveSecret) await ipc.sshSecretForget(id);
    await reload(id, true);
    return id;
  };

  const run = async (action: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (e) {
      setError(asErrorOut(e));
    } finally {
      setBusy(false);
    }
  };

  /// "Test tunnel" works on the SAVED profile, as "Test connection" does
  /// in the connections dialog: edits are saved first.
  const test = () =>
    run(async () => {
      const id = dirty || draft?.id == null ? await save() : draft.id;
      const out = await ipc.sshProfileTest(id, secret || null, saveSecret);
      // Saved credential, confirmed key: the facts change.
      await reload(id, true);
      setTestOk(out.elapsed_ms);
    });

  /// Never-seen bastion: the user trusts the fingerprint shown, then the
  /// test runs again.
  const trust = () =>
    run(async () => {
      if (!draft || draft.id === null || !error?.detail) return;
      const id = draft.id;
      await ipc.sshProfileHostKeyTrust(id, error.detail);
      // The key is saved whatever the test says next.
      await reload(id, true);
      const out = await ipc.sshProfileTest(id, secret || null, saveSecret);
      await reload(id, true);
      setTestOk(out.elapsed_ms);
    });

  const forgetHostKey = () =>
    run(async () => {
      if (!saved) return;
      await ipc.sshHostKeyForget(saved.id as number);
      await reload(saved.id, true);
    });

  const remove = () =>
    run(async () => {
      if (!saved) return;
      const i = profiles.findIndex((p) => p.id === saved.id);
      const neighbour = profiles[i + 1] ?? profiles[i - 1] ?? null;
      await ipc.sshProfileDelete(saved.id as number);
      setConfirm(null);
      // Gone: the connection must not get it back at closing.
      setLastSavedId(null);
      await reload(neighbour ? neighbour.id : null);
    });

  const usage = (p: SshProfileOut) =>
    p.used_by.length === 0
      ? t("not used")
      : p.used_by.length === 1
        ? t("used by 1 connection")
        : t("used by {n} connections", { n: p.used_by.length });

  return (
    <div className="dialog-overlay">
      <div className="dialog connect-dialog ssh-dialog">
        <div className="dialog-title">
          <span>
            {t("SSH profiles")}
            <small className="dialog-sub">{t("bastions shared by the connections that go through them")}</small>
          </span>
          <button className="btn small dialog-close" onClick={close}>
            <IconX />
          </button>
        </div>
        <div className="dialog-body">
          <div className="profile-list">
            <div className="profile-toolbar">
              <button className="btn small" disabled={busy} onClick={() => guarded(() => show(null))}>
                <IconPlus /> {t("New")}
              </button>
              <button
                className="btn small"
                disabled={busy || !draft}
                title={t("A copy of the selected profile, to be saved")}
                onClick={() =>
                  guarded(() => {
                    if (!draft) return;
                    const copy = { ...strip(draft), id: null, name: t("{name} (copy)", { name: draft.name || sshSummary(draft) }) };
                    show(null);
                    setDraft(copy);
                  })
                }
              >
                <IconDuplicate /> {t("Duplicate")}
              </button>
              <div className="spacer" />
              <button
                className="btn small danger"
                disabled={busy || !saved}
                title={t("Delete the selected profile (asks for confirmation)")}
                onClick={() => setConfirm({ kind: "delete" })}
              >
                <IconTrash /> {t("Delete")}
              </button>
            </div>
            <div className="profile-items">
              {profiles.map((p) => (
                <button
                  key={p.id}
                  className={"profile-item" + (p.id === draft?.id ? " active" : "")}
                  onClick={() => guarded(() => show(p))}
                >
                  <span className="profile-text">
                    <span className="profile-name">{p.name}</span>
                    <span className="profile-host">{sshSummary(p)}</span>
                    <span className="profile-sub">{usage(p)}</span>
                  </span>
                </button>
              ))}
              {draft !== null && draft.id === null && (
                <button className="profile-item active">
                  <span className="profile-text">
                    <span className="profile-name">{draft.name.trim() || t("new SSH profile")}</span>
                    <span className="profile-host">{t("not saved yet")}</span>
                  </span>
                </button>
              )}
            </div>
          </div>

          <div className="profile-form">
            {draft && (
              <>
                <div className="form-scroll">
                  <label>
                    {t("Name")}
                    <input
                      value={draft.name}
                      autoFocus={draft.id === null}
                      autoCapitalize="none"
                      autoCorrect="off"
                      spellCheck={false}
                      placeholder={t("empty = user@host")}
                      onChange={(e) => set({ name: e.target.value })}
                    />
                  </label>
                  <div className="form-row">
                    <label className="grow">
                      {t("SSH host")}
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
                        inputMode="numeric"
                        value={draft.port}
                        onChange={(e) => set({ port: Number(e.target.value.replace(/\D/g, "")) })}
                      />
                    </label>
                  </div>
                  <div className="form-row">
                    <label className="grow">
                      {t("SSH user")}
                      <input
                        value={draft.username}
                        autoCapitalize="none"
                        autoCorrect="off"
                        spellCheck={false}
                        onChange={(e) => set({ username: e.target.value })}
                      />
                    </label>
                    <label>
                      {t("Authentication")}
                      <select value={draft.auth} onChange={(e) => set({ auth: e.target.value })}>
                        <option value="agent">{t("SSH agent")}</option>
                        <option value="key">{t("private key")}</option>
                        <option value="password">{t("SSH password")}</option>
                      </select>
                    </label>
                  </div>
                  {draft.auth === "key" && (
                    <div className="form-row">
                      <label className="grow">
                        {t("Private key (path)")}
                        <input
                          value={draft.key_path ?? ""}
                          autoCapitalize="none"
                          autoCorrect="off"
                          spellCheck={false}
                          placeholder="~/.ssh/id_ed25519"
                          onChange={(e) => set({ key_path: e.target.value || null })}
                        />
                      </label>
                      <button
                        className="btn"
                        disabled={busy}
                        onClick={() =>
                          void ipc.pickSshKey().then((p) => {
                            if (p) set({ key_path: p });
                          })
                        }
                      >
                        {t("Browse…")}
                      </button>
                    </div>
                  )}
                  {draft.auth !== "agent" && (
                    <>
                      <label>
                        {draft.auth === "key" ? t("Key passphrase") : t("SSH password")}
                        <input
                          type="password"
                          value={secret}
                          autoComplete="off"
                          placeholder={
                            saved?.secret_saved
                              ? t("saved in the keychain — empty = use it")
                              : draft.auth === "key"
                                ? t("empty if the key is not encrypted")
                                : ""
                          }
                          onChange={(e) => setSecret(e.target.value)}
                        />
                      </label>
                      <label className="check">
                        <input type="checkbox" checked={saveSecret} onChange={(e) => setSaveSecret(e.target.checked)} />
                        {t("save in the keychain")}
                      </label>
                      <div className="form-hint">
                        {t(
                          "Saved by Test tunnel once the bastion accepts it, or by the first connection. Shared by every connection through this bastion; unticked, Save removes it.",
                        )}
                      </div>
                    </>
                  )}
                  {saved && (
                    <dl className="profile-summary ssh-facts">
                      <dt>{t("Host key")}</dt>
                      <dd>
                        {saved.host_fingerprint ? (
                          <>
                            <code title={t("Fingerprint confirmed in Alamo (ssh-keygen -l format)")}>{saved.host_fingerprint}</code>
                            <button
                              className="btn small"
                              disabled={busy}
                              title={t("The next connection asks to confirm the fingerprint again")}
                              onClick={() => void forgetHostKey()}
                            >
                              {t("Forget")}
                            </button>
                          </>
                        ) : (
                          t("never seen: the first connection shows the fingerprint to confirm")
                        )}
                      </dd>
                      <dt>{t("Used by")}</dt>
                      <dd>{saved.used_by.length > 0 ? saved.used_by.join(", ") : t("no connection")}</dd>
                    </dl>
                  )}
                  {testOk !== null && <div className="banner ok">{t("Tunnel opened in {ms} ms", { ms: testOk })}</div>}
                  {error && error.code === "ssh_host_key_unknown" && (
                    <div className="error-panel host-key-panel">
                      <div className="error-head">
                        <span className="error-message">{t("New SSH server")}</span>
                      </div>
                      <div className="error-detail">{error.context}</div>
                      <div className="error-detail">
                        {t(
                          "Never seen before from this Mac. Compare the fingerprint with the one given by whoever runs the server: if it differs, don't trust it.",
                        )}
                      </div>
                      <code className="host-key-fingerprint">{error.detail}</code>
                      <div className="host-key-actions">
                        <button className="btn" onClick={() => setError(null)}>
                          {t("Cancel")}
                        </button>
                        <button className="btn primary" disabled={busy} onClick={() => void trust()}>
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
                      {error.hint && <div className="error-hint">{t("Hint: {hint}", { hint: tMsg(error.hint) })}</div>}
                    </div>
                  )}
                </div>
                <div className="form-actions">
                  <button
                    className="btn"
                    disabled={!valid || busy}
                    title={t("Saves the profile, logs into the bastion and disconnects: nothing is forwarded")}
                    onClick={() => void test()}
                  >
                    {busy ? t("testing…") : t("Test tunnel")}
                  </button>
                  <div className="spacer" />
                  <button className="btn" disabled={!dirty || busy} onClick={() => show(saved)}>
                    {t("Cancel")}
                  </button>
                  <button className="btn primary" disabled={!dirty || !valid || busy} onClick={() => void run(async () => {
                      await save();
                    })}>
                    {t("Save")}
                  </button>
                </div>
              </>
            )}
          </div>
        </div>

        {confirm?.kind === "delete" && saved && (
          <div className="confirm-overlay" onMouseDown={() => setConfirm(null)}>
            <div className="confirm-box" onMouseDown={(e) => e.stopPropagation()}>
              <div className="confirm-title">
                <IconTrash /> {t("Delete the SSH profile “{name}”?", { name: saved.name })}
              </div>
              <ul className="confirm-list">
                {saved.used_by.length > 0 ? (
                  <li>{t("{names}: back to a direct connection, without tunnel.", { names: saved.used_by.join(", ") })}</li>
                ) : (
                  <li>{t("No connection goes through it.")}</li>
                )}
                {saved.secret_saved && <li>{t("Its credential in the keychain is deleted.")}</li>}
                <li>{t("Connections already open stay open until you close them. This cannot be undone.")}</li>
              </ul>
              <div className="confirm-actions">
                <button className="btn" autoFocus onClick={() => setConfirm(null)}>
                  {t("Cancel")}
                </button>
                <button className="btn danger solid" disabled={busy} onClick={() => void remove()}>
                  <IconTrash /> {t("Delete the SSH profile")}
                </button>
              </div>
            </div>
          </div>
        )}
        {confirm?.kind === "discard" && (
          <div className="confirm-overlay" onMouseDown={() => setConfirm(null)}>
            <div className="confirm-box" onMouseDown={(e) => e.stopPropagation()}>
              <div className="confirm-title">{t("Unsaved changes")}</div>
              <ul className="confirm-list">
                <li>{t("The edits to “{name}” are lost if you leave now.", { name: draft?.name.trim() || t("new SSH profile") })}</li>
              </ul>
              <div className="confirm-actions">
                <button className="btn" autoFocus onClick={() => setConfirm(null)}>
                  {t("Keep editing")}
                </button>
                <button
                  className="btn danger strong"
                  onClick={() => {
                    const { then } = confirm;
                    setConfirm(null);
                    then();
                  }}
                >
                  {t("Discard")}
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
