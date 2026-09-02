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
use std::collections::{BTreeMap, HashMap};

use crate::schema::{ArgType, Part, Schema};
use crate::syntax::SyntaxError;
use crate::syntax::ast::{self, BinaryOp, Expr as AstExpr, ItemKind, SourceFile, UnaryOp};
use crate::syntax::lexer::{Keyword, Punct, Span, Token, TokenKind};

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

/// Identifies one `Option<T>`, interned by inner type the same way as a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OptionId(pub u32);

/// The scale of a `fix<S>`: a number, or the const parameter that will become one
/// (spec section 3.16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scale {
    Const(u32),
    Param(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    I32,
    Bool,
    /// Fixed point: an integer on the scoreboard, holding `S`ths of a unit.
    Fix(Scale),
    /// The NBT interop scalars. They live in storage, hold what vanilla wrote, and
    /// have no arithmetic: `Byte(1)` and `Int(1)` are different values to the game,
    /// and only a type can keep them apart (requirements section 4.1).
    I8,
    I16,
    I64,
    F32,
    F64,
    /// A compile-time selector. It has no runtime representation: the only thing that
    /// can be done with one is hand it to `as`, `at` or `for`.
    Selector,
    /// `minecraft:stone`. Compile-time only.
    Resource,
    /// `pos!(~ ~1 ~)`. Compile-time only.
    Pos,
    /// `text!(..)`: chat JSON put together while compiling (spec section 3.22).
    /// Compile-time only, like a selector.
    Component,
    /// An immutable string in storage (spec section 4.17).
    Str,
    /// A composite value. It lives in storage rather than in a register, which is a
    /// third category: neither a score nor compile-time only (spec section 5).
    Struct(StructId),
    /// A tagged union, also in storage (spec section 4.9).
    Enum(EnumId),
    /// An NBT list in storage (spec section 4.11).
    Vec(VecId),
    /// Maybe a `T`, and the difference is whether the path exists at all
    /// (spec section 4.19).
    Option(OptionId),
    /// A view of an entity's NBT: fields that are places rather than values
    /// (spec section 4.20). Compile-time only, like a selector.
    View(StructId),
    /// A type parameter, inside a template that has not been instantiated yet
    /// (spec section 4.12). No value ever has this type: instantiation replaces it.
    Param(u32),
}

impl Type {
    fn parse(name: &str) -> Option<Type> {
        match name {
            "i32" => Some(Type::I32),
            "i8" => Some(Type::I8),
            "i16" => Some(Type::I16),
            "i64" => Some(Type::I64),
            "f32" => Some(Type::F32),
            "f64" => Some(Type::F64),
            "String" => Some(Type::Str),
            "bool" => Some(Type::Bool),
            // Deliberately not spellable in a type annotation: a selector is inferred
            // from the literal, never declared.
            _ => None,
        }
    }

    /// Whether the type exists only while compiling, with nothing to put in a
    /// register at runtime.
    pub fn is_compile_time(&self) -> bool {
        matches!(
            self,
            Type::Selector | Type::Resource | Type::Pos | Type::Component | Type::View(_)
        )
    }

    /// Whether values of this type live in storage rather than on the scoreboard.
    pub fn is_storage(&self) -> bool {
        self.is_compound() || self.is_storage_scalar() || matches!(self, Type::Option(_))
    }

    /// Whether this is a structure in storage, rather than one value in it.
    pub fn is_compound(&self) -> bool {
        matches!(self, Type::Struct(_) | Type::Enum(_) | Type::Vec(_))
    }

    /// Whether values of this type live in storage without being a structure: one
    /// tag, one value.
    pub fn is_storage_scalar(&self) -> bool {
        self.is_nbt_scalar() || *self == Type::Str
    }

    /// Whether this is one of the types that exists to match an NBT tag exactly.
    pub fn is_nbt_scalar(&self) -> bool {
        matches!(
            self,
            Type::I8 | Type::I16 | Type::I64 | Type::F32 | Type::F64
        )
    }

    pub fn name(&self) -> &'static str {
        match self {
            Type::I32 => "i32",
            Type::Bool => "bool",
            Type::I8 => "i8",
            Type::I16 => "i16",
            Type::I64 => "i64",
            Type::F32 => "f32",
            Type::F64 => "f64",
            Type::Str => "String",
            // Only where the scale cannot be spelled; `Types::name_of` writes it out.
            Type::Fix(_) => "fix",
            Type::Selector => "selector",
            Type::Resource => "ResourceLocation",
            Type::Pos => "Pos",
            Type::Component => "TextComponent",
            // Only reachable where the type table is out of reach; every diagnostic
            // that can name the type goes through `Types::name_of` instead.
            Type::Struct(_) => "struct",
            Type::Enum(_) => "enum",
            Type::Vec(_) => "Vec",
            Type::Option(_) => "Option",
            Type::View(_) => "view",
            Type::Param(_) => "a type parameter",
        }
    }
}

/// Every type the program defines, and the names they answer to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Types {
    /// Every concrete struct, including the instances monomorphisation creates. Behind
    /// a cell because an instance is made while the table is only borrowed to read.
    structs: RefCell<Vec<StructDef>>,
    pub enums: Vec<EnumDef>,
    /// Generic structs, kept as written. A template is never a type on its own.
    templates: Vec<StructTemplate>,
    by_name: HashMap<String, Type>,
    template_by_name: HashMap<String, usize>,
    /// Element type per `VecId`, interned the same way.
    vecs: RefCell<Vec<Type>>,
    /// Inner type per `OptionId`.
    options: RefCell<Vec<Type>>,
}

/// A generic parameter as declared: its name, and whether it stands for a scale
/// rather than a type (spec section 3.16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericDef {
    pub name: String,
    pub is_const: bool,
}

impl GenericDef {
    fn collect(params: &[ast::GenericParam]) -> Vec<GenericDef> {
        params
            .iter()
            .map(|param| GenericDef {
                name: param.name.name.clone(),
                is_const: param.is_const,
            })
            .collect()
    }
}

/// A generic `struct`, whose field types may mention `Type::Param`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructTemplate {
    pub name: String,
    pub generics: Vec<GenericDef>,
    pub fields: Vec<Field>,
    pub span: Span,
}

impl Types {
    pub fn get(&self, name: &str) -> Option<Type> {
        self.by_name.get(name).copied()
    }

    /// A struct by id. Returned by value: the table can grow while one is in hand.
    pub fn struct_def(&self, id: StructId) -> StructDef {
        self.structs.borrow()[id.0 as usize].clone()
    }

    pub fn struct_count(&self) -> usize {
        self.structs.borrow().len()
    }

    pub fn template(&self, name: &str) -> Option<&StructTemplate> {
        self.template_by_name
            .get(name)
            .map(|index| &self.templates[*index])
    }

    /// The concrete struct for `name<args>`, made once and then found again.
    pub fn instantiate(&self, name: &str, args: &[Type]) -> Option<Type> {
        let index = *self.template_by_name.get(name)?;
        let key = (index, args.to_vec());
        if let Some(known) = self
            .structs
            .borrow()
            .iter()
            .find(|def| def.from.as_ref() == Some(&key))
        {
            return Some(Type::Struct(known.id));
        }
        let template = &self.templates[index];
        let fields = template
            .fields
            .iter()
            .map(|field| {
                let ty = self.substitute(field.ty, args);
                Field {
                    ty,
                    // A field whose type came from a parameter takes the tag of what
                    // the parameter turned out to be: `T = bool` is a Byte.
                    tag: match self.mentions_param(field.ty) {
                        true => NbtTag::default_for(ty),
                        false => field.tag,
                    },
                    ..field.clone()
                }
            })
            .collect();
        let mut structs = self.structs.borrow_mut();
        let id = StructId(structs.len() as u32);
        let spelled = args
            .iter()
            .map(|ty| self.name_of(*ty))
            .collect::<Vec<_>>()
            .join(", ");
        structs.push(StructDef {
            id,
            name: format!("{}<{spelled}>", template.name),
            fields,
            span: template.span,
            from: Some(key),
        });
        Some(Type::Struct(id))
    }

    /// Replaces the type parameters in `ty` with `args`.
    pub fn substitute(&self, ty: Type, args: &[Type]) -> Type {
        match ty {
            // A const argument travels as the `fix` it stands for, so the scale is
            // already in hand (spec section 6.25).
            Type::Fix(Scale::Param(index)) => args
                .get(index as usize)
                .copied()
                .unwrap_or(Type::Fix(Scale::Const(1))),
            Type::Param(index) => args
                .get(index as usize)
                .copied()
                // Out of range only when the arity was already reported.
                .unwrap_or(Type::I32),
            Type::Vec(id) => self.vec_of(self.substitute(self.element(id), args)),
            Type::Option(id) => self.option_of(self.substitute(self.inner(id), args)),
            Type::Struct(id) => {
                let def = self.struct_def(id);
                match def.from {
                    Some((_, ref targs)) if targs.iter().any(|t| self.mentions_param(*t)) => {
                        let name = self.templates[def.from.as_ref().expect("from").0]
                            .name
                            .clone();
                        let targs: Vec<Type> =
                            targs.iter().map(|t| self.substitute(*t, args)).collect();
                        self.instantiate(&name, &targs).unwrap_or(ty)
                    }
                    _ => ty,
                }
            }
            other => other,
        }
    }

    fn mentions_param(&self, ty: Type) -> bool {
        match ty {
            Type::Param(_) | Type::Fix(Scale::Param(_)) => true,
            Type::Vec(id) => self.mentions_param(self.element(id)),
            Type::Option(id) => self.mentions_param(self.inner(id)),
            Type::Struct(id) => match self.struct_def(id).from {
                Some((_, args)) => args.iter().any(|t| self.mentions_param(*t)),
                None => false,
            },
            _ => false,
        }
    }

    /// Matches a declared type against an actual one, binding type parameters.
    ///
    /// Structural, not a solver: every parameter is bound by appearing somewhere in an
    /// argument's type, and anything else is a mismatch.
    pub fn unify(&self, declared: Type, actual: Type, args: &mut [Option<Type>]) -> bool {
        match (declared, actual) {
            // `fix<S>` against `fix<1000>` binds the scale, which rides in the
            // argument list as the fix type itself (spec section 6.25).
            (Type::Fix(Scale::Param(index)), Type::Fix(Scale::Const(_))) => {
                match args[index as usize] {
                    Some(known) => known == actual,
                    None => {
                        args[index as usize] = Some(actual);
                        true
                    }
                }
            }
            (Type::Param(index), actual) => match args[index as usize] {
                Some(known) => known == actual,
                None => {
                    args[index as usize] = Some(actual);
                    true
                }
            },
            (Type::Vec(a), Type::Vec(b)) => self.unify(self.element(a), self.element(b), args),
            (Type::Option(a), Type::Option(b)) => self.unify(self.inner(a), self.inner(b), args),
            (Type::Struct(a), Type::Struct(b)) => {
                let (a, b) = (self.struct_def(a), self.struct_def(b));
                match (&a.from, &b.from) {
                    (Some((ta, aargs)), Some((tb, bargs))) if ta == tb => aargs
                        .iter()
                        .zip(bargs)
                        .all(|(x, y)| self.unify(*x, *y, args)),
                    _ => a.id == b.id,
                }
            }
            (declared, actual) => declared == actual,
        }
    }

    /// The type as a datapack path can spell it (spec section 6.23).
    pub fn mangle(&self, ty: Type) -> String {
        match ty {
            Type::Vec(id) => format!("vec_{}", self.mangle(self.element(id))),
            Type::Option(id) => format!("option_{}", self.mangle(self.inner(id))),
            Type::Fix(Scale::Const(scale)) => format!("fix_{scale}"),
            Type::Struct(_) | Type::Enum(_) => self
                .name_of(ty)
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect::<String>()
                .trim_matches('_')
                .to_lowercase(),
            other => other.name().to_owned(),
        }
    }

    pub fn enum_def(&self, id: EnumId) -> &EnumDef {
        &self.enums[id.0 as usize]
    }

    /// A type as a diagnostic should spell it, which for a user type is its own name.
    pub fn name_of(&self, ty: Type) -> String {
        match ty {
            Type::Fix(Scale::Const(scale)) => format!("fix<{scale}>"),
            Type::Struct(id) | Type::View(id) => self.struct_def(id).name.clone(),
            Type::Enum(id) => self.enum_def(id).name.clone(),
            Type::Vec(id) => format!("Vec<{}>", self.name_of(self.element(id))),
            Type::Option(id) => format!("Option<{}>", self.name_of(self.inner(id))),
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

    /// `Option<inner>`, interned so that the same option type is the same id.
    pub fn option_of(&self, inner: Type) -> Type {
        let mut options = self.options.borrow_mut();
        let index = match options.iter().position(|known| *known == inner) {
            Some(index) => index,
            None => {
                options.push(inner);
                options.len() - 1
            }
        };
        Type::Option(OptionId(index as u32))
    }

    /// What an option holds when it holds anything.
    pub fn inner(&self, id: OptionId) -> Type {
        self.options.borrow()[id.0 as usize]
    }

    /// The fields a composite type holds, across every variant of an `enum`.
    fn fields(&self, ty: Type) -> Vec<Field> {
        match ty {
            Type::Struct(id) => self.struct_def(id).fields,
            Type::Enum(id) => self
                .enum_def(id)
                .variants
                .iter()
                .flat_map(|variant| variant.fields.iter().cloned())
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
    pub root: Root,
    /// The steps from the root to the value, in order.
    pub steps: Vec<Step>,
    /// The type of the value addressed, which is the innermost step's.
    pub ty: Type,
    /// The tag it is stored as; `None` for a compound or a list.
    pub tag: Option<NbtTag>,
    /// Whether writing through this is allowed.
    pub mutable: bool,
    /// The name the source used, for diagnostics.
    pub via: String,
}

/// Where a place starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Root {
    /// A binding of the function being lowered.
    Local(LocalId),
    /// An entity's NBT, reached through a view (spec section 6.29). The selector is
    /// compile-time text, so the whole path is written into the command.
    Entity { selector: String },
    /// A binding of the caller, lent by reference (spec section 6.24). Borrowing is a
    /// name for someone else's place, so the name of that place is what is carried.
    Lent {
        /// The function the binding belongs to.
        owner: String,
        local: String,
        /// Whether that binding lives in storage rather than in a register.
        storage: bool,
    },
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

    /// A stable spelling of where this points, for keying instances by what they
    /// borrow. Only ever compared, never emitted.
    pub fn key(&self) -> String {
        let root = match &self.root {
            Root::Local(id) => format!("l{}", id.0),
            Root::Entity { selector } => format!("e{selector}"),
            Root::Lent {
                owner,
                local,
                storage,
            } => format!("{owner}.{local}.{storage}"),
        };
        let steps = self
            .steps
            .iter()
            .map(|step| match step {
                Step::Field(name) => format!(".{name}"),
                Step::Index(index) => format!("[{index}]"),
                Step::At(_) => "[?]".to_owned(),
            })
            .collect::<String>();
        format!("{root}{steps}")
    }
}

/// A `struct` definition: an NBT compound with a known shape (spec section 4.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDef {
    pub id: StructId,
    pub name: String,
    pub fields: Vec<Field>,
    pub span: Span,
    /// Which template and type arguments this came from, for a generic struct.
    pub from: Option<(usize, Vec<Type>)>,
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
    Float,
    Double,
}

impl NbtTag {
    fn parse(name: &str) -> Option<NbtTag> {
        Some(match name {
            "byte" => NbtTag::Byte,
            "short" => NbtTag::Short,
            "int" => NbtTag::Int,
            "long" => NbtTag::Long,
            "float" => NbtTag::Float,
            "double" => NbtTag::Double,
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
            NbtTag::Float => "float",
            NbtTag::Double => "double",
        }
    }

    /// As SNBT spells a literal of this tag.
    pub fn suffix(self) -> &'static str {
        match self {
            NbtTag::Byte => "b",
            NbtTag::Short => "s",
            NbtTag::Int => "",
            NbtTag::Long => "L",
            NbtTag::Float => "f",
            NbtTag::Double => "d",
        }
    }

    /// The tag a type is written as when nothing says otherwise.
    pub fn default_for(ty: Type) -> Option<NbtTag> {
        match ty {
            Type::I32 | Type::Fix(_) => Some(NbtTag::Int),
            Type::Bool | Type::I8 => Some(NbtTag::Byte),
            Type::I16 => Some(NbtTag::Short),
            Type::I64 => Some(NbtTag::Long),
            Type::F32 => Some(NbtTag::Float),
            Type::F64 => Some(NbtTag::Double),
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
        scrutinee: Scrutinee,
        arms: Vec<Arm>,
        span: Span,
    },
    /// `for x in vec`: destructive iteration over a copy (spec section 6.22).
    ForVec {
        source: Place,
        binding: LocalId,
        body: Vec<Stmt>,
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
    /// `debug_assert!(c, "m")`: nothing at all in a release build
    /// (spec section 6.30).
    Assert {
        cond: Expr,
        message: Option<String>,
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

/// What a method call turned out to be.
enum MethodOutcome {
    Stmt(Stmt),
    Value(Expr),
}

/// One arm of a `match`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm {
    /// What has to hold for this arm to run.
    pub test: ArmTest,
    /// The name this arm is generated under, already safe as a datapack path.
    pub path: String,
    pub bindings: Vec<Binding>,
    pub body: Vec<Stmt>,
}

/// What is being matched on.
///
/// A compound has to be somewhere to be looked at, so an `enum` is always a place. An
/// option answers in registers as well — a call reports both halves of its outcome —
/// so it can be matched straight from the call (spec section 6.28).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scrutinee {
    Place(Place),
    Option(Expr),
}

impl Scrutinee {
    /// How the matched value is stored, where that is known.
    pub fn tag(&self) -> Option<NbtTag> {
        match self {
            Scrutinee::Place(place) => place.tag,
            Scrutinee::Option(_) => None,
        }
    }
}

/// What decides whether an arm runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmTest {
    /// One variant of an `enum`, told apart by its tag.
    Variant(u32),
    /// An option that holds something, told apart by the path being there.
    Present,
    Absent,
    /// `_`: everything the arms above did not take.
    Other,
}

/// A payload field bound by a pattern, copied out of the compound on arm entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub local: LocalId,
    /// The key to read, under the scrutinee's path. Empty for an option, whose value
    /// is the scrutinee's path itself (spec section 6.28).
    pub nbt: String,
    pub ty: Type,
    /// How it is stored, which decides the scale a `fix<S>` is read with.
    pub tag: Option<NbtTag>,
}

/// A `raw!` command. Interpolation arrives in M9; today the text is literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextKind {
    As,
    At,
    For,
    /// `positioned <pos>`: the body runs once, somewhere else.
    Positioned,
}

/// A selector, resolved at compile time. `@s` is what a `for` binding means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    pub text: String,
    pub span: Span,
}

/// One piece of a `raw!` command (spec section 6.31).
///
/// A constant interpolation is already folded into the literal beside it; only a value
/// that is not known until runtime survives as a part of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawPart {
    Lit(String),
    Value(Expr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCommand {
    pub parts: Vec<RawPart>,
    pub span: Span,
}

impl RawCommand {
    pub fn literal(text: String, span: Span) -> Self {
        Self {
            parts: vec![RawPart::Lit(text)],
            span,
        }
    }

    /// The whole command, when nothing in it has to wait for runtime.
    pub fn as_text(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [RawPart::Lit(text)] => Some(text),
            _ => None,
        }
    }
}

/// A chat component while it is being put together (spec section 6.32).
///
/// The members are already rendered as JSON values, so `"text"` maps to `"\"hi\""`
/// and `"bold"` to `true`. Keeping them in a map is what lets `.red().bold()` add one
/// at a time, and what makes `.red().blue()` mean blue.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Component {
    members: BTreeMap<&'static str, String>,
    /// The components appended after this one, which inherit its style.
    extra: Vec<Component>,
}

impl Component {
    /// A literal run of text.
    fn text(value: &str) -> Self {
        let mut component = Self::default();
        component.set("text", quoted_json(value));
        component
    }

    fn set(&mut self, key: &'static str, value: String) {
        self.members.insert(key, value);
    }

    /// The JSON. `extra` sorts in among the members so that the output of a given
    /// component is always spelled the same way.
    pub fn render(&self) -> String {
        let mut members: BTreeMap<&str, String> = self
            .members
            .iter()
            .map(|(key, value)| (*key, value.clone()))
            .collect();
        if !self.extra.is_empty() {
            let parts: Vec<String> = self.extra.iter().map(Component::render).collect();
            members.insert("extra", format!("[{}]", parts.join(",")));
        }
        let body: Vec<String> = members
            .iter()
            .map(|(key, value)| format!("\"{key}\":{value}"))
            .collect();
        format!("{{{}}}", body.join(","))
    }
}

/// A string as a JSON value, quotes included.
fn quoted_json(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// What a style method sets: its JSON key, and the value it stands for. `None` means
/// the method takes the value as an argument, which only `.color("#rrggbb")` does.
fn style_member(method: &str) -> Option<(&'static str, Option<String>)> {
    const COLOURS: &[&str] = &[
        "black",
        "dark_blue",
        "dark_green",
        "dark_aqua",
        "dark_red",
        "dark_purple",
        "gold",
        "gray",
        "dark_gray",
        "blue",
        "green",
        "aqua",
        "red",
        "light_purple",
        "yellow",
        "white",
    ];
    if let Some(name) = COLOURS.iter().find(|name| **name == method) {
        return Some(("color", Some(format!("\"{name}\""))));
    }
    match method {
        "color" => Some(("color", None)),
        "bold" => Some(("bold", Some("true".to_owned()))),
        "italic" => Some(("italic", Some("true".to_owned()))),
        "underlined" => Some(("underlined", Some("true".to_owned()))),
        "strikethrough" => Some(("strikethrough", Some("true".to_owned()))),
        "obfuscated" => Some(("obfuscated", Some("true".to_owned()))),
        _ => None,
    }
}

/// What a `{name}` in a `raw!` string turned out to be.
enum Interpolated {
    /// A compile-time value: it goes into the string and costs nothing.
    Const(String),
    /// A value only the running game knows, which promotes the line to a macro.
    Value(Expr),
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
    /// `block(pos!(..), minecraft:stone)`: is that block there (spec section 6.39)?
    /// A condition in its own right, so it costs nothing to ask inside an `if`.
    Block {
        at: String,
        id: String,
    },
    /// A chat component, fully built (spec section 3.22). Compile-time only: it ends
    /// up as JSON inside a command's text and costs nothing to run.
    Component(Component),
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
    /// Reading a number out of storage into a register, `S`ths of a unit at a time
    /// (spec section 6.26). The place holds an NBT scalar; the expression's type is
    /// what it is being read into.
    ReadScaled {
        place: Place,
        scale: u32,
    },
    /// The other direction: a register value written back as an NBT scalar. The
    /// expression's type is the tag it lands as.
    AsNbt {
        value: Box<Expr>,
        scale: u32,
    },
    /// `Some(e)`: the value, written where the option lives (spec section 6.28).
    Some(Box<Expr>),
    /// `o?`: the value, having left the function already if there was none.
    Try(Box<Expr>),
    /// `o.expect("m")`: the value, and in a debug build a report when there was none.
    Expect {
        value: Box<Expr>,
        message: String,
    },
    /// `Mob::of(@s)`: a name for an entity's NBT. Never lowered — the binding it goes
    /// into is an alias, and the fields are what get read (spec section 6.29).
    View(Place),
    /// `None`: nothing at all. Writing it removes the path.
    None,
    /// `nbt!({ hp: 20 })`: a compound written out and checked against the type it is
    /// going into (spec section 4.18). Already SNBT, so it costs one `set value`.
    Nbt(String),
    /// `s.slice(1..3)`: a piece of a string, taken by `data modify ... set string`
    /// (spec section 6.27). Both bounds are known while compiling.
    Slice {
        place: Place,
        start: Option<i32>,
        end: Option<i32>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attr {
    Tick,
    Load,
    /// `#[test]`: a function `mwl test` runs (spec section 3.23). Called with no
    /// executor, like a tag, and left out of a release build.
    Test,
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
            "test" => Attr::Test,
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
    /// The id of the one function this name means, or `None` for a template: a
    /// generic function has an id per set of type arguments, not one of its own.
    id: Option<FnId>,
    /// Type and const parameters, empty for an ordinary function.
    generics: Vec<GenericDef>,
    /// Parameters, which for a template may mention `Type::Param`.
    params: Vec<ParamSig>,
    ret: Option<Type>,
    /// What the function requires of its caller, from `#[ctx(..)]`.
    ctx: Vec<Ctx>,
    /// Index into the item list, so an instance can find the body to lower.
    item: usize,
}

/// A parameter as declared: its type, and whether it is borrowed rather than passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParamSig {
    ty: Type,
    /// `None` for an ordinary parameter, which is written before the call.
    borrow: Option<ast::Borrow>,
}

/// The instances monomorphisation has been asked for (spec section 6.23).
#[derive(Debug, Default)]
struct Instances {
    /// Which id a template, its type arguments and what it borrows resolved to, so
    /// the same combination is never instantiated twice.
    by_key: HashMap<(usize, Vec<Type>, Vec<String>), FnId>,
    /// Instances still to be lowered, in the order they were asked for.
    pending: Vec<Pending>,
    next: u32,
}

#[derive(Debug, Clone)]
struct Pending {
    id: FnId,
    /// The item the template was written as.
    item: usize,
    args: Vec<Type>,
    /// What each borrowed parameter was lent, in parameter order.
    borrows: Vec<Option<Place>>,
    /// The name the instance is emitted under.
    name: String,
}

impl Instances {
    fn get_or_create(
        &mut self,
        item: usize,
        args: Vec<Type>,
        borrows: Vec<Option<Place>>,
        base: &str,
        types: &Types,
    ) -> FnId {
        let lent: Vec<String> = borrows
            .iter()
            .map(|place| place.as_ref().map(Place::key).unwrap_or_default())
            .collect();
        let key = (item, args.clone(), lent);
        if let Some(id) = self.by_key.get(&key) {
            return *id;
        }
        let id = FnId(self.next);
        self.next += 1;
        // The name says what it was instantiated with, so the output stays readable.
        let mut name = base.replace("::", "/").to_lowercase();
        for ty in &args {
            name.push('_');
            name.push_str(&types.mangle(*ty));
        }
        for place in borrows.iter().flatten() {
            if let Root::Lent { owner, local, .. } = &place.root {
                name.push('_');
                name.push_str(&owner.replace("::", "_").to_lowercase());
                name.push('_');
                name.push_str(local);
            }
            for step in &place.steps {
                match step {
                    Step::Field(field) => {
                        name.push('_');
                        name.push_str(field);
                    }
                    Step::Index(index) => name.push_str(&format!("_{index}")),
                    Step::At(_) => {}
                }
            }
        }
        self.by_key.insert(key, id);
        self.pending.push(Pending {
            id,
            item,
            args,
            borrows,
            name,
        });
        id
    }
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
    let mut items: Vec<(&ast::Item, &ast::FnItem, String)> = Vec::new();
    // How many functions have an id already; instances are numbered after them.
    let mut concrete = 0usize;

    // First pass: signatures only. Without it, calling a function defined further down
    // the file would be an error — a rule about text order rather than about programs.
    //
    // A method is a function whose name carries its type (`Counter::bump`) and whose
    // first parameter is the receiver.
    let mut written: Vec<(&ast::Item, &ast::FnItem, Option<&str>)> = Vec::new();
    for item in &file.items {
        match &item.kind {
            ItemKind::Fn(f) => written.push((item, f, None)),
            ItemKind::Impl(block) => {
                for method in &block.methods {
                    let ItemKind::Fn(f) = &method.kind else {
                        errors.push(SyntaxError::new(
                            method.span,
                            "an impl block holds functions only",
                        ));
                        continue;
                    };
                    written.push((method, f, Some(&block.ty.name)));
                }
            }
            _ => {}
        }
    }
    for (item, f, owner) in written {
        let name = match owner {
            Some(ty) => format!("{ty}::{}", f.name.name),
            None => f.name.name.clone(),
        };
        if signatures.contains_key(&name) {
            errors.push(SyntaxError::new(
                f.name.span,
                format!("a function named '{name}' is already defined"),
            ));
            continue;
        }
        let generics = GenericDef::collect(&f.generics);
        let mut params: Vec<ParamSig> = Vec::new();
        // The receiver is the first parameter, with the type the impl block names.
        if let Some(receiver) = f.receiver {
            match owner.and_then(|ty| types.get(ty)) {
                Some(ty) => params.push(ParamSig {
                    ty,
                    borrow: receiver.borrow,
                }),
                None => errors.push(SyntaxError::new(
                    receiver.span,
                    "'self' is only a parameter inside an impl block for a known type",
                )),
            }
        }
        for param in &f.params {
            if let Some(ty) = resolve_written(&param.ty, &types, &generics, &mut errors)
                && ty.is_compile_time()
            {
                let name = types.name_of(ty);
                errors.push(SyntaxError::new(
                    param.ty.span,
                    format!(
                        "a {name} exists only while compiling, so it cannot be passed; \
                         write it where it is used"
                    ),
                ));
            }
            let Some(ty) = resolve_written(&param.ty, &types, &generics, &mut errors) else {
                params.push(ParamSig {
                    ty: Type::I32,
                    borrow: None,
                });
                continue;
            };
            params.push(ParamSig {
                ty,
                borrow: param.ty.borrow,
            });
        }
        let ret = f
            .ret
            .as_ref()
            .and_then(|written| resolve_written(written, &types, &generics, &mut errors));
        if let Some(written) = f.ret.as_ref()
            && written.borrow.is_some()
        {
            errors.push(SyntaxError::new(
                written.span,
                "a reference cannot be returned: it is a name for a place in the \
                 caller, and the caller already has that name",
            ));
        }
        // An option is the one storage type that can come back: vanilla's call
        // outcome is a value and whether there was one, which is exactly an option
        // (spec section 6.28). What it holds still has to fit in the value.
        if let (Some(Type::Option(id)), Some(written)) = (ret, f.ret.as_ref())
            && types.inner(id).is_storage()
        {
            let inner = types.name_of(types.inner(id));
            errors.push(SyntaxError::new(
                written.span,
                format!(
                    "an Option<{inner}> cannot come back from a function: the value \
                     half of a call's outcome is a single number"
                ),
            ));
        }
        // Vanilla's function return is a single integer, so there is nowhere for a
        // compound to come back in.
        if let (Some(ty), Some(written)) = (ret, f.ret.as_ref())
            && ty.is_storage()
            && !matches!(ty, Type::Option(_))
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
        let index = items.len();
        // A template has no id of its own: only its instances are real functions. A
        // borrowed parameter makes one too, because the path it stands for is only
        // known at the call site (spec section 6.24).
        let borrows = params.iter().any(|param| param.borrow.is_some());
        let id = (generics.is_empty() && !borrows).then_some(FnId(concrete as u32));
        if id.is_some() {
            concrete += 1;
        }
        signatures.insert(
            name.clone(),
            Signature {
                id,
                generics,
                params,
                ret,
                ctx,
                item: index,
            },
        );
        items.push((item, f, name));
    }

    let mut instances = Instances {
        next: concrete as u32,
        ..Instances::default()
    };
    let mut functions = Vec::new();
    for (item, f, name) in &items {
        let signature = signatures[name].clone();
        // Templates are lowered once per set of type arguments, further down.
        let Some(id) = signature.id else {
            continue;
        };
        let lowered = lower_function(
            LowerOne {
                item,
                f,
                id,
                name: name.replace("::", "/").to_lowercase(),
                signature: &signature,
                type_params: HashMap::new(),
                borrows: Vec::new(),
            },
            namespace,
            &types,
            &signatures,
            toolchain,
            &mut instances,
            &mut references,
            &mut errors,
        );
        functions.push(lowered);
    }

    // Instances, in the order they were asked for. Lowering one can ask for more, so
    // the list is walked by index rather than iterated.
    let mut at = 0;
    while at < instances.pending.len() {
        let pending = instances.pending[at].clone();
        at += 1;
        let (item, f, name) = &items[pending.item];
        let signature = signatures[name].clone();
        let type_params: HashMap<String, Type> = signature
            .generics
            .iter()
            .map(|param| param.name.clone())
            .zip(pending.args.iter().copied())
            .collect();
        let instance = Signature {
            id: Some(pending.id),
            params: signature
                .params
                .iter()
                .map(|param| ParamSig {
                    ty: types.substitute(param.ty, &pending.args),
                    ..*param
                })
                .collect(),
            ret: signature.ret.map(|ty| types.substitute(ty, &pending.args)),
            ..signature.clone()
        };
        let lowered = lower_function(
            LowerOne {
                item,
                f,
                id: pending.id,
                name: pending.name.clone(),
                signature: &instance,
                type_params,
                borrows: pending.borrows.clone(),
            },
            namespace,
            &types,
            &signatures,
            toolchain,
            &mut instances,
            &mut references,
            &mut errors,
        );
        functions.push(lowered);
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

/// One function to lower: the template it came from, and what it was instantiated with.
struct LowerOne<'a> {
    item: &'a ast::Item,
    f: &'a ast::FnItem,
    id: FnId,
    /// The name it is emitted under, which for an instance carries its type arguments.
    name: String,
    signature: &'a Signature,
    /// Type parameters bound to what this instance was asked for.
    type_params: HashMap<String, Type>,
    /// What each borrowed parameter was lent, in parameter order.
    borrows: Vec<Option<Place>>,
}

#[allow(clippy::too_many_arguments)]
fn lower_function(
    one: LowerOne,
    namespace: &str,
    types: &Types,
    signatures: &HashMap<String, Signature>,
    toolchain: Option<&Schema>,
    instances: &mut Instances,
    references: &mut Vec<Reference>,
    errors: &mut Vec<SyntaxError>,
) -> Function {
    let LowerOne {
        item,
        f,
        id,
        name,
        signature,
        type_params,
        borrows,
    } = one;
    {
        let mut cx = FnLowering {
            locals: Vec::new(),
            namespace,
            function: name.clone(),
            place_aliases: HashMap::new(),
            types,
            type_params,
            instances,
            scopes: vec![HashMap::new()],
            selector_aliases: HashMap::new(),
            expected: None,
            provided: Vec::new(),
            in_entity_loop: false,
            loop_depth: 0,
            ret: signature.ret,
            signatures,
            toolchain,
            references,
            errors,
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
        let tagged = attrs
            .iter()
            .any(|a| matches!(a, Attr::Tick | Attr::Load | Attr::Test));
        if tagged && !cx.provided.is_empty() {
            cx.error(
                f.name.span,
                "a #[tick], #[load] or #[test] function cannot require a context: it is \
                 invoked with no executor, so it would silently do nothing",
            );
        }
        // Nothing hands a test arguments or reads its answer, so a signature that
        // wants either is a mistake that would never show up at runtime.
        if attrs.contains(&Attr::Test) && (!f.params.is_empty() || f.ret.is_some()) {
            cx.error(
                f.name.span,
                "a #[test] function takes no arguments and returns nothing: \
                 it is called on its own",
            );
        }
        // The receiver is a parameter like any other, under the name `self`.
        let names: Vec<String> = f
            .receiver
            .iter()
            .map(|_| "self".to_owned())
            .chain(f.params.iter().map(|param| param.name.name.clone()))
            .collect();
        let mut params = Vec::new();
        for (index, (name, param)) in names.iter().zip(&signature.params).enumerate() {
            match param.borrow {
                // Borrowed: the name stands for the caller's place, and nothing is
                // written before the call (spec section 6.24).
                Some(borrow) => {
                    let local = cx.declare(name, param.ty, false);
                    if let Some(Some(place)) = borrows.get(index) {
                        let mut place = place.clone();
                        place.mutable = borrow == ast::Borrow::Mutable;
                        place.via = name.clone();
                        cx.place_aliases.insert(local, place);
                    }
                }
                None => params.push(cx.declare(name, param.ty, false)),
            }
        }
        let body = cx.block(&f.body);
        let locals = cx.locals;
        let returns = always_returns(&body);
        let errors = cx.errors;
        if signature.ret.is_some() && !returns {
            errors.push(SyntaxError::new(
                f.name.span,
                "this function can reach its end without returning a value",
            ));
        }
        Function {
            id,
            path: format!("{namespace}:{name}"),
            name,
            attrs,
            params,
            ret: signature.ret,
            locals,
            body,
            span: item.span,
        }
    }
}

/// The program's `struct` and `enum` definitions.
///
/// Two passes: the names first, so a field can refer to a type declared further down
/// the file, then the fields.
fn collect_types(file: &SourceFile, errors: &mut Vec<SyntaxError>) -> Types {
    let mut types = Types::default();
    let mut structs = Vec::new();
    let mut enums = Vec::new();
    let mut templates = Vec::new();
    for item in &file.items {
        let (name, ty) = match &item.kind {
            ItemKind::Struct(declared) if !declared.generics.is_empty() => {
                // A template is not a type: only `Pair<i32>` is (spec section 4.12).
                if types.template_by_name.contains_key(&declared.name.name)
                    || types.by_name.contains_key(&declared.name.name)
                {
                    let text = &declared.name.name;
                    errors.push(SyntaxError::new(
                        declared.name.span,
                        format!("a type named '{text}' is already defined"),
                    ));
                    continue;
                }
                types
                    .template_by_name
                    .insert(declared.name.name.clone(), templates.len());
                templates.push((item, declared));
                continue;
            }
            ItemKind::Struct(declared) => {
                let id = StructId(structs.len() as u32);
                // `#[entity]` makes the struct a view: its fields are places on an
                // entity rather than a compound of its own (spec section 4.20).
                let ty = match has_attr(item, "entity") {
                    true => Type::View(id),
                    false => Type::Struct(id),
                };
                structs.push((item, declared));
                (&declared.name, ty)
            }
            ItemKind::Enum(declared) => {
                let ty = Type::Enum(EnumId(enums.len() as u32));
                enums.push((item, declared));
                (&declared.name, ty)
            }
            ItemKind::Fn(_) | ItemKind::Impl(_) => continue,
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
        if !has_attr(item, "entity") {
            reject_item_attrs(item, "a struct", errors);
        }
        let fields = collect_fields(&declared.fields, &types, &[], errors);
        let id = StructId(types.struct_count() as u32);
        types.structs.borrow_mut().push(StructDef {
            id,
            name: declared.name.name.clone(),
            fields,
            span: declared.name.span,
            from: None,
        });
    }
    // Templates last: their fields may name a plain struct, and their own parameters
    // are in scope only here.
    for (item, declared) in templates {
        reject_item_attrs(item, "a struct", errors);
        let generics = GenericDef::collect(&declared.generics);
        let fields = collect_fields(&declared.fields, &types, &generics, errors);
        types.templates.push(StructTemplate {
            name: declared.name.name.clone(),
            generics,
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
                let fields = collect_fields(&variant.fields, &types, &[], errors);
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
        .borrow()
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

/// Whether an item carries a bare `#[name]`.
fn has_attr(item: &ast::Item, name: &str) -> bool {
    item.attrs.iter().any(|attr| {
        matches!(attr.tokens.first().map(|t| &t.kind),
            Some(TokenKind::Ident(written)) if written == name)
            && attr.tokens.len() == 1
    })
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
    generics: &[GenericDef],
    errors: &mut Vec<SyntaxError>,
) -> Vec<Field> {
    let mut fields: Vec<Field> = Vec::new();
    for field in declared {
        if field.ty.borrow.is_some() {
            errors.push(SyntaxError::new(
                field.ty.span,
                "a field cannot hold a reference: a reference is a name for a place in \
                 the caller, and a compound outlives the call",
            ));
            continue;
        }
        let Some(ty) = resolve_written(&field.ty, types, generics, errors) else {
            continue;
        };
        // A compound holds values, and a compile-time type is not one.
        if ty.is_compile_time() {
            let name = types.name_of(ty);
            errors.push(SyntaxError::new(
                field.ty.span,
                format!("a {name} exists only while compiling, so a compound cannot hold one"),
            ));
            continue;
        }
        if fields.iter().any(|f| f.name == field.name.name) {
            let name = &field.name.name;
            errors.push(SyntaxError::new(
                field.name.span,
                format!("the field '{name}' is declared twice"),
            ));
            continue;
        }
        let (tag, rename) = nbt_attrs(field, ty, types, errors);
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
    types: &Types,
    errors: &mut Vec<SyntaxError>,
) -> (Option<NbtTag>, Option<String>) {
    // An option holds its value directly — what says `None` is the key not being
    // there — so its tag is the tag of what it holds (spec section 4.19).
    let ty = match ty {
        Type::Option(id) => types.inner(id),
        other => other,
    };
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
                    "a missing field is written into the type: declare it as \
                     Option<T> rather than saying so twice",
                ));
                continue;
            }
            match NbtTag::parse(option) {
                Some(_) if matches!(ty, Type::Param(_)) => errors.push(SyntaxError::new(
                    token.span,
                    "the tag of a field whose type is a parameter comes from the type \
                     argument, so it cannot be written here",
                )),
                Some(_) if ty == Type::Bool => errors.push(SyntaxError::new(
                    token.span,
                    "a bool is stored as a byte; vanilla has no other boolean tag",
                )),
                Some(_) if ty.is_compound() => errors.push(SyntaxError::new(
                    token.span,
                    "a struct field is a compound, so it has no scalar tag",
                )),
                Some(_) if ty.is_nbt_scalar() => {
                    let name = ty.name();
                    errors.push(SyntaxError::new(
                        token.span,
                        format!("an {name} is already a tag: it is stored the one way                                  vanilla writes it, so there is nothing to choose here"),
                    ))
                }
                Some(chosen) => tag = Some(chosen),
                None => {
                    let option = option.clone();
                    errors.push(SyntaxError::new(
                        token.span,
                        format!(
                            "unknown nbt option '{option}'; expected byte, short, int, \
                             long, float, double or rename"
                        ),
                    ));
                }
            }
        }
    }
    (tag, rename)
}

/// Whether a selector can only ever find one entity (spec section 3.19).
///
/// `@s`, `@p` and `@r` are single by definition; `@a` and `@e` need `limit=1` to be.
fn finds_one(text: &str) -> bool {
    let (head, body) = match text.split_once('[') {
        Some((head, body)) => (head.trim(), body),
        None => (text.trim(), ""),
    };
    if matches!(head, "@s" | "@p" | "@r" | "@n") {
        return true;
    }
    body.split(',')
        .any(|part| part.trim_end_matches(']').replace(' ', "") == "limit=1")
}

/// Whether a composite type can reach itself through its fields.
fn contains_itself(types: &Types, start: Type) -> bool {
    let mut stack = vec![start];
    let mut seen: Vec<Type> = Vec::new();
    while let Some(ty) = stack.pop() {
        for field in types.fields(ty) {
            if !field.ty.is_compound() {
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

/// An arithmetic node, with the scale correction `*` and `/` need on a fix
/// (spec section 6.25). Everything else is the plain operator.
fn combine(op: BinaryOp, lhs: Expr, rhs: Expr, ty: Type, span: Span) -> Expr {
    let plain = |lhs: Expr, rhs: Expr| Expr {
        kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
        ty,
        span,
    };
    let (BinaryOp::Mul | BinaryOp::Div, Type::Fix(Scale::Const(scale)), Type::Fix(_)) =
        (op, lhs.ty, rhs.ty)
    else {
        return plain(lhs, rhs);
    };
    // Multiply first in both cases: dividing first would throw away the digits the
    // scale exists to keep.
    match op {
        BinaryOp::Mul => by_const(BinaryOp::Div, plain(lhs, rhs), scale, ty, span),
        _ => plain(by_const(BinaryOp::Mul, lhs, scale, ty, span), rhs),
    }
}

/// `value * n` or `value / n`, where `n` is a scale rather than a value.
fn by_const(op: BinaryOp, value: Expr, n: u32, ty: Type, span: Span) -> Expr {
    let scale = Expr {
        kind: ExprKind::Int(n as i32),
        ty: Type::I32,
        span,
    };
    Expr {
        kind: ExprKind::Binary(op, Box::new(value), Box::new(scale)),
        ty,
        span,
    }
}

/// The `1000` of `fix<1000>`, or the const parameter that will become one.
fn resolve_scale(
    written: &ast::ScaleArg,
    generics: &[GenericDef],
    errors: &mut Vec<SyntaxError>,
) -> Option<Scale> {
    match written {
        ast::ScaleArg::Int(lit) => match lit.value >= 1 {
            true => Some(Scale::Const(lit.value as u32)),
            // A scale of zero would divide by zero, and a negative one flips the sign
            // of every value silently.
            false => {
                errors.push(SyntaxError::new(
                    lit.span,
                    "a scale is 1 or more: 'fix<1000>' counts thousandths",
                ));
                None
            }
        },
        ast::ScaleArg::Param(name) => {
            match generics.iter().position(|param| param.name == name.name) {
                Some(index) if generics[index].is_const => Some(Scale::Param(index as u32)),
                Some(_) => {
                    let name = &name.name;
                    errors.push(SyntaxError::new(
                        written.span(),
                        format!("'{name}' is a type parameter; a scale needs 'const {name}: i32'"),
                    ));
                    None
                }
                None => {
                    let name = &name.name;
                    errors.push(SyntaxError::new(
                        written.span(),
                        format!("unknown scale '{name}'; write a number or a const parameter"),
                    ));
                    None
                }
            }
        }
    }
}

fn resolve_type(
    written: &ast::TypeName,
    types: &Types,
    errors: &mut Vec<SyntaxError>,
) -> Option<Type> {
    resolve_written(written, types, &[], errors)
}

/// As `resolve_type`, with type parameters in scope.
fn resolve_written(
    written: &ast::TypeName,
    types: &Types,
    generics: &[GenericDef],
    errors: &mut Vec<SyntaxError>,
) -> Option<Type> {
    if written.name == "fix" {
        let scale = written.scale.as_ref().expect("the parser requires a scale");
        return resolve_scale(scale, generics, errors).map(Type::Fix);
    }
    if let Some(index) = generics.iter().position(|param| param.name == written.name) {
        if generics[index].is_const {
            let name = &written.name;
            errors.push(SyntaxError::new(
                written.span,
                format!("'{name}' is a const parameter: it is a scale, not a type"),
            ));
            return None;
        }
        if !written.args.is_empty() {
            let name = &written.name;
            errors.push(SyntaxError::new(
                written.span,
                format!("'{name}' is a type parameter; it does not take type arguments"),
            ));
            return None;
        }
        return Some(Type::Param(index as u32));
    }
    if written.name == "Option" {
        let [inner] = written.args.as_slice() else {
            errors.push(SyntaxError::new(
                written.span,
                "Option takes one type argument: write 'Option<i32>'",
            ));
            return None;
        };
        let inner = resolve_written(inner, types, generics, errors)?;
        if inner.is_compile_time() {
            let name = inner.name();
            errors.push(SyntaxError::new(
                written.span,
                format!("a {name} exists only while compiling, so it is never missing"),
            ));
            return None;
        }
        // What says `None` is the path not being there, and a path is either there or
        // not: there is no second level to tell apart (spec section 4.19).
        if matches!(inner, Type::Option(_)) {
            errors.push(SyntaxError::new(
                written.span,
                "Option<Option<T>> cannot be told apart: a path is either there or \
                 not, and that is the only thing 'None' is",
            ));
            return None;
        }
        return Some(types.option_of(inner));
    }
    if written.name == "Vec" {
        let [elem] = written.args.as_slice() else {
            errors.push(SyntaxError::new(
                written.span,
                "Vec takes one type argument: write 'Vec<i32>'",
            ));
            return None;
        };
        let elem = resolve_written(elem, types, generics, errors)?;
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
    if let Some(template) = types.template(&written.name) {
        let wanted = template.generics.len();
        if written.args.len() != wanted {
            let name = &written.name;
            let given = written.args.len();
            errors.push(SyntaxError::new(
                written.span,
                format!("'{name}' takes {wanted} type argument(s), but {given} were given"),
            ));
            return None;
        }
        let mut args = Vec::new();
        for arg in &written.args {
            args.push(resolve_written(arg, types, generics, errors)?);
        }
        return types.instantiate(&written.name, &args);
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
    /// The pack's namespace, which `text!` writes into the JSON it builds.
    namespace: &'a str,
    /// The name of the function being lowered: what a borrow lent from here records
    /// as its owner.
    function: String,
    /// Parameters taken by reference: the caller's place, under the local's name.
    place_aliases: HashMap<LocalId, Place>,
    types: &'a Types,
    /// The type parameters of the instance being lowered, already concrete.
    type_params: HashMap<String, Type>,
    instances: &'a mut Instances,
    ret: Option<Type>,
    signatures: &'a HashMap<String, Signature>,
    /// The command surface of the configured Minecraft version, if there is one.
    toolchain: Option<&'a Schema>,
    references: &'a mut Vec<Reference>,
    /// Innermost scope last. A `let` shadows an outer binding of the same name.
    scopes: Vec<HashMap<String, LocalId>>,
    /// Bindings that stand for a selector rather than a value.
    selector_aliases: HashMap<LocalId, String>,
    /// The type the expression being lowered is going into, where one is known.
    /// Only `None` reads it (spec section 3.18).
    expected: Option<Type>,
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
            ast::Stmt::Expr(AstExpr::Assert(assert)) => {
                let cond = self.condition(&assert.cond)?;
                Some(Stmt::Assert {
                    cond,
                    message: assert.message.clone(),
                    span: assert.span,
                })
            }
            ast::Stmt::Expr(AstExpr::Macro(call)) => self.macro_call(call).map(Stmt::Raw),
            ast::Stmt::Expr(AstExpr::Assign(assign)) => self.assign(assign),
            ast::Stmt::Expr(AstExpr::Method(call)) => self.method(call, true),
            ast::Stmt::Expr(AstExpr::Call(call))
                if !self.signatures.contains_key(&call.callee.name) =>
            {
                let ExprKind::Command(text) = self.command(call)?.kind else {
                    return None;
                };
                Some(Stmt::Raw(RawCommand::literal(text, call.span)))
            }
            ast::Stmt::Expr(AstExpr::Call(call)) => {
                let (callee, _, args) = self.call_parts(call)?;
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
                let expr = self.expr_expecting(expr, want)?;
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
        // A call that answers with an option is matched where it stands: there is no
        // compound to look at, only the two halves of its outcome.
        if let AstExpr::Call(_) = &stmt.scrutinee {
            let value = self.expr(&stmt.scrutinee)?;
            let Type::Option(id) = value.ty else {
                let found = self.ty(value.ty);
                self.error(
                    stmt.scrutinee.span(),
                    format!("only an enum or an option can be matched on, found {found}"),
                );
                return None;
            };
            let inner = self.types.inner(id);
            if inner.is_storage() {
                let found = self.ty(inner);
                self.error(
                    stmt.scrutinee.span(),
                    format!("a {found} does not fit in a register; bind the option first"),
                );
                return None;
            }
            return self.option_match(Scrutinee::Option(value), inner, stmt);
        }
        let scrutinee = self.place(&stmt.scrutinee)?;
        if let Type::Option(id) = scrutinee.ty {
            let inner = self.types.inner(id);
            return self.option_match(Scrutinee::Place(scrutinee), inner, stmt);
        }
        let Type::Enum(id) = scrutinee.ty else {
            let found = self.ty(scrutinee.ty);
            self.error(
                stmt.scrutinee.span(),
                format!("only an enum or an option can be matched on, found {found}"),
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
                    self.arm(ArmTest::Other, Vec::new(), "other".to_owned(), &arm.body)
                }
                // `Some` and `None` belong to an option, and this is an enum.
                pattern @ (ast::Pattern::Some { .. } | ast::Pattern::None(_)) => {
                    let name = &def.name;
                    self.error(
                        pattern.span(),
                        format!("'{name}' is an enum, so its arms name its variants"),
                    );
                    return None;
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
                    if arms.iter().any(|a| a.test == ArmTest::Variant(index)) {
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
                        bindings.push((bind.name.clone(), field.nbt.clone(), field.ty, field.tag));
                    }
                    self.arm(
                        ArmTest::Variant(index),
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
                .filter(|(index, _)| {
                    !arms
                        .iter()
                        .any(|a| a.test == ArmTest::Variant(*index as u32))
                })
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
            scrutinee: Scrutinee::Place(scrutinee),
            arms,
            span: stmt.span,
        })
    }

    /// `match o { Some(x) => .., None => .. }` (spec section 6.28).
    ///
    /// Two arms, and the test is whether the path is there at all — there is no tag to
    /// look at, because what says `None` is the absence itself.
    fn option_match(
        &mut self,
        scrutinee: Scrutinee,
        inner: Type,
        stmt: &ast::MatchStmt,
    ) -> Option<Stmt> {
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
            let (test, bindings, path) = match &arm.pattern {
                ast::Pattern::Some { bind, .. } => (
                    ArmTest::Present,
                    // The option's own tag: what it holds is stored as it says.
                    vec![(bind.name.clone(), String::new(), inner, scrutinee.tag())],
                    "some",
                ),
                ast::Pattern::None(_) => (ArmTest::Absent, Vec::new(), "none"),
                ast::Pattern::Wildcard(_) => {
                    wildcard = true;
                    (ArmTest::Other, Vec::new(), "other")
                }
                ast::Pattern::Variant { ty, .. } => {
                    let name = &ty.name;
                    self.error(
                        arm.pattern.span(),
                        format!("an option is matched with 'Some(x)' and 'None', not '{name}'"),
                    );
                    return None;
                }
            };
            if arms.iter().any(|a| a.test == test) {
                self.error(arm.pattern.span(), "this arm is already covered");
                return None;
            }
            arms.push(self.arm(test, bindings, path.to_owned(), &arm.body));
        }
        let covered = |test: ArmTest| {
            arms.iter()
                .any(|a| a.test == test || a.test == ArmTest::Other)
        };
        match (covered(ArmTest::Present), covered(ArmTest::Absent)) {
            (true, true) => {}
            (present, _) => {
                let missing = if present { "None" } else { "Some(x)" };
                self.error(stmt.span, format!("this match does not cover {missing}"));
                return None;
            }
        }
        if wildcard && arms.len() > 2 {
            self.error(
                stmt.arms.last().expect("a wildcard arm").span,
                "this arm cannot be reached: Some and None are already covered",
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
        test: ArmTest,
        bindings: Vec<(String, String, Type, Option<NbtTag>)>,
        path: String,
        body: &ast::Block,
    ) -> Arm {
        self.scopes.push(HashMap::new());
        let bindings = bindings
            .into_iter()
            .map(|(name, nbt, ty, tag)| Binding {
                local: self.declare(&name, ty, false),
                nbt,
                ty,
                tag,
            })
            .collect();
        let body = body
            .stmts
            .iter()
            .filter_map(|stmt| self.stmt(stmt))
            .collect();
        self.scopes.pop();
        Arm {
            test,
            path,
            bindings,
            body,
        }
    }

    /// `positioned pos!(~ ~1 ~) { .. }` (spec section 6.38).
    ///
    /// It provides a position and nothing else: there is no entity behind a
    /// coordinate, so `@s` inside means whatever it meant outside.
    fn positioned_stmt(&mut self, stmt: &ast::ContextStmt) -> Option<Stmt> {
        let value = self.expr(&stmt.selector)?;
        let ExprKind::Pos(text) = value.kind else {
            let found = self.ty(value.ty);
            self.error(
                value.span,
                format!(
                    "'positioned' takes coordinates, as in 'positioned pos!(~ ~1 ~)'; found {found}"
                ),
            );
            return None;
        };
        let inline = self.inline_attr(&stmt.attrs)?;
        self.provided.push(Ctx::Position);
        self.scopes.push(HashMap::new());
        let body = stmt
            .body
            .stmts
            .iter()
            .filter_map(|stmt| self.stmt(stmt))
            .collect();
        self.scopes.pop();
        self.provided.pop();
        Some(Stmt::Context {
            kind: ContextKind::Positioned,
            selector: Selector {
                text,
                span: value.span,
            },
            body,
            inline,
            span: stmt.span,
        })
    }

    fn context_stmt(&mut self, stmt: &ast::ContextStmt) -> Option<Stmt> {
        // `for` over a list is a different construct that happens to share a keyword.
        if stmt.kind == ast::ContextKind::For
            && let Some(place) = self.list_source(&stmt.selector)
        {
            return self.for_vec(stmt, place);
        }
        // `positioned` takes coordinates rather than a selector, and its body runs
        // once rather than once per entity (spec section 6.38).
        if stmt.kind == ast::ContextKind::Positioned {
            return self.positioned_stmt(stmt);
        }
        let selector = self.selector(&stmt.selector)?;
        let kind = match stmt.kind {
            ast::ContextKind::As => ContextKind::As,
            ast::ContextKind::At => ContextKind::At,
            ast::ContextKind::For => ContextKind::For,
            ast::ContextKind::Positioned => unreachable!("handled above"),
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

    /// The list a `for` iterates, if that is what it was given.
    ///
    /// Reported quietly: anything that is not a list is a selector, and the selector
    /// path produces the diagnostic when it is not one of those either.
    fn list_source(&mut self, expr: &AstExpr) -> Option<Place> {
        if !matches!(
            expr,
            AstExpr::Path(_) | AstExpr::Field(_) | AstExpr::Index(_)
        ) {
            return None;
        }
        let before = self.errors.len();
        let place = self.place(expr);
        match place {
            Some(place) if matches!(place.ty, Type::Vec(_)) => Some(place),
            _ => {
                self.errors.truncate(before);
                None
            }
        }
    }

    /// `for x in v { .. }`.
    fn for_vec(&mut self, stmt: &ast::ContextStmt, source: Place) -> Option<Stmt> {
        let Type::Vec(id) = source.ty else {
            unreachable!("checked by the caller")
        };
        if !source.is_static() {
            self.error(
                stmt.selector.span(),
                "an index that is only known at runtime has to be the last step; \
                 read the element into a binding first",
            );
            return None;
        }
        let elem = self.types.element(id);
        let inline = self.inline_attr(&stmt.attrs)?;
        if inline != Inline::Auto {
            self.error(stmt.span, "a loop is always its own function");
            return None;
        }
        let name = stmt.binding.as_ref().expect("'for' always binds a name");
        self.scopes.push(HashMap::new());
        let binding = self.declare(&name.name, elem, false);
        self.loop_depth += 1;
        // Not an entity loop: `continue` here means the next element, which is the
        // same thing `while` means by it.
        let outer = std::mem::replace(&mut self.in_entity_loop, false);
        let body = stmt
            .body
            .stmts
            .iter()
            .filter_map(|stmt| self.stmt(stmt))
            .collect();
        self.in_entity_loop = outer;
        self.loop_depth -= 1;
        self.scopes.pop();
        Some(Stmt::ForVec {
            source,
            binding,
            body,
            span: stmt.span,
        })
    }

    /// A selector expression: a literal, or a name bound to one.
    /// `Mob::of(@s)`: a name for one entity's NBT (spec section 6.29).
    fn view_of(&mut self, view: &ast::ViewOfExpr, span: Span) -> Option<Expr> {
        let Some(Type::View(id)) = self.types.get(&view.ty.name) else {
            let name = &view.ty.name;
            self.error(
                view.ty.span,
                format!("'{name}' is not a view; declare it with #[entity]"),
            );
            return None;
        };
        let selector = self.selector(&view.selector)?;
        // Vanilla's `data` commands take one entity and fail silently on several.
        // Which entities a selector finds is not knowable here, but how many it may
        // find is (spec section 3.19).
        if !finds_one(&selector.text) {
            let text = &selector.text;
            self.error(
                selector.span,
                format!(
                    "'{text}' can find more than one entity, and data takes exactly                      one; add 'limit=1' or use '@s', '@p' or '@r'"
                ),
            );
            return None;
        }
        if selector.text == "@s" {
            self.require(Ctx::Entity, selector.span, "a view of '@s'");
        }
        Some(Expr {
            kind: ExprKind::View(Place {
                root: Root::Entity {
                    selector: selector.text,
                },
                steps: Vec::new(),
                ty: Type::View(id),
                tag: None,
                // Settled by the binding it goes into.
                mutable: false,
                via: view.ty.name.clone(),
            }),
            ty: Type::View(id),
            span,
        })
    }

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
            // `nbt!` is the other one: it is checked against the type it lands in.
            (AstExpr::Macro(call), Some(written)) if call.name.name == "nbt" => {
                let want = self.resolve(written)?;
                self.nbt_lit(call, want)?
            }
            // And anything holding a `None`, which says nothing about its own type.
            (value, Some(written)) => {
                let want = self.resolve(written)?;
                self.expr_expecting(value, want)?
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
        // A compile-time value has nothing to put anywhere: the binding is a name for
        // it, and the `let` itself is not a statement at all.
        match &value.kind {
            ExprKind::Selector(text) => {
                self.selector_aliases.insert(local, text.clone());
                return None;
            }
            ExprKind::View(place) => {
                let mut place = place.clone();
                place.mutable = stmt.mutable;
                place.via = stmt.name.name.clone();
                self.place_aliases.insert(local, place);
                return None;
            }
            _ if value.ty.is_compile_time() => {
                let found = self.ty(value.ty);
                self.error(
                    stmt.value.span(),
                    format!("a {found} cannot be bound to a name; write it where it is used"),
                );
                return None;
            }
            _ => {}
        }
        Some(Stmt::Let {
            local,
            value,
            span: stmt.span,
        })
    }

    fn assign(&mut self, assign: &ast::AssignExpr) -> Option<Stmt> {
        // The place first: what is being written into is what an untyped `None` on
        // the right needs to know.
        let place = self.place(&assign.target)?;
        let value = self.expr_expecting(&assign.value, place.ty)?;
        // Mutability belongs to the binding, or to the borrow it came through.
        if !place.mutable {
            let name = &place.via;
            self.error(
                assign.span,
                format!("'{name}' is not mutable; declare it with 'let mut'"),
            );
            return None;
        }
        // A compound assignment is the arithmetic, so it inherits arithmetic's rules.
        if let Some(op) = assign.op {
            let ok = match (place.ty, value.ty) {
                (Type::I32, Type::I32) => true,
                (Type::Fix(_), Type::I32) => matches!(op, BinaryOp::Mul | BinaryOp::Div),
                (Type::Fix(_), _) => place.ty == value.ty,
                _ => false,
            };
            if !ok {
                let message = match matches!(place.ty, Type::I32 | Type::Fix(_)) {
                    // The place is the problem: nothing else takes arithmetic.
                    false => format!("compound assignment needs i32, found {}", self.ty(place.ty)),
                    true => format!(
                        "expected {}, found {}",
                        self.ty(place.ty),
                        self.ty(value.ty)
                    ),
                };
                self.error(assign.span, message);
                return None;
            }
            // A scale correction is a second command, and `operation` has room for
            // one. `a *= b` on a fix becomes `a = a * b` (spec section 6.25).
            if matches!(op, BinaryOp::Mul | BinaryOp::Div)
                && matches!(place.ty, Type::Fix(_))
                && place.ty == value.ty
            {
                let target = self.expr(&assign.target)?;
                let value = combine(op, target, value, place.ty, assign.span);
                return Some(Stmt::Assign {
                    place,
                    op: None,
                    value,
                    span: assign.span,
                });
            }
            return Some(Stmt::Assign {
                place,
                op: assign.op,
                value,
                span: assign.span,
            });
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
                // A parameter taken by reference is a name for the caller's place.
                if let Some(alias) = self.place_aliases.get(&local) {
                    return Some(alias.clone());
                }
                let binding = &self.locals[local.0 as usize];
                let (ty, mutable) = (binding.ty, binding.mutable);
                Some(Place {
                    root: Root::Local(local),
                    steps: Vec::new(),
                    ty,
                    tag: NbtTag::default_for(ty),
                    mutable,
                    via: name.name.clone(),
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
                // A view's fields are places on an entity, and are declared the same
                // way a struct's are (spec section 4.20).
                let (Type::Struct(id) | Type::View(id)) = base.ty else {
                    let found = self.ty(base.ty);
                    self.error(
                        access.base.span(),
                        format!("{found} has no fields; only a struct or a view does"),
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
                    steps,
                    ty,
                    tag,
                    ..base
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
                    steps,
                    ty,
                    tag: NbtTag::default_for(ty),
                    ..base
                })
            }
            other => {
                self.error(other.span(), "expected a binding or one of its fields");
                None
            }
        }
    }

    /// A method call: an inherent method from an `impl`, or one of the two on a list.
    fn method(&mut self, call: &ast::MethodCall, as_statement: bool) -> Option<Stmt> {
        match self.method_call(call, as_statement)? {
            MethodOutcome::Stmt(stmt) => Some(stmt),
            MethodOutcome::Value(_) => {
                self.error(call.span, "this expression has no effect");
                None
            }
        }
    }

    /// The methods a `String` has: only the ones vanilla can do (spec section 4.17).
    fn string_method(&mut self, place: Place, call: &ast::MethodCall) -> Option<Expr> {
        let span = call.span;
        match call.name.name.as_str() {
            "len" => {
                if !call.args.is_empty() {
                    self.error(span, "'len' takes no arguments");
                    return None;
                }
                // `data get` on a string answers with its length, which is why this
                // costs one command and not a loop.
                Some(Expr {
                    kind: ExprKind::Len(place),
                    ty: Type::I32,
                    span,
                })
            }
            "slice" => {
                let [ast::Expr::Range(range)] = call.args.as_slice() else {
                    self.error(span, "'slice' takes one range, as in 's.slice(1..3)'");
                    return None;
                };
                // The bounds are part of the command's text, so they have to be known
                // now. A runtime bound would need a macro, and nothing asks for one.
                let bound = |value: &Option<Box<ast::Expr>>| match value.as_deref() {
                    None => Ok(None),
                    Some(ast::Expr::Int(lit)) => Ok(Some(lit.value)),
                    Some(other) => Err(other.span()),
                };
                let (start, end) = match (bound(&range.start), bound(&range.end)) {
                    (Ok(start), Ok(end)) => (start, end),
                    (Err(span), _) | (_, Err(span)) => {
                        self.error(span, "a slice's bounds have to be known while compiling");
                        return None;
                    }
                };
                Some(Expr {
                    kind: ExprKind::Slice { place, start, end },
                    ty: Type::Str,
                    span,
                })
            }
            name => {
                self.error(
                    call.name.span,
                    format!(
                        "'String' has no method named '{name}'; vanilla can measure, \
                         slice and compare a string, and nothing else"
                    ),
                );
                None
            }
        }
    }

    /// `x.as_f64()` and `d.as_i32()`: the two directions of the score/storage round
    /// trip (spec section 4.16).
    fn convert(&mut self, place: Place, target: Type, call: &ast::MethodCall) -> Option<Expr> {
        let span = call.span;
        if !call.args.is_empty() {
            let name = &call.name.name;
            self.error(span, format!("'{name}' takes no arguments"));
            return None;
        }
        let scale = match (place.ty, target) {
            // Out of storage: `data get` multiplies by the scale as it reads.
            (from, Type::I32) if from.is_nbt_scalar() => {
                return Some(Expr {
                    kind: ExprKind::ReadScaled { place, scale: 1 },
                    ty: target,
                    span,
                });
            }
            // Into storage, keeping the units: a fix divides by its scale on the way.
            (Type::Fix(Scale::Const(scale)), Type::F32 | Type::F64) => scale,
            (Type::I32, target) if target.is_nbt_scalar() => 1,
            // A fix into an integer tag would have to choose between the raw units
            // and the rounded value, and neither is what the spelling says.
            (from, to) => {
                let (from, to) = (self.ty(from), self.ty(to));
                self.error(
                    span,
                    format!("there is no conversion from {from} to {to}; NBT numbers                              go through i32, and a fix only through f32 or f64"),
                );
                return None;
            }
        };
        let value = Expr {
            ty: place.ty,
            kind: ExprKind::Field(place),
            span,
        };
        Some(Expr {
            kind: ExprKind::AsNbt {
                value: Box::new(value),
                scale,
            },
            ty: target,
            span,
        })
    }

    /// `text!(a, b, c)` (spec section 3.22).
    fn text_macro(&mut self, call: &ast::TextMacro) -> Option<Component> {
        let mut parts = Vec::new();
        let mut ok = true;
        for arg in &call.args {
            match self.component(arg) {
                Some(part) => parts.push(part),
                None => ok = false,
            }
        }
        if !ok {
            return None;
        }
        if parts.len() == 1 {
            return parts.pop();
        }
        // The head is empty on purpose: the first element of a list is what the rest
        // inherit style from, so writing anything there would colour its siblings.
        let mut joined = Component::text("");
        joined.extra = parts;
        Some(joined)
    }

    /// One argument of `text!`, as a component.
    fn component(&mut self, expr: &AstExpr) -> Option<Component> {
        let value = self.expr(expr)?;
        match (&value.kind, value.ty) {
            (ExprKind::Component(component), _) => Some(component.clone()),
            (ExprKind::Str(text), _) => Some(Component::text(text)),
            (ExprKind::Int(n), _) => Some(Component::text(&n.to_string())),
            (ExprKind::Bool(b), _) => Some(Component::text(&b.to_string())),
            // A binding is *named*, not read: vanilla's JSON can point at a score or a
            // storage path itself, which is why `text!` costs no commands at all.
            (ExprKind::Local(local), ty) => self.binding_component(*local, ty, value.span),
            _ => {
                self.error(
                    value.span,
                    "text! can only show a literal or a binding; bind this to a name \
                     first and write the name",
                );
                None
            }
        }
    }

    /// The component that names where a binding's value lives.
    fn binding_component(&mut self, local: LocalId, ty: Type, span: Span) -> Option<Component> {
        let binding = self.locals[local.0 as usize].name.clone();
        let mut component = Component::default();
        match ty {
            // Vanilla's score component has no scale, so the digits on the board are
            // what would be shown: 3142 for 3.142 (spec section 3.21 says the same of
            // `raw!`).
            Type::Fix(_) => {
                self.error(
                    span,
                    format!(
                        "'{binding}' is a fixed-point number, which holds scaled units; \
                         a score component cannot divide it back"
                    ),
                );
                None
            }
            Type::I32 | Type::Bool => {
                let player = crate::names::fake_player(&self.function, &binding);
                let objective = crate::names::var_objective(self.namespace);
                component.set(
                    "score",
                    format!(
                        "{{\"name\":{},\"objective\":{}}}",
                        quoted_json(&player),
                        quoted_json(&objective)
                    ),
                );
                Some(component)
            }
            ty if ty.is_storage_scalar() => {
                let path = crate::names::var_path(&self.function, &binding);
                component.set("nbt", quoted_json(&path));
                component.set(
                    "storage",
                    quoted_json(&crate::names::storage(self.namespace)),
                );
                Some(component)
            }
            ty => {
                let found = self.ty(ty);
                self.error(span, format!("a {found} cannot be shown by text!"));
                None
            }
        }
    }

    /// `"a".red()` and `text!(..).bold()`: styling, which makes its receiver a
    /// component (spec section 3.22).
    fn style_method(
        &mut self,
        call: &ast::MethodCall,
        key: &'static str,
        fixed: Option<String>,
    ) -> Option<Expr> {
        let name = &call.name.name;
        let value = match fixed {
            Some(value) => {
                if !call.args.is_empty() {
                    self.error(call.span, format!("'{name}' takes no arguments"));
                    return None;
                }
                value
            }
            None => match call.args.as_slice() {
                [AstExpr::Str(lit)] => quoted_json(&lit.value),
                _ => {
                    self.error(
                        call.span,
                        format!("'{name}' takes one string, as in '.color(\"#ff8800\")'"),
                    );
                    return None;
                }
            },
        };
        let mut component = self.component(&call.receiver)?;
        component.set(key, value);
        Some(Expr {
            kind: ExprKind::Component(component),
            ty: Type::Component,
            span: call.span,
        })
    }

    fn method_call(&mut self, call: &ast::MethodCall, as_statement: bool) -> Option<MethodOutcome> {
        // Style method names are reserved: they are what turns a value into a chat
        // component, and their receiver need not be a place (spec section 3.22).
        if let Some((key, fixed)) = style_member(&call.name.name) {
            return Some(MethodOutcome::Value(self.style_method(call, key, fixed)?));
        }
        let place = self.place(&call.receiver)?;
        // An inherent method wins: `impl` is how a type gets behaviour of its own.
        let key = format!("{}::{}", self.types.name_of(place.ty), call.name.name);
        if self.signatures.contains_key(&key) {
            let (callee, ty, args) = self.invoke(&key, call.span, Some(place), &call.args)?;
            return Some(match (as_statement, ty) {
                (true, _) => MethodOutcome::Stmt(Stmt::CallFor {
                    callee,
                    args,
                    span: call.span,
                }),
                (false, Some(ty)) => MethodOutcome::Value(Expr {
                    kind: ExprKind::Call { callee, args },
                    ty,
                    span: call.span,
                }),
                (false, None) => {
                    let name = &call.name.name;
                    self.error(call.span, format!("'{name}' does not return a value"));
                    return None;
                }
            });
        }
        // The NBT interop conversions (spec section 4.16). Methods rather than free
        // functions: the receiver is what decides which direction this is.
        if let Some(target) = Type::parse(call.name.name.strip_prefix("as_").unwrap_or("")) {
            return Some(MethodOutcome::Value(self.convert(place, target, call)?));
        }
        // `expect` unwraps an option and, in a debug build, says so when there was
        // nothing to unwrap (spec section 4.21).
        if let Type::Option(id) = place.ty
            && call.name.name == "expect"
        {
            let inner = self.types.inner(id);
            let [ast::Expr::Str(message)] = call.args.as_slice() else {
                self.error(call.span, "'expect' takes one message, as a string literal");
                return None;
            };
            if inner.is_storage() {
                let found = self.ty(inner);
                self.error(
                    call.span,
                    format!(
                        "'expect' unwraps into a register, and a {found} does not fit \
                             in one; match on it instead"
                    ),
                );
                return None;
            }
            return Some(MethodOutcome::Value(Expr {
                kind: ExprKind::Expect {
                    value: Box::new(Expr {
                        ty: place.ty,
                        kind: ExprKind::Field(place),
                        span: call.span,
                    }),
                    message: message.value.clone(),
                },
                ty: inner,
                span: call.span,
            }));
        }
        if place.ty == Type::Str {
            return self.string_method(place, call).map(MethodOutcome::Value);
        }
        let Type::Vec(id) = place.ty else {
            let (found, name) = (self.ty(place.ty), &call.name.name);
            self.error(
                call.name.span,
                format!("'{found}' has no method named '{name}'"),
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
                if !place.mutable {
                    let name = &place.via;
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
                Some(MethodOutcome::Stmt(Stmt::Push {
                    place,
                    value,
                    span: call.span,
                }))
            }
            "len" => {
                if !call.args.is_empty() {
                    self.error(call.span, "'len' takes no arguments");
                    return None;
                }
                Some(MethodOutcome::Value(Expr {
                    kind: ExprKind::Len(place),
                    ty: Type::I32,
                    span: call.span,
                }))
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
            // `None` says nothing about what is missing, so the type has to come from
            // where it is going (spec section 3.18).
            AstExpr::Path(name) if name.name == "None" && self.lookup_quietly(name).is_none() => {
                match self.expected {
                    Some(ty @ Type::Option(_)) => Some(Expr {
                        kind: ExprKind::None,
                        ty,
                        span,
                    }),
                    Some(other) => {
                        let want = self.ty(other);
                        self.error(span, format!("expected {want}, found None"));
                        None
                    }
                    None => {
                        self.error(
                            span,
                            "'None' does not say what it is an option of: annotate the \
                             binding, as in 'let x: Option<i32> = None;'",
                        );
                        None
                    }
                }
            }
            AstExpr::Path(name) => {
                let local = self.lookup(name)?;
                // Reading through a borrow reads the caller's place, not a local of
                // this function — there is no local behind the name.
                if let Some(alias) = self.place_aliases.get(&local).cloned() {
                    return Some(Expr {
                        ty: alias.ty,
                        kind: ExprKind::Field(alias),
                        span,
                    });
                }
                Some(Expr {
                    kind: ExprKind::Local(local),
                    ty: self.locals[local.0 as usize].ty,
                    span,
                })
            }
            AstExpr::Unary(unary) => {
                let operand = self.expr(&unary.operand)?;
                let want = match unary.op {
                    // Negation is the same command whatever the scale is.
                    UnaryOp::Neg if matches!(operand.ty, Type::Fix(_)) => operand.ty,
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
                Some(combine(binary.op, lhs, rhs, ty, span))
            }
            AstExpr::Fix(cast) => self.fix_cast(cast, span),
            // Spec section 4.3: assignment is a statement. There is no `()` type for
            // it to produce, and inventing one to make this legal buys nothing.
            AstExpr::Assign(_) => {
                self.error(span, "an assignment is a statement and produces no value");
                None
            }
            // `Some(e)` is built in, not a call: `Option` is not a user enum and
            // there is no function behind the name (spec section 3.18).
            AstExpr::Call(call) if call.callee.name == "Some" => {
                let [value] = call.args.as_slice() else {
                    self.error(span, "Some takes one value");
                    return None;
                };
                let value = self.expr(value)?;
                if value.ty.is_compile_time() {
                    let found = value.ty.name();
                    self.error(span, format!("a {found} has no runtime value to hold"));
                    return None;
                }
                if matches!(value.ty, Type::Option(_)) {
                    self.error(
                        span,
                        "Some(None) and None cannot be told apart: a path is either \
                         there or not",
                    );
                    return None;
                }
                Some(Expr {
                    ty: self.types.option_of(value.ty),
                    kind: ExprKind::Some(Box::new(value)),
                    span,
                })
            }
            AstExpr::Call(call) if !self.signatures.contains_key(&call.callee.name) => {
                self.command(call)
            }
            AstExpr::Call(call) => {
                let (callee, ty, args) = self.call_parts(call)?;
                let Some(ty) = ty else {
                    let name = &call.callee.name;
                    self.error(span, format!("'{name}' does not return a value"));
                    return None;
                };
                Some(Expr {
                    kind: ExprKind::Call { callee, args },
                    ty,
                    span,
                })
            }
            AstExpr::Text(call) => {
                let component = self.text_macro(call)?;
                Some(Expr {
                    kind: ExprKind::Component(component),
                    ty: Type::Component,
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
                ty: Type::Str,
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
            // `nbt!` is checked against wherever it is going, so it can only be
            // written where that is known: an annotated `let`, or an argument whose
            // parameter names a concrete type (spec section 6.37).
            AstExpr::Macro(call) if call.name.name == "nbt" => match self.expected {
                Some(want) if !matches!(want, Type::Param(_)) => self.nbt_lit(call, want),
                _ => {
                    self.error(
                        span,
                        "nbt! has to know what it is being written into: annotate the \
                         binding, as in 'let m: Mob = nbt!({ .. });'",
                    );
                    None
                }
            },
            AstExpr::Macro(call) => {
                let name = &call.name.name;
                self.error(span, format!("'{name}!' does not produce a value"));
                None
            }
            AstExpr::Struct(lit) => self.composite_lit(lit),
            // A reference is a name for a place, and a name has to be a parameter to
            // stand for anything (spec section 4.13).
            AstExpr::Borrow(borrow) => {
                self.error(
                    borrow.span,
                    "a reference can only be an argument: pass it to a function, or \
                     use the place directly",
                );
                None
            }
            AstExpr::Field(_) | AstExpr::Index(_) => {
                let place = self.place(expr)?;
                Some(Expr {
                    ty: place.ty,
                    kind: ExprKind::Field(place),
                    span,
                })
            }
            AstExpr::Try(try_expr) => {
                let value = self.expr(&try_expr.value)?;
                let Type::Option(id) = value.ty else {
                    let found = self.ty(value.ty);
                    self.error(span, format!("'?' takes an Option, found {found}"));
                    return None;
                };
                // The value comes out into a register, so it has to be the kind of
                // thing a register holds (spec section 4.19).
                let inner = self.types.inner(id);
                if inner.is_storage() {
                    let found = self.ty(inner);
                    self.error(
                        span,
                        format!(
                            "'?' unwraps into a register, and a {found} does not \
                                 fit in one; match on it instead"
                        ),
                    );
                    return None;
                }
                // Leaving with nothing is only a thing a function that can return
                // nothing can do (spec section 4.19).
                if !matches!(self.ret, Some(Type::Option(_))) {
                    self.error(
                        span,
                        "'?' leaves the function with nothing, so the function has to \
                         return an Option",
                    );
                    return None;
                }
                Some(Expr {
                    ty: inner,
                    kind: ExprKind::Try(Box::new(value)),
                    span,
                })
            }
            AstExpr::Assert(_) => {
                self.error(span, "'debug_assert!' is a statement and produces no value");
                None
            }
            AstExpr::ViewOf(view) => self.view_of(view, span),
            AstExpr::Range(range) => {
                self.error(
                    range.span,
                    "a range is only the argument of 'slice'; there is no range type",
                );
                None
            }
            AstExpr::List(lit) => self.list_lit(lit, None),
            AstExpr::Method(call) => match self.method_call(call, false)? {
                MethodOutcome::Value(value) => Some(value),
                MethodOutcome::Stmt(_) => {
                    let name = &call.name.name;
                    self.error(span, format!("'{name}' does not return a value"));
                    None
                }
            },
        }
    }

    /// `fix::<S>(e)`: the raw units of an `i32`, or another fix restated at `S`.
    fn fix_cast(&mut self, cast: &ast::FixExpr, span: Span) -> Option<Expr> {
        let scale = self.scale(&cast.scale)?;
        let value = self.expr(&cast.value)?;
        let ty = Type::Fix(Scale::Const(scale));
        match value.ty {
            // The integer is already the value in raw units, so nothing runs.
            Type::I32 => Some(Expr { ty, ..value }),
            Type::Fix(Scale::Const(from)) if from == scale => Some(value),
            Type::Fix(Scale::Const(from)) => {
                let up = by_const(BinaryOp::Mul, value, scale, ty, span);
                Some(by_const(BinaryOp::Div, up, from, ty, span))
            }
            // Storage holds the real number; `data get` scales it on the way out
            // (spec section 6.26).
            other if other.is_nbt_scalar() => {
                let place = self.place(&cast.value)?;
                Some(Expr {
                    kind: ExprKind::ReadScaled { place, scale },
                    ty,
                    span,
                })
            }
            other => {
                let found = self.ty(other);
                self.error(
                    span,
                    format!("'fix::<{scale}>' takes an i32 or another fix, found {found}"),
                );
                None
            }
        }
    }

    /// The scale a `fix<S>` was written with. Inside an instance a const parameter is
    /// already a number, which is what the argument list carries (spec section 6.25).
    fn scale(&mut self, written: &ast::ScaleArg) -> Option<u32> {
        if let ast::ScaleArg::Param(name) = written
            && let Some(Type::Fix(Scale::Const(scale))) = self.type_params.get(&name.name).copied()
        {
            return Some(scale);
        }
        match resolve_scale(written, &[], self.errors)? {
            Scale::Const(scale) => Some(scale),
            // Only reachable in a template, and templates are never lowered.
            Scale::Param(_) => None,
        }
    }

    /// Typing for an operator with a fix on either side (spec section 4.14).
    fn fix_binary_type(
        &mut self,
        op: BinaryOp,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
    ) -> Option<Type> {
        use BinaryOp::*;
        // An integer multiplier has no scale of its own, so it needs no correction
        // and mixes freely. Only `*` and `/` take one: what `+ 1` should add is not
        // decidable from the spelling.
        match (op, lhs.ty, rhs.ty) {
            (Mul | Div, Type::Fix(_), Type::I32) => return Some(lhs.ty),
            (Mul, Type::I32, Type::Fix(_)) => return Some(rhs.ty),
            _ => {}
        }
        if lhs.ty != rhs.ty {
            let (a, b) = (self.ty(lhs.ty), self.ty(rhs.ty));
            self.error(
                span,
                format!("cannot mix {a} with {b}; convert one of them with 'fix::<S>(x)'"),
            );
            return None;
        }
        match op {
            Add | Sub | Mul | Div | Rem => Some(lhs.ty),
            Lt | Le | Gt | Ge | Eq | Ne => Some(Type::Bool),
            And | Or => {
                let found = self.ty(lhs.ty);
                self.error(span, format!("this operator needs bool, found {found}"));
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
            // The NBT scalars carry what vanilla wrote and nothing else: the
            // scoreboard has no arithmetic for them (requirements section 4.1).
            if side.ty.is_nbt_scalar() {
                let name = side.ty.name();
                self.error(
                    span,
                    format!(
                        "an {name} is for NBT interop and has no arithmetic; read it \
                         into a fix first, as in 'fix::<1000>(x)'"
                    ),
                );
                return None;
            }
            // Two compounds cannot be compared or combined: `execute if data` matches
            // against a literal, never against another path (spec section 4.8).
            if side.ty.is_compound() {
                let name = self.ty(side.ty);
                self.error(
                    span,
                    format!("'{name}' is a struct, and the game cannot compare two compounds"),
                );
                return None;
            }
        }
        // A string is in storage, but it is one value rather than a structure, and
        // vanilla can compare and splice one (spec section 4.17).
        if lhs.ty == Type::Str || rhs.ty == Type::Str {
            if lhs.ty != rhs.ty {
                let (a, b) = (self.ty(lhs.ty), self.ty(rhs.ty));
                self.error(span, format!("cannot mix {a} with {b}"));
                return None;
            }
            return match op {
                Eq | Ne => Some(Type::Bool),
                Add => Some(Type::Str),
                _ => {
                    self.error(
                        span,
                        "a String has no arithmetic; '+' joins two of them and \
                         that is the whole of it",
                    );
                    None
                }
            };
        }
        // A fix is an integer with a scale attached, and the scale decides which
        // operands go together (spec section 4.14).
        if matches!(lhs.ty, Type::Fix(_)) || matches!(rhs.ty, Type::Fix(_)) {
            return self.fix_binary_type(op, lhs, rhs, span);
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

    /// `block(pos!(~ ~-1 ~), minecraft:stone)` (spec section 6.39).
    ///
    /// Both arguments have to be known while compiling, which they are: a coordinate
    /// and a resource location are compile-time types (spec section 4.7). So the test
    /// is one `execute if block` and costs nothing to build.
    fn block_test(&mut self, call: &ast::CallExpr) -> Option<Expr> {
        let [at, id] = call.args.as_slice() else {
            self.error(
                call.span,
                "'block' takes a position and a block id, as in \
                 'block(pos!(~ ~-1 ~), minecraft:stone)'",
            );
            return None;
        };
        let at = self.expr(at)?;
        let id = self.expr(id)?;
        let (ExprKind::Pos(at_text), ExprKind::Resource(id_text)) = (&at.kind, &id.kind) else {
            self.error(
                call.span,
                "'block' takes a position and a block id, as in \
                 'block(pos!(~ ~-1 ~), minecraft:stone)'",
            );
            return None;
        };
        Some(Expr {
            kind: ExprKind::Block {
                at: at_text.clone(),
                id: id_text.clone(),
            },
            ty: Type::Bool,
            span: call.span,
        })
    }

    /// A command call, if the name is one. User functions win: defining `fn setblock`
    /// shadows the command, which is the only way to wrap one.
    fn command(&mut self, call: &ast::CallExpr) -> Option<Expr> {
        if self.signatures.contains_key(&call.callee.name) {
            return None;
        }
        // `block` is asked of the world rather than run at it, so it is built in
        // rather than read off the command table (spec section 6.39).
        if call.callee.name == "block" {
            return self.block_test(call);
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
        // Words and arguments interleave: a literal can follow an argument, as in
        // `playsound <sound> master <targets>`.
        let mut rendered_args = Vec::new();
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
            rendered_args.push(rendered);
        }
        let parts: Vec<String> = signature
            .parts
            .iter()
            .map(|part| match part {
                Part::Literal(word) => word.clone(),
                Part::Arg(index) => rendered_args[*index].clone(),
            })
            .collect();
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
            (ExprKind::Component(component), ArgType::Component) => component.render(),
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

    /// The callee and what it returns, with a generic call resolved to its instance.
    fn call_parts(&mut self, call: &ast::CallExpr) -> Option<(FnId, Option<Type>, Vec<Expr>)> {
        self.invoke(&call.callee.name, call.span, None, &call.args)
    }

    /// Resolves a call: checks the arguments, borrows what is borrowed, and picks the
    /// instance this combination of type arguments and lent places belongs to.
    fn invoke(
        &mut self,
        name: &str,
        span: Span,
        receiver: Option<Place>,
        written: &[AstExpr],
    ) -> Option<(FnId, Option<Type>, Vec<Expr>)> {
        let sig = self.signatures.get(name)?.clone();
        self.check_ctx(name, span, &sig.ctx);
        let given = written.len() + usize::from(receiver.is_some());
        if given != sig.params.len() {
            let (n, m) = (sig.params.len(), given);
            self.error(
                span,
                format!("'{name}' takes {n} argument(s), but {m} were given"),
            );
            return None;
        }

        // Every parameter is either a value to write before the call, or a place to
        // lend (spec section 6.24).
        let mut args: Vec<Expr> = Vec::new();
        let mut borrows: Vec<Option<Place>> = Vec::new();
        let mut actual: Vec<Type> = Vec::new();
        let mut receiver = receiver;
        let mut at = 0;
        for param in &sig.params {
            let place = receiver.take();
            let arg = match place {
                Some(_) => None,
                None => {
                    let arg = &written[at];
                    at += 1;
                    Some(arg)
                }
            };
            match param.borrow {
                None => {
                    let value = match (place, arg) {
                        // A receiver taken by value is read like any other expression.
                        (Some(place), _) => Expr {
                            ty: place.ty,
                            kind: ExprKind::Field(place),
                            span,
                        },
                        // The parameter's type is the expectation the argument is read
                        // under, which is what lets `nbt!` be written here
                        // (spec section 6.37).
                        (None, Some(arg)) => self.expr_expecting(arg, param.ty)?,
                        (None, None) => unreachable!("one of the two is always there"),
                    };
                    actual.push(value.ty);
                    args.push(value);
                    borrows.push(None);
                }
                Some(kind) => {
                    let place = match (place, arg) {
                        (Some(place), _) => place,
                        (None, Some(AstExpr::Borrow(borrow))) => {
                            if borrow.borrow != kind && kind == ast::Borrow::Mutable {
                                self.error(
                                    borrow.span,
                                    format!("'{name}' takes this by &mut; write '&mut'"),
                                );
                                return None;
                            }
                            self.place(&borrow.place)?
                        }
                        (None, Some(other)) => {
                            self.error(
                                other.span(),
                                format!("'{name}' takes this by reference; write '&' or '&mut'"),
                            );
                            return None;
                        }
                        (None, None) => unreachable!("one of the two is always there"),
                    };
                    // A borrow is a name for a path, and a path that is only finished
                    // at runtime has no name (requirements section 5).
                    if !place.is_static() {
                        self.error(
                            span,
                            "an element reached by a runtime index cannot be borrowed: \
                             its path is not known while compiling; assign to it instead",
                        );
                        return None;
                    }
                    if kind == ast::Borrow::Mutable && !place.mutable {
                        let via = &place.via;
                        self.error(
                            span,
                            format!("'{via}' is not mutable; declare it with 'let mut'"),
                        );
                        return None;
                    }
                    actual.push(place.ty);
                    borrows.push(Some(self.lend(&place)));
                }
            }
        }

        // The argument types decide the type arguments (spec section 4.12).
        let mut bound: Vec<Option<Type>> = vec![None; sig.generics.len()];
        for (param, actual) in sig.params.iter().zip(&actual) {
            self.types.unify(param.ty, *actual, &mut bound);
        }
        let Some(type_args) = bound.iter().copied().collect::<Option<Vec<Type>>>() else {
            let unknown: Vec<&str> = sig
                .generics
                .iter()
                .zip(&bound)
                .filter(|(_, known)| known.is_none())
                .map(|(param, _)| param.name.as_str())
                .collect();
            let list = unknown.join(", ");
            self.error(
                span,
                format!(
                    "cannot tell what {list} is in the call to '{name}'; \
                     no argument mentions it"
                ),
            );
            return None;
        };
        // Now that the parameters are concrete, they are checked as any others are.
        for (param, actual) in sig.params.iter().zip(&actual) {
            let want = self.types.substitute(param.ty, &type_args);
            if *actual != want {
                let (want, found) = (self.ty(want), self.ty(*actual));
                self.error(span, format!("expected {want}, found {found}"));
                return None;
            }
        }
        let ret = sig.ret.map(|ty| self.types.substitute(ty, &type_args));
        let id = match sig.id {
            Some(id) => id,
            None => self
                .instances
                .get_or_create(sig.item, type_args, borrows, name, self.types),
        };
        Some((id, ret, args))
    }

    /// Turns a place of this function into one the callee can name.
    fn lend(&self, place: &Place) -> Place {
        let root = match &place.root {
            Root::Local(id) => {
                let binding = &self.locals[id.0 as usize];
                Root::Lent {
                    owner: self.function.clone(),
                    local: binding.name.clone(),
                    storage: binding.ty.is_storage(),
                }
            }
            // Lending on what was already lent: the owner does not change.
            lent => lent.clone(),
        };
        Place {
            root,
            ..place.clone()
        }
    }

    /// Complains when the call needs a context the caller does not have.
    fn check_ctx(&mut self, name: &str, span: Span, ctx: &[Ctx]) {
        {
            {
                let missing: Vec<Ctx> = ctx
                    .iter()
                    .copied()
                    .filter(|ctx| !self.provided.contains(ctx))
                    .collect();
                if !missing.is_empty() {
                    let kinds = missing
                        .iter()
                        .map(|c| c.name())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.error(
                        span,
                        format!(
                            "'{name}' declares #[ctx({kinds})] but no {kinds} context is \
                             available here; wrap the call in an 'as' block, or declare \
                             #[ctx({kinds})] on this function too"
                        ),
                    );
                }
            }
        }
    }

    /// `nbt!({ hp: 20 })`, checked against the type it is being written into
    /// (spec section 4.18).
    fn nbt_lit(&mut self, call: &ast::MacroCall, want: Type) -> Option<Expr> {
        let mut at = 0;
        let text = self.nbt_value(&call.tokens, &mut at, want, None, call.span)?;
        if at < call.tokens.len() {
            self.error(call.tokens[at].span, "nbt! takes one value");
            return None;
        }
        Some(Expr {
            kind: ExprKind::Nbt(text),
            ty: want,
            span: call.span,
        })
    }

    /// One SNBT value, rendered as the game spells it.
    ///
    /// `want` is what the value has to be, and `tag` the tag the field it lands in
    /// was declared with, which decides the suffix a number gets.
    fn nbt_value(
        &mut self,
        tokens: &[Token],
        at: &mut usize,
        want: Type,
        tag: Option<NbtTag>,
        span: Span,
    ) -> Option<String> {
        let Some(token) = tokens.get(*at) else {
            self.error(span, "expected a value");
            return None;
        };
        let span = token.span;
        match &token.kind {
            TokenKind::Punct(Punct::LBrace) => self.nbt_compound(tokens, at, want, span),
            TokenKind::Punct(Punct::LBracket) => self.nbt_list(tokens, at, want, span),
            TokenKind::Str(text) => {
                *at += 1;
                if want != Type::Str {
                    let want = self.ty(want);
                    self.error(span, format!("expected {want}, found a string"));
                    return None;
                }
                Some(format!("{text:?}"))
            }
            TokenKind::Keyword(k @ (Keyword::True | Keyword::False)) => {
                let value = i32::from(*k == Keyword::True);
                *at += 1;
                if want != Type::Bool {
                    let want = self.ty(want);
                    self.error(span, format!("expected {want}, found a bool"));
                    return None;
                }
                Some(format!("{value}b"))
            }
            TokenKind::Punct(Punct::Minus) | TokenKind::Int(_) => {
                self.nbt_number(tokens, at, want, tag, span)
            }
            _ => {
                self.error(span, "expected a value");
                None
            }
        }
    }

    fn nbt_compound(
        &mut self,
        tokens: &[Token],
        at: &mut usize,
        want: Type,
        span: Span,
    ) -> Option<String> {
        // An enum is a compound too, but a variant has to be named to be chosen and
        // `nbt!` names no variant: `State::Idle` is how that is written.
        let Type::Struct(id) = want else {
            let want = self.ty(want);
            self.error(span, format!("expected {want}, found a compound"));
            return None;
        };
        let fields = self.types.struct_def(id).fields;
        *at += 1;
        let mut written: Vec<(String, String)> = Vec::new();
        while !self.eat_punct(tokens, at, Punct::RBrace) {
            let Some(key) = tokens.get(*at) else {
                self.error(span, "a compound has to be closed with '}'");
                return None;
            };
            let name = match &key.kind {
                TokenKind::Ident(name) => name.clone(),
                TokenKind::Str(name) => name.clone(),
                _ => {
                    self.error(key.span, "expected a field name");
                    return None;
                }
            };
            *at += 1;
            if !self.eat_punct(tokens, at, Punct::Colon) {
                self.error(key.span, "expected ':' after a field name");
                return None;
            }
            let Some(field) = fields.iter().find(|field| field.nbt == name) else {
                let ty = self.ty(want);
                self.error(
                    key.span,
                    format!("'{ty}' has no field written as '{name}' in NBT"),
                );
                return None;
            };
            let value = self.nbt_value(tokens, at, field.ty, field.tag, key.span)?;
            if written.iter().any(|(key, _)| *key == name) {
                self.error(key.span, format!("'{name}' is written twice"));
                return None;
            }
            written.push((name, value));
            if !self.eat_punct(tokens, at, Punct::Comma) {
                if !self.eat_punct(tokens, at, Punct::RBrace) {
                    self.error(key.span, "expected ',' or '}'");
                    return None;
                }
                break;
            }
        }
        // A value has to be whole: a compound with a field missing would leave the
        // binding holding something the type says cannot happen.
        if let Some(missing) = fields
            .iter()
            .find(|field| !written.iter().any(|(key, _)| *key == field.nbt))
        {
            let (ty, name) = (self.ty(want), &missing.nbt);
            self.error(span, format!("'{ty}' needs a value for '{name}' too"));
            return None;
        }
        let body = fields
            .iter()
            .map(|field| {
                let value = written
                    .iter()
                    .find(|(key, _)| *key == field.nbt)
                    .map(|(_, value)| value.clone())
                    .expect("every field is written");
                format!("{}:{value}", field.nbt)
            })
            .collect::<Vec<_>>()
            .join(",");
        Some(format!("{{{body}}}"))
    }

    fn nbt_list(
        &mut self,
        tokens: &[Token],
        at: &mut usize,
        want: Type,
        span: Span,
    ) -> Option<String> {
        let Type::Vec(id) = want else {
            let want = self.ty(want);
            self.error(span, format!("expected {want}, found a list"));
            return None;
        };
        let elem = self.types.element(id);
        let tag = NbtTag::default_for(elem);
        *at += 1;
        let mut values = Vec::new();
        while !self.eat_punct(tokens, at, Punct::RBracket) {
            values.push(self.nbt_value(tokens, at, elem, tag, span)?);
            if !self.eat_punct(tokens, at, Punct::Comma) {
                if !self.eat_punct(tokens, at, Punct::RBracket) {
                    self.error(span, "expected ',' or ']'");
                    return None;
                }
                break;
            }
        }
        Some(format!("[{}]", values.join(",")))
    }

    /// `20`, `-3`: an integer, which takes the suffix of the field it lands in.
    ///
    /// The suffix is never written: the field's type already says what tag it is
    /// stored as, and letting both say it would let them disagree (spec section 4.18).
    /// There is no decimal form either — the language has no float literal anywhere
    /// (spec section 2.5), and `nbt!` is not the place to invent one.
    fn nbt_number(
        &mut self,
        tokens: &[Token],
        at: &mut usize,
        want: Type,
        tag: Option<NbtTag>,
        span: Span,
    ) -> Option<String> {
        let negated = self.eat_punct(tokens, at, Punct::Minus);
        let Some(TokenKind::Int(value)) = tokens.get(*at).map(|t| &t.kind) else {
            self.error(span, "expected a number");
            return None;
        };
        let value = if negated { -*value } else { *value };
        *at += 1;
        let Some(tag) = tag.or_else(|| NbtTag::default_for(want)) else {
            let want = self.ty(want);
            self.error(span, format!("expected {want}, found a number"));
            return None;
        };
        if let Some(TokenKind::Ident(_)) = tokens.get(*at).map(|t| &t.kind) {
            let keyword = tag.keyword();
            self.error(
                tokens[*at].span,
                format!("no suffix here: the field is stored as a {keyword} already"),
            );
            return None;
        }
        if want == Type::Bool && !(0..=1).contains(&value) {
            self.error(span, "a bool is 0 or 1; write true or false");
            return None;
        }
        Some(format!("{value}{}", tag.suffix()))
    }

    fn eat_punct(&self, tokens: &[Token], at: &mut usize, punct: Punct) -> bool {
        if tokens.get(*at).map(|t| &t.kind) == Some(&TokenKind::Punct(punct)) {
            *at += 1;
            return true;
        }
        false
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
                // `~-1` is three tokens, and it is the offset datapacks write most.
                let negated = matches!(
                    tokens.peek().map(|t| &t.kind),
                    Some(TokenKind::Punct(Punct::Minus))
                );
                if negated {
                    tokens.next();
                }
                match tokens.peek().map(|t| &t.kind) {
                    Some(TokenKind::Int(n)) => {
                        let n = *n;
                        tokens.next();
                        Some(if negated { -n } else { n })
                    }
                    _ if negated => {
                        self.error(token.span, "expected a coordinate after '-'");
                        return None;
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
        let token = match call.tokens.as_slice() {
            [token] => token,
            [] => {
                self.error(call.span, "raw! takes a string literal");
                return None;
            }
            tokens => {
                self.error(tokens[1].span, "raw! takes a single string literal");
                return None;
            }
        };
        let TokenKind::Str(text) = &token.kind else {
            self.error(token.span, "raw! takes a string literal");
            return None;
        };
        self.interpolate(&text.clone(), token.span, call.span)
    }

    /// Splits a `raw!` string into literals and the values written `{name}`
    /// (spec section 3.21).
    ///
    /// `{` always opens an interpolation. Falling back to "a name that does not
    /// resolve is just text" would let a typo through silently, which is the whole
    /// reason this language exists.
    fn interpolate(&mut self, text: &str, span: Span, call: Span) -> Option<RawCommand> {
        let mut parts = Vec::new();
        let mut lit = String::new();
        let mut rest = text;
        let mut ok = true;
        while let Some(at) = rest.find(['{', '}']) {
            let (before, tail) = rest.split_at(at);
            lit.push_str(before);
            let (brace, tail) = tail.split_at(1);
            // `{{` and `}}` are the characters themselves.
            if let Some(after) = tail.strip_prefix(brace) {
                lit.push_str(brace);
                rest = after;
                continue;
            }
            if brace == "}" {
                self.error(
                    span,
                    "this '}' in raw! closes nothing; write '}}' to mean the character",
                );
                return None;
            }
            let Some(end) = tail.find('}') else {
                self.error(
                    span,
                    "this '{' in raw! is never closed; write '{{' to mean the character",
                );
                return None;
            };
            let (name, after) = tail.split_at(end);
            rest = &after[1..];
            match self.interpolated(name, span) {
                Some(Interpolated::Const(text)) => lit.push_str(&text),
                Some(Interpolated::Value(value)) => {
                    parts.push(RawPart::Lit(std::mem::take(&mut lit)));
                    parts.push(RawPart::Value(value));
                }
                None => ok = false,
            }
        }
        lit.push_str(rest);
        parts.push(RawPart::Lit(lit));
        ok.then_some(RawCommand { parts, span: call })
    }

    /// What `{name}` stands for: text that is known now, or a value to splice in.
    fn interpolated(&mut self, name: &str, span: Span) -> Option<Interpolated> {
        if name.is_empty() {
            self.error(span, "raw! needs a name between the braces");
            return None;
        }
        let value = self.expr(&AstExpr::Path(ast::Ident {
            name: name.to_owned(),
            span,
        }))?;
        if let ExprKind::Local(local) = &value.kind
            && let Some(text) = self.selector_aliases.get(local)
        {
            return Some(Interpolated::Const(text.clone()));
        }
        match value.ty {
            Type::I32 | Type::Bool | Type::Str => Some(Interpolated::Value(value)),
            // The scoreboard holds S-ths of a unit, so the digits would not be the
            // number the author wrote. Showing a real number is `text!`'s job.
            Type::Fix(_) => {
                self.error(
                    span,
                    format!(
                        "'{name}' is a fixed-point number, which holds scaled units;                          interpolating it would print the wrong value"
                    ),
                );
                None
            }
            ty => {
                let found = self.ty(ty);
                self.error(
                    span,
                    format!("a {found} cannot be interpolated into a raw! command"),
                );
                None
            }
        }
    }

    /// A type as a diagnostic should spell it.
    fn ty(&self, ty: Type) -> String {
        self.types.name_of(ty)
    }

    fn resolve(&mut self, written: &ast::TypeName) -> Option<Type> {
        if written.borrow.is_some() {
            self.error(
                written.span,
                "a reference can only be a parameter: a binding would need a lifetime \
                 to say how long it stays valid",
            );
            return None;
        }
        // The scale may be a const parameter, which this instance already knows.
        if written.name == "fix" {
            let written = written.scale.as_ref().expect("the parser requires a scale");
            return self
                .scale(written)
                .map(|scale| Type::Fix(Scale::Const(scale)));
        }
        // Inside an instance, a type parameter is already a concrete type.
        if let Some(ty) = self.type_params.get(&written.name).copied() {
            if !written.args.is_empty() {
                let name = &written.name;
                self.error(
                    written.span,
                    format!("'{name}' is a type parameter; it does not take type arguments"),
                );
                return None;
            }
            return Some(ty);
        }
        resolve_type(written, self.types, self.errors)
    }

    /// `Point { x: 1, y: 2 }` and `State::Chasing { target: 3 }`.
    fn composite_lit(&mut self, lit: &ast::StructLit) -> Option<Expr> {
        if let Some(template) = self.types.template(&lit.name.name).cloned() {
            return self.generic_struct_lit(lit, &template);
        }
        let Some(ty) = self.types.get(&lit.name.name) else {
            let name = &lit.name.name;
            self.error(lit.name.span, format!("unknown type '{name}'"));
            return None;
        };
        match (ty, &lit.variant) {
            (Type::Struct(id), None) => {
                let def = self.types.struct_def(id);
                let fields = self.init_fields(&def.fields, lit, &def.name)?;
                Some(Expr {
                    kind: ExprKind::Struct { id, fields },
                    ty,
                    span: lit.span,
                })
            }
            (Type::Struct(id), Some(variant)) => {
                let (name, wanted) = (self.types.struct_def(id).name, &variant.name);
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

    /// `Pair { a: 1, b: 2 }` for a generic struct: the values decide the type
    /// arguments (spec section 4.12).
    fn generic_struct_lit(
        &mut self,
        lit: &ast::StructLit,
        template: &StructTemplate,
    ) -> Option<Expr> {
        if let Some(variant) = &lit.variant {
            let (name, wanted) = (&template.name, &variant.name);
            self.error(
                variant.span,
                format!("'{name}' is a struct, so it has no variant '{wanted}'"),
            );
            return None;
        }
        let mut written = Vec::new();
        for init in &lit.fields {
            let value = self.expr(&init.value)?;
            written.push((init.name.clone(), value));
        }
        let mut args: Vec<Option<Type>> = vec![None; template.generics.len()];
        for (name, value) in &written {
            if let Some(field) = template.fields.iter().find(|f| f.name == name.name) {
                self.types.unify(field.ty, value.ty, &mut args);
            }
        }
        let Some(args) = args.iter().copied().collect::<Option<Vec<Type>>>() else {
            let unknown: Vec<&str> = template
                .generics
                .iter()
                .zip(&args)
                .filter(|(_, bound)| bound.is_none())
                .map(|(param, _)| param.name.as_str())
                .collect();
            let (name, list) = (&template.name, unknown.join(", "));
            self.error(
                lit.span,
                format!(
                    "cannot tell what {list} is in '{name}' here; annotate the binding, \
                     as in 'let p: {name}<i32> = ..'"
                ),
            );
            return None;
        };
        let ty = self.types.instantiate(&template.name, &args)?;
        let Type::Struct(id) = ty else {
            unreachable!("a struct template instantiates to a struct")
        };
        let def = self.types.struct_def(id);
        let fields = self.check_fields(&def.fields, written, lit.span, &def.name)?;
        Some(Expr {
            kind: ExprKind::Struct { id, fields },
            ty,
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
        let mut written = Vec::new();
        for init in &lit.fields {
            // The field's type is known here, which is what an untyped `None` needs.
            let value = match declared.iter().find(|f| f.name == init.name.name) {
                Some(field) => self.expr_expecting(&init.value, field.ty)?,
                None => self.expr(&init.value)?,
            };
            written.push((init.name.clone(), value));
        }
        self.check_fields(declared, written, lit.span, what)
    }

    /// As `init_fields`, for values that have already been lowered.
    fn check_fields(
        &mut self,
        declared: &[Field],
        written: Vec<(ast::Ident, Expr)>,
        span: Span,
        what: &str,
    ) -> Option<Vec<Expr>> {
        let mut values: Vec<Option<Expr>> = vec![None; declared.len()];
        for (name, value) in written {
            let init = name;
            let Some(index) = declared.iter().position(|f| f.name == init.name) else {
                let name = &init.name;
                self.error(init.span, format!("'{what}' has no field named '{name}'"));
                return None;
            };
            if value.ty != declared[index].ty {
                let (want, found) = (self.ty(declared[index].ty), self.ty(value.ty));
                self.error(value.span, format!("expected {want}, found {found}"));
                return None;
            }
            if values[index].is_some() {
                let name = &init.name;
                self.error(init.span, format!("the field '{name}' is set twice"));
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
            self.error(span, format!("'{what}' is missing a value for {list}"));
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

    /// Lowers an expression in a place whose type is already known.
    ///
    /// Only `None` reads this: it is the one expression that says nothing about its
    /// own type (spec section 3.18). A nested expression sees the same expectation,
    /// which is harmless — a `None` in the wrong place fails the type check either
    /// way.
    fn expr_expecting(&mut self, expr: &AstExpr, want: Type) -> Option<Expr> {
        let outer = self.expected.replace(want);
        let value = self.expr(expr);
        self.expected = outer;
        value
    }

    fn lookup(&mut self, name: &ast::Ident) -> Option<LocalId> {
        if let Some(id) = self.lookup_quietly(name) {
            return Some(id);
        }
        let text = &name.name;
        self.errors.push(SyntaxError::new(
            name.span,
            format!("'{text}' is not defined"),
        ));
        None
    }

    /// The same lookup without reporting: `None` has to check whether a binding of
    /// that name shadows the keyword before complaining about it.
    fn lookup_quietly(&self, name: &ast::Ident) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name.name).copied())
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

    fn lower_err_with_toolchain(src: &str) -> Vec<SyntaxError> {
        with_toolchain(src).expect_err("expected an error")
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
        let body = &hir.functions[0].body;
        match body.iter().find_map(|stmt| match stmt {
            Stmt::Raw(raw) => Some(raw),
            _ => None,
        }) {
            Some(raw) => raw.as_text().expect("a command is literal").to_owned(),
            None => panic!("expected a command, found {body:?}"),
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

    /// A literal can follow an argument. Vanilla rejects the words in any other
    /// order, and it took a bigger example to notice.
    #[test]
    fn a_literal_after_an_argument_keeps_its_place() {
        assert_eq!(
            command_text("fn main() { playsound_master(minecraft:block.note_block.pling, @a); }"),
            "playsound minecraft:block.note_block.pling master @a"
        );
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
    fn a_relative_coordinate_can_be_negative() {
        assert!(
            command_text("fn main() { setblock(pos!(~ ~-1 ~), minecraft:stone); }")
                .contains("~ ~-1 ~")
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
        assert_eq!(raw.as_text(), Some("say hi"));
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
    fn an_interpolated_name_has_to_exist() {
        let errors = lower_err(r#"fn main() { raw!("say {nope}"); }"#);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("nope"), "{errors:?}");
    }

    #[test]
    fn an_unmatched_brace_says_to_write_it_twice() {
        for src in [
            r#"fn main() { raw!("summon zombie ~ ~ ~ {NoAI:1b"); }"#,
            r#"fn main() { raw!("say a } b"); }"#,
        ] {
            let errors = lower_err(src);
            assert_eq!(errors.len(), 1, "{src}");
            assert!(
                errors[0].message.contains("mean the character"),
                "{errors:?}"
            );
        }
    }

    #[test]
    fn interpolating_a_fixed_point_number_is_refused() {
        let errors = lower_err(r#"fn main() { let r = fix::<1000>(1500); raw!("say {r}"); }"#);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("scaled"), "{errors:?}");
    }

    #[test]
    fn interpolating_a_compound_is_refused() {
        let errors = lower_err(
            r#"struct Mob { hp: i32 } fn main() { let m = Mob { hp: 1 }; raw!("say {m}"); }"#,
        );
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("Mob"), "{errors:?}");
    }

    #[test]
    fn text_turns_a_binding_into_a_score_component() {
        let text = command_text(r#"fn main() { let hp = 3; tellraw(@a, text!("HP: ", hp)); }"#);
        assert!(
            text.contains(r#"{"score":{"name":"$main.hp","objective":"myns.v"}}"#),
            "{text}"
        );
        assert!(text.starts_with("tellraw @a "), "{text}");
    }

    #[test]
    fn text_joins_its_arguments_under_an_unstyled_head() {
        // The first element of a list is what the rest inherit style from, so it has
        // to be empty or ' HP: ' would come out red too.
        let text = command_text(
            r#"fn main() { let hp = 3; tellraw(@a, text!("Danger".red().bold(), " HP: ", hp)); }"#,
        );
        assert!(text.contains(r#""text":""#), "{text}");
        assert!(
            text.contains(r#"{"bold":true,"color":"red","text":"Danger"}"#),
            "{text}"
        );
    }

    #[test]
    fn one_argument_needs_no_wrapper() {
        let text = command_text(r#"fn main() { tellraw(@a, text!("hi")); }"#);
        assert_eq!(text, r#"tellraw @a {"text":"hi"}"#);
    }

    #[test]
    fn a_string_binding_becomes_an_nbt_component() {
        let text = command_text(r#"fn main() { let s = "pit"; tellraw(@a, text!(s)); }"#);
        assert_eq!(
            text,
            r#"tellraw @a {"nbt":"mw.vars.main.s","storage":"myns:mw"}"#
        );
    }

    #[test]
    fn a_hex_colour_is_written_through() {
        let text = command_text(r##"fn main() { tellraw(@a, text!("hi".color("#ff8800"))); }"##);
        assert!(text.contains(r##""color":"#ff8800""##), "{text}");
    }

    #[test]
    fn a_quote_in_a_text_literal_is_escaped() {
        let text = command_text(r#"fn main() { tellraw(@a, text!("a \"b\"")); }"#);
        assert!(text.contains(r#"\"b\""#), "{text}");
    }

    #[test]
    fn text_can_nest() {
        let text = command_text(r#"fn main() { tellraw(@a, text!(text!("a", "b").red())); }"#);
        assert!(text.contains(r#""color":"red""#), "{text}");
        assert!(text.contains(r#"{"text":"a"}"#), "{text}");
    }

    #[test]
    fn a_fixed_point_binding_cannot_be_shown() {
        let errors = lower_err_with_toolchain(
            r#"fn main() { let r = fix::<1000>(1500); tellraw(@a, text!(r)); }"#,
        );
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("scaled"), "{errors:?}");
    }

    #[test]
    fn a_place_has_to_be_bound_first() {
        let errors = lower_err_with_toolchain(
            r#"struct Mob { hp: i32 }
               fn main() { let m = Mob { hp: 1 }; tellraw(@a, text!(m.hp)); }"#,
        );
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("bind"), "{errors:?}");
    }

    #[test]
    fn a_component_cannot_be_bound_to_a_name() {
        let errors = lower_err_with_toolchain(r#"fn main() { let t = text!("hi"); }"#);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("TextComponent"), "{errors:?}");
    }

    #[test]
    fn a_test_function_takes_nothing_and_answers_nothing() {
        assert!(!lower_err("#[test] fn t(n: i32) {}").is_empty());
        assert!(!lower_err("#[test] fn t() -> i32 { return 1; }").is_empty());
        assert!(lower_err("#[test] fn t() {}").is_empty());
    }

    #[test]
    fn a_test_function_cannot_require_a_context() {
        // Nothing gives it an executor, so it would silently do nothing.
        let errors = lower_err("#[test] #[ctx(entity)] fn t() {}");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("#[test]"), "{errors:?}");
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
        fn nbt_optional_points_at_the_type() {
            // Saying it twice lets the two disagree: the type is where it belongs.
            let errors = lower_err("struct Mob { #[nbt(optional)] hp: i32 }");
            assert!(errors[0].message.contains("Option<T>"), "{errors:?}");
            lower_ok("struct Mob { hp: Option<i32> }");
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
        fn a_type_parameter_no_argument_mentions_is_reported() {
            let errors =
                lower_err("fn hold<T>(x: i32) -> i32 { return x; } fn main() { let a = hold(1); }");
            assert!(
                errors[0].message.contains("cannot tell what T"),
                "{errors:?}"
            );
        }

        #[test]
        fn a_generic_struct_checks_its_arity() {
            let errors = lower_err("struct Pair<T> { a: T, b: T } fn f(p: Pair) {}");
            assert!(errors[0].message.contains("type argument"), "{errors:?}");
        }

        #[test]
        fn a_template_is_not_a_type_on_its_own() {
            let errors = lower_err(
                "struct Pair<T> { a: T, b: T } \
                 fn main() { let p = Pair { a: 1, b: true }; }",
            );
            // `a` binds T to i32, so `b` has to be one too.
            assert!(errors[0].message.contains("expected i32"), "{errors:?}");
        }

        #[test]
        fn a_tag_cannot_be_written_on_a_parameter_field() {
            let errors = lower_err("struct Holder<T> { #[nbt(byte)] value: T }");
            assert!(errors[0].message.contains("type argument"), "{errors:?}");
        }

        #[test]
        fn an_argument_still_has_to_match_after_substitution() {
            let errors = lower_err(
                "fn pair<T>(a: T, b: T) -> i32 { return 1; } \
                 fn main() { let n = pair(1, true); }",
            );
            assert!(errors[0].message.contains("expected i32"), "{errors:?}");
        }

        #[test]
        fn a_place_passed_without_an_ampersand_is_reported() {
            let errors = lower_err(
                "struct P { x: i32 } fn f(p: &mut P) {} \
                 fn main() { let mut a = P { x: 1 }; f(a); }",
            );
            assert!(errors[0].message.contains("by reference"), "{errors:?}");
        }

        #[test]
        fn a_shared_borrow_where_mut_is_wanted_is_reported() {
            let errors = lower_err(
                "struct P { x: i32 } fn f(p: &mut P) {} \
                 fn main() { let mut a = P { x: 1 }; f(&a); }",
            );
            assert!(errors[0].message.contains("&mut"), "{errors:?}");
        }

        #[test]
        fn a_field_cannot_hold_a_reference() {
            let errors = lower_err("struct Inner { a: i32 } struct Outer { i: &Inner }");
            assert!(errors[0].message.contains("reference"), "{errors:?}");
        }

        #[test]
        fn a_method_on_a_type_without_one_is_reported() {
            let errors =
                lower_err("struct P { x: i32 } fn main() { let p = P { x: 1 }; p.bump(); }");
            assert!(errors[0].message.contains("no method named"), "{errors:?}");
        }

        #[test]
        fn a_struct_can_be_annotated_and_passed() {
            let hir = lower_ok(
                "struct Point { x: i32 } \
                 fn take(p: Point) {} \
                 fn main() { let p: Point = Point { x: 1 }; take(p); }",
            );
            assert_eq!(hir.types.struct_count(), 1);
            assert_eq!(hir.types.struct_def(StructId(0)).fields[0].name, "x");
        }
    }

    /// `fix<S>` and the const parameter that carries its scale (spec section 4.14).
    mod fixed_point {
        use super::*;

        fn ty_of_let(src: &str) -> Type {
            let hir = lower_ok(src);
            hir.functions[0].locals[0].ty
        }

        #[test]
        fn two_scales_do_not_mix() {
            let errors = lower_err(
                "fn main() { let a = fix::<100>(1); let b = fix::<1000>(1); let c = a + b; }",
            );
            assert!(
                errors[0]
                    .message
                    .contains("cannot mix fix<100> with fix<1000>"),
                "{errors:?}"
            );
            // The same source with one scale is fine, so the check above is the
            // thing that failed.
            lower_ok("fn main() { let a = fix::<100>(1); let b = fix::<100>(1); let c = a + b; }");
        }

        #[test]
        fn a_cast_from_an_integer_is_free() {
            let hir = lower_ok("fn main() { let a = fix::<1000>(1500); }");
            let Stmt::Let { value, .. } = &hir.functions[0].body[0] else {
                panic!("expected a let");
            };
            assert_eq!(value.ty, Type::Fix(Scale::Const(1000)));
            // Raw units: the integer is the value, so no arithmetic is left behind.
            assert_eq!(value.kind, ExprKind::Int(1500));
        }

        #[test]
        fn an_integer_is_not_a_fix() {
            let errors = lower_err("fn main() { let a: fix<1000> = 1; }");
            assert!(
                errors[0].message.contains("expected fix<1000>"),
                "{errors:?}"
            );
            let errors = lower_err("fn main() { let a = fix::<1000>(1) + 1; }");
            assert!(errors[0].message.contains("cannot mix"), "{errors:?}");
        }

        #[test]
        fn an_integer_multiplier_needs_no_conversion() {
            assert_eq!(
                ty_of_let("fn main() { let a = fix::<1000>(1500) * 2; }"),
                Type::Fix(Scale::Const(1000))
            );
            assert_eq!(
                ty_of_let("fn main() { let a = fix::<1000>(1500) / 2; }"),
                Type::Fix(Scale::Const(1000))
            );
            // Dividing by a fix is not the same thing, and has no correction that
            // keeps the units right.
            let errors = lower_err("fn main() { let a = 2 / fix::<1000>(1500); }");
            assert!(errors[0].message.contains("cannot mix"), "{errors:?}");
        }

        #[test]
        fn a_scale_is_one_or_more() {
            let errors = lower_err("fn main() { let a = fix::<0>(1); }");
            assert!(errors[0].message.contains("1 or more"), "{errors:?}");
        }

        #[test]
        fn a_cast_between_scales_is_allowed() {
            assert_eq!(
                ty_of_let("fn main() { let a = fix::<100>(fix::<1000>(1500)); }"),
                Type::Fix(Scale::Const(100))
            );
        }

        #[test]
        fn a_const_parameter_takes_the_scale_of_the_argument() {
            let hir = lower_ok(
                "fn half<const S: i32>(x: fix<S>) -> fix<S> { return x / 2; } \
                 fn main() { let a = half(fix::<1000>(1500)); }",
            );
            let names: Vec<&str> = hir.functions.iter().map(|f| f.name.as_str()).collect();
            assert!(names.contains(&"half_fix_1000"), "{names:?}");
            assert_eq!(hir.functions[0].locals[0].ty, Type::Fix(Scale::Const(1000)));
        }

        #[test]
        fn one_instance_per_scale() {
            let hir = lower_ok(
                "fn id<const S: i32>(x: fix<S>) -> fix<S> { return x; } \
                 fn main() { let a = id(fix::<100>(1)); let b = id(fix::<1000>(1)); \
                             let c = id(fix::<100>(2)); }",
            );
            let instances = hir
                .functions
                .iter()
                .filter(|f| f.name.starts_with("id_"))
                .count();
            assert_eq!(instances, 2);
        }

        #[test]
        fn a_const_parameter_is_not_a_type() {
            let errors = lower_err("fn f<const S: i32>(x: S) {}");
            assert!(errors[0].message.contains("const parameter"), "{errors:?}");
        }

        #[test]
        fn a_type_parameter_is_not_a_scale() {
            let errors = lower_err("fn f<T>(x: fix<T>) {}");
            assert!(errors[0].message.contains("const T: i32"), "{errors:?}");
        }
    }

    /// The types that exist to match an NBT tag exactly (spec section 4.15).
    mod nbt_scalars {
        use super::*;

        #[test]
        fn an_nbt_scalar_has_no_arithmetic() {
            let errors = lower_err("fn f(a: f64, b: f64) { let c = a + b; }");
            assert!(errors[0].message.contains("fix::<1000>(x)"), "{errors:?}");
            let errors = lower_err("fn f(a: i64, b: i64) { let c = a < b; }");
            assert!(errors[0].message.contains("no arithmetic"), "{errors:?}");
        }

        #[test]
        fn an_nbt_scalar_takes_its_tag_from_its_type() {
            let hir = lower_ok("struct Mob { hp: f32, age: i64, flag: i8 }");
            let def = hir.types.struct_def(StructId(0));
            assert_eq!(def.fields[0].tag, Some(NbtTag::Float));
            assert_eq!(def.fields[1].tag, Some(NbtTag::Long));
            assert_eq!(def.fields[2].tag, Some(NbtTag::Byte));
        }

        #[test]
        fn an_nbt_scalar_cannot_be_given_a_tag() {
            let errors = lower_err("struct Mob { #[nbt(byte)] hp: f64 }");
            assert!(errors[0].message.contains("already a tag"), "{errors:?}");
            // An i32 still chooses, which is why both spellings exist.
            lower_ok("struct Mob { #[nbt(byte)] hp: i32 }");
        }

        #[test]
        fn a_float_tag_can_be_asked_for_by_name() {
            let hir = lower_ok("struct Mob { #[nbt(double)] hp: i32 }");
            let def = hir.types.struct_def(StructId(0));
            assert_eq!(def.fields[0].tag, Some(NbtTag::Double));
        }

        #[test]
        fn a_fix_cannot_become_an_integer_tag() {
            let errors = lower_err(
                "struct Mob { age: i64 } \
                 fn main() { let a = fix::<1000>(1500); let m = Mob { age: a.as_i64() }; }",
            );
            assert!(errors[0].message.contains("no conversion"), "{errors:?}");
        }

        #[test]
        fn a_conversion_takes_no_arguments() {
            let errors = lower_err("fn main() { let a = 1; let d = a.as_f64(1); }");
            assert!(errors[0].message.contains("no arguments"), "{errors:?}");
        }

        #[test]
        fn an_nbt_scalar_cannot_be_assigned_a_score() {
            let errors = lower_err("struct Mob { pos: f64 } fn main() { let m = Mob { pos: 1 }; }");
            assert!(errors[0].message.contains("expected f64"), "{errors:?}");
        }
    }

    /// `nbt!` checked against the type it lands in (spec section 4.18).
    mod nbt_literals {
        use super::*;

        #[test]
        fn a_field_the_type_does_not_have_is_reported() {
            let errors = lower_err(
                "struct Mob { hp: i32 } \
                 fn main() { let m: Mob = nbt!({ hp: 20, rage: 3 }); }",
            );
            assert!(
                errors[0].message.contains("no field written as 'rage'"),
                "{errors:?}"
            );
            // The same literal without the extra field compiles, so the check above
            // is what failed.
            lower_ok("struct Mob { hp: i32 } fn main() { let m: Mob = nbt!({ hp: 20 }); }");
        }

        #[test]
        fn a_missing_field_is_reported() {
            let errors = lower_err(
                "struct Mob { hp: i32, name: String } \
                 fn main() { let m: Mob = nbt!({ hp: 20 }); }",
            );
            assert!(errors[0].message.contains("'name'"), "{errors:?}");
        }

        #[test]
        fn a_value_of_the_wrong_shape_is_reported() {
            let errors =
                lower_err("struct Mob { hp: i32 } fn main() { let m: Mob = nbt!({ hp: \"a\" }); }");
            assert!(errors[0].message.contains("expected i32"), "{errors:?}");
            let errors =
                lower_err("struct Mob { hp: i32 } fn main() { let m: Mob = nbt!({ hp: [1] }); }");
            assert!(errors[0].message.contains("expected i32"), "{errors:?}");
        }

        #[test]
        fn a_suffix_is_not_written() {
            let errors = lower_err(
                "struct Mob { weight: f64 } fn main() { let m: Mob = nbt!({ weight: 2 d }); }",
            );
            assert!(errors[0].message.contains("no suffix"), "{errors:?}");
        }

        #[test]
        fn nbt_without_a_type_to_check_against_is_reported() {
            let errors =
                lower_err("struct Mob { hp: i32 } fn main() { let m = nbt!({ hp: 20 }); }");
            assert!(
                errors[0].message.contains("annotate the binding"),
                "{errors:?}"
            );
        }

        #[test]
        fn a_field_written_twice_is_reported() {
            let errors = lower_err(
                "struct Mob { hp: i32 } fn main() { let m: Mob = nbt!({ hp: 1, hp: 2 }); }",
            );
            assert!(errors[0].message.contains("twice"), "{errors:?}");
        }
    }

    /// `Option<T>`: the value, or the path not being there (spec section 4.19).
    mod options {
        use super::*;

        #[test]
        fn none_on_its_own_says_what_is_missing() {
            let errors = lower_err("fn main() { let a = None; }");
            assert!(
                errors[0].message.contains("does not say what it is"),
                "{errors:?}"
            );
            lower_ok("fn main() { let a: Option<i32> = None; }");
        }

        #[test]
        fn an_option_of_an_option_cannot_be_told_apart() {
            let errors = lower_err("fn f(x: Option<Option<i32>>) {}");
            assert!(
                errors[0].message.contains("either there or not"),
                "{errors:?}"
            );
            let errors = lower_err("fn main() { let a: Option<i32> = Some(1); let b = Some(a); }");
            assert!(
                errors[0].message.contains("cannot be told apart"),
                "{errors:?}"
            );
        }

        #[test]
        fn an_option_holds_a_runtime_value() {
            let errors = lower_err("fn main() { let a = Some(@s); }");
            assert!(errors[0].message.contains("no runtime value"), "{errors:?}");
        }

        #[test]
        fn option_takes_one_type_argument() {
            let errors = lower_err("fn f(x: Option) {}");
            assert!(
                errors[0].message.contains("one type argument"),
                "{errors:?}"
            );
        }

        #[test]
        fn the_type_has_to_match_what_it_is_written_into() {
            let errors = lower_err("fn main() { let a: i32 = None; }");
            assert!(errors[0].message.contains("expected i32"), "{errors:?}");
            let errors =
                lower_err("struct Mob { hp: Option<i32> } fn main() { let m = Mob { hp: 1 }; }");
            assert!(
                errors[0].message.contains("expected Option<i32>"),
                "{errors:?}"
            );
        }

        #[test]
        fn a_match_on_an_option_covers_both_sides() {
            let errors =
                lower_err("fn main() { let a: Option<i32> = Some(1); match a { Some(v) => {} } }");
            assert!(
                errors[0].message.contains("does not cover None"),
                "{errors:?}"
            );
            let errors =
                lower_err("fn main() { let a: Option<i32> = Some(1); match a { None => {} } }");
            assert!(
                errors[0].message.contains("does not cover Some(x)"),
                "{errors:?}"
            );
            lower_ok(
                "fn main() { let a: Option<i32> = Some(1); match a { Some(v) => {} None => {} } }",
            );
        }

        #[test]
        fn an_option_arm_cannot_name_a_variant() {
            let errors = lower_err(
                "enum State { Idle } \
                 fn main() { let a: Option<i32> = Some(1); match a { State::Idle => {} } }",
            );
            assert!(
                errors[0].message.contains("'Some(x)' and 'None'"),
                "{errors:?}"
            );
        }

        #[test]
        fn an_enum_arm_cannot_be_some_or_none() {
            let errors = lower_err(
                "enum State { Idle } \
                 fn main() { let s = State::Idle; match s { Some(v) => {} None => {} } }",
            );
            assert!(
                errors[0].message.contains("name its variants"),
                "{errors:?}"
            );
        }

        #[test]
        fn an_arm_after_both_sides_cannot_be_reached() {
            let errors = lower_err(
                "fn main() { let a: Option<i32> = Some(1); \
                 match a { Some(v) => {} None => {} _ => {} } }",
            );
            assert!(
                errors[0].message.contains("cannot be reached"),
                "{errors:?}"
            );
        }

        #[test]
        fn a_question_mark_needs_a_function_that_can_answer_with_nothing() {
            let errors = lower_err("fn f(a: Option<i32>) -> i32 { let v = a?; return v; }");
            assert!(errors[0].message.contains("return an Option"), "{errors:?}");
            lower_ok("fn f(a: Option<i32>) -> Option<i32> { let v = a?; return Some(v); }");
        }

        #[test]
        fn a_question_mark_only_takes_an_option() {
            let errors = lower_err("fn f(a: i32) -> Option<i32> { let v = a?; return Some(v); }");
            assert!(
                errors[0].message.contains("'?' takes an Option"),
                "{errors:?}"
            );
        }

        #[test]
        fn a_question_mark_unwraps_into_a_register() {
            let errors = lower_err(
                "struct P { x: i32 } \
                 fn f(a: Option<P>) -> Option<i32> { let v = a?; return Some(1); }",
            );
            assert!(
                errors[0].message.contains("does not fit in one"),
                "{errors:?}"
            );
        }

        #[test]
        fn an_option_of_a_compound_cannot_be_returned() {
            let errors = lower_err("struct P { x: i32 } fn f() -> Option<P> { return None; }");
            assert!(
                errors[0]
                    .message
                    .contains("cannot come back from a function"),
                "{errors:?}"
            );
        }

        #[test]
        fn a_binding_can_shadow_the_word_none() {
            // `None` is not a keyword: a binding of that name wins, as any other does.
            let hir = lower_ok("fn main() { let None = 1; let x = None; }");
            assert_eq!(hir.functions[0].locals[1].ty, Type::I32);
        }
    }

    /// Views of entity NBT (spec section 4.20).
    mod views {
        use super::*;

        #[test]
        fn a_selector_that_may_find_several_is_reported() {
            let errors = lower_err(
                "#[entity] struct Mob { hp: Option<i32> } \
                 fn main() { let m = Mob::of(@e[type=zombie]); }",
            );
            assert!(
                errors[0].message.contains("more than one entity"),
                "{errors:?}"
            );
            // The same source with a limit is fine, so the check above is what failed.
            lower_ok(
                "#[entity] struct Mob { hp: Option<i32> } \
                 fn main() { let m = Mob::of(@e[type=zombie, limit=1]); }",
            );
            lower_ok("#[entity] struct Mob { hp: Option<i32> } fn main() { let m = Mob::of(@p); }");
        }

        #[test]
        fn a_view_of_the_executor_needs_a_context() {
            let errors = lower_err(
                "#[entity] struct Mob { hp: Option<i32> } fn main() { let m = Mob::of(@s); }",
            );
            assert!(errors[0].message.contains("entity context"), "{errors:?}");
            lower_ok(
                "#[entity] struct Mob { hp: Option<i32> } \
                 #[ctx(entity)] fn f() { let m = Mob::of(@s); }",
            );
        }

        #[test]
        fn only_a_view_has_of() {
            let errors =
                lower_err("struct Mob { hp: Option<i32> } fn main() { let m = Mob::of(@p); }");
            assert!(errors[0].message.contains("#[entity]"), "{errors:?}");
        }

        #[test]
        fn a_view_is_not_a_value() {
            // It has no runtime representation, so nothing can hold one.
            let errors = lower_err(
                "#[entity] struct Mob { hp: Option<i32> } \
                 struct Holder { m: Mob }",
            );
            assert!(!errors.is_empty(), "{errors:?}");
            let errors = lower_err("#[entity] struct Mob { hp: Option<i32> } fn take(m: Mob) {}");
            assert!(!errors.is_empty(), "{errors:?}");
        }

        #[test]
        fn a_view_field_is_read_and_written_like_any_other() {
            let hir = lower_ok(
                "#[entity] struct Mob { #[nbt(float, rename = \"Health\")] hp: Option<fix<1000>> } \
                 #[ctx(entity)] fn f() { let mut m = Mob::of(@s); m.hp = Some(fix::<1000>(1500)); }",
            );
            // The view binding is a name, not a statement: only the write is left.
            assert_eq!(hir.functions[0].body.len(), 1);
        }
    }
}
