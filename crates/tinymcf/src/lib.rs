//! A tiny interpreter for a subset of Minecraft's mcfunction.
//!
//! Exists so that a transpiler targeting mcfunction can assert on *behaviour*
//! (`fact(5) == 120`) instead of on generated text. Deliberately depends on
//! nothing else in this repository.

pub mod args;
pub mod command;
pub mod nbt;
pub mod path;
pub mod snbt;
pub mod world;

/// A hard failure: vanilla would reject the command outright and print red text.
///
/// This is not the same as a command that merely does nothing. Commands that
/// no-op report a success count of 0 and are not errors — that distinction is
/// what `execute store success` observes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    NoSuchObjective(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoSuchObjective(name) => write!(f, "unknown scoreboard objective '{name}'"),
        }
    }
}

impl std::error::Error for Error {}
