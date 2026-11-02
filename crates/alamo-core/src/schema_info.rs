//! Introspection (§10): entities of the schema tree, independent of the dialect.
//!
//! They are pure data: the queries that populate them live in the drivers
//! (`pg_catalog` for PG, `information_schema`/`SHOW` for MySQL at M3), the
//! cache and the invalidation live in the frontend (TanStack Query, §9.2).
//! Here only the common shape, without dependencies — constraint of §2.

/// A schema (namespace). In MySQL "schema" and "database" coincide: the
/// distinction is made by `Capabilities::supports_schemas`, not by this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaInfo {
    pub name: String,
    /// System schema (`pg_catalog`, `information_schema`, toast): the UI shows
    /// them at the bottom, collapsed, never hidden — "I can't find pg_stat..."
    /// is a classic ticket for tools that filter them out.
    pub is_system: bool,
}

/// A foreign key: `schema.table(columns)` points to
/// `ref_schema.ref_table(ref_columns)`, columns in constraint order.
/// It serves navigation between tables: from the child row to the parent
/// record and from the parent to the child rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: String,
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

/// Numbers of a schema for the connection page: how many relations, how much
/// it weighs, with which charset. A more expensive query than the listing (it
/// sums the sizes of all tables): the UI asks for it separately and shows the
/// names without waiting for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaStats {
    pub name: String,
    pub is_system: bool,
    /// Tables and views.
    pub tables: u64,
    /// Data + indexes. `None` if the engine cannot say (views only).
    pub size_bytes: Option<u64>,
    /// MySQL: the schema default. PG: the database encoding (the same for
    /// all schemas).
    pub charset: Option<String>,
    pub collation: Option<String>,
}

/// How many connections the user may hold and how many it already has open
/// (§3.2): used to avoid opening auxiliary lanes until it runs out of seats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionBudget {
    /// Effective limit of the account (on MySQL the `WITH
    /// MAX_USER_CONNECTIONS` one if present, otherwise the global one).
    pub limit: u32,
    /// Connections open with the same user name, from any program (other
    /// profiles, HeidiSQL, DBeaver), including this one.
    pub in_use: u32,
}

/// What a relation is in the tree. Covers PG; MySQL uses `Table`/`View`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelKind {
    Table,
    View,
    MaterializedView,
    ForeignTable,
    /// Partitioned table (the root; the child partitions are `Table`).
    PartitionedTable,
}

impl RelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RelKind::Table => "table",
            RelKind::View => "view",
            RelKind::MaterializedView => "materialized_view",
            RelKind::ForeignTable => "foreign_table",
            RelKind::PartitionedTable => "partitioned_table",
        }
    }
}

/// A relation (table, view, …) inside a schema.
#[derive(Debug, Clone, PartialEq)]
pub struct RelationInfo {
    pub schema: String,
    pub name: String,
    pub kind: RelKind,
    /// Planner estimate (`reltuples`), never a `COUNT(*)`: browsing the tree
    /// must not cost a scan. `None` = never analyzed.
    pub estimated_rows: Option<u64>,
    pub comment: Option<String>,
}

/// A column as it appears in the tree and in autocomplete. It is not
/// `ColumnMeta` (§4.3): that one is captured at execution from the result set,
/// this one comes from the catalog and includes what the protocol does not
/// say (default, PK membership).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    /// Type name as the server writes it (`format_type` in PG).
    pub type_display: String,
    pub nullable: bool,
    /// Default expression, textual, if present.
    pub default: Option<String>,
    pub is_primary_key: bool,
    /// 1-based position in the table.
    pub ordinal: u32,
}
