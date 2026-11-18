//! SQL lexer — §6.5, §9.7.3. Shared by the splitter and the classifier.
//!
//! There are two sets of lexical rules, PG and MySQL/MariaDB, and they diverge
//! exactly where a statement hides: on MySQL `--` is a comment only if
//! followed by whitespace, `#` opens a line comment, block comments do not
//! nest, the backtick delimits identifiers, `$` quotes nothing and the
//! backslash escapes inside strings; on PG `#` is an operator and `E'…'`
//! strings and dollar-quoting exist. Reading text with the other dialect's
//! rules is not a cosmetic error: `WITH a AS (SELECT 1 # 2), d AS (DELETE …`
//! on PG deletes, while a lexer that takes `#` for a comment sees only reads.
//!
//! It remains a heuristic anyway (the server's `sql_mode`, e.g.
//! `NO_BACKSLASH_ESCAPES`, changes the rules): the MySQL driver has the server
//! confirm that each piece is a single statement.

/// What a token is, as far as splitting and classifying need to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Unquoted identifier or keyword.
    Word,
    /// String, quoted identifier or dollar-quoted body: opaque content.
    Quoted,
    /// MySQL executable comment or optimizer hint (`/*!…*/`, `/*M!…*/`,
    /// `/*+…*/`): the server reads what is inside, so it is code, not a
    /// comment.
    Code,
    /// Any other single byte: punctuation, operators, digits.
    Symbol,
}

/// A token as a byte range of the text. Whitespace and comments produce none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
}

impl Token {
    pub fn text<'a>(&self, sql: &'a str) -> &'a str {
        &sql[self.start..self.end]
    }

    /// Unquoted word equal to `keyword` (lowercase), ignoring case.
    pub fn is_word(&self, sql: &str, keyword: &str) -> bool {
        self.kind == TokenKind::Word && self.text(sql).eq_ignore_ascii_case(keyword)
    }

    pub fn is_symbol(&self, sql: &str, symbol: u8) -> bool {
        self.kind == TokenKind::Symbol && sql.as_bytes()[self.start] == symbol
    }
}

/// Tokens of `sql`. `backslash_escapes` comes from `Capabilities` (§2.1) and
/// is true only for MySQL/MariaDB: besides the backslash in ordinary strings
/// (`'a\';b'` is ONE string) it enables the other MySQL lexical rules.
pub fn tokens(sql: &str, backslash_escapes: bool) -> Lexer<'_> {
    Lexer {
        bytes: sql.as_bytes(),
        i: 0,
        mysql: backslash_escapes,
    }
}

pub struct Lexer<'a> {
    bytes: &'a [u8],
    i: usize,
    mysql: bool,
}

impl Iterator for Lexer<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        let bytes = self.bytes;
        loop {
            while self.i < bytes.len() && bytes[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            let start = self.i;
            let b = *bytes.get(start)?;
            let next = bytes.get(start + 1).copied();
            let (kind, end) = match b {
                // MySQL: `--1` is "minus minus one", not a comment.
                b'-' if next == Some(b'-')
                    && (!self.mysql
                        || bytes
                            .get(start + 2)
                            .is_none_or(|c| c.is_ascii_whitespace() || c.is_ascii_control())) =>
                {
                    self.i = line_end(bytes, start);
                    continue;
                }
                b'#' if self.mysql => {
                    self.i = line_end(bytes, start);
                    continue;
                }
                b'/' if next == Some(b'*') => {
                    let rest = &bytes[start + 2..];
                    if self.mysql
                        && (rest.starts_with(b"!")
                            || rest.starts_with(b"M!")
                            || rest.starts_with(b"+"))
                    {
                        (TokenKind::Code, block_comment_end(bytes, start, false))
                    } else {
                        self.i = block_comment_end(bytes, start, !self.mysql);
                        continue;
                    }
                }
                b'\'' => (
                    TokenKind::Quoted,
                    quoted_end(bytes, start + 1, b'\'', self.mysql),
                ),
                b'"' => (
                    TokenKind::Quoted,
                    quoted_end(bytes, start + 1, b'"', self.mysql),
                ),
                b'`' if self.mysql => {
                    (TokenKind::Quoted, quoted_end(bytes, start + 1, b'`', false))
                }
                // `E'…'`: the backslash escapes, including `\'`. Not the `E`
                // ending a word (`… DELETE'…'` cannot happen, but `1E'…'` can).
                b'e' | b'E'
                    if !self.mysql && next == Some(b'\'') && !prev_is_ident(bytes, start) =>
                {
                    (TokenKind::Quoted, quoted_end(bytes, start + 2, b'\'', true))
                }
                b'$' if !self.mysql => match dollar_tag_len(bytes, start) {
                    Some(tag_len) => (TokenKind::Quoted, dollar_end(bytes, start, tag_len)),
                    // `$1` is a placeholder.
                    None => (TokenKind::Symbol, start + 1),
                },
                b if is_ident_start(b) => {
                    let mut j = start + 1;
                    while j < bytes.len() && is_ident_part(bytes[j]) {
                        j += 1;
                    }
                    (TokenKind::Word, j)
                }
                _ => (TokenKind::Symbol, start + 1),
            };
            self.i = end;
            return Some(Token { kind, start, end });
        }
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

/// `$` is allowed inside identifiers on both dialects (`a$b`), so a `$`
/// there never opens dollar-quoting.
fn is_ident_part(b: u8) -> bool {
    is_ident_start(b) || b.is_ascii_digit() || b == b'$'
}

/// Can the previous byte be part of an identifier? If so, the `E` of
/// `1E'...'` does not introduce an escape string.
fn prev_is_ident(bytes: &[u8], i: usize) -> bool {
    i > 0 && is_ident_part(bytes[i - 1])
}

/// Position of the `\n` ending the line comment (or the end of the text).
fn line_end(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |p| start + p)
}

/// End (exclusive) of the block comment opening at `start`; PG nests them,
/// MySQL does not. Unterminated: the end of the text.
fn block_comment_end(bytes: &[u8], start: usize, nested: bool) -> usize {
    let mut depth = 1u32;
    let mut j = start + 2;
    while j < bytes.len() {
        if bytes[j] == b'*' && bytes.get(j + 1) == Some(&b'/') {
            depth -= 1;
            j += 2;
            if depth == 0 {
                return j;
            }
        } else if nested && bytes[j] == b'/' && bytes.get(j + 1) == Some(&b'*') {
            depth += 1;
            j += 2;
        } else {
            j += 1;
        }
    }
    bytes.len()
}

/// End (exclusive) of a quoted token whose content starts at `j`: a doubled
/// quote is an escape, the backslash too when `backslash` is set.
/// Unterminated: the end of the text.
fn quoted_end(bytes: &[u8], mut j: usize, quote: u8, backslash: bool) -> usize {
    while j < bytes.len() {
        if backslash && bytes[j] == b'\\' {
            j += 2;
        } else if bytes[j] == quote {
            if bytes.get(j + 1) == Some(&quote) {
                j += 2;
            } else {
                return j + 1;
            }
        } else {
            j += 1;
        }
    }
    bytes.len()
}

/// If a valid dollar-quoting tag (`$$` or `$ident$`) starts at
/// `bytes[i] == b'$'`, returns the tag length in bytes.
fn dollar_tag_len(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'$' => return Some(j - i + 1),
            b if b.is_ascii_alphanumeric() || b == b'_' => j += 1,
            _ => return None,
        }
    }
    None
}

/// End (exclusive) of the dollar-quoted body opened by the tag at `start`.
fn dollar_end(bytes: &[u8], start: usize, tag_len: usize) -> usize {
    let tag = &bytes[start..start + tag_len];
    let body = start + tag_len;
    bytes[body..]
        .windows(tag_len)
        .position(|w| w == tag)
        .map_or(bytes.len(), |p| body + p + tag_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(sql: &str, mysql: bool) -> Vec<(TokenKind, &str)> {
        tokens(sql, mysql).map(|t| (t.kind, t.text(sql))).collect()
    }

    #[test]
    fn comments_produce_no_tokens() {
        assert_eq!(
            kinds("-- a\n/* b /* c */ d */ x", false),
            [(TokenKind::Word, "x")]
        );
        assert_eq!(kinds("# a\nx", true), [(TokenKind::Word, "x")]);
    }

    #[test]
    fn hash_is_an_operator_on_pg() {
        assert_eq!(
            kinds("1 # 2", false),
            [
                (TokenKind::Symbol, "1"),
                (TokenKind::Symbol, "#"),
                (TokenKind::Symbol, "2")
            ]
        );
    }

    #[test]
    fn mysql_double_dash_needs_whitespace() {
        assert_eq!(kinds("1 --1", true).len(), 4);
        assert_eq!(kinds("1 -- 1", true).len(), 1);
    }

    #[test]
    fn mysql_comments_do_not_nest_and_executable_ones_are_code() {
        assert_eq!(kinds("/* /* */ x", true), [(TokenKind::Word, "x")]);
        assert_eq!(kinds("/* /* */ x", false), []);
        for c in ["/*! DELETE */", "/*M!100000 x */", "/*+ hint */"] {
            assert_eq!(kinds(c, true), [(TokenKind::Code, c)]);
            assert_eq!(kinds(c, false), [], "on PG it is a comment: {c}");
        }
    }

    #[test]
    fn backslash_escapes_only_where_the_dialect_says() {
        // PG standard string: `'\'` is closed.
        assert_eq!(
            kinds(r"'\' x", false),
            [(TokenKind::Quoted, r"'\'"), (TokenKind::Word, "x")]
        );
        // MySQL: the quote is escaped, the string runs to the end.
        assert_eq!(kinds(r"'\' x", true), [(TokenKind::Quoted, r"'\' x")]);
        // PG `E''`, but not after an identifier character.
        assert_eq!(kinds(r"E'\'' x", false)[0], (TokenKind::Quoted, r"E'\''"));
        assert_eq!(kinds("1E'x'", false)[2], (TokenKind::Quoted, "'x'"));
    }

    #[test]
    fn dollar_quoting_and_identifiers_with_dollar() {
        assert_eq!(
            kinds("$a$ ; $a$ x", false),
            [(TokenKind::Quoted, "$a$ ; $a$"), (TokenKind::Word, "x")]
        );
        assert_eq!(kinds("$1", false)[0], (TokenKind::Symbol, "$"));
        assert_eq!(kinds("a$b$ x", false)[0], (TokenKind::Word, "a$b$"));
        assert_eq!(kinds("$$x$$", true)[0], (TokenKind::Symbol, "$"));
    }

    #[test]
    fn unterminated_tokens_run_to_the_end() {
        assert_eq!(kinds("'abc", false), [(TokenKind::Quoted, "'abc")]);
        assert_eq!(kinds("/*! abc", true), [(TokenKind::Code, "/*! abc")]);
        assert_eq!(kinds("x /* abc", false), [(TokenKind::Word, "x")]);
        assert_eq!(kinds(r"'a\", true), [(TokenKind::Quoted, r"'a\")]);
    }

    #[test]
    fn non_ascii_words_stay_whole() {
        assert_eq!(kinds("città;", false)[0], (TokenKind::Word, "città"));
    }
}
