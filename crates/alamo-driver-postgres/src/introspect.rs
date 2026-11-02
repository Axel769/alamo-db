//! PostgreSQL introspection (§10): **`pg_catalog`, never
//! `information_schema`** — richer and much faster. The queries run on the
//! connection dedicated to introspection (§3), never on the sticky one:
//! browsing the schema during an open transaction must not block.

use alamo_core::schema_info::{
    ColumnInfo, ForeignKey, RelKind, RelationInfo, SchemaInfo, SchemaStats,
};
use tokio_postgres::Client;

use crate::PgError;

/// Schemas of the current database. The system ones (`pg_catalog`, toast,
/// temp, `information_schema`) are recognised here and the UI folds them at
/// the bottom.
pub async fn schemas(client: &Client) -> Result<Vec<SchemaInfo>, PgError> {
    let rows = client
        .query(
            "SELECT n.nspname
             FROM pg_catalog.pg_namespace n
             ORDER BY n.nspname",
            &[],
        )
        .await
        .map_err(PgError::from)?;
    Ok(rows
        .iter()
        .map(|r| {
            let name: String = r.get(0);
            let is_system = is_system_schema(&name);
            SchemaInfo { name, is_system }
        })
        .collect())
}

/// Databases of the server that accept connections: each one needs a
/// connection of its own (`can_change_database_in_session` false). The
/// templates are left out; none counts as a system database, `postgres`
/// included: users keep tables there too.
pub async fn databases(client: &Client) -> Result<Vec<SchemaInfo>, PgError> {
    let rows = client
        .query(
            "SELECT d.datname::text
             FROM pg_catalog.pg_database d
             WHERE d.datallowconn AND NOT d.datistemplate
             ORDER BY d.datname",
            &[],
        )
        .await
        .map_err(PgError::from)?;
    Ok(rows
        .iter()
        .map(|r| SchemaInfo {
            name: r.get(0),
            is_system: false,
        })
        .collect())
}

fn is_system_schema(name: &str) -> bool {
    name == "pg_catalog"
        || name == "information_schema"
        || name.starts_with("pg_toast")
        || name.starts_with("pg_temp")
}

/// Numbers per schema of the current database (connection page).
/// Size = `pg_total_relation_size` (data + indexes + toast) of the relations
/// with storage only; encoding and collation belong to the database, the
/// same for every schema.
pub async fn schema_stats(client: &Client) -> Result<Vec<SchemaStats>, PgError> {
    let rows = client
        .query(
            "SELECT n.nspname::text,
                    count(c.oid) FILTER (WHERE c.relkind IN ('r','p','v','m','f')),
                    (sum(pg_catalog.pg_total_relation_size(c.oid))
                        FILTER (WHERE c.relkind IN ('r','m')))::bigint,
                    pg_catalog.pg_encoding_to_char(d.encoding)::text,
                    d.datcollate::text
             FROM pg_catalog.pg_namespace n
             CROSS JOIN pg_catalog.pg_database d
             LEFT JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid
             WHERE d.datname = pg_catalog.current_database()
             GROUP BY n.nspname, d.encoding, d.datcollate
             ORDER BY n.nspname",
            &[],
        )
        .await
        .map_err(PgError::from)?;
    Ok(rows
        .iter()
        .map(|r| {
            let name: String = r.get(0);
            let tables: i64 = r.get(1);
            let size: Option<i64> = r.get(2);
            SchemaStats {
                is_system: is_system_schema(&name),
                name,
                tables: tables.max(0) as u64,
                size_bytes: size.map(|s| s.max(0) as u64),
                charset: r.get(3),
                collation: r.get(4),
            }
        })
        .collect())
}

/// Foreign keys touching the table, in both directions: those leaving from
/// it and those pointing to it (a self-reference appears once, and counts
/// for both). The columns follow the constraint order (`conkey`), not the
/// table's.
pub async fn foreign_keys(
    client: &Client,
    schema: &str,
    table: &str,
) -> Result<Vec<ForeignKey>, PgError> {
    let rows = client
        .query(
            "SELECT con.conname::text, n.nspname::text, c.relname::text,
                    (SELECT array_agg(a.attname::text ORDER BY k.ord)
                     FROM unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord)
                     JOIN pg_catalog.pg_attribute a
                       ON a.attrelid = con.conrelid AND a.attnum = k.attnum),
                    rn.nspname::text, rc.relname::text,
                    (SELECT array_agg(a.attname::text ORDER BY k.ord)
                     FROM unnest(con.confkey) WITH ORDINALITY AS k(attnum, ord)
                     JOIN pg_catalog.pg_attribute a
                       ON a.attrelid = con.confrelid AND a.attnum = k.attnum)
             FROM pg_catalog.pg_constraint con
             JOIN pg_catalog.pg_class c ON c.oid = con.conrelid
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             JOIN pg_catalog.pg_class rc ON rc.oid = con.confrelid
             JOIN pg_catalog.pg_namespace rn ON rn.oid = rc.relnamespace
             WHERE con.contype = 'f'
               AND ((n.nspname = $1 AND c.relname = $2)
                 OR (rn.nspname = $1 AND rc.relname = $2))
             ORDER BY n.nspname, c.relname, con.conname",
            &[&schema, &table],
        )
        .await
        .map_err(PgError::from)?;
    Ok(rows
        .iter()
        .map(|r| ForeignKey {
            name: r.get(0),
            schema: r.get(1),
            table: r.get(2),
            columns: r.get(3),
            ref_schema: r.get(4),
            ref_table: r.get(5),
            ref_columns: r.get(6),
        })
        .collect())
}

fn rel_kind(relkind: i8) -> Option<RelKind> {
    match relkind as u8 as char {
        'r' => Some(RelKind::Table),
        'p' => Some(RelKind::PartitionedTable),
        'v' => Some(RelKind::View),
        'm' => Some(RelKind::MaterializedView),
        'f' => Some(RelKind::ForeignTable),
        _ => None,
    }
}

/// Relations of a schema. `reltuples` is the planner's estimate: -1 = never
/// analysed (`None`), never a `COUNT(*)` — the tree must not cost scans.
pub async fn relations(client: &Client, schema: &str) -> Result<Vec<RelationInfo>, PgError> {
    let rows = client
        .query(
            "SELECT c.relname, c.relkind, c.reltuples::float8,
                    pg_catalog.obj_description(c.oid, 'pg_class')
             FROM pg_catalog.pg_class c
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1
               AND c.relkind IN ('r','p','v','m','f')
               AND NOT c.relispartition
             ORDER BY c.relname",
            &[&schema],
        )
        .await
        .map_err(PgError::from)?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let kind = rel_kind(r.get::<_, i8>(1))?;
            let reltuples: f64 = r.get(2);
            Some(RelationInfo {
                schema: schema.to_owned(),
                name: r.get(0),
                kind,
                estimated_rows: if reltuples < 0.0 {
                    None
                } else {
                    Some(reltuples as u64)
                },
                comment: r.get(3),
            })
        })
        .collect())
}

/// Names of the relations of every schema in one round trip, for the tree
/// filter: one query per schema over a slow link took too long with many
/// schemas. Names and kind only, like the MySQL side: no estimate, no
/// comment (one `obj_description` call per relation is not free either).
pub async fn relations_all(client: &Client) -> Result<Vec<RelationInfo>, PgError> {
    let rows = client
        .query(
            "SELECT n.nspname, c.relname, c.relkind
             FROM pg_catalog.pg_class c
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             WHERE c.relkind IN ('r','p','v','m','f')
               AND NOT c.relispartition
             ORDER BY n.nspname, c.relname",
            &[],
        )
        .await
        .map_err(PgError::from)?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some(RelationInfo {
                schema: r.get(0),
                name: r.get(1),
                kind: rel_kind(r.get::<_, i8>(2))?,
                estimated_rows: None,
                comment: None,
            })
        })
        .collect())
}

/// Columns of a relation, in `attnum` order. `format_type` produces the name
/// exactly as the server writes it (`character varying(16)`, not `varchar`);
/// the PK comes from `pg_index.indisprimary`.
pub async fn columns(
    client: &Client,
    schema: &str,
    table: &str,
) -> Result<Vec<ColumnInfo>, PgError> {
    let rows = client
        .query(
            "SELECT a.attname,
                    pg_catalog.format_type(a.atttypid, a.atttypmod),
                    NOT a.attnotnull,
                    pg_catalog.pg_get_expr(d.adbin, d.adrelid),
                    a.attnum,
                    COALESCE((SELECT true
                              FROM pg_catalog.pg_index i
                              WHERE i.indrelid = c.oid AND i.indisprimary
                                AND a.attnum = ANY (i.indkey)), false)
             FROM pg_catalog.pg_attribute a
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             LEFT JOIN pg_catalog.pg_attrdef d
                    ON d.adrelid = a.attrelid AND d.adnum = a.attnum
             WHERE n.nspname = $1 AND c.relname = $2
               AND a.attnum > 0 AND NOT a.attisdropped
             ORDER BY a.attnum",
            &[&schema, &table],
        )
        .await
        .map_err(PgError::from)?;
    Ok(rows
        .iter()
        .map(|r| ColumnInfo {
            name: r.get(0),
            type_display: r.get(1),
            nullable: r.get(2),
            default: r.get(3),
            ordinal: r.get::<_, i16>(4) as u32,
            is_primary_key: r.get(5),
        })
        .collect())
}

/// DDL rebuilt from the catalog for the DDL sub-tab of the table tab
/// (§9.5). It is not `pg_dump`: it covers columns, constraints and indexes —
/// what is needed to read the structure, not for a faithful restore.
pub async fn table_ddl(client: &Client, schema: &str, table: &str) -> Result<String, PgError> {
    let head = client
        .query_opt(
            "SELECT c.relkind, c.oid, pg_catalog.pg_get_partkeydef(c.oid)
             FROM pg_catalog.pg_class c
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND c.relname = $2",
            &[&schema, &table],
        )
        .await
        .map_err(PgError::from)?;
    let Some(head) = head else {
        return Ok(format!("-- relation {schema}.{table} not found"));
    };
    let relkind = head.get::<_, i8>(0) as u8 as char;
    let oid: u32 = head.get(1);
    let partkey: Option<String> = head.get(2);
    let qname = format!("{}.{}", quote_ident(schema), quote_ident(table));

    // Views: the server already knows how to write the definition.
    if relkind == 'v' || relkind == 'm' {
        let def: String = client
            .query_one(
                "SELECT pg_catalog.pg_get_viewdef($1::pg_catalog.oid, true)",
                &[&oid],
            )
            .await
            .map_err(PgError::from)?
            .get(0);
        let kw = if relkind == 'm' {
            "MATERIALIZED VIEW"
        } else {
            "VIEW"
        };
        let mut ddl = format!("CREATE {kw} {qname} AS\n{def}");
        if relkind == 'm' {
            append_indexes(client, oid, &mut ddl).await?;
        }
        return Ok(ddl);
    }

    let cols = columns(client, schema, table).await?;
    // Identity and generated columns are not DEFAULT: rendering them as such
    // would be false DDL. `attidentity` ('a' = GENERATED ALWAYS AS IDENTITY,
    // 'd' = GENERATED BY DEFAULT AS IDENTITY) and `attgenerated` ('s' =
    // GENERATED ALWAYS AS (expr) STORED) tell the cases apart; for generated
    // columns the expression is in `pg_attrdef` — the same one `columns`
    // reports as default. `attgenerated` exists since PG12: the file already
    // assumes a modern server without version gating (`pg_get_partkeydef`
    // is PG10+).
    let special: std::collections::HashMap<String, (String, String)> = client
        .query(
            "SELECT a.attname, a.attidentity::pg_catalog.text,
                    a.attgenerated::pg_catalog.text
             FROM pg_catalog.pg_attribute a
             WHERE a.attrelid = $1::pg_catalog.oid
               AND a.attnum > 0 AND NOT a.attisdropped",
            &[&oid],
        )
        .await
        .map_err(PgError::from)?
        .iter()
        .map(|r| (r.get(0), (r.get(1), r.get(2))))
        .collect();
    let mut lines: Vec<String> = cols
        .iter()
        .map(|c| {
            let mut l = format!("    {} {}", quote_ident(&c.name), c.type_display);
            let (identity, generated) = special
                .get(&c.name)
                .map(|(i, g)| (i.as_str(), g.as_str()))
                .unwrap_or(("", ""));
            if generated == "s" {
                if let Some(d) = &c.default {
                    l.push_str(&format!(" GENERATED ALWAYS AS ({d}) STORED"));
                }
            } else if identity == "a" {
                l.push_str(" GENERATED ALWAYS AS IDENTITY");
            } else if identity == "d" {
                l.push_str(" GENERATED BY DEFAULT AS IDENTITY");
            } else if let Some(d) = &c.default {
                l.push_str(&format!(" DEFAULT {d}"));
            }
            if !c.nullable {
                l.push_str(" NOT NULL");
            }
            l
        })
        .collect();
    for r in client
        .query(
            "SELECT conname, pg_catalog.pg_get_constraintdef(oid, true)
             FROM pg_catalog.pg_constraint
             WHERE conrelid = $1::pg_catalog.oid
             ORDER BY CASE contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 ELSE 2 END, conname",
            &[&oid],
        )
        .await
        .map_err(PgError::from)?
    {
        let name: String = r.get(0);
        let def: String = r.get(1);
        lines.push(format!("    CONSTRAINT {} {}", quote_ident(&name), def));
    }
    let kw = if relkind == 'f' {
        "FOREIGN TABLE"
    } else {
        "TABLE"
    };
    let mut ddl = format!("CREATE {kw} {qname} (\n{}\n)", lines.join(",\n"));
    if let Some(pk) = partkey {
        ddl.push_str(&format!("\nPARTITION BY {pk}"));
    }
    ddl.push(';');
    append_indexes(client, oid, &mut ddl).await?;
    Ok(ddl)
}

/// Indexes that do not come from a constraint (those are already in the `CREATE`).
async fn append_indexes(client: &Client, oid: u32, ddl: &mut String) -> Result<(), PgError> {
    for r in client
        .query(
            "SELECT pg_catalog.pg_get_indexdef(i.indexrelid, 0, true)
             FROM pg_catalog.pg_index i
             WHERE i.indrelid = $1::pg_catalog.oid
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint con
                               WHERE con.conindid = i.indexrelid)
             ORDER BY 1",
            &[&oid],
        )
        .await
        .map_err(PgError::from)?
    {
        let def: String = r.get(0);
        ddl.push_str(&format!("\n{def};"));
    }
    Ok(())
}

/// Quoting of an identifier, identical to the server's `quote_ident`: always
/// between double quotes, with the inner double quotes doubled.
pub fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Flat catalog for autocomplete (§9.2): all the columns of the non-system
/// schemas in **one** query. On a typical DB they are a few thousand rows;
/// the cap avoids choking the editor on a pathological catalog.
pub struct AutocompleteEntry {
    pub schema: String,
    pub table: String,
    pub column: String,
}

pub const AUTOCOMPLETE_CAP: i64 = 50_000;

/// `schema` = only that schema (same contract as the MySQL driver).
pub async fn autocomplete_catalog(
    client: &Client,
    schema: Option<&str>,
) -> Result<Vec<AutocompleteEntry>, PgError> {
    let rows = client
        .query(
            "SELECT n.nspname, c.relname, a.attname
             FROM pg_catalog.pg_attribute a
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             WHERE c.relkind IN ('r','p','v','m','f')
               AND NOT c.relispartition
               AND a.attnum > 0 AND NOT a.attisdropped
               AND n.nspname NOT IN ('pg_catalog','information_schema')
               AND n.nspname NOT LIKE 'pg\\_toast%'
               AND n.nspname NOT LIKE 'pg\\_temp%'
               AND ($2::text IS NULL OR n.nspname = $2)
             ORDER BY n.nspname, c.relname, a.attnum
             LIMIT $1",
            &[&AUTOCOMPLETE_CAP, &schema],
        )
        .await
        .map_err(PgError::from)?;
    Ok(rows
        .iter()
        .map(|r| AutocompleteEntry {
            schema: r.get(0),
            table: r.get(1),
            column: r.get(2),
        })
        .collect())
}
