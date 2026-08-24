//! Command syntax.
//!
//! Each command family is added by the task that implements its execution, so that
//! every variant here is backed by a behavioural test rather than only by a shape
//! assertion. [`Command::Unknown`] is the fallback that keeps unmodelled commands
//! runnable.

use crate::args::{Args, ParseError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// A command outside the modelled subset. Kept verbatim rather than rejected: a
    /// compiler emitting something unmodelled should still produce a usable trace.
    Unknown { name: String, args: String },
}

impl Command {
    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let mut args = Args::new(line);
        let name = args.word()?.to_owned();
        // Dispatch arms for `scoreboard`, `data`, `function`, `return` and `execute`
        // arrive with their executors.
        Ok(Command::Unknown {
            name,
            args: args.rest().to_owned(),
        })
    }

    pub fn name(&self) -> &str {
        match self {
            Command::Unknown { name, .. } => name,
        }
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
