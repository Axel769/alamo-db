//! Driver errors and translation to the structured record of §6.

use std::fmt;

use tokio_postgres::error::{DbError as PgDbError, ErrorPosition as PgErrorPosition};

use alamo_core::error::{DbError, ErrorCode, ErrorPosition, Severity};
use alamo_core::CoreError;

use crate::dialect::classify_sqlstate;

#[derive(Debug)]
pub enum PgError {
    /// Error reported by the server or the transport, already translated
    /// into the structured record of §6.
    Db(Box<DbError>),
    /// Our own bug in the construction of the layout.
    Core(CoreError),
    /// The server sent a binary value that does not match the documented
    /// format for that type. Distinct from the other two because it
    /// indicates a misalignment between our OID table and the server, not an
    /// SQL error nor a layout bug. Note §7.3: a single malformed cell today
    /// **degrades** in the grid and no longer passes through here; the
    /// variant remains for the malformations that prevent going on.
    Malformed { column: usize, type_name: String },
}

impl PgError {
    pub fn malformed(column: usize, type_name: &str) -> Self {
        PgError::Malformed {
            column,
            type_name: type_name.to_owned(),
        }
    }

    /// The structured record to show to the UI.
    pub fn into_db_error(self) -> DbError {
        match self {
            PgError::Db(e) => *e,
            PgError::Core(e) => e.into(),
            PgError::Malformed { column, type_name } => DbError::internal(format!(
                "malformed binary value in column {column} of type {type_name}"
            )),
        }
    }
}

impl fmt::Display for PgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PgError::Db(e) => write!(f, "{e}"),
            PgError::Core(e) => write!(f, "{e}"),
            PgError::Malformed { column, type_name } => write!(
                f,
                "malformed binary value in column {column} of type {type_name}"
            ),
        }
    }
}

impl std::error::Error for PgError {}

impl From<CoreError> for PgError {
    fn from(e: CoreError) -> Self {
        PgError::Core(e)
    }
}

impl From<tokio_postgres::Error> for PgError {
    fn from(e: tokio_postgres::Error) -> Self {
        PgError::Db(Box::new(translate(&e)))
    }
}

/// Translates the `tokio-postgres` error into the §6 record.
///
/// The fields the UI does not use today (`schema`, `table`, `column`,
/// `constraint`, `datatype`) are kept anyway: they are what will enable
/// "click on the constraint to open the table" without redoing the model.
pub fn translate(err: &tokio_postgres::Error) -> DbError {
    let Some(db) = err.as_db_error() else {
        // No error from the server: it is transport (closed socket, TLS, DNS).
        let mut out = DbError::internal(with_causes(err));
        out.category = alamo_core::ErrorCategory::Transport;
        return out;
    };
    from_pg_db_error(db)
}

/// The `Display` of `tokio_postgres::Error` prints only the category ("error
/// performing TLS handshake", "invalid configuration"): the real reason is
/// in the `source()` chain ("invalid peer certificate: UnknownIssuer",
/// "password missing"). Without it, the user sees an error they cannot fix.
fn with_causes(err: &dyn std::error::Error) -> String {
    let mut msg = err.to_string();
    let mut cause = err.source();
    while let Some(c) = cause {
        let text = c.to_string();
        // Some wrappers (io::Error) repeat the inner cause in their Display.
        if !text.is_empty() && !msg.ends_with(&text) {
            msg.push_str(": ");
            msg.push_str(&text);
        }
        cause = c.source();
    }
    msg
}

pub fn from_pg_db_error(db: &PgDbError) -> DbError {
    let state = db.code().code().to_owned();
    let (position, internal_position, internal_query) = match db.position() {
        Some(PgErrorPosition::Original(p)) => (
            Some(ErrorPosition {
                char_offset_in_statement: *p,
            }),
            None,
            None,
        ),
        Some(PgErrorPosition::Internal { position, query }) => (
            None,
            Some(ErrorPosition {
                char_offset_in_statement: *position,
            }),
            Some(query.clone()),
        ),
        None => (None, None, None),
    };

    // 57014 is query_canceled for BOTH a user cancel and a statement
    // timeout; only the message tells them apart. A timeout must not look
    // like "cancelled by the user" (§16).
    let category = if state == "57014" && db.message().contains("statement timeout") {
        alamo_core::ErrorCategory::Timeout
    } else {
        classify_sqlstate(&state)
    };
    DbError {
        severity: map_severity(db.severity()),
        category,
        code: ErrorCode::SqlState(state),
        message: db.message().to_owned(),
        detail: db.detail().map(str::to_owned),
        hint: db.hint().map(str::to_owned),
        schema: db.schema().map(str::to_owned),
        table: db.table().map(str::to_owned),
        column: db.column().map(str::to_owned),
        constraint: db.constraint().map(str::to_owned),
        datatype: db.datatype().map(str::to_owned),
        context: db.where_().map(str::to_owned),
        position,
        internal_position,
        internal_query,
        statement: None,
    }
}

fn map_severity(s: &str) -> Severity {
    match s {
        "FATAL" => Severity::Fatal,
        "PANIC" => Severity::Panic,
        "WARNING" => Severity::Warning,
        "NOTICE" => Severity::Notice,
        "INFO" => Severity::Info,
        "LOG" => Severity::Log,
        "DEBUG" => Severity::Debug,
        _ => Severity::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_error_carries_the_cause() {
        let err = "host=localhost port=notanumber"
            .parse::<tokio_postgres::Config>()
            .unwrap_err();
        let msg = translate(&err).message;
        assert!(msg.starts_with("invalid connection string: "), "{msg}");
        assert!(msg.contains("port"), "{msg}");
    }
}
