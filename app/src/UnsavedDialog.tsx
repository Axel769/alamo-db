// "Save / Discard / Cancel" for unsaved grid edits: the question is asked
// by `guardUnsaved` (pending.ts), here it is shown.
import { useEffect, useState } from "react";
import { ErrorOut } from "./ipc";
import { locale, t } from "./i18n";
import { unsavedCount, useStore } from "./store";
import { savePendingEdits, useUnsaved } from "./pending";
import { ErrorPanel } from "./ResultPane";
import { IconX } from "./icons";

export default function UnsavedDialog() {
  const req = useUnsaved((s) => s.req);
  const tabs = useStore((s) => s.tabs);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<ErrorOut | null>(null);
  useEffect(() => {
    setSaving(false);
    setError(null);
  }, [req]);

  const finish = (proceed: boolean) => {
    if (!req) return;
    useUnsaved.setState({ req: null });
    req.resolve(proceed);
  };
  const discard = () => {
    if (!req || saving) return;
    for (const id of req.tabIds) {
      useStore.getState().setPending(id, {});
      useStore.getState().setNewRows(id, []);
      useStore.getState().setDeleted(id, {});
    }
    finish(true);
  };
  const save = async () => {
    if (!req || saving) return;
    setSaving(true);
    setError(null);
    for (const id of req.tabIds) {
      const err = await savePendingEdits(id);
      if (err) {
        // Save failed: we stay here, the choice goes back to the user.
        setSaving(false);
        setError(err);
        return;
      }
    }
    setSaving(false);
    finish(true);
  };

  useEffect(() => {
    if (!req) return;
    // Enter = Save, Esc = Cancel. In capture and with stopPropagation: the
    // focus stayed on the grid underneath, which would receive the keys.
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Enter" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault();
        void save();
      } else if (e.key === "Escape") {
        e.preventDefault();
        if (!saving) finish(false);
      }
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  if (!req) return null;
  const rows = req.tabIds
    .map((id) => tabs.find((x) => x.id === id))
    .filter((x): x is NonNullable<typeof x> => !!x && unsavedCount(x) > 0);
  const total = rows.reduce((n, x) => n + unsavedCount(x), 0);
  return (
    <div className="dialog-overlay">
      <div className="dialog update-dialog small-dialog">
        <div className="dialog-title">
          <span>{t("Unsaved changes")}</span>
          <button className="btn small dialog-close" disabled={saving} onClick={() => finish(false)}>
            <IconX />
          </button>
        </div>
        <div className="dialog-body">
          <div>
            {total === 1
              ? t("There is 1 unsaved change in:")
              : t("There are {n} unsaved changes in:", { n: total.toLocaleString(locale()) })}
          </div>
          <ul className="quit-pending">
            {rows.map((x) => (
              <li key={x.id}>
                <span className="mono">
                  {x.edit ? `${x.edit.schema}.${x.edit.table}` + (x.kind === "sql" ? ` (${x.title})` : "") : x.title}
                </span>
                <span className="muted"> · {unsavedCount(x).toLocaleString(locale())}</span>
              </li>
            ))}
          </ul>
          {error && <ErrorPanel error={error} />}
          <div className="form-actions">
            <div className="spacer" />
            <button className="btn" disabled={saving} onClick={() => finish(false)}>
              {t("Cancel")}
            </button>
            <button className="btn" disabled={saving} onClick={discard}>
              {t("Discard")}
            </button>
            <button className="btn primary" disabled={saving} onClick={() => void save()}>
              {saving ? t("Saving…") : t("Save")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
