//! Export and formatted copy of the result set — decision 11 (M4).
//!
//! The formats are TSV / CSV / JSON / INSERT. The registry is read in
//! windows (never a whole copy in memory) and for cells truncated in the
//! window (>4KB, LOB) the full value is resolved with `cell_full`: an
//! export must NEVER write clipped values. The local sort view (§4.8)
//! applies by itself: window and `cell_full` read through the
//! permutation — the export comes out in the order the user sees.
//!
//! JSON: values stay strings (or `null`): DECIMALs are lossless text by
//! choice (§4), converting them to JSON numbers would corrupt them.

use std::io::Write;

use alamo_core::dialect::Dialect;
use alamo_core::error::DbError;
use serde::Deserialize;

use crate::registry::LiveResult;

/// Rows per read window: large enough to amortize the lock, small enough
/// not to inflate memory.
const WINDOW_ROWS: u64 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Tsv,
    Csv,
    Json,
    Insert,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Tsv => "tsv",
            ExportFormat::Csv => "csv",
            ExportFormat::Json => "json",
            ExportFormat::Insert => "sql",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        }
    }
}

/// What the caller leaves out takes the `Default` below.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ExportOptions {
    pub format: ExportFormat,
    /// CSV only: the character between the fields (`,` when absent); TSV
    /// is always tab-separated.
    pub delimiter: Option<char>,
    pub line_ending: LineEnding,
    /// TSV/CSV files only: a UTF-8 byte order mark in front, for Excel on
    /// Windows. Never in the clipboard (see `bom_bytes`).
    pub bom: bool,
    /// Rows per statement in the INSERT format (at least 1).
    pub insert_batch: usize,
    /// Header row (TSV/CSV only).
    pub headers: bool,
    /// Table name for the INSERT format ("schema.table" allowed).
    pub table_name: String,
    /// TSV/CSV: a `'` in front of values a spreadsheet would read as a
    /// formula (see `neutralize_formula`). On if the caller says nothing: a
    /// `=HYPERLINK(…)` or a DDE command coming from the database must not
    /// execute when the file is opened.
    pub neutralize_formulas: bool,
    /// TSV/CSV: every field between quotes, not only the ones that need
    /// them (the Copy setting of §16).
    pub quote_all: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        ExportOptions {
            format: ExportFormat::Csv,
            delimiter: None,
            line_ending: LineEnding::Lf,
            bom: false,
            insert_batch: 500,
            headers: false,
            table_name: String::new(),
            neutralize_formulas: true,
            quote_all: false,
        }
    }
}

impl ExportOptions {
    /// What a file starts with: the UTF-8 byte order mark when asked for a
    /// delimited format, nothing otherwise (JSON forbids it, SQL does not
    /// want it, the clipboard never gets it).
    pub fn bom_bytes(&self) -> &'static [u8] {
        if self.bom && matches!(self.format, ExportFormat::Tsv | ExportFormat::Csv) {
            "\u{FEFF}".as_bytes()
        } else {
            &[]
        }
    }

    fn delimiter(&self) -> char {
        match self.format {
            ExportFormat::Tsv => '\t',
            _ => self.delimiter.unwrap_or(','),
        }
    }
}

fn delimited_field(v: &str, neutralize: bool) -> std::borrow::Cow<'_, str> {
    if neutralize {
        neutralize_formula(v)
    } else {
        v.into()
    }
}

/// Excel, LibreOffice and Google Sheets read as a formula a field starting
/// with `=`, `+`, `-`, `@` (even after leading tabs or newlines): a `'` is
/// prepended, which makes them read it as text (OWASP recommendation).
/// Numbers (`-5`, `+1.2e3`) stay as they were: they are data, not formulas.
fn neutralize_formula(v: &str) -> std::borrow::Cow<'_, str> {
    let risky = matches!(
        v.as_bytes().first(),
        Some(b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r' | b'\n')
    );
    if risky && v.trim().parse::<f64>().is_err() {
        format!("'{v}").into()
    } else {
        v.into()
    }
}

/// Exports the range `[row_start, row_end)` (columns `[col_start,
/// col_end)`) of the result set to `out`. Returns the rows written.
///
/// The caller picks the range: the whole result set for the export to
/// file, the selection rectangle for the copy.
pub fn render(
    live: &LiveResult,
    dialect: &dyn Dialect,
    opts: &ExportOptions,
    rows: std::ops::Range<u64>,
    cols: std::ops::Range<usize>,
    out: &mut dyn Write,
) -> Result<u64, Box<DbError>> {
    let schema = live.schema();
    let col_names: Vec<&str> = schema[cols.start.min(schema.len())..cols.end.min(schema.len())]
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    let io = |e: std::io::Error| Box::new(DbError::internal(format!("write failed: {e}")));
    let nl = opts.line_ending.as_str();

    match opts.format {
        ExportFormat::Tsv | ExportFormat::Csv => {
            let delim = opts.delimiter();
            let neutralize = opts.neutralize_formulas;
            if opts.headers {
                let names: Vec<_> = col_names
                    .iter()
                    .map(|n| delimited_field(n, neutralize))
                    .collect();
                let names: Vec<&str> = names.iter().map(AsRef::as_ref).collect();
                write_delimited_row(out, &names, delim, opts.quote_all, nl).map_err(io)?;
            }
            let mut written = 0u64;
            for_each_row(live, &rows, &cols, |cells| {
                let fields: Vec<_> = cells
                    .iter()
                    .map(|c| delimited_field(c.as_deref().unwrap_or(""), neutralize))
                    .collect();
                let fields: Vec<&str> = fields.iter().map(AsRef::as_ref).collect();
                write_delimited_row(out, &fields, delim, opts.quote_all, nl)?;
                written += 1;
                Ok(())
            })
            .map_err(io)?;
            Ok(written)
        }
        ExportFormat::Json => {
            out.write_all(b"[").map_err(io)?;
            let mut written = 0u64;
            for_each_row(live, &rows, &cols, |cells| {
                write!(out, "{}{nl}  {{", if written == 0 { "" } else { "," })?;
                for (i, (name, v)) in col_names.iter().zip(cells).enumerate() {
                    if i > 0 {
                        out.write_all(b", ")?;
                    }
                    // serde_json escapes keys and values.
                    let key = serde_json::to_string(name).expect("string");
                    out.write_all(key.as_bytes())?;
                    out.write_all(b": ")?;
                    match v {
                        Some(s) => {
                            let val = serde_json::to_string(s.as_str()).expect("string");
                            out.write_all(val.as_bytes())?;
                        }
                        None => out.write_all(b"null")?,
                    }
                }
                out.write_all(b"}")?;
                written += 1;
                Ok(())
            })
            .map_err(io)?;
            write!(out, "{}]{nl}", if written == 0 { "" } else { nl }).map_err(io)?;
            Ok(written)
        }
        ExportFormat::Insert => {
            let table = quote_table(dialect, &opts.table_name);
            let cols_sql = col_names
                .iter()
                .map(|n| dialect.quote_identifier(n))
                .collect::<Vec<_>>()
                .join(", ");
            let batch = opts.insert_batch.max(1);
            let mut written = 0u64;
            let mut in_batch = 0usize;
            for_each_row(live, &rows, &cols, |cells| {
                if in_batch == 0 {
                    write!(out, "INSERT INTO {table} ({cols_sql}) VALUES")?;
                } else {
                    out.write_all(b",")?;
                }
                write!(out, "{nl}  (")?;
                for (i, v) in cells.iter().enumerate() {
                    if i > 0 {
                        out.write_all(b", ")?;
                    }
                    match v {
                        Some(s) => out.write_all(dialect.quote_string_literal(s).as_bytes())?,
                        None => out.write_all(b"NULL")?,
                    }
                }
                out.write_all(b")")?;
                written += 1;
                in_batch += 1;
                if in_batch == batch {
                    write!(out, ";{nl}")?;
                    in_batch = 0;
                }
                Ok(())
            })
            .map_err(io)?;
            if in_batch > 0 {
                write!(out, ";{nl}").map_err(io)?;
            }
            Ok(written)
        }
    }
}

/// One result of a script export (§6.4: every statement keeps its own
/// result). Position and text come from the UI, which owns the editor; the
/// data come from the registry.
#[derive(Debug, Clone, Deserialize)]
pub struct ScriptItem {
    pub result_id: u64,
    /// Position of the statement in the script, from 1.
    pub position: usize,
    pub sql: String,
    /// INSERT target of this result ("schema.table" allowed).
    pub table_name: String,
}

/// All the results of a script in one file, one section per statement, each
/// introduced by its statement: a `[n] sql` row for TSV/CSV, a `-- [n] sql`
/// comment for INSERT, an object `{statement, sql, rows}` for JSON.
/// Returns the rows written.
pub fn render_script(
    results: &[(&ScriptItem, LiveResult)],
    dialect: &dyn Dialect,
    opts: &ExportOptions,
    out: &mut dyn Write,
) -> Result<u64, Box<DbError>> {
    let io = |e: std::io::Error| Box::new(DbError::internal(format!("write failed: {e}")));
    let json = opts.format == ExportFormat::Json;
    let nl = opts.line_ending.as_str();
    if json {
        out.write_all(b"[").map_err(io)?;
    }
    let mut total = 0u64;
    for (i, (item, live)) in results.iter().enumerate() {
        let label = one_line(&item.sql);
        match opts.format {
            ExportFormat::Tsv | ExportFormat::Csv => {
                if i > 0 {
                    out.write_all(nl.as_bytes()).map_err(io)?;
                }
                let title = format!("[{}] {label}", item.position);
                write_delimited_row(out, &[&title], opts.delimiter(), false, nl).map_err(io)?;
            }
            ExportFormat::Insert => {
                if i > 0 {
                    out.write_all(nl.as_bytes()).map_err(io)?;
                }
                write!(out, "-- [{}] {label}{nl}", item.position).map_err(io)?;
            }
            ExportFormat::Json => {
                let sql = serde_json::to_string(&label).expect("string");
                let sep = if i > 0 { "," } else { "" };
                write!(
                    out,
                    "{sep}{nl}{{\"statement\": {}, \"sql\": {sql}, \"rows\": ",
                    item.position
                )
                .map_err(io)?;
            }
        }
        let opts = ExportOptions {
            table_name: item.table_name.clone(),
            ..opts.clone()
        };
        total += render(
            live,
            dialect,
            &opts,
            0..live.rows(),
            0..live.schema().len(),
            out,
        )?;
        if json {
            out.write_all(b"}").map_err(io)?;
        }
    }
    if json {
        write!(out, "{nl}]{nl}").map_err(io)?;
    }
    Ok(total)
}

/// The results of a script as one file per statement in `dir`, named
/// `NN_<statement>.<ext>` (see `script_file_name`). Returns the rows written.
pub fn write_script_dir(
    results: &[(&ScriptItem, LiveResult)],
    dialect: &dyn Dialect,
    opts: &ExportOptions,
    dir: &std::path::Path,
) -> Result<u64, Box<DbError>> {
    let last = results.iter().map(|(it, _)| it.position).max().unwrap_or(1);
    let width = last.to_string().len().max(2);
    let mut total = 0u64;
    for (item, live) in results {
        let name = script_file_name(item.position, width, &item.sql, opts.format.extension());
        let file = std::fs::File::create(dir.join(&name))
            .map_err(|e| Box::new(DbError::internal(format!("opening file {name}: {e}"))))?;
        let mut out = std::io::BufWriter::new(file);
        out.write_all(opts.bom_bytes())
            .map_err(|e| Box::new(DbError::internal(format!("write failed: {e}"))))?;
        let opts = ExportOptions {
            table_name: item.table_name.clone(),
            ..opts.clone()
        };
        total += render(
            live,
            dialect,
            &opts,
            0..live.rows(),
            0..live.schema().len(),
            &mut out,
        )?;
        out.flush()
            .map_err(|e| Box::new(DbError::internal(format!("write failed: {e}"))))?;
    }
    Ok(total)
}

/// New folder `name` inside `parent`, with `-2`, `-3`… if it already
/// exists: a folder export never overwrites files of an earlier one.
pub fn create_export_dir(
    parent: &std::path::Path,
    name: &str,
) -> Result<std::path::PathBuf, Box<DbError>> {
    let base = file_stem(name, 80);
    let base = if base.is_empty() {
        "export".into()
    } else {
        base
    };
    for n in 1..=100 {
        let dir = if n == 1 {
            parent.join(&base)
        } else {
            parent.join(format!("{base}-{n}"))
        };
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(Box::new(DbError::internal(format!(
                    "creating folder {}: {e}",
                    dir.display()
                ))))
            }
        }
    }
    Err(Box::new(DbError::internal(format!(
        "creating folder {base}: too many folders with this name"
    ))))
}

/// `01_select_t_count_from_gt_contacts.csv`: the number keeps the script
/// order in any file manager, the rest says which statement it is. ASCII
/// only, valid on every OS.
fn script_file_name(position: usize, width: usize, sql: &str, ext: &str) -> String {
    let stem = file_stem(&sql.to_ascii_lowercase(), 40);
    let stem = if stem.is_empty() { "result" } else { &stem };
    format!("{position:0width$}_{stem}.{ext}")
}

/// Letters, digits, `-` and `_` (ASCII) kept; every other run of characters
/// becomes one `_`. At most `max` characters, no `_` at the ends.
fn file_stem(s: &str, max: usize) -> String {
    let mut stem = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            stem.push(c);
        } else if !stem.ends_with('_') {
            stem.push('_');
        }
    }
    stem.truncate(max);
    stem.trim_matches('_').to_string()
}

/// A statement on one line: in a `--` comment a newline would end the
/// comment and turn the rest into SQL.
fn one_line(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Iterates the rows in windows, resolving truncated cells with `cell_full`.
/// Each cell is an `Option<String>`: `None` = NULL.
fn for_each_row(
    live: &LiveResult,
    rows: &std::ops::Range<u64>,
    cols: &std::ops::Range<usize>,
    mut f: impl FnMut(&[Option<String>]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut start = rows.start;
    while start < rows.end {
        let end = (start + WINDOW_ROWS).min(rows.end);
        let w = live.window(start..end, cols.clone());
        let n = (w.rows.end - w.rows.start) as usize;
        for r in 0..n {
            let mut cells: Vec<Option<String>> = Vec::with_capacity(w.columns.len());
            for (ci, c) in w.columns.iter().enumerate() {
                if c.nulls[r] {
                    cells.push(None);
                } else if c.truncated[r] {
                    // Full value from the arena (LOBs included): never write
                    // the truncated prefix of the window.
                    cells.push(live.cell_full(w.rows.start + r as u64, cols.start + ci));
                } else {
                    cells.push(Some(c.values[r].clone()));
                }
            }
            f(&cells)?;
        }
        if w.rows.end <= start {
            break; // empty window: end of data (shorter stream)
        }
        start = w.rows.end;
    }
    Ok(())
}

/// CSV/TSV quoting: quote only when needed (delimiter, quotes, newline),
/// or every field with `quote_all`; doubled quotes inside (RFC 4180).
fn write_delimited_row(
    out: &mut dyn Write,
    fields: &[&str],
    delim: char,
    quote_all: bool,
    nl: &str,
) -> std::io::Result<()> {
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            write!(out, "{delim}")?;
        }
        if quote_all || f.contains(delim) || f.contains('"') || f.contains('\n') || f.contains('\r')
        {
            write!(out, "\"{}\"", f.replace('"', "\"\""))?;
        } else {
            out.write_all(f.as_bytes())?;
        }
    }
    out.write_all(nl.as_bytes())
}

/// "schema.table" → both parts quoted; without a dot, the name alone.
fn quote_table(dialect: &dyn Dialect, name: &str) -> String {
    let name = if name.trim().is_empty() {
        "export"
    } else {
        name.trim()
    };
    match name.split_once('.') {
        Some((s, t)) => format!(
            "{}.{}",
            dialect.quote_identifier(s),
            dialect.quote_identifier(t)
        ),
        None => dialect.quote_identifier(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alamo_core::chunk::{ChunkBuilder, ChunkLimits};
    use alamo_core::column::ColumnKind;
    use alamo_core::meta::{ColumnMeta, NativeType};
    use alamo_core::result_set::ResultSetState;
    use std::sync::Arc;

    /// 3×2 result set with NULL and "awkward" values for the escaping.
    fn fixture() -> LiveResult {
        let pg = |oid| NativeType::Postgres {
            oid,
            type_modifier: -1,
        };
        let schema: Arc<[ColumnMeta]> = vec![
            ColumnMeta::new("id", pg(23), "int4"),
            ColumnMeta::new("note", pg(25), "text"),
        ]
        .into();
        let live = LiveResult::new(Arc::clone(&schema));
        let kinds = [ColumnKind::Utf8, ColumnKind::Utf8];
        let mut b = ChunkBuilder::new(&kinds, ChunkLimits::default());
        for (id, note) in [
            ("1", Some("hello, \"world\"")),
            ("2", None),
            ("3", Some("it's\tand tab")),
        ] {
            b.push_text(0, id).unwrap();
            match note {
                Some(s) => b.push_text(1, s).unwrap(),
                None => b.column_mut(1).unwrap().push_null(),
            }
            b.end_row();
        }
        let (chunk, stats) = b.finish();
        live.push_chunk(chunk, stats, 0);
        live.finish(ResultSetState::Complete, None);
        live
    }

    fn render_str(opts: &ExportOptions) -> String {
        let live = fixture();
        let dialect = alamo_driver_postgres::PgDialect::default();
        let mut out = Vec::new();
        let n = render(&live, &dialect, opts, 0..live.rows(), 0..2, &mut out).unwrap();
        assert_eq!(n, 3);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn csv_quotes_only_when_needed_and_null_is_empty() {
        let s = render_str(&ExportOptions {
            format: ExportFormat::Csv,
            headers: true,
            ..Default::default()
        });
        assert_eq!(
            s,
            "id,note\n1,\"hello, \"\"world\"\"\"\n2,\n3,it's\tand tab\n"
        );
    }

    #[test]
    fn csv_quote_all_quotes_every_field_and_the_headers() {
        let s = render_str(&ExportOptions {
            format: ExportFormat::Csv,
            headers: true,
            quote_all: true,
            ..Default::default()
        });
        assert_eq!(
            s,
            "\"id\",\"note\"\n\"1\",\"hello, \"\"world\"\"\"\n\"2\",\"\"\n\"3\",\"it's\tand tab\"\n"
        );
    }

    #[test]
    fn csv_semicolon_and_crlf_bom_only_for_files() {
        let opts = ExportOptions {
            format: ExportFormat::Csv,
            headers: true,
            delimiter: Some(';'),
            line_ending: LineEnding::Crlf,
            bom: true,
            ..Default::default()
        };
        let s = render_str(&opts);
        assert_eq!(
            s,
            "id;note\r\n1;\"hello, \"\"world\"\"\"\r\n2;\r\n3;it's\tand tab\r\n"
        );
        assert_eq!(opts.bom_bytes(), "\u{FEFF}".as_bytes());
        let json = ExportOptions {
            format: ExportFormat::Json,
            bom: true,
            ..Default::default()
        };
        assert!(json.bom_bytes().is_empty());
        let tsv = ExportOptions {
            format: ExportFormat::Tsv,
            delimiter: Some(';'),
            ..Default::default()
        };
        assert!(
            render_str(&tsv).starts_with("1\t\"hello"),
            "TSV keeps the tab"
        );
    }

    #[test]
    fn insert_batch_of_two_and_crlf() {
        let s = render_str(&ExportOptions {
            format: ExportFormat::Insert,
            table_name: "t".into(),
            insert_batch: 2,
            line_ending: LineEnding::Crlf,
            ..Default::default()
        });
        assert_eq!(s.matches("INSERT INTO \"t\"").count(), 2, "{s}");
        assert!(s.contains("('2', NULL);\r\nINSERT INTO"), "{s}");
        assert!(
            !s.contains("\n\n") && !s.contains("(\n"),
            "no bare LF: {s:?}"
        );
    }

    #[test]
    fn tsv_quotes_on_tab() {
        let s = render_str(&ExportOptions {
            format: ExportFormat::Tsv,
            ..Default::default()
        });
        assert!(s.contains("3\t\"it's\tand tab\""), "{s}");
    }

    #[test]
    fn json_string_values_and_null() {
        let s = render_str(&ExportOptions {
            format: ExportFormat::Json,
            ..Default::default()
        });
        assert!(
            s.starts_with("[\n  {\"id\": \"1\", \"note\": \"hello, \\\"world\\\"\"}"),
            "{s}"
        );
        assert!(s.contains("{\"id\": \"2\", \"note\": null}"), "{s}");
        assert!(s.trim_end().ends_with(']'), "{s}");
    }

    #[test]
    fn insert_quotes_identifiers_and_literals() {
        let s = render_str(&ExportOptions {
            format: ExportFormat::Insert,
            table_name: "public.t".into(),
            ..Default::default()
        });
        assert!(
            s.starts_with("INSERT INTO \"public\".\"t\" (\"id\", \"note\") VALUES"),
            "{s}"
        );
        assert!(s.contains("('1', 'hello, \"world\"')"), "{s}");
        assert!(s.contains("('2', NULL)"), "{s}");
        assert!(s.contains("('3', 'it''s\tand tab')"), "{s}");
        assert!(s.trim_end().ends_with(';'), "{s}");
    }

    #[test]
    fn formulas_neutralized_numbers_not() {
        for (v, expected) in [
            ("=HYPERLINK(\"http://x\")", "'=HYPERLINK(\"http://x\")"),
            ("+cmd|' /C calc'!A0", "'+cmd|' /C calc'!A0"),
            ("@SUM(A1)", "'@SUM(A1)"),
            ("-2+3", "'-2+3"),
            ("\t=1", "'\t=1"),
            ("-5", "-5"),
            ("+1.5e3", "+1.5e3"),
            ("text", "text"),
            ("", ""),
        ] {
            assert_eq!(neutralize_formula(v), expected, "{v:?}");
        }
    }

    fn script_str(format: ExportFormat) -> String {
        let items = [1, 3].map(|position| ScriptItem {
            result_id: position as u64,
            position,
            sql: format!("SELECT *\n  FROM t{position} -- x"),
            table_name: format!("t{position}"),
        });
        let results: Vec<_> = items.iter().map(|it| (it, fixture())).collect();
        let dialect = alamo_driver_postgres::PgDialect::default();
        let opts = ExportOptions {
            format,
            headers: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        let n = render_script(&results, &dialect, &opts, &mut out).unwrap();
        assert_eq!(n, 6);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn script_csv_one_section_per_statement() {
        let s = script_str(ExportFormat::Csv);
        assert!(
            s.starts_with("[1] SELECT * FROM t1 -- x\nid,note\n1,"),
            "{s}"
        );
        assert!(
            s.contains("3,it's\tand tab\n\n[3] SELECT * FROM t3 -- x\nid,note\n"),
            "{s}"
        );
    }

    #[test]
    fn script_insert_comment_on_one_line_and_own_table() {
        let s = script_str(ExportFormat::Insert);
        assert!(
            s.starts_with("-- [1] SELECT * FROM t1 -- x\nINSERT INTO \"t1\""),
            "{s}"
        );
        assert!(
            s.contains("\n\n-- [3] SELECT * FROM t3 -- x\nINSERT INTO \"t3\""),
            "{s}"
        );
    }

    #[test]
    fn script_json_is_valid_with_sql_and_rows() {
        let s = script_str(ExportFormat::Json);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1]["statement"], 3);
        assert_eq!(arr[1]["sql"], "SELECT * FROM t3 -- x");
        assert_eq!(arr[1]["rows"][1]["note"], serde_json::Value::Null);
    }

    #[test]
    fn script_file_names_sort_and_are_portable() {
        assert_eq!(
            script_file_name(1, 2, "SHOW INDEX FROM `gt_contacts` WHERE x = 'é'", "csv"),
            "01_show_index_from_gt_contacts_where_x.csv"
        );
        assert_eq!(script_file_name(12, 3, "  ;", "sql"), "012_result.sql");
    }

    #[test]
    fn export_dir_never_reuses_a_folder() {
        let parent = std::env::temp_dir().join(format!("alamo-export-test-{}", std::process::id()));
        std::fs::create_dir_all(&parent).unwrap();
        let a = create_export_dir(&parent, "Query 1 2026-10-28").unwrap();
        let b = create_export_dir(&parent, "Query 1 2026-10-28").unwrap();
        assert_eq!(a.file_name().unwrap(), "Query_1_2026-10-28");
        assert_eq!(b.file_name().unwrap(), "Query_1_2026-10-28-2");
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn export_csv_neutralizes_by_default() {
        let opts: ExportOptions = serde_json::from_str(r#"{"format":"csv"}"#).unwrap();
        assert!(opts.neutralize_formulas);
    }
}
