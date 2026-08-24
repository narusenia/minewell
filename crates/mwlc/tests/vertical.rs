// SPDX-License-Identifier: MIT

//! End to end: `.mwl` in, behaviour out.
//!
//! M1's completion criterion, and the shape every later milestone's tests take.

mod harness;

use tinymcf::Effect;

#[test]
fn hello_world_goes_all_the_way_through() {
    let mc = harness::run(r#"fn main() { raw!("say hi"); }"#);
    assert_eq!(
        mc.effects,
        vec![Effect {
            name: "say".into(),
            args: "hi".into()
        }]
    );
}

#[test]
fn statements_run_in_order() {
    let mc = harness::run(r#"fn main() { raw!("say one"); raw!("say two"); }"#);
    assert_eq!(
        mc.effects
            .iter()
            .map(|e| e.args.as_str())
            .collect::<Vec<_>>(),
        vec!["one", "two"]
    );
}

#[test]
fn a_debug_build_runs_the_same_as_it_reads() {
    // The `# test.mwl:1` comments a debug build inserts are comments to Minecraft too.
    let mc = harness::run(r#"fn main() { raw!("say hi"); }"#);
    assert_eq!(mc.commands_run, 1, "the comment is not a command");
}

#[test]
fn other_functions_are_loaded_but_not_called() {
    let mut mc =
        harness::load(r#"fn main() { raw!("say main"); } fn other() { raw!("say other"); }"#);
    mc.call("test:other");
    assert_eq!(mc.effects.len(), 1);
    assert_eq!(mc.effects[0].args, "other");
}

#[test]
fn an_empty_function_does_nothing_and_says_nothing() {
    let mc = harness::run("fn main() {}");
    assert!(mc.effects.is_empty());
    assert!(mc.diagnostics.is_empty());
}
