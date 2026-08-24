//! Running commands against a [`World`].
//!
//! See `SPEC.md` §2: a command that fails does not abort anything. Execution walks on
//! to the next line and the failure is recorded, because that is what vanilla does and
//! because the whole point of this interpreter is that nothing fails silently.

use crate::command::{Command, Op, Scoreboard};
use crate::world::World;

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

#[derive(Debug, Default)]
pub struct Interpreter {
    pub world: World,
    /// The red text vanilla would print. Recorded so a test can assert *why* a command
    /// did nothing.
    pub diagnostics: Vec<String>,
}

impl Interpreter {
    /// Parses and runs one line. A line that does not parse is a failure like any
    /// other, so a bad line in the middle of a function does not hide the rest of it.
    pub fn run_line(&mut self, line: &str) -> Outcome {
        match Command::parse(line) {
            Ok(command) => self.run(&command),
            Err(e) => self.fail(format!("{line}: {e}")),
        }
    }

    pub fn run(&mut self, command: &Command) -> Outcome {
        match command {
            Command::Scoreboard(cmd) => self.scoreboard(cmd),
            // Unmodelled commands are assumed to have worked; M0-10 records them.
            Command::Unknown { .. } => Outcome::ok(1),
        }
    }

    fn fail(&mut self, message: impl Into<String>) -> Outcome {
        self.diagnostics.push(message.into());
        Outcome::FAILED
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
        assert_eq!(it.world.scoreboard.has_objective("obj"), false);
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
