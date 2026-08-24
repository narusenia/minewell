// SPDX-License-Identifier: MIT

//! The acceptance tests for `tinymcf`: handwritten mcfunction, run for its behaviour.
//!
//! These are the ones that decide whether the interpreter can be trusted as the
//! measuring instrument the compiler is developed against. Each fixture is written the
//! way generated code will be written, so that the shapes the compiler is going to
//! emit are the shapes proven to work here.

use tinymcf::Interpreter;
use tinymcf::nbt::NbtValue;
use tinymcf::path::NbtPath;

fn pack() -> Interpreter {
    let mut mc = Interpreter::default();
    for (id, source) in [
        ("test:load", include_str!("fixtures/load.mcfunction")),
        ("test:fact", include_str!("fixtures/fact.mcfunction")),
        (
            "test:fact_loop",
            include_str!("fixtures/fact_loop.mcfunction"),
        ),
        ("test:fib", include_str!("fixtures/fib.mcfunction")),
        (
            "test:fib_loop",
            include_str!("fixtures/fib_loop.mcfunction"),
        ),
        ("test:sum", include_str!("fixtures/sum.mcfunction")),
        (
            "test:sum_loop",
            include_str!("fixtures/sum_loop.mcfunction"),
        ),
        ("test:nth", include_str!("fixtures/nth.mcfunction")),
    ] {
        mc.load(id, source).expect("fixtures parse");
    }
    mc.call("test:load");
    mc
}

fn score(mc: &Interpreter, holder: &str) -> Option<i32> {
    mc.world
        .scoreboard
        .get("obj", holder)
        .expect("objective exists")
}

fn quiet(mc: &Interpreter) {
    assert!(mc.diagnostics.is_empty(), "{:?}", mc.diagnostics);
}

#[test]
fn factorial() {
    for (n, expected) in [(0, 1), (1, 1), (5, 120), (10, 3_628_800)] {
        let mut mc = pack();
        mc.run_line(&format!("scoreboard players set $n obj {n}"));
        mc.call("test:fact");
        assert_eq!(score(&mc, "$result"), Some(expected), "{n}!");
        quiet(&mc);
    }
}

#[test]
fn fibonacci() {
    for (n, expected) in [(0, 0), (1, 1), (2, 1), (10, 55), (30, 832_040)] {
        let mut mc = pack();
        mc.run_line(&format!("scoreboard players set $n obj {n}"));
        mc.call("test:fib");
        assert_eq!(score(&mc, "$result"), Some(expected), "fib({n})");
        quiet(&mc);
    }
}

#[test]
fn destructive_list_iteration() {
    let mut mc = pack();
    mc.run_line("data modify storage test:mw items set value [3,4,5]");
    mc.call("test:sum");
    assert_eq!(score(&mc, "$sum"), Some(12));
    quiet(&mc);

    // The source list is untouched; only the copy was consumed.
    assert_eq!(
        NbtPath::parse("items")
            .unwrap()
            .resolve(mc.world.storage("test:mw")),
        vec![NbtValue::List(vec![
            NbtValue::Int(3),
            NbtValue::Int(4),
            NbtValue::Int(5),
        ])]
    );

    // And it does not reach for a macro to do it.
    assert!(!mc.report().per_function.is_empty());
}

#[test]
fn an_empty_list_iterates_zero_times() {
    let mut mc = pack();
    mc.run_line("data modify storage test:mw items set value []");
    mc.call("test:sum");
    assert_eq!(score(&mc, "$sum"), Some(0));
    quiet(&mc);
}

#[test]
fn a_runtime_index_reaches_the_element_through_a_macro() {
    let mut mc = pack();
    mc.run_line("data modify storage test:mw items set value [10,20,30]");
    mc.run_line("function test:nth {i:1}");
    assert_eq!(score(&mc, "$out"), Some(20));
    quiet(&mc);
}

#[test]
fn what_a_loop_costs_is_measurable() {
    let mut mc = pack();
    mc.run_line("scoreboard players set $n obj 5");
    let before = mc.commands_run;
    mc.call("test:fact");

    // `test:fact` runs two commands. `test:fact_loop` then runs four apiece for
    // n = 5, 4, 3, 2 — the guard `execute` plus three more — and two for n = 1, where
    // the guard holds and is charged alongside the `return` it runs. The outermost
    // call is not charged at all: `call` enters the body rather than going through a
    // `function` command.
    assert_eq!(mc.commands_run - before, 2 + 4 * 4 + 2);
    assert_eq!(mc.report().max_depth, 6);
}
