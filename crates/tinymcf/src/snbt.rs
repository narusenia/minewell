//! SNBT: the textual NBT notation commands are written in.
//!
//! Parsing and formatting live together so that the round trip stays honest —
//! `parse(v.to_string()) == v` is the test that keeps tag information from leaking away.

use std::fmt;

use crate::nbt::{Compound, NbtValue};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnbtError {
    pub at: usize,
    pub message: String,
}

impl fmt::Display for SnbtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.at)
    }
}

impl std::error::Error for SnbtError {}

pub fn parse(src: &str) -> Result<NbtValue, SnbtError> {
    let mut p = Parser::new(src);
    let value = p.value()?;
    p.skip_ws();
    if p.rest().is_empty() {
        Ok(value)
    } else {
        Err(p.err("trailing input"))
    }
}

struct Parser<'a> {
    src: &'a str,
    at: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Parser { src, at: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.at..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.at += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn err(&self, message: &str) -> SnbtError {
        SnbtError {
            at: self.at,
            message: message.to_owned(),
        }
    }

    fn expect(&mut self, c: char) -> Result<(), SnbtError> {
        self.skip_ws();
        if self.eat(c) {
            Ok(())
        } else {
            Err(self.err(&format!("expected '{c}'")))
        }
    }

    fn value(&mut self) -> Result<NbtValue, SnbtError> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.compound().map(NbtValue::Compound),
            Some('[') => self.list(),
            Some('"') | Some('\'') => self.quoted().map(NbtValue::String),
            Some(_) => self.bare(),
            None => Err(self.err("unexpected end of input")),
        }
    }

    pub fn compound(&mut self) -> Result<Compound, SnbtError> {
        self.expect('{')?;
        let mut fields = Compound::new();
        self.skip_ws();
        if self.eat('}') {
            return Ok(fields);
        }
        loop {
            self.skip_ws();
            let key = match self.peek() {
                Some('"') | Some('\'') => self.quoted()?,
                _ => self.bare_word()?,
            };
            if key.is_empty() {
                return Err(self.err("empty key"));
            }
            self.expect(':')?;
            let value = self.value()?;
            fields.insert(key, value);
            self.skip_ws();
            if self.eat(',') {
                continue;
            }
            if self.eat('}') {
                return Ok(fields);
            }
            return Err(self.err("expected ',' or '}'"));
        }
    }

    fn list(&mut self) -> Result<NbtValue, SnbtError> {
        self.expect('[')?;
        // `[B;`, `[I;`, `[L;` introduce a typed array rather than a list.
        let prefix = {
            let mut chars = self.rest().chars();
            match (chars.next(), chars.next()) {
                (Some(t @ ('B' | 'I' | 'L')), Some(';')) => Some(t),
                _ => None,
            }
        };
        if let Some(tag) = prefix {
            self.at += 2;
            let items = self.items(']')?;
            return typed_array(tag, items).map_err(|m| self.err(&m));
        }
        Ok(NbtValue::List(self.items(']')?))
    }

    fn items(&mut self, close: char) -> Result<Vec<NbtValue>, SnbtError> {
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat(close) {
            return Ok(items);
        }
        loop {
            items.push(self.value()?);
            self.skip_ws();
            if self.eat(',') {
                continue;
            }
            if self.eat(close) {
                return Ok(items);
            }
            return Err(self.err(&format!("expected ',' or '{close}'")));
        }
    }

    fn quoted(&mut self) -> Result<String, SnbtError> {
        let quote = self.bump().ok_or_else(|| self.err("expected a string"))?;
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(self.err("unterminated string")),
                Some('\\') => match self.bump() {
                    Some(c @ ('\\' | '"' | '\'')) => out.push(c),
                    Some(c) => return Err(self.err(&format!("unknown escape '\\{c}'"))),
                    None => return Err(self.err("unterminated escape")),
                },
                Some(c) if c == quote => return Ok(out),
                Some(c) => out.push(c),
            }
        }
    }

    /// An unquoted run of the characters vanilla allows in a bare word.
    fn bare_word(&mut self) -> Result<String, SnbtError> {
        let start = self.at;
        while matches!(self.peek(), Some(c) if is_bare(c)) {
            self.at += 1;
        }
        if self.at == start {
            return Err(self.err("expected a value"));
        }
        Ok(self.src[start..self.at].to_owned())
    }

    fn bare(&mut self) -> Result<NbtValue, SnbtError> {
        let word = self.bare_word()?;
        Ok(scalar(&word))
    }
}

fn is_bare(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+')
}

/// A bare word is a number if it parses as one, and a string otherwise. The trailing
/// type suffix only counts when what precedes it is numeric, so `b` stays a string.
fn scalar(word: &str) -> NbtValue {
    match word {
        "true" => return NbtValue::Byte(1),
        "false" => return NbtValue::Byte(0),
        _ => {}
    }
    let (body, suffix) = word.split_at(word.len() - 1);
    let typed = match suffix {
        "b" | "B" => body.parse().ok().map(NbtValue::Byte),
        "s" | "S" => body.parse().ok().map(NbtValue::Short),
        "l" | "L" => body.parse().ok().map(NbtValue::Long),
        "f" | "F" => body.parse().ok().map(NbtValue::Float),
        "d" | "D" => body.parse().ok().map(NbtValue::Double),
        _ => None,
    };
    if let Some(v) = typed {
        return v;
    }
    if let Ok(i) = word.parse::<i32>() {
        return NbtValue::Int(i);
    }
    if word.contains(['.', 'e', 'E'])
        && let Ok(d) = word.parse::<f64>()
    {
        return NbtValue::Double(d);
    }
    NbtValue::String(word.to_owned())
}

fn typed_array(tag: char, items: Vec<NbtValue>) -> Result<NbtValue, String> {
    let mismatch = |v: &NbtValue| format!("expected {tag} array element, found {}", v.tag_name());
    match tag {
        'B' => items
            .iter()
            .map(|v| match v {
                NbtValue::Byte(b) => Ok(*b),
                other => Err(mismatch(other)),
            })
            .collect::<Result<_, _>>()
            .map(NbtValue::ByteArray),
        'I' => items
            .iter()
            .map(|v| match v {
                NbtValue::Int(i) => Ok(*i),
                other => Err(mismatch(other)),
            })
            .collect::<Result<_, _>>()
            .map(NbtValue::IntArray),
        _ => items
            .iter()
            .map(|v| match v {
                NbtValue::Long(l) => Ok(*l),
                other => Err(mismatch(other)),
            })
            .collect::<Result<_, _>>()
            .map(NbtValue::LongArray),
    }
}

impl fmt::Display for NbtValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NbtValue::Byte(v) => write!(f, "{v}b"),
            NbtValue::Short(v) => write!(f, "{v}s"),
            NbtValue::Int(v) => write!(f, "{v}"),
            NbtValue::Long(v) => write!(f, "{v}L"),
            NbtValue::Float(v) => write!(f, "{}f", decimal(*v as f64)),
            NbtValue::Double(v) => write!(f, "{}d", decimal(*v)),
            NbtValue::String(v) => write!(f, "{}", quote(v)),
            NbtValue::List(items) => write_seq(f, "", items.iter()),
            NbtValue::Compound(fields) => {
                f.write_str("{")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{}:{v}", key(k))?;
                }
                f.write_str("}")
            }
            NbtValue::ByteArray(v) => write_seq(f, "B;", v.iter().map(|b| NbtValue::Byte(*b))),
            NbtValue::IntArray(v) => write_seq(f, "I;", v.iter().map(|i| NbtValue::Int(*i))),
            NbtValue::LongArray(v) => write_seq(f, "L;", v.iter().map(|l| NbtValue::Long(*l))),
        }
    }
}

fn write_seq<T: std::borrow::Borrow<NbtValue>>(
    f: &mut fmt::Formatter<'_>,
    prefix: &str,
    items: impl Iterator<Item = T>,
) -> fmt::Result {
    f.write_str("[")?;
    f.write_str(prefix)?;
    for (i, item) in items.enumerate() {
        if i > 0 {
            f.write_str(",")?;
        }
        write!(f, "{}", item.borrow())?;
    }
    f.write_str("]")
}

/// Vanilla always prints a decimal point, so `20f` formats back as `20.0f`.
fn decimal(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

fn quote(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Keys are left bare when they can be, matching how vanilla prints them.
fn key(k: &str) -> String {
    if !k.is_empty() && k.chars().all(is_bare) {
        k.to_owned()
    } else {
        quote(k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nbt::NbtValue::*;

    fn p(s: &str) -> NbtValue {
        parse(s).unwrap()
    }

    #[test]
    fn number_suffixes_pick_the_tag() {
        assert_eq!(p("1b"), Byte(1));
        assert_eq!(p("1s"), Short(1));
        assert_eq!(p("1"), Int(1));
        assert_eq!(p("1L"), Long(1));
        assert_eq!(p("1.5f"), Float(1.5));
        assert_eq!(p("1.5"), Double(1.5));
        assert_eq!(p("1.5d"), Double(1.5));
        assert_eq!(p("-3"), Int(-3));
    }

    #[test]
    fn booleans_are_bytes() {
        assert_eq!(p("true"), Byte(1));
        assert_eq!(p("false"), Byte(0));
    }

    #[test]
    fn a_bare_word_that_is_not_a_number_is_a_string() {
        assert_eq!(p("stone"), String("stone".into()));
        assert_eq!(p("b"), String("b".into()));
        // ':' is not a bare-word character in SNBT, so resource locations must be quoted.
        assert!(parse("minecraft:stone").is_err());
        assert_eq!(p(r#""minecraft:stone""#), String("minecraft:stone".into()));
    }

    #[test]
    fn quoted_strings_handle_escapes() {
        assert_eq!(p(r#""a\"b""#), String("a\"b".into()));
        assert_eq!(p(r"'a\'b'"), String("a'b".into()));
        assert_eq!(p(r#""a\\b""#), String("a\\b".into()));
    }

    #[test]
    fn compounds_and_lists_nest() {
        assert_eq!(
            p(r#"{Health: 20f, Tags: ["a", "b"], Pos: {x: 1}}"#),
            NbtValue::compound([
                ("Health", Float(20.0)),
                ("Tags", List(vec![String("a".into()), String("b".into())])),
                ("Pos", NbtValue::compound([("x", Int(1))])),
            ])
        );
    }

    #[test]
    fn typed_arrays_are_distinct_from_lists() {
        assert_eq!(p("[B;1b,2b]"), ByteArray(vec![1, 2]));
        assert_eq!(p("[I;1,2]"), IntArray(vec![1, 2]));
        assert_eq!(p("[L;1L,2L]"), LongArray(vec![1, 2]));
        assert_ne!(p("[I;1,2]"), p("[1,2]"));
    }

    #[test]
    fn empty_containers() {
        assert_eq!(p("{}"), Compound(Default::default()));
        assert_eq!(p("[]"), List(vec![]));
    }

    #[test]
    fn trailing_input_is_rejected() {
        assert!(parse("{} junk").is_err());
        assert!(parse("{a:1").is_err());
    }

    #[test]
    fn display_round_trips_through_parse() {
        // Fields come back in key order: `Compound` is a `BTreeMap` so that output
        // is deterministic.
        for src in [
            r#"{Health:20.0f,Pos:{x:1},Tags:["a","b"]}"#,
            "[B;1b,2b]",
            "[I;1,2]",
            "[L;1L,2L]",
            r#"{"needs quoting":1b}"#,
            "[]",
            "{}",
        ] {
            let v = p(src);
            assert_eq!(v.to_string(), src, "formatting {src}");
            assert_eq!(p(&v.to_string()), v, "round trip of {src}");
        }
    }
}
