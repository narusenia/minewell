//! Tokens to AST.
//!
//! Follows `docs/02-spec.md` section 3. Parsing collects errors and recovers rather
//! than stopping: at item level it skips to the next thing that could begin an item,
//! and inside a block it skips to the end of the offending statement. One mistake
//! should cost one diagnostic, not the rest of the file.

use super::SyntaxError;
use super::ast::*;
use super::lexer::{Keyword, Punct, Span, Token, TokenKind, lex};

fn binary(op: BinaryOp, lhs: Expr, rhs: Expr) -> Expr {
    let span = Span {
        start: lhs.span().start,
        end: rhs.span().end,
    };
    Expr::Binary(BinaryExpr {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
        span,
    })
}

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
        if self.peek() == Some(&TokenKind::Keyword(Keyword::Let)) {
            return self.let_stmt().map(Stmt::Let);
        }
        let expr = self.expr()?;
        self.expect(Punct::Semi, ";")?;
        Some(Stmt::Expr(expr))
    }

    fn let_stmt(&mut self) -> Option<LetStmt> {
        let start = self.span().start;
        self.bump();
        let mutable = self.eat_keyword(Keyword::Mut);
        let name = self.binding_name()?;
        let ty = if self.peek() == Some(&TokenKind::Punct(Punct::Colon)) {
            self.bump();
            let span = self.span();
            let name = self.ident()?.name;
            Some(TypeName { name, span })
        } else {
            None
        };
        self.expect(Punct::Eq, "=")?;
        let value = self.expr()?;
        self.expect(Punct::Semi, ";")?;
        let end = self.previous_end();
        Some(LetStmt {
            mutable,
            name,
            ty,
            value,
            span: Span { start, end },
        })
    }

    /// The name in a `let`, with the one lexical trap spelled out.
    ///
    /// `let x:i32` lexes as a single resource location token (spec section 2.8),
    /// because `ident:ident` with no space is always one. Rather than let that surface
    /// as "expected a name", say what to do about it.
    fn binding_name(&mut self) -> Option<Ident> {
        if let Some(TokenKind::Resource(text)) = self.peek() {
            let text = text.clone();
            if let Some((name, ty)) = text.split_once(':') {
                self.error(format!(
                    "a type annotation needs a space after the colon: write '{name}: {ty}'"
                ));
                self.bump();
                return None;
            }
        }
        self.ident()
    }

    fn expr(&mut self) -> Option<Expr> {
        self.assign()
    }

    fn assign(&mut self) -> Option<Expr> {
        let lhs = self.or()?;
        let op = match self.peek() {
            Some(TokenKind::Punct(Punct::Eq)) => None,
            Some(TokenKind::Punct(Punct::PlusEq)) => Some(BinaryOp::Add),
            Some(TokenKind::Punct(Punct::MinusEq)) => Some(BinaryOp::Sub),
            Some(TokenKind::Punct(Punct::StarEq)) => Some(BinaryOp::Mul),
            Some(TokenKind::Punct(Punct::SlashEq)) => Some(BinaryOp::Div),
            Some(TokenKind::Punct(Punct::PercentEq)) => Some(BinaryOp::Rem),
            _ => return Some(lhs),
        };
        let op_span = self.span();
        self.bump();
        // Right associative: `a = b = c` is `a = (b = c)`.
        let value = self.assign()?;
        let Expr::Path(target) = lhs else {
            self.errors.push(SyntaxError::new(
                op_span,
                "the left side of an assignment must be a binding",
            ));
            return None;
        };
        let span = Span {
            start: target.span.start,
            end: value.span().end,
        };
        Some(Expr::Assign(AssignExpr {
            op,
            target,
            value: Box::new(value),
            span,
        }))
    }

    fn or(&mut self) -> Option<Expr> {
        let mut lhs = self.and()?;
        while self.peek() == Some(&TokenKind::Punct(Punct::OrOr)) {
            self.bump();
            lhs = binary(BinaryOp::Or, lhs, self.and()?);
        }
        Some(lhs)
    }

    fn and(&mut self) -> Option<Expr> {
        let mut lhs = self.compare()?;
        while self.peek() == Some(&TokenKind::Punct(Punct::AndAnd)) {
            self.bump();
            lhs = binary(BinaryOp::And, lhs, self.compare()?);
        }
        Some(lhs)
    }

    /// Comparisons do not chain: `a < b < c` is an error, as in Rust.
    fn compare(&mut self) -> Option<Expr> {
        let lhs = self.sum()?;
        let Some(op) = self.compare_op() else {
            return Some(lhs);
        };
        self.bump();
        let rhs = self.sum()?;
        if let Some(second) = self.compare_op() {
            let _ = second;
            self.error("comparisons do not chain; parenthesise if that is what you meant");
            return None;
        }
        Some(binary(op, lhs, rhs))
    }

    fn compare_op(&mut self) -> Option<BinaryOp> {
        let TokenKind::Punct(punct) = self.peek()? else {
            return None;
        };
        Some(match punct {
            Punct::EqEq => BinaryOp::Eq,
            Punct::Ne => BinaryOp::Ne,
            Punct::Lt => BinaryOp::Lt,
            Punct::Le => BinaryOp::Le,
            Punct::Gt => BinaryOp::Gt,
            Punct::Ge => BinaryOp::Ge,
            _ => return None,
        })
    }

    fn sum(&mut self) -> Option<Expr> {
        let mut lhs = self.product()?;
        loop {
            let op = match self.peek() {
                Some(TokenKind::Punct(Punct::Plus)) => BinaryOp::Add,
                Some(TokenKind::Punct(Punct::Minus)) => BinaryOp::Sub,
                _ => return Some(lhs),
            };
            self.bump();
            lhs = binary(op, lhs, self.product()?);
        }
    }

    fn product(&mut self) -> Option<Expr> {
        let mut lhs = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(TokenKind::Punct(Punct::Star)) => BinaryOp::Mul,
                Some(TokenKind::Punct(Punct::Slash)) => BinaryOp::Div,
                Some(TokenKind::Punct(Punct::Percent)) => BinaryOp::Rem,
                _ => return Some(lhs),
            };
            self.bump();
            lhs = binary(op, lhs, self.unary()?);
        }
    }

    fn unary(&mut self) -> Option<Expr> {
        let start = self.span().start;
        let op = match self.peek() {
            Some(TokenKind::Punct(Punct::Minus)) => UnaryOp::Neg,
            Some(TokenKind::Punct(Punct::Bang)) => UnaryOp::Not,
            _ => return self.primary(),
        };
        self.bump();
        let operand = self.unary()?;
        let end = operand.span().end;
        Some(Expr::Unary(UnaryExpr {
            op,
            operand: Box::new(operand),
            span: Span { start, end },
        }))
    }

    fn primary(&mut self) -> Option<Expr> {
        let span = self.span();
        match self.peek() {
            Some(TokenKind::Int(value)) => {
                let value = *value;
                self.bump();
                Some(Expr::Int(IntLit { value, span }))
            }
            Some(TokenKind::Keyword(k @ (Keyword::True | Keyword::False))) => {
                let value = *k == Keyword::True;
                self.bump();
                Some(Expr::Bool(BoolLit { value, span }))
            }
            Some(TokenKind::Punct(Punct::LParen)) => {
                self.bump();
                let inner = self.expr()?;
                self.expect(Punct::RParen, ")")?;
                Some(inner)
            }
            Some(TokenKind::Ident(_)) => {
                let name = self.ident()?;
                if self.peek() == Some(&TokenKind::Punct(Punct::Bang)) {
                    return self.macro_call(name);
                }
                Some(Expr::Path(name))
            }
            _ => {
                self.error("expected an expression");
                None
            }
        }
    }

    fn macro_call(&mut self, name: Ident) -> Option<Expr> {
        let start = name.span.start;
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

    fn eat_keyword(&mut self, keyword: Keyword) -> bool {
        if self.peek() == Some(&TokenKind::Keyword(keyword)) {
            self.bump();
            true
        } else {
            false
        }
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
        let Stmt::Expr(Expr::Macro(call)) = &f.body.stmts[0] else {
            panic!("expected a macro call")
        };
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
        let (file, errors) = parse(r#"fn main() { let = 1; raw!("x"); }"#);
        assert_eq!(errors.len(), 1);
        assert_eq!(fn_item(&file, 0).body.stmts.len(), 1);
    }

    #[test]
    fn lexer_errors_come_through_too() {
        let errors = parse_err("fn main() { raw!(\"unterminated); }");
        assert!(!errors.is_empty());
    }
}
