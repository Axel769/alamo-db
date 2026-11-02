//! Conservative statement classification — §9.7.3.
//!
//! Rule: **if the statement cannot be classified with certainty, on a prod
//! profile it counts as a write**. The known traps (`WITH … DELETE … RETURNING`
//! that starts with `WITH` and writes, opaque `CALL`, `EXPLAIN ANALYZE` that
//! really executes) end up in `Unknown`, which the gates treat as a write. No
//! attempt is made here to write an SQL parser: only leading keywords after
//! comments, plus a scan of the literals for the `WHERE`.
//!
//! Transaction control (`BEGIN`/`COMMIT`/`ROLLBACK`) on the other hand IS
//! classifiable from the keyword: no statement starts with `WITH` and turns
//! into a `BEGIN`. It serves the client-side tracking of the transaction state
//! on PG, where tokio-postgres does not expose the ReadyForQuery status.

/// Classification outcome, for the gates of §9.7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementClass {
    /// Certain read (SELECT, SHOW, EXPLAIN without ANALYZE, …).
    Read,
    /// Certain data write (INSERT/UPDATE/DELETE/…).
    Write,
    /// DDL / maintenance (CREATE/ALTER/DROP/TRUNCATE/VACUUM/…).
    Ddl,
    /// Transaction or session control (BEGIN/COMMIT/SET/USE/…).
    Control,
    /// Not classifiable with certainty (WITH, CALL, DO, …).
    Unknown,
}

impl StatementClass {
    /// For the purposes of the prod gates, unknown is a write (§9.7.3).
    pub fn counts_as_write(self) -> bool {
        matches!(
            self,
            StatementClass::Write | StatementClass::Ddl | StatementClass::Unknown
        )
    }
}

/// Full classification of a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatementInfo {
    pub class: StatementClass,
    /// Irreversible operation (§9.7 point 3): always confirm, even with the
    /// transaction gate already open — DDL on a dialect without transactional
    /// DDL, `TRUNCATE`/`DROP` everywhere (the spec lists them separately, even
    /// though on PG they could be rolled back), `UPDATE`/`DELETE` without WHERE.
    pub irreversible: bool,
    /// `UPDATE`/`DELETE` without WHERE: touches every row of the table.
    /// Confirmed on every profile, not only prod (as DataGrip and DBeaver do).
    pub unfiltered: bool,
}

/// Effect of a control statement on the transaction state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnControl {
    /// `BEGIN` / `START TRANSACTION`.
    Begin,
    /// `COMMIT` / `END` / `ROLLBACK` that CLOSE the transaction.
    /// `chain = true` (`AND CHAIN`) immediately reopens another one.
    End { chain: bool },
    /// `SAVEPOINT` / `RELEASE` / `ROLLBACK TO …`: the transaction stays open.
    Savepoint,
}

pub fn classify_statement(sql: &str, transactional_ddl: bool) -> StatementInfo {
    let words = leading_words(sql, 4);
    let first = words.first().map(String::as_str).unwrap_or("");
    let second = words.get(1).map(String::as_str).unwrap_or("");

    let class = match first {
        "select" | "show" | "table" | "values" | "fetch" | "describe" | "desc" => {
            StatementClass::Read
        }
        // EXPLAIN ANALYZE really executes the statement; the options in
        // parentheses may hide ANALYZE → unknown.
        "explain" => match second {
            "analyze" | "analyse" | "" => StatementClass::Unknown,
            s if s.starts_with('(') => StatementClass::Unknown,
            _ => StatementClass::Read,
        },
        "insert" | "update" | "delete" | "merge" | "replace" | "copy" | "load" | "import"
        | "refresh" => StatementClass::Write,
        "create" | "alter" | "drop" | "truncate" | "rename" | "grant" | "revoke" | "comment"
        | "reindex" | "cluster" | "vacuum" | "analyze" | "analyse" | "optimize" | "lock"
        | "unlock" => StatementClass::Ddl,
        "begin" | "start" | "commit" | "end" | "rollback" | "savepoint" | "release" | "set"
        | "use" | "reset" | "deallocate" | "discard" => StatementClass::Control,
        // PREPARE TRANSACTION (2PC) and PREPARE statement: both out of
        // scope → unknown, hence a write for the gates.
        _ => StatementClass::Unknown,
    };

    let unfiltered = matches!(first, "update" | "delete") && !has_top_level_keyword(sql, "where");
    let irreversible = match first {
        "truncate" | "drop" => true,
        "update" | "delete" => unfiltered,
        _ => class == StatementClass::Ddl && !transactional_ddl,
    };

    StatementInfo {
        class,
        irreversible,
        unfiltered,
    }
}

/// Effect on the transaction state, if the statement is txn control.
/// `None` = it is not transaction control (or it is `SET`/`USE`/…).
pub fn txn_control(sql: &str) -> Option<TxnControl> {
    let words = leading_words(sql, 4);
    let mut it = words.iter().map(String::as_str);
    let first = it.next()?;
    // WORK/TRANSACTION are syntactic noise: `COMMIT WORK AND CHAIN`,
    // `BEGIN TRANSACTION`, `ROLLBACK WORK TO SAVEPOINT x`.
    let rest: Vec<&str> = it.filter(|w| *w != "work" && *w != "transaction").collect();
    match first {
        "begin" | "start" => Some(TxnControl::Begin),
        "commit" | "end" => Some(TxnControl::End {
            chain: rest.first() == Some(&"and") && rest.get(1) == Some(&"chain"),
        }),
        "rollback" => {
            if rest.first() == Some(&"to") {
                Some(TxnControl::Savepoint)
            } else {
                Some(TxnControl::End {
                    chain: rest.first() == Some(&"and") && rest.get(1) == Some(&"chain"),
                })
            }
        }
        "abort" => Some(TxnControl::End { chain: false }),
        "savepoint" | "release" => Some(TxnControl::Savepoint),
        _ => None,
    }
}

/// First `n` words (lowercase) of the statement, skipping whitespace and
/// comments (`--`, MySQL's `#`, nested `/* */`). A non-word token (e.g. `(`)
/// is returned as a string of its own and stops the collection.
fn leading_words(sql: &str, n: usize) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while out.len() < n {
        i = skip_ws_and_comments(bytes, i);
        if i >= bytes.len() {
            break;
        }
        let b = bytes[i];
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
            {
                i += 1;
            }
            out.push(sql[start..i].to_ascii_lowercase());
        } else {
            // `(SELECT …)`, numbers, etc.: opaque token, stop here.
            out.push((b as char).to_string());
            break;
        }
    }
    out
}

fn skip_ws_and_comments(bytes: &[u8], mut i: usize) -> usize {
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if i < bytes.len() && bytes[i] == b'#' {
            // MySQL line comment (§6.5: the splitter does not split on it, but
            // here skipping it is enough to read the real keyword).
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let mut depth = 1u32;
            i += 2;
            while i < bytes.len() && depth > 0 {
                if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else {
            return i;
        }
    }
}

/// Would the control statement disable the server-side read-only net (§9.7
/// point 4)? The net is a session variable set once at connect time: a
/// `SET`/`RESET` on the variable or a transaction opened `READ WRITE` bypass
/// it — and the read-only gate must stop them, or the "double net" can still
/// be disarmed from user SQL.
///
/// Conservative like the rest of the module: keywords outside strings and
/// comments, false positives acceptable (e.g. `SET search_path = read,
/// write` — blocking a harmless SET on a read-only profile does no harm,
/// letting the disarm through does).
pub fn disarms_read_only(sql: &str) -> bool {
    let first = leading_words(sql, 1);
    let first = first.first().map(String::as_str).unwrap_or("");
    if !matches!(first, "set" | "reset" | "begin" | "start") {
        return false;
    }
    // Variable names are searched in the whole text: in double quotes or
    // backticks (`SET "default_transaction_read_only" = off`) they are still
    // the same variable for the server.
    let lower = sql.to_ascii_lowercase();
    if lower.contains("transaction_read_only") || lower.contains("tx_read_only") {
        return true;
    }
    let mut prev = String::new();
    let mut found = false;
    scan_top_level_words(sql, |w| {
        // The net's variables, per dialect: PG (default_…), MySQL
        // (transaction_…), MariaDB (tx_…). `RESET` brings them back to the
        // server's config default (off), so it disarms as much as SET does;
        // `RESET ALL` wipes them all. `READ WRITE` covers `BEGIN`/`START
        // TRANSACTION` and `SET [SESSION CHARACTERISTICS AS] TRANSACTION`.
        if (first == "reset" && w == "all") || (prev == "read" && w == "write") {
            found = true;
        }
        prev = w.to_owned();
        found
    });
    found
}

/// Would the statement open or close the current transaction? `BEGIN`,
/// `COMMIT`, `ROLLBACK` (not `TO SAVEPOINT`), `PREPARE TRANSACTION`, …
/// It serves read-only mode on PG, where every execution runs inside an
/// app-owned READ ONLY transaction: leaving it would reopen the way to
/// disarming with `set_config`.
pub fn begins_or_ends_txn(sql: &str) -> bool {
    if matches!(
        txn_control(sql),
        Some(TxnControl::Begin | TxnControl::End { .. })
    ) {
        return true;
    }
    let words = leading_words(sql, 2);
    words.first().map(String::as_str) == Some("prepare")
        && words.get(1).map(String::as_str) == Some("transaction")
}

/// MySQL dynamic SQL (`PREPARE`/`EXECUTE`): the text the server executes
/// never goes through the classifier.
pub fn is_dynamic_sql(sql: &str) -> bool {
    let words = leading_words(sql, 1);
    matches!(
        words.first().map(String::as_str),
        Some("prepare" | "execute")
    )
}

/// Can `EXPLAIN <sql>` be sent to the server for the estimate in the
/// confirmation dialogs (§9.7) **without executing anything**? The statement
/// must start with a DML/read keyword: then the server reads `EXPLAIN SELECT …`
/// and there is no room for options. Anything else — `(ANALYZE) DELETE …`,
/// `ANALYZE …`, an `EXPLAIN` already written — would become `EXPLAIN (ANALYZE)
/// …` / `EXPLAIN ANALYZE …`, which really executes the statement, and the
/// dialog runs BEFORE the user confirms.
///
/// `leading_words` is not enough here: the keyword must be the same one the
/// SERVER reads, and lexers diverge on comments. Ambiguous cases are rejected
/// rather than resolved: a `/* */` comment containing another one (PG nests
/// them, MySQL does not), executable comments / MySQL hints (`/*!`, `/*M!`,
/// `/*+`), `--` not followed by whitespace (on MySQL it is not a comment). A
/// false negative only costs the estimate; a false positive executes SQL.
pub fn explainable(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut i = 0usize;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let rest = &bytes[i..];
        if rest.starts_with(b"--") {
            if !rest.get(2).is_some_and(|b| b.is_ascii_whitespace()) {
                return false;
            }
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if rest.starts_with(b"#") {
            // MySQL comment; on PG `#` is an operator and `EXPLAIN # …` is a
            // syntax error: in no case does anything get executed.
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if rest.starts_with(b"/*") {
            let body = &rest[2..];
            if body.starts_with(b"!") || body.starts_with(b"M!") || body.starts_with(b"+") {
                return false;
            }
            let Some(end) = body.windows(2).position(|w| w == b"*/") else {
                return false;
            };
            if body[..end].windows(2).any(|w| w == b"/*") {
                return false;
            }
            i += 2 + end + 2;
        } else {
            break;
        }
    }
    let start = i;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    // The keyword must end there: `selectx` is not `select`.
    if i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'$')) {
        return false;
    }
    matches!(
        sql[start..i].to_ascii_lowercase().as_str(),
        "select"
            | "insert"
            | "update"
            | "delete"
            | "with"
            | "values"
            | "table"
            | "replace"
            | "merge"
    )
}

/// Does the keyword appear outside strings, quoted identifiers (double quotes
/// and backticks), dollar-quoting, comments **and parentheses**? Conservative
/// scanner for detecting `UPDATE`/`DELETE` without `WHERE`.
///
/// Parenthesis depth matters: a `WHERE` inside a subquery
/// (`DELETE FROM t RETURNING (SELECT 1 WHERE true)`) does not restrict the
/// DELETE, and counting it would make a statement "reversible" when it is not
/// — anti-conservative with respect to §9.7.
fn has_top_level_keyword(sql: &str, keyword: &str) -> bool {
    let mut found = false;
    scan_top_level_words(sql, |w| {
        if w.eq_ignore_ascii_case(keyword) {
            found = true;
        }
        found
    });
    found
}

/// Scans the words at parenthesis depth 0, outside strings, quoted
/// identifiers, dollar-quoting and comments, and hands them (already
/// lowercase) to `f`; `f` returning `true` stops the scan. Common base of
/// `has_top_level_keyword` and `disarms_read_only`.
fn scan_top_level_words(sql: &str, mut f: impl FnMut(&str) -> bool) {
    let bytes = sql.as_bytes();
    let mut i = 0usize;
    let mut depth = 0usize;
    while i < bytes.len() {
        i = skip_ws_and_comments(bytes, i);
        if i >= bytes.len() {
            break;
        }
        match bytes[i] {
            b'\'' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        // '' = doubled quote inside the string.
                        if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                            i += 2;
                        } else {
                            i += 1;
                            break;
                        }
                    } else if bytes[i] == b'\\' {
                        // Conservative: backslash escapes (MySQL default,
                        // PG E'' strings). On standard PG it skips one extra
                        // character inside the string: harmless.
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'"' | b'`' => {
                let q = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == q {
                        if i + 1 < bytes.len() && bytes[i + 1] == q {
                            i += 2;
                        } else {
                            i += 1;
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'$' => {
                // Dollar-quoting `$$` or `$tag$`; `$1` (placeholder) is not.
                if let Some(tag_len) = dollar_tag(bytes, i) {
                    let tag = &bytes[i..i + tag_len];
                    i += tag_len;
                    while i < bytes.len() {
                        if bytes[i] == b'$' && bytes[i..].starts_with(tag) {
                            i += tag_len;
                            break;
                        }
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                // saturating: an unmatched `)` (malformed SQL) must neither
                // underflow nor make the rest look top-level.
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b if b.is_ascii_alphabetic() || b == b'_' => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
                {
                    i += 1;
                }
                if depth == 0 && f(&sql[start..i].to_ascii_lowercase()) {
                    return;
                }
            }
            _ => i += 1,
        }
    }
}

/// Opening `$…$` starting at `i` (which points to `$`): length of the tag
/// including the two `$`, or `None` if it is not a tag (e.g. placeholder `$1`).
fn dollar_tag(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
        // `$1` placeholders have digits only: if we meet a digit as the first
        // character and then not a `$`, it is not a tag. The exact distinction
        // is made by the final check on the closing `$`.
        j += 1;
    }
    if j < bytes.len() && bytes[j] == b'$' {
        // `$1$` would in any case be a valid tag in PG only if non-numeric;
        // conservative: accepting it at most skips more text.
        Some(j - i + 1)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(sql: &str) -> StatementClass {
        classify_statement(sql, true).class
    }

    #[test]
    fn certain_reads() {
        assert_eq!(class("SELECT 1"), StatementClass::Read);
        assert_eq!(
            class("  -- comment\n  select * from t"),
            StatementClass::Read
        );
        assert_eq!(class("/* x /* y */ */ SHOW TABLES"), StatementClass::Read);
        assert_eq!(class("# mysql comment\nSELECT 1"), StatementClass::Read);
        assert_eq!(class("EXPLAIN SELECT * FROM t"), StatementClass::Read);
        assert_eq!(class("EXPLAIN UPDATE t SET a=1"), StatementClass::Read);
    }

    #[test]
    fn explicit_traps_from_the_spec() {
        // §9.7.3: WITH that writes, opaque CALL, EXPLAIN ANALYZE that executes.
        assert_eq!(
            class("WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x"),
            StatementClass::Unknown
        );
        assert_eq!(class("CALL empty_table()"), StatementClass::Unknown);
        assert_eq!(
            class("EXPLAIN ANALYZE DELETE FROM t"),
            StatementClass::Unknown
        );
        assert_eq!(
            class("EXPLAIN (ANALYZE) DELETE FROM t"),
            StatementClass::Unknown
        );
        assert!(StatementClass::Unknown.counts_as_write());
    }

    #[test]
    fn writes_and_ddl() {
        assert_eq!(class("INSERT INTO t VALUES (1)"), StatementClass::Write);
        assert_eq!(class("update t set a=1 where id=2"), StatementClass::Write);
        assert_eq!(class("CREATE TABLE t (id int)"), StatementClass::Ddl);
        assert_eq!(class("VACUUM t"), StatementClass::Ddl);
        assert_eq!(class("BEGIN"), StatementClass::Control);
        assert_eq!(class("SET search_path = x"), StatementClass::Control);
    }

    #[test]
    fn irreversible_statements() {
        // TRUNCATE/DROP always, even with transactional DDL (spec §9.7).
        assert!(classify_statement("TRUNCATE t", true).irreversible);
        assert!(classify_statement("DROP TABLE t", true).irreversible);
        // Generic DDL: depends on the dialect.
        assert!(!classify_statement("ALTER TABLE t ADD c int", true).irreversible);
        assert!(classify_statement("ALTER TABLE t ADD c int", false).irreversible);
        // UPDATE/DELETE without WHERE.
        assert!(classify_statement("DELETE FROM t", true).irreversible);
        assert!(classify_statement("DELETE FROM t", true).unfiltered);
        assert!(!classify_statement("DELETE FROM t WHERE id = 1", true).unfiltered);
        assert!(!classify_statement("DROP TABLE t", true).unfiltered);
        assert!(!classify_statement("DELETE FROM t WHERE id = 1", true).irreversible);
        // A WHERE inside a string does not count.
        assert!(classify_statement("UPDATE t SET note = 'no where'", true).irreversible);
        // ...nor inside dollar-quoting.
        assert!(classify_statement("UPDATE t SET f = $$ where $$", true).irreversible);
        // ...nor inside a quoted identifier.
        assert!(classify_statement("UPDATE t SET \"where\" = 1", true).irreversible);
        assert!(classify_statement("UPDATE t SET `where` = 1", false).irreversible);
        // ...nor inside a parenthesized subquery: the WHERE does not restrict
        // the outer DELETE (§9.7, the opposite would be anti-conservative).
        assert!(
            classify_statement("DELETE FROM t RETURNING (SELECT 1 WHERE true)", true).irreversible
        );
        assert!(
            classify_statement("UPDATE t SET a = (SELECT b FROM u WHERE u.id = 1)", true)
                .irreversible
        );
        // A WHERE at depth 0 is still recognized even with subqueries.
        assert!(
            !classify_statement("DELETE FROM t WHERE id IN (SELECT id FROM u)", true).irreversible
        );
    }

    #[test]
    fn read_only_disarming() {
        // SET/RESET on the net's variables, per dialect.
        assert!(disarms_read_only("SET default_transaction_read_only = off"));
        assert!(disarms_read_only("SET SESSION transaction_read_only = 0"));
        assert!(disarms_read_only("set @@session.transaction_read_only = 0"));
        assert!(disarms_read_only("SET tx_read_only = 0"));
        assert!(disarms_read_only("RESET default_transaction_read_only"));
        assert!(disarms_read_only("RESET ALL"));
        // Transaction explicitly opened READ WRITE.
        assert!(disarms_read_only("BEGIN READ WRITE"));
        assert!(disarms_read_only("START TRANSACTION READ WRITE"));
        assert!(disarms_read_only("SET TRANSACTION READ WRITE"));
        assert!(disarms_read_only(
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE"
        ));
        // Harmless control: passes.
        assert!(!disarms_read_only("BEGIN"));
        assert!(!disarms_read_only("START TRANSACTION READ ONLY"));
        assert!(!disarms_read_only("SET search_path = public"));
        assert!(!disarms_read_only("SET statement_timeout = '5s'"));
        assert!(!disarms_read_only("COMMIT"));
        // Non-control: never, even if it contains the words.
        assert!(!disarms_read_only("SELECT 'SET transaction_read_only = 0'"));
        // Keywords inside a string do not count.
        assert!(!disarms_read_only("SET application_name = 'read write'"));
        // The variable name in double quotes or backticks is the same
        // variable.
        assert!(disarms_read_only(
            "SET \"default_transaction_read_only\" = off"
        ));
        assert!(disarms_read_only("SET SESSION `transaction_read_only` = 0"));
    }

    #[test]
    fn opens_or_closes_the_transaction() {
        for s in [
            "BEGIN",
            "START TRANSACTION",
            "COMMIT",
            "END",
            "ABORT",
            "ROLLBACK",
            "COMMIT AND CHAIN",
            "PREPARE TRANSACTION 'x'",
            "COMMIT PREPARED 'x'",
        ] {
            assert!(begins_or_ends_txn(s), "{s}");
        }
        for s in [
            "SAVEPOINT a",
            "ROLLBACK TO SAVEPOINT a",
            "RELEASE a",
            "PREPARE q AS SELECT 1",
            "SELECT 1",
        ] {
            assert!(!begins_or_ends_txn(s), "{s}");
        }
    }

    #[test]
    fn dynamic_sql() {
        assert!(is_dynamic_sql("PREPARE s FROM 'SET x = 1'"));
        assert!(is_dynamic_sql("execute s"));
        assert!(!is_dynamic_sql("DEALLOCATE PREPARE s"));
        assert!(!is_dynamic_sql("SELECT 1"));
    }

    #[test]
    fn transaction_control() {
        assert_eq!(txn_control("BEGIN"), Some(TxnControl::Begin));
        assert_eq!(txn_control("begin work"), Some(TxnControl::Begin));
        assert_eq!(
            txn_control("START TRANSACTION READ ONLY"),
            Some(TxnControl::Begin)
        );
        assert_eq!(
            txn_control("COMMIT"),
            Some(TxnControl::End { chain: false })
        );
        assert_eq!(txn_control("END"), Some(TxnControl::End { chain: false }));
        assert_eq!(
            txn_control("COMMIT WORK AND CHAIN"),
            Some(TxnControl::End { chain: true })
        );
        assert_eq!(
            txn_control("ROLLBACK"),
            Some(TxnControl::End { chain: false })
        );
        assert_eq!(
            txn_control("ROLLBACK AND CHAIN"),
            Some(TxnControl::End { chain: true })
        );
        assert_eq!(
            txn_control("ROLLBACK TO SAVEPOINT x"),
            Some(TxnControl::Savepoint)
        );
        assert_eq!(
            txn_control("ROLLBACK WORK TO x"),
            Some(TxnControl::Savepoint)
        );
        assert_eq!(txn_control("SAVEPOINT x"), Some(TxnControl::Savepoint));
        assert_eq!(txn_control("ABORT"), Some(TxnControl::End { chain: false }));
        // A BEGIN inside a DO block is not transaction control.
        assert_eq!(txn_control("DO $$ BEGIN NULL; END $$"), None);
        assert_eq!(txn_control("SELECT 1"), None);
        assert_eq!(txn_control("SET autocommit = 0"), None);
    }

    #[test]
    fn explainable_only_dml_and_reads() {
        for sql in [
            "SELECT 1",
            "  -- estimate\n  update t set a = 1 where id = 2",
            "/* comment */ DELETE FROM t WHERE id = 1",
            "# mysql\nINSERT INTO t VALUES (1)",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "VALUES (1)",
            "TABLE t",
            "REPLACE INTO t VALUES (1)",
            "--\nSELECT 1",
        ] {
            assert!(explainable(sql), "expected explainable: {sql:?}");
        }
    }

    #[test]
    fn explainable_rejects_what_would_become_explain_analyze() {
        for sql in [
            // PG: `EXPLAIN (ANALYZE) DELETE …` executes the DELETE.
            "(ANALYZE) DELETE FROM t",
            "ANALYZE DELETE FROM t",
            "EXPLAIN ANALYZE SELECT 1",
            // Nested comments: PG sees `(ANALYZE) DELETE`, a non-nesting lexer
            // would see `SELECT`.
            "/* /* */ SELECT */ (ANALYZE) DELETE FROM t",
            // MySQL: the executable comment becomes `EXPLAIN ANALYZE DELETE`.
            "/*!ANALYZE */ DELETE FROM t",
            "/*M!ANALYZE */ DELETE FROM t",
            // MySQL: `--1` is not a comment.
            "--1\nSELECT 1",
            "/* never closed SELECT 1",
            "selectx FROM t",
            "CREATE TABLE t (id INT)",
            "CALL p()",
            "DO $$ BEGIN NULL; END $$",
            "",
        ] {
            assert!(!explainable(sql), "expected NOT explainable: {sql:?}");
        }
    }
}
