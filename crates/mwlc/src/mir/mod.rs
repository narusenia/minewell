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
    /// `return <value>`
    Return { value: i32 },
    /// `execute <cond> run <inst>`. Still one command, so still one instruction.
    Guarded { cond: Cond, inst: Box<Inst> },
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
            hir::Stmt::Return(_) => Escapes {
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
    let mut temps = Temps::default();
    let mut functions = Vec::new();
    for f in &hir.functions {
        let mut cx = Lowering {
            function: f,
            insts: Vec::new(),
            temps: &mut temps,
            generated: Vec::new(),
            prefix: f.path.clone(),
            counter: 0,
        };
        for stmt in &f.body {
            cx.stmt(stmt);
        }
        let (insts, generated) = (cx.insts, cx.generated);
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
/// A name is therefore never reused, so no two temporaries can ever be live at once
/// under the same name and correctness needs no liveness analysis. Shrinking this is
/// M9-7's job; until then the naive version is the one that is obviously right.
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

struct Lowering<'a> {
    function: &'a hir::Function,
    insts: Vec<Inst>,
    temps: &'a mut Temps,
    /// Functions split out of this one. Named under `prefix`, so the output stays
    /// walkable (requirements section 12.2).
    generated: Vec<Function>,
    prefix: String,
    counter: u32,
}

impl Lowering<'_> {
    fn stmt(&mut self, stmt: &hir::Stmt) {
        match stmt {
            hir::Stmt::Break(_) => self.jump(CTL_BREAK),
            hir::Stmt::Continue(_) => self.jump(CTL_CONTINUE),
            hir::Stmt::Return(_) => self.jump(CTL_RETURN),
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

    /// `break`, `continue` and `return` all leave the same way: record why in the
    /// control register, then return. Only `return` survives past the enclosing loop.
    fn jump(&mut self, code: i32) {
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

    fn loop_stmt(&mut self, cond: Option<&hir::Expr>, body: &[hir::Stmt]) {
        let escaping = escapes(body);
        let name = if cond.is_some() { "while" } else { "loop" };
        let path = format!("{}/{name}_{}", self.prefix, self.counter);
        self.counter += 1;

        let mut inner = Lowering {
            function: self.function,
            insts: Vec::new(),
            temps: self.temps,
            generated: Vec::new(),
            prefix: path.clone(),
            counter: 0,
        };
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

        let generated = std::mem::take(&mut inner.generated);
        let insts = std::mem::take(&mut inner.insts);
        self.generated.push(Function {
            id: self.function.id,
            path: path.clone(),
            attrs: Vec::new(),
            blocks: vec![Block {
                id: BlockId(0),
                insts,
            }],
        });
        self.generated.extend(generated);

        self.insts.push(Inst::Call { path });
        if escaping.breaks {
            self.consume(CTL_BREAK);
        }
        if escaping.returns {
            self.propagate();
        }
    }

    /// Splits a statement list into its own function and returns its path.
    fn split(&mut self, kind: &str, stmts: &[hir::Stmt]) -> String {
        let path = format!("{}/{kind}_{}", self.prefix, self.counter);
        self.counter += 1;
        let mut inner = Lowering {
            function: self.function,
            insts: Vec::new(),
            temps: self.temps,
            generated: Vec::new(),
            prefix: path.clone(),
            counter: 0,
        };
        for stmt in stmts {
            inner.stmt(stmt);
        }
        let generated = std::mem::take(&mut inner.generated);
        let insts = std::mem::take(&mut inner.insts);
        self.generated.push(Function {
            id: self.function.id,
            path: path.clone(),
            attrs: Vec::new(),
            blocks: vec![Block {
                id: BlockId(0),
                insts,
            }],
        });
        self.generated.extend(generated);
        path
    }

    /// `execute if score $ctl matches 1.. run return 0` — hand the transfer upwards.
    fn propagate(&mut self) {
        let ctl = self.ctl();
        self.insts.push(Inst::Guarded {
            cond: Cond::Matches {
                src: ctl,
                min: Some(CTL_BREAK),
                max: None,
                negated: false,
            },
            inst: Box::new(Inst::Return { value: 0 }),
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

    fn ctl(&self) -> Reg {
        Reg {
            holder: format!("${}.ctl", self.function.name),
            kind: RegKind::Var,
        }
    }

    /// A condition, written straight into the `execute` where possible.
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
        }
    }

    fn unary(&mut self, op: UnaryOp, operand: &hir::Expr) -> Value {
        let value = self.expr(operand);
        match op {
            UnaryOp::Neg => {
                let dst = self.temps.next();
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
                let dst = self.temps.next();
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
            Add | Sub | Mul | Div | Rem | And | Or => {
                let lhs = self.expr(lhs);
                let rhs = self.expr(rhs);
                let dst = self.temps.next();
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
                let dst = self.temps.next();
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
                let dst = self.temps.next();
                self.insts.push(Inst::Const {
                    dst: dst.clone(),
                    value: n,
                });
                dst
            }
        }
    }

    fn local(&self, local: LocalId) -> Reg {
        let name = &self.function.locals[local.0 as usize].name;
        Reg {
            // Qualified by function so that two functions' locals cannot collide once
            // calls exist (M4).
            holder: format!("${}.{name}", self.function.name),
            kind: RegKind::Var,
        }
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
