//! `Dialect` for PostgreSQL — §2.1, §11.

use alamo_core::column::ColumnKind;
use alamo_core::dialect::{Capabilities, Dialect};
use alamo_core::error::{DbError, ErrorCategory, ErrorCode};
use alamo_core::meta::{NativeType, SemanticClass};

use crate::oid;

#[derive(Debug, Clone, Copy)]
pub struct PgDialect {
    /// ⚠️ Not a property of PostgreSQL but of the **connection**: behind a
    /// pooler (PgBouncer) the `CancelRequest` may not reach the right backend
    /// and simply not work (§7.1). It must be verified at connect.
    pub can_cancel: bool,
}

impl Default for PgDialect {
    fn default() -> Self {
        PgDialect { can_cancel: true }
    }
}

fn pg_oid(native: &NativeType) -> Option<u32> {
    match native {
        NativeType::Postgres { oid, .. } => Some(*oid),
        _ => None,
    }
}

impl Dialect for PgDialect {
    fn name(&self) -> &'static str {
        "PostgreSQL"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            supports_schemas: true,
            supports_explain_analyze: true,
            supports_listen_notify: true,
            backslash_escapes: false,
            supports_default_values: true,
            insert_returning: true,
            read_only_txn: true,
            supports_copy: true,
            editable_without_pk: false,
            can_cancel: self.can_cancel,
            // §11: in PG changing database **cannot** be done within the same
            // session. A new connection is needed, and the session state is
            // lost: the UI must warn, not silently make an open transaction
            // disappear.
            can_change_database_in_session: false,
            // §9.7.1: in PG DDL is transactional and rollback really works,
            // with the exceptions that cannot run inside a transaction
            // (`CREATE INDEX CONCURRENTLY`, `VACUUM`, `CREATE/DROP DATABASE`,
            // `REINDEX CONCURRENTLY`, `ALTER SYSTEM`), which must be handled
            // separately.
            transactional_ddl: true,
            reports_error_position: true,
            async_notices: true,
            server_side_timeout: true,
        }
    }

    fn quote_identifier(&self, ident: &str) -> String {
        let mut s = String::with_capacity(ident.len() + 2);
        s.push('"');
        for c in ident.chars() {
            if c == '"' {
                s.push('"');
            }
            s.push(c);
        }
        s.push('"');
        s
    }

    /// `'…'` with the quote doubled; with a backslash in the value, `E'…'`
    /// with the backslash doubled. The plain literal depends on
    /// `standard_conforming_strings`: if the server (or the session) has it
    /// `off`, `\'` closes the string and the rest of the value becomes SQL
    /// (`\' OR true --` in an editing key). The `E'…'` always treats the
    /// backslash as an escape, regardless of the server.
    fn quote_string_literal(&self, value: &str) -> String {
        let escape = value.contains('\\');
        let mut s = String::with_capacity(value.len() + 3);
        if escape {
            s.push('E');
        }
        s.push('\'');
        for c in value.chars() {
            if c == '\'' || (escape && c == '\\') {
                s.push(c);
            }
            s.push(c);
        }
        s.push('\'');
        s
    }

    fn column_kind(&self, native: &NativeType) -> ColumnKind {
        match pg_oid(native) {
            Some(oid::BOOL) => ColumnKind::Bool,
            Some(oid::INT2) => ColumnKind::I16,
            Some(oid::INT4) => ColumnKind::I32,
            Some(oid::OID) => ColumnKind::I64,
            Some(oid::INT8) => ColumnKind::I64,
            Some(oid::FLOAT4) => ColumnKind::F32,
            Some(oid::FLOAT8) => ColumnKind::F64,
            Some(oid::BYTEA) => ColumnKind::Binary,
            Some(oid::DATE) => ColumnKind::Date32,
            Some(oid::TIME) => ColumnKind::Time64Micros,
            Some(oid::TIMESTAMP) => ColumnKind::TimestampMicros { tz: false },
            Some(oid::TIMESTAMPTZ) => ColumnKind::TimestampMicros { tz: true },
            // Everything else — numeric, json/jsonb, uuid, interval, range,
            // array, geometries, domains, bit/varbit, custom types — goes to
            // `Utf8` with the text exactly as the server sends it (§4.4).
            _ => ColumnKind::Utf8,
        }
    }

    fn semantic_class(&self, native: &NativeType) -> SemanticClass {
        match pg_oid(native) {
            Some(oid::BOOL) => SemanticClass::Boolean,
            Some(
                oid::INT2
                | oid::INT4
                | oid::INT8
                | oid::OID
                | oid::FLOAT4
                | oid::FLOAT8
                | oid::NUMERIC
                | oid::MONEY,
            ) => SemanticClass::Numeric,
            Some(
                oid::DATE
                | oid::TIME
                | oid::TIMETZ
                | oid::TIMESTAMP
                | oid::TIMESTAMPTZ
                | oid::INTERVAL,
            ) => SemanticClass::Temporal,
            Some(oid::JSON | oid::JSONB | oid::XML | oid::BYTEA | oid::BIT | oid::VARBIT) => {
                SemanticClass::Structured
            }
            Some(o) if oid::is_array(o) => SemanticClass::Structured,
            _ => SemanticClass::Text,
        }
    }

    fn is_numeric_text(&self, native: &NativeType) -> bool {
        // §4.5: it stays `Utf8` permanently, but the grid right-aligns it and
        // sorts it numerically.
        matches!(pg_oid(native), Some(oid::NUMERIC) | Some(oid::MONEY))
    }

    fn type_display(&self, native: &NativeType) -> String {
        let Some(o) = pg_oid(native) else {
            return "?".to_owned();
        };
        let name = match o {
            oid::BOOL => "bool",
            oid::BYTEA => "bytea",
            oid::CHAR => "\"char\"",
            oid::NAME => "name",
            oid::INT8 => "int8",
            oid::INT2 => "int2",
            oid::INT4 => "int4",
            oid::TEXT => "text",
            oid::OID => "oid",
            oid::JSON => "json",
            oid::XML => "xml",
            oid::FLOAT4 => "float4",
            oid::FLOAT8 => "float8",
            oid::MONEY => "money",
            oid::INET => "inet",
            oid::CIDR => "cidr",
            oid::MACADDR => "macaddr",
            oid::MACADDR8 => "macaddr8",
            oid::BPCHAR => "bpchar",
            oid::VARCHAR => "varchar",
            oid::DATE => "date",
            oid::TIME => "time",
            oid::TIMESTAMP => "timestamp",
            oid::TIMESTAMPTZ => "timestamptz",
            oid::INTERVAL => "interval",
            oid::TIMETZ => "timetz",
            oid::BIT => "bit",
            oid::VARBIT => "varbit",
            oid::NUMERIC => "numeric",
            oid::UUID => "uuid",
            oid::JSONB => "jsonb",
            oid::TEXT_ARRAY => "text[]",
            oid::INT4_ARRAY => "int4[]",
            oid::INT8_ARRAY => "int8[]",
            oid::VARCHAR_ARRAY => "varchar[]",
            oid::NUMERIC_ARRAY => "numeric[]",
            oid::UUID_ARRAY => "uuid[]",
            oid::JSONB_ARRAY => "jsonb[]",
            oid::INET_ARRAY => "inet[]",
            oid::CIDR_ARRAY => "cidr[]",
            oid::MACADDR_ARRAY => "macaddr[]",
            _ => return format!("oid:{o}"),
        };
        name.to_owned()
    }

    fn classify_error(&self, error: &DbError) -> ErrorCategory {
        let ErrorCode::SqlState(state) = &error.code else {
            return ErrorCategory::Internal;
        };
        classify_sqlstate(state)
    }
}

/// §6.2. The line that matters is `57014`: a query cancelled by the user
/// **is not a red error**, and showing it as one makes the tool look broken
/// exactly when it worked.
pub fn classify_sqlstate(state: &str) -> ErrorCategory {
    match state {
        "57014" => ErrorCategory::Cancelled,
        // "current transaction is aborted, commands ignored until end of
        // transaction block" — requires the banner with Rollback of §6.3.
        "25P02" => ErrorCategory::TransactionAborted,
        "57P01" | "57P02" | "57P03" => ErrorCategory::Transport,
        _ => {
            let class = state.get(..2).unwrap_or("");
            match class {
                "08" => ErrorCategory::Transport,
                "28" => ErrorCategory::Authentication,
                "53" | "58" | "XX" => ErrorCategory::Internal,
                _ => ErrorCategory::Sql,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_and_aborted_transaction_are_their_own_categories() {
        assert_eq!(classify_sqlstate("57014"), ErrorCategory::Cancelled);
        assert_eq!(
            classify_sqlstate("25P02"),
            ErrorCategory::TransactionAborted
        );
        assert_eq!(classify_sqlstate("23505"), ErrorCategory::Sql);
        assert_eq!(classify_sqlstate("28P01"), ErrorCategory::Authentication);
        assert_eq!(classify_sqlstate("08006"), ErrorCategory::Transport);
    }

    #[test]
    fn literals_independent_of_standard_conforming_strings() {
        let d = PgDialect::default();
        assert_eq!(d.quote_string_literal("it's"), "'it''s'");
        assert_eq!(
            d.quote_string_literal(r"\' OR true --"),
            r"E'\\'' OR true --'"
        );
        assert_eq!(d.quote_string_literal(r"C:\dir"), r"E'C:\\dir'");
    }

    #[test]
    fn identifier_quoting_escapes_the_delimiter() {
        let d = PgDialect::default();
        assert_eq!(d.quote_identifier("tab"), "\"tab\"");
        assert_eq!(d.quote_identifier("we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn numeric_stays_text_but_sorts_as_a_number() {
        let d = PgDialect::default();
        let n = NativeType::Postgres {
            oid: oid::NUMERIC,
            type_modifier: -1,
        };
        assert_eq!(d.column_kind(&n), ColumnKind::Utf8);
        assert!(d.is_numeric_text(&n));
        assert_eq!(d.semantic_class(&n), SemanticClass::Numeric);
    }
}
