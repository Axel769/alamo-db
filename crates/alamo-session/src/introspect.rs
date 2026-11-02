//! Session-side introspection (§3, §10): a **dedicated** connection, opened on
//! first use and reopened if it drops. Never the sticky one: browsing the
//! schema during an open transaction would block or read inconsistent data.
//!
//! The types serialized towards the UI live here too, so `app` stays pure
//! wiring (same pact as `events.rs`).

use alamo_core::schema_info::{ColumnInfo, ForeignKey, RelationInfo, SchemaInfo, SchemaStats};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct SchemaOut {
    pub name: String,
    pub is_system: bool,
}

impl From<SchemaInfo> for SchemaOut {
    fn from(s: SchemaInfo) -> Self {
        SchemaOut {
            name: s.name,
            is_system: s.is_system,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SchemaStatsOut {
    pub name: String,
    pub is_system: bool,
    pub tables: u64,
    pub size_bytes: Option<u64>,
    pub charset: Option<String>,
    pub collation: Option<String>,
}

impl From<SchemaStats> for SchemaStatsOut {
    fn from(s: SchemaStats) -> Self {
        SchemaStatsOut {
            name: s.name,
            is_system: s.is_system,
            tables: s.tables,
            size_bytes: s.size_bytes,
            charset: s.charset,
            collation: s.collation,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ForeignKeyOut {
    pub name: String,
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

impl From<ForeignKey> for ForeignKeyOut {
    fn from(f: ForeignKey) -> Self {
        ForeignKeyOut {
            name: f.name,
            schema: f.schema,
            table: f.table,
            columns: f.columns,
            ref_schema: f.ref_schema,
            ref_table: f.ref_table,
            ref_columns: f.ref_columns,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationOut {
    pub schema: String,
    pub name: String,
    /// `table | view | materialized_view | foreign_table | partitioned_table`.
    pub kind: &'static str,
    pub estimated_rows: Option<u64>,
    pub comment: Option<String>,
}

impl From<RelationInfo> for RelationOut {
    fn from(r: RelationInfo) -> Self {
        RelationOut {
            schema: r.schema,
            name: r.name,
            kind: r.kind.as_str(),
            estimated_rows: r.estimated_rows,
            comment: r.comment,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ColumnInfoOut {
    pub name: String,
    pub type_display: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub is_primary_key: bool,
    pub ordinal: u32,
}

impl From<ColumnInfo> for ColumnInfoOut {
    fn from(c: ColumnInfo) -> Self {
        ColumnInfoOut {
            name: c.name,
            type_display: c.type_display,
            nullable: c.nullable,
            default: c.default,
            is_primary_key: c.is_primary_key,
            ordinal: c.ordinal,
        }
    }
}

/// Flat entry for autocomplete: the UI aggregates it into `schema.table → columns`.
#[derive(Debug, Clone, Serialize)]
pub struct AutocompleteOut {
    pub schema: String,
    pub table: String,
    pub column: String,
}
