// App settings (§16): the mirror of `settings.json`, which Rust owns.
// Loaded once before the first render (`bootSettings`); afterwards every
// change goes through `patchSettings`: the backend validates and writes,
// the store takes the result and `version` re-renders what shows
// translated text — no reload.
import { create } from "zustand";
import { EffectiveExecution, ipc, SettingsIssue } from "./ipc";
import { DEFAULTS, SettingKey, Settings, SettingValues } from "./settingsSchema";

type Execution = Settings["execution"];

type State = {
  settings: Settings;
  /// Bumped at every change: components subscribe to it, `t()` stays a
  /// plain function.
  version: number;
  /// Light or dark as drawn now: the choice, or the system's on "system".
  resolvedTheme: "light" | "dark";
  /// Values of settings.json this build could not take (told once).
  issues: SettingsIssue[];
  /// Execution settings in force per open connection (§16): the global
  /// ones with the profile's overrides, merged by the backend at connect
  /// and after every change. Absent = the global ones.
  effective: Record<number, Execution>;
};

export const useSettings = create<State>(() => ({
  settings: DEFAULTS,
  version: 0,
  resolvedTheme: "light",
  issues: [],
  effective: {},
}));

/// The Execution settings in force on a connection (§16): what the backend
/// said at connect or at the last change, the global ones without one.
export function executionFor(connId: number | null | undefined): Execution {
  const s = useSettings.getState();
  return (connId !== null && connId !== undefined && s.effective[connId]) || s.settings.execution;
}

/// A connection just opened (§16): what the backend merged for it.
export function setEffective(connId: number, execution: Execution): void {
  useSettings.setState((s) => ({ effective: { ...s.effective, [connId]: execution } }));
}

/// Connections closed: their values go.
export function dropEffective(connIds: Iterable<number>): void {
  useSettings.setState((s) => {
    const effective = { ...s.effective };
    for (const id of connIds) delete effective[id];
    return { effective };
  });
}

function withEffective(s: State, list: EffectiveExecution[]): Record<number, Execution> {
  const effective = { ...s.effective };
  for (const e of list) effective[e.conn_id] = e.execution;
  return effective;
}

/// After a profile save (§16): its open connections with their values.
export function applyEffective(list: EffectiveExecution[]): void {
  useSettings.setState((s) => ({ effective: withEffective(s, list) }));
}

/// One setting by its `category.field` key, from the given values.
export function settingOf<K extends SettingKey>(settings: Settings, key: K): SettingValues[K] {
  const [cat, field] = key.split(".") as [keyof Settings, string];
  return (settings[cat] as Record<string, unknown>)[field] as SettingValues[K];
}

/// One setting, outside React (editor, i18n, theme). Inside a component
/// read `useSettings` instead, so the change re-renders it.
export function setting<K extends SettingKey>(key: K): SettingValues[K] {
  return settingOf(useSettings.getState().settings, key);
}

/// Writes the given settings; all or none. Rejects with the backend's
/// message (for `tMsg`).
export async function patchSettings(patch: Partial<SettingValues>): Promise<void> {
  const out = await ipc.settingsSet(patch);
  useSettings.setState((s) => ({
    settings: out.settings,
    effective: withEffective(s, out.effective),
    version: s.version + 1,
  }));
}

/// Back to the defaults of one category (or of everything with `null`):
/// the keys leave settings.json. Rejects with the backend's message.
export async function resetSettings(category: string | null): Promise<void> {
  const out = await ipc.settingsReset(category);
  useSettings.setState((s) => ({
    settings: out.settings,
    effective: withEffective(s, out.effective),
    version: s.version + 1,
  }));
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
  useSettings.setState((s) => ({
    settings: out.settings,
    issues: out.issues,
    effective: withEffective(s, out.effective),
    version: s.version + 1,
  }));
}
