// App settings (gear at the bottom of the activity bar): language
// (decision 14) and theme; tomorrow the constants. Preferences live in
// localStorage — no backend: they are UI choices, not the profile's.
import { useEffect } from "react";
import { lang, setLang, t } from "./i18n";
import { setThemePref, themePref, ThemePref } from "./theme";
import { IconX } from "./icons";

export default function SettingsDialog({ onClose }: { onClose: () => void }) {
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
          <label>
            {t("Language")}
            {/* The switch reloads the app (see i18n.ts): the dialog dies with
                the reload, nothing to close. */}
            <select value={lang()} onChange={(e) => setLang(e.target.value as "it" | "en")}>
              <option value="it">Italiano</option>
              <option value="en">English</option>
            </select>
          </label>
          <label>
            {t("Theme")}
            {/* This one reloads the app too (see theme.ts). */}
            <select value={themePref()} onChange={(e) => setThemePref(e.target.value as ThemePref)}>
              <option value="system">{t("Same as system")}</option>
              <option value="light">{t("Light")}</option>
              <option value="dark">{t("Dark")}</option>
            </select>
          </label>
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
