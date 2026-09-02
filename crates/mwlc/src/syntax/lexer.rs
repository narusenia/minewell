//! Turning `.mwl` text into tokens.
//!
//! Follows `docs/02-spec.md` section 2. Two parts of that specification shape this
//! code more than the rest: a selector is a single token whose body is kept verbatim,
//! and `ident:ident` with no surrounding space is always a resource location — even
//! when the author meant a type annotation.
//!
//! Lexing never stops at the first problem. Errors are collected and the scan
//! continues, so one stray character does not hide the rest of the file.

use super::SyntaxError;

/// A byte range in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn range(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Ident(String),
    Keyword(Keyword),
    /// A word held back for a feature v1 does not have. The parser rejects it, where
    /// there is room to say why.
    Reserved(String),
    Int(i32),
    Str(String),
    /// `@e[...]`, body verbatim. Parsed in M5.
    Selector(String),
    /// `minecraft:stone`.
    Resource(String),
    Punct(Punct),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    Fn,
    Let,
    Mut,
    Const,
    If,
    Else,
    Match,
    While,
    Loop,
    For,
    In,
    Break,
    Continue,
    Return,
    As,
    At,
    /// `positioned pos!(~ ~1 ~) { .. }`: the execution position, without an entity.
    Positioned,
    Struct,
    Enum,
    Impl,
    /// The receiver of a method, in `impl`.
    SelfValue,
    Mod,
    Use,
    Pub,
    True,
    False,
}

impl Keyword {
    fn parse(word: &str) -> Option<Keyword> {
        Some(match word {
            "fn" => Keyword::Fn,
            "let" => Keyword::Let,
            "mut" => Keyword::Mut,
            "const" => Keyword::Const,
            "if" => Keyword::If,
            "else" => Keyword::Else,
            "match" => Keyword::Match,
            "while" => Keyword::While,
            "loop" => Keyword::Loop,
            "for" => Keyword::For,
            "in" => Keyword::In,
            "break" => Keyword::Break,
            "continue" => Keyword::Continue,
            "return" => Keyword::Return,
            "as" => Keyword::As,
            "positioned" => Keyword::Positioned,
            "at" => Keyword::At,
            "struct" => Keyword::Struct,
            "enum" => Keyword::Enum,
            "impl" => Keyword::Impl,
            "self" => Keyword::SelfValue,
            "mod" => Keyword::Mod,
            "use" => Keyword::Use,
            "pub" => Keyword::Pub,
            "true" => Keyword::True,
            "false" => Keyword::False,
            _ => return None,
        })
    }
}

/// Words held back so that adding the feature later cannot break existing code.
/// See `docs/01-requirements.md` section 19.
const RESERVED: &[&str] = &[
    "async",
    "await",
    "trait",
    "dyn",
    "macro_rules",
    "Self",
    "where",
    "unsafe",
    "static",
    "type",
    "ref",
    "move",
    "box",
    "yield",
    "do",
    "try",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Punct {
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
    Bang,
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    And,
    Dot,
    DotDot,
    DotDotEq,
    Comma,
    Semi,
    Colon,
    ColonColon,
    Arrow,
    FatArrow,
    Hash,
    Tilde,
    Caret,
    Question,
    Underscore,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
}

/// Longest match first, so `..=` beats `..` beats `.`.
const PUNCTUATION: &[(&str, Punct)] = &[
    ("..=", Punct::DotDotEq),
    ("::", Punct::ColonColon),
    ("->", Punct::Arrow),
    ("=>", Punct::FatArrow),
    ("==", Punct::EqEq),
    ("!=", Punct::Ne),
    ("<=", Punct::Le),
    (">=", Punct::Ge),
    ("&&", Punct::AndAnd),
    ("||", Punct::OrOr),
    ("+=", Punct::PlusEq),
    ("-=", Punct::MinusEq),
    ("*=", Punct::StarEq),
    ("/=", Punct::SlashEq),
    ("%=", Punct::PercentEq),
    ("..", Punct::DotDot),
    ("+", Punct::Plus),
    ("-", Punct::Minus),
    ("*", Punct::Star),
    ("/", Punct::Slash),
    ("%", Punct::Percent),
    ("<", Punct::Lt),
    (">", Punct::Gt),
    ("!", Punct::Bang),
    ("=", Punct::Eq),
    ("&", Punct::And),
    (".", Punct::Dot),
    (",", Punct::Comma),
    (";", Punct::Semi),
    (":", Punct::Colon),
    ("#", Punct::Hash),
    ("~", Punct::Tilde),
    ("^", Punct::Caret),
    ("?", Punct::Question),
    ("(", Punct::LParen),
    (")", Punct::RParen),
    ("[", Punct::LBracket),
    ("]", Punct::RBracket),
    ("{", Punct::LBrace),
    ("}", Punct::RBrace),
];

pub fn lex(src: &str) -> (Vec<Token>, Vec<SyntaxError>) {
    Lexer {
        src,
        at: src.strip_prefix('\u{feff}').map_or(0, |_| 3),
        tokens: Vec::new(),
        errors: Vec::new(),
    }
    .run()
}

struct Lexer<'a> {
    src: &'a str,
    at: usize,
    tokens: Vec<Token>,
    errors: Vec<SyntaxError>,
}

impl<'a> Lexer<'a> {
    fn run(mut self) -> (Vec<Token>, Vec<SyntaxError>) {
        while self.skip_trivia() {
            let start = self.at;
            match self.token(start) {
                Some(kind) => {
                    let span = Span {
                        start,
                        end: self.at,
                    };
                    self.tokens.push(Token { kind, span });
                }
                None => {
                    // `token` recorded the problem and advanced past it.
                    debug_assert!(self.at > start, "lexer must make progress");
                }
            }
        }
        (self.tokens, self.errors)
    }

    fn token(&mut self, start: usize) -> Option<TokenKind> {
        let c = self.peek()?;
        if c == '@' {
            return self.selector(start);
        }
        if c == '"' {
            return self.string(start);
        }
        if c.is_ascii_digit() {
            return self.number(start);
        }
        if c == '_' && !self.at(1).is_some_and(is_ident_continue) {
            self.at += 1;
            return Some(TokenKind::Punct(Punct::Underscore));
        }
        if is_ident_start(c) {
            return Some(self.word(start));
        }
        for (text, punct) in PUNCTUATION {
            if self.rest().starts_with(text) {
                self.at += text.len();
                return Some(TokenKind::Punct(*punct));
            }
        }
        self.at += c.len_utf8();
        self.error(start, format!("unexpected character '{c}'"));
        None
    }

    /// An identifier, keyword, reserved word — or a resource location, when a single
    /// `:` follows immediately (spec section 2.8).
    fn word(&mut self, start: usize) -> TokenKind {
        self.take_while(is_ident_continue);
        if self.resource_follows() {
            self.take_resource();
            return TokenKind::Resource(self.src[start..self.at].to_owned());
        }
        let word = &self.src[start..self.at];
        if let Some(keyword) = Keyword::parse(word) {
            return TokenKind::Keyword(keyword);
        }
        if RESERVED.contains(&word) {
            return TokenKind::Reserved(word.to_owned());
        }
        TokenKind::Ident(word.to_owned())
    }

    /// A path segment run (`a/b/c`) may precede the colon, and must follow it.
    fn resource_follows(&self) -> bool {
        let mut at = self.at;
        while self.src[at..].starts_with('/') {
            let after = at + 1;
            let end = self.run_of(after, is_ident_continue);
            if end == after {
                return false;
            }
            at = end;
        }
        if !self.src[at..].starts_with(':') || self.src[at + 1..].starts_with(':') {
            return false;
        }
        let after = at + 1;
        self.run_of(after, is_ident_continue) > after
    }

    fn take_resource(&mut self) {
        while self.rest().starts_with('/') {
            self.at += 1;
            self.take_while(is_ident_continue);
        }
        self.at += 1; // ':'
        // The path may hold dots and dashes: vanilla ids are built that way, and
        // `minecraft:block.note_block.pling` is one id, not a field access.
        self.take_while(is_resource_path);
        while self.rest().starts_with('/') {
            self.at += 1;
            self.take_while(is_resource_path);
        }
        // `minecraft:chest[facing=north]` is one token, the way a selector is
        // (spec section 2.8). A resource location is never indexed, so a `[` here
        // cannot be anything else.
        if self.rest().starts_with('[') {
            self.balanced();
        }
    }

    fn selector(&mut self, start: usize) -> Option<TokenKind> {
        self.at += 1;
        match self.peek() {
            Some(c @ ('a' | 'e' | 'p' | 'r' | 's')) => {
                let _ = c;
                self.at += 1;
            }
            _ => {
                self.error(start, "expected one of @a @e @p @r @s".to_owned());
                return None;
            }
        }
        if self.rest().starts_with('[') && !self.balanced() {
            self.error(start, "unterminated selector".to_owned());
            return None;
        }
        Some(TokenKind::Selector(self.src[start..self.at].to_owned()))
    }

    /// Consumes a bracketed group, honouring nesting and quoted strings. Returns false
    /// if it never closes, having consumed to the end.
    fn balanced(&mut self) -> bool {
        let mut depth = 0usize;
        let mut quote: Option<char> = None;
        while let Some(c) = self.peek() {
            self.at += c.len_utf8();
            match (quote, c) {
                (Some(_), '\\') => {
                    if let Some(next) = self.peek() {
                        self.at += next.len_utf8();
                    }
                }
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"' | '\'') => quote = Some(c),
                (None, '[' | '{') => depth += 1,
                (None, ']' | '}') => {
                    depth -= 1;
                    if depth == 0 {
                        return true;
                    }
                }
                (None, _) => {}
            }
        }
        false
    }

    fn number(&mut self, start: usize) -> Option<TokenKind> {
        let (radix, digits_from) = match self.rest().get(..2) {
            Some("0x" | "0X") => (16, start + 2),
            Some("0b" | "0B") => (2, start + 2),
            _ => (10, start),
        };
        self.at = digits_from;
        self.take_while(|c| c.is_ascii_alphanumeric() || c == '_');
        let text: String = self.src[digits_from..self.at]
            .chars()
            .filter(|c| *c != '_')
            .collect();
        if text.is_empty() {
            self.error(start, "expected digits".to_owned());
            return None;
        }
        match i64::from_str_radix(&text, radix) {
            Ok(value) if i32::try_from(value).is_ok() => Some(TokenKind::Int(value as i32)),
            Ok(_) => {
                self.error(start, "integer literal does not fit in i32".to_owned());
                None
            }
            Err(_) => {
                self.error(start, format!("'{text}' is not a base {radix} integer"));
                None
            }
        }
    }

    fn string(&mut self, start: usize) -> Option<TokenKind> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                self.error(start, "unterminated string".to_owned());
                return None;
            };
            self.at += c.len_utf8();
            match c {
                '"' => return Some(TokenKind::Str(out)),
                '\\' => {
                    let at = self.at;
                    let escaped = self.peek();
                    if let Some(e) = escaped {
                        self.at += e.len_utf8();
                    }
                    out.push(match escaped {
                        Some('\\') => '\\',
                        Some('"') => '"',
                        Some('n') => '\n',
                        Some('t') => '\t',
                        Some('r') => '\r',
                        Some('0') => '\0',
                        Some(other) => {
                            self.error(at, format!("unknown escape '\\{other}'"));
                            continue;
                        }
                        None => {
                            self.error(start, "unterminated string".to_owned());
                            return None;
                        }
                    });
                }
                _ => out.push(c),
            }
        }
    }

    /// Skips whitespace and comments. Returns false at end of input.
    fn skip_trivia(&mut self) -> bool {
        loop {
            let before = self.at;
            self.take_while(|c| c.is_ascii_whitespace());
            if self.rest().starts_with("//") {
                self.take_while(|c| c != '\n');
            } else if self.rest().starts_with("/*") {
                self.block_comment();
            }
            if self.at == before {
                return self.at < self.src.len();
            }
        }
    }

    fn block_comment(&mut self) {
        let start = self.at;
        let mut depth = 0usize;
        loop {
            if self.rest().starts_with("/*") {
                self.at += 2;
                depth += 1;
            } else if self.rest().starts_with("*/") {
                self.at += 2;
                depth -= 1;
                if depth == 0 {
                    return;
                }
            } else {
                match self.peek() {
                    Some(c) => self.at += c.len_utf8(),
                    None => {
                        self.error(start, "unterminated block comment".to_owned());
                        return;
                    }
                }
            }
        }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.at..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn at(&self, offset: usize) -> Option<char> {
        self.rest().chars().nth(offset)
    }

    fn take_while(&mut self, mut pred: impl FnMut(char) -> bool) {
        while let Some(c) = self.peek() {
            if !pred(c) {
                return;
            }
            self.at += c.len_utf8();
        }
    }

    fn run_of(&self, from: usize, mut pred: impl FnMut(char) -> bool) -> usize {
        let mut at = from;
        for c in self.src[from..].chars() {
            if !pred(c) {
                break;
            }
            at += c.len_utf8();
        }
        at
    }

    fn error(&mut self, start: usize, message: String) {
        self.errors.push(SyntaxError::new(
            Span {
                start,
                end: self.at,
            },
            message,
        ));
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

/// The characters a resource location's path may hold (spec section 2.8).
fn is_resource_path(c: char) -> bool {
    is_ident_continue(c) || c == '.' || c == '-'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

// SPDX-License-Identifier: MIT

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let (tokens, errors) = lex(src);
        assert!(errors.is_empty(), "{errors:?}");
        tokens.into_iter().map(|t| t.kind).collect()
    }

    fn errors(src: &str) -> Vec<SyntaxError> {
        lex(src).1
    }

    fn ident(name: &str) -> TokenKind {
        TokenKind::Ident(name.to_owned())
    }

    #[test]
    fn spans_point_at_the_source_bytes() {
        let src = "fn  main";
        let (tokens, _) = lex(src);
        assert_eq!(&src[tokens[0].span.range()], "fn");
        assert_eq!(&src[tokens[1].span.range()], "main");
    }

    #[test]
    fn spans_survive_multibyte_characters() {
        let src = r#""日本語" x"#;
        let (tokens, _) = lex(src);
        assert_eq!(&src[tokens[1].span.range()], "x");
    }

    #[test]
    fn keywords_are_distinct_from_identifiers() {
        assert_eq!(
            kinds("fn value let"),
            vec![
                TokenKind::Keyword(Keyword::Fn),
                ident("value"),
                TokenKind::Keyword(Keyword::Let),
            ]
        );
    }

    #[test]
    fn reserved_words_are_their_own_kind() {
        // Not an error here: the parser reports it, where there is room for a message
        // about why the word is off limits.
        assert_eq!(
            kinds("async"),
            vec![TokenKind::Reserved("async".to_owned())]
        );
    }

    #[test]
    fn integer_literals_in_every_base() {
        assert_eq!(
            kinds("7 0xFF 0b1010 1_000"),
            vec![
                TokenKind::Int(7),
                TokenKind::Int(255),
                TokenKind::Int(10),
                TokenKind::Int(1000),
            ]
        );
    }

    #[test]
    fn an_integer_too_large_for_i32_is_an_error() {
        let errors = errors("2147483648");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("i32"), "{errors:?}");
    }

    #[test]
    fn there_are_no_float_literals() {
        // `1.5` is `1`, `..`-less `.`, `5` — the parser will reject it. Reals are
        // `fix<S>`, and a bare decimal has no scale to be.
        assert_eq!(
            kinds("1.5"),
            vec![
                TokenKind::Int(1),
                TokenKind::Punct(Punct::Dot),
                TokenKind::Int(5),
            ]
        );
    }

    #[test]
    fn strings_and_their_escapes() {
        assert_eq!(
            kinds(r#""a\nb\"c\\d""#),
            vec![TokenKind::Str("a\nb\"c\\d".to_owned())]
        );
        assert!(!errors(r#""unterminated"#).is_empty());
        assert!(!errors(r#""bad \q escape""#).is_empty());
    }

    #[test]
    fn block_comments_nest() {
        assert_eq!(
            kinds("a /* one /* two */ still */ b"),
            vec![ident("a"), ident("b")]
        );
        assert!(!errors("/* unterminated").is_empty());
    }

    #[test]
    fn line_comments_end_at_the_newline() {
        assert_eq!(kinds("a // b\nc"), vec![ident("a"), ident("c")]);
    }

    #[test]
    fn a_selector_is_one_token_even_with_spaces_inside_brackets() {
        assert_eq!(
            kinds("@e[type=zombie, distance=..8]"),
            vec![TokenKind::Selector(
                "@e[type=zombie, distance=..8]".to_owned()
            )]
        );
        assert_eq!(kinds("@s"), vec![TokenKind::Selector("@s".to_owned())]);
    }

    #[test]
    fn a_selector_keeps_nested_brackets_and_quotes_balanced() {
        let src = r#"@e[nbt={Tags:["a]b"]}]"#;
        assert_eq!(kinds(src), vec![TokenKind::Selector(src.to_owned())]);
    }

    #[test]
    fn a_resource_path_may_hold_dots_and_dashes() {
        assert_eq!(
            kinds("minecraft:block.note_block.pling"),
            vec![TokenKind::Resource(
                "minecraft:block.note_block.pling".to_owned()
            )]
        );
        assert_eq!(
            kinds("ns:some-thing/deep.id"),
            vec![TokenKind::Resource("ns:some-thing/deep.id".to_owned())]
        );
    }

    #[test]
    fn resource_locations_are_one_token() {
        assert_eq!(
            kinds("minecraft:stone"),
            vec![TokenKind::Resource("minecraft:stone".to_owned())]
        );
        assert_eq!(
            kinds("ns:foo/bar"),
            vec![TokenKind::Resource("ns:foo/bar".to_owned())]
        );
    }

    #[test]
    fn a_double_colon_is_a_path_separator_not_a_resource() {
        assert_eq!(
            kinds("foo::bar"),
            vec![
                ident("foo"),
                TokenKind::Punct(Punct::ColonColon),
                ident("bar")
            ]
        );
    }

    #[test]
    fn a_type_annotation_without_a_space_lexes_as_a_resource() {
        // The sharp edge spelled out in spec section 2.8. The parser turns this into
        // "a type annotation needs a space after the colon", which is why the lexer
        // does not try to be clever about it.
        assert_eq!(
            kinds("let x:i32"),
            vec![
                TokenKind::Keyword(Keyword::Let),
                TokenKind::Resource("x:i32".to_owned()),
            ]
        );
        assert_eq!(
            kinds("let x: i32"),
            vec![
                TokenKind::Keyword(Keyword::Let),
                ident("x"),
                TokenKind::Punct(Punct::Colon),
                ident("i32"),
            ]
        );
    }

    #[test]
    fn punctuation_prefers_the_longest_match() {
        assert_eq!(
            kinds("..= .. . == = -> => && & <= <"),
            vec![
                TokenKind::Punct(Punct::DotDotEq),
                TokenKind::Punct(Punct::DotDot),
                TokenKind::Punct(Punct::Dot),
                TokenKind::Punct(Punct::EqEq),
                TokenKind::Punct(Punct::Eq),
                TokenKind::Punct(Punct::Arrow),
                TokenKind::Punct(Punct::FatArrow),
                TokenKind::Punct(Punct::AndAnd),
                TokenKind::Punct(Punct::And),
                TokenKind::Punct(Punct::Le),
                TokenKind::Punct(Punct::Lt),
            ]
        );
    }

    #[test]
    fn a_macro_call_is_an_identifier_a_bang_and_a_group() {
        assert_eq!(
            kinds(r#"raw!("say hi")"#),
            vec![
                ident("raw"),
                TokenKind::Punct(Punct::Bang),
                TokenKind::Punct(Punct::LParen),
                TokenKind::Str("say hi".to_owned()),
                TokenKind::Punct(Punct::RParen),
            ]
        );
    }

    #[test]
    fn an_unknown_character_is_reported_and_lexing_carries_on() {
        let (tokens, errors) = lex("a $ b");
        assert_eq!(errors.len(), 1);
        assert_eq!(
            tokens.into_iter().map(|t| t.kind).collect::<Vec<_>>(),
            vec![ident("a"), ident("b")]
        );
    }
}
