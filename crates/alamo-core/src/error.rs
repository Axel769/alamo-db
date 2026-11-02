//! Error model — §6.
//!
//! An error is not a string, it is a **structured record**, complete from the
//! MVP onwards: it costs little now and a great deal later.
//!
//! Many fields below are initially unused by the UI. They stay anyway: they
//! are what enables "click the constraint to open the table" without redoing
//! the model.

use std::fmt;

/// Errors are not the only thing there is.
///
/// PG sends `NOTICE`/`WARNING` **asynchronously** during execution: `RAISE
/// NOTICE` in PL/pgSQL is *the* way debugging is done. Without it, the tool is
/// unusable for anyone writing stored procedures.
///
/// MySQL instead accumulates warnings and requires an explicit `SHOW WARNINGS`
/// when `warning_count > 0` — to be run automatically, with an option to turn
/// it off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Fatal,
    Panic,
    Warning,
    Notice,
    Info,
    Log,
    Debug,
}

impl Severity {
    pub fn is_error(&self) -> bool {
        matches!(self, Severity::Error | Severity::Fatal | Severity::Panic)
    }
}

/// The code as the dialect understands it.
///
/// MySQL needs **both errno and SQLSTATE**: MySQL users think in numbers (1062,
/// 1452…), and a tool that only shows `23000` forces them to look up the
/// translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCode {
    None,
    SqlState(String),
    MySql {
        errno: u16,
        sqlstate: Option<String>,
    },
}

/// The UI reacts differently per category (§6.2): confusing them produces
/// useless reconnections or, worse, a cancellation shown as an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// Offer reconnection.
    Transport,
    /// Reopen the credentials dialog.
    Authentication,
    /// Highlight in the editor, do **not** reconnect.
    Sql,
    /// PG `57014`, MySQL errno `1317`. Must be shown as "cancelled by the
    /// user", **never** as a red error.
    Cancelled,
    Timeout,
    /// PG `25P02`: aborted transaction, everything that follows fails.
    /// Requires the banner with the "Rollback" button of §6.3 — it is what
    /// distinguishes a serious tool from an amateur one.
    TransactionAborted,
    Internal,
}

/// Position of the error in the SQL text — §6.1.
///
/// PG gives it **in characters, 1-based, relative to the statement it
/// received**. The editor buffer is split into statements, so the conversion
/// to the full buffer needs the statement start offset **both in bytes and in
/// characters**.
///
/// ⚠️ With non-ASCII text (comments in Italian, strings with accents)
/// confusing bytes and characters makes the highlight slide: it is *the*
/// classic bug of this feature. That is why the two fields are separate and
/// named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorPosition {
    /// 1-based, in **characters**, relative to the statement.
    pub char_offset_in_statement: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatementOrigin {
    /// Start of the statement in the full buffer, in bytes.
    pub byte_offset: u32,
    /// Start of the statement in the full buffer, in characters.
    pub char_offset: u32,
    /// Index of the statement in the multi-statement batch (§6.4).
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DbError {
    pub severity: Severity,
    pub category: ErrorCategory,
    pub code: ErrorCode,
    /// PG has three distinct fields and the hints are often decisive: they
    /// must be shown hierarchically, not concatenated on one line.
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    /// Populated by PG on constraint violations.
    pub schema: Option<String>,
    pub table: Option<String>,
    pub column: Option<String>,
    pub constraint: Option<String>,
    pub datatype: Option<String>,
    /// PL/pgSQL stack trace.
    pub context: Option<String>,
    /// MySQL **does not give the position**: it stays `None` and the UI
    /// degrades by highlighting the whole statement (§6.1).
    pub position: Option<ErrorPosition>,
    /// Error that happened inside a function or a prepared statement.
    pub internal_position: Option<ErrorPosition>,
    pub internal_query: Option<String>,
    pub statement: Option<StatementOrigin>,
}

impl DbError {
    pub fn internal(message: impl Into<String>) -> Self {
        DbError {
            severity: Severity::Error,
            category: ErrorCategory::Internal,
            code: ErrorCode::None,
            message: message.into(),
            detail: None,
            hint: None,
            schema: None,
            table: None,
            column: None,
            constraint: None,
            datatype: None,
            context: None,
            position: None,
            internal_position: None,
            internal_query: None,
            statement: None,
        }
    }

    pub fn with_category(mut self, category: ErrorCategory) -> Self {
        self.category = category;
        self
    }

    /// §6.2: a cancellation is not a red error.
    pub fn is_cancellation(&self) -> bool {
        self.category == ErrorCategory::Cancelled
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(d) = &self.detail {
            write!(f, " — {d}")?;
        }
        Ok(())
    }
}

impl std::error::Error for DbError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_not_an_error_to_show_in_red() {
        let e = DbError::internal("cancelled").with_category(ErrorCategory::Cancelled);
        assert!(e.is_cancellation());
    }
}
