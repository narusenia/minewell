// SPDX-License-Identifier: MIT

//! The high-level intermediate representation: names resolved, types checked.
//!
//! HIR is where "what did the author mean" is settled — see `docs/02-spec.md` section
//! 4. Every expression carries its type, so nothing downstream has to ask again.
//!
//! Two choices here are deliberate and worth knowing:
//!
//! - **There is no inference.** A `let` takes the type of its annotation, or of its
//!   initialiser. Nothing propagates backwards. The language's types stay few and
//!   concrete, so unification would cost a solver and return nothing.
//! - **Unknown attributes are errors.** A misspelled `#[tik]` that is quietly ignored
//!   is the class of silent failure minewell exists to remove.

use std::collections::HashMap;

use crate::schema::{ArgType, Schema};
use crate::syntax::SyntaxError;
use crate::syntax::ast::{self, BinaryOp, Expr as AstExpr, ItemKind, SourceFile, UnaryOp};
use crate::syntax::lexer::{Punct, Span, TokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FnId(pub u32);

/// Identifies a binding within one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    I32,
    Bool,
    /// A compile-time selector. It has no runtime representation: the only thing that
    /// can be done with one is hand it to `as`, `at` or `for`.
    Selector,
    /// `minecraft:stone`. Compile-time only.
    Resource,
    /// `pos!(~ ~1 ~)`. Compile-time only.
    Pos,
}

impl Type {
    fn parse(name: &str) -> Option<Type> {
        match name {
            "i32" => Some(Type::I32),
            "bool" => Some(Type::Bool),
            // Deliberately not spellable in a type annotation: a selector is inferred
            // from the literal, never declared.
            _ => None,
        }
    }

    /// Whether the type exists only while compiling, with nothing to put in a
    /// register at runtime.
    pub fn is_compile_time(&self) -> bool {
        matches!(self, Type::Selector | Type::Resource | Type::Pos)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Type::I32 => "i32",
            Type::Bool => "bool",
            Type::Selector => "selector",
            Type::Resource => "ResourceLocation",
            Type::Pos => "Pos",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hir {
    pub functions: Vec<Function>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub id: FnId,
    pub name: String,
    /// Where this lands in the datapack: `<namespace>:<path>`.
    pub path: String,
    pub attrs: Vec<Attr>,
    /// Parameters are locals the caller writes before calling, so they are the first
    /// entries in `locals`.
    pub params: Vec<LocalId>,
    pub ret: Option<Type>,
    pub locals: Vec<Local>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    pub id: LocalId,
    pub name: String,
    pub ty: Type,
    pub mutable: bool,
}

/// Whether a block was asked to be inlined into its guard, or split into its own
/// function. `Auto` lets the lowering decide (spec section 6.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inline {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Raw(RawCommand),
    If {
        cond: Expr,
        then: Vec<Stmt>,
        otherwise: Option<Vec<Stmt>>,
        inline: Inline,
        span: Span,
    },
    /// `while` and `loop`; the latter has no condition.
    Loop {
        cond: Option<Expr>,
        body: Vec<Stmt>,
        inline: Inline,
        span: Span,
    },
    /// A call written as a statement. Any value it returns is discarded.
    CallFor {
        callee: FnId,
        args: Vec<Expr>,
        span: Span,
    },
    /// `as` / `at` / `for`. The body runs once per entity the selector finds.
    Context {
        kind: ContextKind,
        selector: Selector,
        body: Vec<Stmt>,
        inline: Inline,
        span: Span,
    },
    Break(Span),
    Continue(Span),
    Return {
        value: Option<Expr>,
        span: Span,
    },
    Let {
        local: LocalId,
        value: Expr,
        span: Span,
    },
    Assign {
        local: LocalId,
        /// `None` for `=`; otherwise the arithmetic to apply first.
        op: Option<BinaryOp>,
        value: Expr,
        span: Span,
    },
}

/// A `raw!` command. Interpolation arrives in M9; today the text is literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextKind {
    As,
    At,
    For,
}

/// A selector, resolved at compile time. `@s` is what a `for` binding means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCommand {
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprKind {
    Int(i32),
    Bool(bool),
    Str(String),
    Local(LocalId),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Call {
        callee: FnId,
        args: Vec<Expr>,
    },
    /// A compile-time selector. Never evaluated into a register.
    Selector(String),
    Resource(String),
    Pos(String),
    /// A whole command, already rendered. One command, one line.
    Command(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attr {
    Tick,
    Load,
    Inline,
    NoInline,
    /// What execution context this function requires of its caller.
    Ctx(Vec<Ctx>),
}

/// A kind of execution context. `dimension` is absent because there is no way to enter
/// one yet, and a requirement nothing can satisfy is not a requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ctx {
    Entity,
    Position,
}

impl Ctx {
    fn parse(name: &str) -> Option<Ctx> {
        match name {
            "entity" => Some(Ctx::Entity),
            "position" => Some(Ctx::Position),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Ctx::Entity => "entity",
            Ctx::Position => "position",
        }
    }
}

impl Attr {
    fn parse(name: &str) -> Option<Attr> {
        Some(match name {
            "tick" => Attr::Tick,
            "load" => Attr::Load,
            "inline" => Attr::Inline,
            "no_inline" => Attr::NoInline,
            _ => return None,
        })
    }
}

/// Attributes the language will have but does not act on yet. Named so the diagnostic
/// can say "not implemented" rather than "unknown", which is a different problem.
const PLANNED_ATTRS: &[&str] = &["score", "storage", "nbt", "unroll", "derive"];

/// A function's shape, known before any body is lowered so that a call can be checked
/// no matter which order the two were written in.
#[derive(Debug, Clone)]
struct Signature {
    id: FnId,
    params: Vec<Type>,
    ret: Option<Type>,
    /// What the function requires of its caller, from `#[ctx(..)]`.
    ctx: Vec<Ctx>,
}

pub fn lower(
    file: &SourceFile,
    namespace: &str,
    toolchain: Option<&Schema>,
) -> (Hir, Vec<SyntaxError>) {
    let mut errors = Vec::new();
    let mut signatures: HashMap<String, Signature> = HashMap::new();
    let mut items: Vec<(&ast::Item, &ast::FnItem)> = Vec::new();

    // First pass: signatures only. Without it, calling a function defined further down
    // the file would be an error — a rule about text order rather than about programs.
    for item in &file.items {
        let ItemKind::Fn(f) = &item.kind;
        if signatures.contains_key(&f.name.name) {
            let name = &f.name.name;
            errors.push(SyntaxError::new(
                f.name.span,
                format!("a function named '{name}' is already defined"),
            ));
            continue;
        }
        let params = f
            .params
            .iter()
            .map(|param| named_type(&param.ty, &mut errors).unwrap_or(Type::I32))
            .collect();
        let ret = f
            .ret
            .as_ref()
            .and_then(|written| named_type(written, &mut errors));
        // Read from the attribute directly: signatures are needed before any body is
        // lowered, and this is the only part of the attributes a caller cares about.
        let ctx = declared_ctx(&item.attrs);
        signatures.insert(
            f.name.name.clone(),
            Signature {
                id: FnId(items.len() as u32),
                params,
                ret,
                ctx,
            },
        );
        items.push((item, f));
    }

    let mut functions = Vec::new();
    for (item, f) in items {
        let signature = signatures[&f.name.name].clone();
        let mut cx = FnLowering {
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
            selector_aliases: HashMap::new(),
            provided: Vec::new(),
            in_entity_loop: false,
            loop_depth: 0,
            ret: signature.ret,
            signatures: &signatures,
            toolchain,
            errors: &mut errors,
        };
        let attrs = cx.attrs(&item.attrs);
        cx.provided = attrs
            .iter()
            .filter_map(|attr| match attr {
                Attr::Ctx(kinds) => Some(kinds.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        // A function tag invokes with no executor, so a tick or load function that
        // needs one is guaranteed to do nothing at runtime — silently. Vanilla can
        // never tell you this.
        let tagged = attrs.iter().any(|a| matches!(a, Attr::Tick | Attr::Load));
        if tagged && !cx.provided.is_empty() {
            cx.error(
                f.name.span,
                "a #[tick] or #[load] function cannot require a context: function tags \
                 invoke it with no executor, so it would silently do nothing",
            );
        }
        let params = f
            .params
            .iter()
            .zip(&signature.params)
            .map(|(param, ty)| cx.declare(&param.name.name, *ty, false))
            .collect();
        let body = cx.block(&f.body);
        let locals = cx.locals;
        if signature.ret.is_some() && !always_returns(&body) {
            errors.push(SyntaxError::new(
                f.name.span,
                "this function can reach its end without returning a value",
            ));
        }
        functions.push(Function {
            id: signature.id,
            name: f.name.name.clone(),
            path: format!("{namespace}:{}", f.name.name),
            attrs,
            params,
            ret: signature.ret,
            locals,
            body,
            span: item.span,
        });
    }
    (Hir { functions }, errors)
}

/// The `#[ctx(..)]` kinds on an item, ignoring anything malformed — the body pass
/// reports those with a span.
fn declared_ctx(attrs: &[ast::Attribute]) -> Vec<Ctx> {
    let mut kinds = Vec::new();
    for attr in attrs {
        let Some(TokenKind::Ident(name)) = attr.tokens.first().map(|t| &t.kind) else {
            continue;
        };
        if name != "ctx" {
            continue;
        }
        for token in attr.tokens.iter().skip(1) {
            if let TokenKind::Ident(kind) = &token.kind
                && let Some(kind) = Ctx::parse(kind)
            {
                kinds.push(kind);
            }
        }
    }
    kinds.sort();
    kinds.dedup();
    kinds
}

fn named_type(written: &ast::TypeName, errors: &mut Vec<SyntaxError>) -> Option<Type> {
    match Type::parse(&written.name) {
        Some(ty) => Some(ty),
        None => {
            let name = &written.name;
            errors.push(SyntaxError::new(
                written.span,
                format!("unknown type '{name}'"),
            ));
            None
        }
    }
}

/// Whether control cannot reach the end of a statement list without returning.
///
/// Deliberately shallow: a top-level `return`, or an `if`/`else` where both sides
/// return. A loop that never exits also never falls through, but proving that needs
/// more than syntax, and guessing would turn a missing `return` into a runtime
/// surprise instead of a compile error.
fn always_returns(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|stmt| match stmt {
        Stmt::Return { .. } => true,
        Stmt::If {
            then,
            otherwise: Some(otherwise),
            ..
        } => always_returns(then) && always_returns(otherwise),
        _ => false,
    })
}

struct FnLowering<'a> {
    locals: Vec<Local>,
    ret: Option<Type>,
    signatures: &'a HashMap<String, Signature>,
    /// The command surface of the configured Minecraft version, if there is one.
    toolchain: Option<&'a Schema>,
    /// Innermost scope last. A `let` shadows an outer binding of the same name.
    scopes: Vec<HashMap<String, LocalId>>,
    /// Bindings that stand for a selector rather than a value.
    selector_aliases: HashMap<LocalId, String>,
    /// The contexts available at this point: the function's own `#[ctx]`, plus
    /// whatever the enclosing `as` / `at` / `for` blocks add.
    provided: Vec<Ctx>,
    /// Whether the innermost loop is a `for` over entities, where `continue` is just
    /// returning from the body.
    in_entity_loop: bool,
    /// How many loops enclose the statement being lowered. `break` outside one is an
    /// error, and it is only detectable here.
    loop_depth: u32,
    errors: &'a mut Vec<SyntaxError>,
}

impl FnLowering<'_> {
    fn attrs(&mut self, attrs: &[ast::Attribute]) -> Vec<Attr> {
        attrs
            .iter()
            .filter_map(|attr| {
                let Some(TokenKind::Ident(name)) = attr.tokens.first().map(|t| &t.kind) else {
                    self.error(attr.span, "expected an attribute name");
                    return None;
                };
                if name == "ctx" {
                    return self.ctx_attr(attr);
                }
                match Attr::parse(name) {
                    Some(attr) => Some(attr),
                    None if PLANNED_ATTRS.contains(&name.as_str()) => {
                        self.error(
                            attr.span,
                            format!("'{name}' is planned but not implemented yet"),
                        );
                        None
                    }
                    None => {
                        self.error(attr.span, format!("unknown attribute '{name}'"));
                        None
                    }
                }
            })
            .collect()
    }

    /// `#[ctx(entity)]`, `#[ctx(entity, position)]`.
    fn ctx_attr(&mut self, attr: &ast::Attribute) -> Option<Attr> {
        let mut kinds = Vec::new();
        // tokens are: `ctx` `(` name `,` name `)`
        for token in attr.tokens.iter().skip(1) {
            match &token.kind {
                TokenKind::Ident(name) => match Ctx::parse(name) {
                    Some(kind) => kinds.push(kind),
                    None => {
                        self.error(
                            token.span,
                            format!("unknown context '{name}'; expected entity or position"),
                        );
                        return None;
                    }
                },
                TokenKind::Punct(_) => {}
                _ => {
                    self.error(token.span, "expected a context name");
                    return None;
                }
            }
        }
        if kinds.is_empty() {
            self.error(
                attr.span,
                "#[ctx(..)] needs at least one of entity, position",
            );
            return None;
        }
        kinds.sort();
        kinds.dedup();
        Some(Attr::Ctx(kinds))
    }

    fn block(&mut self, block: &ast::Block) -> Vec<Stmt> {
        self.scopes.push(HashMap::new());
        let stmts = block
            .stmts
            .iter()
            .filter_map(|stmt| self.stmt(stmt))
            .collect();
        self.scopes.pop();
        stmts
    }

    fn stmt(&mut self, stmt: &ast::Stmt) -> Option<Stmt> {
        match stmt {
            ast::Stmt::Let(let_stmt) => self.let_stmt(let_stmt),
            ast::Stmt::Expr(AstExpr::Macro(call)) => self.macro_call(call).map(Stmt::Raw),
            ast::Stmt::Expr(AstExpr::Assign(assign)) => self.assign(assign),
            ast::Stmt::Expr(AstExpr::Call(call))
                if !self.signatures.contains_key(&call.callee.name) =>
            {
                let ExprKind::Command(text) = self.command(call)?.kind else {
                    return None;
                };
                Some(Stmt::Raw(RawCommand {
                    text,
                    span: call.span,
                }))
            }
            ast::Stmt::Expr(AstExpr::Call(call)) => {
                let (callee, _) = self.call_signature(call)?;
                let args = self.call_args(call)?;
                Some(Stmt::CallFor {
                    callee,
                    args,
                    span: call.span,
                })
            }
            // Every other expression is pure, so evaluating one for its effect is
            // asking for nothing to happen. Say so rather than emit dead commands.
            ast::Stmt::Expr(other) => {
                self.error(other.span(), "this expression has no effect");
                None
            }
            ast::Stmt::If(if_stmt) => self.if_stmt(if_stmt),
            ast::Stmt::Loop(loop_stmt) => self.loop_stmt(loop_stmt),
            ast::Stmt::Context(ctx_stmt) => self.context_stmt(ctx_stmt),
            ast::Stmt::Break(span) => self.jump(*span, "break").map(|()| Stmt::Break(*span)),
            ast::Stmt::Continue(span) => {
                self.jump(*span, "continue").map(|()| Stmt::Continue(*span))
            }
            ast::Stmt::Return { value, span } => self.return_stmt(value.as_ref(), *span),
        }
    }

    fn return_stmt(&mut self, value: Option<&AstExpr>, span: Span) -> Option<Stmt> {
        let value = match (value, self.ret) {
            (None, None) => None,
            (Some(expr), Some(want)) => {
                let expr = self.expr(expr)?;
                if expr.ty != want {
                    self.error(
                        expr.span,
                        format!("expected {}, found {}", want.name(), expr.ty.name()),
                    );
                    return None;
                }
                Some(expr)
            }
            (Some(expr), None) => {
                self.error(expr.span(), "this function does not return a value");
                return None;
            }
            (None, Some(want)) => {
                self.error(span, format!("expected a {} to return", want.name()));
                return None;
            }
        };
        Some(Stmt::Return { value, span })
    }

    fn jump(&mut self, span: Span, keyword: &str) -> Option<()> {
        if self.loop_depth == 0 {
            self.error(span, format!("'{keyword}' is only allowed inside a loop"));
            return None;
        }
        Some(())
    }

    fn if_stmt(&mut self, stmt: &ast::IfStmt) -> Option<Stmt> {
        let cond = self.condition(&stmt.cond)?;
        let then = self.block(&stmt.then);
        let otherwise = stmt.otherwise.as_deref().map(|branch| match branch {
            ast::Else::Block(block) => self.block(block),
            // `else if` is just an `if` in the else block, so it needs no special case
            // beyond wrapping it back up as a statement.
            ast::Else::If(nested) => self.if_stmt(nested).into_iter().collect(),
        });
        let inline = self.inline_attr(&stmt.attrs)?;
        if inline == Inline::Always && !(otherwise.is_none() && then.len() == 1) {
            self.error(
                stmt.span,
                "only a single-statement 'if' with no 'else' can be inlined",
            );
            return None;
        }
        Some(Stmt::If {
            cond,
            then,
            otherwise,
            inline,
            span: stmt.span,
        })
    }

    fn context_stmt(&mut self, stmt: &ast::ContextStmt) -> Option<Stmt> {
        let selector = self.selector(&stmt.selector)?;
        let kind = match stmt.kind {
            ast::ContextKind::As => ContextKind::As,
            ast::ContextKind::At => ContextKind::At,
            ast::ContextKind::For => ContextKind::For,
        };
        // `@s` only means something when there is already an executor.
        if selector.text == "@s" {
            self.require(Ctx::Entity, selector.span, "@s");
        }
        let inline = self.inline_attr(&stmt.attrs)?;

        self.provided.push(match kind {
            ContextKind::At => Ctx::Position,
            _ => Ctx::Entity,
        });
        self.scopes.push(HashMap::new());
        if let Some(binding) = &stmt.binding {
            // The binding is a compile-time alias for `@s` inside the body.
            let local = self.declare(&binding.name, Type::Selector, false);
            self.selector_aliases.insert(local, "@s".to_owned());
        }
        // All three iterate over what the selector found, so `break` and `continue`
        // mean something inside them.
        self.loop_depth += 1;
        // The body is one function per entity, so returning from it is what "next
        // entity" means. `while`'s rules for `continue` do not apply inside.
        let outer_loop = std::mem::replace(&mut self.in_entity_loop, true);
        let body = stmt
            .body
            .stmts
            .iter()
            .filter_map(|stmt| self.stmt(stmt))
            .collect();
        self.in_entity_loop = outer_loop;
        self.loop_depth -= 1;
        self.scopes.pop();
        self.provided.pop();

        Some(Stmt::Context {
            kind,
            selector,
            body,
            inline,
            span: stmt.span,
        })
    }

    /// A selector expression: a literal, or a name bound to one.
    fn selector(&mut self, expr: &AstExpr) -> Option<Selector> {
        let value = self.expr(expr)?;
        match value.kind {
            ExprKind::Selector(text) => Some(Selector {
                text,
                span: value.span,
            }),
            ExprKind::Local(local) => match self.selector_aliases.get(&local) {
                Some(text) => Some(Selector {
                    text: text.clone(),
                    span: value.span,
                }),
                None => {
                    self.error(value.span, "expected a selector");
                    None
                }
            },
            _ => {
                self.error(value.span, "expected a selector");
                None
            }
        }
    }

    /// Records that something here needs a context, and complains if it is missing.
    fn require(&mut self, ctx: Ctx, span: Span, what: &str) {
        if self.provided.contains(&ctx) {
            return;
        }
        let name = ctx.name();
        self.error(
            span,
            format!(
                "{what} needs an {name} context here; wrap it in an 'as' block, \
                 or declare #[ctx({name})] on this function"
            ),
        );
    }

    fn loop_stmt(&mut self, stmt: &ast::LoopStmt) -> Option<Stmt> {
        let cond = match &stmt.cond {
            Some(cond) => Some(self.condition(cond)?),
            None => None,
        };
        let inline = self.inline_attr(&stmt.attrs)?;
        if inline == Inline::Always {
            self.error(stmt.span, "a loop is always its own function");
            return None;
        }
        self.loop_depth += 1;
        let outer = std::mem::replace(&mut self.in_entity_loop, false);
        let body = self.block(&stmt.body);
        self.in_entity_loop = outer;
        self.loop_depth -= 1;
        Some(Stmt::Loop {
            cond,
            body,
            inline,
            span: stmt.span,
        })
    }

    /// A condition has to be `bool`. There is no truthiness: an `i32` is not a
    /// condition, because "non-zero is true" would make `if x` and `if x != 0` two
    /// spellings of one thing and hide the type error in between.
    fn condition(&mut self, expr: &AstExpr) -> Option<Expr> {
        let cond = self.expr(expr)?;
        if cond.ty != Type::Bool {
            self.error(
                cond.span,
                format!("a condition must be bool, found {}", cond.ty.name()),
            );
            return None;
        }
        Some(cond)
    }

    fn inline_attr(&mut self, attrs: &[ast::Attribute]) -> Option<Inline> {
        let mut inline = Inline::Auto;
        for attr in self.attrs(attrs) {
            match attr {
                Attr::Inline => inline = Inline::Always,
                Attr::NoInline => inline = Inline::Never,
                _ => return None,
            }
        }
        Some(inline)
    }

    fn let_stmt(&mut self, stmt: &ast::LetStmt) -> Option<Stmt> {
        let value = self.expr(&stmt.value)?;
        let ty = match &stmt.ty {
            None => value.ty,
            Some(written) => {
                let Some(ty) = Type::parse(&written.name) else {
                    let name = &written.name;
                    self.error(written.span, format!("unknown type '{name}'"));
                    return None;
                };
                if ty != value.ty {
                    self.error(
                        stmt.value.span(),
                        format!("expected {}, found {}", ty.name(), value.ty.name()),
                    );
                    return None;
                }
                ty
            }
        };
        let local = self.declare(&stmt.name.name, ty, stmt.mutable);
        // A selector binding is a compile-time alias, not a value in a register.
        if let ExprKind::Selector(text) = &value.kind {
            self.selector_aliases.insert(local, text.clone());
        }
        Some(Stmt::Let {
            local,
            value,
            span: stmt.span,
        })
    }

    fn assign(&mut self, assign: &ast::AssignExpr) -> Option<Stmt> {
        let value = self.expr(&assign.value)?;
        let local = self.lookup(&assign.target)?;
        let declared = self.locals[local.0 as usize].clone();
        if !declared.mutable {
            let name = &declared.name;
            self.error(
                assign.span,
                format!("'{name}' is not mutable; declare it with 'let mut'"),
            );
            return None;
        }
        // A compound assignment is the arithmetic, so it inherits arithmetic's rules.
        if assign.op.is_some() && declared.ty != Type::I32 {
            self.error(
                assign.span,
                format!(
                    "compound assignment needs i32, found {}",
                    declared.ty.name()
                ),
            );
            return None;
        }
        if declared.ty != value.ty {
            self.error(
                assign.value.span(),
                format!("expected {}, found {}", declared.ty.name(), value.ty.name()),
            );
            return None;
        }
        Some(Stmt::Assign {
            local,
            op: assign.op,
            value,
            span: assign.span,
        })
    }

    fn expr(&mut self, expr: &AstExpr) -> Option<Expr> {
        let span = expr.span();
        match expr {
            AstExpr::Int(lit) => Some(Expr {
                kind: ExprKind::Int(lit.value),
                ty: Type::I32,
                span,
            }),
            AstExpr::Bool(lit) => Some(Expr {
                kind: ExprKind::Bool(lit.value),
                ty: Type::Bool,
                span,
            }),
            AstExpr::Path(name) => {
                let local = self.lookup(name)?;
                Some(Expr {
                    kind: ExprKind::Local(local),
                    ty: self.locals[local.0 as usize].ty,
                    span,
                })
            }
            AstExpr::Unary(unary) => {
                let operand = self.expr(&unary.operand)?;
                let want = match unary.op {
                    UnaryOp::Neg => Type::I32,
                    UnaryOp::Not => Type::Bool,
                };
                if operand.ty != want {
                    self.error(
                        span,
                        format!("expected {}, found {}", want.name(), operand.ty.name()),
                    );
                    return None;
                }
                Some(Expr {
                    kind: ExprKind::Unary(unary.op, Box::new(operand)),
                    ty: want,
                    span,
                })
            }
            AstExpr::Binary(binary) => {
                let lhs = self.expr(&binary.lhs)?;
                let rhs = self.expr(&binary.rhs)?;
                let ty = self.binary_type(binary.op, &lhs, &rhs, span)?;
                Some(Expr {
                    kind: ExprKind::Binary(binary.op, Box::new(lhs), Box::new(rhs)),
                    ty,
                    span,
                })
            }
            // Spec section 4.3: assignment is a statement. There is no `()` type for
            // it to produce, and inventing one to make this legal buys nothing.
            AstExpr::Assign(_) => {
                self.error(span, "an assignment is a statement and produces no value");
                None
            }
            AstExpr::Call(call) if !self.signatures.contains_key(&call.callee.name) => {
                self.command(call)
            }
            AstExpr::Call(call) => {
                let (callee, ty) = self.call_signature(call)?;
                let Some(ty) = ty else {
                    let name = &call.callee.name;
                    self.error(span, format!("'{name}' does not return a value"));
                    return None;
                };
                let args = self.call_args(call)?;
                Some(Expr {
                    kind: ExprKind::Call { callee, args },
                    ty,
                    span,
                })
            }
            AstExpr::Selector(lit) => Some(Expr {
                kind: ExprKind::Selector(lit.text.clone()),
                ty: Type::Selector,
                span,
            }),
            AstExpr::Str(lit) => Some(Expr {
                kind: ExprKind::Str(lit.value.clone()),
                // Strings are compile-time only until M8 gives them a storage home.
                ty: Type::Resource,
                span,
            }),
            AstExpr::Resource(lit) => Some(Expr {
                kind: ExprKind::Resource(lit.text.clone()),
                ty: Type::Resource,
                span,
            }),
            AstExpr::Macro(call) if call.name.name == "pos" => Some(Expr {
                kind: ExprKind::Pos(self.pos(call)?),
                ty: Type::Pos,
                span,
            }),
            AstExpr::Macro(call) => {
                let name = &call.name.name;
                self.error(span, format!("'{name}!' does not produce a value"));
                None
            }
        }
    }

    fn binary_type(&mut self, op: BinaryOp, lhs: &Expr, rhs: &Expr, span: Span) -> Option<Type> {
        use BinaryOp::*;
        for side in [lhs, rhs] {
            if side.ty.is_compile_time() {
                self.error(span, format!("a {} has no runtime value", side.ty.name()));
                return None;
            }
        }
        let (want, result) = match op {
            Add | Sub | Mul | Div | Rem => (Some(Type::I32), Type::I32),
            Lt | Le | Gt | Ge => (Some(Type::I32), Type::Bool),
            And | Or => (Some(Type::Bool), Type::Bool),
            // Equality works on any type, as long as both sides agree.
            Eq | Ne => (None, Type::Bool),
        };
        if let Some(want) = want
            && (lhs.ty != want || rhs.ty != want)
        {
            let found = if lhs.ty != want { lhs.ty } else { rhs.ty };
            self.error(
                span,
                format!(
                    "this operator needs {}, found {}",
                    want.name(),
                    found.name()
                ),
            );
            return None;
        }
        if lhs.ty != rhs.ty {
            self.error(
                span,
                format!("cannot compare {} with {}", lhs.ty.name(), rhs.ty.name()),
            );
            return None;
        }
        Some(result)
    }

    /// A command call, if the name is one. User functions win: defining `fn setblock`
    /// shadows the command, which is the only way to wrap one.
    fn command(&mut self, call: &ast::CallExpr) -> Option<Expr> {
        if self.signatures.contains_key(&call.callee.name) {
            return None;
        }
        let Some(schema) = self.toolchain else {
            self.error(
                call.callee.span,
                format!(
                    "'{}' is not defined; if it is a Minecraft command, set 'toolchain' \
                     in minewell.toml so the compiler knows the command set",
                    call.callee.name
                ),
            );
            return None;
        };
        let signature = schema.get(&call.callee.name)?.clone();
        if call.args.len() != signature.params.len() {
            let (name, n, m) = (&signature.name, signature.params.len(), call.args.len());
            self.error(
                call.span,
                format!("the command '{name}' takes {n} argument(s), but {m} were given"),
            );
            return None;
        }
        let mut parts = signature.literals.clone();
        for (arg, param) in call.args.iter().zip(&signature.params) {
            parts.push(self.command_arg(arg, param.ty)?);
        }
        Some(Expr {
            kind: ExprKind::Command(parts.join(" ")),
            ty: Type::I32,
            span: call.span,
        })
    }

    /// Renders one argument into the command text.
    ///
    /// Everything has to be known now: a command is a string, and putting a runtime
    /// value into one needs a macro function (requirements section 10.1), which
    /// arrives in M9. Until then this says so rather than emitting something wrong.
    fn command_arg(&mut self, arg: &ast::Expr, want: ArgType) -> Option<String> {
        let value = self.expr(arg)?;
        let rendered = match (&value.kind, want) {
            (ExprKind::Pos(text), ArgType::Pos) => text.clone(),
            (ExprKind::Resource(text), ArgType::Resource) => text.clone(),
            (ExprKind::Selector(text), ArgType::Selector) => text.clone(),
            (ExprKind::Int(n), ArgType::I32) => n.to_string(),
            (ExprKind::Bool(b), ArgType::Bool) => b.to_string(),
            // The types with no literal of their own take a string, unexamined.
            (
                ExprKind::Str(text),
                ArgType::Str | ArgType::Nbt | ArgType::Component | ArgType::Raw,
            ) => text.clone(),
            (kind, want) => {
                let found = match kind {
                    ExprKind::Local(_) | ExprKind::Binary(..) | ExprKind::Unary(..) => {
                        return {
                            self.error(
                                value.span,
                                "a command argument has to be known at compile time; \
                                 passing a runtime value needs a macro function, which \
                                 is not implemented yet",
                            );
                            None
                        };
                    }
                    _ => value.ty.name(),
                };
                self.error(
                    value.span,
                    format!("expected {} here, found {found}", want.name()),
                );
                return None;
            }
        };
        Some(rendered)
    }

    fn call_signature(&mut self, call: &ast::CallExpr) -> Option<(FnId, Option<Type>)> {
        match self.signatures.get(&call.callee.name) {
            Some(sig) => {
                let missing: Vec<Ctx> = sig
                    .ctx
                    .iter()
                    .copied()
                    .filter(|ctx| !self.provided.contains(ctx))
                    .collect();
                if !missing.is_empty() {
                    let name = &call.callee.name;
                    let kinds = missing
                        .iter()
                        .map(|c| c.name())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.error(
                        call.span,
                        format!(
                            "'{name}' declares #[ctx({kinds})] but no {kinds} context is \
                             available here; wrap the call in an 'as' block, or declare \
                             #[ctx({kinds})] on this function too"
                        ),
                    );
                }
                Some((sig.id, sig.ret))
            }
            None => {
                let name = &call.callee.name;
                self.error(call.callee.span, format!("'{name}' is not defined"));
                None
            }
        }
    }

    fn call_args(&mut self, call: &ast::CallExpr) -> Option<Vec<Expr>> {
        let want = self.signatures[&call.callee.name].params.clone();
        if call.args.len() != want.len() {
            let name = &call.callee.name;
            let (n, m) = (want.len(), call.args.len());
            self.error(
                call.span,
                format!("'{name}' takes {n} argument(s), but {m} were given"),
            );
            return None;
        }
        let mut args = Vec::new();
        for (arg, want) in call.args.iter().zip(want) {
            let arg = self.expr(arg)?;
            if arg.ty != want {
                self.error(
                    arg.span,
                    format!("expected {}, found {}", want.name(), arg.ty.name()),
                );
                return None;
            }
            args.push(arg);
        }
        Some(args)
    }

    /// `pos!(~ ~1 ~)`. Three coordinates, all in the same notation.
    ///
    /// A coordinate is a token pair rather than a literal because `~` and `^` are
    /// separate tokens (spec section 2.6) — which is also why coordinates live inside
    /// a macro instead of in the expression grammar.
    fn pos(&mut self, call: &ast::MacroCall) -> Option<String> {
        let mut coords: Vec<(Option<char>, Option<i32>)> = Vec::new();
        let mut tokens = call.tokens.iter().peekable();
        while let Some(token) = tokens.next() {
            let prefix = match &token.kind {
                TokenKind::Punct(Punct::Tilde) => Some('~'),
                TokenKind::Punct(Punct::Caret) => Some('^'),
                _ => None,
            };
            let value = if prefix.is_some() {
                match tokens.peek().map(|t| &t.kind) {
                    Some(TokenKind::Int(n)) => {
                        let n = *n;
                        tokens.next();
                        Some(n)
                    }
                    _ => None,
                }
            } else {
                match &token.kind {
                    TokenKind::Int(n) => Some(*n),
                    TokenKind::Punct(Punct::Minus) => match tokens.next().map(|t| &t.kind) {
                        Some(TokenKind::Int(n)) => Some(-n),
                        _ => {
                            self.error(token.span, "expected a coordinate");
                            return None;
                        }
                    },
                    _ => {
                        self.error(token.span, "expected a coordinate");
                        return None;
                    }
                }
            };
            coords.push((prefix, value));
        }
        if coords.len() != 3 {
            self.error(call.span, "pos! takes three coordinates");
            return None;
        }
        // Vanilla rejects a mix, and so does this: `~ ^ ~` is not a position.
        let first = coords[0].0;
        if coords.iter().any(|(prefix, _)| *prefix != first) {
            self.error(
                call.span,
                "all three coordinates must use the same notation: all absolute, all '~' or all '^'",
            );
            return None;
        }
        Some(
            coords
                .iter()
                .map(|(prefix, value)| match (prefix, value) {
                    (Some(p), Some(n)) => format!("{p}{n}"),
                    (Some(p), None) => p.to_string(),
                    (None, Some(n)) => n.to_string(),
                    (None, None) => "0".to_owned(),
                })
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    fn macro_call(&mut self, call: &ast::MacroCall) -> Option<RawCommand> {
        match call.name.name.as_str() {
            "raw" => self.raw(call),
            other => {
                self.error(call.span, format!("unknown macro '{other}!'"));
                None
            }
        }
    }

    fn raw(&mut self, call: &ast::MacroCall) -> Option<RawCommand> {
        match call.tokens.as_slice() {
            [token] => match &token.kind {
                TokenKind::Str(text) => Some(RawCommand {
                    text: text.clone(),
                    span: call.span,
                }),
                _ => {
                    self.error(token.span, "raw! takes a string literal");
                    None
                }
            },
            [] => {
                self.error(call.span, "raw! takes a string literal");
                None
            }
            tokens => {
                let span = tokens[1].span;
                self.error(span, "raw! takes a single string literal");
                None
            }
        }
    }

    fn declare(&mut self, name: &str, ty: Type, mutable: bool) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(Local {
            id,
            name: name.to_owned(),
            ty,
            mutable,
        });
        self.scopes
            .last_mut()
            .expect("a scope is always open")
            .insert(name.to_owned(), id);
        id
    }

    fn lookup(&mut self, name: &ast::Ident) -> Option<LocalId> {
        for scope in self.scopes.iter().rev() {
            if let Some(id) = scope.get(&name.name) {
                return Some(*id);
            }
        }
        let text = &name.name;
        self.errors.push(SyntaxError::new(
            name.span,
            format!("'{text}' is not defined"),
        ));
        None
    }

    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(SyntaxError::new(span, message));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parser::parse;

    fn lower_ok(src: &str) -> Hir {
        let (file, errors) = parse(src);
        assert!(errors.is_empty(), "{errors:?}");
        let (hir, errors) = lower(&file, "myns", None);
        assert!(errors.is_empty(), "{errors:?}");
        hir
    }

    fn lower_err(src: &str) -> Vec<SyntaxError> {
        let (file, errors) = parse(src);
        assert!(errors.is_empty(), "{errors:?}");
        lower(&file, "myns", None).1
    }

    fn schema() -> Schema {
        Schema::parse(include_str!("../../tests/fixtures/commands.json")).expect("fixture")
    }

    fn with_toolchain(src: &str) -> Result<Hir, Vec<SyntaxError>> {
        let (file, errors) = parse(src);
        assert!(errors.is_empty(), "{errors:?}");
        let (hir, errors) = lower(&file, "myns", Some(&schema()));
        if errors.is_empty() {
            Ok(hir)
        } else {
            Err(errors)
        }
    }

    fn command_text(src: &str) -> String {
        let hir = with_toolchain(src).expect("compiles");
        match &hir.functions[0].body[0] {
            Stmt::Raw(raw) => raw.text.clone(),
            other => panic!("expected a command, found {other:?}"),
        }
    }

    #[test]
    fn a_command_becomes_one_line() {
        assert_eq!(
            command_text("fn main() { setblock(pos!(~ ~1 ~), minecraft:stone); }"),
            "setblock ~ ~1 ~ minecraft:stone"
        );
        assert_eq!(command_text("fn main() { reload(); }"), "reload");
    }

    #[test]
    fn coordinates_render_the_way_they_were_written() {
        assert!(
            command_text("fn main() { setblock(pos!(10 64 -5), minecraft:stone); }")
                .contains("10 64 -5")
        );
        assert!(
            command_text("fn main() { setblock(pos!(^ ^ ^5), minecraft:stone); }")
                .contains("^ ^ ^5")
        );
    }

    #[test]
    fn coordinate_notations_cannot_be_mixed() {
        let errors =
            with_toolchain("fn main() { setblock(pos!(~ ^ ~), minecraft:stone); }").unwrap_err();
        assert!(errors[0].message.contains("same notation"), "{errors:?}");
    }

    #[test]
    fn pos_needs_exactly_three_coordinates() {
        assert!(
            with_toolchain("fn main() { setblock(pos!(~ ~), minecraft:stone); }").unwrap_err()[0]
                .message
                .contains("three")
        );
    }

    #[test]
    fn a_command_argument_of_the_wrong_kind_is_reported() {
        let errors = with_toolchain("fn main() { setblock(minecraft:stone, minecraft:stone); }")
            .unwrap_err();
        assert!(errors[0].message.contains("Pos"), "{errors:?}");
    }

    #[test]
    fn a_runtime_value_cannot_go_into_a_command_yet() {
        let errors = with_toolchain(
            "fn main() { let p = 1; data_get_entity(@s); setblock(pos!(~ ~ ~), minecraft:stone); }",
        );
        // The command above is fine; this one is not.
        let errors2 = with_toolchain("fn main() { let n = 1; experiment(n); }").unwrap_err();
        assert!(errors.is_ok(), "{errors:?}");
        assert!(errors2[0].message.contains("compile time"), "{errors2:?}");
    }

    #[test]
    fn a_command_needs_a_toolchain_to_be_known() {
        let errors = lower_err("fn main() { setblock(pos!(~ ~ ~), minecraft:stone); }");
        assert!(errors[0].message.contains("toolchain"), "{errors:?}");
    }

    #[test]
    fn a_user_function_shadows_a_command_of_the_same_name() {
        let hir = with_toolchain("fn reload() { } fn main() { reload(); }").expect("compiles");
        assert!(matches!(hir.functions[1].body[0], Stmt::CallFor { .. }));
    }

    #[test]
    fn a_function_gets_an_id_and_a_datapack_path() {
        let hir = lower_ok("fn main() {}");
        assert_eq!(hir.functions.len(), 1);
        assert_eq!(hir.functions[0].id, FnId(0));
        assert_eq!(hir.functions[0].name, "main");
        assert_eq!(hir.functions[0].path, "myns:main");
    }

    #[test]
    fn ids_are_assigned_in_source_order() {
        let hir = lower_ok("fn a() {} fn b() {}");
        assert_eq!(
            hir.functions.iter().map(|f| f.id).collect::<Vec<_>>(),
            vec![FnId(0), FnId(1)]
        );
    }

    #[test]
    fn raw_carries_the_command_text() {
        let hir = lower_ok(r#"fn main() { raw!("say hi"); }"#);
        let Stmt::Raw(raw) = &hir.functions[0].body[0] else {
            panic!("expected a raw command")
        };
        assert_eq!(raw.text, "say hi");
    }

    #[test]
    fn an_unknown_macro_is_reported() {
        let errors = lower_err("fn main() { nope!(1); }");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("nope"), "{errors:?}");
    }

    #[test]
    fn raw_wants_exactly_one_string() {
        assert!(!lower_err("fn main() { raw!(1); }").is_empty());
        assert!(!lower_err(r#"fn main() { raw!("a", "b"); }"#).is_empty());
        assert!(!lower_err("fn main() { raw!(); }").is_empty());
    }

    #[test]
    fn two_functions_with_one_name_collide() {
        let errors = lower_err("fn main() {} fn main() {}");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("main"), "{errors:?}");
    }

    #[test]
    fn known_attributes_are_recorded() {
        let hir = lower_ok("#[tick] fn main() {}");
        assert_eq!(hir.functions[0].attrs, vec![Attr::Tick]);
    }

    #[test]
    fn an_unknown_attribute_is_an_error_rather_than_being_ignored() {
        // Silently ignoring a typo'd attribute is the failure mode minewell exists to
        // remove. `#[tik]` must not quietly do nothing.
        let errors = lower_err("#[tik] fn main() {}");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("tik"), "{errors:?}");
    }

    fn only_fn(hir: &Hir) -> &Function {
        &hir.functions[0]
    }

    #[test]
    fn a_let_takes_the_type_of_its_initialiser() {
        let hir = lower_ok("fn main() { let x = 1; let b = true; }");
        let locals = &only_fn(&hir).locals;
        assert_eq!(locals[0].ty, Type::I32);
        assert_eq!(locals[1].ty, Type::Bool);
        assert!(!locals[0].mutable);
    }

    #[test]
    fn an_annotation_must_agree_with_the_initialiser() {
        assert!(
            lower_err("fn main() { let x: i32 = true; }")[0]
                .message
                .contains("expected i32")
        );
        assert!(
            lower_ok("fn main() { let x: bool = true; }").functions[0].locals[0]
                .mutable
                .eq(&false)
        );
    }

    #[test]
    fn an_unknown_type_is_reported() {
        assert!(
            lower_err("fn main() { let x: i64 = 1; }")[0]
                .message
                .contains("i64")
        );
    }

    #[test]
    fn a_binding_must_exist_before_it_is_used() {
        assert!(
            lower_err("fn main() { let x = y; }")[0]
                .message
                .contains("'y'")
        );
    }

    #[test]
    fn an_inner_let_shadows_an_outer_one() {
        // Two distinct locals, the second hiding the first.
        let hir = lower_ok("fn main() { let x = 1; let x = true; let y = x; }");
        let f = only_fn(&hir);
        assert_eq!(f.locals.len(), 3);
        assert_eq!(f.locals[2].ty, Type::Bool);
    }

    #[test]
    fn arithmetic_needs_integers() {
        assert!(
            lower_err("fn main() { let x = 1 + true; }")[0]
                .message
                .contains("i32")
        );
    }

    #[test]
    fn logic_needs_booleans() {
        assert!(
            lower_err("fn main() { let x = 1 && 2; }")[0]
                .message
                .contains("bool")
        );
    }

    #[test]
    fn comparison_yields_a_bool() {
        let hir = lower_ok("fn main() { let x = 1 < 2; }");
        assert_eq!(only_fn(&hir).locals[0].ty, Type::Bool);
    }

    #[test]
    fn equality_works_on_either_type_but_not_across_them() {
        assert!(
            lower_ok("fn main() { let a = true == false; }")
                .functions
                .len()
                .eq(&1)
        );
        assert!(
            lower_err("fn main() { let a = 1 == true; }")[0]
                .message
                .contains("cannot compare")
        );
    }

    #[test]
    fn assigning_needs_mut() {
        assert!(
            lower_err("fn main() { let x = 1; x = 2; }")[0]
                .message
                .contains("not mutable")
        );
        assert!(
            lower_ok("fn main() { let mut x = 1; x = 2; }")
                .functions
                .len()
                == 1
        );
    }

    #[test]
    fn an_assignment_keeps_the_declared_type() {
        assert!(
            lower_err("fn main() { let mut x = 1; x = true; }")[0]
                .message
                .contains("expected i32")
        );
    }

    #[test]
    fn compound_assignment_is_arithmetic_and_wants_integers() {
        assert!(
            lower_ok("fn main() { let mut x = 1; x += 2; }")
                .functions
                .len()
                == 1
        );
        assert!(
            lower_err("fn main() { let mut b = true; b += 1; }")[0]
                .message
                .contains("i32")
        );
    }

    #[test]
    fn an_assignment_is_not_a_value() {
        assert!(
            lower_err("fn main() { let mut x = 1; let y = x = 2; }")[0]
                .message
                .contains("produces no value")
        );
    }

    #[test]
    fn an_expression_with_no_effect_is_reported() {
        // M2 expressions are pure, so evaluating one for its effect achieves nothing.
        assert!(
            lower_err("fn main() { 1 + 1; }")[0]
                .message
                .contains("no effect")
        );
    }

    #[test]
    fn a_condition_must_be_bool() {
        // No truthiness: `if x` and `if x != 0` should not be two spellings of one
        // thing, with the type error hidden in between.
        assert!(
            lower_err("fn main() { if 1 { } }")[0]
                .message
                .contains("must be bool")
        );
        assert!(lower_ok("fn main() { if true { } }").functions.len() == 1);
    }

    #[test]
    fn break_and_continue_need_a_loop() {
        assert!(
            lower_err("fn main() { break; }")[0]
                .message
                .contains("break")
        );
        assert!(
            lower_err("fn main() { continue; }")[0]
                .message
                .contains("continue")
        );
        assert!(lower_ok("fn main() { loop { break; } }").functions.len() == 1);
    }

    #[test]
    fn break_inside_an_if_inside_a_loop_is_fine() {
        assert!(
            lower_ok("fn main() { loop { if true { break; } } }")
                .functions
                .len()
                == 1
        );
    }

    #[test]
    fn return_needs_no_loop() {
        assert!(lower_ok("fn main() { return; }").functions.len() == 1);
    }

    #[test]
    fn inline_only_applies_where_it_could_work() {
        assert!(
            lower_err("fn main() { #[inline] if true { } else { } }")[0]
                .message
                .contains("single-statement")
        );
        assert!(
            lower_err("fn main() { #[inline] while true { } }")[0]
                .message
                .contains("always its own function")
        );
    }

    #[test]
    fn a_call_checks_arity_and_types() {
        let prelude = "fn f(a: i32, b: bool) -> i32 { return 1; } ";
        assert!(
            lower_err(&format!("{prelude} fn main() {{ let x = f(1); }}"))[0]
                .message
                .contains("2 argument")
        );
        assert!(
            lower_err(&format!("{prelude} fn main() {{ let x = f(1, 2); }}"))[0]
                .message
                .contains("expected bool")
        );
        assert!(
            lower_ok(&format!("{prelude} fn main() {{ let x = f(1, true); }}"))
                .functions
                .len()
                == 2
        );
    }

    #[test]
    fn calling_something_undefined_is_reported() {
        assert!(
            lower_err("fn main() { let x = nope(); }")[0]
                .message
                .contains("'nope'")
        );
    }

    #[test]
    fn a_function_may_be_called_before_it_is_defined() {
        assert!(
            lower_ok("fn main() { let x = later(); } fn later() -> i32 { return 1; }")
                .functions
                .len()
                == 2
        );
    }

    #[test]
    fn a_void_function_has_no_value_to_use() {
        assert!(
            lower_err("fn v() {} fn main() { let x = v(); }")[0]
                .message
                .contains("does not return a value")
        );
        // As a statement it is fine.
        assert!(lower_ok("fn v() {} fn main() { v(); }").functions.len() == 2);
    }

    #[test]
    fn a_returning_function_must_actually_return() {
        assert!(
            lower_err("fn f() -> i32 { let x = 1; }")[0]
                .message
                .contains("without returning")
        );
        // Both sides of an if/else returning is enough.
        assert!(
            lower_ok("fn f() -> i32 { if true { return 1; } else { return 2; } }")
                .functions
                .len()
                == 1
        );
    }

    #[test]
    fn returning_the_wrong_type_is_reported() {
        assert!(
            lower_err("fn f() -> i32 { return true; }")[0]
                .message
                .contains("expected i32")
        );
        assert!(
            lower_err("fn f() { return 1; }")[0]
                .message
                .contains("does not return a value")
        );
        assert!(
            lower_err("fn f() -> i32 { return; }")[0]
                .message
                .contains("expected a i32")
        );
    }

    // The checks below are why minewell exists: vanilla cannot detect any of them,
    // and every one of them fails silently at runtime.

    #[test]
    fn calling_a_function_that_needs_an_executor_without_one_is_an_error() {
        let errors = lower_err(
            "#[ctx(entity)] fn hurt() {}
             fn main() { hurt(); }",
        );
        assert!(errors[0].message.contains("entity"), "{errors:?}");
        assert!(
            errors[0].message.contains("as") && errors[0].message.contains("#[ctx(entity)]"),
            "the diagnostic should say both ways out: {errors:?}"
        );
    }

    #[test]
    fn an_as_block_supplies_the_executor() {
        assert!(
            lower_ok(
                "#[ctx(entity)] fn hurt() {}
                 fn main() { as @e[type=zombie] { hurt(); } }"
            )
            .functions
            .len()
                == 2
        );
    }

    #[test]
    fn declaring_the_context_passes_the_requirement_to_the_caller() {
        assert!(
            lower_ok(
                "#[ctx(entity)] fn hurt() {}
                 #[ctx(entity)] fn wrapper() { hurt(); }"
            )
            .functions
            .len()
                == 2
        );
        // ...and the caller of *that* still has to supply it.
        assert!(
            !lower_err(
                "#[ctx(entity)] fn hurt() {}
             #[ctx(entity)] fn wrapper() { hurt(); }
             fn main() { wrapper(); }"
            )
            .is_empty()
        );
    }

    #[test]
    fn a_for_loop_supplies_the_executor_too() {
        assert!(
            lower_ok(
                "#[ctx(entity)] fn hurt() {}
                 fn main() { for z in @e[type=zombie] { hurt(); } }"
            )
            .functions
            .len()
                == 2
        );
    }

    #[test]
    fn at_supplies_position_and_not_entity() {
        assert!(
            !lower_err(
                "#[ctx(entity)] fn hurt() {}
             fn main() { at @e[type=zombie] { hurt(); } }"
            )
            .is_empty()
        );
        assert!(
            lower_ok(
                "#[ctx(position)] fn place() {}
                 fn main() { at @e[type=zombie] { place(); } }"
            )
            .functions
            .len()
                == 2
        );
    }

    #[test]
    fn at_s_needs_an_executor_to_be_at() {
        assert!(!lower_err("fn main() { at @s { } }").is_empty());
        assert!(
            lower_ok("fn main() { as @e[type=zombie] { at @s { } } }")
                .functions
                .len()
                == 1
        );
    }

    #[test]
    fn a_tick_function_cannot_require_a_context() {
        // A function tag invokes with no executor, so this would silently do nothing.
        let errors = lower_err("#[tick] #[ctx(entity)] fn t() {}");
        assert!(errors[0].message.contains("silently"), "{errors:?}");
    }

    #[test]
    fn the_context_ends_with_the_block() {
        assert!(
            !lower_err(
                "#[ctx(entity)] fn hurt() {}
             fn main() { as @e[type=zombie] { } hurt(); }"
            )
            .is_empty()
        );
    }

    #[test]
    fn a_selector_can_be_named() {
        assert!(
            lower_ok("fn main() { let zombies = @e[type=zombie]; as zombies { } }")
                .functions
                .len()
                == 1
        );
    }

    #[test]
    fn a_selector_is_not_a_value() {
        assert!(!lower_err("fn main() { let x = @s == @s; }").is_empty());
        assert!(!lower_err("fn f(s: i32) {} fn main() { f(@s); }").is_empty());
    }

    #[test]
    fn an_unknown_context_kind_is_reported() {
        assert!(
            lower_err("#[ctx(dimension)] fn f() {}")[0]
                .message
                .contains("dimension")
        );
    }

    #[test]
    fn lowering_reports_every_problem_it_finds() {
        let errors = lower_err("#[tik] fn main() { nope!(); }");
        assert_eq!(errors.len(), 2);
    }
}
