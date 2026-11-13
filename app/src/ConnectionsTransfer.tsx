// Export/import of the saved connections (like DBeaver, our own format:
// `transfer` module on the Rust side). Opened from the ⋯ menu of the
// connections panel and from the command palette; file dialogs are native, in Rust.
import { useQuery } from "@tanstack/react-query";
import { ProdTag } from "./ProdTag";
import { useEffect, useState } from "react";
import { t, tMsg } from "./i18n";
import { ImportOutcome, ImportPreview, ipc } from "./ipc";
import { connColor } from "./connColor";
import { IconX } from "./icons";
// Same file checked by the Rust test `format_example_in_the_import_dialog_is_valid`.
import formatExample from "./connections-example.json";

export type TransferKind = "export" | "import";

/// Like `transfer::MIN_PASSPHRASE` on the Rust side (which checks again).
const MIN_PASSPHRASE = 8;

/// Esc closes, Enter runs (or closes once the work is done): like the result
/// set export. In capture: the keys do not reach the editor below.
function useDialogKeys(onEnter: () => void, onClose: () => void, deps: unknown[]) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Enter" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault();
        onEnter();
      } else if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}

function today(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

export function ExportConnectionsDialog({ onClose }: { onClose: () => void }) {
  const profiles = useQuery({ queryKey: ["profiles"], queryFn: ipc.profilesList });
  // Deselected instead of selected: by default everything is exported, even
  // the profiles that arrive after the dialog opens.
  const [excluded, setExcluded] = useState<Set<number>>(new Set());
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<{ path: string; count: number; passwords: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Passwords included in the file, encrypted with this one (never saved: typed
  // at every export and needed by the import).
  const [withPasswords, setWithPasswords] = useState(false);
  const [pass, setPass] = useState("");
  const [pass2, setPass2] = useState("");

  const list = profiles.data ?? [];
  const chosen = list.filter((p) => p.id !== null && !excluded.has(p.id)).map((p) => p.id as number);
  const allOn = excluded.size === 0;
  const passProblem = !withPasswords
    ? null
    : [...pass].length < MIN_PASSPHRASE
      ? t("At least {n} characters.", { n: MIN_PASSPHRASE })
      : pass !== pass2
        ? t("The two passwords do not match.")
        : null;

  const toggle = (id: number) =>
    setExcluded((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  const run = async () => {
    if (busy || chosen.length === 0 || passProblem) return;
    setBusy(true);
    setError(null);
    try {
      const out = await ipc.connectionsExport(
        chosen,
        withPasswords ? pass : null,
        `${t("alamo-connections")}-${today()}.json`
      );
      if (out) setDone(out);
      else onClose(); // cancelled from the save dialog
    } catch (e) {
      setError(tMsg(String(e)));
    }
    setBusy(false);
  };

  useDialogKeys(() => (done ? onClose() : void run()), onClose, [
    done,
    busy,
    chosen.join(","),
    withPasswords,
    pass,
    pass2,
  ]);

  return (
    <div className="dialog-overlay">
      <div className="dialog update-dialog transfer-dialog">
        <div className="dialog-title">
          <span>{t("Export connections")}</span>
          <button className="btn small dialog-close" onClick={onClose}>
            <IconX />
          </button>
        </div>
        <div className="dialog-body">
          <div className="update-change">
            {withPasswords
              ? t(
                  "Host, port, user, options and SSH tunnels go into a JSON file, with the passwords saved in the keychain encrypted with the file password."
                )
              : t(
                  "Host, port, user, options and SSH tunnels go into a JSON file. Passwords stay in the keychain: on the other computer you enter them on first connect."
                )}
          </div>
          {profiles.isLoading && <div className="tree-note">{t("loading…")}</div>}
          {profiles.data && list.length === 0 && <div className="tree-note">{t("no saved profiles")}</div>}
          {list.length > 0 && (
            <>
              <label className="check transfer-all">
                <input
                  type="checkbox"
                  checked={allOn}
                  onChange={() =>
                    setExcluded(allOn ? new Set(list.map((p) => p.id as number)) : new Set())
                  }
                />
                {t("All ({n} of {tot})", { n: chosen.length, tot: list.length })}
              </label>
              <div className="transfer-list">
                {list.map((p) => (
                  <label key={p.id} className="check transfer-row">
                    <input
                      type="checkbox"
                      checked={!excluded.has(p.id as number)}
                      onChange={() => toggle(p.id as number)}
                    />
                    <span className="swatch" style={{ background: connColor(p) }} />
                    <span className="transfer-name">{p.name}</span>
                    {p.is_production && <ProdTag />}
                    <span className="transfer-dest" title={`${p.username}@${p.host}:${p.port}`}>
                      {p.username}@{p.host}:{p.port}
                    </span>
                  </label>
                ))}
              </div>
              <label className="check">
                <input
                  type="checkbox"
                  checked={withPasswords}
                  disabled={!!done}
                  onChange={(e) => setWithPasswords(e.target.checked)}
                />
                {t("Include passwords (encrypted)")}
              </label>
              {withPasswords && (
                <div className="transfer-pass">
                  <input
                    type="password"
                    autoFocus
                    value={pass}
                    disabled={!!done}
                    placeholder={t("File password")}
                    onChange={(e) => setPass(e.target.value)}
                  />
                  <input
                    type="password"
                    value={pass2}
                    disabled={!!done}
                    placeholder={t("Repeat the password")}
                    onChange={(e) => setPass2(e.target.value)}
                  />
                  <div className="transfer-hint">
                    {passProblem && (pass || pass2) ? <b>{passProblem} </b> : null}
                    {t(
                      "You will need it to import the file. It cannot be recovered: without it, the passwords in the file are lost. The system may ask for permission to read the keychain."
                    )}
                  </div>
                </div>
              )}
            </>
          )}
          {error && <div className="banner warn">{error}</div>}
          {done && (
            <div className="banner ok">
              {t("{n} connections exported to", { n: done.count })} <span className="mono">{done.path}</span>
              {withPasswords &&
                " — " +
                  (done.passwords > 0
                    ? t("passwords included for {n} of {tot}.", { n: done.passwords, tot: done.count })
                    : t("no password saved in the keychain: the file contains none."))}
            </div>
          )}
          <div className="form-actions">
            <div className="spacer" />
            {done ? (
              <button className="btn primary" onClick={onClose}>
                {t("Close")}
              </button>
            ) : (
              <>
                <button className="btn" disabled={busy} onClick={onClose}>
                  {t("Cancel")}
                </button>
                <button
                  className="btn primary"
                  disabled={busy || chosen.length === 0 || !!passProblem}
                  onClick={() => void run()}
                >
                  {busy ? t("Exporting…") : t("Export…")}
                </button>
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

/// Import: the native dialog opens right away, then the preview. Connections
/// already present start deselected; if ticked they come in as a copy
/// (never overwrites).
export function ImportConnectionsDialog({ onClose, onImported }: { onClose: () => void; onImported: () => void }) {
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [busy, setBusy] = useState(true);
  const [done, setDone] = useState<ImportOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  // File password, if it contains encrypted passwords.
  const [pass, setPass] = useState("");
  // "File format" box: opens by itself when a file is rejected.
  const [showFormat, setShowFormat] = useState(false);
  const hasSecrets = !!preview?.doc.secrets;

  const pick = async () => {
    setBusy(true);
    setError(null);
    try {
      const out = await ipc.connectionsImportOpen();
      if (!out) {
        // Cancelled: if there was already a preview, stay there.
        if (!preview) onClose();
      } else {
        setPreview(out);
        setSelected(new Set(out.rows.flatMap((r, i) => (r.existing ? [] : [i]))));
        setShowFormat(false);
      }
    } catch (e) {
      setError(tMsg(String(e)));
      setShowFormat(true);
    }
    setBusy(false);
  };

  useEffect(() => {
    void pick();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const run = async () => {
    if (busy || !preview || selected.size === 0) return;
    setBusy(true);
    setError(null);
    try {
      setDone(
        await ipc.connectionsImportApply(
          preview.doc,
          [...selected].sort((a, b) => a - b),
          hasSecrets && pass ? pass : null
        )
      );
      onImported();
    } catch (e) {
      setError(tMsg(String(e)));
    }
    setBusy(false);
  };

  useDialogKeys(() => (done ? onClose() : void run()), onClose, [done, busy, preview, selected, pass]);

  const rows = preview?.rows ?? [];
  const allOn = rows.length > 0 && selected.size === rows.length;
  const toggle = (i: number) =>
    setSelected((s) => {
      const n = new Set(s);
      if (n.has(i)) n.delete(i);
      else n.add(i);
      return n;
    });

  return (
    <div className="dialog-overlay">
      <div className="dialog update-dialog transfer-dialog">
        <div className="dialog-title">
          <span>{t("Import connections")}</span>
          <button className="btn small dialog-close" onClick={onClose}>
            <IconX />
          </button>
        </div>
        <div className="dialog-body">
          {!preview && busy && <div className="tree-note">{t("Choose the file to import…")}</div>}
          {preview && (
            <>
              <div className="update-target">
                <span className="mono">{preview.file_name}</span>
                <span className="muted">
                  {" "}
                  — {t("{n} connections in the file", { n: rows.length })}
                </span>
              </div>
              <div className="update-change">
                {hasSecrets
                  ? t(
                      "Connections already here are left untouched: if you select them, they come in as a copy. Passwords go only to the connections imported here."
                    )
                  : t(
                      "Passwords are not in the file: you enter them on first connect. Connections already here are left untouched: if you select them, they come in as a copy."
                    )}
              </div>
              {rows.length > 0 && (
                <>
                  <label className="check transfer-all">
                    <input
                      type="checkbox"
                      checked={allOn}
                      disabled={!!done}
                      onChange={() => setSelected(allOn ? new Set() : new Set(rows.map((_, i) => i)))}
                    />
                    {t("All ({n} of {tot})", { n: selected.size, tot: rows.length })}
                  </label>
                  <div className="transfer-list">
                    {rows.map((r, i) => {
                      const p = preview.doc.profiles[i];
                      const ssh = p.ssh ? preview.doc.ssh_profiles.find((s) => s.ref === p.ssh) : undefined;
                      return (
                        <div key={i} className="transfer-item">
                          <label className="check transfer-row">
                            <input
                              type="checkbox"
                              checked={selected.has(i)}
                              disabled={!!done}
                              onChange={() => toggle(i)}
                            />
                            <span className="swatch" style={{ background: connColor({ color: p.color ?? null, name: r.name }) }} />
                            {/* Already here: the name the user recognizes;
                                the copy's name goes in the tooltip. */}
                            <span
                              className="transfer-name"
                              title={r.existing ? t("If imported: “{name}”", { name: r.name }) : r.name}
                            >
                              {r.existing ? p.name : r.name}
                            </span>
                            {p.is_production && <ProdTag />}
                            {r.existing && (
                              <span className="transfer-tag" title={t("Same server, database, user and tunnel as a saved profile")}>
                                {t("already here")}
                              </span>
                            )}
                            {/* Already here = renamed if imported: a label is enough. */}
                            {r.renamed && !r.existing && (
                              <span className="transfer-tag" title={t("The name “{name}” is already taken", { name: p.name })}>
                                {t("renamed")}
                              </span>
                            )}
                            {(() => {
                              const dest =
                                `${p.username}@${p.host}:${p.port}` +
                                (ssh ? ` · ${t("via {ssh}", { ssh: ssh.name })}` : "");
                              return (
                                <span className="transfer-dest" title={dest}>
                                  {dest}
                                </span>
                              );
                            })()}
                          </label>
                          {r.warnings.map((w) => (
                            <div key={w} className="transfer-warn">
                              {w}
                            </div>
                          ))}
                        </div>
                      );
                    })}
                  </div>
                </>
              )}
              {hasSecrets && (
                <div className="transfer-pass">
                  <input
                    type="password"
                    autoFocus
                    value={pass}
                    disabled={!!done}
                    placeholder={t("File password")}
                    onChange={(e) => setPass(e.target.value)}
                  />
                  <div className="transfer-hint">
                    {t(
                      "The file contains encrypted passwords: enter the password chosen at export to put them in the keychain. Empty = import without passwords."
                    )}
                  </div>
                </div>
              )}
            </>
          )}
          {error && <div className="banner warn">{error}</div>}
          {!done && (preview || error) && (
            <FileFormat open={showFormat} onToggle={() => setShowFormat((v) => !v)} />
          )}
          {done && (
            <div className="banner ok">
              {t("{n} connections imported.", { n: done.imported })}
              {done.ssh_created > 0 && " " + t("SSH tunnels created: {n}.", { n: done.ssh_created })}
              {done.ssh_reused > 0 && " " + t("SSH tunnels already here, reused: {n}.", { n: done.ssh_reused })}
              {done.passwords_saved > 0 &&
                " " + t("Passwords saved in the keychain: {n}.", { n: done.passwords_saved })}
            </div>
          )}
          {done && done.warnings.length > 0 && (
            <div className="banner warn">
              {t("Some passwords did not make it into the keychain:")} {done.warnings.join("; ")}
            </div>
          )}
          <div className="form-actions">
            {!done && (preview || error) && (
              <button className="btn" disabled={busy} onClick={() => void pick()}>
                {t("Another file…")}
              </button>
            )}
            <div className="spacer" />
            {done ? (
              <button className="btn primary" onClick={onClose}>
                {t("Close")}
              </button>
            ) : (
              <>
                <button className="btn" disabled={busy && !!preview} onClick={onClose}>
                  {t("Cancel")}
                </button>
                <button
                  className="btn primary"
                  disabled={busy || !preview || selected.size === 0}
                  onClick={() => void run()}
                >
                  {t("Import {n}", { n: selected.size })}
                </button>
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

/// What an importable file looks like: our format only (no DBeaver import),
/// export from Alamo writes it, it can also be written by hand.
function FileFormat({ open, onToggle }: { open: boolean; onToggle: () => void }) {
  const [copied, setCopied] = useState(false);
  const example = JSON.stringify(formatExample, null, 2);
  return (
    <div className="transfer-format">
      <button className="transfer-format-toggle" onClick={onToggle} aria-expanded={open}>
        {open ? "▾" : "▸"} {t("File format")}
      </button>
      {open && (
        <>
          <ul className="transfer-hint">
            <li>
              {t(
                "A JSON file with \"format\": \"alamo-connections\" and \"version\": 1. Export from Alamo creates it; it can also be written by hand."
              )}
            </li>
            <li>
              {t(
                "Each connection in profiles: name, dialect (\"postgres\", or \"mysql\" also for MariaDB), host, port, database (may be empty), username, sslmode (disable, prefer, require, verify-ca, verify-full)."
              )}
            </li>
            <li>
              {t(
                "Optional: color (\"#RRGGBB\"), is_production, read_only, ssl_root_cert (path of the CA), ssl_client_cert and ssl_client_key (client certificate and key), db_include and db_exclude (database filters), ssh (the ref of a tunnel)."
              )}
            </li>
            <li>
              {t(
                "Tunnels in ssh_profiles: ref, name, host, port, username, auth (\"agent\", \"password\" or \"key\"), key_path with \"key\"."
              )}
            </li>
            <li>{t("No passwords in a hand-written file: they are asked at the first connection.")}</li>
          </ul>
          <div className="transfer-example">
            <button
              className="btn small"
              onClick={() =>
                void navigator.clipboard.writeText(example).then(() => {
                  setCopied(true);
                  setTimeout(() => setCopied(false), 1500);
                })
              }
            >
              {copied ? t("Copied") : t("Copy")}
            </button>
            <pre>{example}</pre>
          </div>
        </>
      )}
    </div>
  );
}
