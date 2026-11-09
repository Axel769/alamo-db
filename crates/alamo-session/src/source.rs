//! §4.3.1: from a result set to the table it can be written to.
//!
//! The drivers say where each column was read from (`ColumnMeta.source`,
//! resolved to names on the catalog lane); the rule here decides whether the
//! result can be edited and, when not, why — in words the UI shows as they
//! are. Pure: no I/O, so every branch has a unit test.

use std::collections::HashMap;

use alamo_core::dialect::Capabilities;
use alamo_core::meta::ColumnMeta;
use alamo_core::schema_info::{ColumnInfo, SourceColumn};
use serde::Serialize;

/// What the UI needs to edit a result set, or the reason it cannot.
#[derive(Debug, Clone, Serialize)]
pub struct ResultSourceOut {
    /// The one table the sourced columns come from; `None` without one.
    pub schema: Option<String>,
    pub table: Option<String>,
    /// Per result column, by position: the source column name, `None` for a
    /// computed column or a later duplicate of a column already in the
    /// result (editing the first is enough, two would race).
    pub columns: Vec<Option<String>>,
    /// The primary key, in constraint order: result column index and name.
    /// Empty when the result is not editable.
    pub key: Vec<KeyColumnOut>,
    /// Per result column: whether the source column takes NULL, and its
    /// default expression. `None` for computed columns.
    pub nullable: Vec<Option<bool>>,
    pub defaults: Vec<Option<String>>,
    pub editable: bool,
    pub reason: Option<NotEditable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyColumnOut {
    pub col: usize,
    pub name: String,
}

/// Why a result set stays read-only. Each carries what the user needs to
/// fix the query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotEditable {
    /// No column was read from a table (computed values, derived tables).
    NoSource,
    /// The sourced columns come from more than one table.
    SeveralTables {
        tables: Vec<String>,
    },
    NoPrimaryKey {
        table: String,
    },
    /// Key columns that are not in the result.
    KeyMissing {
        table: String,
        columns: Vec<String>,
    },
    /// Key columns present under another name: the writes locate them by
    /// name in the result.
    KeyRenamed {
        table: String,
        columns: Vec<String>,
    },
    /// Another column of the result carries the name of a key column.
    KeyShadowed {
        table: String,
        columns: Vec<String>,
    },
}

fn read_only(columns: Vec<Option<String>>, reason: NotEditable) -> ResultSourceOut {
    let n = columns.len();
    ResultSourceOut {
        schema: None,
        table: None,
        columns,
        key: Vec::new(),
        nullable: vec![None; n],
        defaults: vec![None; n],
        editable: false,
        reason: Some(reason),
    }
}

/// `schema`: the result's columns; `resolved`: their sources by position;
/// `info`: the table's columns from introspection, when exactly one table
/// was found (the caller looks it up with `single_table`).
pub(crate) fn classify(
    schema: &[ColumnMeta],
    resolved: &[Option<SourceColumn>],
    info: Option<&[ColumnInfo]>,
    caps: &Capabilities,
) -> ResultSourceOut {
    debug_assert_eq!(schema.len(), resolved.len());
    let Some((table_schema, table)) = single_table(resolved) else {
        let mut tables: Vec<String> = resolved
            .iter()
            .flatten()
            .map(|s| format!("{}.{}", s.schema, s.table))
            .collect();
        tables.sort();
        tables.dedup();
        let columns = vec![None; schema.len()];
        return if tables.is_empty() {
            read_only(columns, NotEditable::NoSource)
        } else {
            read_only(columns, NotEditable::SeveralTables { tables })
        };
    };
    let table_name = format!("{table_schema}.{table}");

    // Source column per result column; a source already taken stays `None`.
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let columns: Vec<Option<String>> = resolved
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let s = s.as_ref()?;
            match seen.entry(s.column.as_str()) {
                std::collections::hash_map::Entry::Occupied(_) => None,
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(i);
                    Some(s.column.clone())
                }
            }
        })
        .collect();

    let info = info.unwrap_or(&[]);
    let by_name: HashMap<&str, &ColumnInfo> = info.iter().map(|c| (c.name.as_str(), c)).collect();
    let nullable: Vec<Option<bool>> = columns
        .iter()
        .map(|c| {
            c.as_deref()
                .and_then(|n| by_name.get(n))
                .map(|c| c.nullable)
        })
        .collect();
    let defaults: Vec<Option<String>> = columns
        .iter()
        .map(|c| {
            c.as_deref()
                .and_then(|n| by_name.get(n))
                .and_then(|c| c.default.clone())
        })
        .collect();

    let mut pk: Vec<&ColumnInfo> = info.iter().filter(|c| c.is_primary_key).collect();
    pk.sort_by_key(|c| c.ordinal);
    let finish = |key: Vec<KeyColumnOut>, reason: Option<NotEditable>| ResultSourceOut {
        schema: Some(table_schema.to_string()),
        table: Some(table.to_string()),
        columns: columns.clone(),
        key,
        nullable: nullable.clone(),
        defaults: defaults.clone(),
        editable: reason.is_none(),
        reason,
    };
    if pk.is_empty() && !caps.editable_without_pk {
        return finish(
            Vec::new(),
            Some(NotEditable::NoPrimaryKey { table: table_name }),
        );
    }

    let mut key = Vec::new();
    let (mut missing, mut renamed, mut shadowed) = (Vec::new(), Vec::new(), Vec::new());
    for k in &pk {
        let Some(&i) = seen.get(k.name.as_str()) else {
            missing.push(k.name.clone());
            continue;
        };
        if schema[i].name != k.name {
            renamed.push(k.name.clone());
            continue;
        }
        // A second copy of the key column itself (`SELECT id, id`) answers
        // to the same name but holds the same value: the lookups by name
        // land on the first one, which is the key. Any other column under
        // that name would hijack them.
        if schema
            .iter()
            .enumerate()
            .any(|(j, m)| j != i && m.name == k.name && resolved[j] != resolved[i])
        {
            shadowed.push(k.name.clone());
            continue;
        }
        key.push(KeyColumnOut {
            col: i,
            name: k.name.clone(),
        });
    }
    let reason = if !missing.is_empty() {
        Some(NotEditable::KeyMissing {
            table: table_name,
            columns: missing,
        })
    } else if !renamed.is_empty() {
        Some(NotEditable::KeyRenamed {
            table: table_name,
            columns: renamed,
        })
    } else if !shadowed.is_empty() {
        Some(NotEditable::KeyShadowed {
            table: table_name,
            columns: shadowed,
        })
    } else {
        None
    };
    if reason.is_some() {
        key.clear();
    }
    finish(key, reason)
}

/// The one `(schema, table)` every sourced column comes from; `None` with
/// no source at all or with several tables.
pub(crate) fn single_table(resolved: &[Option<SourceColumn>]) -> Option<(&str, &str)> {
    let mut found: Option<(&str, &str)> = None;
    for s in resolved.iter().flatten() {
        let t = (s.schema.as_str(), s.table.as_str());
        match found {
            None => found = Some(t),
            Some(f) if f == t => {}
            Some(_) => return None,
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use alamo_core::dialect::Dialect;
    use alamo_core::meta::NativeType;

    fn meta(name: &str) -> ColumnMeta {
        ColumnMeta::new(
            name,
            NativeType::Postgres {
                oid: 23,
                type_modifier: -1,
            },
            "integer",
        )
    }

    fn src(column: &str) -> Option<SourceColumn> {
        Some(SourceColumn {
            schema: "public".into(),
            table: "t".into(),
            column: column.into(),
        })
    }

    fn col(name: &str, pk: bool, ordinal: u32) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            type_display: "integer".into(),
            nullable: !pk,
            default: (!pk).then(|| "0".to_string()),
            is_primary_key: pk,
            ordinal,
        }
    }

    fn caps() -> Capabilities {
        alamo_driver_postgres::PgDialect::default().capabilities()
    }

    fn table() -> Vec<ColumnInfo> {
        vec![
            col("id", true, 1),
            col("nome", false, 2),
            col("n", false, 3),
        ]
    }

    #[test]
    fn plain_select_is_editable_with_its_key() {
        let schema = [meta("id"), meta("nome"), meta("n")];
        let resolved = [src("id"), src("nome"), src("n")];
        let out = classify(&schema, &resolved, Some(&table()), &caps());
        assert!(out.editable);
        assert_eq!(out.reason, None);
        assert_eq!(out.schema.as_deref(), Some("public"));
        assert_eq!(out.table.as_deref(), Some("t"));
        assert_eq!(
            out.key,
            vec![KeyColumnOut {
                col: 0,
                name: "id".into()
            }]
        );
        assert_eq!(out.nullable, vec![Some(false), Some(true), Some(true)]);
        assert_eq!(out.defaults, vec![None, Some("0".into()), Some("0".into())]);
    }

    #[test]
    fn aliases_and_computed_columns_keep_the_rest_editable() {
        let schema = [meta("id"), meta("label"), meta("twice"), meta("id2")];
        let resolved = [src("id"), src("nome"), None, src("id")];
        let out = classify(&schema, &resolved, Some(&table()), &caps());
        assert!(out.editable);
        assert_eq!(
            out.columns,
            vec![Some("id".into()), Some("nome".into()), None, None]
        );
        assert_eq!(out.nullable[2], None);
        assert_eq!(out.key.len(), 1);
    }

    #[test]
    fn renamed_key_is_refused() {
        let schema = [meta("k"), meta("nome")];
        let resolved = [src("id"), src("nome")];
        let out = classify(&schema, &resolved, Some(&table()), &caps());
        assert!(!out.editable);
        assert_eq!(
            out.reason,
            Some(NotEditable::KeyRenamed {
                table: "public.t".into(),
                columns: vec!["id".into()]
            })
        );
        assert!(out.key.is_empty());
    }

    #[test]
    fn missing_key_is_refused() {
        let schema = [meta("nome"), meta("n")];
        let resolved = [src("nome"), src("n")];
        let out = classify(&schema, &resolved, Some(&table()), &caps());
        assert_eq!(
            out.reason,
            Some(NotEditable::KeyMissing {
                table: "public.t".into(),
                columns: vec!["id".into()]
            })
        );
    }

    #[test]
    fn shadowed_key_is_refused() {
        // `SELECT nome AS id, id FROM t`: the key is there, but another
        // column answers to its name.
        let schema = [meta("id"), meta("id")];
        let resolved = [src("nome"), src("id")];
        let out = classify(&schema, &resolved, Some(&table()), &caps());
        assert_eq!(
            out.reason,
            Some(NotEditable::KeyShadowed {
                table: "public.t".into(),
                columns: vec!["id".into()]
            })
        );
    }

    #[test]
    fn composite_key_reports_every_missing_column() {
        let info = vec![col("k1", true, 1), col("k2", true, 2), col("v", false, 3)];
        let schema = [meta("k1"), meta("v")];
        let resolved = [src("k1"), src("v")];
        let out = classify(&schema, &resolved, Some(&info), &caps());
        assert_eq!(
            out.reason,
            Some(NotEditable::KeyMissing {
                table: "public.t".into(),
                columns: vec!["k2".into()]
            })
        );
    }

    #[test]
    fn no_primary_key() {
        let info = vec![col("a", false, 1)];
        let schema = [meta("a")];
        let resolved = [src("a")];
        let out = classify(&schema, &resolved, Some(&info), &caps());
        assert_eq!(
            out.reason,
            Some(NotEditable::NoPrimaryKey {
                table: "public.t".into()
            })
        );
        assert_eq!(out.columns, vec![Some("a".into())]);
    }

    #[test]
    fn no_source_and_several_tables() {
        let schema = [meta("a"), meta("b")];
        let out = classify(&schema, &[None, None], None, &caps());
        assert_eq!(out.reason, Some(NotEditable::NoSource));
        assert_eq!(out.columns, vec![None, None]);

        let other = Some(SourceColumn {
            schema: "public".into(),
            table: "u".into(),
            column: "b".into(),
        });
        let out = classify(&schema, &[src("a"), other], None, &caps());
        assert_eq!(
            out.reason,
            Some(NotEditable::SeveralTables {
                tables: vec!["public.t".into(), "public.u".into()]
            })
        );
    }
}
