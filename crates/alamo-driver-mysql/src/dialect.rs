//! `Dialect` for MySQL/MariaDB — §2.1, §11.

use alamo_core::column::ColumnKind;
use alamo_core::dialect::{Capabilities, Dialect};
use alamo_core::error::{DbError, ErrorCategory, ErrorCode};
use alamo_core::meta::{NativeType, SemanticClass};

use crate::typecode as tc;

/// Protocol charset number that marks binary types: it tells `BLOB`/`BINARY`
/// (bytes) from `TEXT`/`VARCHAR` (text) — the type code alone is not
/// enough, `TEXT` travels as `BLOB` with a text charset.
pub const BINARY_CHARSET: u16 = 63;

/// `UNSIGNED_FLAG` of the protocol.
pub const UNSIGNED_FLAG: u16 = 32;

#[derive(Debug, Clone, Copy)]
pub struct MyDialect {
    /// ⚠️ A property of the **connection**, not of the dialect: `KILL QUERY`
    /// requires privileges that may be missing on managed services (§7.1).
    pub can_cancel: bool,
    /// True if the server is MariaDB: it changes introspection (JSON as a
    /// `JSON_VALID()` constraint, §15.1 divergence 2), not the protocol.
    pub mariadb: bool,
}

impl Default for MyDialect {
    fn default() -> Self {
        MyDialect {
            can_cancel: true,
            mariadb: false,
        }
    }
}

fn my(native: &NativeType) -> Option<(u8, u16, u16)> {
    match native {
        NativeType::MySql {
            code,
            flags,
            charset,
        } => Some((*code, *flags, *charset)),
        _ => None,
    }
}

fn is_unsigned(flags: u16) -> bool {
    flags & UNSIGNED_FLAG != 0
}

impl Dialect for MyDialect {
    fn name(&self) -> &'static str {
        if self.mariadb {
            "MariaDB"
        } else {
            "MySQL"
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // "Schema" and "database" are the same thing in MySQL: the tree
            // shows databases as schemas (§10).
            supports_schemas: true,
            supports_explain_analyze: true,
            supports_listen_notify: false,
            // Default `NO_BACKSLASH_ESCAPES` off: `\'` does not close the string.
            backslash_escapes: true,
            supports_default_values: false,
            insert_returning: false,
            read_only_txn: false,
            supports_copy: false,
            editable_without_pk: false,
            can_cancel: self.can_cancel,
            // §11: `USE db` works within the same session.
            can_change_database_in_session: true,
            // §9.7.1: DDL = implicit commit, cannot be rolled back. The
            // Rollback button does NOT save you from a DROP TABLE — the UI
            // must not pretend.
            transactional_ddl: false,
            // §6.1: no structured position, only "at line N" in the message,
            // best-effort.
            reports_error_position: false,
            // §11: warnings are requested with `SHOW WARNINGS`, never async.
            async_notices: false,
            // `MAX_EXECUTION_TIME` exists but only for SELECT (§2.1).
            server_side_timeout: true,
        }
    }

    fn quote_identifier(&self, ident: &str) -> String {
        format!("`{}`", ident.replace('`', "``"))
    }

    /// Escapes quote and backslash: correct in the default (`\` is an
    /// escape); with `NO_BACKSLASH_ESCAPES` on, the doubled backslash would
    /// become literal — a rare case, accepted until we read the SQL_MODE.
    fn quote_string_literal(&self, s: &str) -> String {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
    }

    fn column_kind(&self, native: &NativeType) -> ColumnKind {
        let Some((code, flags, charset)) = my(native) else {
            return ColumnKind::Utf8;
        };
        match code {
            // TINY/SHORT/INT24 fit in i32 even unsigned; unsigned LONG does not.
            tc::TINY | tc::SHORT | tc::INT24 | tc::YEAR => ColumnKind::I32,
            tc::LONG => {
                if is_unsigned(flags) {
                    ColumnKind::I64
                } else {
                    ColumnKind::I32
                }
            }
            tc::LONGLONG => {
                if is_unsigned(flags) {
                    ColumnKind::U64
                } else {
                    ColumnKind::I64
                }
            }
            tc::FLOAT => ColumnKind::F32,
            tc::DOUBLE => ColumnKind::F64,
            tc::DATE | tc::NEWDATE => ColumnKind::Date32,
            // ⚠️ TIME in MySQL is a signed duration (-838h..838h), not a
            // time of day: it does not fit in `Time64Micros` without lying.
            // It stays text, temporal semantic class.
            tc::TIME => ColumnKind::Utf8,
            // DATETIME and TIMESTAMP arrive as civil components without an
            // offset: tz=false for both, the difference stays in the
            // type_display.
            tc::DATETIME | tc::TIMESTAMP => ColumnKind::TimestampMicros { tz: false },
            // BIT(1..64) is a number for the user (DBeaver shows 0/1): on
            // the wire it is a big-endian byte string, `push_value` decodes
            // it (`bit_of`), not the text parser.
            tc::BIT => ColumnKind::U64,
            tc::GEOMETRY => ColumnKind::Binary,
            tc::TINY_BLOB
            | tc::MEDIUM_BLOB
            | tc::LONG_BLOB
            | tc::BLOB
            | tc::VARCHAR
            | tc::VAR_STRING
            | tc::STRING => {
                if charset == BINARY_CHARSET {
                    ColumnKind::Binary
                } else {
                    ColumnKind::Utf8
                }
            }
            // DECIMAL/NEWDECIMAL lossless text (§4.5), JSON text, ENUM/SET
            // text, and the whole fallback (§4.4).
            _ => ColumnKind::Utf8,
        }
    }

    fn semantic_class(&self, native: &NativeType) -> SemanticClass {
        let Some((code, _, charset)) = my(native) else {
            return SemanticClass::Text;
        };
        match code {
            tc::TINY
            | tc::SHORT
            | tc::INT24
            | tc::LONG
            | tc::LONGLONG
            | tc::FLOAT
            | tc::DOUBLE
            | tc::DECIMAL
            | tc::NEWDECIMAL
            | tc::BIT => SemanticClass::Numeric,
            tc::DATE | tc::NEWDATE | tc::TIME | tc::DATETIME | tc::TIMESTAMP | tc::YEAR => {
                SemanticClass::Temporal
            }
            // ⚠️ In MariaDB `JSON` is TEXT on the wire (§15.1 divergence 2):
            // this branch only fires on MySQL. The MariaDB recovery must be
            // done in introspection, from the `JSON_VALID()` constraint.
            tc::JSON | tc::GEOMETRY => SemanticClass::Structured,
            tc::TINY_BLOB | tc::MEDIUM_BLOB | tc::LONG_BLOB | tc::BLOB
                if charset == BINARY_CHARSET =>
            {
                SemanticClass::Structured
            }
            _ => SemanticClass::Text,
        }
    }

    fn is_numeric_text(&self, native: &NativeType) -> bool {
        matches!(my(native), Some((tc::DECIMAL | tc::NEWDECIMAL, _, _)))
    }

    fn type_display(&self, native: &NativeType) -> String {
        let Some((code, flags, charset)) = my(native) else {
            return "unknown".into();
        };
        let base = match code {
            tc::DECIMAL | tc::NEWDECIMAL => "decimal",
            tc::TINY => "tinyint",
            tc::SHORT => "smallint",
            tc::LONG => "int",
            tc::FLOAT => "float",
            tc::DOUBLE => "double",
            tc::NULL => "null",
            tc::TIMESTAMP => "timestamp",
            tc::LONGLONG => "bigint",
            tc::INT24 => "mediumint",
            tc::DATE | tc::NEWDATE => "date",
            tc::TIME => "time",
            tc::DATETIME => "datetime",
            tc::YEAR => "year",
            tc::VARCHAR | tc::VAR_STRING => {
                if charset == BINARY_CHARSET {
                    "varbinary"
                } else {
                    "varchar"
                }
            }
            tc::BIT => "bit",
            tc::JSON => "json",
            tc::ENUM => "enum",
            tc::SET => "set",
            tc::TINY_BLOB => {
                if charset == BINARY_CHARSET {
                    "tinyblob"
                } else {
                    "tinytext"
                }
            }
            tc::MEDIUM_BLOB => {
                if charset == BINARY_CHARSET {
                    "mediumblob"
                } else {
                    "mediumtext"
                }
            }
            tc::LONG_BLOB => {
                if charset == BINARY_CHARSET {
                    "longblob"
                } else {
                    "longtext"
                }
            }
            tc::BLOB => {
                if charset == BINARY_CHARSET {
                    "blob"
                } else {
                    "text"
                }
            }
            tc::STRING => {
                if charset == BINARY_CHARSET {
                    "binary"
                } else {
                    "char"
                }
            }
            tc::GEOMETRY => "geometry",
            _ => "unknown",
        };
        if is_unsigned(flags)
            && matches!(
                code,
                tc::TINY
                    | tc::SHORT
                    | tc::INT24
                    | tc::LONG
                    | tc::LONGLONG
                    | tc::DECIMAL
                    | tc::NEWDECIMAL
            )
        {
            format!("{base} unsigned")
        } else {
            base.to_owned()
        }
    }

    fn classify_error(&self, error: &DbError) -> ErrorCategory {
        match &error.code {
            ErrorCode::MySql { errno, sqlstate } => {
                crate::error::classify(*errno, sqlstate.as_deref().unwrap_or(""))
            }
            _ => error.category,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(code: u8, flags: u16, charset: u16) -> NativeType {
        NativeType::MySql {
            code,
            flags,
            charset,
        }
    }

    #[test]
    fn unsigned_widens_kind() {
        let d = MyDialect::default();
        assert_eq!(d.column_kind(&n(tc::LONG, 0, 0)), ColumnKind::I32);
        assert_eq!(
            d.column_kind(&n(tc::LONG, UNSIGNED_FLAG, 0)),
            ColumnKind::I64
        );
        assert_eq!(d.column_kind(&n(tc::LONGLONG, 0, 0)), ColumnKind::I64);
        assert_eq!(
            d.column_kind(&n(tc::LONGLONG, UNSIGNED_FLAG, 0)),
            ColumnKind::U64
        );
    }

    #[test]
    fn bit_is_an_unsigned_number() {
        let d = MyDialect::default();
        let b = n(tc::BIT, 0, BINARY_CHARSET);
        assert_eq!(d.column_kind(&b), ColumnKind::U64);
        assert_eq!(d.semantic_class(&b), SemanticClass::Numeric);
        assert_eq!(d.type_display(&b), "bit");
    }

    #[test]
    fn text_vs_blob_by_charset() {
        let d = MyDialect::default();
        assert_eq!(d.column_kind(&n(tc::BLOB, 0, 224)), ColumnKind::Utf8);
        assert_eq!(
            d.column_kind(&n(tc::BLOB, 0, BINARY_CHARSET)),
            ColumnKind::Binary
        );
        assert_eq!(d.type_display(&n(tc::BLOB, 0, 224)), "text");
        assert_eq!(d.type_display(&n(tc::BLOB, 0, BINARY_CHARSET)), "blob");
    }

    #[test]
    fn mysql_time_is_a_duration_and_stays_text() {
        let d = MyDialect::default();
        assert_eq!(
            d.column_kind(&n(tc::TIME, 0, BINARY_CHARSET)),
            ColumnKind::Utf8
        );
        assert_eq!(
            d.semantic_class(&n(tc::TIME, 0, BINARY_CHARSET)),
            SemanticClass::Temporal
        );
    }

    #[test]
    fn decimal_is_numeric_text() {
        let d = MyDialect::default();
        let nd = n(tc::NEWDECIMAL, 0, BINARY_CHARSET);
        assert_eq!(d.column_kind(&nd), ColumnKind::Utf8);
        assert!(d.is_numeric_text(&nd));
        assert_eq!(d.semantic_class(&nd), SemanticClass::Numeric);
    }
}
