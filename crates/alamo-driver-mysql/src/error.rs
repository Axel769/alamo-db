//! MySQL error mapping -> `DbError` (§6).
//!
//! Divergences from PG (§11): the server sends **errno + SQLSTATE** (the
//! former is more specific: 1062 is "duplicate key", SQLSTATE 23000 covers
//! half a chapter of constraints); there is no structured error position —
//! only, sometimes, "at line N" in the message (best-effort, §6.1: the UI
//! degrades by highlighting the whole statement).

use alamo_core::error::{DbError, ErrorCategory, ErrorCode};

#[derive(Debug)]
pub struct MyError(pub mysql_async::Error);

impl From<mysql_async::Error> for MyError {
    fn from(e: mysql_async::Error) -> Self {
        MyError(e)
    }
}

impl std::fmt::Display for MyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for MyError {}

impl MyError {
    pub fn into_db_error(self) -> DbError {
        match self.0 {
            mysql_async::Error::Server(e) => {
                let mut db = DbError::internal(e.message.clone());
                db.code = ErrorCode::MySql {
                    errno: e.code,
                    sqlstate: Some(e.state.clone()),
                };
                db.category = classify(e.code, &e.state);
                db.hint = hint(e.code, &e.message).map(str::to_owned);
                db
            }
            mysql_async::Error::Io(e) => {
                DbError::internal(e.to_string()).with_category(ErrorCategory::Transport)
            }
            // Our own errors (e.g. `TlsOptions::check`): the message without
            // the "Other error:" prefix of mysql_async.
            mysql_async::Error::Other(e) => DbError::internal(e.to_string()),
            e => DbError::internal(e.to_string()),
        }
    }
}

/// Row → type, with an error instead of a panic. The typed reads of
/// mysql_async (`query::<T>`, `exec::<T>`, `query_first::<T>`) panic if
/// the row does not have the expected shape, and with panic=abort the app
/// quits (alpha.3 crash: connection out of phase, `SELECT DATABASE()` with
/// the columns of another query). We read a `Row` and convert it here. The
/// message does not report the row values: they could be user data.
pub(crate) fn row_as<T: mysql_async::prelude::FromRow>(
    row: mysql_async::Row,
) -> Result<T, MyError> {
    let columns = row.len();
    T::from_row_opt(row).map_err(|_| {
        MyError(mysql_async::Error::Other(
            format!(
                "unexpected reply from the server: a row of {columns} columns \
                 does not have the expected shape ({})",
                std::any::type_name::<T>()
            )
            .into(),
        ))
    })
}

/// `row_as` on every row.
pub(crate) fn rows_as<T: mysql_async::prelude::FromRow>(
    rows: Vec<mysql_async::Row>,
) -> Result<Vec<T>, MyError> {
    rows.into_iter().map(row_as).collect()
}

/// `row_as` on the first row, if any.
pub(crate) fn first_as<T: mysql_async::prelude::FromRow>(
    row: Option<mysql_async::Row>,
) -> Result<Option<T>, MyError> {
    row.map(row_as).transpose()
}

/// Classification for the UI reaction (§6.2). The errno is the primary
/// axis; the SQLSTATE fills in where the errno is not enough.
pub(crate) fn classify(errno: u16, state: &str) -> ErrorCategory {
    match errno {
        // ER_QUERY_INTERRUPTED: it is the `KILL QUERY` of §7.1 — a
        // cancellation, not a red error (the equivalent of PG's `57014`).
        1317 => ErrorCategory::Cancelled,
        // ER_QUERY_TIMEOUT / MAX_EXECUTION_TIME exceeded.
        1907 | 3024 => ErrorCategory::Timeout,
        // Credentials and privileges: reopen the dialog (§6.2).
        1044 | 1045 | 1142 | 1143 | 1227 | 3118 => ErrorCategory::Authentication,
        // Server full / connection lost / KILL of the connection.
        1040 | 1042 | 1043 | 1053 | 1152 | 1184 | 2002 | 2003 | 2006 | 2013 => {
            ErrorCategory::Transport
        }
        _ => match state.get(..2) {
            Some("08") => ErrorCategory::Transport,
            Some("28") => ErrorCategory::Authentication,
            _ => ErrorCategory::Sql,
        },
    }
}

/// MySQL does not send hints like PG does: for the errnos that alone do not
/// explain what to do, we provide the advice ourselves. In English, like the
/// other strings of the app: the UI passes it through `t()` for Italian.
fn hint(errno: u16, message: &str) -> Option<&'static str> {
    match errno {
        // Connection limit of the user, not of the server: 1203 if it is the
        // global `@@max_user_connections`, 1226 if it is the account's one
        // (`WITH MAX_USER_CONNECTIONS`) — 1226 also covers the other hourly
        // limits, which are irrelevant here. The session adapts to the limit
        // (shared lanes, zombies closed on reopen): if the error still
        // arrives, the seats are held by other programs or by connections
        // left hanging on the server, and the advice says so.
        _ if is_user_connection_limit(errno, message) => Some(USER_CONNECTION_LIMIT),
        _ => None,
    }
}

/// The user has run out of connections (see `hint`): the session uses this
/// to fall back on an already open lane instead of failing.
pub fn is_user_connection_limit(errno: u16, message: &str) -> bool {
    errno == 1203 || (errno == 1226 && message.contains("max_user_connections"))
}

const USER_CONNECTION_LIMIT: &str = "This database user has no free connections (max_user_connections). Other programs with the same user (e.g. HeidiSQL, DBeaver) count too, and so do connections left open after the PC slept: close them, or ask the database administrator to raise the limit.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_query_is_cancellation_not_red_error() {
        assert_eq!(classify(1317, "70100"), ErrorCategory::Cancelled);
    }

    #[test]
    fn user_connection_limit() {
        let account =
            "User 'u' has exceeded the 'max_user_connections' resource (current value: 1)";
        assert_eq!(
            hint(
                1203,
                "User u already has more than 'max_user_connections' active connections"
            ),
            Some(USER_CONNECTION_LIMIT)
        );
        assert_eq!(hint(1226, account), Some(USER_CONNECTION_LIMIT));
        // The same 1226 for an hourly limit: no wrong advice.
        assert_eq!(
            hint(
                1226,
                "User 'u' has exceeded the 'max_questions' resource (current value: 10)"
            ),
            None
        );
        assert_eq!(hint(1064, account), None);
    }

    #[test]
    fn sqlstate_fallback_when_errno_unknown() {
        assert_eq!(classify(9999, "08S01"), ErrorCategory::Transport);
        assert_eq!(classify(1064, "42000"), ErrorCategory::Sql);
    }
}
