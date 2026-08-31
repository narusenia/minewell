// SPDX-License-Identifier: MIT

//! The mid-level intermediate representation: scoreboard registers and basic blocks.
//!
//! MIR is close enough to mcfunction that emitting is mechanical, and abstract enough
//! that register allocation and the inline-or-split decision
//! (`docs/01-requirements.md` section 7) belong here rather than in the emitter.
//!
//! Every instruction corresponds to exactly one command. That is what lets the
//! compiler report what a function costs before it has emitted anything, and it is why
//! choosing `players set` over `operation =` for a constant lives here: it is not an
//! optimisation, it is picking the command the game provides for the job.
//!
//! Today every function is one block. Control flow arrives in M3.

use std::collections::HashMap;

use crate::hir::{self, FnId, Hir, LocalId, NbtTag, Root, Step, TAG_KEY, Type, Types};
use crate::syntax::ast::{BinaryOp, UnaryOp};
use crate::syntax::lexer::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub u32);

/// A scoreboard holder. Which objective it lives in is decided at emit time, where the
/// namespace is known.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Reg {
    pub holder: String,
    pub kind: RegKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegKind {
    /// A binding the author wrote.
    Var,
    /// A compiler temporary.
    Temp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mir {
    pub functions: Vec<Function>,
    /// The pack's namespace. Kept here rather than read back off a function path,
    /// because dead code elimination can leave no functions to read it from.
    pub namespace: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub id: FnId,
    /// The datapack id this is written to.
    pub path: String,
    pub attrs: Vec<hir::Attr>,
    pub blocks: Vec<Block>,
}

impl Function {
    pub fn entry(&self) -> &Block {
        &self.blocks[0]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub id: BlockId,
    pub insts: Vec<Inst>,
}

/// One instruction, one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inst {
    /// A command to emit as written.
    Raw { text: String, span: Span },
    /// `scoreboard players set <dst> <value>`
    Const { dst: Reg, value: i32 },
    /// `scoreboard players add|remove <dst> <value>`
    AddConst { dst: Reg, value: i32 },
    /// `scoreboard players operation <dst> <op> <src>`
    Op { dst: Reg, op: Op, src: Reg },
    /// `execute store success score <dst> if|unless score <lhs> <cmp> <rhs>`
    Cmp {
        dst: Reg,
        cmp: Cmp,
        negated: bool,
        lhs: Reg,
        rhs: Reg,
    },
    /// `execute store success score <dst> if|unless score <src> matches <min>..<max>`
    Matches {
        dst: Reg,
        src: Reg,
        min: Option<i32>,
        max: Option<i32>,
        negated: bool,
    },
    /// `function <path>`
    Call { path: String },
    /// `execute store result score <dst> run <inst>`
    StoreResult { dst: Reg, inst: Box<Inst> },
    /// `scoreboard players get <src>`, whose result is the score.
    Get { src: Reg },
    /// `return run <inst>`
    ReturnRun { inst: Box<Inst> },
    /// `data modify storage <ns>:mw mw.stack append value {}`
    PushFrame,
    /// `data remove storage <ns>:mw mw.stack[-1]`
    PopFrame,
    /// Saves a register into the top frame under `slot`.
    Save { reg: Reg, slot: u32 },
    /// Reads a register back out of the top frame.
    Restore { reg: Reg, slot: u32 },
    /// Saves a value in storage into the top frame under `slot`.
    SaveData { path: String, slot: u32 },
    /// Reads a value in storage back out of the top frame.
    RestoreData { path: String, slot: u32 },
    /// `return <value>`
    Return { value: i32 },
    /// `data modify storage <ns>:mw <path> set value <snbt>`
    SetValue { path: DataRef, value: String },
    /// `data modify storage <ns>:mw <dst> set from storage <ns>:mw <src>`
    CopyData { dst: DataRef, src: DataRef },
    /// `data get <target> <path>`, whose result is the value there.
    GetData { path: DataRef },
    /// `data get <target> <path> <scale>`: the same read, in `scale`ths of a unit,
    /// which is how a `fix<S>` comes out of storage (spec section 6.26).
    GetScaled { path: DataRef, scale: u32 },
    /// `data remove storage <ns>:mw <path>`
    RemoveData { path: DataRef },
    /// `data modify storage <ns>:mw <path> append value <snbt>`
    AppendValue { path: DataRef, value: String },
    /// `data modify storage <ns>:mw <dst> append from storage <ns>:mw <src>`
    AppendFrom { dst: DataRef, src: DataRef },
    /// `function <path> with storage <ns>:mw mw.args`
    CallWithArgs { path: String },
    /// A macro line: the same command with `$(i)` spliced into a path, which vanilla
    /// substitutes per call. Only ever the whole body of a generated helper, so macro
    /// promotion does not spread to the caller (requirements section 10.1).
    Macro { inst: Box<Inst> },
    /// `execute store result storage <ns>:mw <path> <tag> 1 run <inst>`
    StoreData {
        path: DataRef,
        tag: &'static str,
        inst: Box<Inst>,
    },
    /// The same store, dividing by `scale` on the way in: the register holds
    /// `scale`ths of a unit and storage holds the unit (spec section 6.26).
    StoreScaled {
        path: DataRef,
        tag: &'static str,
        scale: u32,
        inst: Box<Inst>,
    },
    /// `tellraw @a` naming where it came from: what a check says when it fails
    /// (spec section 6.30). Only debug builds ever hold one.
    Report { message: String, span: Span },
    /// `return fail`: what returning `None` is (spec section 6.28).
    ReturnFail,
    /// `execute store success score <ok> store result score <dst> run <inst>`: both
    /// halves of a call's outcome, which is what an `Option<T>` is made of.
    StoreBoth { ok: Reg, dst: Reg, inst: Box<Inst> },
    /// `execute store success score <dst> <cond>`: the condition's answer as 0 or 1.
    StoreCond { dst: Reg, cond: Cond },
    /// `data modify storage <ns>:mw <dst> set string storage <ns>:mw <src> [a] [b]`
    SetString {
        dst: DataRef,
        src: DataRef,
        start: Option<i32>,
        end: Option<i32>,
    },
    /// `execute <cond> run <inst>`. Still one command, so still one instruction.
    Guarded { cond: Cond, inst: Box<Inst> },
    /// A `match`'s `_` arm: run when none of `tags` is the one in `path`. Several
    /// `unless` clauses, but still one command.
    Otherwise {
        path: DataRef,
        tags: Vec<String>,
        inst: Box<Inst>,
    },
    /// `execute as|at <selector> run <inst>`.
    Context { clause: ExecuteAs, inst: Box<Inst> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecuteAs {
    As(String),
    At(String),
}

/// A test that can be written straight into an `execute`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cond {
    /// `if|unless score <lhs> <cmp> <rhs>`
    Score {
        lhs: Reg,
        cmp: Cmp,
        rhs: Reg,
        negated: bool,
    },
    /// `if|unless score <src> matches <min>..<max>`
    Matches {
        src: Reg,
        min: Option<i32>,
        max: Option<i32>,
        negated: bool,
    },
    /// `if|unless data storage <ns>:mw <path><filter>`
    Data {
        path: DataRef,
        /// An SNBT compound, appended to the path: `{tag:"Idle"}`.
        filter: String,
        negated: bool,
    },
}

impl Cond {
    pub fn negate(self) -> Cond {
        match self {
            Cond::Score {
                lhs,
                cmp,
                rhs,
                negated,
            } => Cond::Score {
                lhs,
                cmp,
                rhs,
                negated: !negated,
            },
            Cond::Matches {
                src,
                min,
                max,
                negated,
            } => Cond::Matches {
                src,
                min,
                max,
                negated: !negated,
            },
            Cond::Data {
                path,
                filter,
                negated,
            } => Cond::Data {
                path,
                filter,
                negated: !negated,
            },
        }
    }
}

/// What can leave a block other than reaching its end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Escapes {
    breaks: bool,
    continues: bool,
    returns: bool,
}

impl Escapes {
    fn any(&self) -> bool {
        self.breaks || self.continues || self.returns
    }

    fn union(self, other: Escapes) -> Escapes {
        Escapes {
            breaks: self.breaks || other.breaks,
            continues: self.continues || other.continues,
            returns: self.returns || other.returns,
        }
    }
}

/// What can escape this statement list. A loop swallows the `break` and `continue` of
/// its own body, so only a `return` gets past it.
fn escapes(stmts: &[hir::Stmt]) -> Escapes {
    stmts.iter().fold(Escapes::default(), |acc, stmt| {
        // A `?` leaves the function when it finds nothing, so it escapes the same way
        // a `return` does (spec section 6.28).
        let acc = acc.union(Escapes {
            returns: tries(stmt),
            ..Escapes::default()
        });
        acc.union(match stmt {
            hir::Stmt::Break(_) => Escapes {
                breaks: true,
                ..Escapes::default()
            },
            hir::Stmt::Continue(_) => Escapes {
                continues: true,
                ..Escapes::default()
            },
            hir::Stmt::Return { .. } => Escapes {
                returns: true,
                ..Escapes::default()
            },
            hir::Stmt::If {
                then, otherwise, ..
            } => escapes(then).union(otherwise.as_deref().map(escapes).unwrap_or_default()),
            hir::Stmt::Loop { body, .. } => Escapes {
                returns: escapes(body).returns,
                ..Escapes::default()
            },
            hir::Stmt::Match { arms, .. } => arms
                .iter()
                .fold(Escapes::default(), |acc, arm| acc.union(escapes(&arm.body))),
            // A list loop swallows its own `break` and `continue`, as `while` does.
            hir::Stmt::ForVec { body, .. } => Escapes {
                returns: escapes(body).returns,
                ..Escapes::default()
            },
            // A context block consumes its own `continue`: returning from the body is
            // what going to the next entity means. `break` and `return` get out.
            hir::Stmt::Context { body, .. } => Escapes {
                continues: false,
                ..escapes(body)
            },
            _ => Escapes::default(),
        })
    })
}

/// The scale a place is read and written with.
///
/// A `fix<S>` under a float or double tag holds the **real number**, so the round trip
/// multiplies and divides by `S`. Under an integer tag it holds the raw units, which
/// is how the compiler stores its own values (spec section 4.16).
fn scale_of(place: &hir::Place) -> u32 {
    scale_from(place.ty, place.tag)
}

/// As `scale_of`, for a value whose place has already been taken apart.
fn scale_from(ty: Type, tag: Option<NbtTag>) -> u32 {
    match (ty, tag) {
        (Type::Fix(hir::Scale::Const(scale)), Some(NbtTag::Float | NbtTag::Double)) => scale,
        _ => 1,
    }
}

/// The storage path an option is read from, for the ones that are somewhere rather
/// than in a register.
fn option_path(function: &hir::Function, expr: &hir::Expr) -> Option<DataRef> {
    match &expr.kind {
        hir::ExprKind::Local(local) => Some(local_path(function, *local).into()),
        hir::ExprKind::Field(place) => Some(place_path(function, place)),
        _ => None,
    }
}

/// Whether a statement's own expressions hold a `?`. Nested blocks are walked by
/// `escapes` itself.
fn tries(stmt: &hir::Stmt) -> bool {
    let exprs: Vec<&hir::Expr> = match stmt {
        hir::Stmt::If { cond, .. } => vec![cond],
        hir::Stmt::Loop {
            cond: Some(cond), ..
        } => vec![cond],
        hir::Stmt::CallFor { args, .. } => args.iter().collect(),
        hir::Stmt::Return {
            value: Some(value), ..
        }
        | hir::Stmt::Let { value, .. }
        | hir::Stmt::Push { value, .. }
        | hir::Stmt::Assign { value, .. } => vec![value],
        _ => Vec::new(),
    };
    exprs.into_iter().any(has_try)
}

fn has_try(expr: &hir::Expr) -> bool {
    match &expr.kind {
        hir::ExprKind::Try(_) => true,
        hir::ExprKind::Unary(_, operand) => has_try(operand),
        hir::ExprKind::Binary(_, lhs, rhs) => has_try(lhs) || has_try(rhs),
        hir::ExprKind::Some(inner) | hir::ExprKind::AsNbt { value: inner, .. } => has_try(inner),
        hir::ExprKind::Call { args, .. } => args.iter().any(has_try),
        hir::ExprKind::Struct { fields, .. } | hir::ExprKind::Enum { fields, .. } => {
            fields.iter().any(has_try)
        }
        hir::ExprKind::List { values, .. } => values.iter().any(has_try),
        _ => false,
    }
}

/// Where a data command points (spec section 6.29).
///
/// Storage is the common case — every local, temporary and argument lives there — so
/// `DataRef::from` makes one. An entity target names the selector it reads through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRef {
    pub target: DataTarget,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataTarget {
    Storage,
    /// `entity <selector>`. The selector is compile-time text.
    Entity(String),
}

impl DataRef {
    /// The same target, somewhere else in it.
    fn with_path(&self, path: String) -> DataRef {
        DataRef {
            target: self.target.clone(),
            path,
        }
    }
}

impl From<String> for DataRef {
    fn from(path: String) -> DataRef {
        DataRef {
            target: DataTarget::Storage,
            path,
        }
    }
}

impl From<&str> for DataRef {
    fn from(path: &str) -> DataRef {
        DataRef::from(path.to_owned())
    }
}

/// The control register's values. See spec section 6.10.
const CTL_NORMAL: i32 = 0;
const CTL_BREAK: i32 = 1;
const CTL_CONTINUE: i32 = 2;
const CTL_RETURN: i32 = 3;
/// A `return None`, or a `?` that found nothing: a return that reports failure.
const CTL_FAIL: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    /// `<`, which for 0/1 values is logical and.
    Min,
    /// `>`, which for 0/1 values is logical or.
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

/// A place a frame has to be saved from and restored to.
///
/// Two, because a function's state is in two places: `i32` and `bool` are registers,
/// composites are storage paths (spec section 5). Saving one as the other reads a
/// register that was never written.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    Score(Reg),
    Data(String),
}

/// Where a value to be written into storage came from.
/// What a call's outcome is captured into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    Nothing,
    Result,
    /// Both halves: an option-returning function puts the value in one and whether
    /// there was a value in the other.
    Option,
}

/// What a call left behind.
struct Called {
    value: Reg,
    present: Option<Reg>,
}

/// How a string comparison is asked: a condition that can be used where it stands, or
/// one that has to run inside a macro function first (spec section 6.27).
enum Match {
    Here(Cond),
    Spliced { cond: Cond, src: String },
}

enum Written {
    /// Already SNBT: `set value` takes it as it is.
    Const(String),
    Reg(Reg),
    /// A path to copy from, for a composite.
    Data(String),
}

/// What an expression produced: either a number known now, or a register holding it.
///
/// Keeping constants unmaterialised until the last moment is what lets `let x = 5;`
/// be one `players set` instead of a `set` into a temporary and an `operation =`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Const(i32),
    Reg(Reg),
}

/// `debug` says whether the checks that exist only to catch mistakes are lowered at
/// all: `debug_assert!` and `expect` (requirements section 15). A release build drops
/// the statement rather than the command, so nothing is spent on evaluating it either.
pub fn lower(hir: &Hir, debug: bool) -> Mir {
    let components = strongly_connected(hir);
    let mut program = Program {
        debug,
        functions: &hir.functions,
        types: &hir.types,
        components,
        temps: Temps::default(),
        used: Vec::new(),
        used_data: Vec::new(),
        used_ctl: false,
        initialised: Vec::new(),
    };
    let mut functions = Vec::new();
    for f in &hir.functions {
        program.used.clear();
        program.used_data.clear();
        program.used_ctl = false;
        // Parameters arrive with values already in them, written by the caller.
        program.initialised.clear();
        program.initialised.extend(f.params.iter().copied());
        let mut cx = Lowering {
            function: f,
            program: &mut program,
            insts: Vec::new(),
            generated: Vec::new(),
            prefix: f.path.clone(),
            counter: 0,
            top_level: true,
            in_entity_body: false,
            entity_body_root: false,
        };
        for stmt in &f.body {
            cx.stmt(stmt);
        }
        let (mut insts, generated) = (cx.insts, cx.generated);
        // The register outlives the call: a `return` that reached the top left it
        // raised, and the next invocation would read that as its own. Clear it on the
        // way in, where the value can no longer mean anything.
        if program.used_ctl {
            insts.insert(
                0,
                Inst::Const {
                    dst: Reg {
                        holder: format!("${}.ctl", f.name),
                        kind: RegKind::Var,
                    },
                    value: CTL_NORMAL,
                },
            );
        }
        functions.push(Function {
            id: f.id,
            path: f.path.clone(),
            attrs: f.attrs.clone(),
            blocks: vec![Block {
                id: BlockId(0),
                insts,
            }],
        });
        functions.extend(generated);
    }
    let namespace = hir
        .functions
        .first()
        .and_then(|f| f.path.split_once(':'))
        .map_or_else(|| "minecraft".to_owned(), |(ns, _)| ns.to_owned());
    let mut mir = Mir {
        functions,
        namespace,
    };
    if !debug {
        prune(&mut mir);
    }
    mir
}

/// Drops the functions nothing can reach (requirements section 15).
///
/// A function tag is the only way vanilla enters a pack, so the tagged functions are
/// the roots. `raw!` is text this compiler does not read, so a function whose id
/// appears in one counts as reached as well: keeping a function too many wastes a
/// file, dropping one that is called fails silently in game.
fn prune(mir: &mut Mir) {
    let mut keep = vec![false; mir.functions.len()];
    {
        let paths: Vec<&str> = mir.functions.iter().map(|f| f.path.as_str()).collect();
        let index: HashMap<&str, usize> = paths.iter().copied().zip(0..).collect();
        let mut queue = Vec::new();
        for (i, f) in mir.functions.iter().enumerate() {
            if f.attrs
                .iter()
                .any(|attr| matches!(attr, hir::Attr::Tick | hir::Attr::Load))
            {
                keep[i] = true;
                queue.push(i);
            }
        }
        let mut reached = Vec::new();
        while let Some(i) = queue.pop() {
            reached.clear();
            for block in &mir.functions[i].blocks {
                for inst in &block.insts {
                    reaches(inst, &paths, &mut reached);
                }
            }
            for path in &reached {
                let Some(&j) = index.get(*path) else { continue };
                if !keep[j] {
                    keep[j] = true;
                    queue.push(j);
                }
            }
        }
    }
    let mut alive = keep.into_iter();
    mir.functions.retain(|_| alive.next().unwrap_or(true));
}

/// The functions one instruction can hand control to.
fn reaches<'a>(inst: &'a Inst, paths: &[&'a str], out: &mut Vec<&'a str>) {
    match inst {
        Inst::Call { path } | Inst::CallWithArgs { path } => out.push(path.as_str()),
        // Not parsed, so anything that looks like a function id in it is one.
        Inst::Raw { text, .. } => out.extend(paths.iter().filter(|path| text.contains(**path))),
        Inst::StoreResult { inst, .. }
        | Inst::ReturnRun { inst }
        | Inst::Macro { inst }
        | Inst::StoreData { inst, .. }
        | Inst::StoreScaled { inst, .. }
        | Inst::StoreBoth { inst, .. }
        | Inst::Guarded { inst, .. }
        | Inst::Otherwise { inst, .. }
        | Inst::Context { inst, .. } => reaches(inst, paths, out),
        _ => {}
    }
}

/// Temporary names, counted across the whole program.
///
/// A name is therefore never reused, so no two temporaries can be live at once under
/// the same name and correctness needs no liveness analysis. Shrinking this is M9-7's
/// job; until then the naive version is the one that is obviously right.
#[derive(Debug, Default)]
struct Temps {
    scores: u32,
    data: u32,
    iters: u32,
}

impl Temps {
    fn next(&mut self) -> Reg {
        let reg = Reg {
            holder: format!("$t{}", self.scores),
            kind: RegKind::Temp,
        };
        self.scores += 1;
        reg
    }

    /// A temporary in storage, for a composite that has no register to sit in.
    fn next_data(&mut self) -> String {
        let path = format!("mw.tmp.m{}", self.data);
        self.data += 1;
        path
    }

    /// The copy a `for` walks through, under the root reserved for it
    /// (requirements section 3.3).
    fn next_iter(&mut self) -> String {
        let path = format!("mw.iter.i{}", self.iters);
        self.iters += 1;
        path
    }
}

/// State shared by every block of one program.
struct Program<'a> {
    /// Whether the build carries its checks (spec section 6.30).
    debug: bool,
    functions: &'a [hir::Function],
    types: &'a Types,
    /// Which strongly connected component each function belongs to. Two functions in
    /// the same one can reach each other, so a call between them is recursive.
    components: Vec<u32>,
    temps: Temps,
    /// Temporaries handed out in the function being lowered, in order.
    used: Vec<Reg>,
    /// The same, for temporaries that live in storage.
    used_data: Vec<String>,
    /// Whether the function being lowered touched the control register at all.
    used_ctl: bool,
    /// Locals that have been given a value at this point in the lowering. A local
    /// whose `let` has not run yet holds nothing, and reading it to save it would fail
    /// the command — so it is not saved.
    initialised: Vec<LocalId>,
}

impl Program<'_> {
    fn same_component(&self, a: FnId, b: FnId) -> bool {
        self.components[a.0 as usize] == self.components[b.0 as usize]
    }
}

/// Tarjan's algorithm over the call graph.
///
/// A function is recursive exactly when something in its own component calls it, which
/// covers direct and mutual recursion alike. Everything else pays nothing for frames.
fn strongly_connected(hir: &Hir) -> Vec<u32> {
    let n = hir.functions.len();
    let edges: Vec<Vec<usize>> = hir
        .functions
        .iter()
        .map(|f| {
            let mut callees = Vec::new();
            collect_calls(&f.body, &mut callees);
            callees
        })
        .collect();

    struct State {
        index: Vec<Option<u32>>,
        low: Vec<u32>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        next_index: u32,
        component: Vec<u32>,
        next_component: u32,
    }

    fn visit(v: usize, edges: &[Vec<usize>], st: &mut State) {
        st.index[v] = Some(st.next_index);
        st.low[v] = st.next_index;
        st.next_index += 1;
        st.stack.push(v);
        st.on_stack[v] = true;
        for &w in &edges[v] {
            match st.index[w] {
                None => {
                    visit(w, edges, st);
                    st.low[v] = st.low[v].min(st.low[w]);
                }
                Some(index) if st.on_stack[w] => st.low[v] = st.low[v].min(index),
                Some(_) => {}
            }
        }
        if st.low[v] == st.index[v].expect("visited") {
            let id = st.next_component;
            st.next_component += 1;
            while let Some(w) = st.stack.pop() {
                st.on_stack[w] = false;
                st.component[w] = id;
                if w == v {
                    break;
                }
            }
        }
    }

    let mut st = State {
        index: vec![None; n],
        low: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        next_index: 0,
        component: vec![0; n],
        next_component: 0,
    };
    for v in 0..n {
        if st.index[v].is_none() {
            visit(v, &edges, &mut st);
        }
    }

    // Tarjan gives every function a component; a function is only recursive when a
    // call edge actually reaches it from its own component, and `same_component` is
    // asked exactly at call edges, so nothing more is needed here.
    let mut components = st.component;
    for (v, callees) in edges.iter().enumerate() {
        let alone = components.iter().filter(|c| **c == components[v]).count() == 1;
        if alone && !callees.contains(&v) {
            // Alone and not self-calling: give it a component nothing else can match,
            // including itself via a call edge that does not exist.
            components[v] = u32::MAX - v as u32;
        }
    }
    components
}

fn collect_calls(stmts: &[hir::Stmt], out: &mut Vec<usize>) {
    for stmt in stmts {
        match stmt {
            hir::Stmt::Let { value, .. } | hir::Stmt::Assign { value, .. } => calls_in(value, out),
            hir::Stmt::Return {
                value: Some(value), ..
            } => calls_in(value, out),
            hir::Stmt::CallFor { callee, args, .. } => {
                out.push(callee.0 as usize);
                for arg in args {
                    calls_in(arg, out);
                }
            }
            hir::Stmt::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                calls_in(cond, out);
                collect_calls(then, out);
                if let Some(otherwise) = otherwise {
                    collect_calls(otherwise, out);
                }
            }
            hir::Stmt::Loop { cond, body, .. } => {
                if let Some(cond) = cond {
                    calls_in(cond, out);
                }
                collect_calls(body, out);
            }
            hir::Stmt::Match { arms, .. } => {
                for arm in arms {
                    collect_calls(&arm.body, out);
                }
            }
            hir::Stmt::ForVec { body, .. } => collect_calls(body, out),
            _ => {}
        }
    }
}

fn calls_in(expr: &hir::Expr, out: &mut Vec<usize>) {
    match &expr.kind {
        hir::ExprKind::Call { callee, args } => {
            out.push(callee.0 as usize);
            for arg in args {
                calls_in(arg, out);
            }
        }
        hir::ExprKind::Unary(_, operand) => calls_in(operand, out),
        hir::ExprKind::Binary(_, lhs, rhs) => {
            calls_in(lhs, out);
            calls_in(rhs, out);
        }
        _ => {}
    }
}

/// Whether evaluating this expression can do anything other than produce a value.
/// Only calls can, and only they make short-circuiting observable.
fn is_pure(expr: &hir::Expr) -> bool {
    match &expr.kind {
        hir::ExprKind::Call { .. } => false,
        hir::ExprKind::Unary(_, operand) => is_pure(operand),
        hir::ExprKind::Binary(_, lhs, rhs) => is_pure(lhs) && is_pure(rhs),
        _ => true,
    }
}

fn local_reg(function: &hir::Function, local: LocalId) -> Reg {
    Reg {
        holder: crate::names::fake_player(&function.name, &function.locals[local.0 as usize].name),
        kind: RegKind::Var,
    }
}

fn param_reg(function: &hir::Function, local: LocalId) -> Reg {
    local_reg(function, local)
}

/// Where a composite binding lives: `mw.vars.<function>.<binding>` in `<ns>:mw`
/// (spec section 6.18). Qualified by function for the same reason a register is.
fn local_path(function: &hir::Function, local: LocalId) -> String {
    crate::names::var_path(&function.name, &function.locals[local.0 as usize].name)
}

/// Where a place lives: the binding's path with one step per field or index.
///
/// A runtime index is not part of it — that path can only be finished by a macro
/// (spec section 6.21), so it comes back as the prefix the macro splices into.
fn place_path(function: &hir::Function, place: &hir::Place) -> DataRef {
    let (target, mut path) = match &place.root {
        hir::Root::Local(local) => (DataTarget::Storage, local_path(function, *local)),
        // A borrowed place belongs to the caller, and says so by name.
        hir::Root::Lent { owner, local, .. } => {
            (DataTarget::Storage, crate::names::var_path(owner, local))
        }
        // A view names an entity, and the path is written on the command itself.
        hir::Root::Entity { selector } => (DataTarget::Entity(selector.clone()), String::new()),
    };
    for step in &place.steps {
        match step {
            Step::Field(name) => {
                // An entity's path starts at its root compound, so the first field
                // needs no separator before it.
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(name);
            }
            Step::Index(index) => path.push_str(&format!("[{index}]")),
            // The caller splices the index in; see `runtime_index`.
            Step::At(_) => break,
        }
    }
    DataRef { target, path }
}

/// The register a place names, for a scalar that lives on the scoreboard.
fn place_reg(function: &hir::Function, place: &hir::Place) -> Reg {
    match &place.root {
        Root::Local(local) => local_reg(function, *local),
        // A view's fields are in the entity's NBT; none of them is a register.
        Root::Entity { .. } => unreachable!("a view has no register"),
        Root::Lent { owner, local, .. } => Reg {
            holder: crate::names::fake_player(owner, local),
            kind: RegKind::Var,
        },
    }
}

/// The index expression of a place whose last step is only known at runtime.
fn runtime_index(place: &hir::Place) -> Option<&hir::Expr> {
    match place.steps.last() {
        Some(Step::At(index)) => Some(index),
        _ => None,
    }
}

/// The path a macro line writes, with the substitution in place of the index.
/// A string as SNBT: quoted, with the two characters that cannot stand alone inside
/// quotes escaped.
/// Whether `snbt` could write this value out in full, with nothing left to store.
fn is_literal(value: &hir::Expr) -> bool {
    match &value.kind {
        hir::ExprKind::Int(_) | hir::ExprKind::Bool(_) | hir::ExprKind::Str(_) => true,
        hir::ExprKind::Nbt(_) => true,
        hir::ExprKind::Some(inner) => is_literal(inner),
        _ => false,
    }
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", escaped(text))
}

/// The inside of a quoted string: the two characters that cannot stand alone there.
fn escaped(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn spliced(prefix: &str) -> String {
    format!("{prefix}[$({MACRO_INDEX})]")
}

/// The argument a macro helper reads its index from, under `mw.args`.
const MACRO_INDEX: &str = "i";

/// The macro parameter a spliced string arrives in.
const MACRO_TEXT: &str = "s";

/// The tag to store a scalar as when nothing named one.
fn tag_of(tag: Option<NbtTag>) -> &'static str {
    tag.unwrap_or(NbtTag::Int).keyword()
}

struct Lowering<'a, 'p> {
    function: &'a hir::Function,
    program: &'a mut Program<'p>,
    insts: Vec<Inst>,
    /// Functions split out of this one. Named under `prefix`, so the output stays
    /// walkable (requirements section 12.2).
    generated: Vec<Function>,
    prefix: String,
    counter: u32,
    /// A plain `return` only reaches the caller from the function's own top level.
    top_level: bool,
    /// Whether this block is inside the body of a context block.
    in_entity_body: bool,
    /// Whether this block *is* that body, rather than something nested in it.
    entity_body_root: bool,
}

impl<'p> Lowering<'_, 'p> {
    fn stmt(&mut self, stmt: &hir::Stmt) {
        match stmt {
            hir::Stmt::Break(_) => self.jump(CTL_BREAK),
            hir::Stmt::Continue(_) => self.jump(CTL_CONTINUE),
            hir::Stmt::Return { value, .. } => self.return_stmt(value.as_ref()),
            hir::Stmt::CallFor { callee, args, .. } => {
                self.call(*callee, args, Capture::Nothing);
            }
            hir::Stmt::Context {
                kind,
                selector,
                body,
                inline,
                ..
            } => self.context_stmt(*kind, selector, body, *inline),
            hir::Stmt::If {
                cond,
                then,
                otherwise,
                inline,
                ..
            } => self.if_stmt(cond, then, otherwise.as_deref(), *inline),
            hir::Stmt::Loop { cond, body, .. } => self.loop_stmt(cond.as_ref(), body),
            hir::Stmt::ForVec {
                source,
                binding,
                body,
                ..
            } => self.for_vec(source, *binding, body),
            hir::Stmt::Match {
                scrutinee, arms, ..
            } => self.match_stmt(scrutinee, arms),
            hir::Stmt::Raw(raw) => self.raw_command(raw),
            hir::Stmt::Let { local, value, .. } if self.local_ty(*local).is_storage() => {
                let path = local_path(self.function, *local);
                self.store_struct(&path.into(), value);
                self.program.initialised.push(*local);
            }
            // A check the release build does not carry is not lowered at all: the
            // condition is never evaluated, so no temporary is taken for it.
            hir::Stmt::Assert {
                cond,
                message,
                span,
            } if self.program.debug => {
                let cond = self.cond(cond).negate();
                let message = message
                    .clone()
                    .unwrap_or_else(|| "assertion failed".to_owned());
                self.insts.push(Inst::Guarded {
                    cond,
                    inst: Box::new(Inst::Report {
                        message,
                        span: *span,
                    }),
                });
            }
            hir::Stmt::Assert { .. } => {}
            hir::Stmt::Push { place, value, .. } => self.push_into(place, value),
            // Anything addressed through a step, and any composite, is in storage.
            hir::Stmt::Assign {
                place, op, value, ..
            } if place.ty.is_storage() || !place.steps.is_empty() => {
                self.assign_to_place(place, *op, value)
            }
            hir::Stmt::Let { local, value, .. } => {
                let dst = self.local(*local);
                self.store(dst, None, value);
                // Only now: a call inside `value` must not try to save this local,
                // which has nothing in it yet.
                self.program.initialised.push(*local);
            }
            hir::Stmt::Assign {
                place, op, value, ..
            } => {
                let dst = place_reg(self.function, place);
                self.store(dst, *op, value);
            }
        }
    }

    fn local_ty(&self, local: LocalId) -> Type {
        self.function.locals[local.0 as usize].ty
    }

    /// Writes through a place, which may need a macro to finish its path.
    fn assign_to_place(&mut self, place: &hir::Place, op: Option<BinaryOp>, value: &hir::Expr) {
        let path = place_path(self.function, place);
        let Some(index) = runtime_index(place) else {
            self.assign_in_storage(&path, place.ty, place.tag, op, value);
            return;
        };
        // Read-modify-write against a runtime index: read through a macro, do the
        // arithmetic on the scoreboard, write back through another.
        let value = match op {
            None => self.expr_or_copy(place.ty, value),
            Some(op) => {
                let acc = self.temps_next();
                let read = self.macro_helper(
                    "index",
                    Inst::ReturnRun {
                        inst: Box::new(Inst::GetData {
                            path: path.with_path(spliced(&path.path)),
                        }),
                    },
                );
                self.write_index_arg(index);
                self.insts.push(Inst::StoreResult {
                    dst: acc.clone(),
                    inst: Box::new(Inst::CallWithArgs { path: read }),
                });
                self.store(acc.clone(), Some(op), value);
                Written::Reg(acc)
            }
        };
        let write = match value {
            Written::Const(text) => Inst::SetValue {
                path: path.with_path(spliced(&path.path)),
                value: text,
            },
            Written::Reg(src) => Inst::StoreData {
                path: path.with_path(spliced(&path.path)),
                tag: tag_of(place.tag),
                inst: Box::new(Inst::Get { src }),
            },
            Written::Data(src) => Inst::CopyData {
                dst: path.with_path(spliced(&path.path)),
                src: src.into(),
            },
        };
        let helper = self.macro_helper("index", write);
        self.write_index_arg(index);
        self.insts.push(Inst::CallWithArgs { path: helper });
    }

    /// `s == "lit"` and `s == other` (spec section 6.27).
    ///
    /// Vanilla can only ask whether a path *holds* a value, so the comparison is a
    /// path match. A literal goes into the match as it is written; another string has
    /// to be spliced in, which is what makes that case a macro.
    fn string_compare(
        &mut self,
        op: BinaryOp,
        lhs: &hir::Expr,
        rhs: &hir::Expr,
        into: Option<Reg>,
    ) -> Value {
        match self.string_match(op, lhs, rhs) {
            // Both sides were literals, so the answer is already known.
            Err(answer) => Value::Const(answer),
            // `execute store success` names its destination, so the answer goes
            // straight where it is wanted.
            Ok(matched) => {
                let dst = into.unwrap_or_else(|| self.temps_next());
                self.store_match(dst.clone(), matched);
                Value::Reg(dst)
            }
        }
    }

    /// Writes a match's 0/1 into `dst`.
    fn store_match(&mut self, dst: Reg, matched: Match) {
        match matched {
            Match::Here(cond) => self.insts.push(Inst::StoreCond { dst, cond }),
            Match::Spliced { cond, src } => {
                let helper = self.macro_helper("streq", Inst::StoreCond { dst, cond });
                self.insts.push(Inst::CopyData {
                    dst: format!("mw.args.{MACRO_TEXT}").into(),
                    src: src.into(),
                });
                self.insts.push(Inst::CallWithArgs { path: helper });
            }
        }
    }

    /// The path match behind a string comparison, or the answer when both sides are
    /// literals and there is nothing to ask the game.
    fn string_match(
        &mut self,
        op: BinaryOp,
        lhs: &hir::Expr,
        rhs: &hir::Expr,
    ) -> Result<Match, i32> {
        let negated = op == BinaryOp::Ne;
        let lhs = self.expr_or_copy(Type::Str, lhs);
        let rhs = self.expr_or_copy(Type::Str, rhs);
        if let (Written::Const(a), Written::Const(b)) = (&lhs, &rhs) {
            return Err(i32::from((a == b) != negated));
        }
        let (path, other) = match (lhs, rhs) {
            (Written::Data(path), other) | (other, Written::Data(path)) => (path, other),
            _ => unreachable!("a string is never a register"),
        };
        let (parent, key) = self.filterable(path.into());
        Ok(match other {
            Written::Const(text) => Match::Here(Cond::Data {
                path: parent.clone(),
                filter: format!("{{{key}:{text}}}"),
                negated,
            }),
            // Two runtime strings: the only way vanilla can compare them is to splice
            // one into the match, and splicing needs a macro.
            Written::Data(src) => Match::Spliced {
                cond: Cond::Data {
                    path: parent,
                    filter: format!("{{{key}:\"$({MACRO_TEXT})\"}}"),
                    negated,
                },
                src,
            },
            Written::Reg(_) => unreachable!("a string is never a register"),
        })
    }

    /// A path split into the compound that holds it and the key inside, which is the
    /// form `execute if data` needs to match a value.
    ///
    /// A path that does not end in a key — an element of a list — is copied into
    /// `mw.tmp` first, which is the same trick `match` uses (spec section 6.20).
    fn filterable(&mut self, path: DataRef) -> (DataRef, String) {
        match path.path.rsplit_once('.') {
            Some((parent, key)) if !key.ends_with(']') => {
                (path.with_path(parent.to_owned()), key.to_owned())
            }
            _ => {
                // A temporary rather than a fixed path: whatever is in storage has to
                // be saved and restored across recursion, and `data_temp` is what the
                // save list is built from.
                let temp = self.data_temp();
                self.insts.push(Inst::CopyData {
                    dst: temp.clone().into(),
                    src: path,
                });
                let (parent, key) = temp.rsplit_once('.').expect("a temp path has a key");
                (DataRef::from(parent.to_owned()), key.to_owned())
            }
        }
    }

    /// `a + b` on strings: one macro line, with every literal part already in it
    /// (spec section 6.27).
    fn concat_into(&mut self, path: &DataRef, lhs: &hir::Expr, rhs: &hir::Expr) {
        let mut spliced = false;
        let mut body = String::new();
        for (side, name) in [(lhs, "a"), (rhs, "b")] {
            match &side.kind {
                hir::ExprKind::Str(text) => body.push_str(&escaped(text)),
                _ => {
                    let Written::Data(src) = self.expr_or_copy(Type::Str, side) else {
                        unreachable!("a string is a literal or a path")
                    };
                    self.insts.push(Inst::CopyData {
                        dst: format!("mw.args.{name}").into(),
                        src: src.into(),
                    });
                    body.push_str(&format!("$({name})"));
                    spliced = true;
                }
            }
        }
        let write = Inst::SetValue {
            path: path.to_owned(),
            value: format!("\"{body}\""),
        };
        // Two literals joined is still a literal: nothing has to be substituted.
        if !spliced {
            self.insts.push(write);
            return;
        }
        let helper = self.macro_helper("concat", write);
        self.insts.push(Inst::CallWithArgs { path: helper });
    }

    /// Evaluates a value into whatever a storage write can take: a literal, a register
    /// or a path to copy from.
    fn expr_or_copy(&mut self, ty: Type, value: &hir::Expr) -> Written {
        if !ty.is_storage() {
            return match self.expr(value) {
                Value::Const(n) => Written::Const(format!(
                    "{n}{}",
                    NbtTag::default_for(ty).map(NbtTag::suffix).unwrap_or("")
                )),
                Value::Reg(src) => Written::Reg(src),
            };
        }
        match &value.kind {
            // A literal is SNBT already; nothing has to hold it first.
            hir::ExprKind::Str(text) => Written::Const(quoted(text)),
            hir::ExprKind::Local(local) => Written::Data(local_path(self.function, *local)),
            hir::ExprKind::Field(place) if place.is_static() => {
                Written::Data(place_path(self.function, place).path)
            }
            // A composite that is not already somewhere: build it aside, then copy.
            _ => {
                let temp = self.data_temp();
                self.store_struct(&temp.clone().into(), value);
                Written::Data(temp)
            }
        }
    }

    /// Reads a scalar out of storage into `dst`, through a macro if the path needs one.
    ///
    /// A list read this way gives its element count, which is what `len` is.
    fn read_place_into(&mut self, dst: Reg, place: &hir::Place) {
        self.read_place_scaled(dst, place, scale_of(place));
    }

    /// The same read, in `scale`ths of a unit (spec section 6.26).
    fn read_place_scaled(&mut self, dst: Reg, place: &hir::Place, scale: u32) {
        // A scalar binding lives on the scoreboard, borrowed or not: there is nothing
        // in storage to read.
        if place.steps.is_empty() && !place.ty.is_storage() {
            let src = place_reg(self.function, place);
            self.insts.push(Inst::Op {
                dst,
                op: Op::Assign,
                src,
            });
            return;
        }
        let path = place_path(self.function, place);
        let read = |path: DataRef| match scale {
            1 => Inst::GetData { path },
            scale => Inst::GetScaled { path, scale },
        };
        let Some(index) = runtime_index(place) else {
            self.insts.push(Inst::StoreResult {
                dst,
                inst: Box::new(read(path.clone())),
            });
            return;
        };
        let helper = self.macro_helper(
            "index",
            Inst::ReturnRun {
                inst: Box::new(read(path.with_path(spliced(&path.path)))),
            },
        );
        self.write_index_arg(index);
        self.insts.push(Inst::StoreResult {
            dst,
            inst: Box::new(Inst::CallWithArgs { path: helper }),
        });
    }

    /// Writes the index a macro helper will splice into its path.
    fn write_index_arg(&mut self, index: &hir::Expr) {
        let value = self.expr(index);
        let src = self.materialise(value);
        self.insts.push(Inst::StoreData {
            path: format!("mw.args.{MACRO_INDEX}").into(),
            tag: "int",
            inst: Box::new(Inst::Get { src }),
        });
    }

    /// `raw!`, with any runtime value spliced in through a macro helper
    /// (spec section 6.31).
    ///
    /// The helper is what keeps the `$` line out of this function: a `#[tick]` function
    /// is called with no arguments, and a macro function called that way fails.
    fn raw_command(&mut self, raw: &hir::RawCommand) {
        let mut text = String::new();
        let mut args = Vec::new();
        for part in &raw.parts {
            match part {
                hir::RawPart::Lit(lit) => text.push_str(lit),
                hir::RawPart::Value(value) => match self.expr_or_copy(value.ty, value) {
                    // Known now after all: it can go straight into the line.
                    Written::Const(literal) if !value.ty.is_storage() => text.push_str(&literal),
                    written => {
                        let name = format!("a{}", args.len());
                        text.push_str(&format!("$({name})"));
                        args.push((name, written));
                    }
                },
            }
        }
        let inst = Inst::Raw {
            text,
            span: raw.span,
        };
        if args.is_empty() {
            self.insts.push(inst);
            return;
        }
        let helper = self.macro_helper("raw", inst);
        // Written last: evaluating a value can itself call a macro helper, which uses
        // `mw.args` for its own arguments.
        for (name, written) in args {
            self.write_macro_arg(&name, written);
        }
        self.insts.push(Inst::CallWithArgs { path: helper });
    }

    /// Puts one value where the macro helper will read it from.
    fn write_macro_arg(&mut self, name: &str, written: Written) {
        let path = DataRef::from(format!("mw.args.{name}"));
        self.insts.push(match written {
            Written::Const(value) => Inst::SetValue { path, value },
            Written::Reg(src) => Inst::StoreData {
                path,
                tag: "int",
                inst: Box::new(Inst::Get { src }),
            },
            Written::Data(src) => Inst::CopyData {
                dst: path,
                src: src.into(),
            },
        });
    }

    /// A generated function whose whole body is one macro line.
    fn macro_helper(&mut self, kind: &str, inst: Inst) -> String {
        let path = format!("{}/{kind}_{}", self.prefix, self.counter);
        self.counter += 1;
        self.record(
            path.clone(),
            vec![Inst::Macro {
                inst: Box::new(inst),
            }],
            Vec::new(),
        );
        path
    }

    /// `v.push(x)`.
    fn push_into(&mut self, place: &hir::Place, value: &hir::Expr) {
        let path = place_path(self.function, place);
        if runtime_index(place).is_some() {
            // Two macro calls and a placeholder; nothing needs it yet, and guessing at
            // the shape would be guessing.
            self.insts.push(Inst::Raw {
                text: "# pushing into a list reached by a runtime index is not implemented"
                    .to_owned(),
                span: value.span,
            });
            return;
        }
        let hir::Type::Vec(id) = place.ty else {
            unreachable!("push is only allowed on a list")
        };
        let elem = self.program.types.element(id);
        let tag = NbtTag::default_for(elem);
        match self.expr_or_copy(elem, value) {
            Written::Const(text) => self.insts.push(Inst::AppendValue { path, value: text }),
            Written::Data(src) => self.insts.push(Inst::AppendFrom {
                dst: path,
                src: src.into(),
            }),
            // The list has to grow before the value can be stored into its last slot.
            Written::Reg(src) => {
                self.insts.push(Inst::AppendValue {
                    path: path.clone(),
                    value: format!("0{}", tag.map(NbtTag::suffix).unwrap_or("")),
                });
                self.insts.push(Inst::StoreData {
                    path: path.with_path(format!("{}[-1]", path.path)),
                    tag: tag_of(tag),
                    inst: Box::new(Inst::Get { src }),
                });
            }
        }
    }

    /// Writes to something that lives in storage: a whole compound, or one field.
    ///
    /// A compound assignment is the one case that costs three commands: the arithmetic
    /// has to happen on the scoreboard, so the field is read out, changed and written
    /// back. Vanilla has no arithmetic on storage to shorten this with.
    fn assign_in_storage(
        &mut self,
        path: &DataRef,
        ty: Type,
        tag: Option<NbtTag>,
        op: Option<BinaryOp>,
        value: &hir::Expr,
    ) {
        // An option is the value itself, or the path not being there at all. The tag
        // is the one the field was declared with, and it belongs to what is held.
        if let Type::Option(id) = ty {
            let inner = self.program.types.inner(id);
            self.store_option(path, inner, tag, value, false);
            return;
        }
        if ty.is_storage() {
            self.store_struct(path, value);
            return;
        }
        let src = match op {
            None => self.expr(value),
            Some(op) => {
                let acc = self.temps_next();
                self.insts.push(Inst::StoreResult {
                    dst: acc.clone(),
                    inst: Box::new(Inst::GetData {
                        path: path.to_owned(),
                    }),
                });
                self.store(acc.clone(), Some(op), value);
                Value::Reg(acc)
            }
        };
        // A `fix<S>` under a float or double tag is the real number, so the raw units
        // are divided by `S` on the way in (spec section 4.16).
        let scale = scale_from(ty, tag);
        let suffix = tag.unwrap_or(NbtTag::Int).suffix();
        match src {
            // A constant needs no register to pass through: `set value` takes it.
            Value::Const(n) => self.insts.push(Inst::SetValue {
                path: path.to_owned(),
                value: match scale {
                    1 => format!("{n}{suffix}"),
                    scale => format!("{}{suffix}", f64::from(n) / f64::from(scale)),
                },
            }),
            Value::Reg(src) => self.insts.push(match scale {
                1 => Inst::StoreData {
                    path: path.to_owned(),
                    tag: tag_of(tag),
                    inst: Box::new(Inst::Get { src }),
                },
                scale => Inst::StoreScaled {
                    path: path.to_owned(),
                    tag: tag_of(tag),
                    scale,
                    inst: Box::new(Inst::Get { src }),
                },
            }),
        }
    }

    /// Writes an option (spec section 6.28).
    ///
    /// `fresh` says the path is known to hold nothing yet — the key of a compound that
    /// was just written whole — which is what lets the copy skip its `data remove`.
    fn store_option(
        &mut self,
        path: &DataRef,
        inner: Type,
        tag: Option<NbtTag>,
        value: &hir::Expr,
        fresh: bool,
    ) {
        match &value.kind {
            hir::ExprKind::None => self.insts.push(Inst::RemoveData {
                path: path.to_owned(),
            }),
            // Writing the value overwrites whatever was there, so nothing has to be
            // cleared first.
            hir::ExprKind::Some(value) => self.assign_in_storage(path, inner, tag, None, value),
            // A call answers in two registers, so the option is put together here.
            hir::ExprKind::Call { callee, args } => {
                let called = self
                    .call(*callee, args, Capture::Option)
                    .expect("a value was wanted");
                if !fresh {
                    self.insts.push(Inst::RemoveData {
                        path: path.to_owned(),
                    });
                }
                self.insts.push(Inst::Guarded {
                    cond: Cond::Matches {
                        src: called.present.expect("both halves"),
                        min: Some(1),
                        max: Some(1),
                        negated: false,
                    },
                    inst: Box::new(Inst::StoreData {
                        path: path.to_owned(),
                        tag: tag_of(tag.or_else(|| NbtTag::default_for(inner))),
                        inst: Box::new(Inst::Get { src: called.value }),
                    }),
                });
            }
            // Copying another option: `set from` on a path that is not there fails and
            // leaves what was there, so the destination is cleared first.
            _ => {
                if !fresh {
                    self.insts.push(Inst::RemoveData {
                        path: path.to_owned(),
                    });
                }
                match self.expr_or_copy(value.ty, value) {
                    Written::Data(src) => self.insts.push(Inst::CopyData {
                        dst: path.to_owned(),
                        src: src.into(),
                    }),
                    _ => unreachable!("an option is a path or a value written into one"),
                }
            }
        }
    }

    /// Writes a composite value to `path`.
    ///
    /// One `set value` puts everything that is known now in place, and only the fields
    /// that are not get a command of their own (spec section 6.18).
    fn store_struct(&mut self, path: &DataRef, value: &hir::Expr) {
        if let Type::Option(id) = value.ty {
            let inner = self.program.types.inner(id);
            let tag = NbtTag::default_for(inner);
            self.store_option(path, inner, tag, value, false);
            return;
        }
        match &value.kind {
            hir::ExprKind::Struct { .. }
            | hir::ExprKind::Enum { .. }
            | hir::ExprKind::List { .. } => {
                let snbt = self.snbt(value, None);
                self.insts.push(Inst::SetValue {
                    path: path.to_owned(),
                    value: snbt,
                });
                self.write_runtime_fields(path, value);
            }
            // Already SNBT, and checked while compiling: one `set value`.
            hir::ExprKind::Nbt(text) => self.insts.push(Inst::SetValue {
                path: path.to_owned(),
                value: text.clone(),
            }),
            hir::ExprKind::Binary(BinaryOp::Add, lhs, rhs) => self.concat_into(path, lhs, rhs),
            hir::ExprKind::Slice { place, start, end } => {
                let src = place_path(self.function, place);
                self.insts.push(Inst::SetString {
                    dst: path.to_owned(),
                    src,
                    start: *start,
                    end: *end,
                });
            }
            // A literal is one `set value`, the same as any other constant.
            hir::ExprKind::Str(text) => self.insts.push(Inst::SetValue {
                path: path.to_owned(),
                value: quoted(text),
            }),
            hir::ExprKind::Local(local) => {
                let src = local_path(self.function, *local);
                self.insts.push(Inst::CopyData {
                    dst: path.to_owned(),
                    src: src.into(),
                });
            }
            hir::ExprKind::Field(place) => {
                let src = place_path(self.function, place);
                match runtime_index(place) {
                    None => self.insts.push(Inst::CopyData {
                        dst: path.clone(),
                        src,
                    }),
                    Some(index) => {
                        let helper = self.macro_helper(
                            "index",
                            Inst::CopyData {
                                dst: path.clone(),
                                src: src.with_path(spliced(&src.path)),
                            },
                        );
                        self.write_index_arg(index);
                        self.insts.push(Inst::CallWithArgs { path: helper });
                    }
                }
            }
            // One command, the same as any other store into storage: `execute store`
            // divides by the scale as it writes (spec section 6.26).
            hir::ExprKind::AsNbt {
                value: inner,
                scale,
            } => {
                let src = match &inner.kind {
                    // A score binding is already a register; copying it into a
                    // temporary first would be a command spent on nothing.
                    hir::ExprKind::Field(place)
                        if place.steps.is_empty() && !place.ty.is_storage() =>
                    {
                        place_reg(self.function, place)
                    }
                    _ => {
                        let src = self.expr(inner);
                        self.materialise(src)
                    }
                };
                let store = Inst::StoreScaled {
                    path: path.to_owned(),
                    tag: tag_of(NbtTag::default_for(value.ty)),
                    scale: *scale,
                    inst: Box::new(Inst::Get { src }),
                };
                self.insts.push(store);
            }
            other => unreachable!("{other:?} is not a composite value"),
        }
    }

    /// The value as SNBT, with a placeholder wherever it is not known until runtime.
    ///
    /// The placeholder costs nothing: the key has to be in the compound either way,
    /// and writing it here means the store that follows overwrites rather than
    /// creates — one fewer way for a mistake to leave the field quietly absent.
    fn snbt(&self, value: &hir::Expr, tag: Option<NbtTag>) -> String {
        let suffix = tag.map(NbtTag::suffix).unwrap_or_default();
        match &value.kind {
            hir::ExprKind::Int(n) => format!("{n}{suffix}"),
            hir::ExprKind::Str(text) => quoted(text),
            hir::ExprKind::Nbt(text) => text.clone(),
            hir::ExprKind::Bool(b) => format!("{}{suffix}", i32::from(*b)),
            hir::ExprKind::Struct { id, fields } => {
                let def = self.program.types.struct_def(*id);
                format!(
                    "{{{}}}",
                    self.compound_body(&def.fields, fields, Vec::new())
                )
            }
            hir::ExprKind::List { elem, values } => {
                let tag = NbtTag::default_for(*elem);
                let body = values
                    .iter()
                    .map(|value| self.snbt(value, tag))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("[{body}]")
            }
            hir::ExprKind::Enum {
                id,
                variant,
                fields,
            } => {
                let variant = &self.program.types.enum_def(*id).variants[*variant as usize];
                // The tag comes first, as vanilla's own compounds read.
                let tag = vec![format!("{TAG_KEY}:\"{}\"", variant.name)];
                format!("{{{}}}", self.compound_body(&variant.fields, fields, tag))
            }
            hir::ExprKind::Some(inner) => self.snbt(inner, tag),
            // Not known now: leave room for the write that follows.
            _ => match value.ty {
                Type::Struct(_) | Type::Enum(_) => "{}".to_owned(),
                Type::Vec(_) => "[]".to_owned(),
                Type::Str => "\"\"".to_owned(),
                _ => format!("0{suffix}"),
            },
        }
    }

    /// The inside of a compound: `first` entries, then one per field.
    fn compound_body(
        &self,
        declared: &[hir::Field],
        values: &[hir::Expr],
        first: Vec<String>,
    ) -> String {
        first
            .into_iter()
            .chain(
                declared
                    .iter()
                    .zip(values)
                    // An option that is not a value right here leaves no key at all:
                    // the key being absent is what `None` is (spec section 6.28).
                    .filter(|(field, value)| {
                        !matches!(field.ty, Type::Option(_))
                            || matches!(value.kind, hir::ExprKind::Some(_))
                    })
                    .map(|(field, value)| format!("{}:{}", field.nbt, self.snbt(value, field.tag))),
            )
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Writes the fields `snbt` could only leave a placeholder for.
    fn write_runtime_fields(&mut self, path: &DataRef, value: &hir::Expr) {
        if let hir::ExprKind::List { elem, values } = &value.kind {
            let elem = *elem;
            let tag = NbtTag::default_for(elem);
            for (index, value) in values.iter().enumerate() {
                let path = path.with_path(format!("{}[{index}]", path.path));
                match &value.kind {
                    hir::ExprKind::Int(_) | hir::ExprKind::Bool(_) => {}
                    _ if elem.is_storage() => self.store_struct(&path, value),
                    hir::ExprKind::Struct { .. }
                    | hir::ExprKind::Enum { .. }
                    | hir::ExprKind::List { .. } => self.write_runtime_fields(&path, value),
                    _ => {
                        let src = self.expr(value);
                        let src = self.materialise(src);
                        self.insts.push(Inst::StoreData {
                            path,
                            tag: tag_of(tag),
                            inst: Box::new(Inst::Get { src }),
                        });
                    }
                }
            }
            return;
        }
        let (declared, values) = match &value.kind {
            hir::ExprKind::Struct { id, fields } => {
                (self.program.types.struct_def(*id).fields.clone(), fields)
            }
            hir::ExprKind::Enum {
                id,
                variant,
                fields,
            } => (
                self.program.types.enum_def(*id).variants[*variant as usize]
                    .fields
                    .clone(),
                fields,
            ),
            _ => return,
        };
        for (field, value) in declared.iter().zip(values) {
            let path = path.with_path(format!("{}.{}", path.path, field.nbt));
            // The key was left out of the compound, so there is nothing to clear.
            if let Type::Option(id) = field.ty {
                let inner = self.program.types.inner(id);
                match &value.kind {
                    // Already in the compound, or deliberately not in it.
                    hir::ExprKind::None => {}
                    hir::ExprKind::Some(inner_value) if is_literal(inner_value) => {}
                    _ => self.store_option(&path, inner, field.tag, value, true),
                }
                continue;
            }
            match &value.kind {
                hir::ExprKind::Int(_) | hir::ExprKind::Bool(_) => {}
                hir::ExprKind::Struct { .. } => self.write_runtime_fields(&path, value),
                _ if field.ty.is_storage() => self.store_struct(&path, value),
                _ => {
                    let src = self.expr(value);
                    let src = self.materialise(src);
                    self.insts.push(Inst::StoreData {
                        path,
                        tag: tag_of(field.tag),
                        inst: Box::new(Inst::Get { src }),
                    });
                }
            }
        }
    }

    /// Writes `value` into `dst`, applying `op` first for a compound assignment.
    fn store(&mut self, dst: Reg, op: Option<BinaryOp>, value: &hir::Expr) {
        if op.is_none() {
            self.expr_into(dst, value);
            return;
        }
        let value = self.expr(value);
        match (op, value) {
            // `players add` and `remove` exist for exactly this, so use them.
            (Some(BinaryOp::Add), Value::Const(n)) => {
                self.insts.push(Inst::AddConst { dst, value: n })
            }
            (Some(BinaryOp::Sub), Value::Const(n)) => self.insts.push(Inst::AddConst {
                dst,
                value: n.wrapping_neg(),
            }),
            (Some(op), value) => {
                let src = self.materialise(value);
                self.insts.push(Inst::Op {
                    dst,
                    op: arith(op),
                    src,
                });
            }
            (None, _) => unreachable!("handled above"),
        }
    }

    /// Evaluates `expr` directly into `dst` where the command already writes somewhere.
    ///
    /// `execute store success score <dst> ... if ...` names its destination, so routing
    /// a comparison through a temporary and copying would be two commands doing one
    /// command's work. Safe even when `dst` is also an operand: the condition is
    /// evaluated before the store.
    fn expr_into(&mut self, dst: Reg, expr: &hir::Expr) {
        match &expr.kind {
            // A string comparison writes its own destination too, but it is a path
            // match rather than a score comparison (spec section 6.27).
            hir::ExprKind::Binary(op, lhs, rhs) if lhs.ty == Type::Str => {
                match self.string_compare(*op, lhs, rhs, Some(dst.clone())) {
                    // Both sides were literals, so nothing was stored yet.
                    value @ Value::Const(_) => self.copy_into(dst, value),
                    Value::Reg(_) => {}
                }
            }
            hir::ExprKind::Binary(op, lhs, rhs) if is_comparison(*op) => {
                let lhs = self.expr(lhs);
                let rhs = self.expr(rhs);
                self.compare_into(dst, *op, lhs, rhs);
            }
            hir::ExprKind::Unary(UnaryOp::Not, operand) => {
                let value = self.expr(operand);
                let src = self.materialise(value);
                self.insts.push(Inst::Matches {
                    dst,
                    src,
                    min: Some(0),
                    max: Some(0),
                    negated: false,
                });
            }
            // `execute store result score <dst> run data get ...` names its
            // destination too, so reading a field needs no temporary in between.
            hir::ExprKind::Field(place) | hir::ExprKind::Len(place) if !expr.ty.is_storage() => {
                self.read_place_into(dst, place);
            }
            hir::ExprKind::ReadScaled { place, scale } => {
                self.read_place_scaled(dst, place, *scale);
            }
            _ => {
                let value = self.expr(expr);
                self.copy_into(dst, value);
            }
        }
    }

    /// `return` from a generated block cannot reach the caller by itself: mcfunction's
    /// `return` only leaves the function it is written in. So a nested `return` parks
    /// its value in `$<fn>.ret`, raises the control register, and lets the propagation
    /// guards carry it out to the top, where it becomes a real return.
    fn return_stmt(&mut self, value: Option<&hir::Expr>) {
        // Returning an option is returning both halves of vanilla's call outcome:
        // the value, and whether there was one (spec section 6.28).
        if let Some(expr) = value
            && matches!(expr.ty, Type::Option(_))
        {
            self.return_option(expr);
            return;
        }
        let value = value.map(|expr| self.expr(expr));
        if self.top_level {
            match value {
                // `return` takes an integer literal, so a constant needs no help.
                Some(Value::Const(n)) => self.insts.push(Inst::Return { value: n }),
                Some(Value::Reg(src)) => self.insts.push(Inst::ReturnRun {
                    inst: Box::new(Inst::Get { src }),
                }),
                None => self.insts.push(Inst::Return { value: 0 }),
            }
            return;
        }
        if let Some(value) = value {
            let ret = self.ret_reg();
            self.copy_into(ret, value);
        }
        self.jump(CTL_RETURN);
    }

    /// `return Some(v)` / `return None` / `return o`.
    fn return_option(&mut self, expr: &hir::Expr) {
        match &expr.kind {
            hir::ExprKind::None => match self.top_level {
                true => self.insts.push(Inst::ReturnFail),
                false => self.jump(CTL_FAIL),
            },
            hir::ExprKind::Some(inner) => {
                let value = self.expr(inner);
                match self.top_level {
                    true => match value {
                        Value::Const(n) => self.insts.push(Inst::Return { value: n }),
                        Value::Reg(src) => self.insts.push(Inst::ReturnRun {
                            inst: Box::new(Inst::Get { src }),
                        }),
                    },
                    false => {
                        let ret = self.ret_reg();
                        self.copy_into(ret, value);
                        self.jump(CTL_RETURN);
                    }
                }
            }
            // An option that is only known at runtime: ask, then answer either way.
            _ => {
                let (value, present) = self.option_parts(expr);
                if self.top_level {
                    self.leave_if_absent(present);
                    self.insts.push(Inst::ReturnRun {
                        inst: Box::new(Inst::Get { src: value }),
                    });
                    return;
                }
                let ret = self.ret_reg();
                self.copy_into(ret, Value::Reg(value));
                let ctl = self.ctl();
                self.insts.push(Inst::Const {
                    dst: ctl.clone(),
                    value: CTL_FAIL,
                });
                self.insts.push(Inst::Guarded {
                    cond: Cond::Matches {
                        src: present,
                        min: Some(1),
                        max: Some(1),
                        negated: false,
                    },
                    inst: Box::new(Inst::Const {
                        dst: ctl,
                        value: CTL_RETURN,
                    }),
                });
                self.insts.push(Inst::Return { value: 0 });
            }
        }
    }

    /// An option in two registers: what it holds, and whether it holds anything.
    ///
    /// A call answers both halves at once (spec section 6.28); a path has to be asked
    /// twice, and the read of an absent path stores 0 rather than failing to store.
    fn option_parts(&mut self, expr: &hir::Expr) -> (Reg, Reg) {
        if let hir::ExprKind::Call { callee, args } = &expr.kind {
            let called = self
                .call(*callee, args, Capture::Option)
                .expect("a value was wanted");
            return (called.value, called.present.expect("both halves"));
        }
        let path = option_path(self.function, expr).expect("an option this can read");
        let present = self.present_at(&path);
        let value = self.temps_next();
        self.insts.push(Inst::StoreResult {
            dst: value.clone(),
            inst: Box::new(Inst::GetData { path }),
        });
        (value, present)
    }

    /// Whether a path holds anything, in a register: one command.
    fn present_at(&mut self, path: &DataRef) -> Reg {
        let present = self.temps_next();
        self.insts.push(Inst::StoreCond {
            dst: present.clone(),
            cond: Cond::Data {
                path: path.clone(),
                filter: String::new(),
                negated: false,
            },
        });
        present
    }

    /// Leaves the function with nothing when `present` says there was nothing.
    fn leave_if_absent(&mut self, present: Reg) {
        let absent = Cond::Matches {
            src: present,
            min: Some(0),
            max: Some(0),
            negated: false,
        };
        if self.top_level {
            self.insts.push(Inst::Guarded {
                cond: absent,
                inst: Box::new(Inst::ReturnFail),
            });
            return;
        }
        // Inside a generated block a `return` only leaves that block, so the reason
        // goes in the control register and the guards carry it out (spec section 6.10).
        let ctl = self.ctl();
        self.insts.push(Inst::Guarded {
            cond: absent.clone(),
            inst: Box::new(Inst::Const {
                dst: ctl,
                value: CTL_FAIL,
            }),
        });
        self.insts.push(Inst::Guarded {
            cond: absent,
            inst: Box::new(Inst::Return { value: 0 }),
        });
    }

    fn ret_reg(&self) -> Reg {
        Reg {
            holder: format!("${}.ret", self.function.name),
            kind: RegKind::Var,
        }
    }

    /// `break`, `continue` and `return` all leave the same way: record why in the
    /// control register, then return. Only `return` survives past the enclosing loop.
    ///
    /// Inside a `for` over entities, `continue` is the exception: the body is one
    /// function per entity, so returning from it *is* going to the next one, and
    /// raising the register would make every later entity skip itself as well.
    fn jump(&mut self, code: i32) {
        // Returning from the body function is already "next entity", so at the body's
        // own top level `continue` needs nothing else. From a block nested inside it,
        // the return has to be carried out through the control register first.
        if code == CTL_CONTINUE && self.entity_body_root {
            self.insts.push(Inst::Return { value: 0 });
            return;
        }
        let ctl = self.ctl();
        self.insts.push(Inst::Const {
            dst: ctl,
            value: code,
        });
        self.insts.push(Inst::Return { value: 0 });
    }

    fn if_stmt(
        &mut self,
        cond: &hir::Expr,
        then: &[hir::Stmt],
        otherwise: Option<&[hir::Stmt]>,
        inline: hir::Inline,
    ) {
        let escaping = escapes(then).union(otherwise.map(escapes).unwrap_or_default());
        let cond = self.cond(cond);

        // A single command under a guard needs no function of its own.
        let inlinable = otherwise.is_none()
            && then.len() == 1
            && !escaping.any()
            && inline != hir::Inline::Never;
        if inlinable {
            let before = self.insts.len();
            self.stmt(&then[0]);
            if self.insts.len() == before + 1 {
                let inst = self.insts.pop().expect("just pushed");
                self.insts.push(Inst::Guarded {
                    cond,
                    inst: Box::new(inst),
                });
                return;
            }
            // The statement needed more than one command after all; undo and split.
            self.insts.truncate(before);
        }

        let then_path = self.split("if", then);
        self.insts.push(Inst::Guarded {
            cond: cond.clone(),
            inst: Box::new(Inst::Call { path: then_path }),
        });
        if let Some(otherwise) = otherwise {
            let else_path = self.split("else", otherwise);
            self.insts.push(Inst::Guarded {
                cond: cond.negate(),
                inst: Box::new(Inst::Call { path: else_path }),
            });
        }
        if escaping.any() {
            self.propagate();
        }
    }

    /// One guard per arm. The tags are exclusive, so nothing has to stop the others.
    fn match_stmt(&mut self, scrutinee: &hir::Scrutinee, arms: &[hir::Arm]) {
        let escaping = arms
            .iter()
            .fold(Escapes::default(), |acc, arm| acc.union(escapes(&arm.body)));
        // Guards run in order, and an arm is free to rewrite what is being matched —
        // a state machine does exactly that. Testing a copy taken on the way in is
        // what keeps exactly one arm running (spec section 6.20).
        //
        // For an option the copy is one bit — whether there is anything — so it fits
        // in a register, and one command puts it there (spec section 6.28).
        let (path, present, held) = match scrutinee {
            // A call: both halves of its outcome are already in registers.
            hir::Scrutinee::Option(value) => {
                let (held, present) = self.option_parts(value);
                (DataRef::from(String::new()), Some(present), Some(held))
            }
            hir::Scrutinee::Place(place) => {
                let path = place_path(self.function, place);
                let present = matches!(place.ty, Type::Option(_)).then(|| {
                    let reg = self.temps_next();
                    self.insts.push(Inst::StoreCond {
                        dst: reg.clone(),
                        cond: Cond::Data {
                            path: path.clone(),
                            filter: String::new(),
                            negated: false,
                        },
                    });
                    reg
                });
                (path, present, None)
            }
        };
        let tested = match present {
            Some(_) => DataRef::from(String::new()),
            None => {
                let tested: DataRef = self.data_temp().into();
                self.insts.push(Inst::CopyData {
                    dst: tested.clone(),
                    src: path.clone(),
                });
                tested
            }
        };
        let base = format!("match_{}", self.counter);
        self.counter += 1;

        let mut covered = Vec::new();
        for arm in arms {
            let arm_path = format!("{}/{base}/{}", self.prefix, arm.path);
            let mut inner = self.child(arm_path.clone());
            // The payload is copied into registers on the way in: the arm reads a
            // binding, not a path, and the compound is not written back.
            for binding in &arm.bindings {
                let dst = local_reg(inner.function, binding.local);
                // A call's option is already in a register; anything in storage is
                // read from its path. An option holds its value at the scrutinee's
                // own path, while an enum's payload is a key under it.
                if let Some(held) = &held {
                    inner.copy_into(dst, Value::Reg(held.clone()));
                    continue;
                }
                let field = match binding.nbt.is_empty() {
                    true => path.clone(),
                    false => path.with_path(format!("{}.{}", path.path, binding.nbt)),
                };
                if binding.ty.is_storage() {
                    let dst = local_path(inner.function, binding.local);
                    inner.insts.push(Inst::CopyData {
                        dst: dst.into(),
                        src: field,
                    });
                } else {
                    // The tag decides the scale, the same as any other read
                    // (spec section 4.16).
                    let read = match scale_from(binding.ty, binding.tag) {
                        1 => Inst::GetData { path: field },
                        scale => Inst::GetScaled { path: field, scale },
                    };
                    inner.insts.push(Inst::StoreResult {
                        dst,
                        inst: Box::new(read),
                    });
                }
            }
            for stmt in &arm.body {
                inner.stmt(stmt);
            }
            let (insts, generated) = inner.finish();
            self.record(arm_path.clone(), insts, generated);

            let call = Inst::Call { path: arm_path };
            match (arm.test, &present) {
                (hir::ArmTest::Variant(variant), _) => {
                    let tag = self.tag_of_variant(scrutinee, variant);
                    covered.push(tag.clone());
                    self.insts.push(Inst::Guarded {
                        cond: Cond::Data {
                            path: tested.clone(),
                            filter: tag_filter(&tag),
                            negated: false,
                        },
                        inst: Box::new(call),
                    });
                }
                // The snapshot is 1 or 0, and the arms ask which.
                (test, Some(reg)) => {
                    let wanted = match test {
                        hir::ArmTest::Present => 1,
                        hir::ArmTest::Absent => 0,
                        // A `_` after one of the two is the other one.
                        _ => i32::from(!arms.iter().any(|a| a.test == hir::ArmTest::Present)),
                    };
                    self.insts.push(Inst::Guarded {
                        cond: Cond::Matches {
                            src: reg.clone(),
                            min: Some(wanted),
                            max: Some(wanted),
                            negated: false,
                        },
                        inst: Box::new(call),
                    });
                }
                (_, None) => self.insts.push(Inst::Otherwise {
                    path: tested.clone(),
                    tags: covered.clone(),
                    inst: Box::new(call),
                }),
            }
        }
        // Every arm is a function of its own, so a `break` or `return` inside one gets
        // out the same way it does from an `if` (spec section 6.10).
        if escaping.any() {
            self.propagate();
        }
    }

    /// The tag string a variant of the matched enum is stored under.
    fn tag_of_variant(&self, scrutinee: &hir::Scrutinee, variant: u32) -> String {
        let hir::Scrutinee::Place(place) = scrutinee else {
            unreachable!("only a place holds an enum")
        };
        let hir::Type::Enum(id) = place.ty else {
            unreachable!("only an enum is matched on")
        };
        self.program.types.enum_def(id).variants[variant as usize]
            .name
            .clone()
    }

    /// `as` / `at` / `for`: the body becomes a function, run once per entity.
    fn context_stmt(
        &mut self,
        kind: hir::ContextKind,
        selector: &hir::Selector,
        body: &[hir::Stmt],
        inline: hir::Inline,
    ) {
        let escaping = escapes(body);
        let clause = match kind {
            hir::ContextKind::At => ExecuteAs::At(selector.text.clone()),
            _ => ExecuteAs::As(selector.text.clone()),
        };

        // A single command that cannot transfer control needs no function.
        let inlinable = body.len() == 1 && !escaping.any() && inline != hir::Inline::Never;
        if inlinable {
            let before = self.insts.len();
            self.stmt(&body[0]);
            if self.insts.len() == before + 1 {
                let inst = self.insts.pop().expect("just pushed");
                self.insts.push(Inst::Context {
                    clause,
                    inst: Box::new(inst),
                });
                return;
            }
            self.insts.truncate(before);
        }

        let name = match kind {
            hir::ContextKind::As => "as",
            hir::ContextKind::At => "at",
            hir::ContextKind::For => "for",
        };
        let path = format!("{}/{name}_{}", self.prefix, self.counter);
        self.counter += 1;

        let mut inner = self.child(path.clone());
        inner.in_entity_body = true;
        inner.entity_body_root = true;
        // A `continue` raised from a nested block has done its job by the time it
        // reaches here, so clear it before the guard below could mistake it for a
        // `break`.
        if escaping.continues {
            inner.consume(CTL_CONTINUE);
        }
        // The body runs once per entity, and nothing can stop `execute as` partway
        // through. A `break` or `return` therefore makes every later entity return
        // immediately instead.
        if escaping.breaks || escaping.returns {
            let ctl = inner.ctl();
            inner.insts.push(Inst::Guarded {
                cond: Cond::Matches {
                    src: ctl,
                    min: Some(CTL_BREAK),
                    max: None,
                    negated: false,
                },
                inst: Box::new(Inst::Return { value: 0 }),
            });
        }
        for stmt in body {
            inner.stmt(stmt);
        }
        let (insts, generated) = inner.finish();
        self.record(path.clone(), insts, generated);

        self.insts.push(Inst::Context {
            clause,
            inst: Box::new(Inst::Call { path }),
        });
        if escaping.breaks {
            self.consume(CTL_BREAK);
        }
        if escaping.returns {
            self.propagate();
        }
    }

    /// `for x in v`: walk a copy, taking `[0]` each time (spec section 6.22).
    ///
    /// The index is always zero, so every path is known while compiling and no macro
    /// is needed — which is the reason iteration is destructive in the first place.
    fn for_vec(&mut self, source: &hir::Place, binding: LocalId, body: &[hir::Stmt]) {
        let escaping = escapes(body);
        let src = place_path(self.function, source);
        let iter = self.iter_temp();
        self.insts.push(Inst::CopyData {
            dst: iter.clone().into(),
            src,
        });
        let path = format!("{}/for_{}", self.prefix, self.counter);
        self.counter += 1;

        let mut inner = self.child(path.clone());
        let head = format!("{iter}[0]");
        // Nothing left to take: the loop is over.
        inner.insts.push(Inst::Guarded {
            cond: Cond::Data {
                path: head.clone().into(),
                filter: String::new(),
                negated: true,
            },
            inst: Box::new(Inst::Return { value: 0 }),
        });
        let ty = inner.local_ty(binding);
        if ty.is_storage() {
            let dst = local_path(inner.function, binding);
            inner.insts.push(Inst::CopyData {
                dst: dst.into(),
                src: head.clone().into(),
            });
        } else {
            let dst = local_reg(inner.function, binding);
            inner.insts.push(Inst::StoreResult {
                dst,
                inst: Box::new(Inst::GetData {
                    path: head.clone().into(),
                }),
            });
        }
        inner.insts.push(Inst::RemoveData { path: head.into() });
        // The binding holds a value from here on, so a recursive call has to save it.
        inner.program.initialised.push(binding);
        if escaping.continues {
            // `continue` returns, and a return from the loop function would end the
            // loop. Give the body its own function so the tail call still happens.
            let body_path = inner.split("body", body);
            inner.insts.push(Inst::Call { path: body_path });
            inner.consume(CTL_CONTINUE);
            inner.propagate();
        } else {
            for stmt in body {
                inner.stmt(stmt);
            }
        }
        inner.insts.push(Inst::Call { path: path.clone() });
        let (insts, generated) = inner.finish();
        self.record(path.clone(), insts, generated);

        self.insts.push(Inst::Call { path });
        if escaping.breaks {
            self.consume(CTL_BREAK);
        }
        if escaping.returns {
            self.propagate();
        }
    }

    fn loop_stmt(&mut self, cond: Option<&hir::Expr>, body: &[hir::Stmt]) {
        let escaping = escapes(body);
        let name = if cond.is_some() { "while" } else { "loop" };
        let path = format!("{}/{name}_{}", self.prefix, self.counter);
        self.counter += 1;

        let mut inner = self.child(path.clone());
        if let Some(cond) = cond {
            let cond = inner.cond(cond);
            inner.insts.push(Inst::Guarded {
                cond: cond.negate(),
                inst: Box::new(Inst::Return { value: 0 }),
            });
        }
        if escaping.continues {
            // `continue` returns, and a return from the loop function would end the
            // loop. Give the body its own function so the tail call still happens.
            let body_path = inner.split("body", body);
            inner.insts.push(Inst::Call { path: body_path });
            inner.consume(CTL_CONTINUE);
            inner.propagate();
        } else {
            for stmt in body {
                inner.stmt(stmt);
            }
        }
        inner.insts.push(Inst::Call { path: path.clone() });
        let (insts, generated) = inner.finish();
        self.record(path.clone(), insts, generated);

        self.insts.push(Inst::Call { path });
        if escaping.breaks {
            self.consume(CTL_BREAK);
        }
        if escaping.returns {
            self.propagate();
        }
    }

    /// Splits an expression into its own function, evaluated into `dst`. Used for the
    /// right-hand side of a short-circuiting operator.
    fn split_expr(&mut self, kind: &str, dst: Reg, expr: &hir::Expr) -> String {
        let path = format!("{}/{kind}_{}", self.prefix, self.counter);
        self.counter += 1;
        let mut inner = self.child(path.clone());
        let value = inner.expr(expr);
        inner.copy_into(dst, value);
        let (insts, generated) = inner.finish();
        self.record(path.clone(), insts, generated);
        path
    }

    /// Splits a statement list into its own function and returns its path.
    fn split(&mut self, kind: &str, stmts: &[hir::Stmt]) -> String {
        let path = format!("{}/{kind}_{}", self.prefix, self.counter);
        self.counter += 1;
        let mut inner = self.child(path.clone());
        for stmt in stmts {
            inner.stmt(stmt);
        }
        let (insts, generated) = inner.finish();
        self.record(path.clone(), insts, generated);
        path
    }

    /// Hands a control transfer upwards.
    ///
    /// Inside a generated block that means returning so the parent's own guard sees
    /// it. At a function's top level there is nowhere left to pass it to, so a pending
    /// `return` becomes the real one, carrying the value out of `$<fn>.ret`.
    fn propagate(&mut self) {
        let ctl = self.ctl();
        if !self.top_level {
            self.insts.push(Inst::Guarded {
                cond: Cond::Matches {
                    src: ctl,
                    min: Some(CTL_BREAK),
                    max: None,
                    negated: false,
                },
                inst: Box::new(Inst::Return { value: 0 }),
            });
            return;
        }
        // A return that reports failure carries its own code, so the two are told
        // apart without a second register (spec section 6.28).
        if matches!(self.function.ret, Some(Type::Option(_))) {
            let ctl = self.ctl();
            self.insts.push(Inst::Guarded {
                cond: Cond::Matches {
                    src: ctl,
                    min: Some(CTL_FAIL),
                    max: Some(CTL_FAIL),
                    negated: false,
                },
                inst: Box::new(Inst::ReturnFail),
            });
        }
        // `break` and `continue` cannot reach here: HIR rejects them outside a loop,
        // and a loop consumes its own. Only a `return` is left.
        let inst = match self.function.ret {
            Some(_) => Inst::ReturnRun {
                inst: Box::new(Inst::Get {
                    src: self.ret_reg(),
                }),
            },
            None => Inst::Return { value: 0 },
        };
        self.insts.push(Inst::Guarded {
            cond: Cond::Matches {
                src: ctl,
                min: Some(CTL_RETURN),
                max: Some(CTL_RETURN),
                negated: false,
            },
            inst: Box::new(inst),
        });
    }

    /// Clears one control code, because this is the construct it was meant for.
    fn consume(&mut self, code: i32) {
        let ctl = self.ctl();
        self.insts.push(Inst::Guarded {
            cond: Cond::Matches {
                src: ctl.clone(),
                min: Some(code),
                max: Some(code),
                negated: false,
            },
            inst: Box::new(Inst::Const {
                dst: ctl,
                value: CTL_NORMAL,
            }),
        });
    }

    fn ctl(&mut self) -> Reg {
        self.program.used_ctl = true;
        Reg {
            holder: format!("${}.ctl", self.function.name),
            kind: RegKind::Var,
        }
    }

    /// A condition, written straight into the `execute` where possible.
    /// Emits a call, returning where its result landed when one was wanted.
    fn call(&mut self, callee: FnId, args: &[hir::Expr], capture: Capture) -> Option<Called> {
        let recursive = self.program.same_component(self.function.id, callee);
        let callee_fn = &self.program.functions[callee.0 as usize];

        // Arguments are evaluated before anything is saved: they are expressions in
        // the caller's frame and must be read while that frame is still intact.
        // A composite argument is written straight into the callee's storage path
        // below; there is no register to hold it in the meantime.
        let values: Vec<Option<Value>> = args
            .iter()
            .map(|arg| (!arg.ty.is_storage()).then(|| self.expr(arg)))
            .collect();

        let saved = if recursive {
            let saved = self.live_slots();
            self.insts.push(Inst::PushFrame);
            for (slot, place) in saved.iter().enumerate() {
                let slot = slot as u32;
                self.insts.push(match place {
                    Slot::Score(reg) => Inst::Save {
                        reg: reg.clone(),
                        slot,
                    },
                    Slot::Data(path) => Inst::SaveData {
                        path: path.clone(),
                        slot,
                    },
                });
            }
            saved
        } else {
            Vec::new()
        };

        for ((value, arg), param) in values.into_iter().zip(args).zip(&callee_fn.params) {
            match value {
                Some(value) => self.copy_into(param_reg(callee_fn, *param), value),
                None => {
                    let path = local_path(callee_fn, *param);
                    self.store_struct(&path.into(), arg);
                }
            }
        }

        let call = Inst::Call {
            path: callee_fn.path.clone(),
        };
        // Allocated after the saves, so restoring cannot clobber them.
        let result = match capture {
            Capture::Nothing => None,
            Capture::Result => Some(Called {
                value: self.program.temps.next(),
                present: None,
            }),
            // An option-returning function answers in both halves at once: the value
            // in the result, whether there was one in the success (spec section 6.28).
            Capture::Option => Some(Called {
                value: self.program.temps.next(),
                present: Some(self.program.temps.next()),
            }),
        };
        match &result {
            Some(Called {
                value,
                present: Some(ok),
            }) => self.insts.push(Inst::StoreBoth {
                ok: ok.clone(),
                dst: value.clone(),
                inst: Box::new(call),
            }),
            Some(Called { value, .. }) => self.insts.push(Inst::StoreResult {
                dst: value.clone(),
                inst: Box::new(call),
            }),
            None => self.insts.push(call),
        }

        if recursive {
            for (slot, place) in saved.iter().enumerate() {
                let slot = slot as u32;
                self.insts.push(match place {
                    Slot::Score(reg) => Inst::Restore {
                        reg: reg.clone(),
                        slot,
                    },
                    Slot::Data(path) => Inst::RestoreData {
                        path: path.clone(),
                        slot,
                    },
                });
            }
            self.insts.push(Inst::PopFrame);
        }
        result
    }

    /// Everything this function might still need after a call comes back.
    ///
    /// Every local, plus every temporary handed out so far, each from wherever its
    /// type actually lives. Narrowing this to what is live is the liveness analysis's
    /// job later; until then the set that is obviously sufficient is the right one.
    fn live_slots(&self) -> Vec<Slot> {
        let locals = self.program.initialised.iter().map(|local| {
            if self.local_ty(*local).is_storage() {
                Slot::Data(local_path(self.function, *local))
            } else {
                Slot::Score(local_reg(self.function, *local))
            }
        });
        let temps = self.program.used.iter().cloned().map(Slot::Score);
        let data = self.program.used_data.iter().cloned().map(Slot::Data);
        locals.chain(temps).chain(data).collect()
    }

    fn cond(&mut self, expr: &hir::Expr) -> Cond {
        match &expr.kind {
            hir::ExprKind::Unary(UnaryOp::Not, inner) => self.cond(inner).negate(),
            // `execute if data` is a condition already: asking it into a register and
            // then asking about the register would be a command wasted.
            hir::ExprKind::Binary(op, lhs, rhs) if lhs.ty == Type::Str => {
                match self.string_match(*op, lhs, rhs) {
                    Ok(Match::Here(cond)) => cond,
                    // A spliced match runs inside a macro function, which has to put
                    // its answer somewhere before this condition can read it.
                    Ok(matched) => {
                        let dst = self.temps_next();
                        self.store_match(dst.clone(), matched);
                        Cond::Matches {
                            src: dst,
                            min: Some(1),
                            max: Some(1),
                            negated: false,
                        }
                    }
                    Err(answer) => {
                        let src = self.materialise(Value::Const(answer));
                        Cond::Matches {
                            src,
                            min: Some(1),
                            max: Some(1),
                            negated: false,
                        }
                    }
                }
            }
            hir::ExprKind::Binary(op, lhs, rhs) if is_comparison(*op) => {
                let lhs = self.expr(lhs);
                let rhs = self.expr(rhs);
                if let (Value::Reg(src), Value::Const(n)) = (&lhs, &rhs)
                    && let Some((min, max, negated)) = range_for(*op, *n)
                {
                    return Cond::Matches {
                        src: src.clone(),
                        min,
                        max,
                        negated,
                    };
                }
                let lhs = self.materialise(lhs);
                let rhs = self.materialise(rhs);
                let (cmp, negated) = match op {
                    BinaryOp::Eq => (Cmp::Eq, false),
                    BinaryOp::Ne => (Cmp::Eq, true),
                    BinaryOp::Lt => (Cmp::Lt, false),
                    BinaryOp::Le => (Cmp::Le, false),
                    BinaryOp::Gt => (Cmp::Gt, false),
                    BinaryOp::Ge => (Cmp::Ge, false),
                    _ => unreachable!("not a comparison"),
                };
                Cond::Score {
                    lhs,
                    cmp,
                    rhs,
                    negated,
                }
            }
            _ => {
                let value = self.expr(expr);
                let src = self.materialise(value);
                Cond::Matches {
                    src,
                    min: Some(1),
                    max: Some(1),
                    negated: false,
                }
            }
        }
    }

    fn expr(&mut self, expr: &hir::Expr) -> Value {
        match &expr.kind {
            // A component is JSON inside a command's text; it never reaches a register.
            hir::ExprKind::Component(_) => unreachable!("a component is part of a command"),
            hir::ExprKind::Int(n) => Value::Const(*n),
            hir::ExprKind::Bool(b) => Value::Const(i32::from(*b)),
            hir::ExprKind::Local(local) => Value::Reg(self.local(*local)),
            hir::ExprKind::Unary(op, operand) => self.unary(*op, operand),
            hir::ExprKind::Binary(op, lhs, rhs) => self.binary(*op, lhs, rhs),
            hir::ExprKind::Call { callee, args } => Value::Reg(
                self.call(*callee, args, Capture::Result)
                    .expect("a value was wanted")
                    .value,
            ),
            // A command as an expression is a command that ran; its value is not
            // captured, so this is the statement form reaching here by mistake.
            hir::ExprKind::Command(text) => {
                self.insts.push(Inst::Raw {
                    text: text.clone(),
                    span: expr.span,
                });
                Value::Const(1)
            }
            // Compile-time values never reach a register: HIR only lets them be handed
            // to the constructs that consume them while compiling.
            hir::ExprKind::Selector(_) | hir::ExprKind::Resource(_) | hir::ExprKind::Pos(_) => {
                unreachable!("a {} has no runtime value", expr.ty.name())
            }
            hir::ExprKind::Str(_) => unreachable!("strings have no runtime value until M8"),
            // Composite values live in storage; every path that produces one goes
            // through `store_struct`, which never asks for a register.
            hir::ExprKind::Struct { .. } | hir::ExprKind::Enum { .. } => {
                unreachable!("a composite value is not a register value")
            }
            // Reading a scalar out of storage: one command, straight into a register.
            hir::ExprKind::Field(place) | hir::ExprKind::Len(place) => {
                let dst = self.temps_next();
                self.read_place_into(dst.clone(), place);
                Value::Reg(dst)
            }
            hir::ExprKind::List { .. } => unreachable!("a list is not a register value"),
            hir::ExprKind::ReadScaled { place, scale } => {
                let dst = self.temps_next();
                self.read_place_scaled(dst.clone(), place, *scale);
                Value::Reg(dst)
            }
            // `expect` is `?` without the leaving: ask, say so in a debug build, read.
            hir::ExprKind::Expect { value, message } => {
                let path = option_path(self.function, value).expect("an option this can read");
                if self.program.debug {
                    let present = self.present_at(&path);
                    self.insts.push(Inst::Guarded {
                        cond: Cond::Matches {
                            src: present,
                            min: Some(0),
                            max: Some(0),
                            negated: false,
                        },
                        inst: Box::new(Inst::Report {
                            message: message.clone(),
                            span: expr.span,
                        }),
                    });
                }
                let dst = self.temps_next();
                self.insts.push(Inst::StoreResult {
                    dst: dst.clone(),
                    inst: Box::new(Inst::GetData { path }),
                });
                Value::Reg(dst)
            }
            hir::ExprKind::Try(inner) => {
                // Ask first, leave if there was nothing, and only then read: on the
                // way out the read would be a command spent on a value nobody sees.
                if let Some(path) = option_path(self.function, inner) {
                    let present = self.present_at(&path);
                    self.leave_if_absent(present);
                    let value = self.temps_next();
                    self.insts.push(Inst::StoreResult {
                        dst: value.clone(),
                        inst: Box::new(Inst::GetData { path }),
                    });
                    return Value::Reg(value);
                }
                let (value, present) = self.option_parts(inner);
                self.leave_if_absent(present);
                Value::Reg(value)
            }
            // The value only means anything once it is in storage, and everything
            // that puts it there goes through `store_struct`.
            hir::ExprKind::AsNbt { .. } => unreachable!("an NBT scalar is not a register value"),
            hir::ExprKind::Slice { .. }
            | hir::ExprKind::Nbt(_)
            | hir::ExprKind::Some(_)
            | hir::ExprKind::None => {
                unreachable!("a compound is not a register value")
            }
            // A view is a name, and names are settled while compiling.
            hir::ExprKind::View(_) => unreachable!("a view has no runtime value"),
        }
    }

    fn unary(&mut self, op: UnaryOp, operand: &hir::Expr) -> Value {
        let value = self.expr(operand);
        if !self.program.debug
            && let Value::Const(n) = &value
        {
            return Value::Const(match op {
                UnaryOp::Neg => n.wrapping_neg(),
                UnaryOp::Not => i32::from(*n == 0),
            });
        }
        match op {
            UnaryOp::Neg => {
                let dst = self.temps_next();
                self.insts.push(Inst::Const {
                    dst: dst.clone(),
                    value: 0,
                });
                let src = self.materialise(value);
                self.insts.push(Inst::Op {
                    dst: dst.clone(),
                    op: Op::Sub,
                    src,
                });
                Value::Reg(dst)
            }
            UnaryOp::Not => {
                let src = self.materialise(value);
                let dst = self.temps_next();
                self.insts.push(Inst::Matches {
                    dst: dst.clone(),
                    src,
                    min: Some(0),
                    max: Some(0),
                    negated: false,
                });
                Value::Reg(dst)
            }
        }
    }

    /// Both sides known while compiling, so the answer is too (spec section 6.33).
    ///
    /// The arithmetic is vanilla's rather than Rust's — floor division, wrapping
    /// overflow — because that is what the commands this replaces would have done.
    fn fold(&self, op: BinaryOp, lhs: &Value, rhs: &Value) -> Option<Value> {
        use BinaryOp::*;
        // Debug keeps source and output one to one (requirements section 15).
        if self.program.debug {
            return None;
        }
        let (Value::Const(a), Value::Const(b)) = (lhs, rhs) else {
            return None;
        };
        let (a, b) = (*a, *b);
        Some(Value::Const(match op {
            Add => a.wrapping_add(b),
            Sub => a.wrapping_sub(b),
            Mul => a.wrapping_mul(b),
            // Vanilla fails and leaves the target unchanged, so there is no answer to
            // fold to; letting it happen at runtime keeps the behaviour.
            Div | Rem if b == 0 => return None,
            Div => floor_div(a, b),
            Rem => a.wrapping_sub(floor_div(a, b).wrapping_mul(b)),
            // For 0/1 values min is and, max is or, which is how they are lowered.
            And => a.min(b),
            Or => a.max(b),
            Eq => i32::from(a == b),
            Ne => i32::from(a != b),
            Lt => i32::from(a < b),
            Le => i32::from(a <= b),
            Gt => i32::from(a > b),
            Ge => i32::from(a >= b),
        }))
    }

    fn binary(&mut self, op: BinaryOp, lhs: &hir::Expr, rhs: &hir::Expr) -> Value {
        use BinaryOp::*;
        // Strings are matched, not compared: there is no `scoreboard` value to put
        // one in (spec section 6.27).
        if lhs.ty == Type::Str {
            return self.string_compare(op, lhs, rhs, None);
        }
        let is_bool = lhs.ty == Type::Bool;
        match op {
            And | Or if !is_pure(rhs) => {
                // Spec section 6.14. With a call on the right, short-circuiting is
                // observable, so it has to actually happen.
                let lhs = self.expr(lhs);
                let dst = self.temps_next();
                self.copy_into(dst.clone(), lhs);
                let wanted = if op == And { 1 } else { 0 };
                let name = if op == And { "and" } else { "or" };
                let path = self.split_expr(name, dst.clone(), rhs);
                self.insts.push(Inst::Guarded {
                    cond: Cond::Matches {
                        src: dst.clone(),
                        min: Some(wanted),
                        max: Some(wanted),
                        negated: false,
                    },
                    inst: Box::new(Inst::Call { path }),
                });
                Value::Reg(dst)
            }
            Add | Sub | Mul | Div | Rem | And | Or => {
                let lhs = self.expr(lhs);
                let rhs = self.expr(rhs);
                if let Some(folded) = self.fold(op, &lhs, &rhs) {
                    return folded;
                }
                let dst = self.temps_next();
                self.copy_into(dst.clone(), lhs);
                match (op, &rhs) {
                    (Add, Value::Const(n)) => self.insts.push(Inst::AddConst {
                        dst: dst.clone(),
                        value: *n,
                    }),
                    (Sub, Value::Const(n)) => self.insts.push(Inst::AddConst {
                        dst: dst.clone(),
                        value: n.wrapping_neg(),
                    }),
                    _ => {
                        let src = self.materialise(rhs);
                        self.insts.push(Inst::Op {
                            dst: dst.clone(),
                            op: arith(op),
                            src,
                        });
                    }
                }
                Value::Reg(dst)
            }
            Eq | Ne | Lt | Le | Gt | Ge => {
                let _ = is_bool;
                let lhs = self.expr(lhs);
                let rhs = self.expr(rhs);
                if let Some(folded) = self.fold(op, &lhs, &rhs) {
                    return folded;
                }
                let dst = self.temps_next();
                self.compare_into(dst.clone(), op, lhs, rhs);
                Value::Reg(dst)
            }
        }
    }

    /// A comparison against a constant becomes a `matches` range, which is one command
    /// rather than two. Ranges cannot express every bound without overflowing, so the
    /// general path stays available.
    fn compare_into(&mut self, dst: Reg, op: BinaryOp, lhs: Value, rhs: Value) {
        use BinaryOp::*;
        if let (Value::Reg(src), Value::Const(n)) = (&lhs, &rhs)
            && let Some((min, max, negated)) = range_for(op, *n)
        {
            self.insts.push(Inst::Matches {
                dst,
                src: src.clone(),
                min,
                max,
                negated,
            });
            return;
        }
        let lhs = self.materialise(lhs);
        let rhs = self.materialise(rhs);
        let (cmp, negated) = match op {
            Eq => (Cmp::Eq, false),
            Ne => (Cmp::Eq, true),
            Lt => (Cmp::Lt, false),
            Le => (Cmp::Le, false),
            Gt => (Cmp::Gt, false),
            Ge => (Cmp::Ge, false),
            _ => unreachable!("not a comparison"),
        };
        self.insts.push(Inst::Cmp {
            dst,
            cmp,
            negated,
            lhs,
            rhs,
        });
    }

    fn copy_into(&mut self, dst: Reg, value: Value) {
        match value {
            Value::Const(n) => self.insts.push(Inst::Const { dst, value: n }),
            Value::Reg(src) => self.insts.push(Inst::Op {
                dst,
                op: Op::Assign,
                src,
            }),
        }
    }

    /// Puts a value in a register, spending a command only if it was a constant.
    fn materialise(&mut self, value: Value) -> Reg {
        match value {
            Value::Reg(reg) => reg,
            Value::Const(n) => {
                let dst = self.temps_next();
                self.insts.push(Inst::Const {
                    dst: dst.clone(),
                    value: n,
                });
                dst
            }
        }
    }

    fn local(&self, local: LocalId) -> Reg {
        local_reg(self.function, local)
    }

    fn temps_next(&mut self) -> Reg {
        let reg = self.program.temps.next();
        self.program.used.push(reg.clone());
        reg
    }

    fn data_temp(&mut self) -> String {
        let path = self.program.temps.next_data();
        self.program.used_data.push(path.clone());
        path
    }

    fn iter_temp(&mut self) -> String {
        let path = self.program.temps.next_iter();
        self.program.used_data.push(path.clone());
        path
    }

    /// A lowering context for a block split out of this one.
    fn child(&mut self, prefix: String) -> Lowering<'_, 'p> {
        Lowering {
            function: self.function,
            program: self.program,
            insts: Vec::new(),
            generated: Vec::new(),
            prefix,
            counter: 0,
            top_level: false,
            in_entity_body: self.in_entity_body,
            entity_body_root: false,
        }
    }

    /// Consumes a child context, releasing its borrow of the shared state.
    fn finish(self) -> (Vec<Inst>, Vec<Function>) {
        (self.insts, self.generated)
    }

    fn record(&mut self, path: String, insts: Vec<Inst>, generated: Vec<Function>) {
        self.generated.push(Function {
            id: self.function.id,
            path,
            attrs: Vec::new(),
            blocks: vec![Block {
                id: BlockId(0),
                insts,
            }],
        });
        self.generated.extend(generated);
    }
}

/// `{tag:"Idle"}`, the filter that picks one variant out of a compound.
fn tag_filter(tag: &str) -> String {
    format!("{{{TAG_KEY}:\"{tag}\"}}")
}

fn is_comparison(op: BinaryOp) -> bool {
    use BinaryOp::*;
    matches!(op, Eq | Ne | Lt | Le | Gt | Ge)
}

/// Vanilla floors rather than truncating: `-7 / 2` is `-4`
/// (`crates/tinymcf/SPEC.md` section 6).
///
/// Spelled out again here rather than shared: `mwlc` does not depend on `tinymcf`.
fn floor_div(a: i32, b: i32) -> i32 {
    let quotient = a.wrapping_div(b);
    if a.wrapping_rem(b) != 0 && (a < 0) != (b < 0) {
        quotient - 1
    } else {
        quotient
    }
}

fn arith(op: BinaryOp) -> Op {
    match op {
        BinaryOp::Add => Op::Add,
        BinaryOp::Sub => Op::Sub,
        BinaryOp::Mul => Op::Mul,
        BinaryOp::Div => Op::Div,
        BinaryOp::Rem => Op::Rem,
        // For 0/1 values, min is and, max is or. Spec section 6.3 explains why not
        // short-circuiting is correct while M2 expressions are pure.
        BinaryOp::And => Op::Min,
        BinaryOp::Or => Op::Max,
        other => unreachable!("{other:?} is not arithmetic"),
    }
}

/// The `matches` range for `<reg> <op> <n>`, or `None` when the bound would overflow.
fn range_for(op: BinaryOp, n: i32) -> Option<(Option<i32>, Option<i32>, bool)> {
    use BinaryOp::*;
    Some(match op {
        Eq => (Some(n), Some(n), false),
        Ne => (Some(n), Some(n), true),
        Lt => (None, Some(n.checked_sub(1)?), false),
        Le => (None, Some(n), false),
        Gt => (Some(n.checked_add(1)?), None, false),
        Ge => (Some(n), None, false),
        _ => return None,
    })
}
