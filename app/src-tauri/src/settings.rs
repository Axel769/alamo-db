//! App settings (§16): one `settings.json` in the config directory, owned
//! by Rust. The file holds only the values that differ from the default,
//! plus `version`; everything else is the default of this build, so a
//! default can change without migrating anybody's file.
//!
//! Every setting is declared ONCE, in the `settings!` block below: the
//! typed structs the code reads, the `DEFS` table the window and the
//! validation use, and the keys of the file all come from it.
//! `macro_rules!` is a template expanded at compile time: it writes the
//! struct, the `Default` and the table from the same lines.
//!
//! Keys are `category.field` (`appearance.theme`); on disk the object is
//! nested (`{"appearance": {"theme": "dark"}}`), readable and editable by
//! hand. A value that does not validate falls back to the default and is
//! reported once (`Issue`), never silently; a key this build does not know
//! is kept and written back untouched, so a newer build's settings survive
//! a downgrade.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// How a value is edited and validated. The variants carry what the
/// window needs to draw the control and what `validate` needs to check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// One of a fixed list of names (a `<select>`): `(name, label)`, the
    /// name is what the file holds, the label what the window shows.
    Enum(&'static [(&'static str, &'static str)]),
    /// Free text.
    Str,
    /// On or off (a checkbox).
    Bool,
    /// A whole number within the range, both ends included.
    Int { min: i64, max: i64 },
}

/// A type usable as a setting: says which `Kind` it is.
pub trait SettingType {
    const KIND: Kind;
}

impl SettingType for String {
    const KIND: Kind = Kind::Str;
}

impl SettingType for bool {
    const KIND: Kind = Kind::Bool;
}

/// The `Kind` of a field: from its range when declared (`u32 [9..=32]`,
/// the only way to get an `Int`), otherwise from the type.
macro_rules! kind {
    ($ty:ty;) => {
        <$ty as SettingType>::KIND
    };
    ($ty:ty; $min:literal, $max:literal) => {
        Kind::Int {
            min: $min,
            max: $max,
        }
    };
}

/// An enum setting: the serde names, the Rust variants and the labels of
/// the window from one list.
macro_rules! setting_enum {
    ($Name:ident { $( $Variant:ident = $name:literal => $label:literal ),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
        pub enum $Name {
            $( #[serde(rename = $name)] $Variant, )+
        }
        impl SettingType for $Name {
            const KIND: Kind = Kind::Enum(&[$( ($name, $label) ),+]);
        }
    };
}

// Language names stay in their own language, as every language picker does.
setting_enum!(Language { It = "it" => "Italiano", En = "en" => "English" });
setting_enum!(ThemePref {
    System = "system" => "Same as system",
    Light = "light" => "Light",
    Dark = "dark" => "Dark",
});
// Row heights of §9.6.2: 26 px is the compact default, 30 px the roomier
// preset; the text size is a separate setting on purpose (§9.6.2).
setting_enum!(GridDensity {
    Compact = "compact" => "Compact (26 px rows)",
    Comfortable = "comfortable" => "Comfortable (30 px rows)",
});

setting_enum!(RunShortcut {
    Current = "current" => "Statement at the cursor, or the selection",
    Selection = "selection" => "Selection, otherwise the whole script",
    All = "all" => "The whole script",
});

// Data display (§8.2): presentation only, the canonical value stays in
// the cache and goes to copy and export.
setting_enum!(DateFormat {
    Iso = "iso" => "ISO (2024-03-05 10:00:00)",
    Locale = "locale" => "Of the language (05/03/2024 10:00:00)",
});
setting_enum!(TimeZonePref {
    Utc = "utc" => "UTC, as stored",
    Local = "local" => "This computer's",
});
setting_enum!(BinaryDisplay {
    Hex = "hex" => "Hexadecimal (\\x0aff)",
    Base64 = "base64" => "Base64",
});

// Copy from the grid (⌘C / Ctrl+C): what goes between the cells.
setting_enum!(CopySeparator {
    Tab = "tab" => "Tab",
    Comma = "comma" => "Comma",
    Semicolon = "semicolon" => "Semicolon",
});
setting_enum!(QuoteMode {
    Needed = "needed" => "Only when needed",
    Always = "always" => "Always",
});

// Export defaults: the dialog starts from these, each export may differ.
setting_enum!(ExportDefault {
    Csv = "csv" => "CSV",
    Tsv = "tsv" => "TSV",
    Json = "json" => "JSON",
    Insert = "insert" => "INSERT",
});
setting_enum!(CsvDelimiter {
    Comma = "comma" => "Comma",
    Semicolon = "semicolon" => "Semicolon",
});
setting_enum!(LineEnding {
    Lf = "lf" => "LF (Unix, macOS)",
    Crlf = "crlf" => "CRLF (Windows)",
});

setting_enum!(PasswordPolicy {
    Keychain = "keychain" => "Save in the keychain when asked",
    Ask = "ask" => "Always ask, never save",
});

/// Monospace of the editor and the grid: the system's on each platform
/// (SF Mono, Consolas, DejaVu Sans Mono), the generic family last.
const MONO: &str = "ui-monospace, SFMono-Regular, Menlo, Consolas, 'DejaVu Sans Mono', monospace";

/// One setting as the window and the validation see it. `label` is
/// English and is the i18n key (`t()`), like every string of the UI.
#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    pub key: &'static str,
    pub category: &'static str,
    pub kind: Kind,
    pub label: &'static str,
}

/// `false` when the optional `overridable` literal is absent on a category.
macro_rules! or_false {
    () => {
        false
    };
    ($v:literal) => {
        $v
    };
}

macro_rules! settings {
    ($(
        $cat:ident : $Cat:ident ($cat_label:literal $(, $overridable:literal )?) {
            $( $field:ident : $ty:ty $( [ $min:literal ..= $max:literal ] )? = $default:expr, $label:literal $( , $help:literal )?; )*
        }
    )*) => {
        $(
            #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
            #[serde(default)]
            pub struct $Cat {
                $( pub $field: $ty, )*
            }
            impl Default for $Cat {
                fn default() -> Self {
                    Self { $( $field: $default, )* }
                }
            }
        )*

        /// All the settings, typed: what the code reads.
        #[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
        #[serde(default)]
        pub struct Settings {
            $( pub $cat: $Cat, )*
        }

        /// The categories in declaration order: `(id, label, overridable)`;
        /// the label is English and an i18n key (the heading in the window),
        /// `overridable` means a connection profile may override the
        /// category's settings (§16, per-connection overrides).
        pub static CATEGORIES: &[(&str, &str, bool)] = &[
            $( (stringify!($cat), $cat_label, or_false!($( $overridable )?)), )*
        ];

        /// `(key, help)` of the settings that have a line of help under
        /// the label: text for the window only (English, i18n key), so
        /// not in `SettingDef`; its readers are the TS generator and the
        /// translation test, both tests.
        #[cfg(test)]
        pub static HELP: &[(&str, &str)] = &[
            $( $( $( (concat!(stringify!($cat), ".", stringify!($field)), $help), )? )* )*
        ];

        /// All the settings, declared: what the window and `validate` read.
        pub static DEFS: &[SettingDef] = &[
            $( $(
                SettingDef {
                    key: concat!(stringify!($cat), ".", stringify!($field)),
                    category: stringify!($cat),
                    kind: kind!($ty; $( $min, $max )?),
                    label: $label,
                },
            )* )*
        ];
    };
}

settings! {
    appearance: Appearance ("Appearance") {
        language: Language = Language::It, "Language";
        theme: ThemePref = ThemePref::System, "Theme";
        editor_font: String = MONO.into(), "Editor font";
        editor_font_size: u32 [9..=32] = 14, "Editor text size (px)";
        grid_font: String = MONO.into(), "Grid font";
        grid_font_size: u32 [9..=24] = 13, "Grid text size (px)";
        grid_density: GridDensity = GridDensity::Compact, "Grid density";
        high_contrast_grid: bool = false, "High-contrast grid lines";
    }
    editor: Editor ("Editor") {
        autocomplete: bool = true, "Autocomplete while typing";
        autocomplete_delay_ms: u32 [0..=2000] = 100, "Autocomplete delay (ms)";
        uppercase_keywords: bool = false, "Uppercase keywords as you type",
            "When a space or a bracket closes a keyword; never inside strings, comments or quoted names.";
        tab_size: u32 [2..=8] = 2, "Indent width (spaces)";
        line_wrap: bool = false, "Wrap long lines";
        run_shortcut: RunShortcut = RunShortcut::Current, "Run (⌘↵ / Ctrl+Enter) executes",
            "⇧⌘↵ (Shift+Ctrl+Enter) always runs the whole script.";
        confirm_unfiltered_writes: bool = true, "Confirm UPDATE and DELETE without WHERE",
            "On production profiles the confirmation is always asked, whatever this setting.";
        history_size: u32 [50..=2000] = 200, "Query history entries";
    }
    execution: Execution ("Execution", true) {
        sql_pause_rows: u32 [50..=100_000] = 200, "Rows fetched before the SQL tab pauses",
            "Scrolling to the bottom brings another page; \"Load all\" removes the pause. The SQL is never rewritten.";
        table_page_rows: u32 [100..=10_000] = 500, "Rows per page of a table tab";
        memory_budget_mb: u32 [64..=8192] = 384, "Memory per result set (MB)",
            "The fetch pauses when a result set reaches this size; the rows already loaded stay.";
        manual_commit_default: bool = false, "Manual commit on new connections",
            "Production profiles always use manual commit.";
        idle_rollback_minutes: u32 [1..=120] = 15, "Roll back a transaction idle for (minutes)",
            "An open transaction without statements for this long is rolled back, on every profile.";
        confirm_ddl_on_production: bool = false, "Confirm schema changes (DDL) on production",
            "Irreversible operations are always confirmed; this adds the DDL that a Rollback could undo.";
        query_timeout_s: u32 [0..=86400] = 0, "Query timeout (seconds, 0 = none)",
            "The server stops a statement past this time. On MySQL it bounds only SELECT; on PostgreSQL and MariaDB, every statement. Applies from the next query.";
    }
    data: Data ("Data") {
        null_text: String = "NULL".into(), "Text shown for NULL",
            "Grid and panels only: a NULL copies and exports as nothing. Empty = NULL.";
        date_format: DateFormat = DateFormat::Iso, "Date and time format";
        timestamp_timezone: TimeZonePref = TimeZonePref::Utc, "Time zone of timestamps with time zone",
            "They are stored in UTC. Timestamps without time zone are shown as stored.";
        thousands_separator: bool = false, "Thousands separator in numbers",
            "Grouped as the language does (1.234.567,89 in Italian); copy and export keep the plain number.";
        max_cell_chars: u32 [100..=4096] = 500, "Characters shown per cell",
            "Longer values open in the cell panel. The grid receives at most 4 KB per cell.";
        binary_display: BinaryDisplay = BinaryDisplay::Hex, "Binary values",
            "Copy and export keep the database form (\\x hexadecimal).";
    }
    copy: Copy ("Copy") {
        separator: CopySeparator = CopySeparator::Tab, "Separator between cells (⌘C / Ctrl+C)",
            "Paste into the grid always reads tabs, as spreadsheets write them.";
        headers: bool = false, "Column names on the first line",
            "When more than one cell is copied, also with \"Copy selection as\".";
        quote: QuoteMode = QuoteMode::Needed, "Quotes around values",
            "\"Only when needed\": a value with the separator, quotes or a newline.";
    }
    export: Export ("Export") {
        format: ExportDefault = ExportDefault::Csv, "Format proposed by the export dialog";
        csv_delimiter: CsvDelimiter = CsvDelimiter::Comma, "CSV delimiter",
            "Excel in Italian and other European locales opens semicolon-separated files directly.";
        csv_bom: bool = false, "UTF-8 byte order mark in CSV and TSV",
            "Lets Excel on Windows read accented text; other tools may show it as a stray character.";
        line_ending: LineEnding = LineEnding::Lf, "Line ending";
        insert_batch: u32 [1..=10_000] = 500, "Rows per INSERT statement";
    }
    connections: Connections ("Connections") {
        connect_timeout_s: u32 [3..=120] = 15, "Connection timeout (seconds)",
            "Database server and SSH bastion. Applies to the next connections.";
        tcp_keepalive_s: u32 [5..=600] = 30, "TCP keep-alive after idle (seconds)",
            "Keeps NATs and firewalls from dropping an idle connection. Applies to the next connections.";
        ssh_keepalive_s: u32 [5..=600] = 30, "SSH keep-alive (seconds)",
            "Applies to the next tunnels.";
        auto_reconnect: bool = true, "Reconnect automatically",
            "Off: a lost connection is reported and Reconnect is up to you.";
        password_policy: PasswordPolicy = PasswordPolicy::Keychain, "Passwords",
            "\"Always ask\": the save box disappears and passwords already saved are removed at the next connection.";
    }
    updates: Updates ("Updates") {
        skipped_version: String = String::new(), "Skipped version";
    }
}

impl Connections {
    /// What the database drivers get at connect (§16).
    pub fn net(&self) -> alamo_core::NetOptions {
        alamo_core::NetOptions {
            connect_timeout: Duration::from_secs(self.connect_timeout_s.into()),
            tcp_keepalive: Duration::from_secs(self.tcp_keepalive_s.into()),
        }
    }

    /// What the SSH tunnel gets at open (§16).
    pub fn tunnel_net(&self) -> alamo_tunnel::TunnelNet {
        alamo_tunnel::TunnelNet {
            connect_timeout: Duration::from_secs(self.connect_timeout_s.into()),
            keepalive: Duration::from_secs(self.ssh_keepalive_s.into()),
        }
    }
}

impl Execution {
    /// The limits a session takes (§16).
    pub fn limits(&self) -> alamo_session::LimitsIn {
        alamo_session::LimitsIn {
            pause_rows: u64::from(self.sql_pause_rows),
            page_rows: u64::from(self.table_page_rows),
            soft_budget_bytes: u64::from(self.memory_budget_mb) * 1024 * 1024,
            idle_rollback: std::time::Duration::from_secs(
                u64::from(self.idle_rollback_minutes) * 60,
            ),
            query_timeout: std::time::Duration::from_secs(u64::from(self.query_timeout_s)),
        }
    }
}

/// `app/src/settingsSchema.ts`: the same table for the frontend, generated
/// so the two cannot drift. A test compares it with the file on disk;
/// `cargo xtask gen-settings` writes it.
#[cfg(test)]
fn typescript() -> String {
    let defaults = to_tree(&Settings::default());
    let ts_type = |kind: Kind| match kind {
        Kind::Enum(values) => values
            .iter()
            .map(|(name, _)| format!("{name:?}"))
            .collect::<Vec<_>>()
            .join(" | "),
        Kind::Str => "string".into(),
        Kind::Bool => "boolean".into(),
        Kind::Int { .. } => "number".into(),
    };
    let ts_kind = |kind: Kind| match kind {
        Kind::Enum(values) => format!(
            "{{ kind: \"enum\", values: [{}], labels: [{}] }}",
            values
                .iter()
                .map(|(name, _)| format!("{name:?}"))
                .collect::<Vec<_>>()
                .join(", "),
            values
                .iter()
                .map(|(_, label)| format!("{label:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Kind::Str => "{ kind: \"str\" }".into(),
        Kind::Bool => "{ kind: \"bool\" }".into(),
        Kind::Int { min, max } => format!("{{ kind: \"int\", min: {min}, max: {max} }}"),
    };
    let mut out = String::from(
        "// Generated from app/src-tauri/src/settings.rs by `cargo xtask gen-settings`:\n\
         // do not edit. Every setting is declared once, there (§16).\n\n\
         /// The settings as `settings_get` returns them.\n\
         export type Settings = {\n",
    );
    for (cat, _, _) in CATEGORIES {
        out.push_str(&format!("  {cat}: {{\n"));
        for def in DEFS.iter().filter(|d| d.category == *cat) {
            let field = def.key.rsplit('.').next().unwrap_or(def.key);
            out.push_str(&format!("    {field}: {};\n", ts_type(def.kind)));
        }
        out.push_str("  };\n");
    }
    out.push_str(
        "};\n\n/// The same values by `category.field` key.\nexport type SettingValues = {\n",
    );
    for def in DEFS {
        out.push_str(&format!("  {:?}: {};\n", def.key, ts_type(def.kind)));
    }
    out.push_str(
        "};\n\n\
         export type SettingKey = keyof SettingValues;\n\n\
         export type SettingKind =\n\
         \x20 | { kind: \"enum\"; values: readonly string[]; labels: readonly string[] }\n\
         \x20 | { kind: \"str\" }\n\
         \x20 | { kind: \"bool\" }\n\
         \x20 | { kind: \"int\"; min: number; max: number };\n\n\
         export type SettingDef = { key: SettingKey; category: string; kind: SettingKind; label: string; help: string };\n\n\
         /// The categories, in declaration order, with the English label (i18n\n\
         /// key) and whether a connection profile may override them (§16).\n\
         export const SETTING_CATEGORIES: readonly { id: string; label: string; overridable: boolean }[] = [\n",
    );
    for (cat, label, overridable) in CATEGORIES {
        out.push_str(&format!(
            "  {{ id: {cat:?}, label: {label:?}, overridable: {overridable} }},\n"
        ));
    }
    out.push_str(
        "];\n\n\
         /// Every setting, in declaration order: what the window renders.\n\
         export const SETTING_DEFS: readonly SettingDef[] = [\n",
    );
    for def in DEFS {
        out.push_str(&format!(
            "  {{ key: {:?}, category: {:?}, kind: {}, label: {:?}, help: {:?} }},\n",
            def.key,
            def.category,
            ts_kind(def.kind),
            def.label,
            HELP.iter()
                .find(|(k, _)| *k == def.key)
                .map_or("", |(_, h)| *h)
        ));
    }
    out.push_str("];\n\n/// The defaults of this build.\nexport const DEFAULTS: Settings = ");
    out.push_str(&serde_json::to_string_pretty(&defaults).expect("serializing defaults"));
    out.push_str(";\n");
    out
}

/// Format of the file; bumped only if the shape changes in a way the
/// `version 1` reader cannot take.
const FILE_VERSION: u64 = 1;
const FILE_NAME: &str = "settings.json";

/// A value the file had and this build could not take: told to the user
/// once, in the window. `found` is the JSON text of the value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Issue {
    pub key: String,
    pub found: String,
    pub message: String,
}

/// The settings in memory and on disk. Commands read `current`; `apply`
/// and `reset` validate, write the file and only then swap the memory, so
/// memory and file never disagree.
pub struct SettingsStore {
    path: PathBuf,
    current: RwLock<Settings>,
    /// Leaves of the file this build does not know, written back as they
    /// were.
    unknown: Mutex<Value>,
    /// Problems found at load, handed to the window once.
    issues: Mutex<Vec<Issue>>,
}

impl SettingsStore {
    /// Reads `settings.json` in `dir`. A missing file is the normal first
    /// start: defaults, no issue. An unreadable file is renamed to
    /// `settings.json.invalid` (nothing of the user's is lost) and reported.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(FILE_NAME);
        let mut issues = Vec::new();
        let tree = match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
            Err(e) => {
                issues.push(unreadable(&path, &e.to_string()));
                json!({})
            }
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(v) if v.is_object() => v,
                Ok(_) => {
                    issues.push(unreadable(&path, "not a JSON object"));
                    json!({})
                }
                Err(e) => {
                    issues.push(unreadable(&path, &e.to_string()));
                    json!({})
                }
            },
        };
        let (settings, unknown, mut found) = split(tree);
        issues.append(&mut found);
        Self {
            path,
            current: RwLock::new(settings),
            unknown: Mutex::new(unknown),
            issues: Mutex::new(issues),
        }
    }

    pub fn snapshot(&self) -> Settings {
        self.current
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The load problems, once: the second call returns nothing.
    pub fn take_issues(&self) -> Vec<Issue> {
        std::mem::take(&mut *self.issues.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Sets the given keys (`category.field` → value). All or nothing: one
    /// invalid value and nothing changes.
    pub fn apply(&self, patch: &Map<String, Value>) -> Result<Settings, String> {
        let mut tree = to_tree(&self.snapshot());
        for (key, value) in patch {
            let def = DEFS
                .iter()
                .find(|d| d.key == *key)
                .ok_or_else(|| format!("Unknown setting {key}"))?;
            validate(def, value)?;
            set_leaf(&mut tree, key, value.clone());
        }
        self.commit(tree)
    }

    /// Back to the defaults: of one category, or of everything.
    pub fn reset(&self, category: Option<&str>) -> Result<Settings, String> {
        if let Some(c) = category {
            if !CATEGORIES.iter().any(|(id, _, _)| *id == c) {
                return Err(format!("Unknown settings category {c}"));
            }
        }
        let mut tree = to_tree(&self.snapshot());
        let defaults = to_tree(&Settings::default());
        for def in DEFS
            .iter()
            .filter(|d| category.map_or(true, |c| d.category == c))
        {
            let v = leaf(&defaults, def.key).cloned().unwrap_or(Value::Null);
            set_leaf(&mut tree, def.key, v);
        }
        self.commit(tree)
    }

    /// Writes the file and swaps the memory. `tree` holds every known leaf,
    /// already validated.
    fn commit(&self, tree: Value) -> Result<Settings, String> {
        let settings: Settings =
            serde_json::from_value(tree).map_err(|e| format!("settings: {e}"))?;
        let mut disk = self
            .unknown
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        for (key, value) in non_default_leaves(&settings) {
            set_leaf(&mut disk, &key, value);
        }
        disk["version"] = json!(FILE_VERSION);
        write_atomic(&self.path, &disk)?;
        *self.current.write().unwrap_or_else(|p| p.into_inner()) = settings.clone();
        Ok(settings)
    }
}

fn unreadable(path: &Path, why: &str) -> Issue {
    let invalid = path.with_extension("json.invalid");
    let _ = std::fs::rename(path, &invalid);
    Issue {
        key: FILE_NAME.into(),
        found: String::new(),
        message: format!(
            "{FILE_NAME} could not be read ({why}): it was renamed to {} and the defaults are in use",
            invalid
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ),
    }
}

/// Splits the file's tree into the typed settings (invalid leaves dropped
/// and reported) and what this build does not know.
fn split(mut tree: Value) -> (Settings, Value, Vec<Issue>) {
    let mut issues = Vec::new();
    let mut known = json!({});
    for def in DEFS {
        let Some(v) = take_leaf(&mut tree, def.key) else {
            continue;
        };
        match validate(def, &v) {
            Ok(()) => set_leaf(&mut known, def.key, v),
            Err(message) => issues.push(Issue {
                key: def.key.into(),
                found: shorten(&v.to_string()),
                message,
            }),
        }
    }
    if let Some(o) = tree.as_object_mut() {
        o.remove("version");
    }
    // Every known leaf validated: the typed form cannot fail to build.
    let settings = serde_json::from_value(known).unwrap_or_default();
    (settings, tree, issues)
}

fn shorten(s: &str) -> String {
    const MAX: usize = 80;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let cut: String = s.chars().take(MAX).collect();
    format!("{cut}…")
}

/// The value is acceptable for the setting. The message is for the user:
/// which setting and what it takes.
fn validate(def: &SettingDef, v: &Value) -> Result<(), String> {
    match def.kind {
        Kind::Enum(values) => match v.as_str() {
            Some(s) if values.iter().any(|(name, _)| *name == s) => Ok(()),
            _ => Err(format!(
                "{} must be one of {}",
                def.label,
                values
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        },
        Kind::Str => v
            .is_string()
            .then_some(())
            .ok_or_else(|| format!("{} must be text", def.label)),
        Kind::Bool => v
            .is_boolean()
            .then_some(())
            .ok_or_else(|| format!("{} must be true or false", def.label)),
        Kind::Int { min, max } => match v.as_i64() {
            Some(n) if (min..=max).contains(&n) => Ok(()),
            _ => Err(format!(
                "{} must be a whole number between {min} and {max}",
                def.label
            )),
        },
    }
}

/// Whether a connection profile may override the settings of this
/// category (§16, per-connection overrides).
pub fn category_overridable(cat: &str) -> bool {
    CATEGORIES.iter().any(|(id, _, over)| *id == cat && *over)
}

/// The global settings with a profile's overrides applied (§16): only the
/// keys of an overridable category and only valid values; anything else is
/// returned as an `Issue` and ignored, so a stale or hand-edited profile
/// cannot break the connection. The frontend never merges — this is the
/// one place.
pub fn effective(global: &Settings, overrides: &Map<String, Value>) -> (Settings, Vec<Issue>) {
    let mut tree = to_tree(global);
    let mut issues = Vec::new();
    for (key, value) in overrides {
        let Some(def) = DEFS.iter().find(|d| d.key == *key) else {
            issues.push(Issue {
                key: key.clone(),
                found: shorten(&value.to_string()),
                message: format!("Unknown setting {key}"),
            });
            continue;
        };
        if !category_overridable(def.category) {
            issues.push(Issue {
                key: key.clone(),
                found: shorten(&value.to_string()),
                message: format!("{} cannot be set per connection", def.label),
            });
            continue;
        }
        match validate(def, value) {
            Ok(()) => set_leaf(&mut tree, key, value.clone()),
            Err(message) => issues.push(Issue {
                key: key.clone(),
                found: shorten(&value.to_string()),
                message,
            }),
        }
    }
    (serde_json::from_value(tree).unwrap_or_default(), issues)
}

fn to_tree(s: &Settings) -> Value {
    serde_json::to_value(s).expect("serializing Settings")
}

/// `(key, value)` of every setting that differs from its default: what
/// goes to disk.
fn non_default_leaves(s: &Settings) -> Vec<(String, Value)> {
    let tree = to_tree(s);
    let defaults = to_tree(&Settings::default());
    DEFS.iter()
        .filter_map(|d| {
            let v = leaf(&tree, d.key)?;
            (leaf(&defaults, d.key) != Some(v)).then(|| (d.key.to_string(), v.clone()))
        })
        .collect()
}

fn leaf<'a>(tree: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.').try_fold(tree, |node, part| node.get(part))
}

/// Sets the leaf, creating the objects on the way; a non-object on the
/// way is replaced (hand-edited file).
fn set_leaf(tree: &mut Value, key: &str, value: Value) {
    let mut node = tree;
    let mut parts = key.split('.').peekable();
    while let Some(part) = parts.next() {
        if !node.is_object() {
            *node = json!({});
        }
        let obj = node.as_object_mut().expect("object");
        if parts.peek().is_none() {
            obj.insert(part.into(), value);
            return;
        }
        node = obj.entry(part).or_insert_with(|| json!({}));
    }
}

/// Removes the leaf and returns it, pruning the objects left empty.
fn take_leaf(tree: &mut Value, key: &str) -> Option<Value> {
    let (first, rest) = key.split_once('.')?;
    let obj = tree.as_object_mut()?;
    let child = obj.get_mut(first)?;
    let taken = if rest.contains('.') {
        take_leaf(child, rest)
    } else {
        child.as_object_mut()?.remove(rest)
    };
    if child.as_object().is_some_and(Map::is_empty) {
        obj.remove(first);
    }
    taken
}

/// Temporary file next to the target, then rename: a crash halfway leaves
/// the old file whole.
fn write_atomic(path: &Path, v: &Value) -> Result<(), String> {
    let failed = |e: std::io::Error| format!("Settings file could not be written: {e}");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut text = serde_json::to_string_pretty(v).map_err(|e| failed(e.into()))?;
    text.push('\n');
    std::fs::write(&tmp, text).map_err(failed)?;
    std::fs::rename(&tmp, path).map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder per test, removed at the end (no temp-dir crate: the
    /// standard library is enough here).
    struct TempDir(PathBuf);

    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn dir() -> TempDir {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("alamo-settings-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    fn read(dir: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join(FILE_NAME)).unwrap()).unwrap()
    }

    fn patch(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    /// The session's own defaults are the M0 values; the settings must
    /// default to the same, or a fresh install would behave differently
    /// from the tests of the session crate.
    /// The format names of the Export setting are the exporter's.
    #[test]
    fn export_format_names_match_the_exporter() {
        let Kind::Enum(values) = ExportDefault::KIND else {
            panic!("enum")
        };
        for (name, _) in values {
            serde_json::from_value::<alamo_session::export::ExportFormat>(json!(name))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    /// The defaults shown in the window are the ones the crates use when
    /// nobody says anything (bench, tests).
    #[test]
    fn connections_defaults_match_the_crates() {
        let c = Connections::default();
        assert_eq!(c.net(), alamo_core::NetOptions::default());
        assert_eq!(c.tunnel_net(), alamo_tunnel::TunnelNet::default());
    }

    #[test]
    fn execution_defaults_match_the_session() {
        assert_eq!(
            Execution::default().limits(),
            alamo_session::LimitsIn::default()
        );
    }

    #[test]
    fn first_start_is_defaults_without_file() {
        let d = dir();
        let store = SettingsStore::load(d.path());
        assert_eq!(store.snapshot(), Settings::default());
        assert!(store.take_issues().is_empty());
        assert!(!d.path().join(FILE_NAME).exists());
    }

    #[test]
    fn only_non_default_values_reach_the_disk() {
        let d = dir();
        let store = SettingsStore::load(d.path());
        let s = store
            .apply(&patch(&[("appearance.theme", json!("dark"))]))
            .unwrap();
        assert_eq!(s.appearance.theme, ThemePref::Dark);
        assert_eq!(
            read(d.path()),
            json!({"version": 1, "appearance": {"theme": "dark"}})
        );
        // Back to the default: the key goes away, the file stays valid.
        store
            .apply(&patch(&[("appearance.theme", json!("system"))]))
            .unwrap();
        assert_eq!(read(d.path()), json!({"version": 1}));
        // A new store reads what was written.
        store
            .apply(&patch(&[("appearance.language", json!("en"))]))
            .unwrap();
        let again = SettingsStore::load(d.path());
        assert_eq!(again.snapshot().appearance.language, Language::En);
    }

    #[test]
    fn invalid_patch_changes_nothing() {
        let d = dir();
        let store = SettingsStore::load(d.path());
        let err = store
            .apply(&patch(&[
                ("appearance.language", json!("en")),
                ("appearance.theme", json!("blue")),
            ]))
            .unwrap_err();
        assert_eq!(err, "Theme must be one of system, light, dark");
        assert_eq!(store.snapshot(), Settings::default());
        assert!(!d.path().join(FILE_NAME).exists());
        assert_eq!(
            store.apply(&patch(&[("nope.key", json!(1))])).unwrap_err(),
            "Unknown setting nope.key"
        );
        assert_eq!(
            store
                .apply(&patch(&[("updates.skipped_version", json!(3))]))
                .unwrap_err(),
            "Skipped version must be text"
        );
        for bad in [json!(8), json!(33), json!(12.5), json!("12")] {
            assert_eq!(
                store
                    .apply(&patch(&[("appearance.editor_font_size", bad)]))
                    .unwrap_err(),
                "Editor text size (px) must be a whole number between 9 and 32"
            );
        }
        assert_eq!(
            store
                .apply(&patch(&[("appearance.high_contrast_grid", json!("yes"))]))
                .unwrap_err(),
            "High-contrast grid lines must be true or false"
        );
        let s = store
            .apply(&patch(&[
                ("appearance.editor_font_size", json!(32)),
                ("appearance.high_contrast_grid", json!(true)),
            ]))
            .unwrap();
        assert_eq!(s.appearance.editor_font_size, 32);
        assert!(s.appearance.high_contrast_grid);
    }

    #[test]
    fn hand_edited_file_invalid_values_fall_back_and_are_reported() {
        let d = dir();
        std::fs::write(
            d.path().join(FILE_NAME),
            r#"{"version": 1, "appearance": {"theme": 7, "language": "en"},
                "future": {"thing": true}, "updates": {"other": 1}}"#,
        )
        .unwrap();
        let store = SettingsStore::load(d.path());
        let s = store.snapshot();
        assert_eq!(s.appearance.theme, ThemePref::System);
        assert_eq!(s.appearance.language, Language::En);
        let issues = store.take_issues();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].key, "appearance.theme");
        assert_eq!(issues[0].found, "7");
        assert_eq!(
            issues[0].message,
            "Theme must be one of system, light, dark"
        );
        assert!(store.take_issues().is_empty(), "reported once");
        // Unknown keys survive a write; the invalid value does not come back.
        store
            .apply(&patch(&[("updates.skipped_version", json!("1.2.3"))]))
            .unwrap();
        assert_eq!(
            read(d.path()),
            json!({
                "version": 1,
                "appearance": {"language": "en"},
                "future": {"thing": true},
                "updates": {"other": 1, "skipped_version": "1.2.3"}
            })
        );
    }

    #[test]
    fn unreadable_file_is_set_aside_not_overwritten() {
        let d = dir();
        std::fs::write(d.path().join(FILE_NAME), "{not json").unwrap();
        let store = SettingsStore::load(d.path());
        assert_eq!(store.snapshot(), Settings::default());
        let issues = store.take_issues();
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("settings.json.invalid"));
        assert_eq!(
            std::fs::read_to_string(d.path().join("settings.json.invalid")).unwrap(),
            "{not json"
        );
        assert!(!d.path().join(FILE_NAME).exists());
        store
            .apply(&patch(&[("appearance.theme", json!("light"))]))
            .unwrap();
        assert_eq!(
            read(d.path()),
            json!({"version": 1, "appearance": {"theme": "light"}})
        );
    }

    #[test]
    fn effective_applies_only_valid_overridable_overrides() {
        let global = Settings::default();
        // A valid override of an overridable category wins; the rest is the global.
        let (eff, issues) = effective(
            &global,
            &patch(&[("execution.sql_pause_rows", json!(5000))]),
        );
        assert_eq!(eff.execution.sql_pause_rows, 5000);
        assert!(issues.is_empty());

        // A non-overridable category, an unknown key and an out-of-range
        // value are reported and ignored; the effective stays the default.
        let (eff, issues) = effective(
            &global,
            &patch(&[
                ("appearance.theme", json!("dark")),
                ("execution.sql_pause_rows", json!(5)),
                ("nope.key", json!(1)),
            ]),
        );
        assert_eq!(eff, global);
        let mut msgs: Vec<&str> = issues.iter().map(|i| i.message.as_str()).collect();
        msgs.sort_unstable();
        assert_eq!(
            msgs,
            vec![
                "Rows fetched before the SQL tab pauses must be a whole number between 50 and 100000",
                "Theme cannot be set per connection",
                "Unknown setting nope.key",
            ]
        );
    }

    #[test]
    fn reset_one_category_or_all() {
        let d = dir();
        let store = SettingsStore::load(d.path());
        store
            .apply(&patch(&[
                ("appearance.theme", json!("dark")),
                ("updates.skipped_version", json!("9.9")),
            ]))
            .unwrap();
        let s = store.reset(Some("appearance")).unwrap();
        assert_eq!(s.appearance, Appearance::default());
        assert_eq!(s.updates.skipped_version, "9.9");
        assert_eq!(
            read(d.path()),
            json!({"version": 1, "updates": {"skipped_version": "9.9"}})
        );
        assert_eq!(
            store.reset(Some("nope")).unwrap_err(),
            "Unknown settings category nope"
        );
        assert_eq!(store.reset(None).unwrap(), Settings::default());
        assert_eq!(read(d.path()), json!({"version": 1}));
    }

    #[test]
    fn defs_cover_the_struct_and_enum_names_match_serde() {
        let tree = to_tree(&Settings::default());
        let mut leaves = Vec::new();
        collect_leaves(&tree, String::new(), &mut leaves);
        let mut keys: Vec<&str> = DEFS.iter().map(|d| d.key).collect();
        keys.sort_unstable();
        leaves.sort_unstable();
        assert_eq!(keys, leaves, "every field has a def and vice versa");
        for def in DEFS {
            if let Kind::Enum(values) = def.kind {
                for (v, _) in values {
                    let mut t = tree.clone();
                    set_leaf(&mut t, def.key, json!(v));
                    serde_json::from_value::<Settings>(t)
                        .unwrap_or_else(|e| panic!("{}: {v} not a serde name: {e}", def.key));
                }
            }
        }
    }

    fn collect_leaves(v: &Value, prefix: String, out: &mut Vec<&'static str>) {
        match v.as_object() {
            Some(o) => {
                for (k, child) in o {
                    let key = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    collect_leaves(child, key, out);
                }
            }
            None => out.push(Box::leak(prefix.into_boxed_str())),
        }
    }

    /// The frontend's table is this one: `ALAMO_GEN_SETTINGS=1` writes it
    /// (`cargo xtask gen-settings`), otherwise it must already match.
    #[test]
    fn settings_schema_ts_is_current() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/settingsSchema.ts");
        let want = typescript();
        if std::env::var_os("ALAMO_GEN_SETTINGS").is_some() {
            std::fs::write(&path, want).unwrap();
            return;
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            have == want,
            "app/src/settingsSchema.ts is out of date: run `cargo xtask gen-settings`"
        );
    }

    /// Every label of `DEFS` (setting, category, enum option) has its
    /// Italian string: the window shows them through `t()`, which falls
    /// back to English in silence.
    #[test]
    fn labels_are_translated() {
        let i18n =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/i18n.ts"))
                .unwrap();
        let mut texts: Vec<&str> = CATEGORIES.iter().map(|(_, label, _)| *label).collect();
        texts.extend(HELP.iter().map(|(_, help)| *help));
        for def in DEFS {
            texts.push(def.label);
            if let Kind::Enum(values) = def.kind {
                texts.extend(values.iter().map(|(_, label)| *label));
            }
        }
        texts.sort_unstable();
        texts.dedup();
        let missing: Vec<&str> = texts
            .into_iter()
            .filter(|s| {
                let js = s.replace('\\', "\\\\").replace('"', "\\\"");
                !i18n.contains(&format!("  \"{js}\": \""))
            })
            .collect();
        assert!(missing.is_empty(), "missing in i18n.ts IT: {missing:?}");
    }
}
