// SPDX-License-Identifier: MIT

//! The abstract syntax tree.
//!
//! Follows `docs/02-spec.md` section 3, which currently covers only what M1 needs:
//! functions, blocks, statements and macro calls. Every node carries a span, because
//! diagnostics and the `# src/foo.mwl:42` comments in debug output both need one.

use super::Span;
use super::lexer::Token;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub attrs: Vec<Attribute>,
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemKind {
    Fn(FnItem),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnItem {
    pub name: Ident,
    pub body: Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Let(LetStmt),
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetStmt {
    pub mutable: bool,
    pub name: Ident,
    pub ty: Option<TypeName>,
    pub value: Expr,
    pub span: Span,
}

/// A written type. Resolved to a real type in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeName {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Int(IntLit),
    Bool(BoolLit),
    /// A reference to a binding.
    Path(Ident),
    Unary(UnaryExpr),
    Binary(BinaryExpr),
    Assign(AssignExpr),
    Macro(MacroCall),
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(e) => e.span,
            Expr::Bool(e) => e.span,
            Expr::Path(e) => e.span,
            Expr::Unary(e) => e.span,
            Expr::Binary(e) => e.span,
            Expr::Assign(e) => e.span,
            Expr::Macro(e) => e.span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntLit {
    pub value: i32,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoolLit {
    pub value: bool,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnaryExpr {
    pub op: UnaryOp,
    pub operand: Box<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryExpr {
    pub op: BinaryOp,
    pub lhs: Box<Expr>,
    pub rhs: Box<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignExpr {
    /// `None` for plain `=`; otherwise the arithmetic to apply first.
    pub op: Option<BinaryOp>,
    pub target: Ident,
    pub value: Box<Expr>,
    pub span: Span,
}

/// The arguments are kept as tokens. Each built-in macro has its own grammar, so
/// there is nothing general to parse them into (`docs/02-spec.md` section 2.9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroCall {
    pub name: Ident,
    pub tokens: Vec<Token>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub tokens: Vec<Token>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}
