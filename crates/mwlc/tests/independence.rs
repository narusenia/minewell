// SPDX-License-Identifier: MIT

//! The compiler must not depend on the interpreter.
//!
//! `tinymcf` is meant to be publishable on its own, and the compiler must not be able
//! to reach for it as a shortcut — a compiler that can run its own output is a
//! compiler that will start trusting it. They meet in dev-dependencies and nowhere
//! else. This is easy to violate by accident and invisible in review, so it is a test.

/// The `[dependencies]` table, as raw lines.
fn dependencies(manifest: &str) -> Vec<&str> {
    manifest
        .lines()
        .skip_while(|line| line.trim() != "[dependencies]")
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('['))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

#[test]
fn the_compiler_does_not_depend_on_the_interpreter() {
    let manifest = include_str!("../Cargo.toml");
    let offending: Vec<_> = dependencies(manifest)
        .into_iter()
        .filter(|line| line.contains("tinymcf"))
        .collect();
    assert!(
        offending.is_empty(),
        "mwlc must not depend on tinymcf: {offending:?}"
    );
}

#[test]
fn the_dependency_reader_finds_what_is_there() {
    // Otherwise the test above passes by failing to read the file.
    let manifest =
        "[package]\nname = \"x\"\n\n[dependencies]\ntinymcf = \"1\"\nserde = \"1\"\n\n[lints]\n";
    assert_eq!(
        dependencies(manifest),
        vec!["tinymcf = \"1\"", "serde = \"1\""]
    );
}
