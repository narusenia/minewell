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
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Macro(MacroCall),
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
