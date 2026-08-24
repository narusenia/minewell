//! Splitting a command line into arguments.
//!
//! Vanilla's grammar is positional and mostly whitespace-separated, but three kinds of
//! argument carry their own whitespace: selectors (`@e[type=zombie, distance=..8]`),
//! SNBT (`{Health: 20f}`) and quoted strings. One balanced scanner handles all three,
//! so every command parser can just ask for "the next word".

use crate::nbt::NbtValue;
use crate::path::NbtPath;
use crate::snbt::{self, SnbtError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub at: usize,
    pub message: String,
}

impl ParseError {
    fn new(at: usize, message: impl Into<String>) -> Self {
        ParseError {
            at,
            message: message.into(),
        }
    }
}

impl From<SnbtError> for ParseError {
    fn from(e: SnbtError) -> Self {
        ParseError::new(e.at, e.message)
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.at)
    }
}

impl std::error::Error for ParseError {}

pub struct Args<'a> {
    src: &'a str,
    at: usize,
}

impl<'a> Args<'a> {
    pub fn new(src: &'a str) -> Self {
        Args { src, at: 0 }
    }

    pub fn is_empty(&mut self) -> bool {
        self.skip_ws();
        self.at >= self.src.len()
    }

    /// The next word, without consuming it.
    pub fn peek(&mut self) -> Option<&'a str> {
        let save = self.at;
        let word = self.word().ok();
        self.at = save;
        word
    }

    pub fn word(&mut self) -> Result<&'a str, ParseError> {
        self.skip_ws();
        let start = self.at;
        if start >= self.src.len() {
            return Err(ParseError::new(start, "expected an argument"));
        }
        self.at = scan(self.src, start)?;
        Ok(&self.src[start..self.at])
    }

    /// Consumes the next word only when it is exactly `expected`.
    pub fn literal(&mut self, expected: &str) -> bool {
        if self.peek() == Some(expected) {
            let _ = self.word();
            true
        } else {
            false
        }
    }

    pub fn int(&mut self) -> Result<i32, ParseError> {
        let at = {
            self.skip_ws();
            self.at
        };
        let word = self.word()?;
        word.parse()
            .map_err(|_| ParseError::new(at, format!("expected an integer, found '{word}'")))
    }

    /// Everything left, trimmed. Vanilla's greedy string argument.
    pub fn rest(&mut self) -> &'a str {
        let rest = self.src[self.at..].trim();
        self.at = self.src.len();
        rest
    }

    pub fn path(&mut self) -> Result<NbtPath, ParseError> {
        let at = {
            self.skip_ws();
            self.at
        };
        let word = self.word()?;
        NbtPath::parse(word).map_err(|e| ParseError::new(at + e.at, e.message))
    }

    pub fn value(&mut self) -> Result<NbtValue, ParseError> {
        self.skip_ws();
        let (value, next) = snbt::parse_value_at(self.src, self.at)?;
        self.at = next;
        Ok(value)
    }

    /// Asserts nothing is left. Vanilla rejects trailing arguments too.
    pub fn end(&mut self) -> Result<(), ParseError> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(ParseError::new(self.at, "unexpected trailing argument"))
        }
    }

    fn skip_ws(&mut self) {
        while self.src[self.at..].starts_with(|c: char| c.is_ascii_whitespace()) {
            self.at += 1;
        }
    }
}

/// Finds the end of the argument starting at `start`: whitespace ends it, unless the
/// whitespace sits inside brackets, braces or quotes.
fn scan(src: &str, start: usize) -> Result<usize, ParseError> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut chars = src[start..].char_indices();
    while let Some((i, c)) = chars.next() {
        let at = start + i;
        if let Some(q) = quote {
            match c {
                '\\' => {
                    chars.next();
                }
                _ if c == q => quote = None,
                _ => {}
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '[' | '{' => depth += 1,
            ']' | '}' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| ParseError::new(at, format!("unmatched '{c}'")))?;
            }
            _ if c.is_ascii_whitespace() && depth == 0 => return Ok(at),
            _ => {}
        }
    }
    if depth > 0 {
        return Err(ParseError::new(start, "unclosed bracket"));
    }
    if quote.is_some() {
        return Err(ParseError::new(start, "unterminated string"));
    }
    Ok(src.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nbt::NbtValue;

    #[test]
    fn words_split_on_whitespace() {
        let mut a = Args::new("scoreboard players set");
        assert_eq!(a.word().unwrap(), "scoreboard");
        assert_eq!(a.word().unwrap(), "players");
        assert_eq!(a.word().unwrap(), "set");
        assert!(a.is_empty());
        assert!(a.word().is_err());
    }

    #[test]
    fn a_selector_is_one_word_even_with_spaces_inside_brackets() {
        let mut a = Args::new("@e[type=zombie, distance=..8] extra");
        assert_eq!(a.word().unwrap(), "@e[type=zombie, distance=..8]");
        assert_eq!(a.word().unwrap(), "extra");
    }

    #[test]
    fn a_quoted_argument_is_one_word() {
        let mut a = Args::new(r#""a b" c"#);
        assert_eq!(a.word().unwrap(), r#""a b""#);
        assert_eq!(a.word().unwrap(), "c");
    }

    #[test]
    fn braces_hold_a_word_together() {
        let mut a = Args::new("{Health: 20f} tail");
        assert_eq!(a.word().unwrap(), "{Health: 20f}");
        assert_eq!(a.word().unwrap(), "tail");
    }

    #[test]
    fn rest_is_greedy_and_trimmed() {
        let mut a = Args::new("say  hi   there  ");
        assert_eq!(a.word().unwrap(), "say");
        assert_eq!(a.rest(), "hi   there");
        assert!(a.is_empty());
    }

    #[test]
    fn literal_consumes_only_on_a_match() {
        let mut a = Args::new("set value");
        assert!(!a.literal("get"));
        assert!(a.literal("set"));
        assert_eq!(a.word().unwrap(), "value");
    }

    #[test]
    fn integers_and_their_failure() {
        let mut a = Args::new("-7 nope");
        assert_eq!(a.int().unwrap(), -7);
        assert!(a.int().is_err());
    }

    #[test]
    fn paths_and_snbt_values_come_back_parsed() {
        let mut a = Args::new(r#"a.b[0] {k: 1, s: "x y"}"#);
        assert_eq!(a.path().unwrap(), NbtPath::parse("a.b[0]").unwrap());
        assert_eq!(
            a.value().unwrap(),
            NbtValue::compound([
                ("k", NbtValue::Int(1)),
                ("s", NbtValue::String("x y".into())),
            ])
        );
    }

    #[test]
    fn end_rejects_leftovers() {
        let mut a = Args::new("one two");
        assert_eq!(a.word().unwrap(), "one");
        assert!(a.end().is_err());
        assert_eq!(a.word().unwrap(), "two");
        assert!(a.end().is_ok());
    }

    #[test]
    fn an_unterminated_bracket_is_an_error() {
        let mut a = Args::new("@e[type=zombie");
        assert!(a.word().is_err());
    }
}
