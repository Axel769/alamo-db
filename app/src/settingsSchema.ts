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
  data: {
    null_text: string;
    date_format: "iso" | "locale";
    timestamp_timezone: "utc" | "local";
    thousands_separator: boolean;
    max_cell_chars: number;
    binary_display: "hex" | "base64";
  };
  copy: {
    separator: "tab" | "comma" | "semicolon";
    headers: boolean;
    quote: "needed" | "always";
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
  "data.null_text": string;
  "data.date_format": "iso" | "locale";
  "data.timestamp_timezone": "utc" | "local";
  "data.thousands_separator": boolean;
  "data.max_cell_chars": number;
  "data.binary_display": "hex" | "base64";
  "copy.separator": "tab" | "comma" | "semicolon";
  "copy.headers": boolean;
  "copy.quote": "needed" | "always";
  "updates.skipped_version": string;
};

export type SettingKey = keyof SettingValues;

export type SettingKind =
  | { kind: "enum"; values: readonly string[]; labels: readonly string[] }
  | { kind: "str" }
  | { kind: "bool" }
  | { kind: "int"; min: number; max: number };

export type SettingDef = { key: SettingKey; category: string; kind: SettingKind; label: string; help: string };

/// The categories, in declaration order, with the English label (i18n
/// key) and whether a connection profile may override them (§16).
export const SETTING_CATEGORIES: readonly { id: string; label: string; overridable: boolean }[] = [
  { id: "appearance", label: "Appearance", overridable: false },
  { id: "editor", label: "Editor", overridable: false },
  { id: "execution", label: "Execution", overridable: true },
  { id: "data", label: "Data", overridable: false },
  { id: "copy", label: "Copy", overridable: false },
  { id: "updates", label: "Updates", overridable: false },
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
  { key: "data.null_text", category: "data", kind: { kind: "str" }, label: "Text shown for NULL", help: "Grid and panels only: a NULL copies and exports as nothing. Empty = NULL." },
  { key: "data.date_format", category: "data", kind: { kind: "enum", values: ["iso", "locale"], labels: ["ISO (2024-03-05 10:00:00)", "Of the language (05/03/2024 10:00:00)"] }, label: "Date and time format", help: "" },
  { key: "data.timestamp_timezone", category: "data", kind: { kind: "enum", values: ["utc", "local"], labels: ["UTC, as stored", "This computer's"] }, label: "Time zone of timestamps with time zone", help: "They are stored in UTC. Timestamps without time zone are shown as stored." },
  { key: "data.thousands_separator", category: "data", kind: { kind: "bool" }, label: "Thousands separator in numbers", help: "Grouped as the language does (1.234.567,89 in Italian); copy and export keep the plain number." },
  { key: "data.max_cell_chars", category: "data", kind: { kind: "int", min: 100, max: 4096 }, label: "Characters shown per cell", help: "Longer values open in the cell panel. The grid receives at most 4 KB per cell." },
  { key: "data.binary_display", category: "data", kind: { kind: "enum", values: ["hex", "base64"], labels: ["Hexadecimal (\\x0aff)", "Base64"] }, label: "Binary values", help: "Copy and export keep the database form (\\x hexadecimal)." },
  { key: "copy.separator", category: "copy", kind: { kind: "enum", values: ["tab", "comma", "semicolon"], labels: ["Tab", "Comma", "Semicolon"] }, label: "Separator between cells (⌘C / Ctrl+C)", help: "Paste into the grid always reads tabs, as spreadsheets write them." },
  { key: "copy.headers", category: "copy", kind: { kind: "bool" }, label: "Column names on the first line", help: "When more than one cell is copied, also with \"Copy selection as\"." },
  { key: "copy.quote", category: "copy", kind: { kind: "enum", values: ["needed", "always"], labels: ["Only when needed", "Always"] }, label: "Quotes around values", help: "\"Only when needed\": a value with the separator, quotes or a newline." },
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
  "copy": {
    "headers": false,
    "quote": "needed",
    "separator": "tab"
  },
  "data": {
    "binary_display": "hex",
    "date_format": "iso",
    "max_cell_chars": 500,
    "null_text": "NULL",
    "thousands_separator": false,
    "timestamp_timezone": "utc"
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
