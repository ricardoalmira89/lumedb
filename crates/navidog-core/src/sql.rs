//! SQL lexing: statement splitting, highlighting and formatting.

use crate::config::DbKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    Space,
    Comment,
    Str,
    /// Quoted identifier ("x", `x`).
    QuotedIdent,
    Number,
    Word,
    Semicolon,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokKind,
    pub start: usize,
    pub end: usize,
}

pub fn tokenize(sql: &str, kind: DbKind) -> Vec<Token> {
    let b = sql.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let start = i;
        let c = b[i];
        let tk = if c.is_ascii_whitespace() {
            while i < n && b[i].is_ascii_whitespace() {
                i += 1;
            }
            TokKind::Space
        } else if c == b'-' && b.get(i + 1) == Some(&b'-') || (c == b'#' && kind == DbKind::Mysql) {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            TokKind::Comment
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < n && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i = (i + 2).min(n);
            TokKind::Comment
        } else if c == b'\'' || c == b'"' || c == b'`' {
            i += 1;
            loop {
                if i >= n {
                    break;
                }
                if b[i] == b'\\' && kind == DbKind::Mysql && c != b'`' {
                    i += 2;
                    continue;
                }
                if b[i] == c {
                    if b.get(i + 1) == Some(&c) {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            i = i.min(n);
            if c == b'\'' || (c == b'"' && kind == DbKind::Mysql) {
                TokKind::Str
            } else {
                TokKind::QuotedIdent
            }
        } else if c == b'$' && kind == DbKind::Postgres && dollar_tag(&sql[i..]).is_some() {
            let tag = dollar_tag(&sql[i..]).unwrap();
            i += tag.len();
            match sql[i..].find(tag) {
                Some(p) => i += p + tag.len(),
                None => i = n,
            }
            TokKind::Str
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                i += 1;
            }
            TokKind::Number
        } else if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || b[i] >= 0x80) {
                i += 1;
            }
            TokKind::Word
        } else if c == b';' {
            i += 1;
            TokKind::Semicolon
        } else {
            i += 1;
            // Don't split UTF-8 characters.
            while i < n && (b[i] & 0xC0) == 0x80 {
                i += 1;
            }
            TokKind::Other
        };
        out.push(Token { kind: tk, start, end: i });
    }
    out
}

/// `$tag$` at the start of `s`.
fn dollar_tag(s: &str) -> Option<&str> {
    let rest = &s[1..];
    let end = rest.find('$')?;
    let tag = &rest[..end];
    if tag.chars().all(|c| c.is_alphanumeric() || c == '_') && !tag.starts_with(|c: char| c.is_ascii_digit()) {
        Some(&s[..end + 2])
    } else {
        None
    }
}

/// A script statement with its position in the original text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub sql: String,
    pub start: usize,
}

/// Splits a script into statements, honouring strings, comments, `$$`, `DELIMITER`
/// (MySQL) and trigger `BEGIN … END` bodies.
pub fn split_statements(sql: &str, kind: DbKind) -> Vec<Statement> {
    let toks = tokenize(sql, kind);
    let mut out = Vec::new();
    let mut delimiter = ";".to_string();
    let mut stmt_start: Option<usize> = None;
    let mut words: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut skip_until = 0usize;

    let push = |out: &mut Vec<Statement>, from: usize, to: usize| {
        let text = sql[from..to].trim();
        if !text.is_empty() {
            let lead = sql[from..to].len() - sql[from..to].trim_start().len();
            out.push(Statement { sql: text.to_string(), start: from + lead });
        }
    };

    let mut idx = 0;
    while idx < toks.len() {
        let t = toks[idx];
        idx += 1;
        if t.start < skip_until {
            continue;
        }
        if matches!(t.kind, TokKind::Space | TokKind::Comment) {
            continue;
        }
        // DELIMITER at the start of a statement (MySQL client).
        if stmt_start.is_none() && kind == DbKind::Mysql && t.kind == TokKind::Word && sql[t.start..t.end].eq_ignore_ascii_case("DELIMITER") {
            let line_end = sql[t.end..].find('\n').map_or(sql.len(), |p| t.end + p);
            let d = sql[t.end..line_end].trim();
            if !d.is_empty() {
                delimiter = d.to_string();
            }
            skip_until = line_end;
            continue;
        }
        if stmt_start.is_none() {
            stmt_start = Some(t.start);
            words.clear();
            depth = 0;
        }
        if t.kind == TokKind::Word {
            let w = sql[t.start..t.end].to_ascii_uppercase();
            if words.len() < 4 {
                words.push(w.clone());
            }
            let trigger = words.first().is_some_and(|w| w == "CREATE")
                && words.iter().take(4).any(|w| w == "TRIGGER");
            if trigger && delimiter == ";" {
                match w.as_str() {
                    "BEGIN" | "CASE" => depth += 1,
                    "END" => depth -= 1,
                    _ => {}
                }
            }
        }
        let is_delim = if delimiter == ";" {
            t.kind == TokKind::Semicolon && depth <= 0
        } else {
            !matches!(t.kind, TokKind::Str | TokKind::QuotedIdent) && sql[t.start..].starts_with(delimiter.as_str())
        };
        if is_delim {
            push(&mut out, stmt_start.take().unwrap(), t.start);
            skip_until = t.start + delimiter.len();
        }
    }
    if let Some(s) = stmt_start {
        push(&mut out, s, sql.len());
    }
    out
}

/// Statement under the cursor (for "Run current statement").
pub fn statement_at(sql: &str, kind: DbKind, cursor: usize) -> Option<Statement> {
    let stmts = split_statements(sql, kind);
    stmts.iter().rev().find(|s| s.start <= cursor).or(stmts.first()).cloned()
}

pub fn format_sql(sql: &str) -> String {
    let opts = sqlformat::FormatOptions { indent: sqlformat::Indent::Spaces(2), uppercase: true, lines_between_queries: 2 };
    sqlformat::format(sql, &sqlformat::QueryParams::None, opts)
}

/// Does the statement return rows? (to decide whether to apply LIMIT, EXPLAIN…)
pub fn is_query(sql: &str) -> bool {
    let first = sql.trim_start().split(|c: char| !c.is_alphanumeric()).next().unwrap_or("").to_ascii_uppercase();
    matches!(first.as_str(), "SELECT" | "WITH" | "SHOW" | "VALUES" | "TABLE" | "EXPLAIN" | "DESCRIBE" | "DESC" | "PRAGMA")
}

pub const KEYWORDS: &[&str] = &[
    "ADD", "ALL", "ALTER", "AND", "ANY", "AS", "ASC", "AUTO_INCREMENT", "BEGIN", "BETWEEN", "BIGINT", "BOOLEAN", "BY", "CASCADE",
    "CASE", "CAST", "CHECK", "COLUMN", "COMMENT", "COMMIT", "CONSTRAINT", "COUNT", "CREATE", "CROSS", "CURRENT_DATE",
    "CURRENT_TIMESTAMP", "DATABASE", "DATE", "DEFAULT", "DELETE", "DESC", "DISTINCT", "DROP", "ELSE", "END", "EXCEPT", "EXISTS",
    "EXPLAIN", "FALSE", "FOREIGN", "FROM", "FULL", "FUNCTION", "GRANT", "GROUP", "HAVING", "IF", "IN", "INDEX", "INNER", "INSERT",
    "INT", "INTEGER", "INTERSECT", "INTO", "IS", "JOIN", "KEY", "LEFT", "LIKE", "ILIKE", "LIMIT", "NOT", "NULL", "OFFSET", "ON", "OR",
    "ORDER", "OUTER", "OVER", "PARTITION", "PRIMARY", "PROCEDURE", "REFERENCES", "RENAME", "REPLACE", "RETURNING", "RETURNS",
    "REVOKE", "RIGHT", "ROLLBACK", "SCHEMA", "SELECT", "SET", "SHOW", "TABLE", "TEXT", "THEN", "TIMESTAMP", "TO", "TRIGGER",
    "TRUE", "TRUNCATE", "UNION", "UNIQUE", "UPDATE", "USING", "VALUES", "VARCHAR", "VIEW", "WHEN", "WHERE", "WITH",
];

pub fn is_keyword(word: &str) -> bool {
    KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(word))
}
