// SPDX-License-Identifier: MIT

//! Running commands against a [`World`].
//!
//! See `SPEC.md` §2: a command that fails does not abort anything. Execution walks on
//! to the next line and the failure is recorded, because that is what vanilla does and
//! because the whole point of this interpreter is that nothing fails silently.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::args::ParseError;
use crate::command::{
    Clause, Cmp, Command, Condition, Data, Execute, FnArgs, ModifyKind, NumTag, Op, Return,
    ScoreTest, Scoreboard, Source, StoreTarget, Target,
};
use crate::nbt::{Compound, NbtValue};
use crate::path::NbtPath;
use crate::world::World;

/// Replaces every `$(name)` with its argument.
fn substitute(text: &str, args: &Compound) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("$(") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find(')')
            .ok_or_else(|| format!("unclosed '$(' in macro line: {text}"))?;
        let name = &after[..end];
        let value = args
            .get(name)
            .ok_or_else(|| format!("no argument named '{name}' was supplied"))?;
        out.push_str(&render(value));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// How vanilla renders an argument into a command line: a string contributes its
/// characters, a number its digits with no tag suffix, anything else its SNBT.
fn render(value: &NbtValue) -> String {
    match value {
        NbtValue::String(s) => s.clone(),
        NbtValue::Byte(v) => v.to_string(),
        NbtValue::Short(v) => v.to_string(),
        NbtValue::Int(v) => v.to_string(),
        NbtValue::Long(v) => v.to_string(),
        NbtValue::Float(v) => v.to_string(),
        NbtValue::Double(v) => v.to_string(),
        other => other.to_string(),
    }
}

fn deferred(what: &str) -> String {
    format!("'{what}' is not implemented yet; see SPEC.md section 4.4 (M0-8b)")
}

fn unmodelled(target: &Target) -> String {
    format!(
        "{} targets are not modelled by tinymcf; see SPEC.md section 1",
        target.describe()
    )
}

/// Why a read produced nothing.
///
/// `None` is an ordinary absence, and carries no diagnostic: a path that is not there
/// is an answer in vanilla's data model, the same way an unset score is (SPEC.md
/// section 4.2). It is what `Option<T>` is made of in the source language.
type ReadError = Option<String>;

/// Vanilla refuses a source or a `data get` that is ambiguous.
fn exactly_one(matches: &[NbtValue]) -> Result<NbtValue, ReadError> {
    match matches {
        [one] => Ok(one.clone()),
        [] => Err(None),
        many => Err(Some(format!(
            "found {} elements matching the path",
            many.len()
        ))),
    }
}

/// What `data get` reports for a value, before scaling.
fn measure(value: &NbtValue) -> Option<f64> {
    Some(match value {
        NbtValue::Byte(v) => *v as f64,
        NbtValue::Short(v) => *v as f64,
        NbtValue::Int(v) => *v as f64,
        NbtValue::Long(v) => *v as f64,
        NbtValue::Float(v) => *v as f64,
        NbtValue::Double(v) => *v,
        NbtValue::String(s) => s.chars().count() as f64,
        NbtValue::List(items) => items.len() as f64,
        NbtValue::Compound(fields) => fields.len() as f64,
        NbtValue::ByteArray(items) => items.len() as f64,
        NbtValue::IntArray(items) => items.len() as f64,
        NbtValue::LongArray(items) => items.len() as f64,
    })
}

/// `[start, end)`, with negative bounds counting from the end.
fn slice(text: &str, start: Option<i32>, end: Option<i32>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len() as i32;
    let resolve = |i: i32| (if i < 0 { len + i } else { i }).clamp(0, len) as usize;
    let from = resolve(start.unwrap_or(0));
    let to = resolve(end.unwrap_or(len));
    if from >= to {
        return String::new();
    }
    chars[from..to].iter().collect()
}

fn apply(slot: &mut NbtValue, kind: ModifyKind, value: &NbtValue) {
    match kind {
        ModifyKind::Set => *slot = value.clone(),
        ModifyKind::Merge => merge_into(slot, value),
        ModifyKind::Append | ModifyKind::Prepend | ModifyKind::Insert(_) => {
            let NbtValue::List(items) = slot else {
                return;
            };
            let at = match kind {
                ModifyKind::Append => items.len(),
                ModifyKind::Prepend => 0,
                ModifyKind::Insert(i) => {
                    let len = items.len() as i32;
                    (if i < 0 { len + i + 1 } else { i }).clamp(0, len) as usize
                }
                _ => unreachable!(),
            };
            items.insert(at, value.clone());
        }
    }
}

/// Recursive compound merge, as `data merge` does it: nested compounds combine rather
/// than replacing one another.
fn merge_into(target: &mut NbtValue, source: &NbtValue) {
    let (NbtValue::Compound(into), NbtValue::Compound(from)) = (target, source) else {
        return;
    };
    for (key, value) in from {
        match into.get_mut(key) {
            Some(existing @ NbtValue::Compound(_)) if matches!(value, NbtValue::Compound(_)) => {
                merge_into(existing, value)
            }
            _ => {
                into.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Java's `Math.floorDiv`, which vanilla uses instead of truncating division.
/// `-7 / 2` is `-4` here and `-3` in Rust.
fn floor_div(a: i32, b: i32) -> i32 {
    let q = a.wrapping_div(b);
    if a.wrapping_rem(b) != 0 && (a < 0) != (b < 0) {
        q - 1
    } else {
        q
    }
}

/// The remainder that goes with [`floor_div`]. `-7 % 2` is `1`.
fn floor_mod(a: i32, b: i32) -> i32 {
    a.wrapping_sub(floor_div(a, b).wrapping_mul(b))
}

/// What `execute store success` and `execute store result` observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub success: u32,
    pub result: i32,
}

impl Outcome {
    pub const FAILED: Outcome = Outcome {
        success: 0,
        result: 0,
    };

    pub fn ok(result: i32) -> Outcome {
        Outcome { success: 1, result }
    }

    pub fn failed(&self) -> bool {
        self.success == 0
    }
}

/// Vanilla's default `maxCommandChainLength`.
pub const DEFAULT_BUDGET: u64 = 65536;

/// How deep function calls may nest. Not a vanilla limit: vanilla's executor is a
/// queue, while this one recurses, so without a cap a runaway recursion overflows the
/// native stack instead of reporting anything.
pub const DEFAULT_MAX_DEPTH: usize = 256;

/// What a run cost. See `SPEC.md` §5 for the accounting rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub commands: u64,
    pub per_function: BTreeMap<String, u64>,
    pub max_depth: usize,
    pub over_budget: bool,
}

/// A command outside the modelled subset, recorded rather than simulated.
#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    pub name: String,
    pub args: String,
    /// Who it ran as, if anyone. Lets a test assert not just that something happened
    /// but who it happened for.
    pub executor: Option<String>,
    /// Where it ran. The arguments keep the coordinates as written (`~ ~1 ~`), so
    /// this is the only place the resolved position shows up.
    pub position: [f64; 3],
}

/// What a command runs in: an executor and a position.
///
/// The dimension and rotation vanilla also carries are absent because nothing here
/// reads them yet.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Context {
    pub executor: Option<String>,
    pub pos: [f64; 3],
    /// Yaw and pitch in degrees, which is what `^` is measured from
    /// (`SPEC.md` section 4.4).
    pub rot: [f64; 2],
}

/// One line of a loaded function.
#[derive(Debug)]
enum Line {
    /// Parsed at load time.
    Plain(Command),
    /// A `$` line. Its text can only be parsed once the arguments are known, so it is
    /// kept verbatim and parsed per call.
    Macro(String),
}

/// How a command left the enclosing function.
enum Flow {
    /// Carry on with the next line.
    Next(Outcome),
    /// `return`: the function ends here with this outcome.
    Return(Outcome),
    /// The command budget ran out. Nothing else runs.
    Halt,
}

#[derive(Debug)]
pub struct Interpreter {
    pub world: World,
    /// The red text vanilla would print. Recorded so a test can assert *why* a command
    /// did nothing.
    pub diagnostics: Vec<String>,
    /// `maxCommandChainLength`. Also what stops a runaway recursion from hanging a test.
    pub budget: u64,
    pub commands_run: u64,
    /// See [`DEFAULT_MAX_DEPTH`].
    pub max_call_depth: usize,
    /// Commands outside the modelled subset, in the order they ran.
    pub effects: Vec<Effect>,
    /// What the command currently running runs in.
    pub context: Context,
    functions: BTreeMap<String, Rc<Vec<Line>>>,
    /// The functions currently executing. Empty at the top level, where `return` is an
    /// error and commands are charged to no function.
    stack: Vec<String>,
    per_function: BTreeMap<String, u64>,
    max_depth: usize,
    over_budget: bool,
}

impl Default for Interpreter {
    fn default() -> Self {
        Interpreter {
            world: World::default(),
            diagnostics: Vec::new(),
            budget: DEFAULT_BUDGET,
            commands_run: 0,
            max_call_depth: DEFAULT_MAX_DEPTH,
            effects: Vec::new(),
            context: Context::default(),
            functions: BTreeMap::new(),
            stack: Vec::new(),
            per_function: BTreeMap::new(),
            max_depth: 0,
            over_budget: false,
        }
    }
}

impl Interpreter {
    pub fn report(&self) -> Report {
        Report {
            commands: self.commands_run,
            per_function: self.per_function.clone(),
            max_depth: self.max_depth,
            over_budget: self.over_budget,
        }
    }

    /// Parses a function body and registers it. Blank lines and `#` comments are
    /// dropped; everything else is parsed now, so syntax errors surface at load time.
    pub fn load(&mut self, id: &str, source: &str) -> Result<(), ParseError> {
        let mut lines = Vec::new();
        for line in source.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            lines.push(match line.strip_prefix('$') {
                Some(rest) => Line::Macro(rest.to_owned()),
                None => Line::Plain(Command::parse(line)?),
            });
        }
        self.functions.insert(id.to_owned(), Rc::new(lines));
        Ok(())
    }

    /// Runs a loaded function, as the `function` command does.
    pub fn call(&mut self, id: &str) -> Outcome {
        match self.call_inner(id, None) {
            Flow::Next(outcome) | Flow::Return(outcome) => outcome,
            Flow::Halt => Outcome::FAILED,
        }
    }

    fn call_inner(&mut self, id: &str, args: Option<&Compound>) -> Flow {
        let Some(body) = self.functions.get(id).cloned() else {
            return Flow::Next(self.fail(format!("unknown function '{id}'")));
        };
        if args.is_none() && body.iter().any(|l| matches!(l, Line::Macro(_))) {
            return Flow::Next(self.fail(format!(
                "function '{id}' has macro lines but was called without arguments"
            )));
        }
        if self.stack.len() >= self.max_call_depth {
            let limit = self.max_call_depth;
            return Flow::Next(self.fail(format!(
                "function calls nested more than {limit} deep; see SPEC.md section 5"
            )));
        }
        self.stack.push(id.to_owned());
        self.max_depth = self.max_depth.max(self.stack.len());
        let mut ran = 0i32;
        let mut outcome = None;
        for line in body.iter() {
            let command = match line {
                Line::Plain(command) => command.clone(),
                Line::Macro(text) => match substitute(text, args.expect("checked above")) {
                    Err(message) => {
                        self.fail(message);
                        self.stack.pop();
                        return Flow::Next(Outcome::FAILED);
                    }
                    Ok(line) => match Command::parse(&line) {
                        Err(e) => {
                            self.fail(format!("{line}: {e}"));
                            self.stack.pop();
                            return Flow::Next(Outcome::FAILED);
                        }
                        Ok(command) => command,
                    },
                },
            };
            match self.step(&command) {
                Flow::Next(_) => ran = ran.saturating_add(1),
                Flow::Return(o) => {
                    outcome = Some(o);
                    break;
                }
                Flow::Halt => {
                    self.stack.pop();
                    return Flow::Halt;
                }
            }
        }
        self.stack.pop();
        // Falling off the end reports how many commands the body ran.
        Flow::Next(outcome.unwrap_or(Outcome::ok(ran)))
    }

    /// Charges one command against the budget, then runs it.
    fn step(&mut self, command: &Command) -> Flow {
        if self.commands_run >= self.budget {
            if !self.over_budget {
                self.over_budget = true;
                let budget = self.budget;
                self.fail(format!(
                    "maxCommandChainLength of {budget} reached; the rest of the chain did not run"
                ));
            }
            return Flow::Halt;
        }
        self.commands_run += 1;
        if let Some(current) = self.stack.last() {
            *self.per_function.entry(current.clone()).or_default() += 1;
        }
        self.exec(command)
    }
    /// Parses and runs one line. A line that does not parse is a failure like any
    /// other, so a bad line in the middle of a function does not hide the rest of it.
    pub fn run_line(&mut self, line: &str) -> Outcome {
        match Command::parse(line) {
            Ok(command) => self.run(&command),
            Err(e) => self.fail(format!("{line}: {e}")),
        }
    }

    pub fn run(&mut self, command: &Command) -> Outcome {
        match self.step(command) {
            Flow::Next(outcome) | Flow::Return(outcome) => outcome,
            Flow::Halt => Outcome::FAILED,
        }
    }

    fn exec(&mut self, command: &Command) -> Flow {
        match command {
            Command::Scoreboard(cmd) => Flow::Next(self.scoreboard(cmd)),
            Command::Data(cmd) => Flow::Next(self.data(cmd)),
            Command::Function { id, args } => match self.function_args(args) {
                Err(message) => Flow::Next(self.fail(message)),
                Ok(args) => self.call_inner(id, args.as_ref()),
            },
            Command::Return(kind) => self.ret(kind),
            Command::Execute(cmd) => self.execute(cmd),
            Command::Unknown { name, args } => {
                self.effects.push(Effect {
                    name: name.clone(),
                    args: args.clone(),
                    executor: self.context.executor.clone(),
                    position: self.context.pos,
                });
                Flow::Next(Outcome::ok(1))
            }
        }
    }

    fn function_args(&self, args: &FnArgs) -> Result<Option<Compound>, String> {
        match args {
            FnArgs::None => Ok(None),
            FnArgs::Inline(fields) => Ok(Some(fields.clone())),
            FnArgs::From { target, path } => {
                match self.read_source(target, path.as_ref()).map_err(|error| {
                    error.unwrap_or_else(|| "found no arguments at the path".to_owned())
                })? {
                    NbtValue::Compound(fields) => Ok(Some(fields)),
                    other => Err(format!(
                        "function arguments must be a compound, found {}",
                        other.tag_name()
                    )),
                }
            }
        }
    }

    fn execute(&mut self, cmd: &Execute) -> Flow {
        // Vanilla threads a *list* of contexts through the clauses: `as` and `at` fork
        // it, conditions filter it, and the command runs once per survivor.
        let mut contexts = vec![self.context.clone()];
        let mut stores = Vec::new();

        for clause in &cmd.clauses {
            match clause {
                Clause::Store { success, into } => stores.push((*success, into)),
                Clause::Deferred(name) => {
                    return Flow::Next(self.fail(deferred(name)));
                }
                Clause::As(selector) => {
                    contexts = contexts
                        .into_iter()
                        .flat_map(|ctx| {
                            self.world
                                .resolve(selector, ctx.executor.as_deref())
                                .into_iter()
                                .map(move |id| Context {
                                    executor: Some(id),
                                    ..ctx.clone()
                                })
                        })
                        .collect();
                }
                // Vanilla's `at` moves the rotation as well as the position, which
                // is what makes `at @s` then `^ ^ ^1` mean "a block ahead of where it
                // is looking".
                Clause::At(selector) => {
                    contexts = contexts
                        .into_iter()
                        .flat_map(|ctx| {
                            let world = &self.world;
                            world
                                .resolve(selector, ctx.executor.as_deref())
                                .into_iter()
                                .filter_map(|id| world.entity(&id).map(|e| (e.pos, e.rot)))
                                .map(move |(pos, rot)| Context {
                                    pos,
                                    rot,
                                    ..ctx.clone()
                                })
                                .collect::<Vec<_>>()
                        })
                        .collect();
                }
                Clause::Positioned(coords) => {
                    for ctx in &mut contexts {
                        ctx.pos = coords.resolve(ctx.pos, ctx.rot);
                    }
                }
                Clause::Rotated(yaw, pitch) => {
                    for ctx in &mut contexts {
                        ctx.rot = [*yaw, *pitch];
                    }
                }
                Clause::Cond { negated, cond } => {
                    let mut kept = Vec::new();
                    for ctx in contexts {
                        let outer = std::mem::replace(&mut self.context, ctx.clone());
                        let held = self.condition(cond);
                        self.context = outer;
                        match held {
                            // A deferred condition is not a false one: nothing about
                            // this command can be trusted, so stop rather than guess.
                            None => return Flow::Next(Outcome::FAILED),
                            Some(held) if held != *negated => kept.push(ctx),
                            Some(_) => {}
                        }
                    }
                    contexts = kept;
                }
            }
        }

        let (outcome, flow) = self.run_in(contexts, cmd);

        // Stores fire even when nothing matched, writing 0.
        for (success, into) in stores {
            self.store(success, into, outcome);
        }
        match flow {
            Some(outcome) => Flow::Return(outcome),
            None => Flow::Next(outcome),
        }
    }

    /// Runs the command once per context, accumulating success as vanilla does.
    fn run_in(&mut self, contexts: Vec<Context>, cmd: &Execute) -> (Outcome, Option<Outcome>) {
        let Some(command) = &cmd.run else {
            // The bare conditional form reports how many contexts survived.
            let success = contexts.len() as u32;
            return (
                Outcome {
                    success,
                    result: success as i32,
                },
                None,
            );
        };

        let outer = self.context.clone();
        let mut total = 0u32;
        let mut result = 0;
        let mut flow = None;
        for ctx in contexts {
            self.context = ctx;
            match self.step(command) {
                Flow::Next(outcome) => {
                    total += outcome.success;
                    result = outcome.result;
                }
                // `run return ...` leaves the enclosing function, so the remaining
                // contexts do not get their turn.
                Flow::Return(outcome) => {
                    total += outcome.success;
                    result = outcome.result;
                    flow = Some(outcome);
                    break;
                }
                Flow::Halt => {
                    self.context = outer;
                    return (Outcome::FAILED, None);
                }
            }
        }
        self.context = outer;
        (
            Outcome {
                success: total,
                result,
            },
            flow,
        )
    }

    /// `None` when the condition is one of the deferred kinds.
    fn condition(&mut self, cond: &Condition) -> Option<bool> {
        match cond {
            Condition::Deferred(name) => {
                let message = deferred(name);
                self.fail(message);
                None
            }
            // An unset score makes the condition false rather than an error, so
            // `if score` never needs the holder to exist first.
            Condition::Score {
                holder,
                objective,
                test,
            } => {
                let Ok(Some(value)) = self.world.scoreboard.get(objective, holder) else {
                    return Some(false);
                };
                Some(match test {
                    ScoreTest::Matches { min, max } => {
                        min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max)
                    }
                    ScoreTest::Against {
                        op,
                        holder,
                        objective,
                    } => {
                        let Ok(Some(other)) = self.world.scoreboard.get(objective, holder) else {
                            return Some(false);
                        };
                        match op {
                            Cmp::Lt => value < other,
                            Cmp::Le => value <= other,
                            Cmp::Eq => value == other,
                            Cmp::Ge => value >= other,
                            Cmp::Gt => value > other,
                        }
                    }
                })
            }
            Condition::Entity(selector) => {
                let found = self
                    .world
                    .resolve(selector, self.context.executor.as_deref());
                Some(!found.is_empty())
            }
            Condition::Data { target, path } => match self.read_target(target) {
                Err(error) => {
                    self.failed_read(error);
                    None
                }
                Ok(root) => Some(!path.resolve(&root).is_empty()),
            },
        }
    }

    fn store(&mut self, success: bool, into: &StoreTarget, outcome: Outcome) {
        let value = if success {
            outcome.success as i32
        } else {
            outcome.result
        };
        match into {
            StoreTarget::Score { holder, objective } => {
                if let Err(e) = self.world.scoreboard.set(objective, holder, value) {
                    self.fail(e.to_string());
                }
            }
            StoreTarget::Storage {
                id,
                path,
                tag,
                scale,
            } => {
                let scaled = value as f64 * scale;
                let value = match tag {
                    NumTag::Byte => NbtValue::Byte(scaled as i8),
                    NumTag::Short => NbtValue::Short(scaled as i16),
                    NumTag::Int => NbtValue::Int(scaled as i32),
                    NumTag::Long => NbtValue::Long(scaled as i64),
                    NumTag::Float => NbtValue::Float(scaled as f32),
                    NumTag::Double => NbtValue::Double(scaled),
                };
                path.set(self.world.storage_mut(id), value);
            }
        }
    }

    fn ret(&mut self, kind: &Return) -> Flow {
        if self.stack.is_empty() {
            return Flow::Next(self.fail("'return' can only be used inside a function"));
        }
        match kind {
            Return::Value(value) => Flow::Return(Outcome::ok(*value)),
            Return::Fail => Flow::Return(Outcome::FAILED),
            Return::Run(command) => match self.step(command) {
                Flow::Next(outcome) | Flow::Return(outcome) => Flow::Return(outcome),
                Flow::Halt => Flow::Halt,
            },
        }
    }

    fn fail(&mut self, message: impl Into<String>) -> Outcome {
        self.diagnostics.push(message.into());
        Outcome::FAILED
    }

    /// A read that produced nothing: a diagnostic if it was a mistake, and a plain
    /// failure if the path was simply not there.
    fn failed_read(&mut self, error: ReadError) -> Outcome {
        match error {
            Some(message) => self.fail(message),
            None => Outcome::FAILED,
        }
    }

    fn data(&mut self, cmd: &Data) -> Outcome {
        match cmd {
            Data::Get {
                target,
                path,
                scale,
            } => {
                let root = match self.read_target(target) {
                    Ok(root) => root,
                    Err(message) => return self.failed_read(message),
                };
                let Some(path) = path else {
                    return Outcome::ok(1);
                };
                match exactly_one(&path.resolve(&root)) {
                    Err(error) => self.failed_read(error),
                    Ok(value) => match measure(&value) {
                        None => self.fail(format!("{value} is not a value data can read")),
                        Some(n) => Outcome::ok((n * scale).floor() as i32),
                    },
                }
            }
            Data::Merge { target, value } => self.with_target(target, |root| {
                merge_into(root, value);
                Outcome::ok(1)
            }),
            Data::Remove { target, path } => {
                self.with_target(target, |root| match path.remove(root) {
                    0 => Outcome::FAILED,
                    n => Outcome {
                        success: n as u32,
                        result: n as i32,
                    },
                })
            }
            Data::Modify {
                target,
                path,
                kind,
                source,
            } => {
                let value = match self.resolve_source(source) {
                    Ok(value) => value,
                    Err(error) => return self.failed_read(error),
                };
                self.with_target(target, |root| {
                    let leaf = if kind.wants_list() {
                        NbtValue::List(Vec::new())
                    } else {
                        NbtValue::Compound(Compound::new())
                    };
                    let n = path.modify_creating(root, leaf, &mut |slot| {
                        apply(slot, *kind, &value);
                    });
                    match n {
                        0 => Outcome::FAILED,
                        _ => Outcome::ok(1),
                    }
                })
            }
        }
    }

    /// A snapshot of a target's root, or why it cannot be read.
    fn read_target(&self, target: &Target) -> Result<NbtValue, ReadError> {
        match target {
            Target::Storage(id) => Ok(self.world.storage(id).clone()),
            Target::Entity(selector) => {
                let id = self.one_entity(selector)?;
                Ok(self
                    .world
                    .entity(&id)
                    .expect("the selector found it")
                    .nbt
                    .clone())
            }
            other => Err(Some(unmodelled(other))),
        }
    }

    /// The one entity a data command may point at.
    ///
    /// Vanilla refuses `data` on anything but a single entity. Finding nobody is an
    /// ordinary answer — a condition asking about an entity that is not there is
    /// false, not wrong — while finding several is the mistake the source language
    /// rejects before it can happen (SPEC.md section 4.2).
    fn one_entity(&self, selector: &str) -> Result<String, ReadError> {
        let found = self
            .world
            .resolve(selector, self.context.executor.as_deref());
        match found.len() {
            1 => Ok(found.into_iter().next().expect("one")),
            0 => Err(None),
            n => Err(Some(format!(
                "'{selector}' found {n} entities, and data takes exactly one"
            ))),
        }
    }

    fn with_target(
        &mut self,
        target: &Target,
        f: impl FnOnce(&mut NbtValue) -> Outcome,
    ) -> Outcome {
        match target {
            Target::Storage(id) => f(self.world.storage_mut(id)),
            Target::Entity(selector) => match self.one_entity(selector) {
                Err(error) => self.failed_read(error),
                Ok(id) => {
                    let entity = self.world.entity_mut(&id).expect("the selector found it");
                    f(&mut entity.nbt)
                }
            },
            other => {
                let message = unmodelled(other);
                self.fail(message)
            }
        }
    }

    fn resolve_source(&self, source: &Source) -> Result<NbtValue, ReadError> {
        match source {
            Source::Value(value) => Ok(value.clone()),
            Source::From { target, path } => self.read_source(target, path.as_ref()),
            Source::Str {
                target,
                path,
                start,
                end,
            } => {
                let value = self.read_source(target, path.as_ref())?;
                let text = match &value {
                    NbtValue::String(s) => s.clone(),
                    other => other.to_string(),
                };
                Ok(NbtValue::String(slice(&text, *start, *end)))
            }
        }
    }

    fn read_source(&self, target: &Target, path: Option<&NbtPath>) -> Result<NbtValue, ReadError> {
        let root = self.read_target(target)?;
        match path {
            None => Ok(root),
            Some(path) => exactly_one(&path.resolve(&root)),
        }
    }

    fn scoreboard(&mut self, cmd: &Scoreboard) -> Outcome {
        match cmd {
            Scoreboard::AddObjective(name) => {
                if self.world.scoreboard.has_objective(name) {
                    return self.fail(format!("an objective already exists by the name '{name}'"));
                }
                self.world.scoreboard.add_objective(name);
                Outcome::ok(1)
            }
            Scoreboard::RemoveObjective(name) => {
                if !self.world.scoreboard.has_objective(name) {
                    return self.fail(format!("unknown scoreboard objective '{name}'"));
                }
                self.world.scoreboard.remove_objective(name);
                Outcome::ok(1)
            }
            Scoreboard::Get { holder, objective } => {
                match self.world.scoreboard.get(objective, holder) {
                    Err(e) => self.fail(e.to_string()),
                    Ok(None) => self.fail(format!(
                        "can't get value of '{objective}' for '{holder}'; none is set"
                    )),
                    Ok(Some(value)) => Outcome::ok(value),
                }
            }
            Scoreboard::Set {
                holder,
                objective,
                value,
            } => match self.world.scoreboard.set(objective, holder, *value) {
                Err(e) => self.fail(e.to_string()),
                Ok(()) => Outcome::ok(*value),
            },
            Scoreboard::Add {
                holder,
                objective,
                delta,
            } => match self.world.scoreboard.get_or_create(objective, holder) {
                Err(e) => self.fail(e.to_string()),
                Ok(current) => {
                    let updated = current.wrapping_add(*delta);
                    let _ = self.world.scoreboard.set(objective, holder, updated);
                    Outcome::ok(updated)
                }
            },
            Scoreboard::Reset { holder, objective } => {
                match objective {
                    Some(objective) => self.world.scoreboard.reset(objective, holder),
                    None => self.world.scoreboard.reset_all(holder),
                }
                Outcome::ok(1)
            }
            Scoreboard::Operation {
                holder,
                objective,
                op,
                source,
                source_objective,
            } => self.operation(holder, objective, *op, source, source_objective),
        }
    }

    fn operation(
        &mut self,
        holder: &str,
        objective: &str,
        op: Op,
        source: &str,
        source_objective: &str,
    ) -> Outcome {
        let board = &mut self.world.scoreboard;
        // Both sides are created as 0 if absent, as vanilla does.
        let target = match board.get_or_create(objective, holder) {
            Ok(v) => v,
            Err(e) => return self.fail(e.to_string()),
        };
        let operand = match board.get_or_create(source_objective, source) {
            Ok(v) => v,
            Err(e) => return self.fail(e.to_string()),
        };

        if matches!(op, Op::Div | Op::Rem) && operand == 0 {
            return self.fail(format!(
                "can't divide '{holder}' by zero from '{source}'; target left unchanged"
            ));
        }

        let updated = match op {
            Op::Assign => operand,
            Op::Add => target.wrapping_add(operand),
            Op::Sub => target.wrapping_sub(operand),
            Op::Mul => target.wrapping_mul(operand),
            // Vanilla floors rather than truncating, so -7 / 2 is -4 and -7 % 2 is 1.
            Op::Div => floor_div(target, operand),
            Op::Rem => floor_mod(target, operand),
            Op::Min => target.min(operand),
            Op::Max => target.max(operand),
            Op::Swap => operand,
        };

        let board = &mut self.world.scoreboard;
        let _ = board.set(objective, holder, updated);
        if op == Op::Swap {
            let _ = board.set(source_objective, source, target);
        }
        Outcome::ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs lines in order, returning the last outcome.
    fn run(lines: &[&str]) -> (Interpreter, Outcome) {
        let mut it = Interpreter::default();
        let mut last = Outcome::FAILED;
        for line in lines {
            last = it.run_line(line);
        }
        (it, last)
    }

    fn score(it: &Interpreter, holder: &str) -> Option<i32> {
        it.world.scoreboard.get("obj", holder).unwrap()
    }

    const SETUP: &str = "scoreboard objectives add obj dummy";

    #[test]
    fn set_then_get() {
        let (_, out) = run(&[
            SETUP,
            "scoreboard players set $a obj 7",
            "scoreboard players get $a obj",
        ]);
        assert_eq!(out, Outcome::ok(7));
    }

    #[test]
    fn getting_an_unset_score_fails_and_says_why() {
        let (it, out) = run(&[SETUP, "scoreboard players get $a obj"]);
        assert_eq!(out, Outcome::FAILED);
        assert_eq!(it.diagnostics.len(), 1);
        assert!(it.diagnostics[0].contains("$a"), "{:?}", it.diagnostics);
    }

    #[test]
    fn an_unknown_objective_fails_rather_than_reading_as_zero() {
        let (it, out) = run(&["scoreboard players set $a nope 1"]);
        assert_eq!(out, Outcome::FAILED);
        assert!(it.diagnostics[0].contains("nope"));
    }

    #[test]
    fn a_failed_command_does_not_stop_the_ones_after_it() {
        let (it, out) = run(&[
            SETUP,
            "scoreboard players get $a obj",
            "scoreboard players set $a obj 3",
        ]);
        assert_eq!(out, Outcome::ok(3));
        assert_eq!(score(&it, "$a"), Some(3));
    }

    #[test]
    fn adding_an_existing_objective_fails() {
        let (_, out) = run(&[SETUP, SETUP]);
        assert_eq!(out, Outcome::FAILED);
    }

    #[test]
    fn add_and_remove_wrap_like_java_ints() {
        let (it, _) = run(&[
            SETUP,
            "scoreboard players set $a obj 2147483647",
            "scoreboard players add $a obj 1",
        ]);
        assert_eq!(score(&it, "$a"), Some(i32::MIN));
    }

    #[test]
    fn division_and_modulo_are_floored_not_truncated() {
        // Rust would give -3 and -1 here. Vanilla gives -4 and 1.
        let (it, _) = run(&[
            SETUP,
            "scoreboard players set $a obj -7",
            "scoreboard players set $b obj -7",
            "scoreboard players set $two obj 2",
            "scoreboard players operation $a obj /= $two obj",
            "scoreboard players operation $b obj %= $two obj",
        ]);
        assert_eq!(score(&it, "$a"), Some(-4));
        assert_eq!(score(&it, "$b"), Some(1));
    }

    #[test]
    fn dividing_by_zero_fails_and_leaves_the_target_alone() {
        let (it, out) = run(&[
            SETUP,
            "scoreboard players set $a obj 5",
            "scoreboard players set $z obj 0",
            "scoreboard players operation $a obj /= $z obj",
        ]);
        assert_eq!(out, Outcome::FAILED);
        assert_eq!(score(&it, "$a"), Some(5));
    }

    #[test]
    fn min_max_and_swap() {
        let (it, _) = run(&[
            SETUP,
            "scoreboard players set $a obj 1",
            "scoreboard players set $b obj 2",
            "scoreboard players operation $a obj > $b obj",
            "scoreboard players set $c obj 9",
            "scoreboard players set $d obj 4",
            "scoreboard players operation $c obj < $d obj",
            "scoreboard players set $e obj 1",
            "scoreboard players set $f obj 2",
            "scoreboard players operation $e obj >< $f obj",
        ]);
        assert_eq!(score(&it, "$a"), Some(2));
        assert_eq!(score(&it, "$c"), Some(4));
        assert_eq!((score(&it, "$e"), score(&it, "$f")), (Some(2), Some(1)));
    }

    #[test]
    fn an_operation_creates_missing_scores_as_zero() {
        let (it, _) = run(&[SETUP, "scoreboard players operation $a obj += $b obj"]);
        assert_eq!(score(&it, "$a"), Some(0));
        assert_eq!(score(&it, "$b"), Some(0));
    }

    #[test]
    fn reset_targets_one_objective_or_all_of_them() {
        let (it, _) = run(&[
            SETUP,
            "scoreboard objectives add other dummy",
            "scoreboard players set $a obj 1",
            "scoreboard players set $a other 2",
            "scoreboard players reset $a obj",
        ]);
        assert_eq!(score(&it, "$a"), None);
        assert_eq!(it.world.scoreboard.get("other", "$a"), Ok(Some(2)));

        let (it, _) = run(&[
            SETUP,
            "scoreboard objectives add other dummy",
            "scoreboard players set $a obj 1",
            "scoreboard players set $a other 2",
            "scoreboard players reset $a",
        ]);
        assert_eq!(score(&it, "$a"), None);
        assert_eq!(it.world.scoreboard.get("other", "$a"), Ok(None));
    }

    #[test]
    fn removing_an_objective_takes_its_scores_with_it() {
        let (it, _) = run(&[
            SETUP,
            "scoreboard players set $a obj 1",
            "scoreboard objectives remove obj",
        ]);
        assert!(!it.world.scoreboard.has_objective("obj"));
    }

    #[test]
    fn a_line_that_does_not_parse_fails_and_is_reported() {
        let (it, out) = run(&[SETUP, "scoreboard players set $a obj notanumber"]);
        assert_eq!(out, Outcome::FAILED);
        assert_eq!(it.diagnostics.len(), 1);
    }

    #[test]
    fn an_unmodelled_command_succeeds_and_is_not_a_diagnostic() {
        let (it, out) = run(&["say hi"]);
        assert_eq!(out, Outcome::ok(1));
        assert!(it.diagnostics.is_empty());
    }
}

#[cfg(test)]
mod data_tests {
    use super::*;
    use crate::nbt::NbtValue;
    use crate::path::NbtPath;

    fn run(lines: &[&str]) -> (Interpreter, Outcome) {
        let mut it = Interpreter::default();
        let mut last = Outcome::FAILED;
        for line in lines {
            last = it.run_line(line);
        }
        (it, last)
    }

    fn at(it: &Interpreter, path: &str) -> Vec<NbtValue> {
        NbtPath::parse(path)
            .unwrap()
            .resolve(it.world.storage("ns:mw"))
    }

    #[test]
    fn get_scales_and_floors() {
        let (_, out) = run(&[
            "data modify storage ns:mw v set value 7",
            "data get storage ns:mw v 0.5",
        ]);
        assert_eq!(out, Outcome::ok(3));

        // Flooring, not truncation: -3.5 floors to -4.
        let (_, out) = run(&[
            "data modify storage ns:mw v set value -7",
            "data get storage ns:mw v 0.5",
        ]);
        assert_eq!(out, Outcome::ok(-4));
    }

    #[test]
    fn get_on_a_string_returns_its_length() {
        // The premise behind `String::len()` being free in the source language.
        let (_, out) = run(&[
            r#"data modify storage ns:mw s set value "hello""#,
            "data get storage ns:mw s",
        ]);
        assert_eq!(out, Outcome::ok(5));
    }

    #[test]
    fn get_on_a_collection_returns_its_size() {
        let (_, out) = run(&[
            "data modify storage ns:mw l set value [1,2,3]",
            "data get storage ns:mw l",
        ]);
        assert_eq!(out, Outcome::ok(3));
        let (_, out) = run(&[
            "data modify storage ns:mw c set value {a:1,b:2}",
            "data get storage ns:mw c",
        ]);
        assert_eq!(out, Outcome::ok(2));
    }

    #[test]
    fn get_fails_on_no_match_and_on_several() {
        // A path that is not there is an answer, not a mistake: it is what the source
        // language reads as `None`, so it carries no diagnostic (SPEC.md section 4.2).
        let (it, out) = run(&["data get storage ns:mw missing"]);
        assert_eq!(out, Outcome::FAILED);
        assert!(it.diagnostics.is_empty(), "{:?}", it.diagnostics);

        // Several matches is a mistake, and does say so.
        let (it, out) = run(&[
            "data modify storage ns:mw l set value [1,2]",
            "data get storage ns:mw l[]",
        ]);
        assert_eq!(out, Outcome::FAILED);
        assert!(!it.diagnostics.is_empty());
    }

    #[test]
    fn entity_data_reads_and_writes_the_entity_it_finds() {
        let mut it = Interpreter::default();
        it.world.spawn("zombie-1", [0.0, 64.0, 0.0]).nbt =
            crate::snbt::parse("{Health:18.0f}").expect("snbt");
        it.world
            .bind_selector("@e[type=zombie,limit=1]", ["zombie-1"]);

        let out = it.run_line("data get entity @e[type=zombie,limit=1] Health 1000");
        assert_eq!(out, Outcome::ok(18000));

        let out = it.run_line("data modify entity @e[type=zombie,limit=1] Fire set value 100s");
        assert_eq!(out, Outcome::ok(1));
        assert_eq!(
            it.world.entity("zombie-1").expect("spawned").nbt,
            crate::snbt::parse("{Health:18.0f,Fire:100s}").expect("snbt")
        );
        assert!(it.diagnostics.is_empty(), "{:?}", it.diagnostics);
    }

    #[test]
    fn entity_data_takes_exactly_one_entity() {
        let mut it = Interpreter::default();
        it.world.spawn("a", [0.0, 0.0, 0.0]);
        it.world.spawn("b", [0.0, 0.0, 0.0]);
        it.world.bind_selector("@e[type=zombie]", ["a", "b"]);

        // Several is the mistake the source language rejects; say so if it gets here.
        let out = it.run_line("data get entity @e[type=zombie] Health");
        assert_eq!(out, Outcome::FAILED);
        assert!(
            it.diagnostics[0].contains("exactly one"),
            "{:?}",
            it.diagnostics
        );

        // Nobody there is an ordinary answer, the same as a path that is not there.
        it.diagnostics.clear();
        let out = it.run_line("data get entity @e[type=creeper] Health");
        assert_eq!(out, Outcome::FAILED);
        assert!(it.diagnostics.is_empty(), "{:?}", it.diagnostics);
    }

    #[test]
    fn a_condition_about_an_entity_is_just_false_when_there_is_none() {
        let mut it = Interpreter::default();
        it.world.spawn("zombie-1", [0.0, 64.0, 0.0]);
        it.world
            .bind_selector("@e[type=zombie,limit=1]", ["zombie-1"]);

        let out = it.run_line("execute if data entity @e[type=zombie,limit=1] Health");
        assert_eq!(out, Outcome::FAILED);
        assert!(it.diagnostics.is_empty(), "{:?}", it.diagnostics);
    }

    #[test]
    fn copying_from_a_path_that_is_not_there_says_nothing() {
        // `data modify ... set from <missing>` leaves the target alone and reports
        // failure. That silence is what lets an option be copied.
        let (it, out) = run(&[
            "data modify storage ns:mw a set value 1",
            "data modify storage ns:mw a set from storage ns:mw missing",
        ]);
        assert_eq!(out, Outcome::FAILED);
        assert!(it.diagnostics.is_empty(), "{:?}", it.diagnostics);
        assert_eq!(at(&it, "a"), vec![NbtValue::Int(1)]);
    }

    #[test]
    fn set_value_creates_intermediates() {
        let (it, out) = run(&["data modify storage ns:mw a.b.c set value 5"]);
        assert_eq!(out, Outcome::ok(1));
        assert_eq!(at(&it, "a.b.c"), vec![NbtValue::Int(5)]);
    }

    #[test]
    fn set_from_copies_a_value() {
        let (it, _) = run(&[
            "data modify storage ns:mw src set value {k:1b}",
            "data modify storage ns:mw dst set from storage ns:mw src",
        ]);
        assert_eq!(at(&it, "dst.k"), vec![NbtValue::Byte(1)]);
    }

    #[test]
    fn set_from_fails_when_the_source_matches_several() {
        let (_, out) = run(&[
            "data modify storage ns:mw l set value [1,2]",
            "data modify storage ns:mw dst set from storage ns:mw l[]",
        ]);
        assert_eq!(out, Outcome::FAILED);
    }

    #[test]
    fn set_string_slices_with_negative_bounds() {
        let (it, _) = run(&[
            r#"data modify storage ns:mw s set value "abcdef""#,
            "data modify storage ns:mw a set string storage ns:mw s 1 3",
            "data modify storage ns:mw b set string storage ns:mw s -2",
            "data modify storage ns:mw c set string storage ns:mw s",
        ]);
        assert_eq!(at(&it, "a"), vec![NbtValue::String("bc".into())]);
        assert_eq!(at(&it, "b"), vec![NbtValue::String("ef".into())]);
        assert_eq!(at(&it, "c"), vec![NbtValue::String("abcdef".into())]);
    }

    #[test]
    fn list_operations_create_a_missing_list() {
        let (it, _) = run(&[
            "data modify storage ns:mw l append value 2",
            "data modify storage ns:mw l prepend value 1",
            "data modify storage ns:mw l insert 2 value 3",
        ]);
        assert_eq!(
            at(&it, "l"),
            vec![NbtValue::List(vec![
                NbtValue::Int(1),
                NbtValue::Int(2),
                NbtValue::Int(3),
            ])]
        );
    }

    #[test]
    fn merge_combines_compounds() {
        let (it, _) = run(&[
            "data modify storage ns:mw c set value {a:1}",
            "data modify storage ns:mw c merge value {b:2}",
            "data merge storage ns:mw {top:9}",
        ]);
        assert_eq!(at(&it, "c.a"), vec![NbtValue::Int(1)]);
        assert_eq!(at(&it, "c.b"), vec![NbtValue::Int(2)]);
        assert_eq!(at(&it, "top"), vec![NbtValue::Int(9)]);
    }

    #[test]
    fn remove_reports_how_many_it_detached() {
        let (it, out) = run(&[
            "data modify storage ns:mw l set value [{k:1},{k:2},{k:1}]",
            "data remove storage ns:mw l[{k:1}]",
        ]);
        assert_eq!(out.success, 2);
        assert_eq!(at(&it, "l").len(), 1);

        let (_, out) = run(&["data remove storage ns:mw nothing"]);
        assert_eq!(out, Outcome::FAILED);
    }

    #[test]
    fn storage_namespaces_do_not_leak_into_each_other() {
        let (it, _) = run(&[
            "data modify storage a:mw v set value 1",
            "data modify storage b:mw v set value 2",
        ]);
        assert_eq!(
            NbtPath::parse("v")
                .unwrap()
                .resolve(it.world.storage("a:mw")),
            vec![NbtValue::Int(1)]
        );
    }

    #[test]
    fn block_targets_parse_but_say_they_are_not_modelled() {
        let (it, out) = run(&["data get block 0 0 0 Items"]);
        assert_eq!(out, Outcome::FAILED);
        assert!(it.diagnostics[0].contains("not modelled"));

        // An entity target with no executor finds nobody, which is an answer.
        let (it, out) = run(&["data get entity @s Health"]);
        assert_eq!(out, Outcome::FAILED);
        assert!(it.diagnostics.is_empty(), "{:?}", it.diagnostics);
    }
}

#[cfg(test)]
mod function_tests {
    use super::*;

    fn pack(functions: &[(&str, &str)]) -> Interpreter {
        let mut it = Interpreter::default();
        for (id, source) in functions {
            it.load(id, source).unwrap();
        }
        it
    }

    const SETUP: &str = "scoreboard objectives add obj dummy";

    #[test]
    fn a_call_runs_the_body() {
        let mut it = pack(&[("ns:f", "scoreboard players set $a obj 3")]);
        it.run_line(SETUP);
        it.run_line("function ns:f");
        assert_eq!(it.world.scoreboard.get("obj", "$a"), Ok(Some(3)));
    }

    #[test]
    fn blank_lines_and_comments_are_dropped() {
        let mut it = pack(&[(
            "ns:f",
            "# a comment\n\n   \n   # indented comment\nscoreboard players set $a obj 1\n",
        )]);
        it.run_line(SETUP);
        assert_eq!(it.run_line("function ns:f"), Outcome::ok(1));
    }

    #[test]
    fn a_syntax_error_is_reported_when_the_pack_loads() {
        let mut it = Interpreter::default();
        assert!(
            it.load("ns:f", "scoreboard players set $a obj notanumber")
                .is_err()
        );
    }

    #[test]
    fn return_ends_the_function_and_supplies_its_value() {
        let mut it = pack(&[("ns:f", "return 7\nscoreboard players set $a obj 99")]);
        it.run_line(SETUP);
        assert_eq!(it.run_line("function ns:f"), Outcome::ok(7));
        // The line after `return` never ran.
        assert_eq!(it.world.scoreboard.get("obj", "$a"), Ok(None));
    }

    #[test]
    fn return_fail_reports_failure() {
        let mut it = pack(&[("ns:f", "return fail")]);
        assert_eq!(it.run_line("function ns:f"), Outcome::FAILED);
    }

    #[test]
    fn return_run_passes_the_commands_outcome_through() {
        let mut it = pack(&[("ns:f", "return run scoreboard players get $a obj")]);
        it.run_line(SETUP);
        it.run_line("scoreboard players set $a obj 4");
        assert_eq!(it.run_line("function ns:f"), Outcome::ok(4));
    }

    #[test]
    fn returning_does_not_propagate_to_the_caller() {
        let mut it = pack(&[
            (
                "ns:outer",
                "function ns:inner\nscoreboard players set $a obj 5",
            ),
            ("ns:inner", "return 1"),
        ]);
        it.run_line(SETUP);
        it.run_line("function ns:outer");
        assert_eq!(it.world.scoreboard.get("obj", "$a"), Ok(Some(5)));
    }

    #[test]
    fn falling_off_the_end_reports_the_commands_run() {
        let mut it = pack(&[("ns:f", "say a\nsay b\nsay c")]);
        assert_eq!(it.run_line("function ns:f"), Outcome::ok(3));
    }

    #[test]
    fn calling_an_unknown_function_fails() {
        let mut it = Interpreter::default();
        assert_eq!(it.run_line("function ns:nope"), Outcome::FAILED);
        assert!(it.diagnostics[0].contains("ns:nope"));
    }
}

#[cfg(test)]
mod execute_tests {
    use super::*;
    use crate::nbt::NbtValue;
    use crate::path::NbtPath;

    fn setup() -> Interpreter {
        let mut it = Interpreter::default();
        it.run_line("scoreboard objectives add obj dummy");
        it
    }

    fn score(it: &Interpreter, holder: &str) -> Option<i32> {
        it.world.scoreboard.get("obj", holder).unwrap()
    }

    fn at(it: &Interpreter, path: &str) -> Vec<NbtValue> {
        NbtPath::parse(path)
            .unwrap()
            .resolve(it.world.storage("ns:mw"))
    }

    #[test]
    fn a_condition_with_no_run_reports_whether_it_held() {
        let mut it = setup();
        it.run_line("scoreboard players set $a obj 3");
        assert_eq!(
            it.run_line("execute if score $a obj matches 1..5"),
            Outcome::ok(1)
        );
        assert_eq!(
            it.run_line("execute if score $a obj matches 4..5"),
            Outcome::FAILED
        );
        assert_eq!(
            it.run_line("execute unless score $a obj matches 4..5"),
            Outcome::ok(1)
        );
    }

    #[test]
    fn ranges_are_open_at_either_end() {
        let mut it = setup();
        it.run_line("scoreboard players set $a obj 3");
        for (range, expected) in [
            ("3", true),
            ("4", false),
            ("3..", true),
            ("4..", false),
            ("..3", true),
            ("..2", false),
            ("1..5", true),
        ] {
            let out = it.run_line(&format!("execute if score $a obj matches {range}"));
            assert_eq!(!out.failed(), expected, "range {range}");
        }
    }

    #[test]
    fn scores_compare_against_each_other() {
        let mut it = setup();
        it.run_line("scoreboard players set $a obj 3");
        it.run_line("scoreboard players set $b obj 5");
        for (op, expected) in [
            ("<", true),
            ("<=", true),
            ("=", false),
            (">=", false),
            (">", false),
        ] {
            let out = it.run_line(&format!("execute if score $a obj {op} $b obj"));
            assert_eq!(!out.failed(), expected, "operator {op}");
        }
    }

    #[test]
    fn an_unset_score_makes_a_condition_false_rather_than_an_error() {
        let mut it = setup();
        let out = it.run_line("execute if score $missing obj matches 0..");
        assert_eq!(out, Outcome::FAILED);
        assert!(it.diagnostics.is_empty(), "{:?}", it.diagnostics);
    }

    #[test]
    fn if_data_tests_whether_a_path_matches() {
        let mut it = setup();
        it.run_line("data modify storage ns:mw a.b set value 1");
        assert_eq!(
            it.run_line("execute if data storage ns:mw a.b"),
            Outcome::ok(1)
        );
        assert_eq!(
            it.run_line("execute if data storage ns:mw a.c"),
            Outcome::FAILED
        );
        assert_eq!(
            it.run_line("execute unless data storage ns:mw a.c"),
            Outcome::ok(1)
        );
    }

    #[test]
    fn run_executes_only_when_every_condition_holds() {
        let mut it = setup();
        it.run_line("scoreboard players set $a obj 1");
        it.run_line("execute if score $a obj matches 1 run scoreboard players set $hit obj 1");
        assert_eq!(score(&it, "$hit"), Some(1));

        it.run_line("execute if score $a obj matches 1 if score $a obj matches 2 run scoreboard players set $miss obj 1");
        assert_eq!(score(&it, "$miss"), None);
    }

    #[test]
    fn store_result_captures_the_commands_value() {
        let mut it = setup();
        it.run_line("scoreboard players set $a obj 42");
        it.run_line("execute store result score $out obj run scoreboard players get $a obj");
        assert_eq!(score(&it, "$out"), Some(42));
    }

    #[test]
    fn store_success_is_zero_for_a_failed_command() {
        // The premise behind `Option<T>` being cheap in the source language.
        let mut it = setup();
        it.run_line("execute store success score $ok obj run scoreboard players get $missing obj");
        assert_eq!(score(&it, "$ok"), Some(0));

        it.run_line("scoreboard players set $missing obj 1");
        it.run_line("execute store success score $ok obj run scoreboard players get $missing obj");
        assert_eq!(score(&it, "$ok"), Some(1));
    }

    #[test]
    fn store_applies_even_when_a_condition_blocked_the_command() {
        let mut it = setup();
        it.run_line("scoreboard players set $out obj 99");
        it.run_line("execute store result score $out obj if score $a obj matches 1 run say hi");
        assert_eq!(score(&it, "$out"), Some(0));
    }

    #[test]
    fn store_into_storage_honours_the_tag_and_scale() {
        let mut it = setup();
        it.run_line("scoreboard players set $a obj 7");
        it.run_line("execute store result storage ns:mw v int 1 run scoreboard players get $a obj");
        it.run_line(
            "execute store result storage ns:mw b byte 1 run scoreboard players get $a obj",
        );
        it.run_line(
            "execute store result storage ns:mw d double 0.5 run scoreboard players get $a obj",
        );
        assert_eq!(at(&it, "v"), vec![NbtValue::Int(7)]);
        assert_eq!(at(&it, "b"), vec![NbtValue::Byte(7)]);
        assert_eq!(at(&it, "d"), vec![NbtValue::Double(3.5)]);
    }

    #[test]
    fn several_stores_all_fire() {
        let mut it = setup();
        it.run_line("scoreboard players set $a obj 5");
        it.run_line(
            "execute store result score $r obj store success score $s obj run scoreboard players get $a obj",
        );
        assert_eq!((score(&it, "$r"), score(&it, "$s")), (Some(5), Some(1)));
    }

    #[test]
    fn execute_nests() {
        let mut it = setup();
        it.run_line("scoreboard players set $a obj 1");
        it.run_line(
            "execute if score $a obj matches 1 run execute if score $a obj matches 1 run scoreboard players set $deep obj 1",
        );
        assert_eq!(score(&it, "$deep"), Some(1));
    }

    #[test]
    fn recursion_terminates_when_its_condition_stops_holding() {
        let mut it = setup();
        it.load(
            "ns:count",
            "scoreboard players remove $n obj 1\n\
             execute if score $n obj matches 1.. run function ns:count",
        )
        .unwrap();
        it.run_line("scoreboard players set $n obj 5");
        it.run_line("function ns:count");
        assert_eq!(score(&it, "$n"), Some(0));
        assert!(it.diagnostics.is_empty(), "{:?}", it.diagnostics);
    }

    #[test]
    fn the_clauses_that_need_a_world_say_they_are_deferred() {
        let mut it = setup();
        for line in [
            "execute in minecraft:overworld run say hi",
            "execute anchored eyes run say hi",
            "execute if block 0 0 0 stone run say hi",
            "execute if predicate ns:p run say hi",
        ] {
            it.diagnostics.clear();
            assert_eq!(it.run_line(line), Outcome::FAILED, "{line}");
            assert!(
                it.diagnostics.iter().any(|d| d.contains("M0-8b")),
                "{line}: {:?}",
                it.diagnostics
            );
        }
    }
}

#[cfg(test)]
mod macro_tests {
    use super::*;

    fn setup(functions: &[(&str, &str)]) -> Interpreter {
        let mut it = Interpreter::default();
        it.run_line("scoreboard objectives add obj dummy");
        for (id, source) in functions {
            it.load(id, source).unwrap();
        }
        it
    }

    fn score(it: &Interpreter, holder: &str) -> Option<i32> {
        it.world.scoreboard.get("obj", holder).unwrap()
    }

    #[test]
    fn an_argument_is_substituted_into_the_line() {
        let mut it = setup(&[("ns:f", "$scoreboard players set $(who) obj $(n)")]);
        it.run_line("function ns:f {who:\"$a\", n:7}");
        assert_eq!(score(&it, "$a"), Some(7));
    }

    #[test]
    fn values_render_without_their_tag_suffix() {
        let mut it = setup(&[("ns:f", "$data modify storage ns:mw v set value $(x)")]);
        it.run_line("function ns:f {x:3b}");
        assert_eq!(
            crate::path::NbtPath::parse("v")
                .unwrap()
                .resolve(it.world.storage("ns:mw")),
            vec![crate::nbt::NbtValue::Int(3)]
        );
    }

    #[test]
    fn arguments_can_come_from_storage() {
        let mut it = setup(&[("ns:f", "$scoreboard players set $(who) obj 5")]);
        it.run_line("data modify storage ns:mw args set value {who:\"$b\"}");
        it.run_line("function ns:f with storage ns:mw args");
        assert_eq!(score(&it, "$b"), Some(5));
    }

    #[test]
    fn a_macro_function_called_without_arguments_fails() {
        // The rule a compiler is tested against: a #[tick] function must not be a
        // macro function, because function tags invoke without arguments.
        let mut it = setup(&[("ns:f", "$say $(x)")]);
        assert_eq!(it.run_line("function ns:f"), Outcome::FAILED);
        assert!(
            it.diagnostics.iter().any(|d| d.contains("macro")),
            "{:?}",
            it.diagnostics
        );
    }

    #[test]
    fn referring_to_a_missing_argument_fails() {
        let mut it = setup(&[("ns:f", "$say $(missing)")]);
        assert_eq!(it.run_line("function ns:f {other:1}"), Outcome::FAILED);
        assert!(it.diagnostics.iter().any(|d| d.contains("missing")));
    }

    #[test]
    fn a_substituted_line_that_does_not_parse_fails_at_call_time() {
        let mut it = setup(&[("ns:f", "$scoreboard players set $a obj $(n)")]);
        it.load("ns:f", "$scoreboard players set $a obj $(n)")
            .unwrap();
        assert_eq!(
            it.run_line("function ns:f {n:\"notanumber\"}"),
            Outcome::FAILED
        );
    }

    #[test]
    fn arguments_to_a_plain_function_are_harmless() {
        let mut it = setup(&[("ns:f", "scoreboard players set $a obj 1")]);
        assert!(!it.run_line("function ns:f {unused:1}").failed());
        assert_eq!(score(&it, "$a"), Some(1));
    }

    #[test]
    fn a_macro_line_without_any_placeholder_still_runs() {
        let mut it = setup(&[("ns:f", "$scoreboard players set $a obj 2")]);
        it.run_line("function ns:f {}");
        assert_eq!(score(&it, "$a"), Some(2));
    }
}

#[cfg(test)]
mod effect_tests {
    use super::*;

    #[test]
    fn unmodelled_commands_are_logged_in_order() {
        let mut it = Interpreter::default();
        it.run_line("say hello  world");
        it.run_line("setblock ~ ~1 ~ minecraft:stone");
        assert_eq!(
            it.effects,
            vec![
                Effect {
                    name: "say".into(),
                    args: "hello  world".into(),
                    executor: None,
                    position: [0.0; 3],
                },
                Effect {
                    name: "setblock".into(),
                    args: "~ ~1 ~ minecraft:stone".into(),
                    executor: None,
                    position: [0.0; 3],
                },
            ]
        );
    }

    #[test]
    fn a_command_that_did_not_run_leaves_nothing_behind() {
        let mut it = Interpreter::default();
        it.run_line("scoreboard objectives add obj dummy");
        it.run_line("execute if score $a obj matches 1 run say never");
        assert!(it.effects.is_empty());
    }

    #[test]
    fn modelled_commands_are_not_effects() {
        let mut it = Interpreter::default();
        it.run_line("scoreboard objectives add obj dummy");
        it.run_line("data modify storage ns:mw v set value 1");
        assert!(it.effects.is_empty());
    }
}

#[cfg(test)]
mod measurement_tests {
    use super::*;

    #[test]
    fn commands_are_charged_to_the_function_they_ran_in() {
        let mut it = Interpreter::default();
        it.load("ns:f", "say a\nsay b\nsay c").unwrap();
        it.run_line("function ns:f");

        let report = it.report();
        // Three inside the function, plus the top-level `function` command itself.
        assert_eq!(report.commands, 4);
        assert_eq!(report.per_function.get("ns:f"), Some(&3));
        assert_eq!(report.max_depth, 1);
    }

    #[test]
    fn an_execute_is_charged_alongside_the_command_it_runs() {
        let mut it = Interpreter::default();
        it.run_line("scoreboard objectives add obj dummy");
        it.run_line("scoreboard players set $a obj 1");
        let before = it.commands_run;
        it.run_line("execute if score $a obj matches 1 run say hi");
        assert_eq!(it.commands_run - before, 2);
    }

    #[test]
    fn a_loop_costs_exactly_what_the_accounting_rules_predict() {
        let mut it = Interpreter::default();
        it.run_line("scoreboard objectives add obj dummy");
        it.load(
            "ns:loop",
            "scoreboard players remove $n obj 1\n\
             execute if score $n obj matches 1.. run function ns:loop",
        )
        .unwrap();
        it.run_line("scoreboard players set $n obj 10");
        let before = it.commands_run;
        it.run_line("function ns:loop");

        // Ten iterations of (remove + execute) = 20, plus the nested `function`
        // command on the nine iterations that recurse, plus the outermost call.
        assert_eq!(it.commands_run - before, 10 * 2 + 9 + 1);
        assert_eq!(it.report().per_function.get("ns:loop"), Some(&(10 * 2 + 9)));
        assert_eq!(it.report().max_depth, 10);
    }

    #[test]
    fn the_report_says_when_the_budget_stopped_the_run() {
        let mut it = Interpreter::default();
        it.load("ns:long", &"say x\n".repeat(60)).unwrap();
        it.budget = 50;
        it.run_line("function ns:long");
        assert!(it.report().over_budget);
        assert_eq!(it.report().commands, 50);
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;

    fn zombies() -> Interpreter {
        let mut it = Interpreter::default();
        it.run_line("scoreboard objectives add obj dummy");
        it.world.spawn("z1", [1.0, 64.0, 0.0]);
        it.world.spawn("z2", [2.0, 64.0, 0.0]);
        it.world.bind_selector("@e[type=zombie]", ["z1", "z2"]);
        it
    }

    #[test]
    fn as_runs_once_per_entity_and_binds_the_executor() {
        let mut it = zombies();
        let out = it.run_line("execute as @e[type=zombie] run say hi");
        assert_eq!(out.success, 2);
        assert_eq!(
            it.effects
                .iter()
                .map(|e| e.executor.clone())
                .collect::<Vec<_>>(),
            vec![Some("z1".to_owned()), Some("z2".to_owned())]
        );
    }

    #[test]
    fn an_unbound_selector_finds_nothing_and_runs_nothing() {
        let mut it = zombies();
        let out = it.run_line("execute as @e[type=creeper] run say hi");
        assert_eq!(out, Outcome::FAILED);
        assert!(it.effects.is_empty());
    }

    #[test]
    fn at_moves_the_position_and_leaves_the_executor_alone() {
        let mut it = zombies();
        it.world.bind_selector("@e[type=zombie,limit=1]", ["z2"]);
        it.run_line("execute at @e[type=zombie,limit=1] run say hi");
        assert_eq!(
            it.effects[0].executor, None,
            "`at` does not change who runs it"
        );
    }

    #[test]
    fn as_then_at_uses_the_entity_just_bound() {
        let mut it = zombies();
        // `@s` needs no binding: it is whoever the enclosing `as` picked.
        let out = it.run_line("execute as @e[type=zombie] at @s run say hi");
        assert_eq!(out.success, 2);
    }

    #[test]
    fn s_resolves_to_the_current_executor() {
        let mut it = zombies();
        it.run_line("execute as @e[type=zombie] run execute if entity @s run say hi");
        assert_eq!(it.effects.len(), 2);
    }

    #[test]
    fn if_entity_is_false_with_no_executor() {
        let mut it = zombies();
        let out = it.run_line("execute if entity @s run say hi");
        assert_eq!(out, Outcome::FAILED);
        assert!(it.effects.is_empty());
    }

    #[test]
    fn a_condition_is_evaluated_once_per_context() {
        let mut it = zombies();
        it.world.bind_selector("@e[tag=chosen]", ["z2"]);
        // Only the context whose executor is also `@e[tag=chosen]` survives... but
        // `if entity` asks about the selector, not the executor, so both survive.
        let out = it.run_line("execute as @e[type=zombie] if entity @e[tag=chosen] run say hi");
        assert_eq!(out.success, 2);
    }

    #[test]
    fn the_context_is_restored_afterwards() {
        let mut it = zombies();
        it.run_line("execute as @e[type=zombie] run say inner");
        it.run_line("say outer");
        assert_eq!(it.effects.last().unwrap().executor, None);
    }

    #[test]
    fn positioned_moves_where_a_command_runs() {
        let mut it = zombies();
        it.run_line("execute positioned 1 2 3 run say a");
        it.run_line("execute positioned 1 2 3 positioned ~ ~1 ~ run say b");
        let at: Vec<[f64; 3]> = it.effects.iter().map(|e| e.position).collect();
        assert_eq!(at, vec![[1.0, 2.0, 3.0], [1.0, 3.0, 3.0]]);
    }

    #[test]
    fn a_bare_tilde_is_no_offset_at_all() {
        let mut it = zombies();
        it.run_line("execute positioned 4 5 6 positioned ~ ~ ~ run say a");
        assert_eq!(it.effects[0].position, [4.0, 5.0, 6.0]);
    }

    #[test]
    fn local_coordinates_follow_the_rotation_at_moved_to() {
        let mut it = zombies();
        // Facing yaw 90 is facing -X, so a step forward is a step west.
        let z1 = it.world.entity_mut("z1").expect("spawned");
        z1.rot = [90.0, 0.0];
        z1.pos = [0.0, 64.0, 0.0];
        it.world.bind_selector("@e[type=zombie]", ["z1"]);
        it.run_line("execute as @e[type=zombie] at @s positioned ^ ^ ^1 run say ahead");
        let at = it.effects[0].position;
        assert!((at[0] - -1.0).abs() < 1e-9, "{at:?}");
        assert!((at[1] - 64.0).abs() < 1e-9, "{at:?}");
        assert!(at[2].abs() < 1e-9, "{at:?}");
    }

    #[test]
    fn looking_up_makes_forward_up() {
        let mut it = zombies();
        let z1 = it.world.entity_mut("z1").expect("spawned");
        z1.rot = [0.0, -90.0];
        z1.pos = [0.0, 64.0, 0.0];
        it.world.bind_selector("@e[type=zombie]", ["z1"]);
        it.run_line("execute as @e[type=zombie] at @s positioned ^ ^ ^2 run say up");
        let at = it.effects[0].position;
        assert!(at[0].abs() < 1e-9, "{at:?}");
        assert!((at[1] - 66.0).abs() < 1e-9, "{at:?}");
        assert!(at[2].abs() < 1e-9, "{at:?}");
    }

    #[test]
    fn mixing_local_with_the_others_is_refused() {
        let mut it = zombies();
        assert_eq!(
            it.run_line("execute positioned ^ ~1 ^ run say hi"),
            Outcome::FAILED
        );
    }

    #[test]
    fn rotated_sets_the_frame_local_coordinates_use() {
        let mut it = zombies();
        it.run_line("execute positioned 0 0 0 rotated 180 0 positioned ^ ^ ^1 run say back");
        let at = it.effects[0].position;
        assert!((at[2] - -1.0).abs() < 1e-9, "{at:?}");
    }

    #[test]
    fn the_still_deferred_clauses_name_themselves() {
        let mut it = zombies();
        for line in [
            "execute in minecraft:overworld run say hi",
            "execute anchored eyes run say hi",
            "execute if block 0 0 0 stone run say hi",
            "execute if predicate ns:p run say hi",
        ] {
            it.diagnostics.clear();
            assert_eq!(it.run_line(line), Outcome::FAILED, "{line}");
            assert!(
                it.diagnostics.iter().any(|d| d.contains("M0-8b")),
                "{line}: {:?}",
                it.diagnostics
            );
        }
    }
}
