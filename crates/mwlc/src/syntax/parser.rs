//! Tokens to AST.
//!
//! Follows `docs/02-spec.md` section 3. Parsing collects errors and recovers rather
//! than stopping: at item level it skips to the next thing that could begin an item,
//! and inside a block it skips to the end of the offending statement. One mistake
//! should cost one diagnostic, not the rest of the file.

use super::SyntaxError;
use super::ast::*;
use super::lexer::{Keyword, Punct, Span, Token, TokenKind, lex};

pub fn parse(src: &str) -> (SourceFile, Vec<SyntaxError>) {
    let (tokens, errors) = lex(src);
    let mut parser = Parser {
        tokens,
        at: 0,
        errors,
        end: src.len(),
    };
    let items = parser.items();
    (SourceFile { items }, parser.errors)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    errors: Vec<SyntaxError>,
    end: usize,
}

impl Parser {
    fn items(&mut self) -> Vec<Item> {
        let mut items = Vec::new();
        while self.peek().is_some() {
            match self.item() {
                Some(item) => items.push(item),
                None => self.recover_to_item(),
            }
        }
        items
    }

    fn item(&mut self) -> Option<Item> {
        let start = self.span().start;
        let attrs = self.attributes();
        match self.peek() {
            Some(TokenKind::Keyword(Keyword::Fn)) => {}
            Some(TokenKind::Reserved(word)) => {
                let word = word.clone();
                self.error(format!(
                    "'{word}' is reserved for a future version of minewell and cannot be used"
                ));
                return None;
            }
            _ => {
                self.error("expected an item");
                return None;
            }
        }
        self.bump();

        let name = self.ident()?;
        self.expect(Punct::LParen, "(")?;
        self.expect(Punct::RParen, ")")?;
        let body = self.block()?;
        let span = Span {
            start,
            end: body.span.end,
        };
        Some(Item {
            attrs,
            kind: ItemKind::Fn(FnItem { name, body }),
            span,
        })
    }

    fn attributes(&mut self) -> Vec<Attribute> {
        let mut attrs = Vec::new();
        while self.peek() == Some(&TokenKind::Punct(Punct::Hash)) {
            let start = self.span().start;
            self.bump();
            let Some(tokens) = self.group(Punct::LBracket) else {
                self.error("expected '[' after '#'");
                break;
            };
            let end = self.previous_end();
            attrs.push(Attribute {
                tokens,
                span: Span { start, end },
            });
        }
        attrs
    }

    fn block(&mut self) -> Option<Block> {
        let start = self.span().start;
        self.expect(Punct::LBrace, "{")?;
        let mut stmts = Vec::new();
        loop {
            match self.peek() {
                None => {
                    self.error("unterminated block: expected '}'");
                    return None;
                }
                Some(TokenKind::Punct(Punct::RBrace)) => {
                    self.bump();
                    let end = self.previous_end();
                    return Some(Block {
                        stmts,
                        span: Span { start, end },
                    });
                }
                _ => match self.stmt() {
                    Some(stmt) => stmts.push(stmt),
                    None => self.recover_to_statement_end(),
                },
            }
        }
    }

    fn stmt(&mut self) -> Option<Stmt> {
        let expr = self.expr()?;
        self.expect(Punct::Semi, ";")?;
        Some(Stmt::Expr(expr))
    }

    fn expr(&mut self) -> Option<Expr> {
        // M1 has exactly one expression form. M2 replaces this with the real grammar.
        let start = self.span().start;
        let name = self.ident()?;
        self.expect(Punct::Bang, "!")?;
        let open = match self.peek() {
            Some(TokenKind::Punct(p @ (Punct::LParen | Punct::LBracket | Punct::LBrace))) => *p,
            _ => {
                self.error("expected '(', '[' or '{' after a macro name");
                return None;
            }
        };
        let tokens = self.group(open)?;
        let end = self.previous_end();
        Some(Expr::Macro(MacroCall {
            name,
            tokens,
            span: Span { start, end },
        }))
    }

    /// Consumes a bracketed run of tokens and returns the ones inside it.
    fn group(&mut self, open: Punct) -> Option<Vec<Token>> {
        let close = match open {
            Punct::LParen => Punct::RParen,
            Punct::LBracket => Punct::RBracket,
            Punct::LBrace => Punct::RBrace,
            _ => unreachable!("group opens with a bracket"),
        };
        if self.peek() != Some(&TokenKind::Punct(open)) {
            self.error("expected an opening bracket");
            return None;
        }
        self.bump();
        let mut depth = 1usize;
        let mut tokens = Vec::new();
        loop {
            let Some(kind) = self.peek() else {
                self.error("unterminated bracket");
                return None;
            };
            if let TokenKind::Punct(p) = kind {
                if *p == open {
                    depth += 1;
                } else if *p == close {
                    depth -= 1;
                    if depth == 0 {
                        self.bump();
                        return Some(tokens);
                    }
                }
            }
            tokens.push(self.tokens[self.at].clone());
            self.bump();
        }
    }

    fn ident(&mut self) -> Option<Ident> {
        match self.peek() {
            Some(TokenKind::Ident(name)) => {
                let name = name.clone();
                let span = self.span();
                self.bump();
                Some(Ident { name, span })
            }
            Some(TokenKind::Reserved(word)) => {
                let word = word.clone();
                self.error(format!("'{word}' is reserved and cannot be used as a name"));
                None
            }
            _ => {
                self.error("expected a name");
                None
            }
        }
    }

    fn expect(&mut self, punct: Punct, shown: &str) -> Option<()> {
        if self.peek() == Some(&TokenKind::Punct(punct)) {
            self.bump();
            return Some(());
        }
        self.error(format!("expected '{shown}'"));
        None
    }

    /// Skips to something that could begin the next item, so one bad item costs one
    /// diagnostic.
    fn recover_to_item(&mut self) {
        while let Some(kind) = self.peek() {
            if matches!(
                kind,
                TokenKind::Keyword(Keyword::Fn) | TokenKind::Punct(Punct::Hash)
            ) {
                return;
            }
            self.bump();
        }
    }

    /// Skips past the end of a statement, stopping before the block's closing brace so
    /// the block itself still terminates properly.
    fn recover_to_statement_end(&mut self) {
        while let Some(kind) = self.peek() {
            match kind {
                TokenKind::Punct(Punct::Semi) => {
                    self.bump();
                    return;
                }
                TokenKind::Punct(Punct::RBrace) => return,
                _ => self.bump(),
            }
        }
    }

    fn peek(&self) -> Option<&TokenKind> {
        self.tokens.get(self.at).map(|t| &t.kind)
    }

    fn bump(&mut self) {
        self.at += 1;
    }

    /// The current token's span, or a zero-width span at end of input.
    fn span(&self) -> Span {
        match self.tokens.get(self.at) {
            Some(token) => token.span,
            None => Span {
                start: self.end,
                end: self.end,
            },
        }
    }

    fn previous_end(&self) -> usize {
        self.tokens
            .get(self.at.wrapping_sub(1))
            .map_or(self.end, |t| t.span.end)
    }

    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(SyntaxError::new(self.span(), message));
    }
}

// SPDX-License-Identifier: MIT

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(src: &str) -> SourceFile {
        let (file, errors) = parse(src);
        assert!(errors.is_empty(), "{errors:?}");
        file
    }

    fn parse_err(src: &str) -> Vec<SyntaxError> {
        parse(src).1
    }

    fn fn_item(file: &SourceFile, i: usize) -> &FnItem {
        match &file.items[i].kind {
            ItemKind::Fn(f) => f,
        }
    }

    #[test]
    fn a_function_with_one_macro_call() {
        let file = parse_ok(r#"fn main() { raw!("say hi"); }"#);
        assert_eq!(file.items.len(), 1);
        let f = fn_item(&file, 0);
        assert_eq!(f.name.name, "main");
        assert_eq!(f.body.stmts.len(), 1);
        let Stmt::Expr(Expr::Macro(call)) = &f.body.stmts[0];
        assert_eq!(call.name.name, "raw");
        assert_eq!(call.tokens.len(), 1);
        assert_eq!(call.tokens[0].kind, TokenKind::Str("say hi".to_owned()));
    }

    #[test]
    fn an_empty_body_is_fine() {
        let file = parse_ok("fn main() {}");
        assert!(fn_item(&file, 0).body.stmts.is_empty());
    }

    #[test]
    fn several_items_and_statements() {
        let file = parse_ok(
            r#"
            fn a() { raw!("x"); raw!("y"); }
            fn b() { raw!("z"); }
            "#,
        );
        assert_eq!(file.items.len(), 2);
        assert_eq!(fn_item(&file, 0).body.stmts.len(), 2);
    }

    #[test]
    fn attributes_attach_to_the_item() {
        let file = parse_ok("#[tick] fn main() {}");
        assert_eq!(file.items[0].attrs.len(), 1);
        assert_eq!(
            file.items[0].attrs[0].tokens[0].kind,
            TokenKind::Ident("tick".to_owned())
        );
    }

    #[test]
    fn spans_cover_the_whole_construct() {
        let src = "fn main() { raw!(\"x\"); }";
        let file = parse_ok(src);
        assert_eq!(&src[file.items[0].span.range()], src);
        assert_eq!(
            &src[fn_item(&file, 0).body.span.range()],
            "{ raw!(\"x\"); }"
        );
    }

    #[test]
    fn a_missing_semicolon_is_reported_where_it_should_be() {
        let src = r#"fn main() { raw!("x") }"#;
        let errors = parse_err(src);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains(';'), "{errors:?}");
    }

    #[test]
    fn a_reserved_word_says_it_is_reserved() {
        let errors = parse_err("async fn main() {}");
        assert!(
            errors[0].message.contains("reserved") && errors[0].message.contains("async"),
            "{errors:?}"
        );
    }

    #[test]
    fn parsing_recovers_and_keeps_finding_items() {
        // The junk item is reported, and `good` is still found.
        let (file, errors) = parse("nonsense fn good() {}");
        assert_eq!(errors.len(), 1);
        assert_eq!(file.items.len(), 1);
        assert_eq!(fn_item(&file, 0).name.name, "good");
    }

    #[test]
    fn a_bad_statement_does_not_swallow_the_rest_of_the_block() {
        let (file, errors) = parse(r#"fn main() { 1; raw!("x"); }"#);
        assert_eq!(errors.len(), 1);
        assert_eq!(fn_item(&file, 0).body.stmts.len(), 1);
    }

    #[test]
    fn lexer_errors_come_through_too() {
        let errors = parse_err("fn main() { raw!(\"unterminated); }");
        assert!(!errors.is_empty());
    }
}
