//! Structured Spark identifiers and their SQL/display rendering.

use std::borrow::Cow;
use std::fmt;

use sqlparser::ast::ObjectNamePart;
use sqlparser::dialect::Dialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer, Whitespace};
use unicode_general_category::{get_general_category, GeneralCategory};

use super::name_fold::{eq_fold, fold_key};

#[derive(Debug)]
struct SparkRelationIdentifierDialect;

impl Dialect for SparkRelationIdentifierDialect {
    fn is_identifier_start(&self, ch: char) -> bool {
        ch == '_' || is_spark_unicode_letter(ch)
    }

    fn is_identifier_part(&self, ch: char) -> bool {
        ch == '_' || ch.is_ascii_digit() || is_spark_unicode_letter(ch)
    }

    fn is_delimited_identifier_start(&self, ch: char) -> bool {
        ch == '`'
    }

    fn supports_numeric_prefix(&self) -> bool {
        true
    }

    fn supports_nested_comments(&self) -> bool {
        true
    }
}

/// A relation qualifier whose parsed name-part boundaries are preserved.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Qualifier(Vec<String>);

impl Qualifier {
    /// Construct a one-part qualifier, as used for a literal relation alias.
    pub fn single(part: impl Into<String>) -> Self {
        Self(vec![part.into()])
    }

    /// Construct a qualifier from already-parsed multipart name parts.
    pub fn from_parts(parts: Vec<String>) -> Self {
        debug_assert!(!parts.is_empty(), "a qualifier has at least one name part");
        Self(parts)
    }

    /// Borrow the qualifier's ordered name parts.
    pub fn parts(&self) -> &[String] {
        &self.0
    }

    /// Whether `suffix` matches this qualifier's final parts under Spark's
    /// case-insensitive identifier resolver.
    pub fn matches_suffix(&self, suffix: &Self) -> bool {
        self.0.len() >= suffix.0.len()
            && self.0[self.0.len() - suffix.0.len()..]
                .iter()
                .zip(&suffix.0)
                .all(|(left, right)| eq_fold(left, right))
    }

    /// Whether both qualifiers have the same case-folded parts.
    pub fn eq_case_folded(&self, other: &Self) -> bool {
        self.0.len() == other.0.len() && self.matches_suffix(other)
    }

    /// Return a componentwise case-folded qualifier for keyed lookup.
    pub(crate) fn case_folded(&self) -> Self {
        Self(self.0.iter().map(|part| fold_key(part)).collect())
    }

    /// Render a Spark-facing name, backtick-quoting non-simple parts.
    pub fn display_name(&self) -> String {
        display_identifier_parts(&self.0)
    }

    /// Render a DuckDB identifier path, quoting every part independently.
    pub(crate) fn to_sql(&self) -> String {
        self.0
            .iter()
            .map(|part| quote_ident(part).into_owned())
            .collect::<Vec<_>>()
            .join(".")
    }
}

impl fmt::Display for Qualifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_name())
    }
}

/// A malformed Spark multipart identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidMultipartIdentifier;

impl fmt::Display for InvalidMultipartIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid multipart identifier")
    }
}

impl std::error::Error for InvalidMultipartIdentifier {}

/// Spark SQL error class for a malformed relation identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlIdentifierError {
    ParseEmptyStatement,
    ParseSyntax,
    InvalidIdentifier,
}

impl SqlIdentifierError {
    pub fn error_class(self) -> &'static str {
        match self {
            Self::ParseEmptyStatement => "PARSE_EMPTY_STATEMENT",
            Self::ParseSyntax => "PARSE_SYNTAX_ERROR",
            Self::InvalidIdentifier => "INVALID_IDENTIFIER",
        }
    }
}

impl fmt::Display for SqlIdentifierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.error_class())
    }
}

impl std::error::Error for SqlIdentifierError {}

/// Parse Spark's dotted/backtick identifier grammar into ordered name parts.
pub fn parse_multipart_identifier(name: &str) -> Result<Vec<String>, InvalidMultipartIdentifier> {
    split_multipart_identifier(name).map(|parts| parts.into_iter().map(|part| part.value).collect())
}

/// Parse a SQL relation identifier with Spark's SQL lexer rules.
pub fn parse_sql_multipart_identifier(name: &str) -> Result<Vec<String>, SqlIdentifierError> {
    match parse_sql_object_name(name) {
        Ok(parts) => Ok(parts),
        Err(SqlIdentifierError::InvalidIdentifier) => Err(SqlIdentifierError::InvalidIdentifier),
        Err(_) => split_multipart_identifier(name)
            .map_err(|_| classify_sql_identifier_error(name))
            .and_then(|parts| {
                if parts.iter().any(|part| {
                    !part.quoted
                        && (part.value.is_empty()
                            || !part
                                .value
                                .chars()
                                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                            || is_numeric_literal_token(&part.value))
                }) {
                    return Err(classify_sql_identifier_error(name));
                }
                Ok(parts.into_iter().map(|part| part.value).collect())
            }),
    }
}

fn parse_sql_object_name(name: &str) -> Result<Vec<String>, SqlIdentifierError> {
    let dialect = SparkRelationIdentifierDialect;
    let normalized = normalize_sql_identifier_input(name)?;
    let mut parser = Parser::new(&dialect)
        .try_with_sql(&normalized)
        .map_err(|_| classify_sql_identifier_error(name))?;
    let object = parser
        .parse_object_name(false)
        .map_err(|_| classify_sql_identifier_error(name))?;
    parser
        .expect_token(&Token::EOF)
        .map_err(|_| classify_sql_identifier_error(name))?;
    object
        .0
        .into_iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(identifier) if identifier.quote_style.is_some() => {
                Ok(identifier.value)
            }
            ObjectNamePart::Identifier(identifier) if !identifier.value.is_ascii() => {
                Err(SqlIdentifierError::InvalidIdentifier)
            }
            ObjectNamePart::Identifier(identifier)
                if !is_numeric_literal_token(&identifier.value) =>
            {
                Ok(identifier.value)
            }
            _ => Err(SqlIdentifierError::ParseSyntax),
        })
        .collect()
}

fn normalize_sql_identifier_input(name: &str) -> Result<Cow<'_, str>, SqlIdentifierError> {
    let bytes = name.as_bytes();
    let mut masked_tokens = Vec::new();
    let mut depth = 0;
    let mut index = 0;
    while index < bytes.len() {
        if depth == 0 {
            if matches!(bytes[index], b'`' | b'\'' | b'"') {
                index = skip_sql_quote(bytes, index, bytes[index]);
            } else if bytes.get(index..index + 2) == Some(b"--") {
                index += 2;
                while index < bytes.len() {
                    if bytes.get(index..index + 2) == Some(b"\\\n") {
                        masked_tokens.push((index, 2));
                        index += 2;
                    } else if matches!(bytes[index], b'\n' | b'\r') {
                        index += 1;
                        break;
                    } else {
                        index += 1;
                    }
                }
            } else if bytes.get(index..index + 3) == Some(b"/*+") {
                index = bytes[index + 3..]
                    .windows(2)
                    .position(|window| window == b"*/")
                    .map_or(bytes.len(), |offset| index + 3 + offset + 2);
            } else if bytes.get(index..index + 2) == Some(b"/*") {
                depth = 1;
                index += 2;
            } else {
                let ch = name[index..]
                    .chars()
                    .next()
                    .ok_or(SqlIdentifierError::ParseSyntax)?;
                if matches!(ch, '\u{0085}' | '\u{2029}') {
                    return Err(SqlIdentifierError::ParseSyntax);
                }
                index += ch.len_utf8();
            }
        } else if bytes.get(index..index + 3) == Some(b"/*+") {
            masked_tokens.push((index, 3));
            index += 3;
        } else if bytes.get(index..index + 2) == Some(b"/*") {
            depth += 1;
            index += 2;
        } else if bytes.get(index..index + 2) == Some(b"*/") {
            depth -= 1;
            index += 2;
        } else {
            index += 1;
        }
    }
    if masked_tokens.is_empty() {
        return Ok(Cow::Borrowed(name));
    }
    let mut normalized = name.to_owned();
    for (offset, len) in masked_tokens {
        normalized.replace_range(offset..offset + len, if len == 2 { "  " } else { "   " });
    }
    Ok(Cow::Owned(normalized))
}

fn skip_sql_quote(bytes: &[u8], mut index: usize, quote: u8) -> usize {
    index += 1;
    while index < bytes.len() {
        if quote != b'`' && bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
        } else if bytes[index] == quote {
            if bytes.get(index + 1) == Some(&quote) {
                index += 2;
            } else {
                return index + 1;
            }
        } else {
            index += 1;
        }
    }
    index
}

fn classify_sql_identifier_error(name: &str) -> SqlIdentifierError {
    let dialect = SparkRelationIdentifierDialect;
    let Ok(tokens) = Tokenizer::new(&dialect, name).tokenize() else {
        return SqlIdentifierError::ParseSyntax;
    };
    let has_comment = tokens.iter().any(|token| {
        matches!(
            token,
            Token::Whitespace(
                Whitespace::SingleLineComment { .. } | Whitespace::MultiLineComment(_)
            )
        )
    });
    let tokens: Vec<&Token> = tokens
        .iter()
        .filter(|token| !matches!(token, Token::Whitespace(_)))
        .collect();
    if tokens.is_empty() {
        return if has_comment {
            SqlIdentifierError::ParseSyntax
        } else {
            SqlIdentifierError::ParseEmptyStatement
        };
    }
    if invalid_uri_at_parser_frontier(name) || invalid_identifier_at_parser_frontier(&tokens) {
        SqlIdentifierError::InvalidIdentifier
    } else {
        SqlIdentifierError::ParseSyntax
    }
}

fn invalid_uri_at_parser_frontier(name: &str) -> bool {
    name.match_indices("://").any(|(scheme, _)| {
        if !sql_position_is_code(name, scheme)
            || !name[scheme + 3..]
                .chars()
                .next()
                .is_some_and(is_spark_uri_char)
        {
            return false;
        }
        let scheme_start = name[..scheme]
            .char_indices()
            .rev()
            .find_map(|(index, ch)| {
                (!is_spark_identifier_char(ch)).then_some(index + ch.len_utf8())
            })
            .unwrap_or(0);
        let scheme_name = &name[scheme_start..scheme];
        if scheme_name.is_empty() || !scheme_name.chars().all(is_spark_unicode_letter) {
            return false;
        }
        let mut prefix = name[..scheme_start].to_owned();
        prefix.push_str("td_uri");
        parse_sql_object_name(&prefix).is_ok()
    })
}

fn is_spark_uri_char(ch: char) -> bool {
    ch == '_'
        || ch.is_ascii_digit()
        || is_spark_unicode_letter(ch)
        || matches!(ch, '/' | '-' | '.' | '?' | '=' | '&' | '#' | '%')
}

fn is_spark_identifier_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_digit() || is_spark_unicode_letter(ch)
}

fn is_spark_unicode_letter(ch: char) -> bool {
    matches!(
        get_general_category(ch),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
    )
}

fn sql_position_is_code(name: &str, target: usize) -> bool {
    let bytes = name.as_bytes();
    let mut index = 0;
    while index < target {
        let end = if matches!(bytes[index], b'`' | b'\'' | b'"') {
            skip_sql_quote(bytes, index, bytes[index])
        } else if bytes.get(index..index + 2) == Some(b"--") {
            skip_sql_line_comment(bytes, index)
        } else if bytes.get(index..index + 2) == Some(b"/*") {
            skip_sql_block_comment(bytes, index)
        } else {
            index += name[index..].chars().next().map_or(1, char::len_utf8);
            continue;
        };
        if target < end {
            return false;
        }
        index = end;
    }
    true
}

fn skip_sql_line_comment(bytes: &[u8], mut index: usize) -> usize {
    index += 2;
    while index < bytes.len() {
        if bytes.get(index..index + 2) == Some(b"\\\n") {
            index += 2;
        } else if matches!(bytes[index], b'\n' | b'\r') {
            return index + 1;
        } else {
            index += 1;
        }
    }
    index
}

fn skip_sql_block_comment(bytes: &[u8], mut index: usize) -> usize {
    let mut depth = 1;
    index += 2;
    while index < bytes.len() {
        if bytes.get(index..index + 3) == Some(b"/*+") {
            index += 3;
        } else if bytes.get(index..index + 2) == Some(b"/*") {
            depth += 1;
            index += 2;
        } else if bytes.get(index..index + 2) == Some(b"*/") {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return index;
            }
        } else {
            index += 1;
        }
    }
    index
}

fn invalid_identifier_at_parser_frontier(tokens: &[&Token]) -> bool {
    let mut index = 0;
    loop {
        let Some(Token::Word(word)) = tokens.get(index).copied() else {
            return false;
        };
        if word.quote_style.is_none() && !word.value.is_ascii() {
            return true;
        }
        if matches!(
            tokens.get(index + 1..index + 3),
            Some([Token::Minus, Token::Word(_)])
        ) {
            return true;
        }
        if tokens.get(index + 1) != Some(&&Token::Period) {
            return false;
        }
        index += 2;
    }
}

fn is_numeric_literal_token(part: &str) -> bool {
    let bytes = part.as_bytes();
    let mut index = 0;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    if index == 0 {
        return false;
    }
    if index == bytes.len() {
        return true;
    }
    match bytes[index].to_ascii_lowercase() {
        b'd' | b'l' | b's' | b'y' | b'f' => index + 1 == bytes.len(),
        b'b' => {
            bytes
                .get(index + 1)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(&b'd'))
                && index + 2 == bytes.len()
        }
        b'e' => {
            index += 1;
            if matches!(bytes.get(index), Some(b'+' | b'-')) {
                index += 1;
            }
            let exponent_start = index;
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
            if index == exponent_start {
                return false;
            }
            if index == bytes.len() {
                return true;
            }
            match bytes[index].to_ascii_lowercase() {
                b'f' | b'd' => index + 1 == bytes.len(),
                b'b' => {
                    bytes
                        .get(index + 1)
                        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(&b'd'))
                        && index + 2 == bytes.len()
                }
                _ => false,
            }
        }
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedNamePart {
    value: String,
    quoted: bool,
}

fn split_multipart_identifier(
    name: &str,
) -> Result<Vec<ParsedNamePart>, InvalidMultipartIdentifier> {
    if name.is_empty() {
        return Ok(vec![ParsedNamePart {
            value: String::new(),
            quoted: false,
        }]);
    }
    let chars: Vec<char> = name.chars().collect();
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut in_backticks = false;
    let mut just_closed_quote = false;
    let mut quoted_part = false;
    let mut index = 0;

    while index < chars.len() {
        match (in_backticks, chars[index]) {
            (true, '`') if chars.get(index + 1) == Some(&'`') => {
                part.push('`');
                index += 1;
            }
            (true, '`') => {
                in_backticks = false;
                just_closed_quote = true;
                if chars.get(index + 1).is_some_and(|next| *next != '.') {
                    return Err(InvalidMultipartIdentifier);
                }
            }
            (true, ch) => part.push(ch),
            (false, '`') => {
                if just_closed_quote || !part.is_empty() {
                    return Err(InvalidMultipartIdentifier);
                }
                in_backticks = true;
                just_closed_quote = false;
                quoted_part = true;
            }
            (false, '.') => {
                if (!just_closed_quote && part.is_empty()) || index + 1 == chars.len() {
                    return Err(InvalidMultipartIdentifier);
                }
                parts.push(ParsedNamePart {
                    value: std::mem::take(&mut part),
                    quoted: quoted_part,
                });
                just_closed_quote = false;
                quoted_part = false;
            }
            (false, ch) => {
                if just_closed_quote {
                    return Err(InvalidMultipartIdentifier);
                }
                part.push(ch);
            }
        }
        index += 1;
    }

    if in_backticks || (!just_closed_quote && part.is_empty()) {
        return Err(InvalidMultipartIdentifier);
    }
    parts.push(ParsedNamePart {
        value: part,
        quoted: quoted_part,
    });
    Ok(parts)
}

/// Render ordered Spark name parts without losing quoted boundaries.
pub(super) fn display_identifier_parts(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| {
            if is_simple_spark_identifier(part) {
                part.clone()
            } else {
                format!("`{}`", part.replace('`', "``"))
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn is_simple_spark_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// DuckDB reserved words that require SQL identifier quoting.
pub(crate) const DUCKDB_RESERVED: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asymmetric",
    "at",
    "both",
    "case",
    "cast",
    "check",
    "collate",
    "column",
    "constraint",
    "create",
    "cross",
    "current_catalog",
    "current_date",
    "current_role",
    "current_time",
    "current_timestamp",
    "current_user",
    "default",
    "deferrable",
    "desc",
    "describe",
    "distinct",
    "do",
    "else",
    "end",
    "except",
    "false",
    "fetch",
    "for",
    "foreign",
    "from",
    "full",
    "grant",
    "group",
    "groups",
    "having",
    "in",
    "initially",
    "inner",
    "intersect",
    "into",
    "join",
    "lateral",
    "leading",
    "left",
    "limit",
    "list",
    "map",
    "natural",
    "not",
    "null",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "outer",
    "over",
    "partition",
    "pivot",
    "placing",
    "primary",
    "qualify",
    "range",
    "references",
    "returning",
    "right",
    "rows",
    "sample",
    "select",
    "session_user",
    "some",
    "struct",
    "symmetric",
    "table",
    "then",
    "to",
    "trailing",
    "true",
    "union",
    "unique",
    "unpivot",
    "user",
    "using",
    "variadic",
    "when",
    "where",
    "window",
    "with",
];

/// Quote a SQL identifier only when required.
pub(crate) fn quote_ident(name: &str) -> Cow<'_, str> {
    if is_safe_sql_identifier(name) {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(format!("\"{}\"", name.replace('"', "\"\"")))
    }
}

fn is_safe_sql_identifier(name: &str) -> bool {
    if !is_simple_spark_identifier(name) {
        return false;
    }
    DUCKDB_RESERVED
        .binary_search_by(|reserved| ascii_ci_cmp(reserved.as_bytes(), name.as_bytes()))
        .is_err()
}

fn ascii_ci_cmp(left: &[u8], right: &[u8]) -> std::cmp::Ordering {
    for (left, right) in left.iter().zip(right) {
        match left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase()) {
            std::cmp::Ordering::Equal => {}
            ordering => return ordering,
        }
    }
    left.len().cmp(&right.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_preserves_quoted_dots_and_escaped_backticks() {
        assert_eq!(
            parse_multipart_identifier("catalog.`a.b`.`c``d`").expect("valid name"),
            ["catalog", "a.b", "c`d"]
        );
        assert!(parse_multipart_identifier("a..b").is_err());
        assert!(parse_multipart_identifier(".a").is_err());
        assert!(parse_multipart_identifier("a.").is_err());
        assert!(parse_multipart_identifier("`broken").is_err());
        assert_eq!(parse_multipart_identifier(""), Ok(vec![String::new()]));
        assert_eq!(parse_multipart_identifier("``"), Ok(vec![String::new()]));
        assert_eq!(
            parse_multipart_identifier("``.a"),
            Ok(vec![String::new(), "a".to_owned()])
        );
    }

    #[test]
    fn sql_parser_rejects_raw_empty_and_invalid_unquoted_parts() {
        assert_eq!(
            parse_sql_multipart_identifier(""),
            Err(SqlIdentifierError::ParseEmptyStatement)
        );
        for name in [
            "a b", "a-b", "a*b", "a/b", "a:b", "123", "12e3", "12D", "12L", "12S", "12Y", "12F",
            "12BD", "12e3f",
        ] {
            assert!(
                parse_sql_multipart_identifier(name).is_err(),
                "expected invalid SQL identifier: {name}"
            );
        }
    }

    #[test]
    fn sql_parser_preserves_spark_identifier_error_classes() {
        for name in [
            "`broken",
            "123",
            "12e3",
            "a:b",
            "a..b",
            "s3://bucket/table",
            "ab:// x",
            "ab://@ x",
            "ab://+ x",
            "orders -- ab://x\n junk",
            "orders /* ab://x */ junk",
            "/*x*/",
            "--x\n",
            "a\u{0085}.\u{0085}b",
            "a\u{2029}.\u{2029}b",
            "a\u{0661}",
            "a\u{2160}",
            "\u{2160}",
            "\u{05B0}",
            "\u{05B0}://x",
            "a://\u{05B0}",
        ] {
            assert_eq!(
                parse_sql_multipart_identifier(name),
                Err(SqlIdentifierError::ParseSyntax),
                "unexpected error class for {name}"
            );
        }
        for name in [
            "a-b", "a - b", "a-b-c", "a-b x", "a.b-c", "a.`b`-c", "a.b-c.d", "a.b.c-d", "Δelta",
            "Δ x", "a.Δ", "a.Δ.x",
        ] {
            assert_eq!(
                parse_sql_multipart_identifier(name),
                Err(SqlIdentifierError::InvalidIdentifier),
                "unexpected error class for {name}"
            );
        }
        for name in [
            "-a-b", "x a-b", ".a.b-c", "a..b-c", "x a.b-c", ".Δ", "x Δ", "a..Δ", "-Δ",
        ] {
            assert_eq!(
                parse_sql_multipart_identifier(name),
                Err(SqlIdentifierError::ParseSyntax),
                "unexpected error class for {name}"
            );
        }
        for name in [
            "x ab://y z",
            ".ab://x y",
            "a..ab://x y",
            "ab_://x y",
            "1a://x y",
            "ab1://x y",
        ] {
            assert_eq!(
                parse_sql_multipart_identifier(name),
                Err(SqlIdentifierError::ParseSyntax),
                "unexpected error class for {name}"
            );
        }
        for name in [
            "ab://x y",
            "a.ab://x y",
            "ab://x /*c*/ z",
            "Δ://x y",
            "ab://x",
            "ab://?",
            "a://x",
            "/* s3://x */ ab://y",
            "/* ab://x */ cd://y",
            "`s3://x`.ab://y",
            "-- s3://x\n ab://y",
        ] {
            assert_eq!(
                parse_sql_multipart_identifier(name),
                Err(SqlIdentifierError::InvalidIdentifier),
                "unexpected error class for {name}"
            );
        }
        assert_eq!(
            parse_sql_multipart_identifier("ab://x/identifier /*c*/ ('foo')"),
            Err(SqlIdentifierError::InvalidIdentifier)
        );
        assert_eq!(
            parse_sql_multipart_identifier("`Δelta`"),
            Ok(vec!["Δelta".to_owned()])
        );
    }

    #[test]
    fn sql_parser_accepts_quoted_empty_parts_and_leading_digits() {
        assert_eq!(
            parse_sql_multipart_identifier("``"),
            Ok(vec![String::new()])
        );
        assert_eq!(
            parse_sql_multipart_identifier("``.a"),
            Ok(vec![String::new(), "a".to_owned()])
        );
        assert_eq!(
            parse_sql_multipart_identifier("1a"),
            Ok(vec!["1a".to_owned()])
        );
        assert_eq!(
            parse_sql_multipart_identifier("1_"),
            Ok(vec!["1_".to_owned()])
        );
        assert_eq!(
            parse_sql_multipart_identifier("_1"),
            Ok(vec!["_1".to_owned()])
        );
        assert_eq!(
            parse_sql_multipart_identifier("select"),
            Ok(vec!["select".to_owned()])
        );
        assert_eq!(
            parse_sql_multipart_identifier("1e"),
            Ok(vec!["1e".to_owned()])
        );
        assert_eq!(
            parse_sql_multipart_identifier("1fx"),
            Ok(vec!["1fx".to_owned()])
        );
        assert_eq!(
            parse_sql_multipart_identifier("`a b`.`c-d`"),
            Ok(vec!["a b".to_owned(), "c-d".to_owned()])
        );
    }

    #[test]
    fn sql_parser_ignores_whitespace_and_comments_between_parts() {
        assert_eq!(
            parse_sql_multipart_identifier(" a "),
            Ok(vec!["a".to_owned()])
        );
        for name in ["a . b", "`a` . `b`", "a\n.\nb", "a/*x*/.b", "a --x\n . b"] {
            assert_eq!(
                parse_sql_multipart_identifier(name),
                Ok(vec!["a".to_owned(), "b".to_owned()]),
                "{name:?}"
            );
        }
        assert_eq!(
            parse_sql_multipart_identifier("/* text /*+ */ orders"),
            Ok(vec!["orders".to_owned()])
        );
        assert_eq!(
            parse_sql_multipart_identifier("orders -- x\\\n junk"),
            Ok(vec!["orders".to_owned()])
        );
        assert_eq!(
            parse_sql_multipart_identifier("-- x\\\n orders"),
            Err(SqlIdentifierError::ParseSyntax)
        );
    }

    #[test]
    fn suffix_matching_is_componentwise_and_case_folded() {
        let full = Qualifier::from_parts(vec!["Catalog".into(), "a.b".into()]);
        assert!(full.matches_suffix(&Qualifier::single("A.B")));
        assert!(full.matches_suffix(&Qualifier::from_parts(vec!["catalog".into(), "A.B".into()])));
        assert!(!full.matches_suffix(&Qualifier::from_parts(vec!["catalog.a".into(), "b".into()])));
        assert!(full.eq_case_folded(&Qualifier::from_parts(vec!["catalog".into(), "A.B".into()])));
        assert!(!full.eq_case_folded(&Qualifier::single("A.B")));
    }

    #[test]
    fn display_and_sql_render_each_part_independently() {
        let qualifier = Qualifier::from_parts(vec!["catalog".into(), "a.b".into()]);
        assert_eq!(qualifier.display_name(), "catalog.`a.b`");
        assert_eq!(qualifier.to_sql(), "catalog.\"a.b\"");
        assert_eq!(Qualifier::single("a.b").to_sql(), "\"a.b\"");
    }

    #[test]
    fn sql_reserved_words_are_sorted_lowercase_ascii() {
        assert!(DUCKDB_RESERVED.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(DUCKDB_RESERVED.iter().all(|word| {
            word.bytes()
                .all(|byte| byte.is_ascii() && !byte.is_ascii_uppercase())
        }));
    }

    #[test]
    fn quote_ident_preserves_safe_names_and_quotes_unsafe_names() {
        assert!(matches!(quote_ident("id"), Cow::Borrowed(_)));
        assert_eq!(quote_ident("select"), "\"select\"");
        assert_eq!(quote_ident("at"), "\"at\"");
        assert_eq!(quote_ident("AT"), "\"AT\"");
        assert_eq!(quote_ident("first name"), "\"first name\"");
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }
}
