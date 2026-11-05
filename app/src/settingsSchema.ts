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
  editor: {
    autocomplete: boolean;
    autocomplete_delay_ms: number;
    uppercase_keywords: boolean;
    tab_size: number;
    line_wrap: boolean;
    run_shortcut: "current" | "selection" | "all";
    confirm_unfiltered_writes: boolean;
    history_size: number;
  };
  execution: {
    sql_pause_rows: number;
    table_page_rows: number;
    memory_budget_mb: number;
    manual_commit_default: boolean;
    idle_rollback_minutes: number;
    confirm_ddl_on_production: boolean;
    query_timeout_s: number;
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
  "editor.autocomplete": boolean;
  "editor.autocomplete_delay_ms": number;
  "editor.uppercase_keywords": boolean;
  "editor.tab_size": number;
  "editor.line_wrap": boolean;
  "editor.run_shortcut": "current" | "selection" | "all";
  "editor.confirm_unfiltered_writes": boolean;
  "editor.history_size": number;
  "execution.sql_pause_rows": number;
  "execution.table_page_rows": number;
  "execution.memory_budget_mb": number;
  "execution.manual_commit_default": boolean;
  "execution.idle_rollback_minutes": number;
  "execution.confirm_ddl_on_production": boolean;
  "execution.query_timeout_s": number;
  "updates.skipped_version": string;
};

export type SettingKey = keyof SettingValues;

export type SettingKind =
  | { kind: "enum"; values: readonly string[]; labels: readonly string[] }
  | { kind: "str" }
  | { kind: "bool" }
  | { kind: "int"; min: number; max: number };

export type SettingDef = { key: SettingKey; category: string; kind: SettingKind; label: string; help: string };

/// The categories, in declaration order, with the English label (i18n key).
export const SETTING_CATEGORIES: readonly { id: string; label: string }[] = [
  { id: "appearance", label: "Appearance" },
  { id: "editor", label: "Editor" },
  { id: "execution", label: "Execution" },
  { id: "updates", label: "Updates" },
];

/// Every setting, in declaration order: what the window renders.
export const SETTING_DEFS: readonly SettingDef[] = [
  { key: "appearance.language", category: "appearance", kind: { kind: "enum", values: ["it", "en"], labels: ["Italiano", "English"] }, label: "Language", help: "" },
  { key: "appearance.theme", category: "appearance", kind: { kind: "enum", values: ["system", "light", "dark"], labels: ["Same as system", "Light", "Dark"] }, label: "Theme", help: "" },
  { key: "appearance.editor_font", category: "appearance", kind: { kind: "str" }, label: "Editor font", help: "" },
  { key: "appearance.editor_font_size", category: "appearance", kind: { kind: "int", min: 9, max: 32 }, label: "Editor text size (px)", help: "" },
  { key: "appearance.grid_font", category: "appearance", kind: { kind: "str" }, label: "Grid font", help: "" },
  { key: "appearance.grid_font_size", category: "appearance", kind: { kind: "int", min: 9, max: 24 }, label: "Grid text size (px)", help: "" },
  { key: "appearance.grid_density", category: "appearance", kind: { kind: "enum", values: ["compact", "comfortable"], labels: ["Compact (26 px rows)", "Comfortable (30 px rows)"] }, label: "Grid density", help: "" },
  { key: "appearance.high_contrast_grid", category: "appearance", kind: { kind: "bool" }, label: "High-contrast grid lines", help: "" },
  { key: "editor.autocomplete", category: "editor", kind: { kind: "bool" }, label: "Autocomplete while typing", help: "" },
  { key: "editor.autocomplete_delay_ms", category: "editor", kind: { kind: "int", min: 0, max: 2000 }, label: "Autocomplete delay (ms)", help: "" },
  { key: "editor.uppercase_keywords", category: "editor", kind: { kind: "bool" }, label: "Uppercase keywords as you type", help: "When a space or a bracket closes a keyword; never inside strings, comments or quoted names." },
  { key: "editor.tab_size", category: "editor", kind: { kind: "int", min: 2, max: 8 }, label: "Indent width (spaces)", help: "" },
  { key: "editor.line_wrap", category: "editor", kind: { kind: "bool" }, label: "Wrap long lines", help: "" },
  { key: "editor.run_shortcut", category: "editor", kind: { kind: "enum", values: ["current", "selection", "all"], labels: ["Statement at the cursor, or the selection", "Selection, otherwise the whole script", "The whole script"] }, label: "Run (⌘↵ / Ctrl+Enter) executes", help: "⇧⌘↵ (Shift+Ctrl+Enter) always runs the whole script." },
  { key: "editor.confirm_unfiltered_writes", category: "editor", kind: { kind: "bool" }, label: "Confirm UPDATE and DELETE without WHERE", help: "On production profiles the confirmation is always asked, whatever this setting." },
  { key: "editor.history_size", category: "editor", kind: { kind: "int", min: 50, max: 2000 }, label: "Query history entries", help: "" },
  { key: "execution.sql_pause_rows", category: "execution", kind: { kind: "int", min: 50, max: 100000 }, label: "Rows fetched before the SQL tab pauses", help: "Scrolling to the bottom brings another page; \"Load all\" removes the pause. The SQL is never rewritten." },
  { key: "execution.table_page_rows", category: "execution", kind: { kind: "int", min: 100, max: 10000 }, label: "Rows per page of a table tab", help: "" },
  { key: "execution.memory_budget_mb", category: "execution", kind: { kind: "int", min: 64, max: 8192 }, label: "Memory per result set (MB)", help: "The fetch pauses when a result set reaches this size; the rows already loaded stay." },
  { key: "execution.manual_commit_default", category: "execution", kind: { kind: "bool" }, label: "Manual commit on new connections", help: "Production profiles always use manual commit." },
  { key: "execution.idle_rollback_minutes", category: "execution", kind: { kind: "int", min: 1, max: 120 }, label: "Roll back a transaction idle for (minutes)", help: "An open transaction without statements for this long is rolled back, on every profile." },
  { key: "execution.confirm_ddl_on_production", category: "execution", kind: { kind: "bool" }, label: "Confirm schema changes (DDL) on production", help: "Irreversible operations are always confirmed; this adds the DDL that a Rollback could undo." },
  { key: "execution.query_timeout_s", category: "execution", kind: { kind: "int", min: 0, max: 86400 }, label: "Query timeout (seconds, 0 = none)", help: "The server stops a statement past this time. On MySQL it bounds only SELECT; on PostgreSQL and MariaDB, every statement. Applies from the next query." },
  { key: "updates.skipped_version", category: "updates", kind: { kind: "str" }, label: "Skipped version", help: "" },
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
  "editor": {
    "autocomplete": true,
    "autocomplete_delay_ms": 100,
    "confirm_unfiltered_writes": true,
    "history_size": 200,
    "line_wrap": false,
    "run_shortcut": "current",
    "tab_size": 2,
    "uppercase_keywords": false
  },
  "execution": {
    "confirm_ddl_on_production": false,
    "idle_rollback_minutes": 15,
    "manual_commit_default": false,
    "memory_budget_mb": 384,
    "query_timeout_s": 0,
    "sql_pause_rows": 200,
    "table_page_rows": 500
  },
  "updates": {
    "skipped_version": ""
  }
};
