// SPDX-License-Identifier: MIT

//! Running commands against a [`World`].
//!
//! See `SPEC.md` §2: a command that fails does not abort anything. Execution walks on
//! to the next line and the failure is recorded, because that is what vanilla does and
//! because the whole point of this interpreter is that nothing fails silently.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::args::ParseError;
use crate::command::{Command, Data, ModifyKind, Op, Return, Scoreboard, Source, Target};
use crate::nbt::{Compound, NbtValue};
use crate::path::NbtPath;
use crate::world::World;

fn unmodelled(target: &Target) -> String {
    format!(
        "{} targets are not modelled by tinymcf; see SPEC.md section 1",
        target.describe()
    )
}

/// Vanilla refuses a source or a `data get` that is ambiguous.
fn exactly_one(matches: &[NbtValue]) -> Result<NbtValue, String> {
    match matches {
        [one] => Ok(one.clone()),
        [] => Err("found no elements matching the path".to_owned()),
        many => Err(format!("found {} elements matching the path", many.len())),
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
    functions: BTreeMap<String, Rc<Vec<Command>>>,
    /// 0 at the top level. `return` outside a function is an error.
    depth: usize,
    over_budget: bool,
}

impl Default for Interpreter {
    fn default() -> Self {
        Interpreter {
            world: World::default(),
            diagnostics: Vec::new(),
            budget: DEFAULT_BUDGET,
            commands_run: 0,
            functions: BTreeMap::new(),
            depth: 0,
            over_budget: false,
        }
    }
}

impl Interpreter {
    /// Parses a function body and registers it. Blank lines and `#` comments are
    /// dropped; everything else is parsed now, so syntax errors surface at load time.
    pub fn load(&mut self, id: &str, source: &str) -> Result<(), ParseError> {
        let mut commands = Vec::new();
        for line in source.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            commands.push(Command::parse(line)?);
        }
        self.functions.insert(id.to_owned(), Rc::new(commands));
        Ok(())
    }

    /// Runs a loaded function, as the `function` command does.
    pub fn call(&mut self, id: &str) -> Outcome {
        match self.call_inner(id) {
            Flow::Next(outcome) | Flow::Return(outcome) => outcome,
            Flow::Halt => Outcome::FAILED,
        }
    }

    fn call_inner(&mut self, id: &str) -> Flow {
        let Some(body) = self.functions.get(id).cloned() else {
            return Flow::Next(self.fail(format!("unknown function '{id}'")));
        };
        self.depth += 1;
        let mut ran = 0i32;
        let mut outcome = None;
        for command in body.iter() {
            match self.step(command) {
                Flow::Next(_) => ran = ran.saturating_add(1),
                Flow::Return(o) => {
                    outcome = Some(o);
                    break;
                }
                Flow::Halt => {
                    self.depth -= 1;
                    return Flow::Halt;
                }
            }
        }
        self.depth -= 1;
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
            Command::Function(id) => self.call_inner(id),
            Command::Return(kind) => self.ret(kind),
            // Unmodelled commands are assumed to have worked; M0-10 records them.
            Command::Unknown { .. } => Flow::Next(Outcome::ok(1)),
        }
    }

    fn ret(&mut self, kind: &Return) -> Flow {
        if self.depth == 0 {
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

    fn data(&mut self, cmd: &Data) -> Outcome {
        match cmd {
            Data::Get {
                target,
                path,
                scale,
            } => {
                let root = match self.read_target(target) {
                    Ok(root) => root,
                    Err(message) => return self.fail(message),
                };
                let Some(path) = path else {
                    return Outcome::ok(1);
                };
                match exactly_one(&path.resolve(&root)) {
                    Err(message) => self.fail(message),
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
                    Err(message) => return self.fail(message),
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
    fn read_target(&self, target: &Target) -> Result<NbtValue, String> {
        match target {
            Target::Storage(id) => Ok(self.world.storage(id).clone()),
            other => Err(unmodelled(other)),
        }
    }

    fn with_target(
        &mut self,
        target: &Target,
        f: impl FnOnce(&mut NbtValue) -> Outcome,
    ) -> Outcome {
        match target {
            Target::Storage(id) => f(self.world.storage_mut(id)),
            other => {
                let message = unmodelled(other);
                self.fail(message)
            }
        }
    }

    fn resolve_source(&self, source: &Source) -> Result<NbtValue, String> {
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

    fn read_source(&self, target: &Target, path: Option<&NbtPath>) -> Result<NbtValue, String> {
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
        let (it, out) = run(&["data get storage ns:mw missing"]);
        assert_eq!(out, Outcome::FAILED);
        assert!(!it.diagnostics.is_empty());

        let (_, out) = run(&[
            "data modify storage ns:mw l set value [1,2]",
            "data get storage ns:mw l[]",
        ]);
        assert_eq!(out, Outcome::FAILED);
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
    fn entity_and_block_targets_parse_but_say_they_are_not_modelled() {
        let (it, out) = run(&["data get entity @s Health"]);
        assert_eq!(out, Outcome::FAILED);
        assert!(
            it.diagnostics[0].contains("not modelled"),
            "{:?}",
            it.diagnostics
        );

        let (it, out) = run(&["data get block 0 0 0 Items"]);
        assert_eq!(out, Outcome::FAILED);
        assert!(it.diagnostics[0].contains("not modelled"));
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

    #[test]
    fn runaway_recursion_stops_at_the_command_budget() {
        let mut it = pack(&[("ns:loop", "function ns:loop")]);
        it.budget = 1000;
        let out = it.run_line("function ns:loop");
        assert_eq!(out, Outcome::FAILED);
        assert!(
            it.diagnostics
                .iter()
                .any(|d| d.contains("maxCommandChainLength")),
            "{:?}",
            it.diagnostics
        );
        assert_eq!(it.commands_run, 1000);
    }
}
