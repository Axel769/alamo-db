// Transaction log (§9.7.2): the writes of the open transaction, listed
// before Commit — on prod the Commit button opens it, everywhere the
// transaction badge does. Enter commits, Esc closes.
import { useEffect, useState } from "react";
import { ipc, Profile, TxnLog } from "./ipc";
import { asErrorOut } from "./errors";
import { connColor } from "./connColor";
import { locale, t, tMsg } from "./i18n";
import { IconX } from "./icons";

export default function TxnLogDialog({
  connId,
  profile,
  busy,
  onCommit,
  onRollback,
  onClose,
}: {
  connId: number;
  profile: Profile | null;
  /// Sticky running: Commit and Rollback wait.
  busy: boolean;
  onCommit: () => void;
  onRollback: () => void;
  onClose: () => void;
}) {
  const [log, setLog] = useState<TxnLog | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    ipc
      .txnLog(connId)
      .then(setLog)
      .catch((e) => setError(tMsg(asErrorOut(e).message)));
  }, [connId]);

  const canCommit = !busy && !!log && log.total > 0;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Enter" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault();
        if (canCommit) onCommit();
      } else if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [canCommit, onCommit, onClose]);

  const fmtAt = (ms: number) => {
    const s = Math.floor(ms / 1000);
    return `+${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  };
  return (
    <div className="dialog-overlay">
      <div className="dialog update-dialog txn-log-dialog">
        <div className="dialog-title">
          <span>
            {!log
              ? t("Open transaction")
              : log.total === 1
                ? t("Open transaction: 1 uncommitted write")
                : t("Open transaction: {n} uncommitted writes", { n: log.total.toLocaleString(locale()) })}
            {profile?.is_production && <span className="prod-tag"> {t("production")}</span>}
          </span>
          <button className="btn small dialog-close" onClick={onClose}>
            <IconX />
          </button>
        </div>
        <div className="dialog-body">
          {profile && (
            <div className="update-target">
              <span className="swatch" style={{ background: connColor(profile) }} />{" "}
              <span className="mono">{profile.name}</span>
            </div>
          )}
          <div className="update-change">
            {t(
              "These statements have already run on the server and wait in the transaction: Commit makes them permanent, Rollback undoes them."
            )}
          </div>
          {error && <div className="banner warn">{error}</div>}
          {log && log.total === 0 && (
            <div className="muted">{t("No writes: the transaction only read data.")}</div>
          )}
          {log && log.writes.length > 0 && (
            <ol className="txn-log">
              {log.writes.map((w, i) => (
                <li key={i}>
                  <pre className="mono">
                    {w.sql}
                    {w.truncated ? " …" : ""}
                  </pre>
                  <span className="muted">
                    {fmtAt(w.at_ms)}
                    {w.rows !== null &&
                      " · " +
                        (w.rows === 1 ? t("1 row") : t("{n} rows", { n: w.rows.toLocaleString(locale()) }))}
                  </span>
                </li>
              ))}
            </ol>
          )}
          {log && log.total > log.writes.length && (
            <div className="muted">
              {t("…and {n} more: only the first {max} are listed.", {
                n: (log.total - log.writes.length).toLocaleString(locale()),
                max: log.writes.length.toLocaleString(locale()),
              })}
            </div>
          )}
          <div className="form-actions">
            <button className="btn" disabled={busy || !log} onClick={onRollback}>
              Rollback
            </button>
            <div className="spacer" />
            <button className="btn" onClick={onClose}>
              {t("Close")}
            </button>
            <button className="btn primary" disabled={!canCommit} onClick={onCommit}>
              Commit
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
