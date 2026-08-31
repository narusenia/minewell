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
        no_struct_lit: false,
    };
    let items = parser.items();
    (SourceFile { items }, parser.errors)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    errors: Vec<SyntaxError>,
    end: usize,
    /// Whether a `{` here starts a block rather than a struct literal. True while
    /// parsing the head of an `if`, `while`, `as`, `at` or `for` (spec section 3.10).
    no_struct_lit: bool,
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
            Some(TokenKind::Keyword(Keyword::Struct)) => return self.struct_item(attrs, start),
            Some(TokenKind::Keyword(Keyword::Enum)) => return self.enum_item(attrs, start),
            Some(TokenKind::Keyword(Keyword::Impl)) => return self.impl_item(attrs, start),
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
        let generics = self.generics()?;
        self.expect(Punct::LParen, "(")?;
        let receiver = self.receiver();
        if receiver.is_some() {
            self.eat_punct(Punct::Comma);
        }
        let mut params = Vec::new();
        while self.peek() != Some(&TokenKind::Punct(Punct::RParen)) {
            params.push(self.param()?);
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect(Punct::RParen, ")")?;
        let ret = if self.eat_punct(Punct::Arrow) {
            Some(self.type_name()?)
        } else {
            None
        };
        let body = self.block()?;
        let span = Span {
            start,
            end: body.span.end,
        };
        Some(Item {
            attrs,
            kind: ItemKind::Fn(FnItem {
                name,
                generics,
                receiver,
                params,
                ret,
                body,
            }),
            span,
        })
    }

    /// `struct Point { x: i32, y: i32 }`.
    fn struct_item(&mut self, attrs: Vec<Attribute>, start: usize) -> Option<Item> {
        self.bump();
        let name = self.ident()?;
        let generics = self.generics()?;
        self.expect(Punct::LBrace, "{")?;
        let mut fields = Vec::new();
        while self.peek() != Some(&TokenKind::Punct(Punct::RBrace)) {
            fields.push(self.field_def()?);
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect(Punct::RBrace, "}")?;
        let end = self.previous_end();
        Some(Item {
            attrs,
            kind: ItemKind::Struct(StructItem {
                name,
                generics,
                fields,
            }),
            span: Span { start, end },
        })
    }

    /// `impl Point { fn bump(&mut self) { .. } }`.
    fn impl_item(&mut self, attrs: Vec<Attribute>, start: usize) -> Option<Item> {
        self.bump();
        let ty = self.ident()?;
        self.expect(Punct::LBrace, "{")?;
        let mut methods = Vec::new();
        while self.peek() != Some(&TokenKind::Punct(Punct::RBrace)) {
            methods.push(self.item()?);
        }
        self.expect(Punct::RBrace, "}")?;
        let end = self.previous_end();
        Some(Item {
            attrs,
            kind: ItemKind::Impl(ImplItem { ty, methods }),
            span: Span { start, end },
        })
    }

    /// `enum State { Idle, Chasing { target: i32 } }`.
    fn enum_item(&mut self, attrs: Vec<Attribute>, start: usize) -> Option<Item> {
        self.bump();
        let name = self.ident()?;
        self.expect(Punct::LBrace, "{")?;
        let mut variants = Vec::new();
        while self.peek() != Some(&TokenKind::Punct(Punct::RBrace)) {
            variants.push(self.variant_def()?);
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect(Punct::RBrace, "}")?;
        let end = self.previous_end();
        Some(Item {
            attrs,
            kind: ItemKind::Enum(EnumItem { name, variants }),
            span: Span { start, end },
        })
    }

    fn variant_def(&mut self) -> Option<VariantDef> {
        let start = self.span().start;
        let name = self.ident()?;
        // A tuple variant would need invented keys; say so rather than guess (spec
        // section 3.11).
        if self.peek() == Some(&TokenKind::Punct(Punct::LParen)) {
            self.error("a variant names its fields: write 'V { field: i32 }'");
            return None;
        }
        let mut fields = Vec::new();
        if self.eat_punct(Punct::LBrace) {
            while self.peek() != Some(&TokenKind::Punct(Punct::RBrace)) {
                fields.push(self.field_def()?);
                if !self.eat_punct(Punct::Comma) {
                    break;
                }
            }
            self.expect(Punct::RBrace, "}")?;
        }
        let end = self.previous_end();
        Some(VariantDef {
            name,
            fields,
            span: Span { start, end },
        })
    }

    /// `<T, const S: i32>` after a name. Empty when there are none.
    fn generics(&mut self) -> Option<Vec<GenericParam>> {
        let mut params = Vec::new();
        if !self.eat_punct(Punct::Lt) {
            return Some(params);
        }
        while self.peek() != Some(&TokenKind::Punct(Punct::Gt)) {
            let is_const = self.eat_keyword(Keyword::Const);
            let name = self.ident()?;
            // A const parameter is always a scale, so `i32` is the only type it can
            // have (spec section 3.16). Written out anyway, to read as Rust does.
            if is_const {
                self.expect(Punct::Colon, ":")?;
                let ty = self.ident()?;
                if ty.name != "i32" {
                    self.error("a const parameter is a scale, so its type is i32");
                    return None;
                }
            }
            params.push(GenericParam { name, is_const });
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect(Punct::Gt, ">")?;
        Some(params)
    }

    fn field_def(&mut self) -> Option<FieldDef> {
        let start = self.span().start;
        let attrs = self.attributes();
        let name = self.binding_name()?;
        self.expect(Punct::Colon, ":")?;
        let ty = self.type_name()?;
        let end = self.previous_end();
        Some(FieldDef {
            attrs,
            name,
            ty,
            span: Span { start, end },
        })
    }

    /// `&self` / `&mut self` / `self` at the head of a parameter list.
    fn receiver(&mut self) -> Option<Receiver> {
        let start = self.span().start;
        let borrow = match self.peek() {
            Some(TokenKind::Keyword(Keyword::SelfValue)) => None,
            Some(TokenKind::Punct(Punct::And)) => {
                // `& self` / `& mut self`; anything else is a parameter type.
                let mutable = matches!(
                    self.tokens.get(self.at + 1).map(|t| &t.kind),
                    Some(TokenKind::Keyword(Keyword::Mut))
                );
                let after = if mutable { 2 } else { 1 };
                if !matches!(
                    self.tokens.get(self.at + after).map(|t| &t.kind),
                    Some(TokenKind::Keyword(Keyword::SelfValue))
                ) {
                    return None;
                }
                self.bump();
                if mutable {
                    self.bump();
                }
                Some(match mutable {
                    true => Borrow::Mutable,
                    false => Borrow::Shared,
                })
            }
            _ => return None,
        };
        self.bump();
        Some(Receiver {
            borrow,
            span: Span {
                start,
                end: self.previous_end(),
            },
        })
    }

    fn param(&mut self) -> Option<Param> {
        let start = self.span().start;
        let name = self.binding_name()?;
        self.expect(Punct::Colon, ":")?;
        let ty = self.type_name()?;
        let end = self.previous_end();
        Some(Param {
            name,
            ty,
            span: Span { start, end },
        })
    }

    fn type_name(&mut self) -> Option<TypeName> {
        let start = self.span().start;
        let borrow = if self.eat_punct(Punct::And) {
            Some(match self.eat_keyword(Keyword::Mut) {
                true => Borrow::Mutable,
                false => Borrow::Shared,
            })
        } else {
            None
        };
        let name = self.ident()?.name;
        let mut args = Vec::new();
        let mut scale = None;
        // `Vec<i32>`. Only types take angle brackets, so there is no ambiguity with
        // the comparison operators here.
        if name == "fix" {
            self.expect(Punct::Lt, "<")?;
            scale = Some(self.scale_arg()?);
            self.expect(Punct::Gt, ">")?;
        } else if self.eat_punct(Punct::Lt) {
            while self.peek() != Some(&TokenKind::Punct(Punct::Gt)) {
                args.push(self.type_name()?);
                if !self.eat_punct(Punct::Comma) {
                    break;
                }
            }
            self.expect(Punct::Gt, ">")?;
        }
        let end = self.previous_end();
        Some(TypeName {
            borrow,
            name,
            args,
            scale,
            span: Span { start, end },
        })
    }

    /// The `1000` of `fix<1000>`, or the name of a const parameter.
    fn scale_arg(&mut self) -> Option<ScaleArg> {
        let span = self.span();
        match self.peek() {
            Some(TokenKind::Int(value)) => {
                let value = *value;
                self.bump();
                Some(ScaleArg::Int(IntLit { value, span }))
            }
            Some(TokenKind::Ident(_)) => Some(ScaleArg::Param(self.ident()?)),
            _ => {
                self.error("a scale is an integer, as in 'fix<1000>'");
                None
            }
        }
    }

    fn eat_punct(&mut self, punct: Punct) -> bool {
        if self.peek() == Some(&TokenKind::Punct(punct)) {
            self.bump();
            true
        } else {
            false
        }
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
        let attrs = self.attributes();
        let span = self.span();
        match self.peek() {
            // `if let` is a `match` with two arms, and says so here rather than in a
            // second lowering (spec section 3.18).
            Some(TokenKind::Keyword(Keyword::If))
                if matches!(
                    self.tokens.get(self.at + 1).map(|t| &t.kind),
                    Some(TokenKind::Keyword(Keyword::Let))
                ) =>
            {
                self.if_let_stmt().map(Stmt::Match)
            }
            Some(TokenKind::Keyword(Keyword::If)) => self.if_stmt(attrs).map(Stmt::If),
            Some(TokenKind::Keyword(Keyword::While | Keyword::Loop)) => {
                self.loop_stmt(attrs).map(Stmt::Loop)
            }
            Some(TokenKind::Keyword(Keyword::As | Keyword::At | Keyword::For)) => {
                self.context_stmt(attrs).map(Stmt::Context)
            }
            Some(TokenKind::Keyword(Keyword::Match)) => {
                if let Some(attr) = attrs.first() {
                    self.errors.push(SyntaxError::new(
                        attr.span,
                        "attributes here only apply to 'if', 'while' and 'loop'",
                    ));
                    return None;
                }
                self.match_stmt().map(Stmt::Match)
            }
            _ => {
                if let Some(attr) = attrs.first() {
                    self.errors.push(SyntaxError::new(
                        attr.span,
                        "attributes here only apply to 'if', 'while' and 'loop'",
                    ));
                    return None;
                }
                match self.peek() {
                    Some(TokenKind::Keyword(Keyword::Let)) => self.let_stmt().map(Stmt::Let),
                    Some(TokenKind::Keyword(Keyword::Break)) => {
                        self.bump();
                        self.expect(Punct::Semi, ";")?;
                        Some(Stmt::Break(span))
                    }
                    Some(TokenKind::Keyword(Keyword::Continue)) => {
                        self.bump();
                        self.expect(Punct::Semi, ";")?;
                        Some(Stmt::Continue(span))
                    }
                    Some(TokenKind::Keyword(Keyword::Return)) => {
                        self.bump();
                        let value = if self.peek() == Some(&TokenKind::Punct(Punct::Semi)) {
                            None
                        } else {
                            Some(self.expr()?)
                        };
                        self.expect(Punct::Semi, ";")?;
                        Some(Stmt::Return { value, span })
                    }
                    _ => {
                        let expr = self.expr()?;
                        self.expect(Punct::Semi, ";")?;
                        Some(Stmt::Expr(expr))
                    }
                }
            }
        }
    }

    fn if_stmt(&mut self, attrs: Vec<Attribute>) -> Option<IfStmt> {
        let start = self.span().start;
        self.bump();
        let cond = self.head_expr()?;
        let then = self.block()?;
        let otherwise = if self.eat_keyword(Keyword::Else) {
            Some(Box::new(
                if self.peek() == Some(&TokenKind::Keyword(Keyword::If)) {
                    Else::If(self.if_stmt(Vec::new())?)
                } else {
                    Else::Block(self.block()?)
                },
            ))
        } else {
            None
        };
        let end = self.previous_end();
        Some(IfStmt {
            attrs,
            cond,
            then,
            otherwise,
            span: Span { start, end },
        })
    }

    /// `if let Some(x) = o { .. } else { .. }`, as the `match` it is.
    fn if_let_stmt(&mut self) -> Option<MatchStmt> {
        let start = self.span().start;
        self.bump();
        self.bump();
        let pattern = self.pattern()?;
        self.expect(Punct::Eq, "=")?;
        let scrutinee = self.head_expr()?;
        let then = self.block()?;
        let then_span = then.span;
        let otherwise = match self.eat_keyword(Keyword::Else) {
            true => self.block()?,
            false => Block {
                stmts: Vec::new(),
                span: then.span,
            },
        };
        let else_span = otherwise.span;
        let span = Span {
            start,
            end: self.previous_end(),
        };
        Some(MatchStmt {
            scrutinee,
            arms: vec![
                MatchArm {
                    span: pattern.span(),
                    pattern,
                    body: then,
                },
                // Everything the first arm did not take, which for an option is the
                // other one of the two.
                MatchArm {
                    pattern: Pattern::Wildcard(else_span),
                    body: otherwise,
                    span: then_span,
                },
            ],
            span,
        })
    }

    /// `match s { State::Idle => { .. } _ => { .. } }`.
    fn match_stmt(&mut self) -> Option<MatchStmt> {
        let start = self.span().start;
        self.bump();
        let scrutinee = self.head_expr()?;
        self.expect(Punct::LBrace, "{")?;
        let mut arms = Vec::new();
        while self.peek() != Some(&TokenKind::Punct(Punct::RBrace)) {
            arms.push(self.match_arm()?);
            // A comma between arms is allowed but not required, as in Rust.
            self.eat_punct(Punct::Comma);
        }
        self.expect(Punct::RBrace, "}")?;
        let end = self.previous_end();
        Some(MatchStmt {
            scrutinee,
            arms,
            span: Span { start, end },
        })
    }

    fn match_arm(&mut self) -> Option<MatchArm> {
        let start = self.span().start;
        let pattern = self.pattern()?;
        self.expect(Punct::FatArrow, "=>")?;
        let body = self.block()?;
        let end = self.previous_end();
        Some(MatchArm {
            pattern,
            body,
            span: Span { start, end },
        })
    }

    fn pattern(&mut self) -> Option<Pattern> {
        let start = self.span().start;
        if self.eat_punct(Punct::Underscore) {
            return Some(Pattern::Wildcard(Span {
                start,
                end: self.previous_end(),
            }));
        }
        let ty = self.ident()?;
        // `Some(x)` and `None` are built in: `Option` is not a user enum, so they are
        // spelled the way everyone spells them (spec section 3.18).
        if ty.name == "None" {
            return Some(Pattern::None(Span {
                start,
                end: self.previous_end(),
            }));
        }
        if ty.name == "Some" {
            self.expect(Punct::LParen, "(")?;
            let bind = self.ident()?;
            self.expect(Punct::RParen, ")")?;
            return Some(Pattern::Some {
                bind,
                span: Span {
                    start,
                    end: self.previous_end(),
                },
            });
        }
        if !self.eat_punct(Punct::ColonColon) {
            self.error("expected a variant, as in 'State::Idle'");
            return None;
        }
        let variant = self.ident()?;
        let mut binds = Vec::new();
        if self.eat_punct(Punct::LBrace) {
            while self.peek() != Some(&TokenKind::Punct(Punct::RBrace)) {
                binds.push(self.ident()?);
                if !self.eat_punct(Punct::Comma) {
                    break;
                }
            }
            self.expect(Punct::RBrace, "}")?;
        }
        let end = self.previous_end();
        Some(Pattern::Variant {
            ty,
            variant,
            binds,
            span: Span { start, end },
        })
    }

    fn context_stmt(&mut self, attrs: Vec<Attribute>) -> Option<ContextStmt> {
        let start = self.span().start;
        let kind = match self.peek() {
            Some(TokenKind::Keyword(Keyword::As)) => ContextKind::As,
            Some(TokenKind::Keyword(Keyword::At)) => ContextKind::At,
            _ => ContextKind::For,
        };
        self.bump();
        let binding = if kind == ContextKind::For {
            let name = self.ident()?;
            if !self.eat_keyword(Keyword::In) {
                self.error("expected 'in'");
                return None;
            }
            Some(name)
        } else {
            None
        };
        let selector = self.head_expr()?;
        let body = self.block()?;
        let end = self.previous_end();
        Some(ContextStmt {
            attrs,
            kind,
            binding,
            selector,
            body,
            span: Span { start, end },
        })
    }

    fn loop_stmt(&mut self, attrs: Vec<Attribute>) -> Option<LoopStmt> {
        let start = self.span().start;
        let conditional = self.peek() == Some(&TokenKind::Keyword(Keyword::While));
        self.bump();
        let cond = if conditional {
            Some(self.head_expr()?)
        } else {
            None
        };
        let body = self.block()?;
        let end = self.previous_end();
        Some(LoopStmt {
            attrs,
            cond,
            body,
            span: Span { start, end },
        })
    }

    fn let_stmt(&mut self) -> Option<LetStmt> {
        let start = self.span().start;
        self.bump();
        let mutable = self.eat_keyword(Keyword::Mut);
        let name = self.binding_name()?;
        let ty = if self.eat_punct(Punct::Colon) {
            Some(self.type_name()?)
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

    /// The expression before a block: a condition, or a selector.
    ///
    /// A struct literal cannot appear here, because `if p { .. }` would otherwise not
    /// say whether the brace opens a block or a value. Rust draws the line in the same
    /// place, and parentheses lift the restriction.
    fn head_expr(&mut self) -> Option<Expr> {
        let outer = std::mem::replace(&mut self.no_struct_lit, true);
        let expr = self.expr();
        self.no_struct_lit = outer;
        expr
    }

    fn assign(&mut self) -> Option<Expr> {
        let lhs = self.range()?;
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

        if !matches!(lhs, Expr::Path(_) | Expr::Field(_) | Expr::Index(_)) {
            self.errors.push(SyntaxError::new(
                op_span,
                "the left side of an assignment must be a binding, a field or an element",
            ));
            return None;
        }
        let span = Span {
            start: lhs.span().start,
            end: value.span().end,
        };
        Some(Expr::Assign(AssignExpr {
            op,
            target: Box::new(lhs),
            value: Box::new(value),
            span,
        }))
    }

    /// `a..b`. Only `slice` takes one, but it parses here so that the bounds are
    /// ordinary expressions rather than a second grammar (spec section 3.17).
    fn range(&mut self) -> Option<Expr> {
        let start = self.or()?;
        if self.peek() != Some(&TokenKind::Punct(Punct::DotDot)) {
            return Some(start);
        }
        let span = start.span();
        self.bump();
        let end = match self.peek() {
            Some(TokenKind::Punct(Punct::RParen | Punct::Comma)) => None,
            _ => Some(Box::new(self.or()?)),
        };
        Some(Expr::Range(RangeExpr {
            span: Span {
                start: span.start,
                end: self.previous_end(),
            },
            start: Some(Box::new(start)),
            end,
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

    /// A primary expression and the accesses that follow it: fields, indices, methods.
    fn primary(&mut self) -> Option<Expr> {
        let mut expr = self.atom()?;
        loop {
            let start = expr.span().start;
            match self.peek() {
                Some(TokenKind::Punct(Punct::Dot)) => {
                    self.bump();
                    let name = self.ident()?;
                    expr = if self.peek() == Some(&TokenKind::Punct(Punct::LParen)) {
                        let args = self.call_args()?;
                        Expr::Method(MethodCall {
                            receiver: Box::new(expr),
                            name,
                            args,
                            span: Span {
                                start,
                                end: self.previous_end(),
                            },
                        })
                    } else {
                        Expr::Field(FieldExpr {
                            base: Box::new(expr),
                            name,
                            span: Span {
                                start,
                                end: self.previous_end(),
                            },
                        })
                    };
                }
                Some(TokenKind::Punct(Punct::LBracket)) => {
                    self.bump();
                    // Inside brackets a struct literal is unambiguous again.
                    let outer = std::mem::replace(&mut self.no_struct_lit, false);
                    let index = self.expr();
                    self.no_struct_lit = outer;
                    let index = index?;
                    self.expect(Punct::RBracket, "]")?;
                    expr = Expr::Index(IndexExpr {
                        base: Box::new(expr),
                        index: Box::new(index),
                        span: Span {
                            start,
                            end: self.previous_end(),
                        },
                    });
                }
                // `o?` binds tighter than any operator: it is part of reading the
                // value, not something done to the value afterwards.
                Some(TokenKind::Punct(Punct::Question)) => {
                    self.bump();
                    expr = Expr::Try(TryExpr {
                        value: Box::new(expr),
                        span: Span {
                            start,
                            end: self.previous_end(),
                        },
                    });
                }
                _ => return Some(expr),
            }
        }
    }

    fn atom(&mut self) -> Option<Expr> {
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
                // Inside brackets there is no block to be confused with.
                let outer = std::mem::replace(&mut self.no_struct_lit, false);
                let inner = self.expr();
                self.no_struct_lit = outer;
                let inner = inner?;
                self.expect(Punct::RParen, ")")?;
                Some(inner)
            }
            Some(TokenKind::Str(text)) => {
                let value = text.clone();
                self.bump();
                Some(Expr::Str(StrLit { value, span }))
            }
            Some(TokenKind::Punct(Punct::And)) => {
                self.bump();
                let borrow = match self.eat_keyword(Keyword::Mut) {
                    true => Borrow::Mutable,
                    false => Borrow::Shared,
                };
                let place = self.primary()?;
                Some(Expr::Borrow(BorrowExpr {
                    borrow,
                    span: Span {
                        start: span.start,
                        end: place.span().end,
                    },
                    place: Box::new(place),
                }))
            }
            Some(TokenKind::Keyword(Keyword::SelfValue)) => {
                self.bump();
                Some(Expr::Path(Ident {
                    name: "self".to_owned(),
                    span,
                }))
            }
            Some(TokenKind::Punct(Punct::LBracket)) => {
                self.bump();
                let outer = std::mem::replace(&mut self.no_struct_lit, false);
                let mut values = Vec::new();
                let mut failed = false;
                while self.peek() != Some(&TokenKind::Punct(Punct::RBracket)) {
                    match self.expr() {
                        Some(value) => values.push(value),
                        None => {
                            failed = true;
                            break;
                        }
                    }
                    if !self.eat_punct(Punct::Comma) {
                        break;
                    }
                }
                self.no_struct_lit = outer;
                if failed {
                    return None;
                }
                self.expect(Punct::RBracket, "]")?;
                Some(Expr::List(ListLit {
                    values,
                    span: Span {
                        start: span.start,
                        end: self.previous_end(),
                    },
                }))
            }
            Some(TokenKind::Resource(text)) => {
                let text = text.clone();
                self.bump();
                Some(Expr::Resource(ResourceLit { text, span }))
            }
            Some(TokenKind::Selector(text)) => {
                let text = text.clone();
                self.bump();
                Some(Expr::Selector(SelectorLit { text, span }))
            }
            Some(TokenKind::Ident(_)) => {
                let name = self.ident()?;
                match self.peek() {
                    Some(TokenKind::Punct(Punct::Bang)) => self.macro_call(name),
                    Some(TokenKind::Punct(Punct::LParen)) => self.call(name),
                    Some(TokenKind::Punct(Punct::LBrace)) if !self.no_struct_lit => {
                        self.struct_lit(name, None)
                    }
                    // `State::Idle`, with or without a payload.
                    Some(TokenKind::Punct(Punct::ColonColon)) => {
                        self.bump();
                        // `fix::<1000>(1500)`. Angle brackets after `::` are the one
                        // turbofish the language has (spec section 3.16).
                        if self.peek() == Some(&TokenKind::Punct(Punct::Lt)) {
                            return self.fix_cast(name);
                        }
                        let variant = self.ident()?;
                        // `Mob::of(@s)`: the one associated function there is
                        // (spec section 3.19).
                        if self.peek() == Some(&TokenKind::Punct(Punct::LParen)) {
                            if variant.name != "of" {
                                self.error("the only associated function is 'of'");
                                return None;
                            }
                            self.expect(Punct::LParen, "(")?;
                            let selector = self.expr()?;
                            self.expect(Punct::RParen, ")")?;
                            let start = name.span.start;
                            return Some(Expr::ViewOf(ViewOfExpr {
                                ty: name,
                                selector: Box::new(selector),
                                span: Span {
                                    start,
                                    end: self.previous_end(),
                                },
                            }));
                        }
                        if self.peek() == Some(&TokenKind::Punct(Punct::LBrace))
                            && !self.no_struct_lit
                        {
                            return self.struct_lit(name, Some(variant));
                        }
                        let span = Span {
                            start: name.span.start,
                            end: variant.span.end,
                        };
                        Some(Expr::Struct(StructLit {
                            name,
                            variant: Some(variant),
                            fields: Vec::new(),
                            span,
                        }))
                    }
                    _ => Some(Expr::Path(name)),
                }
            }
            _ => {
                self.error("expected an expression");
                None
            }
        }
    }

    /// `fix::<1000>(1500)`, from the `<` onwards.
    fn fix_cast(&mut self, name: Ident) -> Option<Expr> {
        if name.name != "fix" {
            // Type arguments are never written at a call: they follow from the
            // arguments (spec section 3.14).
            self.error("only 'fix' takes an argument in angle brackets here");
            return None;
        }
        self.expect(Punct::Lt, "<")?;
        let scale = self.scale_arg()?;
        self.expect(Punct::Gt, ">")?;
        self.expect(Punct::LParen, "(")?;
        let value = self.expr()?;
        self.expect(Punct::RParen, ")")?;
        Some(Expr::Fix(FixExpr {
            scale,
            value: Box::new(value),
            span: Span {
                start: name.span.start,
                end: self.previous_end(),
            },
        }))
    }

    fn call(&mut self, callee: Ident) -> Option<Expr> {
        let start = callee.span.start;
        let args = self.call_args()?;
        let end = self.previous_end();
        Some(Expr::Call(CallExpr {
            callee,
            args,
            span: Span { start, end },
        }))
    }

    /// `Point { x: 1, y: 2 }`.
    fn struct_lit(&mut self, name: Ident, variant: Option<Ident>) -> Option<Expr> {
        let start = name.span.start;
        self.bump();
        let mut fields = Vec::new();
        while self.peek() != Some(&TokenKind::Punct(Punct::RBrace)) {
            let field_start = self.span().start;
            let field = self.ident()?;
            self.expect(Punct::Colon, ":")?;
            // A field's value is a full expression again: the brace is already open.
            let outer = std::mem::replace(&mut self.no_struct_lit, false);
            let value = self.expr();
            self.no_struct_lit = outer;
            fields.push(FieldInit {
                name: field,
                value: value?,
                span: Span {
                    start: field_start,
                    end: self.previous_end(),
                },
            });
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect(Punct::RBrace, "}")?;
        let end = self.previous_end();
        Some(Expr::Struct(StructLit {
            name,
            variant,
            fields,
            span: Span { start, end },
        }))
    }

    /// The bracketed argument list of a call, from the `(` onwards.
    fn call_args(&mut self) -> Option<Vec<Expr>> {
        self.expect(Punct::LParen, "(")?;
        let mut args = Vec::new();
        while self.peek() != Some(&TokenKind::Punct(Punct::RParen)) {
            // Arguments are a fresh context: a brace here opens a value, not a block.
            let outer = std::mem::replace(&mut self.no_struct_lit, false);
            let arg = self.expr();
            self.no_struct_lit = outer;
            args.push(arg?);
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect(Punct::RParen, ")")?;
        Some(args)
    }

    fn macro_call(&mut self, name: Ident) -> Option<Expr> {
        let start = name.span.start;
        self.expect(Punct::Bang, "!")?;
        // `debug_assert!` takes an expression, not a token soup: it is a check
        // written in the language (spec section 3.20).
        if name.name == "debug_assert" {
            self.expect(Punct::LParen, "(")?;
            let cond = self.expr()?;
            let message = match self.eat_punct(Punct::Comma) {
                false => None,
                true => match self.peek() {
                    Some(TokenKind::Str(text)) => {
                        let text = text.clone();
                        self.bump();
                        Some(text)
                    }
                    _ => {
                        self.error("a message is a string literal");
                        return None;
                    }
                },
            };
            self.expect(Punct::RParen, ")")?;
            return Some(Expr::Assert(AssertExpr {
                cond: Box::new(cond),
                message,
                span: Span {
                    start,
                    end: self.previous_end(),
                },
            }));
        }
        // `text!` takes expressions too: its arguments are values and method chains
        // written in the language (spec section 3.22).
        if name.name == "text" {
            let args = self.call_args()?;
            return Some(Expr::Text(TextMacro {
                args,
                span: Span {
                    start,
                    end: self.previous_end(),
                },
            }));
        }
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
            other => panic!("expected a function, found {other:?}"),
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
    fn statement_attributes_only_go_where_they_mean_something() {
        let errors = parse_err("fn main() { #[inline] let x = 1; }");
        assert!(errors[0].message.contains("only apply to"), "{errors:?}");
    }

    #[test]
    fn control_flow_parses() {
        parse_ok("fn main() { if a { raw!(\"x\"); } }");
        parse_ok("fn main() { if a { } else { } }");
        parse_ok("fn main() { if a { } else if b { } else { } }");
        parse_ok("fn main() { while a { } }");
        parse_ok("fn main() { loop { break; continue; return; } }");
        parse_ok("fn main() { #[no_inline] if a { } }");
    }

    #[test]
    fn lexer_errors_come_through_too() {
        let errors = parse_err("fn main() { raw!(\"unterminated); }");
        assert!(!errors.is_empty());
    }
    #[test]
    fn a_struct_item_and_its_fields() {
        let file = parse_ok("struct Point { x: i32, y: bool }");
        let ItemKind::Struct(item) = &file.items[0].kind else {
            panic!("expected a struct")
        };
        assert_eq!(item.name.name, "Point");
        assert_eq!(item.fields.len(), 2);
        assert_eq!(item.fields[1].ty.name, "bool");
    }

    #[test]
    fn a_struct_literal_is_an_expression() {
        let file = parse_ok("fn main() { let p = Point { x: 1 }; }");
        let Stmt::Let(let_stmt) = &fn_item(&file, 0).body.stmts[0] else {
            panic!("expected a let")
        };
        let Expr::Struct(lit) = &let_stmt.value else {
            panic!("expected a struct literal, found {:?}", let_stmt.value)
        };
        assert_eq!(lit.name.name, "Point");
        assert_eq!(lit.fields[0].name.name, "x");
    }

    /// Spec section 3.10: in the head of an `if`, a `{` opens the block. Without this
    /// rule `if p { .. }` would have two readings and the parser would pick one.
    #[test]
    fn a_field_access_chains() {
        let file = parse_ok("fn main() { let a = o.inner.a; }");
        let Stmt::Let(let_stmt) = &fn_item(&file, 0).body.stmts[0] else {
            panic!("expected a let")
        };
        let Expr::Field(outer) = &let_stmt.value else {
            panic!("expected a field access")
        };
        assert_eq!(outer.name.name, "a");
        assert!(matches!(*outer.base, Expr::Field(_)));
    }

    #[test]
    fn a_method_call_parses() {
        let file = parse_ok("fn main() { v.push(1); }");
        let Stmt::Expr(Expr::Method(call)) = &fn_item(&file, 0).body.stmts[0] else {
            panic!("expected a method call")
        };
        assert_eq!(call.name.name, "push");
        assert_eq!(call.args.len(), 1);
    }

    #[test]
    fn an_index_parses() {
        let file = parse_ok("fn main() { let x = v[i]; }");
        let Stmt::Let(let_stmt) = &fn_item(&file, 0).body.stmts[0] else {
            panic!("expected a let")
        };
        assert!(matches!(let_stmt.value, Expr::Index(_)));
    }

    #[test]
    fn a_brace_after_a_condition_opens_the_block() {
        let file = parse_ok("fn main() { if p { } }");
        let Stmt::If(if_stmt) = &fn_item(&file, 0).body.stmts[0] else {
            panic!("expected an if")
        };
        assert!(matches!(if_stmt.cond, Expr::Path(_)));
        assert!(if_stmt.then.stmts.is_empty());
    }

    #[test]
    fn parentheses_bring_the_literal_back() {
        let file = parse_ok("fn main() { if (Flag { on: true }) == f { } }");
        let Stmt::If(if_stmt) = &fn_item(&file, 0).body.stmts[0] else {
            panic!("expected an if")
        };
        let Expr::Binary(cond) = &if_stmt.cond else {
            panic!("expected a comparison")
        };
        assert!(matches!(*cond.lhs, Expr::Struct(_)));
    }
}
