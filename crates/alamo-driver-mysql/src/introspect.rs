//! MySQL/MariaDB introspection (§10).
//!
//! Here `information_schema` is fine: the ban in §10 is a **PostgreSQL**
//! performance rule (pg_catalog is richer and faster); in MySQL the
//! information_schema IS the catalog. "Schema" and "database" are the same
//! thing: the tree shows databases as schemas.

use alamo_core::schema_info::{
    ColumnInfo, ForeignKey, RelKind, RelationInfo, SchemaInfo, SchemaStats,
};
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Row};

use crate::error::{first_as, rows_as, MyError};

fn is_system(name: &str) -> bool {
    matches!(
        name,
        "mysql" | "information_schema" | "performance_schema" | "sys"
    )
}

pub async fn schemas(conn: &mut Conn) -> Result<Vec<SchemaInfo>, MyError> {
    let names: Vec<String> = conn
        .query::<Row, _>("SELECT schema_name FROM information_schema.schemata ORDER BY schema_name")
        .await
        .map_err(MyError::from)
        .and_then(rows_as)?;
    Ok(names
        .into_iter()
        .map(|name| {
            let is_system = is_system(&name);
            SchemaInfo { name, is_system }
        })
        .collect())
}

/// Row of `schema_stats`: name, charset, collation, tables, bytes.
type StatsRow = (String, Option<String>, Option<String>, u64, Option<u64>);

/// Numbers per database (connection page). `data_length + index_length` is
/// the engine's estimate, like `table_rows`: no data scan. On servers with
/// many databases reading `information_schema.tables` can still be costly:
/// that is why it is a separate call, never on the tree's path.
pub async fn schema_stats(conn: &mut Conn) -> Result<Vec<SchemaStats>, MyError> {
    let rows: Vec<StatsRow> = conn
        .query::<Row, _>(
            "SELECT s.schema_name, s.default_character_set_name, s.default_collation_name,
                    COUNT(t.table_name),
                    CAST(SUM(t.data_length + t.index_length) AS UNSIGNED)
             FROM information_schema.schemata s
             LEFT JOIN information_schema.tables t ON t.table_schema = s.schema_name
             GROUP BY s.schema_name, s.default_character_set_name, s.default_collation_name
             ORDER BY s.schema_name",
        )
        .await
        .map_err(MyError::from)
        .and_then(rows_as)?;
    Ok(rows
        .into_iter()
        .map(
            |(name, charset, collation, tables, size_bytes)| SchemaStats {
                is_system: is_system(&name),
                name,
                tables,
                size_bytes,
                charset,
                collation,
            },
        )
        .collect())
}

/// `table_rows` is the engine's estimate (InnoDB): never a `COUNT(*)` — the
/// tree must not cost scans. On views it is NULL.
pub async fn relations(conn: &mut Conn, schema: &str) -> Result<Vec<RelationInfo>, MyError> {
    let rows: Vec<(String, String, Option<u64>, Option<String>)> = conn
        .exec::<Row, _, _>(
            "SELECT table_name, table_type, table_rows,
                    CASE WHEN table_type = 'VIEW' THEN NULL
                         ELSE NULLIF(table_comment, '') END
             FROM information_schema.tables
             WHERE table_schema = ?
               AND table_type IN ('BASE TABLE', 'VIEW', 'SYSTEM VIEW',
                                  'SYSTEM VERSIONED')
             ORDER BY table_name",
            (schema,),
        )
        .await
        .map_err(MyError::from)
        .and_then(rows_as)?;
    Ok(rows
        .into_iter()
        .map(|(name, ttype, estimated_rows, comment)| RelationInfo {
            schema: schema.to_owned(),
            name,
            // 'SYSTEM VERSIONED' is MariaDB's system-versioned table: for
            // the tree it is a normal table, not a view.
            kind: if ttype == "BASE TABLE" || ttype == "SYSTEM VERSIONED" {
                RelKind::Table
            } else {
                RelKind::View
            },
            estimated_rows,
            comment,
        })
        .collect())
}

/// Names of the relations of every database in one round trip, for the
/// tree filter: with many databases over a slow link (SSH tunnel to a
/// remote server) one query per database took tens of seconds, serialized
/// on the introspection connection. Names and type only: `table_rows`
/// (and the comment) make the server open or refresh the statistics of
/// every table of every database — minutes on a hosted server with
/// thousands of tables — while schema, name and type come from the
/// dictionary. No row estimate (`None`) and no comment: the full rows of a
/// database arrive from `relations` when it is opened.
pub async fn relations_all(conn: &mut Conn) -> Result<Vec<RelationInfo>, MyError> {
    let rows: Vec<(String, String, String)> = conn
        .query::<Row, _>(
            "SELECT table_schema, table_name, table_type
             FROM information_schema.tables
             ORDER BY table_schema, table_name",
        )
        .await
        .map_err(MyError::from)
        .and_then(rows_as)?;
    Ok(rows
        .into_iter()
        .map(|(schema, name, ttype)| RelationInfo {
            schema,
            name,
            kind: if ttype == "BASE TABLE" || ttype == "SYSTEM VERSIONED" {
                RelKind::Table
            } else {
                RelKind::View
            },
            estimated_rows: None,
            comment: None,
        })
        .collect())
}

/// Row of `key_column_usage`: constraint, child (schema, table, column),
/// parent (schema, table, column).
type FkRow = (String, String, String, String, String, String, String);

/// Foreign keys touching the table, in both directions (see the PostgreSQL
/// twin). `key_column_usage` gives one row per column: grouped by
/// constraint, which in MySQL is unique per schema+table.
pub async fn foreign_keys(
    conn: &mut Conn,
    schema: &str,
    table: &str,
) -> Result<Vec<ForeignKey>, MyError> {
    let rows: Vec<FkRow> = conn
        .exec::<Row, _, _>(
            "SELECT constraint_name, table_schema, table_name, column_name,
                    referenced_table_schema, referenced_table_name, referenced_column_name
             FROM information_schema.key_column_usage
             WHERE referenced_table_name IS NOT NULL
               AND ((table_schema = ? AND table_name = ?)
                 OR (referenced_table_schema = ? AND referenced_table_name = ?))
             ORDER BY table_schema, table_name, constraint_name, ordinal_position",
            (schema, table, schema, table),
        )
        .await
        .map_err(MyError::from)
        .and_then(rows_as)?;
    let mut out: Vec<ForeignKey> = Vec::new();
    for (name, schema, table, column, ref_schema, ref_table, ref_column) in rows {
        match out.last_mut() {
            Some(fk) if fk.name == name && fk.schema == schema && fk.table == table => {
                fk.columns.push(column);
                fk.ref_columns.push(ref_column);
            }
            _ => out.push(ForeignKey {
                name,
                schema,
                table,
                columns: vec![column],
                ref_schema,
                ref_table,
                ref_columns: vec![ref_column],
            }),
        }
    }
    Ok(out)
}

pub async fn columns(
    conn: &mut Conn,
    schema: &str,
    table: &str,
) -> Result<Vec<ColumnInfo>, MyError> {
    let rows: Vec<(String, String, String, Option<String>, u32, String)> = conn
        .exec::<Row, _, _>(
            "SELECT column_name, column_type, is_nullable, column_default,
                    ordinal_position, column_key
             FROM information_schema.columns
             WHERE table_schema = ? AND table_name = ?
             ORDER BY ordinal_position",
            (schema, table),
        )
        .await
        .map_err(MyError::from)
        .and_then(rows_as)?;
    Ok(rows
        .into_iter()
        .map(
            |(name, type_display, nullable, default, ordinal, key)| ColumnInfo {
                name,
                type_display,
                nullable: nullable == "YES",
                default,
                ordinal,
                is_primary_key: key == "PRI",
            },
        )
        .collect())
}

pub struct AutocompleteEntry {
    pub schema: String,
    pub table: String,
    pub column: String,
}

pub const AUTOCOMPLETE_CAP: i64 = 50_000;

/// `schema` = only that database: on a server with dozens of databases the
/// whole catalog exceeds the cap (sorted by name, the last ones are left
/// out) and is slow; the current database's arrives at once and whole.
pub async fn autocomplete_catalog(
    conn: &mut Conn,
    schema: Option<&str>,
) -> Result<Vec<AutocompleteEntry>, MyError> {
    let rows: Vec<(String, String, String)> = match schema {
        Some(schema) => {
            conn.exec::<Row, _, _>(
                "SELECT table_schema, table_name, column_name
                 FROM information_schema.columns
                 WHERE table_schema = ?
                 ORDER BY table_name, ordinal_position
                 LIMIT ?",
                (schema, AUTOCOMPLETE_CAP),
            )
            .await
        }
        None => {
            conn.exec::<Row, _, _>(
                "SELECT table_schema, table_name, column_name
                 FROM information_schema.columns
                 WHERE table_schema NOT IN ('mysql', 'information_schema',
                                            'performance_schema', 'sys')
                 ORDER BY table_schema, table_name, ordinal_position
                 LIMIT ?",
                (AUTOCOMPLETE_CAP,),
            )
            .await
        }
    }
    .map_err(MyError::from)
    .and_then(rows_as)?;
    Ok(rows
        .into_iter()
        .map(|(schema, table, column)| AutocompleteEntry {
            schema,
            table,
            column,
        })
        .collect())
}

/// DDL: here the server writes it (`SHOW CREATE ...`), no need to rebuild it
/// from the catalog as in PG.
pub async fn table_ddl(conn: &mut Conn, schema: &str, table: &str) -> Result<String, MyError> {
    let quoted = format!(
        "`{}`.`{}`",
        schema.replace('`', "``"),
        table.replace('`', "``")
    );
    let ttype: Option<String> = conn
        .exec_first::<Row, _, _>(
            "SELECT table_type FROM information_schema.tables
             WHERE table_schema = ? AND table_name = ?",
            (schema, table),
        )
        .await
        .map_err(MyError::from)
        .and_then(first_as)?;
    let Some(ttype) = ttype else {
        return Ok(format!("-- relation {schema}.{table} not found"));
    };
    // Here too 'SYSTEM VERSIONED' (MariaDB) is a table: the right path is
    // SHOW CREATE TABLE, not SHOW CREATE VIEW.
    if ttype == "BASE TABLE" || ttype == "SYSTEM VERSIONED" {
        let row: Option<(String, String)> = conn
            .query_first::<Row, _>(format!("SHOW CREATE TABLE {quoted}"))
            .await
            .map_err(MyError::from)
            .and_then(first_as)?;
        Ok(row.map(|(_, ddl)| ddl).unwrap_or_default())
    } else {
        // SHOW CREATE VIEW: (View, Create View, character_set, collation).
        let row: Option<(String, String, String, String)> = conn
            .query_first::<Row, _>(format!("SHOW CREATE VIEW {quoted}"))
            .await
            .map_err(MyError::from)
            .and_then(first_as)?;
        Ok(row.map(|(_, ddl, _, _)| ddl).unwrap_or_default())
    }
}
