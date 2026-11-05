// Generated from app/src-tauri/src/settings.rs by `cargo xtask gen-settings`:
// do not edit. Every setting is declared once, there (§16).

/// The settings as `settings_get` returns them.
export type Settings = {
  appearance: {
    language: "it" | "en";
    theme: "system" | "light" | "dark";
  };
  updates: {
    skipped_version: string;
  };
};

/// The same values by `category.field` key.
export type SettingValues = {
  "appearance.language": "it" | "en";
  "appearance.theme": "system" | "light" | "dark";
  "updates.skipped_version": string;
};

export type SettingKey = keyof SettingValues;

export type SettingKind = { kind: "enum"; values: readonly string[]; labels: readonly string[] } | { kind: "str" };

export type SettingDef = { key: SettingKey; category: string; kind: SettingKind; label: string };

/// The categories, in declaration order, with the English label (i18n key).
export const SETTING_CATEGORIES: readonly { id: string; label: string }[] = [
  { id: "appearance", label: "Appearance" },
  { id: "updates", label: "Updates" },
];

/// Every setting, in declaration order: what the window renders.
export const SETTING_DEFS: readonly SettingDef[] = [
  { key: "appearance.language", category: "appearance", kind: { kind: "enum", values: ["it", "en"], labels: ["Italiano", "English"] }, label: "Language" },
  { key: "appearance.theme", category: "appearance", kind: { kind: "enum", values: ["system", "light", "dark"], labels: ["Same as system", "Light", "Dark"] }, label: "Theme" },
  { key: "updates.skipped_version", category: "updates", kind: { kind: "str" }, label: "Skipped version" },
];

/// The defaults of this build.
export const DEFAULTS: Settings = {
  "appearance": {
    "language": "it",
    "theme": "system"
  },
  "updates": {
    "skipped_version": ""
  }
};
