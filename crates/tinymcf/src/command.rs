//! Command syntax.
//!
//! Each command family is added by the task that implements its execution, so that
//! every variant here is backed by a behavioural test rather than only by a shape
//! assertion. [`Command::Unknown`] is the fallback that keeps unmodelled commands
//! runnable.

use crate::args::{Args, ParseError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Scoreboard(Scoreboard),
    /// A command outside the modelled subset. Kept verbatim rather than rejected: a
    /// compiler emitting something unmodelled should still produce a usable trace.
    Unknown {
        name: String,
        args: String,
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
            _ => Ok(Command::Unknown {
                name,
                args: args.rest().to_owned(),
            }),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Command::Scoreboard(_) => "scoreboard",
            Command::Unknown { name, .. } => name,
        }
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
