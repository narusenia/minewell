// SPDX-License-Identifier: MIT

//! Command syntax.
//!
//! Each command family is added by the task that implements its execution, so that
//! every variant here is backed by a behavioural test rather than only by a shape
//! assertion. [`Command::Unknown`] is the fallback that keeps unmodelled commands
//! runnable.

use crate::args::{Args, ParseError};
use crate::nbt::{Compound, NbtValue};
use crate::path::NbtPath;

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Scoreboard(Scoreboard),
    Data(Data),
    Function {
        id: String,
        args: FnArgs,
    },
    Return(Return),
    Execute(Execute),
    /// A command outside the modelled subset. Kept verbatim rather than rejected: a
    /// compiler emitting something unmodelled should still produce a usable trace.
    Unknown {
        name: String,
        args: String,
    },
}

/// Where a macro function's arguments come from.
#[derive(Debug, Clone, PartialEq)]
pub enum FnArgs {
    None,
    Inline(Compound),
    From {
        target: Target,
        path: Option<NbtPath>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Execute {
    pub clauses: Vec<Clause>,
    /// Absent for the bare conditional form, whose outcome is the conditions themselves.
    pub run: Option<Box<Command>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Clause {
    Cond {
        negated: bool,
        cond: Condition,
    },
    Store {
        success: bool,
        into: StoreTarget,
    },
    /// Parsed but not implemented; see `SPEC.md` §4.4.
    Deferred(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    Score {
        holder: String,
        objective: String,
        test: ScoreTest,
    },
    Data {
        target: Target,
        path: NbtPath,
    },
    /// Parsed but not implemented; see `SPEC.md` §4.4.
    Deferred(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScoreTest {
    Against {
        op: Cmp,
        holder: String,
        objective: String,
    },
    /// An open range: either bound may be absent.
    Matches { min: Option<i32>, max: Option<i32> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StoreTarget {
    Score {
        holder: String,
        objective: String,
    },
    Storage {
        id: String,
        path: NbtPath,
        tag: NumTag,
        scale: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumTag {
    Byte,
    Short,
    Int,
    Long,
    Float,
    Double,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Return {
    Value(i32),
    Fail,
    /// The function's outcome becomes the wrapped command's outcome.
    Run(Box<Command>),
}

/// Where NBT lives. Only [`Target::Storage`] executes; see `SPEC.md` §4.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Storage(String),
    Entity(String),
    Block(String),
}

impl Target {
    fn parse(args: &mut Args) -> Result<Target, ParseError> {
        let kind = args.word()?;
        Ok(match kind {
            "storage" => Target::Storage(args.word()?.to_owned()),
            "entity" => Target::Entity(args.word()?.to_owned()),
            "block" => {
                let (x, y, z) = (args.word()?, args.word()?, args.word()?);
                Target::Block(format!("{x} {y} {z}"))
            }
            other => return Err(unexpected(other)),
        })
    }

    pub fn describe(&self) -> &'static str {
        match self {
            Target::Storage(_) => "storage",
            Target::Entity(_) => "entity",
            Target::Block(_) => "block",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Data {
    Get {
        target: Target,
        path: Option<NbtPath>,
        scale: f64,
    },
    Merge {
        target: Target,
        value: NbtValue,
    },
    Remove {
        target: Target,
        path: NbtPath,
    },
    Modify {
        target: Target,
        path: NbtPath,
        kind: ModifyKind,
        source: Source,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifyKind {
    Set,
    Append,
    Prepend,
    Insert(i32),
    Merge,
}

impl ModifyKind {
    /// Whether the value the path addresses should be created as a list when missing.
    pub fn wants_list(&self) -> bool {
        matches!(
            self,
            ModifyKind::Append | ModifyKind::Prepend | ModifyKind::Insert(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Value(NbtValue),
    From {
        target: Target,
        path: Option<NbtPath>,
    },
    /// The source rendered as text, sliced by `[start, end)`.
    Str {
        target: Target,
        path: Option<NbtPath>,
        start: Option<i32>,
        end: Option<i32>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scoreboard {
    AddObjective(String),
    RemoveObjective(String),
    Get {
        holder: String,
        objective: String,
    },
    Set {
        holder: String,
        objective: String,
        value: i32,
    },
    /// `add` and `remove`, the latter parsed as a negated `add`.
    Add {
        holder: String,
        objective: String,
        delta: i32,
    },
    /// A missing objective resets the holder everywhere.
    Reset {
        holder: String,
        objective: Option<String>,
    },
    Operation {
        holder: String,
        objective: String,
        op: Op,
        source: String,
        source_objective: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Min,
    Max,
    Swap,
}

impl Op {
    fn parse(token: &str) -> Option<Op> {
        Some(match token {
            "=" => Op::Assign,
            "+=" => Op::Add,
            "-=" => Op::Sub,
            "*=" => Op::Mul,
            "/=" => Op::Div,
            "%=" => Op::Rem,
            "<" => Op::Min,
            ">" => Op::Max,
            "><" => Op::Swap,
            _ => return None,
        })
    }
}

impl Command {
    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let mut args = Args::new(line);
        let name = args.word()?.to_owned();
        // Dispatch arms for `data`, `function`, `return` and `execute` arrive with
        // their executors.
        match name.as_str() {
            "scoreboard" => Ok(Command::Scoreboard(scoreboard(&mut args)?)),
            "data" => Ok(Command::Data(data(&mut args)?)),
            "execute" => Ok(Command::Execute(execute(&mut args)?)),
            "function" => {
                let id = args.word()?.to_owned();
                let fn_args = if args.is_empty() {
                    FnArgs::None
                } else if args.literal("with") {
                    let target = Target::parse(&mut args)?;
                    FnArgs::From {
                        target,
                        path: optional_path(&mut args)?,
                    }
                } else {
                    match args.value()? {
                        NbtValue::Compound(fields) => FnArgs::Inline(fields),
                        other => {
                            return Err(ParseError::new(
                                0,
                                format!(
                                    "function arguments must be a compound, found {}",
                                    other.tag_name()
                                ),
                            ));
                        }
                    }
                };
                args.end()?;
                Ok(Command::Function { id, args: fn_args })
            }
            "return" => Ok(Command::Return(if args.literal("fail") {
                args.end()?;
                Return::Fail
            } else if args.literal("run") {
                Return::Run(Box::new(Command::parse(args.rest())?))
            } else {
                let value = args.int()?;
                args.end()?;
                Return::Value(value)
            })),
            _ => Ok(Command::Unknown {
                name,
                args: args.rest().to_owned(),
            }),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Command::Scoreboard(_) => "scoreboard",
            Command::Data(_) => "data",
            Command::Function { .. } => "function",
            Command::Return(_) => "return",
            Command::Execute(_) => "execute",
            Command::Unknown { name, .. } => name,
        }
    }
}

fn execute(args: &mut Args) -> Result<Execute, ParseError> {
    let mut clauses = Vec::new();
    loop {
        if args.is_empty() {
            return Ok(Execute { clauses, run: None });
        }
        if args.literal("run") {
            let run = Box::new(Command::parse(args.rest())?);
            return Ok(Execute {
                clauses,
                run: Some(run),
            });
        }
        clauses.push(clause(args)?);
    }
}

fn clause(args: &mut Args) -> Result<Clause, ParseError> {
    let word = args.word()?;
    match word {
        "if" | "unless" => Ok(Clause::Cond {
            negated: word == "unless",
            cond: condition(args)?,
        }),
        "store" => {
            let what = args.word()?;
            let success = match what {
                "result" => false,
                "success" => true,
                other => return Err(unexpected(other)),
            };
            Ok(Clause::Store {
                success,
                into: store_target(args)?,
            })
        }
        // Everything that needs a world. Consumed so the rest of the line still parses.
        "as" | "at" | "positioned" | "rotated" | "in" | "anchored" | "align" | "facing" | "on"
        | "summon" => {
            args.word()?;
            Ok(Clause::Deferred(word.to_owned()))
        }
        other => Err(unexpected(other)),
    }
}

fn condition(args: &mut Args) -> Result<Condition, ParseError> {
    let kind = args.word()?;
    match kind {
        "score" => {
            let holder = args.word()?.to_owned();
            let objective = args.word()?.to_owned();
            let word = args.word()?;
            let test = if word == "matches" {
                let range = args.word()?;
                let (min, max) = parse_range(range)?;
                ScoreTest::Matches { min, max }
            } else {
                let op = match word {
                    "<" => Cmp::Lt,
                    "<=" => Cmp::Le,
                    "=" => Cmp::Eq,
                    ">=" => Cmp::Ge,
                    ">" => Cmp::Gt,
                    other => return Err(unexpected(other)),
                };
                ScoreTest::Against {
                    op,
                    holder: args.word()?.to_owned(),
                    objective: args.word()?.to_owned(),
                }
            };
            Ok(Condition::Score {
                holder,
                objective,
                test,
            })
        }
        "data" => {
            let target = Target::parse(args)?;
            let path = args.path()?;
            Ok(Condition::Data { target, path })
        }
        "entity" | "block" | "predicate" | "biome" | "blocks" | "dimension" | "loaded" => {
            // Consumed whole, so the deferral is reported when it runs rather than as
            // a syntax error the caller cannot act on.
            args.rest();
            Ok(Condition::Deferred(kind.to_owned()))
        }
        other => Err(unexpected(other)),
    }
}

/// `3`, `3..`, `..3`, `1..5`.
fn parse_range(text: &str) -> Result<(Option<i32>, Option<i32>), ParseError> {
    let bound = |s: &str| -> Result<Option<i32>, ParseError> {
        if s.is_empty() {
            Ok(None)
        } else {
            s.parse()
                .map(Some)
                .map_err(|_| ParseError::new(0, format!("bad range bound '{s}'")))
        }
    };
    match text.split_once("..") {
        Some((min, max)) => Ok((bound(min)?, bound(max)?)),
        None => {
            let exact = bound(text)?;
            Ok((exact, exact))
        }
    }
}

fn store_target(args: &mut Args) -> Result<StoreTarget, ParseError> {
    let kind = args.word()?;
    match kind {
        "score" => Ok(StoreTarget::Score {
            holder: args.word()?.to_owned(),
            objective: args.word()?.to_owned(),
        }),
        "storage" => {
            let id = args.word()?.to_owned();
            let path = args.path()?;
            let word = args.word()?;
            let tag = match word {
                "byte" => NumTag::Byte,
                "short" => NumTag::Short,
                "int" => NumTag::Int,
                "long" => NumTag::Long,
                "float" => NumTag::Float,
                "double" => NumTag::Double,
                other => return Err(unexpected(other)),
            };
            let word = args.word()?;
            let scale = word
                .parse()
                .map_err(|_| ParseError::new(0, format!("expected a scale, found '{word}'")))?;
            Ok(StoreTarget::Storage {
                id,
                path,
                tag,
                scale,
            })
        }
        other => Err(unexpected(other)),
    }
}

fn data(args: &mut Args) -> Result<Data, ParseError> {
    let action = args.word()?;
    let target = Target::parse(args)?;
    match action {
        "get" => {
            let path = if args.is_empty() {
                None
            } else {
                Some(args.path()?)
            };
            // Vanilla only allows a scale once a path is given, so the ambiguity
            // between "next word is a path" and "next word is a scale" never arises.
            let scale = if args.is_empty() {
                1.0
            } else {
                let word = args.word()?;
                word.parse()
                    .map_err(|_| ParseError::new(0, format!("expected a scale, found '{word}'")))?
            };
            args.end()?;
            Ok(Data::Get {
                target,
                path,
                scale,
            })
        }
        "merge" => {
            let value = args.value()?;
            args.end()?;
            Ok(Data::Merge { target, value })
        }
        "remove" => {
            let path = args.path()?;
            args.end()?;
            Ok(Data::Remove { target, path })
        }
        "modify" => {
            let path = args.path()?;
            let word = args.word()?;
            let kind = match word {
                "set" => ModifyKind::Set,
                "append" => ModifyKind::Append,
                "prepend" => ModifyKind::Prepend,
                "merge" => ModifyKind::Merge,
                "insert" => ModifyKind::Insert(args.int()?),
                other => return Err(unexpected(other)),
            };
            let source = source(args)?;
            args.end()?;
            Ok(Data::Modify {
                target,
                path,
                kind,
                source,
            })
        }
        other => Err(unexpected(other)),
    }
}

fn source(args: &mut Args) -> Result<Source, ParseError> {
    let word = args.word()?;
    match word {
        "value" => Ok(Source::Value(args.value()?)),
        "from" => {
            let target = Target::parse(args)?;
            let path = optional_path(args)?;
            Ok(Source::From { target, path })
        }
        "string" => {
            let target = Target::parse(args)?;
            let path = optional_path(args)?;
            let start = optional_int(args)?;
            let end = optional_int(args)?;
            Ok(Source::Str {
                target,
                path,
                start,
                end,
            })
        }
        other => Err(unexpected(other)),
    }
}

fn optional_path(args: &mut Args) -> Result<Option<NbtPath>, ParseError> {
    if args.is_empty() {
        Ok(None)
    } else {
        Ok(Some(args.path()?))
    }
}

fn optional_int(args: &mut Args) -> Result<Option<i32>, ParseError> {
    if args.is_empty() {
        Ok(None)
    } else {
        Ok(Some(args.int()?))
    }
}

fn scoreboard(args: &mut Args) -> Result<Scoreboard, ParseError> {
    let section = args.word()?;
    match section {
        "objectives" => {
            let action = args.word()?;
            let name = args.word()?.to_owned();
            match action {
                "add" => {
                    // Criteria and display name are parsed and discarded: nothing here
                    // observes anything but `dummy`.
                    args.word()?;
                    args.rest();
                    Ok(Scoreboard::AddObjective(name))
                }
                "remove" => {
                    args.end()?;
                    Ok(Scoreboard::RemoveObjective(name))
                }
                other => Err(unexpected(other)),
            }
        }
        "players" => {
            let action = args.word()?;
            let holder = args.word()?.to_owned();
            match action {
                "reset" => {
                    let objective = args.peek().map(str::to_owned);
                    if objective.is_some() {
                        args.word()?;
                    }
                    args.end()?;
                    Ok(Scoreboard::Reset { holder, objective })
                }
                "get" => {
                    let objective = args.word()?.to_owned();
                    args.end()?;
                    Ok(Scoreboard::Get { holder, objective })
                }
                "set" | "add" | "remove" => {
                    let objective = args.word()?.to_owned();
                    let value = args.int()?;
                    args.end()?;
                    Ok(match action {
                        "set" => Scoreboard::Set {
                            holder,
                            objective,
                            value,
                        },
                        // `remove` is `add` with the sign flipped, so the executor has
                        // one arithmetic path instead of two.
                        "remove" => Scoreboard::Add {
                            holder,
                            objective,
                            delta: value.wrapping_neg(),
                        },
                        _ => Scoreboard::Add {
                            holder,
                            objective,
                            delta: value,
                        },
                    })
                }
                "operation" => {
                    let objective = args.word()?.to_owned();
                    let token = args.word()?;
                    let op = Op::parse(token).ok_or_else(|| unexpected(token))?;
                    let source = args.word()?.to_owned();
                    let source_objective = args.word()?.to_owned();
                    args.end()?;
                    Ok(Scoreboard::Operation {
                        holder,
                        objective,
                        op,
                        source,
                        source_objective,
                    })
                }
                other => Err(unexpected(other)),
            }
        }
        other => Err(unexpected(other)),
    }
}

fn unexpected(token: &str) -> ParseError {
    ParseError {
        at: 0,
        message: format!("unexpected '{token}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unmodelled_command_keeps_its_name_and_arguments_verbatim() {
        assert_eq!(
            Command::parse("say hi   there").unwrap(),
            Command::Unknown {
                name: "say".into(),
                args: "hi   there".into(),
            }
        );
    }

    #[test]
    fn a_command_with_no_arguments_has_empty_arguments() {
        assert_eq!(
            Command::parse("reload").unwrap(),
            Command::Unknown {
                name: "reload".into(),
                args: String::new(),
            }
        );
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(Command::parse("  say hi  ").unwrap().name(), "say");
    }

    #[test]
    fn an_empty_line_is_not_a_command() {
        assert!(Command::parse("   ").is_err());
    }
}
