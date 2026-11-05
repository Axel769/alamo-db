// App settings (gear at the bottom of the activity bar, §16): language and
// theme, applied at once — the dialog stays open and switches language
// with the rest of the app. Values the file had and this build could not
// take are listed here, once.
import { useEffect, useState } from "react";
import { setLang, t, tMsg } from "./i18n";
import { setThemePref, ThemePref } from "./theme";
import { useSettings } from "./settings";
import { IconX } from "./icons";

export default function SettingsDialog({ onClose }: { onClose: () => void }) {
  const settings = useSettings((s) => s.settings);
  const issues = useSettings((s) => s.issues);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);
  const save = (p: Promise<void>) => {
    setError(null);
    p.catch((e) => setError(tMsg(String(e))));
  };
  return (
    <div className="dialog-overlay">
      <div className="dialog update-dialog settings-dialog">
        <div className="dialog-title">
          <span>{t("Settings")}</span>
          <button className="btn small dialog-close" onClick={onClose}>
            <IconX />
          </button>
        </div>
        <div className="dialog-body">
          {issues.length > 0 && (
            <div className="settings-issues">
              <div>{t("Some values in settings.json could not be used and are back to the default:")}</div>
              <ul>
                {issues.map((i) => (
                  <li key={i.key}>
                    <code>{i.key}</code>
                    {i.found && (
                      <>
                        {" = "}
                        <code>{i.found}</code>
                      </>
                    )}
                    {" — "}
                    {tMsg(i.message)}
                  </li>
                ))}
              </ul>
            </div>
          )}
          <label>
            {t("Language")}
            <select
              value={settings.appearance.language}
              onChange={(e) => save(setLang(e.target.value as "it" | "en"))}
            >
              <option value="it">Italiano</option>
              <option value="en">English</option>
            </select>
          </label>
          <label>
            {t("Theme")}
            <select
              value={settings.appearance.theme}
              onChange={(e) => save(setThemePref(e.target.value as ThemePref))}
            >
              <option value="system">{t("Same as system")}</option>
              <option value="light">{t("Light")}</option>
              <option value="dark">{t("Dark")}</option>
            </select>
          </label>
          {error && <div className="app-update-error">{error}</div>}
          <div className="form-actions">
            <div className="spacer" />
            <button className="btn" onClick={onClose}>
              {t("Close")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
