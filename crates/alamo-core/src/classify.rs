//! Conservative statement classification — §9.7.3.
//!
//! Rule: **if the statement cannot be classified with certainty, on a prod
//! profile it counts as a write**. The known traps (opaque `CALL`, `EXPLAIN
//! ANALYZE` that really executes) end up in `Unknown`, which the gates treat
//! as a write. No attempt is made here to write an SQL parser: leading
//! keywords, the `WITH` list (each CTE body and the statement after it) and a
//! scan of the top-level words for the `WHERE`, all on the dialect's own
//! tokens (`lexer`) — reading the text with the wrong rules can turn a write
//! into a read.
//!
//! Transaction control (`BEGIN`/`COMMIT`/`ROLLBACK`) on the other hand IS
//! classifiable from the keyword: no statement starts with `WITH` and turns
//! into a `BEGIN`. It serves the client-side tracking of the transaction state
//! on PG, where tokio-postgres does not expose the ReadyForQuery status.

use crate::dialect::Capabilities;
use crate::lexer::{tokens, Token, TokenKind};

/// What classification needs from the dialect (§2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    /// DDL can be rolled back (PG): otherwise every DDL is irreversible.
    pub transactional_ddl: bool,
    /// MySQL lexical rules (`lexer::tokens`).
    pub backslash_escapes: bool,
}

impl From<&Capabilities> for Rules {
    fn from(caps: &Capabilities) -> Self {
        Rules {
            transactional_ddl: caps.transactional_ddl,
            backslash_escapes: caps.backslash_escapes,
        }
    }
}

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

const UNKNOWN: StatementInfo = StatementInfo {
    class: StatementClass::Unknown,
    irreversible: false,
    unfiltered: false,
};

/// Nesting (CTE bodies, parentheses) beyond which the statement is not
/// followed further: `Unknown`, rather than a stack overflow on generated SQL.
const MAX_DEPTH: u32 = 32;

pub fn classify_statement(sql: &str, rules: Rules) -> StatementInfo {
    let toks: Vec<Token> = tokens(sql, rules.backslash_escapes).collect();
    classify_tokens(sql, &toks, rules, 0)
}

fn classify_tokens(sql: &str, toks: &[Token], rules: Rules, depth: u32) -> StatementInfo {
    if depth > MAX_DEPTH {
        return UNKNOWN;
    }
    if toks.first().is_some_and(|t| t.is_symbol(sql, b'(')) {
        return classify_parenthesized(sql, toks, rules, depth);
    }
    let words = leading_words(sql, toks, 2);
    let first = words.first().map(String::as_str).unwrap_or("");
    let second = words.get(1).map(String::as_str).unwrap_or("");

    let class = match first {
        "with" => return classify_with(sql, toks, rules, depth).unwrap_or(UNKNOWN),
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

    let unfiltered =
        matches!(first, "update" | "delete") && !has_top_level_keyword(sql, toks, "where");
    let irreversible = match first {
        "truncate" | "drop" => true,
        "update" | "delete" => unfiltered,
        _ => class == StatementClass::Ddl && !rules.transactional_ddl,
    };

    StatementInfo {
        class,
        irreversible,
        unfiltered,
    }
}

/// `(SELECT …) UNION (SELECT …)`: a query expression, never DML — neither
/// dialect accepts `(DELETE …)`, and on PG a data-modifying `WITH` must be
/// at the top level. A read if the first group is one, otherwise unknown.
fn classify_parenthesized(sql: &str, toks: &[Token], rules: Rules, depth: u32) -> StatementInfo {
    match closing_paren(sql, toks, 0) {
        Some(close)
            if classify_tokens(sql, &toks[1..close], rules, depth + 1).class
                == StatementClass::Read =>
        {
            StatementInfo {
                class: StatementClass::Read,
                irreversible: false,
                unfiltered: false,
            }
        }
        _ => UNKNOWN,
    }
}

/// `WITH [RECURSIVE] name [(cols)] AS [[NOT] MATERIALIZED] (body)
/// [SEARCH … SET col] [CYCLE … USING col], … statement` (§9.7.3).
///
/// A read only if every CTE body and the final statement are reads: on PG a
/// body can be `INSERT`/`UPDATE`/`DELETE`/`MERGE … RETURNING` and it writes;
/// MySQL 8 accepts `WITH … UPDATE`/`DELETE`. A DML body or statement makes
/// it a write, with the `WHERE` checked on each of them. Anything else, or a
/// shape not recognised here, is `None` (unknown).
fn classify_with(sql: &str, toks: &[Token], rules: Rules, depth: u32) -> Option<StatementInfo> {
    // A MySQL executable comment is read by the server only:
    // `WITH a AS (SELECT 1) /*! DELETE FROM t */` deletes every row.
    if toks.iter().any(|t| t.kind == TokenKind::Code) {
        return None;
    }
    let is_word = |i: usize, kw: &str| toks.get(i).is_some_and(|t| t.is_word(sql, kw));
    let is_symbol = |i: usize, s: u8| toks.get(i).is_some_and(|t| t.is_symbol(sql, s));
    let is_name = |i: usize| {
        toks.get(i)
            .is_some_and(|t| matches!(t.kind, TokenKind::Word | TokenKind::Quoted))
    };

    let mut parts = Vec::new();
    let mut i = 1;
    if is_word(i, "recursive") {
        i += 1;
    }
    loop {
        if !is_name(i) {
            return None;
        }
        i += 1;
        if is_symbol(i, b'(') {
            // Column list: names and commas only.
            let close = closing_paren(sql, toks, i)?;
            if !(i + 1..close).all(|j| is_name(j) || is_symbol(j, b',')) {
                return None;
            }
            i = close + 1;
        }
        if !is_word(i, "as") {
            return None;
        }
        i += 1;
        if is_word(i, "not") {
            i += 1;
        }
        if is_word(i, "materialized") {
            i += 1;
        }
        if !is_symbol(i, b'(') {
            return None;
        }
        let close = closing_paren(sql, toks, i)?;
        parts.push(classify_tokens(sql, &toks[i + 1..close], rules, depth + 1));
        i = close + 1;
        // PG 14: `SEARCH … FIRST BY cols SET col`, `CYCLE cols SET col
        // [TO v DEFAULT v] USING col`. Names, commas and literals only.
        while let Some(last) = ["search", "cycle"]
            .iter()
            .position(|kw| is_word(i, kw))
            .map(|p| ["set", "using"][p])
        {
            while !is_word(i, last) {
                if toks.get(i)?.kind == TokenKind::Code || is_symbol(i, b'(') {
                    return None;
                }
                i += 1;
            }
            if !is_name(i + 1) {
                return None;
            }
            i += 2;
        }
        if !is_symbol(i, b',') {
            break;
        }
        i += 1;
    }
    if i >= toks.len() {
        return None;
    }
    parts.push(classify_tokens(sql, &toks[i..], rules, depth + 1));

    let mut class = StatementClass::Read;
    for part in &parts {
        match part.class {
            StatementClass::Read => {}
            StatementClass::Write => class = StatementClass::Write,
            _ => return None,
        }
    }
    let unfiltered = parts.iter().any(|p| p.unfiltered);
    Some(StatementInfo {
        class,
        irreversible: unfiltered,
        unfiltered,
    })
}

/// Index of the `)` closing the `(` at `open`.
fn closing_paren(sql: &str, toks: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (j, t) in toks.iter().enumerate().skip(open) {
        if t.is_symbol(sql, b'(') {
            depth += 1;
        } else if t.is_symbol(sql, b')') {
            depth -= 1;
            if depth == 0 {
                return Some(j);
            }
        }
    }
    None
}

/// Effect on the transaction state, if the statement is txn control.
/// `None` = it is not transaction control (or it is `SET`/`USE`/…).
pub fn txn_control(sql: &str, rules: Rules) -> Option<TxnControl> {
    let words = first_words(sql, rules, 4);
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

fn first_words(sql: &str, rules: Rules, n: usize) -> Vec<String> {
    let toks: Vec<Token> = tokens(sql, rules.backslash_escapes).take(n).collect();
    leading_words(sql, &toks, n)
}

/// First `n` words (lowercase). A token that is not a word (e.g. `(`) is
/// returned as its first character and stops the collection.
fn leading_words(sql: &str, toks: &[Token], n: usize) -> Vec<String> {
    let mut out = Vec::new();
    for t in toks.iter().take(n) {
        let text = t.text(sql);
        if t.kind == TokenKind::Word {
            out.push(text.to_ascii_lowercase());
        } else {
            out.extend(text.chars().next().map(String::from));
            break;
        }
    }
    out
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
pub fn disarms_read_only(sql: &str, rules: Rules) -> bool {
    let toks: Vec<Token> = tokens(sql, rules.backslash_escapes).collect();
    let first = leading_words(sql, &toks, 1);
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
    // MySQL executable comment: `START TRANSACTION /*! READ WRITE */` is
    // read by the server and not here.
    if toks.iter().any(|t| t.kind == TokenKind::Code) {
        return true;
    }
    // The net's variables, per dialect: PG (default_…), MySQL
    // (transaction_…), MariaDB (tx_…). `RESET` brings them back to the
    // server's config default (off), so it disarms as much as SET does;
    // `RESET ALL` wipes them all. `READ WRITE` covers `BEGIN`/`START
    // TRANSACTION` and `SET [SESSION CHARACTERISTICS AS] TRANSACTION`.
    let words = top_level_words(sql, &toks);
    words
        .iter()
        .zip(words.iter().skip(1))
        .any(|(prev, w)| (first == "reset" && *w == "all") || (*prev == "read" && *w == "write"))
}

/// Would the statement open or close the current transaction? `BEGIN`,
/// `COMMIT`, `ROLLBACK` (not `TO SAVEPOINT`), `PREPARE TRANSACTION`, …
/// It serves read-only mode on PG, where every execution runs inside an
/// app-owned READ ONLY transaction: leaving it would reopen the way to
/// disarming with `set_config`.
pub fn begins_or_ends_txn(sql: &str, rules: Rules) -> bool {
    if matches!(
        txn_control(sql, rules),
        Some(TxnControl::Begin | TxnControl::End { .. })
    ) {
        return true;
    }
    let words = first_words(sql, rules, 2);
    words.first().map(String::as_str) == Some("prepare")
        && words.get(1).map(String::as_str) == Some("transaction")
}

/// MySQL dynamic SQL (`PREPARE`/`EXECUTE`): the text the server executes
/// never goes through the classifier.
pub fn is_dynamic_sql(sql: &str, rules: Rules) -> bool {
    let words = first_words(sql, rules, 1);
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

/// Does the keyword appear at parenthesis depth 0 of the tokens? Used for
/// detecting `UPDATE`/`DELETE` without `WHERE`.
///
/// Parenthesis depth matters: a `WHERE` inside a subquery
/// (`DELETE FROM t RETURNING (SELECT 1 WHERE true)`) does not restrict the
/// DELETE, and counting it would make a statement "reversible" when it is not
/// — anti-conservative with respect to §9.7.
fn has_top_level_keyword(sql: &str, toks: &[Token], keyword: &str) -> bool {
    top_level_words(sql, toks).iter().any(|w| *w == keyword)
}

/// The words at parenthesis depth 0, lowercase. Strings, quoted identifiers
/// and comments are not words (`lexer`).
fn top_level_words(sql: &str, toks: &[Token]) -> Vec<String> {
    let mut depth = 0usize;
    let mut out = Vec::new();
    for t in toks {
        if t.is_symbol(sql, b'(') {
            depth += 1;
        } else if t.is_symbol(sql, b')') {
            // saturating: an unmatched `)` (malformed SQL) must neither
            // underflow nor make the rest look top-level.
            depth = depth.saturating_sub(1);
        } else if depth == 0 && t.kind == TokenKind::Word {
            out.push(t.text(sql).to_ascii_lowercase());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG: Rules = Rules {
        transactional_ddl: true,
        backslash_escapes: false,
    };
    const MY: Rules = Rules {
        transactional_ddl: false,
        backslash_escapes: true,
    };

    fn class(sql: &str) -> StatementClass {
        classify_statement(sql, PG).class
    }

    #[test]
    fn certain_reads() {
        assert_eq!(class("SELECT 1"), StatementClass::Read);
        assert_eq!(
            class("  -- comment\n  select * from t"),
            StatementClass::Read
        );
        assert_eq!(class("/* x /* y */ */ SHOW TABLES"), StatementClass::Read);
        assert_eq!(
            classify_statement("# mysql comment\nSELECT 1", MY).class,
            StatementClass::Read
        );
        assert_eq!(class("EXPLAIN SELECT * FROM t"), StatementClass::Read);
        assert_eq!(class("EXPLAIN UPDATE t SET a=1"), StatementClass::Read);
    }

    #[test]
    fn explicit_traps_from_the_spec() {
        // §9.7.3: WITH that writes, opaque CALL, EXPLAIN ANALYZE that executes.
        assert_eq!(
            class("WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x"),
            StatementClass::Write
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
        assert!(classify_statement("TRUNCATE t", PG).irreversible);
        assert!(classify_statement("DROP TABLE t", PG).irreversible);
        // Generic DDL: depends on the dialect.
        assert!(!classify_statement("ALTER TABLE t ADD c int", PG).irreversible);
        assert!(classify_statement("ALTER TABLE t ADD c int", MY).irreversible);
        // UPDATE/DELETE without WHERE.
        assert!(classify_statement("DELETE FROM t", PG).irreversible);
        assert!(classify_statement("DELETE FROM t", PG).unfiltered);
        assert!(!classify_statement("DELETE FROM t WHERE id = 1", PG).unfiltered);
        assert!(!classify_statement("DROP TABLE t", PG).unfiltered);
        assert!(!classify_statement("DELETE FROM t WHERE id = 1", PG).irreversible);
        // A WHERE inside a string does not count.
        assert!(classify_statement("UPDATE t SET note = 'no where'", PG).irreversible);
        // ...nor inside dollar-quoting.
        assert!(classify_statement("UPDATE t SET f = $$ where $$", PG).irreversible);
        // ...nor inside a quoted identifier.
        assert!(classify_statement("UPDATE t SET \"where\" = 1", PG).irreversible);
        assert!(classify_statement("UPDATE t SET `where` = 1", MY).irreversible);
        // ...nor inside a parenthesized subquery: the WHERE does not restrict
        // the outer DELETE (§9.7, the opposite would be anti-conservative).
        assert!(
            classify_statement("DELETE FROM t RETURNING (SELECT 1 WHERE true)", PG).irreversible
        );
        assert!(
            classify_statement("UPDATE t SET a = (SELECT b FROM u WHERE u.id = 1)", PG)
                .irreversible
        );
        // A WHERE at depth 0 is still recognized even with subqueries.
        assert!(
            !classify_statement("DELETE FROM t WHERE id IN (SELECT id FROM u)", PG).irreversible
        );
    }

    #[test]
    fn read_only_disarming() {
        // SET/RESET on the net's variables, per dialect.
        assert!(disarms_read_only(
            "SET default_transaction_read_only = off",
            MY
        ));
        assert!(disarms_read_only(
            "SET SESSION transaction_read_only = 0",
            MY
        ));
        assert!(disarms_read_only(
            "set @@session.transaction_read_only = 0",
            MY
        ));
        assert!(disarms_read_only("SET tx_read_only = 0", MY));
        assert!(disarms_read_only("RESET default_transaction_read_only", MY));
        assert!(disarms_read_only("RESET ALL", MY));
        // Transaction explicitly opened READ WRITE.
        assert!(disarms_read_only("BEGIN READ WRITE", MY));
        assert!(disarms_read_only("START TRANSACTION READ WRITE", MY));
        assert!(disarms_read_only("SET TRANSACTION READ WRITE", MY));
        assert!(disarms_read_only(
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
            MY
        ));
        // Harmless control: passes.
        assert!(!disarms_read_only("BEGIN", MY));
        assert!(!disarms_read_only("START TRANSACTION READ ONLY", MY));
        assert!(!disarms_read_only("SET search_path = public", MY));
        assert!(!disarms_read_only("SET statement_timeout = '5s'", MY));
        assert!(!disarms_read_only("COMMIT", MY));
        // Non-control: never, even if it contains the words.
        assert!(!disarms_read_only(
            "SELECT 'SET transaction_read_only = 0'",
            MY
        ));
        // Keywords inside a string do not count.
        assert!(!disarms_read_only(
            "SET application_name = 'read write'",
            MY
        ));
        // The variable name in double quotes or backticks is the same
        // variable.
        assert!(disarms_read_only(
            "SET \"default_transaction_read_only\" = off",
            PG
        ));
        assert!(disarms_read_only(
            "SET SESSION `transaction_read_only` = 0",
            MY
        ));
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
            assert!(begins_or_ends_txn(s, PG), "{s}");
        }
        for s in [
            "SAVEPOINT a",
            "ROLLBACK TO SAVEPOINT a",
            "RELEASE a",
            "PREPARE q AS SELECT 1",
            "SELECT 1",
        ] {
            assert!(!begins_or_ends_txn(s, PG), "{s}");
        }
    }

    #[test]
    fn dynamic_sql() {
        assert!(is_dynamic_sql("PREPARE s FROM 'SET x = 1'", MY));
        assert!(is_dynamic_sql("execute s", MY));
        assert!(!is_dynamic_sql("DEALLOCATE PREPARE s", MY));
        assert!(!is_dynamic_sql("SELECT 1", MY));
    }

    #[test]
    fn transaction_control() {
        assert_eq!(txn_control("BEGIN", PG), Some(TxnControl::Begin));
        assert_eq!(txn_control("begin work", PG), Some(TxnControl::Begin));
        assert_eq!(
            txn_control("START TRANSACTION READ ONLY", PG),
            Some(TxnControl::Begin)
        );
        assert_eq!(
            txn_control("COMMIT", PG),
            Some(TxnControl::End { chain: false })
        );
        assert_eq!(
            txn_control("END", PG),
            Some(TxnControl::End { chain: false })
        );
        assert_eq!(
            txn_control("COMMIT WORK AND CHAIN", PG),
            Some(TxnControl::End { chain: true })
        );
        assert_eq!(
            txn_control("ROLLBACK", PG),
            Some(TxnControl::End { chain: false })
        );
        assert_eq!(
            txn_control("ROLLBACK AND CHAIN", PG),
            Some(TxnControl::End { chain: true })
        );
        assert_eq!(
            txn_control("ROLLBACK TO SAVEPOINT x", PG),
            Some(TxnControl::Savepoint)
        );
        assert_eq!(
            txn_control("ROLLBACK WORK TO x", PG),
            Some(TxnControl::Savepoint)
        );
        assert_eq!(txn_control("SAVEPOINT x", PG), Some(TxnControl::Savepoint));
        assert_eq!(
            txn_control("ABORT", PG),
            Some(TxnControl::End { chain: false })
        );
        // A BEGIN inside a DO block is not transaction control.
        assert_eq!(txn_control("DO $$ BEGIN NULL; END $$", PG), None);
        assert_eq!(txn_control("SELECT 1", PG), None);
        assert_eq!(txn_control("SET autocommit = 0", PG), None);
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

    fn info(sql: &str, rules: Rules) -> StatementInfo {
        classify_statement(sql, rules)
    }

    #[test]
    fn with_reads_are_reads() {
        // The shape reported on prod: several CTEs, window, LIKE with `\_`.
        let report = r"WITH dev AS (
  SELECT DISTINCT deviceid FROM sync_user_device
  WHERE applicationversion LIKE '6.1.0\_3%' AND applicationversion LIKE '%Android%'
),
p AS (
  SELECT g.millis, LAG(g.millis) OVER w AS prev_ms
  FROM gpstracker g JOIN dev d ON d.deviceid = g.sync_deviceid
  WINDOW w AS (PARTITION BY g.sync_deviceid ORDER BY g.millis)
),
q AS (SELECT * FROM p)
SELECT * FROM q GROUP BY 1 ORDER BY 1";
        for rules in [PG, MY] {
            let i = info(report, rules);
            assert_eq!(i.class, StatementClass::Read);
            assert!(!i.irreversible && !i.unfiltered);
        }
        // Server-verified shapes (PG 18 / MySQL 8.0 / MariaDB 11.8).
        for sql in [
            "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r WHERE n<3) SELECT * FROM r",
            "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r WHERE n<3) \
             SEARCH DEPTH FIRST BY n SET ord SELECT * FROM r",
            "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r WHERE n<3) \
             CYCLE n SET is_cycle USING path SELECT n FROM r",
            "WITH a AS MATERIALIZED (SELECT 1), b AS NOT MATERIALIZED (VALUES (1)), c AS (TABLE t) SELECT 1",
            "WITH a AS ((SELECT 1) UNION (SELECT 2)) SELECT * FROM a",
            "WITH a AS (SELECT 1) (SELECT * FROM a)",
            "WITH a AS (SELECT 1) VALUES (1)",
            "WITH a AS (SELECT 1) TABLE a",
            "WITH a AS (WITH b AS (SELECT 1) SELECT * FROM b) SELECT * FROM a",
            "WITH \"odd name\"(x, \"y\") AS (SELECT 1, 2) SELECT * FROM \"odd name\"",
            // Keywords inside strings and comments are not statements.
            "WITH a AS (SELECT 'DELETE FROM t') /* UPDATE t */ SELECT * FROM a",
        ] {
            assert_eq!(info(sql, PG).class, StatementClass::Read, "{sql}");
        }
        assert_eq!(
            info("WITH `a` AS (SELECT 1) SELECT * FROM `a`", MY).class,
            StatementClass::Read
        );
        assert_eq!(class("(SELECT 1) UNION (SELECT 2)"), StatementClass::Read);
    }

    #[test]
    fn with_that_writes_is_a_write() {
        // PG: data-modifying CTEs write, the WHERE is checked in each body.
        for (sql, unfiltered) in [
            ("WITH d AS (DELETE FROM t WHERE id=1 RETURNING *) SELECT * FROM d", false),
            ("WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d", true),
            ("WITH d AS (INSERT INTO t VALUES (9,9) RETURNING *) SELECT 1", false),
            ("WITH d AS (UPDATE t SET v=0 RETURNING *) SELECT 1", true),
            (
                "WITH d AS (MERGE INTO t USING s ON t.id=s.id WHEN MATCHED THEN DELETE RETURNING t.*) SELECT 1",
                false,
            ),
            ("WITH a AS (SELECT 1) INSERT INTO t SELECT 5,5", false),
            ("WITH a AS (SELECT 1) DELETE FROM t", true),
        ] {
            let i = info(sql, PG);
            assert_eq!(i.class, StatementClass::Write, "{sql}");
            assert_eq!(i.unfiltered, unfiltered, "{sql}");
            assert_eq!(i.irreversible, unfiltered, "{sql}");
        }
        // MySQL 8: `WITH … UPDATE`/`DELETE`, also multi-table.
        for sql in [
            "WITH a AS (SELECT 1) DELETE FROM t",
            "WITH a AS (SELECT 1) UPDATE t SET v=0",
        ] {
            let i = info(sql, MY);
            assert_eq!(i.class, StatementClass::Write, "{sql}");
            assert!(i.unfiltered && i.irreversible, "{sql}");
        }
        let i = info(
            "WITH a AS (SELECT 1 AS id) DELETE t FROM t JOIN a ON t.id=a.id WHERE t.v > 0",
            MY,
        );
        assert_eq!(i.class, StatementClass::Write);
        assert!(!i.unfiltered);
    }

    #[test]
    fn with_not_recognised_is_unknown() {
        for sql in [
            "WITH",
            "WITH a AS (SELECT 1)",
            "WITH a AS (SELECT 1",
            "WITH a (SELECT 1) SELECT 1",
            "WITH a AS SELECT 1",
            "WITH a(x + 1) AS (SELECT 1) SELECT 1",
            "WITH a AS (CALL p()) SELECT 1",
            "WITH a AS (SELECT 1) CALL p()",
            "WITH a AS (SELECT 1) CREATE TABLE x (id int)",
            "WITH a AS (SELECT 1) SET x = 1",
            "WITH a AS (SELECT 1) ; DELETE FROM t",
            "WITH RECURSIVE r AS (SELECT 1) SEARCH DEPTH FIRST BY n SET SELECT 1",
            "WITH RECURSIVE r AS (SELECT 1) CYCLE n SET c TO (DELETE) USING p SELECT 1",
            "(DELETE FROM t)",
            "(SELECT 1",
        ] {
            assert_eq!(info(sql, PG).class, StatementClass::Unknown, "{sql}");
        }
        // Generated SQL nested beyond any real query: not followed.
        let deep = format!("{}SELECT 1{}", "(".repeat(10_000), ")".repeat(10_000));
        assert_eq!(class(&deep), StatementClass::Unknown);
    }

    #[test]
    fn with_is_read_with_the_server_lexical_rules() {
        // Each of these deleted every row on the real server (§9.7.3); a
        // lexer with the other dialect's rules sees only reads.
        // PG: `#` is an operator, not a comment.
        let hash = "WITH a AS (SELECT 1 # 2), d AS (DELETE FROM t RETURNING 1\n), b AS (SELECT 1) SELECT 1";
        assert_eq!(info(hash, PG).class, StatementClass::Write);
        // PG: `'\'` is a closed string.
        let backslash = r"WITH a AS (SELECT '\'), d AS (DELETE FROM t RETURNING 1) SELECT 1";
        assert_eq!(info(backslash, PG).class, StatementClass::Write);
        // MySQL: `--1` is not a comment, so the `/*` hides the rest.
        let dashdash = "WITH a AS (SELECT 1 --1) DELETE FROM t WHERE 1 /*\n) SELECT 1 -- */";
        assert_eq!(info(dashdash, MY).class, StatementClass::Write);
        // MySQL: the executable comment is code.
        for sql in [
            "WITH a AS (SELECT 1) /*! DELETE FROM t */",
            "WITH a AS (SELECT 1) /*!80000 DELETE FROM t */ SELECT 1",
            "WITH a AS (SELECT 1 /*! ) DELETE FROM t -- */ ) SELECT 1",
            "WITH a AS (SELECT /*+ MAX_EXECUTION_TIME(1000) */ 1) SELECT 1",
        ] {
            assert_eq!(info(sql, MY).class, StatementClass::Unknown, "{sql}");
        }
    }

    #[test]
    fn executable_comment_in_control_may_disarm() {
        assert!(disarms_read_only("START TRANSACTION /*! READ WRITE */", MY));
        assert!(!disarms_read_only("START TRANSACTION /* READ WRITE */", MY));
    }
}
