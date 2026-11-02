//! `Dialect` trait and `Capabilities` — §2.1.
//!
//! **Features specific to a single dialect do NOT go into the trait** with
//! empty methods in the other implementation. They are exposed through
//! `Capabilities`, which the UI queries.
//!
//! Stated goal: **zero `if dialect == Postgres` scattered across the frontend.**

use crate::column::ColumnKind;
use crate::error::{DbError, ErrorCategory};
use crate::meta::{NativeType, SemanticClass};

/// What the dialect — and the concrete connection — can do.
///
/// Some entries are not properties of the dialect but of the **connection**:
/// `can_cancel` depends on privileges (`PROCESS` on MySQL) and on whether a
/// pooler sits in between (§7.1). They must be verified at connect time, not
/// deduced from the product name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub supports_schemas: bool,
    pub supports_explain_analyze: bool,
    pub supports_listen_notify: bool,
    pub supports_copy: bool,
    pub editable_without_pk: bool,
    /// ⚠️ Must be **verified at connect time**, not assumed: behind PgBouncer
    /// the `CancelRequest` may not reach the right backend, and on managed
    /// MySQL services `KILL QUERY` may fail silently (§7.1).
    /// Better a disabled Cancel button with a tooltip than one that pretends.
    pub can_cancel: bool,
    /// In PG it is **impossible**: changing database means opening a new
    /// connection and **losing the session state** (§11).
    pub can_change_database_in_session: bool,
    /// In MySQL it is **false**: `CREATE`/`ALTER`/`DROP`/`TRUNCATE` cause an
    /// implicit commit that cannot be rolled back (§9.7.1). On MySQL prod the
    /// Rollback button **does not save** you from a `DROP TABLE`: if the user
    /// believes otherwise, the tool gave them a false sense of security —
    /// worse than no security at all.
    pub transactional_ddl: bool,
    /// PG gives the error position, MySQL does not (§6.1).
    pub reports_error_position: bool,
    /// PG sends notices asynchronously; MySQL requires `SHOW WARNINGS`.
    pub async_notices: bool,
    /// Server-side timeout available (`statement_timeout` in PG,
    /// `MAX_EXECUTION_TIME` in MySQL — the latter only on SELECT).
    /// ⚠️ It changes the semantics of the user's queries: optional per
    /// profile, never a hidden default (§7).
    pub server_side_timeout: bool,
    /// The backslash escapes inside ordinary strings (`'a\''` stays open).
    /// True on MySQL/MariaDB (default `NO_BACKSLASH_ESCAPES` off), false on
    /// PG (`standard_conforming_strings` on; `E'...'` strings are handled
    /// separately). Drives the splitter and the classifier: getting this wrong
    /// means splitting a statement inside a literal.
    pub backslash_escapes: bool,
    /// `INSERT INTO t DEFAULT VALUES` (row entirely at defaults) exists in PG;
    /// MySQL/MariaDB want `INSERT INTO t () VALUES ()`.
    pub supports_default_values: bool,
    /// `INSERT … RETURNING *` returns the inserted row (PG). Where it is
    /// missing, the row is read back with a SELECT by key / `LAST_INSERT_ID()`.
    pub insert_returning: bool,
    /// Read-only profiles (§9.7 point 4) run every execution inside a `READ
    /// ONLY` transaction opened and closed by Alamo (PG), so the user's own
    /// `BEGIN`/`COMMIT` is refused. Off: the session-level setting is the
    /// only server net and dynamic SQL, which could disable it unseen, is
    /// refused instead (MySQL/MariaDB).
    pub read_only_txn: bool,
}

pub trait Dialect: Send + Sync {
    fn name(&self) -> &'static str;

    fn capabilities(&self) -> Capabilities;

    /// Identifier quoting, with escaping of the delimiter.
    fn quote_identifier(&self, ident: &str) -> String;

    fn quote_string_literal(&self, s: &str) -> String;

    /// Native type -> columnar variant mapping (§4.4).
    fn column_kind(&self, native: &NativeType) -> ColumnKind;

    /// Native type -> semantic class mapping for colouring (§9.6).
    ///
    /// ⚠️ In MariaDB `JSON` is not a native type: it is an alias of `LONGTEXT`
    /// with a `JSON_VALID()` constraint, and the type code from the protocol is
    /// **TEXT** (§15.1). Without intervention those columns would be coloured
    /// as text: it is the first case where a dialect difference lands on the
    /// design.
    fn semantic_class(&self, native: &NativeType) -> SemanticClass;

    /// True for types that stay text but must be right-aligned and sorted
    /// numerically (PG's `NUMERIC`, §4.5).
    fn is_numeric_text(&self, native: &NativeType) -> bool;

    /// Type name shown to the user.
    fn type_display(&self, native: &NativeType) -> String;

    fn classify_error(&self, error: &DbError) -> ErrorCategory;
}
