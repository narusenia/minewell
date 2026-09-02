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
    /// `impl Point { fn bump(&mut self) { .. } }`. Inherent methods only.
    Impl(ImplItem),
    Struct(StructItem),
    Enum(EnumItem),
}

/// `enum State { Idle, Chasing { target: i32 } }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumItem {
    pub name: Ident,
    pub variants: Vec<VariantDef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantDef {
    pub name: Ident,
    /// Empty for a unit variant. Variants name their fields; there is no tuple form.
    pub fields: Vec<FieldDef>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplItem {
    pub ty: Ident,
    pub methods: Vec<Item>,
}

/// `struct Point { x: i32, y: i32 }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructItem {
    pub name: Ident,
    /// Type parameters, monomorphised at every use (spec section 3.14).
    pub generics: Vec<GenericParam>,
    pub fields: Vec<FieldDef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDef {
    /// `#[nbt(..)]` and friends. Carried so an unknown one can be reported (M7-3).
    pub attrs: Vec<Attribute>,
    pub name: Ident,
    pub ty: TypeName,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnItem {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    /// The receiver, for a method: `&self`, `&mut self` or `self`.
    pub receiver: Option<Receiver>,
    pub params: Vec<Param>,
    pub ret: Option<TypeName>,
    pub body: Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receiver {
    /// `None` means `self` by value, which is a copy.
    pub borrow: Option<Borrow>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: Ident,
    pub ty: TypeName,
    pub span: Span,
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
    If(IfStmt),
    /// `while c { .. }` and `loop { .. }`; the latter has no condition.
    Loop(LoopStmt),
    /// `as <sel> { }`, `at <sel> { }`, `positioned <pos> { }` and
    /// `for e in <sel> { }`.
    Context(ContextStmt),
    Match(MatchStmt),
    Break(Span),
    Continue(Span),
    Return {
        value: Option<Expr>,
        span: Span,
    },
}

/// `match s { State::Idle => { .. } _ => { .. } }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchStmt {
    pub scrutinee: Expr,
    pub arms: Vec<MatchArm>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// `State::Chasing { target }`: the variant, and the payload fields to bind.
    Variant {
        ty: Ident,
        variant: Ident,
        binds: Vec<Ident>,
        span: Span,
    },
    /// `Some(x)`: built in, because `Option` is (spec section 3.18).
    Some {
        bind: Ident,
        span: Span,
    },
    /// `None`.
    None(Span),
    Wildcard(Span),
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::Variant { span, .. } | Pattern::Some { span, .. } => *span,
            Pattern::None(span) | Pattern::Wildcard(span) => *span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfStmt {
    pub attrs: Vec<Attribute>,
    pub cond: Expr,
    pub then: Block,
    pub otherwise: Option<Box<Else>>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Else {
    Block(Block),
    If(IfStmt),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextStmt {
    pub attrs: Vec<Attribute>,
    pub kind: ContextKind,
    /// The binding `for` introduces.
    pub binding: Option<Ident>,
    pub selector: Expr,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextKind {
    As,
    At,
    For,
    /// `positioned <pos>`: a position with no entity behind it.
    Positioned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopStmt {
    pub attrs: Vec<Attribute>,
    pub cond: Option<Expr>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetStmt {
    pub mutable: bool,
    pub name: Ident,
    pub ty: Option<TypeName>,
    pub value: Expr,
    pub span: Span,
}

/// A generic parameter as written: a type, or a `const` one that stands for a scale
/// (spec section 3.16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericParam {
    pub name: Ident,
    /// `<const S: i32>`. A const parameter is only ever a scale, so it has no type of
    /// its own to carry.
    pub is_const: bool,
}

/// `fix<1000>` / `fix<S>`: the scale of a fixed-point type, and the only const
/// argument the language has (spec section 3.16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaleArg {
    Int(IntLit),
    Param(Ident),
}

impl ScaleArg {
    pub fn span(&self) -> Span {
        match self {
            ScaleArg::Int(lit) => lit.span,
            ScaleArg::Param(name) => name.span,
        }
    }
}

/// A written type. Resolved to a real type in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeName {
    /// `&T` / `&mut T`: compile-time only, and legal on a parameter alone.
    pub borrow: Option<Borrow>,
    pub name: String,
    /// `Vec<i32>`: the arguments between the angle brackets.
    pub args: Vec<TypeName>,
    /// `fix<1000>`: the scale, for the one type that takes a const argument.
    pub scale: Option<ScaleArg>,
    pub span: Span,
}

/// How something is borrowed. There is no runtime difference; the distinction is what
/// lets a write through a shared borrow be an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Borrow {
    Shared,
    Mutable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Int(IntLit),
    Bool(BoolLit),
    Str(StrLit),
    /// A reference to a binding.
    Path(Ident),
    Unary(UnaryExpr),
    Binary(BinaryExpr),
    Assign(AssignExpr),
    Call(CallExpr),
    Selector(SelectorLit),
    Resource(ResourceLit),
    Macro(MacroCall),
    Struct(StructLit),
    Field(FieldExpr),
    List(ListLit),
    Index(IndexExpr),
    Method(MethodCall),
    /// `&p` / `&mut p`, which only an argument can be.
    Borrow(BorrowExpr),
    /// `fix::<1000>(1500)`.
    Fix(FixExpr),
    /// `1..3`, which only `slice` takes.
    Range(RangeExpr),
    /// `o?`: the value, or leave the function with nothing (spec section 3.18).
    Try(TryExpr),
    /// `Mob::of(@s)`: a view of an entity's NBT (spec section 3.19).
    ViewOf(ViewOfExpr),
    /// `debug_assert!(c, "m")`: a check that only debug builds carry
    /// (spec section 3.20).
    Assert(AssertExpr),
    /// `text!(a, b)`: a chat component put together while compiling
    /// (spec section 3.22).
    Text(TextMacro),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMacro {
    pub args: Vec<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssertExpr {
    pub cond: Box<Expr>,
    pub message: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewOfExpr {
    pub ty: Ident,
    pub selector: Box<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TryExpr {
    pub value: Box<Expr>,
    pub span: Span,
}

/// `a..b`, with either end able to be left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeExpr {
    pub start: Option<Box<Expr>>,
    pub end: Option<Box<Expr>>,
    pub span: Span,
}

/// `fix::<S>(e)`: the only way to make a fixed-point value (spec section 3.16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixExpr {
    pub scale: ScaleArg,
    pub value: Box<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorrowExpr {
    pub borrow: Borrow,
    pub place: Box<Expr>,
    pub span: Span,
}

/// `[1, 2, 3]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListLit {
    pub values: Vec<Expr>,
    pub span: Span,
}

/// `v[i]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexExpr {
    pub base: Box<Expr>,
    pub index: Box<Expr>,
    pub span: Span,
}

/// `v.push(x)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodCall {
    pub receiver: Box<Expr>,
    pub name: Ident,
    pub args: Vec<Expr>,
    pub span: Span,
}

/// `p.x`, and `o.inner.a` by nesting. The base is an expression so that the parser
/// stays simple; HIR is where "only a binding can be addressed" is decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldExpr {
    pub base: Box<Expr>,
    pub name: Ident,
    pub span: Span,
}

/// `Point { x: 1, y: 2 }`, and `State::Chasing { target: 3 }` with a variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructLit {
    pub name: Ident,
    /// The variant, for an `enum`. `None` means the type itself.
    pub variant: Option<Ident>,
    pub fields: Vec<FieldInit>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInit {
    pub name: Ident,
    pub value: Expr,
    pub span: Span,
}

/// `@e[type=zombie]`, body verbatim. Its contents are the game's grammar, not this
/// language's, so they are carried through untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorLit {
    pub text: String,
    pub span: Span,
}

/// `minecraft:stone`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLit {
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallExpr {
    pub callee: Ident,
    pub args: Vec<Expr>,
    pub span: Span,
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(e) => e.span,
            Expr::Fix(e) => e.span,
            Expr::Range(e) => e.span,
            Expr::Try(e) => e.span,
            Expr::ViewOf(e) => e.span,
            Expr::Assert(e) => e.span,
            Expr::Text(e) => e.span,
            Expr::Bool(e) => e.span,
            Expr::Str(e) => e.span,
            Expr::Path(e) => e.span,
            Expr::Unary(e) => e.span,
            Expr::Binary(e) => e.span,
            Expr::Assign(e) => e.span,
            Expr::Call(e) => e.span,
            Expr::Selector(e) => e.span,
            Expr::Resource(e) => e.span,
            Expr::Macro(e) => e.span,
            Expr::Struct(e) => e.span,
            Expr::Field(e) => e.span,
            Expr::List(e) => e.span,
            Expr::Index(e) => e.span,
            Expr::Method(e) => e.span,
            Expr::Borrow(e) => e.span,
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

/// Only meaningful as a command argument today; `String` values arrive in M8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrLit {
    pub value: String,
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
    /// A binding, or a field reached from one. Checked in HIR.
    pub target: Box<Expr>,
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
