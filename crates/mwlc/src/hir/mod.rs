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

use crate::syntax::SyntaxError;
use crate::syntax::ast::{self, BinaryOp, Expr as AstExpr, ItemKind, SourceFile, UnaryOp};
use crate::syntax::lexer::{Span, TokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FnId(pub u32);

/// Identifies a binding within one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    I32,
    Bool,
}

impl Type {
    fn parse(name: &str) -> Option<Type> {
        match name {
            "i32" => Some(Type::I32),
            "bool" => Some(Type::Bool),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Type::I32 => "i32",
            Type::Bool => "bool",
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
    Break(Span),
    Continue(Span),
    Return(Span),
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
    Local(LocalId),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attr {
    Tick,
    Load,
    Inline,
    NoInline,
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
const PLANNED_ATTRS: &[&str] = &["ctx", "score", "storage", "nbt", "unroll", "derive"];

pub fn lower(file: &SourceFile, namespace: &str) -> (Hir, Vec<SyntaxError>) {
    let mut errors = Vec::new();
    let mut functions: Vec<Function> = Vec::new();
    for item in &file.items {
        let ItemKind::Fn(f) = &item.kind;
        if let Some(previous) = functions
            .iter()
            .find(|existing| existing.name == f.name.name)
        {
            let name = &f.name.name;
            let at = previous.span.start;
            errors.push(SyntaxError::new(
                f.name.span,
                format!("a function named '{name}' is already defined (at byte {at})"),
            ));
            continue;
        }
        let mut cx = FnLowering {
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
            loop_depth: 0,
            errors: &mut errors,
        };
        let attrs = cx.attrs(&item.attrs);
        let body = cx.block(&f.body);
        let locals = cx.locals;
        functions.push(Function {
            id: FnId(functions.len() as u32),
            name: f.name.name.clone(),
            path: format!("{namespace}:{}", f.name.name),
            attrs,
            locals,
            body,
            span: item.span,
        });
    }
    (Hir { functions }, errors)
}

struct FnLowering<'a> {
    locals: Vec<Local>,
    /// Innermost scope last. A `let` shadows an outer binding of the same name.
    scopes: Vec<HashMap<String, LocalId>>,
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
            // Every other expression is pure, so evaluating one for its effect is
            // asking for nothing to happen. Say so rather than emit dead commands.
            ast::Stmt::Expr(other) => {
                self.error(other.span(), "this expression has no effect");
                None
            }
            ast::Stmt::If(if_stmt) => self.if_stmt(if_stmt),
            ast::Stmt::Loop(loop_stmt) => self.loop_stmt(loop_stmt),
            ast::Stmt::Break(span) => self.jump(*span, "break").map(|()| Stmt::Break(*span)),
            ast::Stmt::Continue(span) => {
                self.jump(*span, "continue").map(|()| Stmt::Continue(*span))
            }
            ast::Stmt::Return(span) => Some(Stmt::Return(*span)),
        }
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
        let body = self.block(&stmt.body);
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
            AstExpr::Macro(call) => {
                let name = &call.name.name;
                self.error(span, format!("'{name}!' does not produce a value"));
                None
            }
        }
    }

    fn binary_type(&mut self, op: BinaryOp, lhs: &Expr, rhs: &Expr, span: Span) -> Option<Type> {
        use BinaryOp::*;
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
        let (hir, errors) = lower(&file, "myns");
        assert!(errors.is_empty(), "{errors:?}");
        hir
    }

    fn lower_err(src: &str) -> Vec<SyntaxError> {
        let (file, errors) = parse(src);
        assert!(errors.is_empty(), "{errors:?}");
        lower(&file, "myns").1
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
    fn lowering_reports_every_problem_it_finds() {
        let errors = lower_err("#[tik] fn main() { nope!(); }");
        assert_eq!(errors.len(), 2);
    }
}
