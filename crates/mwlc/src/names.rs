// SPDX-License-Identifier: MIT

//! The names a generated datapack uses (requirements section 3.3).
//!
//! They are load-bearing, and more than one stage writes them: `mir` and `emit` put
//! them into commands, while `text!` puts them into JSON that names a score or a
//! storage path directly. A mismatch between the two would fail silently in game,
//! which is exactly the class of bug this compiler exists to prevent — so each name
//! is decided in one place, here.

/// The fake player a binding's score is kept under: `$<function>.<binding>`.
///
/// Qualified by function so two functions' locals cannot collide, and prefixed with
/// `$` so it can never be a real player's name.
pub fn fake_player(owner: &str, binding: &str) -> String {
    format!("${owner}.{binding}")
}

/// Where a binding that lives in storage is kept: `mw.vars.<function>.<binding>`.
pub fn var_path(owner: &str, binding: &str) -> String {
    format!("mw.vars.{owner}.{binding}")
}

/// The objective holding the bindings the author wrote.
pub fn var_objective(namespace: &str) -> String {
    format!("{namespace}.v")
}

/// The objective holding compiler temporaries.
pub fn temp_objective(namespace: &str) -> String {
    format!("{namespace}.t")
}

/// The one storage a pack uses, split by root path (requirements section 3.3).
pub fn storage(namespace: &str) -> String {
    format!("{namespace}:mw")
}
