// Alpha updates, like DBeaver: at startup the backend is asked whether
// there is a new version; if so, a window with the news offers "Install and
// restart". Errors of the check (no network, platform not published) are
// silent: the app works the same and we try again at the next startup.
import { useEffect, useState } from "react";
import { ErrorOut, ipc } from "./ipc";
import { t, tMsg } from "./i18n";
import { IconX } from "./icons";
import { patchSettings, setting } from "./settings";

/// Version dismissed with "Skip this version" (a setting, §16): not offered
/// again until a different one comes out.
function skipped(): string {
  return setting("updates.skipped_version");
}

type Update = { version: string; current_version: string; notes: string | null };

export default function UpdateDialog() {
  const [update, setUpdate] = useState<Update | null>(null);
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    ipc
      .updateCheck()
      .then((u) => setUpdate(u && u.version !== skipped() ? u : null))
      .catch(() => {});
  }, []);

  const later = () => {
    if (!installing) setUpdate(null);
  };
  const skip = () => {
    if (!update || installing) return;
    // If the save fails it just closes: at the next startup it shows up again.
    void patchSettings({ "updates.skipped_version": update.version }).catch(() => {});
    setUpdate(null);
  };
  const install = () => {
    setInstalling(true);
    setError(null);
    // Success = the app closes and restarts: the then never arrives.
    ipc.updateInstall().catch((e: ErrorOut) => {
      setInstalling(false);
      setError(tMsg(e.message));
    });
  };

  useEffect(() => {
    if (!update) return;
    // Esc = Later. In capture: the focus may be in the editor underneath.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      later();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  if (!update) return null;
  return (
    <div className="dialog-overlay">
      <div className="dialog update-dialog small-dialog">
        <div className="dialog-title">
          <span>{t("Update available")}</span>
          <button className="btn small dialog-close" disabled={installing} onClick={later}>
            <IconX />
          </button>
        </div>
        <div className="dialog-body">
          <div className="update-target">
            {t("Alamo DB {v} is available", { v: update.version })}
            <div className="muted">{t("Installed version: {v}", { v: update.current_version })}</div>
          </div>
          {update.notes && (
            <>
              <div className="update-change">{t("What's new:")}</div>
              <div className="app-update-notes">{update.notes}</div>
            </>
          )}
          {installing && (
            <div className="update-change">{t("Downloading… the app will restart by itself.")}</div>
          )}
          {error && <div className="app-update-error">{error}</div>}
          <div className="form-actions">
            <button className="btn" disabled={installing} onClick={skip}>
              {t("Skip this version")}
            </button>
            <div className="spacer" />
            <button className="btn" disabled={installing} onClick={later}>
              {t("Later")}
            </button>
            <button className="btn primary" disabled={installing} onClick={install}>
              {t("Install and restart")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
