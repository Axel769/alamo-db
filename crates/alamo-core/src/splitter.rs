//! Statement splitter — §6.5.
//!
//! It must handle **Postgres dollar-quoting `$$`** (MySQL's `DELIMITER` comes
//! with M3): it is a classic breaking point.
//!
//! There are two sets of lexical rules, PG and MySQL/MariaDB, and they diverge
//! exactly where a statement hides: on MySQL `--` is a comment only if
//! followed by whitespace, `#` opens a line comment, block comments do not
//! nest, the backtick delimits identifiers and `$` quotes nothing. The
//! splitter remains a heuristic anyway (the server's `sql_mode`, e.g.
//! `NO_BACKSLASH_ESCAPES`, changes the rules): the MySQL driver has the server
//! confirm that each piece is a single statement.
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

/// A statement found in the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitStatement {
    /// Statement text, without the trailing `;`, not trimmed: the offsets
    /// point to the first non-whitespace character.
    pub sql: String,
    pub origin: StatementOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Plain,
    LineComment,
    /// Block comments in PG nest: `/* /* */ */` is a single comment. The
    /// field is the depth (on MySQL it stays 1).
    BlockComment(u32),
    SingleQuote,
    /// MySQL: identifier between backticks, a doubled `` `` `` is an escape.
    Backtick,
    /// `E'...'`: the backslash escapes, including `\'`.
    EscapeString,
    DoubleQuote,
    /// Dollar-quoting: the value is the byte length of the opening tag
    /// (`$tag$` including the two `$`), stored as a start index into the
    /// buffer for textual comparison.
    Dollar {
        tag_start: usize,
        tag_len: usize,
    },
}

/// Splits the buffer into statements on the `;` at level zero.
///
/// Empty statements (whitespace or comments only) are dropped. The last
/// statement does not require a `;`.
///
/// `backslash_escapes` comes from `Capabilities` (§2.1) and is true only for
/// MySQL/MariaDB: besides the backslash in ordinary strings (`'a\';b'` is ONE
/// string) it enables the other MySQL lexical rules (see the module doc).
pub fn split_statements(sql: &str, backslash_escapes: bool) -> Vec<SplitStatement> {
    let mysql = backslash_escapes;
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut state = State::Plain;

    // Start of the current statement (first byte after the last `;`).
    let mut stmt_start_byte = 0usize;
    // Characters seen up to stmt_start_byte.
    let mut stmt_start_char = 0u32;
    let mut chars_seen = 0u32;

    // Does the current statement contain anything besides whitespace and
    // comments? Without this flag "-- comment only;" would be sent to the
    // server as a statement of its own (EmptyQueryResponse: harmless but noise
    // in the UI).
    let mut has_content = false;

    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        // Advance in characters: count only UTF-8 leading bytes.
        let is_char_boundary = (b & 0xC0) != 0x80;

        match state {
            State::Plain => match b {
                // MySQL: `--1` is "minus minus one", not a comment.
                b'-' if bytes.get(i + 1) == Some(&b'-')
                    && (!mysql
                        || bytes
                            .get(i + 2)
                            .map_or(true, |c| c.is_ascii_whitespace() || c.is_ascii_control())) =>
                {
                    state = State::LineComment
                }
                b'#' if mysql => state = State::LineComment,
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    state = State::BlockComment(1);
                    // MySQL: `/*!…*/`, `/*M!…*/` and `/*+…*/` are code
                    // (executable comments and hints), not comments.
                    if mysql && matches!(bytes.get(i + 2), Some(b'!' | b'M' | b'+')) {
                        has_content = true;
                    }
                    // Consume the asterisk too, so `/*/` is not re-read.
                    i += 1;
                    chars_seen += 1;
                }
                b'\'' => {
                    state = State::SingleQuote;
                    has_content = true;
                }
                b'`' if mysql => {
                    state = State::Backtick;
                    has_content = true;
                }
                b'e' | b'E'
                    if !mysql && bytes.get(i + 1) == Some(&b'\'') && !prev_is_ident(bytes, i) =>
                {
                    state = State::EscapeString;
                    has_content = true;
                    i += 1;
                    chars_seen += 1;
                }
                b'"' => {
                    state = State::DoubleQuote;
                    has_content = true;
                }
                b'$' => {
                    has_content = true;
                    if let Some(tag_len) = dollar_tag_len(bytes, i).filter(|_| !mysql) {
                        state = State::Dollar {
                            tag_start: i,
                            tag_len,
                        };
                        // Skip the whole tag: it is ASCII, bytes = characters.
                        i += tag_len;
                        chars_seen += tag_len as u32;
                        continue;
                    }
                }
                b';' => {
                    if has_content {
                        push_statement(
                            sql,
                            stmt_start_byte,
                            i,
                            stmt_start_char,
                            out.len() as u32,
                            &mut out,
                        );
                    }
                    stmt_start_byte = i + 1;
                    stmt_start_char = chars_seen + 1;
                    has_content = false;
                }
                b if !b.is_ascii_whitespace() => has_content = true,
                _ => {}
            },
            State::LineComment => {
                if b == b'\n' {
                    state = State::Plain;
                }
            }
            State::BlockComment(depth) => {
                if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    state = if depth == 1 {
                        State::Plain
                    } else {
                        State::BlockComment(depth - 1)
                    };
                    i += 1;
                    chars_seen += 1;
                } else if !mysql && b == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    state = State::BlockComment(depth + 1);
                    i += 1;
                    chars_seen += 1;
                }
            }
            State::SingleQuote => {
                if backslash_escapes && b == b'\\' {
                    // MySQL: skip the escaped byte, covers `\'`.
                    if i + 1 < bytes.len() {
                        if (bytes[i + 1] & 0xC0) != 0x80 {
                            chars_seen += 1;
                        }
                        i += 1;
                    }
                } else if b == b'\'' {
                    // `''` is an escaped quote, not a closing one.
                    if bytes.get(i + 1) == Some(&b'\'') {
                        i += 1;
                        chars_seen += 1;
                    } else {
                        state = State::Plain;
                    }
                }
            }
            State::Backtick => {
                if b == b'`' {
                    if bytes.get(i + 1) == Some(&b'`') {
                        i += 1;
                        chars_seen += 1;
                    } else {
                        state = State::Plain;
                    }
                }
            }
            State::EscapeString => {
                if b == b'\\' {
                    // Skip the next byte whatever it is (covers `\'`).
                    if i + 1 < bytes.len() {
                        if (bytes[i + 1] & 0xC0) != 0x80 {
                            chars_seen += 1;
                        }
                        i += 1;
                    }
                } else if b == b'\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        i += 1;
                        chars_seen += 1;
                    } else {
                        state = State::Plain;
                    }
                }
            }
            State::DoubleQuote => {
                // In MySQL (without ANSI_QUOTES) `"..."` is a string and the
                // backslash escapes here too.
                if backslash_escapes && b == b'\\' {
                    if i + 1 < bytes.len() {
                        if (bytes[i + 1] & 0xC0) != 0x80 {
                            chars_seen += 1;
                        }
                        i += 1;
                    }
                } else if b == b'"' {
                    if bytes.get(i + 1) == Some(&b'"') {
                        i += 1;
                        chars_seen += 1;
                    } else {
                        state = State::Plain;
                    }
                }
            }
            State::Dollar { tag_start, tag_len } => {
                if b == b'$' && i + tag_len <= bytes.len() {
                    let open = &bytes[tag_start..tag_start + tag_len];
                    if &bytes[i..i + tag_len] == open {
                        state = State::Plain;
                        i += tag_len;
                        chars_seen += tag_len as u32;
                        continue;
                    }
                }
            }
        }

        if is_char_boundary {
            chars_seen += 1;
        }
        i += 1;
    }

    if has_content {
        push_statement(
            sql,
            stmt_start_byte,
            bytes.len(),
            stmt_start_char,
            out.len() as u32,
            &mut out,
        );
    }
    out
}

/// Can the previous byte be part of an identifier? If so, the `E` of
/// `TABLE E'...'`-like sequences belongs to the word before (`UPDATE`,
/// `DELETE`…) and does not introduce an escape string.
fn prev_is_ident(bytes: &[u8], i: usize) -> bool {
    if i == 0 {
        return false;
    }
    let p = bytes[i - 1];
    p.is_ascii_alphanumeric() || p == b'_' || p >= 0x80
}

/// If a valid dollar-quoting tag (`$$` or `$ident$`) starts at
/// `bytes[i] == b'$'`, returns the tag length in bytes.
fn dollar_tag_len(bytes: &[u8], i: usize) -> Option<usize> {
    debug_assert_eq!(bytes[i], b'$');
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'$' => return Some(j - i + 1),
            b if b.is_ascii_alphanumeric() || b == b'_' => j += 1,
            // `$1` is a placeholder, not a tag; any other byte breaks the
            // tag.
            _ => return None,
        }
    }
    None
}

fn push_statement(
    sql: &str,
    start_byte: usize,
    end_byte: usize,
    start_char: u32,
    index: u32,
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
            index,
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
