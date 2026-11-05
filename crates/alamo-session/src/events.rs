//! Events towards the UI (§8: execute → exec_id immediately, then streamed events).
//!
//! Serializable here, so the `app` crate stays pure wiring. The frontend
//! subscriber is unique and dispatches into the store; events with unknown ids
//! (tab closed in the meantime) must be ignored without breaking (§9.3).

use alamo_core::column::ColumnKind;
use alamo_core::error::{DbError, ErrorCategory, ErrorCode, Severity};
use alamo_core::meta::{ColumnMeta, ColumnSource, SemanticClass};
use alamo_core::result_set::ResultSetState;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ColumnOut {
    pub name: String,
    pub type_display: String,
    /// Columnar kind (§4.4) the driver chose for the type: the grid formats
    /// the display text of dates, numbers and binaries from it (§8.2). A
    /// column degraded to text later (§4.6) keeps it: its values are text
    /// the formatter leaves alone.
    pub kind: &'static str,
    /// Class → colour resolved **once per column**, never per cell (§9.6).
    pub semantic_class: &'static str,
    pub nullable: Option<bool>,
    /// PG `NUMERIC` → lossy text: "right-align, sort numerically"
    /// (§4.5).
    pub numeric_text: bool,
    pub undecoded: bool,
    /// Source table `db.table` (MySQL only for now; PG gives an oid still to
    /// be resolved): if all columns come from the same one, the INSERT export
    /// of a query uses it as the name instead of "result".
    pub source_table: Option<String>,
}

impl ColumnOut {
    pub fn new(m: &ColumnMeta, kind: ColumnKind) -> Self {
        ColumnOut {
            name: m.name.clone(),
            type_display: m.type_display.clone(),
            kind: match kind {
                ColumnKind::Bool => "bool",
                ColumnKind::I16 => "i16",
                ColumnKind::I32 => "i32",
                ColumnKind::I64 => "i64",
                ColumnKind::U64 => "u64",
                ColumnKind::F32 => "f32",
                ColumnKind::F64 => "f64",
                ColumnKind::Utf8 => "utf8",
                ColumnKind::Binary => "binary",
                ColumnKind::Date32 => "date",
                ColumnKind::Time64Micros => "time",
                ColumnKind::TimestampMicros { tz: false } => "timestamp",
                ColumnKind::TimestampMicros { tz: true } => "timestamptz",
            },
            semantic_class: match m.semantic_class {
                SemanticClass::Numeric => "numeric",
                SemanticClass::Temporal => "temporal",
                SemanticClass::Text => "text",
                SemanticClass::Boolean => "boolean",
                SemanticClass::Structured => "structured",
            },
            nullable: m.nullable,
            numeric_text: m.numeric_text,
            undecoded: m.undecoded_wire_format,
            source_table: match &m.source {
                Some(ColumnSource::MySql { db, org_table, .. }) if db.is_empty() => {
                    Some(org_table.clone())
                }
                Some(ColumnSource::MySql { db, org_table, .. }) => {
                    Some(format!("{db}.{org_table}"))
                }
                _ => None,
            },
        }
    }
}

/// `DbError` serialized for the UI, hierarchical (message/detail/hint kept
/// separate, §6) and with the position already accompanied by the statement
/// offsets for the conversion to a buffer position (§6.1).
#[derive(Debug, Clone, Serialize)]
pub struct ErrorOut {
    pub severity: &'static str,
    pub category: &'static str,
    pub code: Option<String>,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub context: Option<String>,
    /// 1-based, in characters, relative to the statement (§6.1).
    pub position_char: Option<u32>,
    pub internal_query: Option<String>,
    /// Offset of the statement in the whole buffer, in characters.
    pub statement_char_offset: Option<u32>,
    pub statement_index: Option<u32>,
}

impl From<&DbError> for ErrorOut {
    fn from(e: &DbError) -> Self {
        ErrorOut {
            severity: match e.severity {
                Severity::Error => "error",
                Severity::Fatal => "fatal",
                Severity::Panic => "panic",
                Severity::Warning => "warning",
                Severity::Notice => "notice",
                Severity::Info => "info",
                Severity::Log => "log",
                Severity::Debug => "debug",
            },
            category: match e.category {
                ErrorCategory::Transport => "transport",
                ErrorCategory::Authentication => "authentication",
                ErrorCategory::Sql => "sql",
                ErrorCategory::Cancelled => "cancelled",
                ErrorCategory::Timeout => "timeout",
                ErrorCategory::TransactionAborted => "transaction_aborted",
                ErrorCategory::Internal => "internal",
            },
            code: match &e.code {
                ErrorCode::None => None,
                ErrorCode::SqlState(s) => Some(s.clone()),
                ErrorCode::MySql { errno, .. } => Some(errno.to_string()),
            },
            message: e.message.clone(),
            detail: e.detail.clone(),
            hint: e.hint.clone(),
            context: e.context.clone(),
            position_char: e.position.map(|p| p.char_offset_in_statement),
            internal_query: e.internal_query.clone(),
            statement_char_offset: e.statement.as_ref().map(|s| s.char_offset),
            statement_index: e.statement.as_ref().map(|s| s.index),
        }
    }
}

pub fn state_str(s: ResultSetState) -> &'static str {
    match s {
        ResultSetState::Streaming => "streaming",
        ResultSetState::Paused => "paused",
        ResultSetState::Complete => "complete",
        ResultSetState::Truncated => "truncated",
        ResultSetState::PartialCancelled => "partial_cancelled",
    }
}

/// Outcome of a single statement (§6.4: never an aggregated outcome).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutcomeOut {
    RowSet {
        result_id: u64,
        rows: u64,
        state: &'static str,
    },
    Affected {
        rows: u64,
    },
    Void,
    Failed {
        error: ErrorOut,
    },
    /// §6.2: shown as "cancelled by the user", never as a red error.
    /// Partial data, if any, stays readable (§7.3).
    Cancelled {
        result_id: Option<u64>,
        rows: u64,
    },
    /// Statement never executed: a previous one failed or was cancelled
    /// (§6.4, default stop-at-first-error).
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecEvent {
    Started {
        exec_id: u64,
        statement_count: u32,
    },
    StatementStarted {
        exec_id: u64,
        index: u32,
        char_offset: u32,
        byte_offset: u32,
    },
    Schema {
        exec_id: u64,
        index: u32,
        result_id: u64,
        columns: Vec<ColumnOut>,
    },
    /// Progress per closed chunk: the UI updates "N+ rows" (§5.2, total
    /// unknown while the stream is open).
    Rows {
        exec_id: u64,
        result_id: u64,
        rows: u64,
        layout_bytes: u64,
    },
    /// Banner of §5: "Continue / Stop". The user decides.
    BudgetPaused {
        exec_id: u64,
        result_id: u64,
        rows: u64,
        layout_bytes: u64,
        /// Precautionary pause at N rows, not the memory budget.
        by_rows: bool,
    },
    Notice {
        exec_id: u64,
        index: u32,
        notice: ErrorOut,
    },
    StatementFinished {
        exec_id: u64,
        index: u32,
        outcome: OutcomeOut,
        elapsed_ms: f64,
    },
    Finished {
        exec_id: u64,
        state: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use alamo_core::meta::NativeType;

    fn col(source: Option<ColumnSource>) -> ColumnOut {
        let native = NativeType::MySql {
            code: 253,
            flags: 0,
            charset: 45,
        };
        let mut m = ColumnMeta::new("c", native, "varchar");
        m.source = source;
        ColumnOut::new(&m, ColumnKind::Utf8)
    }

    /// The name of a query's INSERT export comes from here: `db.table`,
    /// the table alone without db, nothing for computed columns.
    #[test]
    fn source_table_from_mysql_origin() {
        let my = |db: &str| ColumnSource::MySql {
            db: db.into(),
            org_table: "ma_config".into(),
            org_name: "c".into(),
        };
        assert_eq!(
            col(Some(my("g_transport"))).source_table.as_deref(),
            Some("g_transport.ma_config")
        );
        assert_eq!(col(Some(my(""))).source_table.as_deref(), Some("ma_config"));
        assert_eq!(col(None).source_table, None);
    }
}
