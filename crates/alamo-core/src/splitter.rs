//! Statement splitter — §6.5.
//!
//! It must handle **Postgres dollar-quoting `$$`** (MySQL's `DELIMITER` comes
//! with M3): it is a classic breaking point. The lexical rules of each dialect
//! live in `lexer`, shared with the classifier: a `;` splits only when it is a
//! token of its own.
//!
//! Every statement carries its start offset in the full buffer **both in
//! bytes and in characters** (§6.1): PG reports error positions in 1-based
//! characters relative to the statement, and with non-ASCII text confusing
//! bytes and characters makes the highlight slide.
//!
//! This is the *robust* splitter on the Rust side. Autocomplete while typing
//! requires a tolerant tokenizer on the frontend side: they are two different
//! things and this splitter must not try to cover both (§6.5).

use crate::error::StatementOrigin;
use crate::lexer::tokens;

/// A statement found in the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitStatement {
    /// Statement text, without the trailing `;`, not trimmed: the offsets
    /// point to the first non-whitespace character.
    pub sql: String,
    pub origin: StatementOrigin,
}

/// Splits the buffer into statements on the `;` at level zero.
///
/// Empty statements (whitespace or comments only) are dropped. The last
/// statement does not require a `;`.
///
/// `backslash_escapes` comes from `Capabilities` (§2.1): see `lexer::tokens`.
pub fn split_statements(sql: &str, backslash_escapes: bool) -> Vec<SplitStatement> {
    let mut out = Vec::new();
    // Start of the current statement (first byte after the last `;`).
    let mut stmt_start = 0usize;
    // Characters up to `counted`, advanced as statements start: the starts
    // only grow, so the whole buffer is counted once.
    let mut counted = 0usize;
    let mut chars = 0u32;
    let mut char_at = |byte: usize| {
        chars += sql[counted..byte].chars().count() as u32;
        counted = byte;
        chars
    };
    // Does the current statement contain anything besides whitespace and
    // comments? Without this flag "-- comment only;" would be sent to the
    // server as a statement of its own (EmptyQueryResponse: harmless but noise
    // in the UI).
    let mut has_content = false;

    for tok in tokens(sql, backslash_escapes) {
        if tok.is_symbol(sql, b';') {
            if has_content {
                let start_char = char_at(stmt_start);
                push_statement(sql, stmt_start, tok.start, start_char, &mut out);
            }
            stmt_start = tok.end;
            has_content = false;
        } else {
            has_content = true;
        }
    }
    if has_content {
        let start_char = char_at(stmt_start);
        push_statement(sql, stmt_start, sql.len(), start_char, &mut out);
    }
    out
}

fn push_statement(
    sql: &str,
    start_byte: usize,
    end_byte: usize,
    start_char: u32,
    out: &mut Vec<SplitStatement>,
) {
    let raw = &sql[start_byte..end_byte];
    // Drop statements made only of whitespace; the declared offset points to
    // the first useful character, so error highlighting does not start from
    // an empty line.
    let trimmed_start = raw.len() - raw.trim_start().len();
    let trimmed = raw.trim_end();
    if trimmed[trimmed_start.min(trimmed.len())..].is_empty() {
        return;
    }
    let lead_chars = raw[..trimmed_start].chars().count() as u32;
    out.push(SplitStatement {
        sql: trimmed[trimmed_start..].to_string(),
        origin: StatementOrigin {
            byte_offset: (start_byte + trimmed_start) as u32,
            char_offset: start_char + lead_chars,
            index: out.len() as u32,
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqls(input: &str) -> Vec<String> {
        split_statements(input, false)
            .into_iter()
            .map(|s| s.sql)
            .collect()
    }

    fn sqls_my(input: &str) -> Vec<String> {
        split_statements(input, true)
            .into_iter()
            .map(|s| s.sql)
            .collect()
    }

    #[test]
    fn splits_on_semicolon() {
        assert_eq!(sqls("SELECT 1; SELECT 2"), vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn ignores_semicolons_in_strings_and_comments() {
        // Leading comments stay in the statement: PG counts error positions
        // from the start of what it receives.
        assert_eq!(
            sqls("SELECT ';'; -- nothing; here\nSELECT 2; /* ; */ SELECT 3"),
            vec![
                "SELECT ';'",
                "-- nothing; here\nSELECT 2",
                "/* ; */ SELECT 3"
            ]
        );
    }

    #[test]
    fn double_quoted_identifier_keeps_semicolon() {
        assert_eq!(
            sqls(r#"SELECT "a;b" FROM t"#),
            vec![r#"SELECT "a;b" FROM t"#]
        );
    }

    #[test]
    fn escaped_quote_by_doubling() {
        assert_eq!(sqls("SELECT 'it''s; fine'; SELECT 2").len(), 2);
    }

    #[test]
    fn escape_string_with_backslash() {
        assert_eq!(sqls(r"SELECT E'a\'b;c'; SELECT 2").len(), 2);
    }

    #[test]
    fn dollar_quoting_plain_and_tagged() {
        let f = "CREATE FUNCTION f() RETURNS int AS $$ SELECT 1; $$ LANGUAGE sql";
        assert_eq!(sqls(f), vec![f]);
        let g =
            "CREATE FUNCTION g() AS $body$ BEGIN RETURN; END; $body$ LANGUAGE plpgsql; SELECT 1";
        assert_eq!(sqls(g).len(), 2);
    }

    #[test]
    fn dollar_placeholder_is_not_a_tag() {
        assert_eq!(sqls("SELECT $1; SELECT $2"), vec!["SELECT $1", "SELECT $2"]);
    }

    #[test]
    fn nested_block_comments() {
        assert_eq!(
            sqls("SELECT 1 /* outer /* inner; */ still; */; SELECT 2").len(),
            2
        );
    }

    #[test]
    fn empty_statements_are_dropped() {
        assert_eq!(sqls(";;  ;\n-- comment only\n;SELECT 1;"), vec!["SELECT 1"]);
    }

    #[test]
    fn offsets_in_bytes_and_chars_diverge_on_non_ascii() {
        // "è" = 2 bytes, 1 character. The comment precedes the second statement.
        let input = "SELECT 'perché'; SELECT 2";
        let stmts = split_statements(input, false);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].origin.byte_offset, 0);
        assert_eq!(stmts[0].origin.char_offset, 0);
        // First statement: 16 characters, 17 bytes ("é" is two) + "; " → the
        // second one starts at byte 18, character 17.
        assert_eq!(stmts[1].sql, "SELECT 2");
        assert_eq!(
            stmts[1].origin.byte_offset as usize,
            input.find("SELECT 2").unwrap()
        );
        assert_eq!(stmts[1].origin.char_offset, 17);
        assert_eq!(stmts[1].origin.index, 1);
    }

    #[test]
    fn update_ending_in_e_is_not_escape_string() {
        // The trailing `E` of "WHERE" must not open an escape string.
        assert_eq!(sqls("UPDATE t SET a = 1 WHERE'x' = 'x'; SELECT 2").len(), 2);
    }

    #[test]
    fn mysql_backslash_keeps_string_open() {
        // `'a\';b'` on MySQL is ONE string: the `;` is inside the literal.
        assert_eq!(sqls_my(r"SELECT 'a\';b'"), vec![r"SELECT 'a\';b'"]);
        assert_eq!(sqls_my(r#"SELECT "a\";b""#), vec![r#"SELECT "a\";b""#]);
        // On PG the same sequence closes the string at `'`: two statements.
        assert_eq!(sqls(r"SELECT 'a\';b'").len(), 2);
    }

    #[test]
    fn mysql_trailing_backslash_at_end_of_buffer() {
        // Backslash as the last byte: it must not read past the buffer.
        assert_eq!(sqls_my(r"SELECT 'a\").len(), 1);
    }

    #[test]
    fn mysql_double_dash_without_space_is_not_a_comment() {
        assert_eq!(
            sqls_my("SELECT 1 --1; DELETE FROM t"),
            vec!["SELECT 1 --1", "DELETE FROM t"]
        );
        assert_eq!(
            sqls_my("SELECT 1 -- note; still\nSELECT 2"),
            vec!["SELECT 1 -- note; still\nSELECT 2"]
        );
        // `--` at the end of the buffer is an empty comment.
        assert_eq!(sqls_my("SELECT 1 --"), vec!["SELECT 1 --"]);
        // On PG `--1` is a comment.
        assert_eq!(sqls("SELECT 1 --1; DELETE FROM t").len(), 1);
    }

    #[test]
    fn mysql_hash_is_a_comment() {
        assert_eq!(
            sqls_my("SELECT 1 # note; here\nFROM t; SELECT 2"),
            vec!["SELECT 1 # note; here\nFROM t", "SELECT 2"]
        );
    }

    #[test]
    fn mysql_comments_do_not_nest() {
        assert_eq!(
            sqls_my("SELECT 1 /* /* */; DELETE FROM t; /* */"),
            vec!["SELECT 1 /* /* */", "DELETE FROM t"]
        );
    }

    #[test]
    fn mysql_executable_comment_is_content() {
        assert_eq!(
            sqls_my("SELECT 1; /*!50000 DELETE FROM t */"),
            vec!["SELECT 1", "/*!50000 DELETE FROM t */"]
        );
    }

    #[test]
    fn mysql_backtick() {
        assert_eq!(
            sqls_my("SELECT 1 AS `x'`; DELETE FROM t; SELECT 1 AS `'`"),
            vec!["SELECT 1 AS `x'`", "DELETE FROM t", "SELECT 1 AS `'`"]
        );
        assert_eq!(
            sqls_my("SELECT `a;b`, `c``;d`"),
            vec!["SELECT `a;b`, `c``;d`"]
        );
    }

    #[test]
    fn mysql_no_dollar_quoting() {
        assert_eq!(
            sqls_my("SELECT 1 AS a$q$; DELETE FROM t; SELECT 1 AS $q$"),
            vec!["SELECT 1 AS a$q$", "DELETE FROM t", "SELECT 1 AS $q$"]
        );
    }

    #[test]
    fn trailing_statement_without_semicolon() {
        let stmts = split_statements("SELECT 1;\n  SELECT 2", false);
        assert_eq!(stmts[1].sql, "SELECT 2");
        assert_eq!(stmts[1].origin.byte_offset, 12);
    }
}
