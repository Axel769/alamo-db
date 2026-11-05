// Generated from app/src-tauri/src/settings.rs by `cargo xtask gen-settings`:
// do not edit. Every setting is declared once, there (§16).

/// The settings as `settings_get` returns them.
export type Settings = {
  appearance: {
    language: "it" | "en";
    theme: "system" | "light" | "dark";
    editor_font: string;
    editor_font_size: number;
    grid_font: string;
    grid_font_size: number;
    grid_density: "compact" | "comfortable";
    high_contrast_grid: boolean;
  };
  updates: {
    skipped_version: string;
  };
};

/// The same values by `category.field` key.
export type SettingValues = {
  "appearance.language": "it" | "en";
  "appearance.theme": "system" | "light" | "dark";
  "appearance.editor_font": string;
  "appearance.editor_font_size": number;
  "appearance.grid_font": string;
  "appearance.grid_font_size": number;
  "appearance.grid_density": "compact" | "comfortable";
  "appearance.high_contrast_grid": boolean;
  "updates.skipped_version": string;
};

export type SettingKey = keyof SettingValues;

export type SettingKind =
  | { kind: "enum"; values: readonly string[]; labels: readonly string[] }
  | { kind: "str" }
  | { kind: "bool" }
  | { kind: "int"; min: number; max: number };

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
  { key: "appearance.editor_font", category: "appearance", kind: { kind: "str" }, label: "Editor font" },
  { key: "appearance.editor_font_size", category: "appearance", kind: { kind: "int", min: 9, max: 32 }, label: "Editor text size (px)" },
  { key: "appearance.grid_font", category: "appearance", kind: { kind: "str" }, label: "Grid font" },
  { key: "appearance.grid_font_size", category: "appearance", kind: { kind: "int", min: 9, max: 24 }, label: "Grid text size (px)" },
  { key: "appearance.grid_density", category: "appearance", kind: { kind: "enum", values: ["compact", "comfortable"], labels: ["Compact (26 px rows)", "Comfortable (30 px rows)"] }, label: "Grid density" },
  { key: "appearance.high_contrast_grid", category: "appearance", kind: { kind: "bool" }, label: "High-contrast grid lines" },
  { key: "updates.skipped_version", category: "updates", kind: { kind: "str" }, label: "Skipped version" },
];

/// The defaults of this build.
export const DEFAULTS: Settings = {
  "appearance": {
    "editor_font": "ui-monospace, SFMono-Regular, Menlo, Consolas, 'DejaVu Sans Mono', monospace",
    "editor_font_size": 14,
    "grid_density": "compact",
    "grid_font": "ui-monospace, SFMono-Regular, Menlo, Consolas, 'DejaVu Sans Mono', monospace",
    "grid_font_size": 13,
    "high_contrast_grid": false,
    "language": "it",
    "theme": "system"
  },
  "updates": {
    "skipped_version": ""
  }
};
