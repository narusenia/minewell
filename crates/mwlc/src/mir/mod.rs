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

use crate::hir::{self, FnId, Hir, LocalId, Type};
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
    /// `return <value>`
    Return { value: i32 },
    /// `execute <cond> run <inst>`. Still one command, so still one instruction.
    Guarded { cond: Cond, inst: Box<Inst> },
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

/// The control register's values. See spec section 6.10.
const CTL_NORMAL: i32 = 0;
const CTL_BREAK: i32 = 1;
const CTL_CONTINUE: i32 = 2;
const CTL_RETURN: i32 = 3;

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

/// What an expression produced: either a number known now, or a register holding it.
///
/// Keeping constants unmaterialised until the last moment is what lets `let x = 5;`
/// be one `players set` instead of a `set` into a temporary and an `operation =`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Const(i32),
    Reg(Reg),
}

pub fn lower(hir: &Hir) -> Mir {
    let components = strongly_connected(hir);
    let mut program = Program {
        functions: &hir.functions,
        components,
        temps: Temps::default(),
        used: Vec::new(),
        used_ctl: false,
        initialised: Vec::new(),
    };
    let mut functions = Vec::new();
    for f in &hir.functions {
        program.used.clear();
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
    Mir { functions }
}

/// Temporary names, counted across the whole program.
///
/// A name is therefore never reused, so no two temporaries can be live at once under
/// the same name and correctness needs no liveness analysis. Shrinking this is M9-7's
/// job; until then the naive version is the one that is obviously right.
#[derive(Debug, Default)]
struct Temps(u32);

impl Temps {
    fn next(&mut self) -> Reg {
        let reg = Reg {
            holder: format!("$t{}", self.0),
            kind: RegKind::Temp,
        };
        self.0 += 1;
        reg
    }
}

/// State shared by every block of one program.
struct Program<'a> {
    functions: &'a [hir::Function],
    /// Which strongly connected component each function belongs to. Two functions in
    /// the same one can reach each other, so a call between them is recursive.
    components: Vec<u32>,
    temps: Temps,
    /// Temporaries handed out in the function being lowered, in order.
    used: Vec<Reg>,
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
        // Qualified by function so two functions' locals cannot collide.
        holder: format!(
            "${}.{}",
            function.name, function.locals[local.0 as usize].name
        ),
        kind: RegKind::Var,
    }
}

fn param_reg(function: &hir::Function, local: LocalId) -> Reg {
    local_reg(function, local)
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
                self.call(*callee, args, false);
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
            hir::Stmt::Raw(raw) => self.insts.push(Inst::Raw {
                text: raw.text.clone(),
                span: raw.span,
            }),
            hir::Stmt::Let { local, value, .. } => {
                let dst = self.local(*local);
                self.store(dst, None, value);
                // Only now: a call inside `value` must not try to save this local,
                // which has nothing in it yet.
                self.program.initialised.push(*local);
            }
            hir::Stmt::Assign {
                local, op, value, ..
            } => {
                let dst = self.local(*local);
                self.store(dst, *op, value);
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
    fn call(&mut self, callee: FnId, args: &[hir::Expr], want_result: bool) -> Option<Reg> {
        let recursive = self.program.same_component(self.function.id, callee);
        let callee_fn = &self.program.functions[callee.0 as usize];

        // Arguments are evaluated before anything is saved: they are expressions in
        // the caller's frame and must be read while that frame is still intact.
        let values: Vec<Value> = args.iter().map(|arg| self.expr(arg)).collect();

        let saved = if recursive {
            let saved = self.live_registers();
            self.insts.push(Inst::PushFrame);
            for (slot, reg) in saved.iter().enumerate() {
                self.insts.push(Inst::Save {
                    reg: reg.clone(),
                    slot: slot as u32,
                });
            }
            saved
        } else {
            Vec::new()
        };

        for (value, param) in values.into_iter().zip(&callee_fn.params) {
            let dst = param_reg(callee_fn, *param);
            self.copy_into(dst, value);
        }

        let call = Inst::Call {
            path: callee_fn.path.clone(),
        };
        // Allocated after the saves, so restoring cannot clobber it.
        let result = want_result.then(|| self.program.temps.next());
        match &result {
            Some(dst) => self.insts.push(Inst::StoreResult {
                dst: dst.clone(),
                inst: Box::new(call),
            }),
            None => self.insts.push(call),
        }

        if recursive {
            for (slot, reg) in saved.iter().enumerate() {
                self.insts.push(Inst::Restore {
                    reg: reg.clone(),
                    slot: slot as u32,
                });
            }
            self.insts.push(Inst::PopFrame);
        }
        result
    }

    /// Everything this function might still need after a call comes back.
    ///
    /// Every local, plus every temporary handed out so far. Narrowing this to what is
    /// actually live is M9-7's liveness analysis; until then the set that is obviously
    /// sufficient is the right one.
    fn live_registers(&self) -> Vec<Reg> {
        let locals = self
            .program
            .initialised
            .iter()
            .map(|local| local_reg(self.function, *local));
        locals.chain(self.program.used.iter().cloned()).collect()
    }

    fn cond(&mut self, expr: &hir::Expr) -> Cond {
        match &expr.kind {
            hir::ExprKind::Unary(UnaryOp::Not, inner) => self.cond(inner).negate(),
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
            hir::ExprKind::Int(n) => Value::Const(*n),
            hir::ExprKind::Bool(b) => Value::Const(i32::from(*b)),
            hir::ExprKind::Local(local) => Value::Reg(self.local(*local)),
            hir::ExprKind::Unary(op, operand) => self.unary(*op, operand),
            hir::ExprKind::Binary(op, lhs, rhs) => self.binary(*op, lhs, rhs),
            hir::ExprKind::Call { callee, args } => {
                Value::Reg(self.call(*callee, args, true).expect("a value was wanted"))
            }
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
        }
    }

    fn unary(&mut self, op: UnaryOp, operand: &hir::Expr) -> Value {
        let value = self.expr(operand);
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

    fn binary(&mut self, op: BinaryOp, lhs: &hir::Expr, rhs: &hir::Expr) -> Value {
        use BinaryOp::*;
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

fn is_comparison(op: BinaryOp) -> bool {
    use BinaryOp::*;
    matches!(op, Eq | Ne | Lt | Le | Gt | Ge)
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
