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

use std::cell::RefCell;
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

/// Identifies a `struct` definition within the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StructId(pub u32);

/// Identifies an `enum` definition within the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StructOrEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EnumId(pub u32);

/// Identifies one `Vec<T>`, interned by element type.
///
/// `Type` is `Copy`, so a type that contains another cannot hold it inline. Interning
/// keeps the id small and makes `Vec<i32> == Vec<i32>` a comparison of two numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VecId(pub u32);

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
    /// A composite value. It lives in storage rather than in a register, which is a
    /// third category: neither a score nor compile-time only (spec section 5).
    Struct(StructId),
    /// A tagged union, also in storage (spec section 4.9).
    Enum(EnumId),
    /// An NBT list in storage (spec section 4.11).
    Vec(VecId),
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

    /// Whether values of this type live in storage rather than on the scoreboard.
    pub fn is_storage(&self) -> bool {
        matches!(self, Type::Struct(_) | Type::Enum(_) | Type::Vec(_))
    }

    pub fn name(&self) -> &'static str {
        match self {
            Type::I32 => "i32",
            Type::Bool => "bool",
            Type::Selector => "selector",
            Type::Resource => "ResourceLocation",
            Type::Pos => "Pos",
            // Only reachable where the type table is out of reach; every diagnostic
            // that can name the type goes through `Types::name_of` instead.
            Type::Struct(_) => "struct",
            Type::Enum(_) => "enum",
            Type::Vec(_) => "Vec",
        }
    }
}

/// Every type the program defines, and the names they answer to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Types {
    pub structs: Vec<StructDef>,
    pub enums: Vec<EnumDef>,
    by_name: HashMap<String, Type>,
    /// Element type per `VecId`. Interned on demand, which is why it is behind a cell:
    /// a list literal creates its type while the table is only borrowed to read.
    vecs: RefCell<Vec<Type>>,
}

impl Types {
    pub fn get(&self, name: &str) -> Option<Type> {
        self.by_name.get(name).copied()
    }

    pub fn struct_def(&self, id: StructId) -> &StructDef {
        &self.structs[id.0 as usize]
    }

    pub fn enum_def(&self, id: EnumId) -> &EnumDef {
        &self.enums[id.0 as usize]
    }

    /// A type as a diagnostic should spell it, which for a user type is its own name.
    pub fn name_of(&self, ty: Type) -> String {
        match ty {
            Type::Struct(id) => self.struct_def(id).name.clone(),
            Type::Enum(id) => self.enum_def(id).name.clone(),
            Type::Vec(id) => format!("Vec<{}>", self.name_of(self.element(id))),
            other => other.name().to_owned(),
        }
    }

    /// `Vec<elem>`, interned so that the same list type is the same id.
    pub fn vec_of(&self, elem: Type) -> Type {
        let mut vecs = self.vecs.borrow_mut();
        let index = match vecs.iter().position(|known| *known == elem) {
            Some(index) => index,
            None => {
                vecs.push(elem);
                vecs.len() - 1
            }
        };
        Type::Vec(VecId(index as u32))
    }

    /// What a list holds.
    pub fn element(&self, id: VecId) -> Type {
        self.vecs.borrow()[id.0 as usize]
    }

    /// The fields a composite type holds, across every variant of an `enum`.
    fn fields(&self, ty: Type) -> Vec<&Field> {
        match ty {
            Type::Struct(id) => self.struct_def(id).fields.iter().collect(),
            Type::Enum(id) => self
                .enum_def(id)
                .variants
                .iter()
                .flat_map(|variant| variant.fields.iter())
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// A value addressed by name: a binding, or a field reached from one.
///
/// Composite values live in storage, where "where is it" is a path rather than a
/// register, and a field is the same path with one more step (spec section 6.18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub local: LocalId,
    /// The steps from the binding to the value, in order.
    pub steps: Vec<Step>,
    /// The type of the value addressed, which is the innermost step's.
    pub ty: Type,
    /// The tag it is stored as; `None` for a compound or a list.
    pub tag: Option<NbtTag>,
}

/// One step of a path: into a field, or into a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// An NBT key.
    Field(String),
    /// `v[2]`: known now, so it is part of the path.
    Index(i32),
    /// `v[i]`: known only at runtime, so the path has to be built by a macro
    /// (spec section 6.21). Allowed as the last step only.
    At(Box<Expr>),
}

impl Place {
    /// Whether the whole path is known while compiling.
    pub fn is_static(&self) -> bool {
        !self.steps.iter().any(|step| matches!(step, Step::At(_)))
    }
}

/// A `struct` definition: an NBT compound with a known shape (spec section 4.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDef {
    pub id: StructId,
    pub name: String,
    pub fields: Vec<Field>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// The name the source writes.
    pub name: String,
    /// The key in the compound, which `#[nbt(rename = "..")]` can change.
    pub nbt: String,
    pub ty: Type,
    /// `None` for a composite field: a compound has no scalar tag.
    pub tag: Option<NbtTag>,
}

/// The NBT tag a scalar field is stored as.
///
/// It has to be part of the type, not a detail of emission: vanilla treats `Byte(1)`
/// and `Int(1)` as different values and ignores the wrong one without a word
/// (requirements section 4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NbtTag {
    Byte,
    Short,
    Int,
    Long,
}

impl NbtTag {
    fn parse(name: &str) -> Option<NbtTag> {
        Some(match name {
            "byte" => NbtTag::Byte,
            "short" => NbtTag::Short,
            "int" => NbtTag::Int,
            "long" => NbtTag::Long,
            _ => return None,
        })
    }

    /// As `execute store result storage` spells it.
    pub fn keyword(self) -> &'static str {
        match self {
            NbtTag::Byte => "byte",
            NbtTag::Short => "short",
            NbtTag::Int => "int",
            NbtTag::Long => "long",
        }
    }

    /// As SNBT spells a literal of this tag.
    pub fn suffix(self) -> &'static str {
        match self {
            NbtTag::Byte => "b",
            NbtTag::Short => "s",
            NbtTag::Int => "",
            NbtTag::Long => "L",
        }
    }

    /// The tag a type is written as when nothing says otherwise.
    pub fn default_for(ty: Type) -> Option<NbtTag> {
        match ty {
            Type::I32 => Some(NbtTag::Int),
            Type::Bool => Some(NbtTag::Byte),
            _ => None,
        }
    }
}

impl StructDef {
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }
}

/// An `enum` definition: a compound whose `tag` says which variant it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDef {
    pub id: EnumId,
    pub name: String,
    pub variants: Vec<Variant>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Field>,
}

impl EnumDef {
    pub fn variant(&self, name: &str) -> Option<(u32, &Variant)> {
        self.variants
            .iter()
            .position(|v| v.name == name)
            .map(|index| (index as u32, &self.variants[index]))
    }
}

/// The key a variant's name is stored under (requirements section 4.2).
pub const TAG_KEY: &str = "tag";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hir {
    pub functions: Vec<Function>,
    pub types: Types,
    /// Ids the program names but does not define. Checked once the datapack is known
    /// (`driver`), because whether one resolves depends on files this stage cannot see.
    pub references: Vec<Reference>,
}

/// A reference to something that has to exist somewhere in the pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub id: String,
    pub kind: RefKind,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Function,
}

impl RefKind {
    pub fn directory(&self) -> &'static str {
        match self {
            RefKind::Function => "function",
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            RefKind::Function => "mcfunction",
        }
    }
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
    /// `match`: one arm per variant, each its own guarded block.
    Match {
        scrutinee: Place,
        arms: Vec<Arm>,
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
    /// `v.push(e)`.
    Push {
        place: Place,
        value: Expr,
        span: Span,
    },
    Assign {
        place: Place,
        /// `None` for `=`; otherwise the arithmetic to apply first.
        op: Option<BinaryOp>,
        value: Expr,
        span: Span,
    },
}

/// One arm of a `match`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm {
    /// Which variant this arm is for; `None` for `_`.
    pub variant: Option<u32>,
    /// The name this arm is generated under, already safe as a datapack path.
    pub path: String,
    pub bindings: Vec<Binding>,
    pub body: Vec<Stmt>,
}

/// A payload field bound by a pattern, copied out of the compound on arm entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub local: LocalId,
    /// The key to read, under the scrutinee's path.
    pub nbt: String,
    pub ty: Type,
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
    /// A composite value, its fields in declaration order.
    Struct {
        id: StructId,
        fields: Vec<Expr>,
    },
    /// Reading a binding's field.
    Field(Place),
    /// A tagged union value: which variant, and that variant's fields in order.
    Enum {
        id: EnumId,
        variant: u32,
        fields: Vec<Expr>,
    },
    /// `[1, 2, 3]`.
    List {
        elem: Type,
        values: Vec<Expr>,
    },
    /// `v.len()`.
    Len(Place),
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
    let mut references = Vec::new();
    let types = collect_types(file, &mut errors);
    let mut signatures: HashMap<String, Signature> = HashMap::new();
    let mut items: Vec<(&ast::Item, &ast::FnItem)> = Vec::new();

    // First pass: signatures only. Without it, calling a function defined further down
    // the file would be an error — a rule about text order rather than about programs.
    for item in &file.items {
        let ItemKind::Fn(f) = &item.kind else {
            continue;
        };
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
            .map(|param| resolve_type(&param.ty, &types, &mut errors).unwrap_or(Type::I32))
            .collect();
        let ret = f
            .ret
            .as_ref()
            .and_then(|written| resolve_type(written, &types, &mut errors));
        // Vanilla's function return is a single integer, so there is nowhere for a
        // compound to come back in.
        if let (Some(ty), Some(written)) = (ret, f.ret.as_ref())
            && ty.is_storage()
        {
            errors.push(SyntaxError::new(
                written.span,
                "returning a composite value is not implemented yet: a function's \
                 return value is a single number, so a compound has nowhere to come \
                 back in",
            ));
        }
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
            types: &types,
            scopes: vec![HashMap::new()],
            selector_aliases: HashMap::new(),
            provided: Vec::new(),
            in_entity_loop: false,
            loop_depth: 0,
            ret: signature.ret,
            signatures: &signatures,
            toolchain,
            references: &mut references,
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
    (
        Hir {
            functions,
            types,
            references,
        },
        errors,
    )
}

/// The program's `struct` and `enum` definitions.
///
/// Two passes: the names first, so a field can refer to a type declared further down
/// the file, then the fields.
fn collect_types(file: &SourceFile, errors: &mut Vec<SyntaxError>) -> Types {
    let mut types = Types::default();
    let mut structs = Vec::new();
    let mut enums = Vec::new();
    for item in &file.items {
        let (name, ty) = match &item.kind {
            ItemKind::Struct(declared) => {
                let ty = Type::Struct(StructId(structs.len() as u32));
                structs.push((item, declared));
                (&declared.name, ty)
            }
            ItemKind::Enum(declared) => {
                let ty = Type::Enum(EnumId(enums.len() as u32));
                enums.push((item, declared));
                (&declared.name, ty)
            }
            ItemKind::Fn(_) => continue,
        };
        if types.by_name.contains_key(&name.name) {
            let text = &name.name;
            errors.push(SyntaxError::new(
                name.span,
                format!("a type named '{text}' is already defined"),
            ));
            continue;
        }
        types.by_name.insert(name.name.clone(), ty);
    }

    for (item, declared) in structs {
        reject_item_attrs(item, "a struct", errors);
        let fields = collect_fields(&declared.fields, &types, errors);
        types.structs.push(StructDef {
            id: StructId(types.structs.len() as u32),
            name: declared.name.name.clone(),
            fields,
            span: declared.name.span,
        });
    }
    for (item, declared) in enums {
        reject_item_attrs(item, "an enum", errors);
        let variants = declared
            .variants
            .iter()
            .map(|variant| {
                let fields = collect_fields(&variant.fields, &types, errors);
                // The tag shares the compound with the payload, so no field can be
                // stored under its key.
                if let Some(clash) = fields.iter().find(|f| f.nbt == TAG_KEY) {
                    let name = &clash.name;
                    errors.push(SyntaxError::new(
                        variant.span,
                        format!(
                            "'{name}' collides with the '{TAG_KEY}' the variant is stored under"
                        ),
                    ));
                }
                Variant {
                    name: variant.name.name.clone(),
                    fields,
                }
            })
            .collect();
        // Arms become functions named after their variant, and a datapack path is
        // lowercase only (spec section 6.20), so two variants that differ in case
        // would land on one file.
        for (index, variant) in declared.variants.iter().enumerate() {
            let lowered = variant.name.name.to_lowercase();
            if let Some(other) = declared.variants[..index]
                .iter()
                .find(|earlier| earlier.name.name.to_lowercase() == lowered)
            {
                let (a, b) = (&other.name.name, &variant.name.name);
                errors.push(SyntaxError::new(
                    variant.name.span,
                    format!(
                        "'{a}' and '{b}' differ only in case, and the functions their \
                         match arms generate would collide"
                    ),
                ));
            }
        }
        types.enums.push(EnumDef {
            id: EnumId(types.enums.len() as u32),
            name: declared.name.name.clone(),
            variants,
            span: declared.name.span,
        });
    }

    let composites: Vec<Type> = types
        .structs
        .iter()
        .map(|def| Type::Struct(def.id))
        .chain(types.enums.iter().map(|def| Type::Enum(def.id)))
        .collect();
    for ty in composites {
        if !contains_itself(&types, ty) {
            continue;
        }
        let name = types.name_of(ty);
        let span = match ty {
            Type::Struct(id) => types.struct_def(id).span,
            Type::Enum(id) => types.enum_def(id).span,
            _ => unreachable!("only composites are checked"),
        };
        errors.push(SyntaxError::new(
            span,
            format!("'{name}' contains itself, so it has no value a compound could hold"),
        ));
    }
    types
}

fn reject_item_attrs(item: &ast::Item, what: &str, errors: &mut Vec<SyntaxError>) {
    for attr in &item.attrs {
        errors.push(SyntaxError::new(
            attr.span,
            format!("attributes on {what} are not implemented yet"),
        ));
    }
}

/// The fields of a struct, or of one variant, with their NBT keys and tags settled.
fn collect_fields(
    declared: &[ast::FieldDef],
    types: &Types,
    errors: &mut Vec<SyntaxError>,
) -> Vec<Field> {
    let mut fields: Vec<Field> = Vec::new();
    for field in declared {
        let Some(ty) = resolve_type(&field.ty, types, errors) else {
            continue;
        };
        if fields.iter().any(|f| f.name == field.name.name) {
            let name = &field.name.name;
            errors.push(SyntaxError::new(
                field.name.span,
                format!("the field '{name}' is declared twice"),
            ));
            continue;
        }
        let (tag, rename) = nbt_attrs(field, ty, errors);
        let nbt = rename.unwrap_or_else(|| field.name.name.clone());
        // Two fields writing one key is one field, silently: the second write would
        // overwrite the first and nothing would say so.
        if let Some(other) = fields.iter().find(|f| f.nbt == nbt) {
            let other = other.name.clone();
            errors.push(SyntaxError::new(
                field.name.span,
                format!("this field and '{other}' would both be stored as '{nbt}'"),
            ));
            continue;
        }
        fields.push(Field {
            name: field.name.name.clone(),
            nbt,
            ty,
            tag,
        });
    }
    fields
}

/// The `#[nbt(..)]` options on a field: which tag, and which key.
fn nbt_attrs(
    field: &ast::FieldDef,
    ty: Type,
    errors: &mut Vec<SyntaxError>,
) -> (Option<NbtTag>, Option<String>) {
    let mut tag = NbtTag::default_for(ty);
    let mut rename = None;
    for attr in &field.attrs {
        let Some(TokenKind::Ident(name)) = attr.tokens.first().map(|t| &t.kind) else {
            errors.push(SyntaxError::new(attr.span, "expected an attribute name"));
            continue;
        };
        if name != "nbt" {
            let name = name.clone();
            errors.push(SyntaxError::new(
                attr.span,
                format!("'{name}' is not an attribute a field can carry"),
            ));
            continue;
        }
        let mut tokens = attr.tokens.iter().skip(1).peekable();
        while let Some(token) = tokens.next() {
            let TokenKind::Ident(option) = &token.kind else {
                continue;
            };
            if option == "rename" {
                // `rename = "Health"`.
                let text = tokens.nth(1).map(|t| t.kind.clone());
                match text {
                    Some(TokenKind::Str(text)) => rename = Some(text),
                    _ => errors.push(SyntaxError::new(
                        token.span,
                        "rename takes a string: #[nbt(rename = \"Health\")]",
                    )),
                }
                continue;
            }
            if option == "optional" {
                errors.push(SyntaxError::new(
                    token.span,
                    "#[nbt(optional)] is not implemented yet: a missing field is read \
                     as Option<T>, which arrives with enums",
                ));
                continue;
            }
            match NbtTag::parse(option) {
                Some(_) if ty == Type::Bool => errors.push(SyntaxError::new(
                    token.span,
                    "a bool is stored as a byte; vanilla has no other boolean tag",
                )),
                Some(_) if ty.is_storage() => errors.push(SyntaxError::new(
                    token.span,
                    "a struct field is a compound, so it has no scalar tag",
                )),
                Some(chosen) => tag = Some(chosen),
                None => {
                    let option = option.clone();
                    errors.push(SyntaxError::new(
                        token.span,
                        format!(
                            "unknown nbt option '{option}'; expected byte, short, int, \
                             long or rename"
                        ),
                    ));
                }
            }
        }
    }
    (tag, rename)
}

/// Whether a composite type can reach itself through its fields.
fn contains_itself(types: &Types, start: Type) -> bool {
    let mut stack = vec![start];
    let mut seen: Vec<Type> = Vec::new();
    while let Some(ty) = stack.pop() {
        for field in types.fields(ty) {
            if !field.ty.is_storage() {
                continue;
            }
            if field.ty == start {
                return true;
            }
            if !seen.contains(&field.ty) {
                seen.push(field.ty);
                stack.push(field.ty);
            }
        }
    }
    false
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

fn resolve_type(
    written: &ast::TypeName,
    types: &Types,
    errors: &mut Vec<SyntaxError>,
) -> Option<Type> {
    if written.name == "Vec" {
        let [elem] = written.args.as_slice() else {
            errors.push(SyntaxError::new(
                written.span,
                "Vec takes one type argument: write 'Vec<i32>'",
            ));
            return None;
        };
        let elem = resolve_type(elem, types, errors)?;
        if elem.is_compile_time() {
            let name = elem.name();
            errors.push(SyntaxError::new(
                written.span,
                format!("a {name} exists only while compiling, so a list cannot hold one"),
            ));
            return None;
        }
        return Some(types.vec_of(elem));
    }
    if !written.args.is_empty() {
        let name = &written.name;
        errors.push(SyntaxError::new(
            written.span,
            format!("'{name}' does not take type arguments"),
        ));
        return None;
    }
    if let Some(ty) = Type::parse(&written.name) {
        return Some(ty);
    }
    if let Some(ty) = types.get(&written.name) {
        return Some(ty);
    }
    let name = &written.name;
    errors.push(SyntaxError::new(
        written.span,
        format!("unknown type '{name}'"),
    ));
    None
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
        // Exhaustive by construction, so if every arm returns, so does the match.
        Stmt::Match { arms, .. } => arms.iter().all(|arm| always_returns(&arm.body)),
        _ => false,
    })
}

struct FnLowering<'a> {
    locals: Vec<Local>,
    types: &'a Types,
    ret: Option<Type>,
    signatures: &'a HashMap<String, Signature>,
    /// The command surface of the configured Minecraft version, if there is one.
    toolchain: Option<&'a Schema>,
    references: &'a mut Vec<Reference>,
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
            ast::Stmt::Expr(AstExpr::Method(call)) => self.method(call, true),
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
            ast::Stmt::Match(match_stmt) => self.match_stmt(match_stmt),
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
                        format!("expected {}, found {}", self.ty(want), self.ty(expr.ty)),
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
                let want = self.ty(want);
                self.error(span, format!("expected a {want} to return"));
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

    /// `match`, checked for exhaustiveness (spec section 4.10).
    ///
    /// A missing variant is the silent failure this language exists to remove: the
    /// generated guards would simply all fail and the block would do nothing.
    fn match_stmt(&mut self, stmt: &ast::MatchStmt) -> Option<Stmt> {
        let scrutinee = self.place(&stmt.scrutinee)?;
        let Type::Enum(id) = scrutinee.ty else {
            let found = self.ty(scrutinee.ty);
            self.error(
                stmt.scrutinee.span(),
                format!("only an enum can be matched on, found {found}"),
            );
            return None;
        };
        let def = self.types.enum_def(id).clone();
        let mut arms: Vec<Arm> = Vec::new();
        let mut wildcard = false;
        for arm in &stmt.arms {
            if wildcard {
                self.error(
                    arm.span,
                    "'_' has to be the last arm; nothing after it can run",
                );
                return None;
            }
            let arm = match &arm.pattern {
                ast::Pattern::Wildcard(_) => {
                    wildcard = true;
                    self.arm(None, Vec::new(), "other".to_owned(), &arm.body)
                }
                ast::Pattern::Variant {
                    ty,
                    variant,
                    binds,
                    span,
                } => {
                    if ty.name != def.name {
                        let (want, found) = (&def.name, &ty.name);
                        self.error(
                            ty.span,
                            format!("expected a variant of '{want}', found '{found}'"),
                        );
                        return None;
                    }
                    let Some((index, declared)) = def.variant(&variant.name) else {
                        let (name, wanted) = (&def.name, &variant.name);
                        self.error(
                            variant.span,
                            format!("'{name}' has no variant named '{wanted}'"),
                        );
                        return None;
                    };
                    if arms.iter().any(|a| a.variant == Some(index)) {
                        let name = &variant.name;
                        self.error(
                            *span,
                            format!("'{name}' is already covered by an earlier arm"),
                        );
                        return None;
                    }
                    let declared = declared.clone();
                    let mut bindings = Vec::new();
                    for bind in binds {
                        let Some(field) = declared.fields.iter().find(|f| f.name == bind.name)
                        else {
                            let (name, wanted) = (&variant.name, &bind.name);
                            self.error(
                                bind.span,
                                format!("'{name}' has no field named '{wanted}'"),
                            );
                            return None;
                        };
                        bindings.push((bind.name.clone(), field.nbt.clone(), field.ty));
                    }
                    self.arm(
                        Some(index),
                        bindings,
                        declared.name.to_lowercase(),
                        &arm.body,
                    )
                }
            };
            arms.push(arm);
        }
        if !wildcard {
            let missing: Vec<&str> = def
                .variants
                .iter()
                .enumerate()
                .filter(|(index, _)| !arms.iter().any(|a| a.variant == Some(*index as u32)))
                .map(|(_, variant)| variant.name.as_str())
                .collect();
            if !missing.is_empty() {
                let (name, list) = (&def.name, missing.join(", "));
                self.error(
                    stmt.span,
                    format!("this match does not cover every variant of '{name}': {list}"),
                );
                return None;
            }
        } else if arms.len() > def.variants.len() {
            // Every variant was listed before the wildcard, so it can never run.
            self.error(
                stmt.arms.last().expect("a wildcard arm").span,
                "this arm cannot be reached: every variant is already covered",
            );
            return None;
        }
        Some(Stmt::Match {
            scrutinee,
            arms,
            span: stmt.span,
        })
    }

    /// Lowers one arm's body, with its payload bindings in scope.
    fn arm(
        &mut self,
        variant: Option<u32>,
        bindings: Vec<(String, String, Type)>,
        path: String,
        body: &ast::Block,
    ) -> Arm {
        self.scopes.push(HashMap::new());
        let bindings = bindings
            .into_iter()
            .map(|(name, nbt, ty)| Binding {
                local: self.declare(&name, ty, false),
                nbt,
                ty,
            })
            .collect();
        let body = body
            .stmts
            .iter()
            .filter_map(|stmt| self.stmt(stmt))
            .collect();
        self.scopes.pop();
        Arm {
            variant,
            path,
            bindings,
            body,
        }
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
                format!("a condition must be bool, found {}", self.ty(cond.ty)),
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
        // The annotation is read first only for a list, which is the one expression
        // that cannot say what it is on its own.
        let value = match (&stmt.value, &stmt.ty) {
            (AstExpr::List(lit), Some(written)) => {
                let want = self.resolve(written)?;
                self.list_lit(lit, Some(want))?
            }
            _ => self.expr(&stmt.value)?,
        };
        let ty = match &stmt.ty {
            None => value.ty,
            Some(written) => {
                let ty = self.resolve(written)?;
                if ty != value.ty {
                    let (want, found) = (self.ty(ty), self.ty(value.ty));
                    self.error(stmt.value.span(), format!("expected {want}, found {found}"));
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
        let place = self.place(&assign.target)?;
        // Mutability is a property of the binding: writing a field writes the binding.
        let binding = self.locals[place.local.0 as usize].clone();
        if !binding.mutable {
            let name = &binding.name;
            self.error(
                assign.span,
                format!("'{name}' is not mutable; declare it with 'let mut'"),
            );
            return None;
        }
        // A compound assignment is the arithmetic, so it inherits arithmetic's rules.
        if assign.op.is_some() && place.ty != Type::I32 {
            self.error(
                assign.span,
                format!("compound assignment needs i32, found {}", self.ty(place.ty)),
            );
            return None;
        }
        if place.ty != value.ty {
            let (want, found) = (self.ty(place.ty), self.ty(value.ty));
            self.error(
                assign.value.span(),
                format!("expected {want}, found {found}"),
            );
            return None;
        }
        Some(Stmt::Assign {
            place,
            op: assign.op,
            value,
            span: assign.span,
        })
    }

    /// Resolves `p`, `p.x` or `o.inner.a` to the value it addresses.
    ///
    /// Only a binding and its fields can be addressed. A field of something else —
    /// a call's result, a literal — would have no name to write through, and there is
    /// no temporary in storage to give it one.
    fn place(&mut self, expr: &AstExpr) -> Option<Place> {
        match expr {
            AstExpr::Path(name) => {
                let local = self.lookup(name)?;
                let ty = self.locals[local.0 as usize].ty;
                Some(Place {
                    local,
                    steps: Vec::new(),
                    ty,
                    tag: NbtTag::default_for(ty),
                })
            }
            AstExpr::Field(access) => {
                let base = self.place(&access.base)?;
                // A macro builds a path that ends at the index it splices in
                // (spec section 6.21), so nothing can follow one.
                if !base.is_static() {
                    self.error(
                        access.span,
                        "an index that is only known at runtime has to be the last step; \
                         read the element into a binding first",
                    );
                    return None;
                }
                if let Type::Enum(_) = base.ty {
                    let found = self.ty(base.ty);
                    self.error(
                        access.base.span(),
                        format!(
                            "'{found}' is an enum, so which fields it has depends on its \
                             variant; read it with 'match'"
                        ),
                    );
                    return None;
                }
                let Type::Struct(id) = base.ty else {
                    let found = self.ty(base.ty);
                    self.error(
                        access.base.span(),
                        format!("{found} has no fields; only a struct does"),
                    );
                    return None;
                };
                let def = self.types.struct_def(id);
                let Some(field) = def.field(&access.name.name) else {
                    let (ty, name) = (def.name.clone(), &access.name.name);
                    self.error(
                        access.name.span,
                        format!("'{ty}' has no field named '{name}'"),
                    );
                    return None;
                };
                let (ty, tag, key) = (field.ty, field.tag, field.nbt.clone());
                let mut steps = base.steps;
                steps.push(Step::Field(key));
                Some(Place {
                    local: base.local,
                    steps,
                    ty,
                    tag,
                })
            }
            AstExpr::Index(access) => {
                let base = self.place(&access.base)?;
                let Type::Vec(id) = base.ty else {
                    let found = self.ty(base.ty);
                    self.error(
                        access.base.span(),
                        format!("{found} cannot be indexed; only a Vec can"),
                    );
                    return None;
                };
                // A macro can splice one index into a path, and the path it builds ends
                // there (spec section 6.21).
                if !base.is_static() {
                    self.error(
                        access.span,
                        "an index that is only known at runtime has to be the last step; \
                         read the element into a binding first",
                    );
                    return None;
                }
                let index = self.expr(&access.index)?;
                if index.ty != Type::I32 {
                    let found = self.ty(index.ty);
                    self.error(index.span, format!("an index has to be i32, found {found}"));
                    return None;
                }
                let ty = self.types.element(id);
                let mut steps = base.steps;
                steps.push(match index.kind {
                    ExprKind::Int(n) => Step::Index(n),
                    _ => Step::At(Box::new(index)),
                });
                Some(Place {
                    local: base.local,
                    steps,
                    ty,
                    tag: NbtTag::default_for(ty),
                })
            }
            other => {
                self.error(other.span(), "expected a binding or one of its fields");
                None
            }
        }
    }

    /// `v.len()` and `v.push(x)`. Methods on user types arrive with `impl`.
    fn method(&mut self, call: &ast::MethodCall, as_statement: bool) -> Option<Stmt> {
        let place = self.place(&call.receiver)?;
        let Type::Vec(id) = place.ty else {
            let found = self.ty(place.ty);
            self.error(
                call.span,
                format!("{found} has no methods; user-defined methods are not implemented yet"),
            );
            return None;
        };
        let elem = self.types.element(id);
        match call.name.name.as_str() {
            "push" => {
                if !as_statement {
                    self.error(call.span, "'push' does not return a value");
                    return None;
                }
                let binding = self.locals[place.local.0 as usize].clone();
                if !binding.mutable {
                    let name = &binding.name;
                    self.error(
                        call.span,
                        format!("'{name}' is not mutable; declare it with 'let mut'"),
                    );
                    return None;
                }
                let [value] = call.args.as_slice() else {
                    self.error(call.span, "'push' takes one value");
                    return None;
                };
                let value = self.expr(value)?;
                if value.ty != elem {
                    let (want, found) = (self.ty(elem), self.ty(value.ty));
                    self.error(value.span, format!("expected {want}, found {found}"));
                    return None;
                }
                Some(Stmt::Push {
                    place,
                    value,
                    span: call.span,
                })
            }
            "len" => {
                self.error(call.span, "'len' produces a value; it is not a statement");
                None
            }
            other => {
                let list = self.ty(place.ty);
                self.error(
                    call.name.span,
                    format!("'{list}' has no method named '{other}'"),
                );
                None
            }
        }
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
                        format!("expected {}, found {}", want.name(), self.ty(operand.ty)),
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
            AstExpr::Struct(lit) => self.composite_lit(lit),
            AstExpr::Field(_) | AstExpr::Index(_) => {
                let place = self.place(expr)?;
                Some(Expr {
                    ty: place.ty,
                    kind: ExprKind::Field(place),
                    span,
                })
            }
            AstExpr::List(lit) => self.list_lit(lit, None),
            AstExpr::Method(call) => {
                let place = self.place(&call.receiver)?;
                if !matches!(place.ty, Type::Vec(_)) || call.name.name != "len" {
                    // Everything that is not `len` is either a statement or not there.
                    self.method(call, false);
                    return None;
                }
                if !call.args.is_empty() {
                    self.error(call.span, "'len' takes no arguments");
                    return None;
                }
                Some(Expr {
                    kind: ExprKind::Len(place),
                    ty: Type::I32,
                    span,
                })
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
            // Two compounds cannot be compared or combined: `execute if data` matches
            // against a literal, never against another path (spec section 4.8).
            if side.ty.is_storage() {
                let name = self.ty(side.ty);
                self.error(
                    span,
                    format!("'{name}' is a struct, and the game cannot compare two compounds"),
                );
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
                format!(
                    "cannot compare {} with {}",
                    self.ty(lhs.ty),
                    self.ty(rhs.ty)
                ),
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
            let rendered = self.command_arg(arg, param.ty)?;
            // A command naming a function that does not exist is the archetypal silent
            // failure: vanilla runs it and nothing happens.
            if param.parser == "minecraft:function" {
                self.references.push(Reference {
                    id: rendered.clone(),
                    kind: RefKind::Function,
                    span: arg.span(),
                });
            }
            parts.push(rendered);
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
                    _ => &self.ty(value.ty),
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
                    format!("expected {}, found {}", self.ty(want), self.ty(arg.ty)),
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

    /// A type as a diagnostic should spell it.
    fn ty(&self, ty: Type) -> String {
        self.types.name_of(ty)
    }

    fn resolve(&mut self, written: &ast::TypeName) -> Option<Type> {
        resolve_type(written, self.types, self.errors)
    }

    /// `Point { x: 1, y: 2 }` and `State::Chasing { target: 3 }`.
    fn composite_lit(&mut self, lit: &ast::StructLit) -> Option<Expr> {
        let Some(ty) = self.types.get(&lit.name.name) else {
            let name = &lit.name.name;
            self.error(lit.name.span, format!("unknown type '{name}'"));
            return None;
        };
        match (ty, &lit.variant) {
            (Type::Struct(id), None) => {
                let def = self.types.struct_def(id).clone();
                let fields = self.init_fields(&def.fields, lit, &def.name)?;
                Some(Expr {
                    kind: ExprKind::Struct { id, fields },
                    ty,
                    span: lit.span,
                })
            }
            (Type::Struct(id), Some(variant)) => {
                let (name, wanted) = (self.types.struct_def(id).name.clone(), &variant.name);
                self.error(
                    variant.span,
                    format!("'{name}' is a struct, so it has no variant '{wanted}'"),
                );
                None
            }
            (Type::Enum(id), Some(written)) => {
                let def = self.types.enum_def(id).clone();
                let Some((variant, declared)) = def.variant(&written.name) else {
                    let (name, wanted) = (&def.name, &written.name);
                    self.error(
                        written.span,
                        format!("'{name}' has no variant named '{wanted}'"),
                    );
                    return None;
                };
                let what = format!("{}::{}", def.name, declared.name);
                let fields = self.init_fields(&declared.fields.clone(), lit, &what)?;
                Some(Expr {
                    kind: ExprKind::Enum {
                        id,
                        variant,
                        fields,
                    },
                    ty,
                    span: lit.span,
                })
            }
            (Type::Enum(id), None) => {
                let name = self.types.enum_def(id).name.clone();
                self.error(
                    lit.span,
                    format!("'{name}' is an enum; name the variant, as in '{name}::Idle'"),
                );
                None
            }
            (other, _) => {
                let name = self.ty(other);
                self.error(lit.span, format!("'{name}' is not a composite type"));
                None
            }
        }
    }

    /// `[1, 2, 3]`. Every element has the same type, which is the list's.
    ///
    /// `[]` alone says nothing about what it holds, so it only type-checks where an
    /// annotation says (spec section 3.13); nothing here infers backwards.
    fn list_lit(&mut self, lit: &ast::ListLit, want: Option<Type>) -> Option<Expr> {
        let want_elem = match want {
            Some(Type::Vec(id)) => Some(self.types.element(id)),
            _ => None,
        };
        let mut values = Vec::new();
        let mut elem = want_elem;
        for value in &lit.values {
            let value = self.expr(value)?;
            match elem {
                None => elem = Some(value.ty),
                Some(elem) if elem != value.ty => {
                    let (want, found) = (self.ty(elem), self.ty(value.ty));
                    self.error(
                        value.span,
                        format!("every element of a list has the same type: expected {want}, found {found}"),
                    );
                    return None;
                }
                Some(_) => {}
            }
            values.push(value);
        }
        let Some(elem) = elem else {
            self.error(
                lit.span,
                "an empty list needs a type: write 'let v: Vec<i32> = [];'",
            );
            return None;
        };
        if elem.is_compile_time() {
            let name = elem.name();
            self.error(
                lit.span,
                format!("a {name} exists only while compiling, so a list cannot hold one"),
            );
            return None;
        }
        Some(Expr {
            ty: self.types.vec_of(elem),
            kind: ExprKind::List { elem, values },
            span: lit.span,
        })
    }

    /// Checks an initialiser list against a declaration: every field, exactly once, at
    /// its declared type.
    ///
    /// Omission is not allowed. A compound missing a key is not an error in NBT —
    /// vanilla reads it as absent and carries on — so a partial construction would
    /// only show up as a value that is quietly never there.
    fn init_fields(
        &mut self,
        declared: &[Field],
        lit: &ast::StructLit,
        what: &str,
    ) -> Option<Vec<Expr>> {
        let mut values: Vec<Option<Expr>> = vec![None; declared.len()];
        for init in &lit.fields {
            let Some(index) = declared.iter().position(|f| f.name == init.name.name) else {
                let name = &init.name.name;
                self.error(
                    init.name.span,
                    format!("'{what}' has no field named '{name}'"),
                );
                return None;
            };
            let value = self.expr(&init.value)?;
            if value.ty != declared[index].ty {
                let (want, found) = (self.ty(declared[index].ty), self.ty(value.ty));
                self.error(value.span, format!("expected {want}, found {found}"));
                return None;
            }
            if values[index].is_some() {
                let name = &init.name.name;
                self.error(init.name.span, format!("the field '{name}' is set twice"));
                return None;
            }
            values[index] = Some(value);
        }
        let missing: Vec<String> = declared
            .iter()
            .zip(&values)
            .filter(|(_, value)| value.is_none())
            .map(|(field, _)| format!("'{}'", field.name))
            .collect();
        if !missing.is_empty() {
            let list = missing.join(", ");
            self.error(lit.span, format!("'{what}' is missing a value for {list}"));
            return None;
        }
        Some(values.into_iter().flatten().collect())
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
    /// `struct`, spec sections 3.10 and 4.8. Composite values live in storage, so most
    /// of what this stage does for them is refuse the shapes vanilla cannot hold.
    mod structs {
        use super::*;

        #[test]
        fn a_construction_missing_a_field_is_reported() {
            let errors =
                lower_err("struct Point { x: i32, y: i32 } fn main() { let p = Point { x: 1 }; }");
            assert!(errors[0].message.contains('y'), "{errors:?}");
        }

        #[test]
        fn a_field_that_is_not_declared_is_reported() {
            let errors = lower_err("struct Point { x: i32 } fn main() { let p = Point { z: 1 }; }");
            assert!(
                errors[0].message.contains("no field named 'z'"),
                "{errors:?}"
            );
        }

        #[test]
        fn a_field_set_twice_is_reported() {
            let errors =
                lower_err("struct Point { x: i32 } fn main() { let p = Point { x: 1, x: 2 }; }");
            assert!(errors[0].message.contains("twice"), "{errors:?}");
        }

        #[test]
        fn a_field_of_the_wrong_type_is_reported() {
            let errors =
                lower_err("struct Point { x: i32 } fn main() { let p = Point { x: true }; }");
            assert!(errors[0].message.contains("expected i32"), "{errors:?}");
        }

        #[test]
        fn a_struct_names_itself_in_a_diagnostic() {
            let errors =
                lower_err("struct Point { x: i32 } fn main() { let p: i32 = Point { x: 1 }; }");
            assert!(errors[0].message.contains("found Point"), "{errors:?}");
        }

        #[test]
        fn two_structs_cannot_be_compared() {
            let errors = lower_err(
                "struct Point { x: i32 } \
                 fn main() { let p = Point { x: 1 }; let q = Point { x: 1 }; let b = p == q; }",
            );
            assert!(errors[0].message.contains("compare"), "{errors:?}");
        }

        #[test]
        fn a_struct_cannot_be_returned() {
            let errors =
                lower_err("struct Point { x: i32 } fn make() -> Point { return Point { x: 1 }; }");
            assert!(errors[0].message.contains("not implemented"), "{errors:?}");
        }

        #[test]
        fn a_struct_containing_itself_is_reported() {
            let errors = lower_err("struct Node { next: Node }");
            assert!(errors[0].message.contains("contains itself"), "{errors:?}");
            // Through another struct, too: the cycle is what matters, not its length.
            let errors = lower_err("struct A { b: B } struct B { a: A }");
            assert!(!errors.is_empty(), "a two-step cycle is still a cycle");
        }

        #[test]
        fn a_compile_time_type_cannot_be_a_field() {
            // `selector` has no spellable name (spec section 4.2), so a field asking
            // for one never gets as far as the storage question.
            let errors = lower_err("struct Held { who: selector }");
            assert!(errors[0].message.contains("unknown type"), "{errors:?}");
        }

        #[test]
        fn an_nbt_tag_a_bool_cannot_have_is_reported() {
            let errors = lower_err("struct Mob { #[nbt(int)] alive: bool }");
            assert!(errors[0].message.contains("byte"), "{errors:?}");
        }

        #[test]
        fn an_unknown_nbt_option_is_reported() {
            let errors = lower_err("struct Mob { #[nbt(gigantic)] hp: i32 }");
            assert!(errors[0].message.contains("gigantic"), "{errors:?}");
        }

        #[test]
        fn nbt_optional_says_what_it_is_waiting_for() {
            let errors = lower_err("struct Mob { #[nbt(optional)] hp: i32 }");
            assert!(errors[0].message.contains("not implemented"), "{errors:?}");
        }

        #[test]
        fn a_tag_on_a_composite_field_is_reported() {
            let errors =
                lower_err("struct Inner { a: i32 } struct Outer { #[nbt(byte)] i: Inner }");
            assert!(errors[0].message.contains("compound"), "{errors:?}");
        }

        #[test]
        fn two_fields_cannot_share_one_nbt_key() {
            let errors = lower_err("struct Mob { hp: i32, #[nbt(rename = \"hp\")] health: i32 }");
            assert!(errors[0].message.contains("both be stored"), "{errors:?}");
        }

        #[test]
        fn an_unknown_field_attribute_is_reported() {
            let errors = lower_err("struct Mob { #[score] hp: i32 }");
            assert!(errors[0].message.contains("score"), "{errors:?}");
        }

        #[test]
        fn a_struct_declared_twice_is_reported() {
            let errors = lower_err("struct A { x: i32 } struct A { y: i32 }");
            assert!(errors[0].message.contains("already defined"), "{errors:?}");
        }

        #[test]
        fn a_field_of_something_that_is_not_a_struct_is_reported() {
            let errors = lower_err("fn main() { let n = 1; let x = n.field; }");
            assert!(errors[0].message.contains("has no fields"), "{errors:?}");
        }

        #[test]
        fn reading_a_field_that_does_not_exist_is_reported() {
            let errors = lower_err(
                "struct Point { x: i32 } fn main() { let p = Point { x: 1 }; let y = p.y; }",
            );
            assert!(
                errors[0].message.contains("no field named 'y'"),
                "{errors:?}"
            );
        }

        #[test]
        fn writing_a_field_needs_a_mutable_binding() {
            let errors =
                lower_err("struct Point { x: i32 } fn main() { let p = Point { x: 1 }; p.x = 2; }");
            assert!(errors[0].message.contains("not mutable"), "{errors:?}");
        }

        #[test]
        fn a_field_of_the_wrong_type_cannot_be_assigned() {
            let errors = lower_err(
                "struct Point { x: i32 } \
                 fn main() { let mut p = Point { x: 1 }; p.x = true; }",
            );
            assert!(errors[0].message.contains("expected i32"), "{errors:?}");
        }

        #[test]
        fn a_composite_field_cannot_take_arithmetic() {
            let errors = lower_err(
                "struct Inner { a: i32 } struct Outer { i: Inner } \
                 fn main() { let mut o = Outer { i: Inner { a: 1 } }; o.i += 1; }",
            );
            assert!(errors[0].message.contains("needs i32"), "{errors:?}");
        }

        #[test]
        fn a_variant_that_does_not_exist_is_reported() {
            let errors = lower_err("enum State { Idle } fn main() { let s = State::Running; }");
            assert!(errors[0].message.contains("no variant"), "{errors:?}");
        }

        #[test]
        fn an_enum_needs_a_variant() {
            let errors = lower_err("enum State { Idle } fn main() { let s = State { }; }");
            assert!(errors[0].message.contains("State::Idle"), "{errors:?}");
        }

        #[test]
        fn a_struct_has_no_variants() {
            let errors = lower_err("struct Point { x: i32 } fn main() { let p = Point::Origin; }");
            assert!(errors[0].message.contains("no variant"), "{errors:?}");
        }

        #[test]
        fn a_variant_payload_is_checked_like_a_struct() {
            let errors = lower_err(
                "enum State { Chasing { target: i32 } } \
                 fn main() { let s = State::Chasing { }; }",
            );
            assert!(errors[0].message.contains("'target'"), "{errors:?}");
        }

        #[test]
        fn an_enum_field_cannot_be_read_without_match() {
            let errors = lower_err(
                "enum State { Chasing { target: i32 } } \
                 fn main() { let s = State::Chasing { target: 1 }; let t = s.target; }",
            );
            assert!(errors[0].message.contains("match"), "{errors:?}");
        }

        #[test]
        fn a_variant_field_cannot_take_the_tag_key() {
            let errors = lower_err("enum State { Chasing { tag: i32 } }");
            assert!(errors[0].message.contains("tag"), "{errors:?}");
        }

        #[test]
        fn a_tuple_variant_says_to_name_its_fields() {
            let (_, errors) = parse("enum State { Chasing(i32) }");
            assert!(errors[0].message.contains("names its fields"), "{errors:?}");
        }

        #[test]
        fn an_enum_cannot_contain_itself_through_a_struct() {
            let errors = lower_err("enum List { Cons { rest: Wrap } } struct Wrap { l: List }");
            assert!(errors[0].message.contains("contains itself"), "{errors:?}");
        }

        #[test]
        fn a_match_that_misses_a_variant_is_reported() {
            let errors = lower_err(
                "enum State { Idle, Waking } \
                 fn main() { let s = State::Idle; match s { State::Idle => { } } }",
            );
            assert!(errors[0].message.contains("Waking"), "{errors:?}");
        }

        #[test]
        fn a_variant_covered_twice_is_reported() {
            let errors = lower_err(
                "enum State { Idle } \
                 fn main() { let s = State::Idle; \
                             match s { State::Idle => { } State::Idle => { } } }",
            );
            assert!(errors[0].message.contains("already covered"), "{errors:?}");
        }

        #[test]
        fn a_wildcard_has_to_come_last() {
            let errors = lower_err(
                "enum State { Idle, Waking } \
                 fn main() { let s = State::Idle; \
                             match s { _ => { } State::Idle => { } } }",
            );
            assert!(errors[0].message.contains("last arm"), "{errors:?}");
        }

        #[test]
        fn a_wildcard_that_cannot_run_is_reported() {
            let errors = lower_err(
                "enum State { Idle } \
                 fn main() { let s = State::Idle; \
                             match s { State::Idle => { } _ => { } } }",
            );
            assert!(
                errors[0].message.contains("cannot be reached"),
                "{errors:?}"
            );
        }

        #[test]
        fn a_pattern_from_another_enum_is_reported() {
            let errors = lower_err(
                "enum State { Idle } enum Mood { Angry } \
                 fn main() { let s = State::Idle; match s { Mood::Angry => { } } }",
            );
            assert!(
                errors[0].message.contains("variant of 'State'"),
                "{errors:?}"
            );
        }

        #[test]
        fn binding_a_field_the_variant_does_not_have_is_reported() {
            let errors = lower_err(
                "enum State { Chasing { target: i32 } } \
                 fn main() { let s = State::Chasing { target: 1 }; \
                             match s { State::Chasing { speed } => { } } }",
            );
            assert!(errors[0].message.contains("speed"), "{errors:?}");
        }

        #[test]
        fn only_an_enum_can_be_matched() {
            let errors = lower_err("fn main() { let n = 1; match n { _ => { } } }");
            assert!(errors[0].message.contains("only an enum"), "{errors:?}");
        }

        #[test]
        fn variants_that_differ_only_in_case_are_reported() {
            let errors = lower_err("enum State { Idle, IDLE }");
            assert!(
                errors[0].message.contains("differ only in case"),
                "{errors:?}"
            );
        }

        #[test]
        fn a_binding_is_only_in_scope_inside_its_arm() {
            let errors = lower_err(
                "enum State { Chasing { target: i32 } } \
                 fn main() { let s = State::Chasing { target: 1 }; \
                             match s { State::Chasing { target } => { } } \
                             let x = target; }",
            );
            assert!(errors[0].message.contains("not defined"), "{errors:?}");
        }

        #[test]
        fn an_empty_list_without_a_type_is_reported() {
            let errors = lower_err("fn main() { let v = []; }");
            assert!(errors[0].message.contains("empty list"), "{errors:?}");
        }

        #[test]
        fn a_list_of_mixed_types_is_reported() {
            let errors = lower_err("fn main() { let v = [1, true]; }");
            assert!(errors[0].message.contains("same type"), "{errors:?}");
        }

        #[test]
        fn only_a_list_can_be_indexed() {
            let errors = lower_err("fn main() { let n = 1; let x = n[0]; }");
            assert!(errors[0].message.contains("indexed"), "{errors:?}");
        }

        #[test]
        fn an_index_has_to_be_a_number() {
            let errors = lower_err("fn main() { let v = [1]; let x = v[true]; }");
            assert!(errors[0].message.contains("index"), "{errors:?}");
        }

        #[test]
        fn pushing_needs_a_mutable_binding() {
            let errors = lower_err("fn main() { let v = [1]; v.push(2); }");
            assert!(errors[0].message.contains("not mutable"), "{errors:?}");
        }

        #[test]
        fn pushing_the_wrong_type_is_reported() {
            let errors = lower_err("fn main() { let mut v = [1]; v.push(true); }");
            assert!(errors[0].message.contains("expected i32"), "{errors:?}");
        }

        #[test]
        fn an_unknown_method_is_reported() {
            let errors = lower_err("fn main() { let mut v = [1]; v.pop(); }");
            assert!(
                errors[0].message.contains("no method named 'pop'"),
                "{errors:?}"
            );
        }

        #[test]
        fn a_runtime_index_cannot_be_followed_by_a_field() {
            let errors = lower_err(
                "struct Point { x: i32 } \
                 fn main() { let v = [Point { x: 1 }]; let i = 0; let n = v[i].x; }",
            );
            assert!(errors[0].message.contains("last step"), "{errors:?}");
        }

        #[test]
        fn a_list_cannot_hold_a_compile_time_type() {
            let errors = lower_err("fn f(v: Vec<selector>) {}");
            assert!(!errors.is_empty(), "{errors:?}");
        }

        #[test]
        fn vec_needs_exactly_one_type_argument() {
            let errors = lower_err("fn f(v: Vec) {}");
            assert!(
                errors[0].message.contains("one type argument"),
                "{errors:?}"
            );
            let errors = lower_err("fn f(v: i32<bool>) {}");
            assert!(errors[0].message.contains("type arguments"), "{errors:?}");
        }

        #[test]
        fn a_struct_can_be_annotated_and_passed() {
            let hir = lower_ok(
                "struct Point { x: i32 } \
                 fn take(p: Point) {} \
                 fn main() { let p: Point = Point { x: 1 }; take(p); }",
            );
            assert_eq!(hir.types.structs.len(), 1);
            assert_eq!(hir.types.structs[0].fields[0].name, "x");
        }
    }
}
