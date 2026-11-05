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

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// How a value is edited and validated. The variants carry what the
/// window needs to draw the control and what `validate` needs to check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// One of a fixed list of names (a `<select>`).
    Enum(&'static [&'static str]),
    /// Free text.
    Str,
}

/// A type usable as a setting: says which `Kind` it is.
pub trait SettingType {
    const KIND: Kind;
}

impl SettingType for String {
    const KIND: Kind = Kind::Str;
}

/// An enum setting: the serde names and the Rust variants from one list.
macro_rules! setting_enum {
    ($Name:ident { $( $Variant:ident = $name:literal ),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
        pub enum $Name {
            $( #[serde(rename = $name)] $Variant, )+
        }
        impl SettingType for $Name {
            const KIND: Kind = Kind::Enum(&[$( $name ),+]);
        }
    };
}

setting_enum!(Language { It = "it", En = "en" });
setting_enum!(ThemePref { System = "system", Light = "light", Dark = "dark" });

/// One setting as the window and the validation see it. `label` is
/// English and is the i18n key (`t()`), like every string of the UI.
#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    pub key: &'static str,
    pub category: &'static str,
    pub kind: Kind,
    pub label: &'static str,
}

macro_rules! settings {
    ($(
        $cat:ident : $Cat:ident {
            $( $field:ident : $ty:ty = $default:expr, $label:literal; )*
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

        /// All the settings, declared: what the window and `validate` read.
        pub static DEFS: &[SettingDef] = &[
            $( $(
                SettingDef {
                    key: concat!(stringify!($cat), ".", stringify!($field)),
                    category: stringify!($cat),
                    kind: <$ty as SettingType>::KIND,
                    label: $label,
                },
            )* )*
        ];
    };
}

settings! {
    appearance: Appearance {
        language: Language = Language::It, "Language";
        theme: ThemePref = ThemePref::System, "Theme";
    }
    updates: Updates {
        skipped_version: String = String::new(), "Skipped version";
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
            .map(|v| format!("{v:?}"))
            .collect::<Vec<_>>()
            .join(" | "),
        Kind::Str => "string".into(),
    };
    let ts_kind = |kind: Kind| match kind {
        Kind::Enum(values) => format!(
            "{{ kind: \"enum\", values: [{}] }}",
            values
                .iter()
                .map(|v| format!("{v:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Kind::Str => "{ kind: \"str\" }".into(),
    };
    let mut out = String::from(
        "// Generated from app/src-tauri/src/settings.rs by `cargo xtask gen-settings`:\n\
         // do not edit. Every setting is declared once, there (§16).\n\n\
         /// The settings as `settings_get` returns them.\n\
         export type Settings = {\n",
    );
    let mut categories: Vec<&str> = Vec::new();
    for def in DEFS {
        if !categories.contains(&def.category) {
            categories.push(def.category);
        }
    }
    for cat in &categories {
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
         export type SettingKind = { kind: \"enum\"; values: readonly string[] } | { kind: \"str\" };\n\n\
         export type SettingDef = { key: SettingKey; category: string; kind: SettingKind; label: string };\n\n\
         /// Every setting, in declaration order: what the window renders.\n\
         export const SETTING_DEFS: readonly SettingDef[] = [\n",
    );
    for def in DEFS {
        out.push_str(&format!(
            "  {{ key: {:?}, category: {:?}, kind: {}, label: {:?} }},\n",
            def.key,
            def.category,
            ts_kind(def.kind),
            def.label
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
            if !DEFS.iter().any(|d| d.category == c) {
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
            Some(s) if values.contains(&s) => Ok(()),
            _ => Err(format!(
                "{} must be one of {}",
                def.label,
                values.join(", ")
            )),
        },
        Kind::Str => v
            .is_string()
            .then_some(())
            .ok_or_else(|| format!("{} must be text", def.label)),
    }
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
                for v in values {
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

    /// Every label of `DEFS` has its Italian string: the window shows them
    /// through `t()`, which falls back to English in silence.
    #[test]
    fn labels_are_translated() {
        let i18n =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/i18n.ts"))
                .unwrap();
        let mut texts: Vec<&str> = DEFS.iter().map(|d| d.label).collect();
        texts.sort_unstable();
        texts.dedup();
        let missing: Vec<&str> = texts
            .into_iter()
            .filter(|s| !i18n.contains(&format!("  \"{}\": \"", s.replace('"', "\\\""))))
            .collect();
        assert!(missing.is_empty(), "missing in i18n.ts IT: {missing:?}");
    }
}
