// App settings (§16): the mirror of `settings.json`, which Rust owns.
// Loaded once before the first render (`bootSettings`); afterwards every
// change goes through `patchSettings`: the backend validates and writes,
// the store takes the result and `version` re-renders what shows
// translated text — no reload.
import { create } from "zustand";
import { ipc, SettingsIssue } from "./ipc";
import { DEFAULTS, SettingKey, Settings, SettingValues } from "./settingsSchema";

type State = {
  settings: Settings;
  /// Bumped at every change: components subscribe to it, `t()` stays a
  /// plain function.
  version: number;
  /// Light or dark as drawn now: the choice, or the system's on "system".
  resolvedTheme: "light" | "dark";
  /// Values of settings.json this build could not take (told once).
  issues: SettingsIssue[];
};

export const useSettings = create<State>(() => ({
  settings: DEFAULTS,
  version: 0,
  resolvedTheme: "light",
  issues: [],
}));

/// One setting, outside React (editor, i18n, theme). Inside a component
/// read `useSettings` instead, so the change re-renders it.
export function setting<K extends SettingKey>(key: K): SettingValues[K] {
  const [cat, field] = key.split(".") as [keyof Settings, string];
  return (useSettings.getState().settings[cat] as Record<string, unknown>)[field] as SettingValues[K];
}

/// Writes the given settings; all or none. Rejects with the backend's
/// message (for `tMsg`).
export async function patchSettings(patch: Partial<SettingValues>): Promise<void> {
  const out = await ipc.settingsSet(patch);
  useSettings.setState((s) => ({ settings: out.settings, version: s.version + 1 }));
}

/// Up to alpha.13 these lived in the webview's localStorage.
const LEGACY_KEYS = ["alamo.lang", "alamo.theme", "alamo.update.skip"] as const;

function legacyPatch(): Partial<SettingValues> {
  const p: Partial<SettingValues> = {};
  const lang = localStorage.getItem("alamo.lang");
  if (lang === "it" || lang === "en") p["appearance.language"] = lang;
  const theme = localStorage.getItem("alamo.theme");
  if (theme === "light" || theme === "dark") p["appearance.theme"] = theme;
  const skip = localStorage.getItem("alamo.update.skip");
  if (skip) p["updates.skipped_version"] = skip;
  return p;
}

/// First load, before the first render. The choices kept in localStorage
/// by the previous builds are carried into settings.json once, then the
/// keys are removed.
export async function bootSettings(): Promise<void> {
  if (LEGACY_KEYS.some((k) => localStorage.getItem(k) !== null)) {
    const patch = legacyPatch();
    if (Object.keys(patch).length) {
      try {
        await ipc.settingsSet(patch);
      } catch {
        // They were a convenience: the defaults are the fallback.
      }
    }
    for (const k of LEGACY_KEYS) localStorage.removeItem(k);
  }
  const out = await ipc.settingsGet();
  useSettings.setState((s) => ({ settings: out.settings, issues: out.issues, version: s.version + 1 }));
}
