// SPDX-License-Identifier: MIT

//! The minewell compiler.
//!
//! `.mwl` source becomes a Minecraft datapack through four stages, each its own
//! module: `syntax` produces an AST, `hir` resolves and type checks it, `mir` lowers
//! it to basic blocks with virtual registers, and `emit` writes the commands out.
//!
//! This crate does not depend on `tinymcf`, in either direction. The interpreter is
//! independently publishable, and the two only meet in tests.

pub mod diagnostics;
pub mod emit;
pub mod hir;
pub mod mir;
pub mod syntax;
