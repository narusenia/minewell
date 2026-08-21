//! A tiny interpreter for a subset of Minecraft's mcfunction.
//!
//! Exists so that a transpiler targeting mcfunction can assert on *behaviour*
//! (`fact(5) == 120`) instead of on generated text. Deliberately depends on
//! nothing else in this repository.

pub mod nbt;
