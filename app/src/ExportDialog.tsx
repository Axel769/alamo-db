// Export dialog (decision 11): format and its options. The file choice and
// the writing live in the backend; the caller says which command runs.
import { useEffect, useState } from "react";
import { ExportFormat, ExportOptions, ExportOut, ScriptExportTarget } from "./ipc";
import { locale, t, tMsg } from "./i18n";
import { IconX } from "./icons";

export function ExportDialog({
  title,
  subject,
  detail,
  tableName,
  script,
  onExport,
  onClose,
}: {
  title: string;
  /// What is exported (table or result name), in monospace.
  subject: string;
  /// Rows and completeness, next to the subject.
  detail: string;
  /// INSERT target, editable. `null` = several results, each into its own
  /// source table.
  tableName: string | null;
  /// Several results (a script): the user picks one file or a folder.
  script: boolean;
  onExport: (opts: ExportOptions, target: ScriptExportTarget) => Promise<ExportOut | null>;
  onClose: () => void;
}) {
  const [format, setFormat] = useState<ExportFormat>("csv");
  const [headers, setHeaders] = useState(true);
  const [neutralize, setNeutralize] = useState(true);
  const [table, setTable] = useState(tableName ?? "");
  const [target, setTarget] = useState<ScriptExportTarget>("file");
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<ExportOut | null>(null);
  const [error, setError] = useState<string | null>(null);

  const run = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const out = await onExport(
        { format, headers, table_name: table, neutralize_formulas: neutralize },
        target
      );
      if (out) setDone(out);
      else onClose(); // cancelled from the save dialog
    } catch (e) {
      const err = e as { message?: string };
      setError(tMsg(err?.message ?? String(e)));
    }
    setBusy(false);
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Enter" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault();
        if (done) onClose();
        else void run();
      } else if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [done, busy, format, headers, neutralize, table, target]);

  const delimited = format === "csv" || format === "tsv";
  return (
    <div className="dialog-overlay">
      <div className="dialog update-dialog export-dialog">
        <div className="dialog-title">
          <span>{title}</span>
          <button className="btn small dialog-close" onClick={onClose}>
            <IconX />
          </button>
        </div>
        <div className="dialog-body">
          <div className="update-target">
            <span className="mono">{subject}</span>
            <span className="muted"> — {detail}</span>
          </div>
          <label>
            {t("Format")}
            <select value={format} onChange={(e) => setFormat(e.target.value as ExportFormat)}>
              <option value="csv">CSV</option>
              <option value="tsv">TSV</option>
              <option value="json">JSON</option>
              <option value="insert">INSERT</option>
            </select>
          </label>
          {script && (
            <label>
              {t("Destination")}
              <select value={target} onChange={(e) => setTarget(e.target.value as ScriptExportTarget)}>
                <option value="file">{t("One file, a section per statement")}</option>
                <option value="folder">{t("A new folder, one file per statement")}</option>
              </select>
            </label>
          )}
          {delimited && (
            <label className="check">
              <input type="checkbox" checked={headers} onChange={(e) => setHeaders(e.target.checked)} />
              {t("Header row")}
            </label>
          )}
          {delimited && (
            <label className="check">
              <input type="checkbox" checked={neutralize} onChange={(e) => setNeutralize(e.target.checked)} />
              {t("Neutralize formulas (Excel)")}
            </label>
          )}
          {delimited && (
            <div className="muted export-note">
              {t("Prefixes ' to values starting with = + - @: opening the file, Excel does not run them as formulas")}
            </div>
          )}
          {format === "insert" &&
            (tableName === null ? (
              <div className="muted export-note">
                {t("Each result goes into the table it comes from, or into \"result\" if it mixes tables.")}
              </div>
            ) : (
              <label>
                {t("Target table")}
                <input className="mono" value={table} onChange={(e) => setTable(e.target.value)} />
              </label>
            ))}
          {error && <div className="banner warn">{error}</div>}
          {done && (
            <div className="banner ok">
              {t("{n} rows written to", { n: done.rows.toLocaleString(locale()) })}{" "}
              <span className="mono">{done.path}</span>
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
                <button className="btn primary" disabled={busy} onClick={() => void run()}>
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
