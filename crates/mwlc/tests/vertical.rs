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

/// Everything below runs the compiled program and asks what it computed, rather than
/// what it looks like. Spec section 6 says what the commands should be; these say what
/// they should mean.
mod expressions {
    use super::harness::{cost, local, run};

    fn value(src: &str) -> i32 {
        let mc = run(&format!("fn main() {{ {src} }}"));
        local(&mc, "main", "x").expect("x is set")
    }

    #[test]
    fn arithmetic_respects_precedence() {
        assert_eq!(value("let x = 2 + 3 * 4;"), 14);
        assert_eq!(value("let x = (2 + 3) * 4;"), 20);
        assert_eq!(value("let x = 10 - 2 - 3;"), 5);
    }

    #[test]
    fn division_and_remainder_are_floored_as_the_scoreboard_does_them() {
        // Spec section 6.2: this differs from Rust, which gives -3 and -1. Matching
        // vanilla costs one command; matching Rust would cost a sign correction on
        // every division.
        assert_eq!(value("let x = -7 / 2;"), -4);
        assert_eq!(value("let x = -7 % 2;"), 1);
        assert_eq!(value("let x = 7 / 2;"), 3);
    }

    #[test]
    fn unary_minus_and_not() {
        assert_eq!(value("let x = -5;"), -5);
        assert_eq!(value("let a = true; let x = !a;"), 0);
        assert_eq!(value("let a = false; let x = !a;"), 1);
    }

    #[test]
    fn comparisons_produce_one_or_zero() {
        assert_eq!(value("let x = 1 < 2;"), 1);
        assert_eq!(value("let x = 2 < 2;"), 0);
        assert_eq!(value("let x = 2 <= 2;"), 1);
        assert_eq!(value("let x = 3 > 2;"), 1);
        assert_eq!(value("let x = 2 >= 3;"), 0);
        assert_eq!(value("let x = 2 == 2;"), 1);
        assert_eq!(value("let x = 2 != 2;"), 0);
    }

    #[test]
    fn comparing_two_bindings_rather_than_a_constant() {
        assert_eq!(value("let a = 3; let b = 4; let x = a < b;"), 1);
        assert_eq!(value("let a = 3; let b = 4; let x = a == b;"), 0);
        assert_eq!(value("let a = 3; let b = 4; let x = a != b;"), 1);
    }

    #[test]
    fn and_and_or() {
        assert_eq!(value("let x = true && false;"), 0);
        assert_eq!(value("let x = true && true;"), 1);
        assert_eq!(value("let x = false || true;"), 1);
        assert_eq!(value("let x = false || false;"), 0);
    }

    #[test]
    fn bindings_read_back() {
        assert_eq!(value("let a = 6; let x = a * 7;"), 42);
    }

    #[test]
    fn assignment_and_compound_assignment() {
        assert_eq!(value("let mut x = 1; x = 9;"), 9);
        assert_eq!(value("let mut x = 1; x += 4;"), 5);
        assert_eq!(value("let mut x = 10; x -= 4;"), 6);
        assert_eq!(value("let mut x = 3; x *= 4;"), 12);
        assert_eq!(value("let mut x = 3; let n = 2; x += n;"), 5);
    }

    #[test]
    fn a_shadowing_let_is_a_different_binding() {
        // Both exist; the later one is what the name means from then on.
        let mc = run("fn main() { let x = 1; let y = x; let x = 2; }");
        assert_eq!(local(&mc, "main", "y"), Some(1));
        assert_eq!(local(&mc, "main", "x"), Some(2));
    }

    #[test]
    fn a_constant_binding_costs_exactly_one_command() {
        // `scoreboard players set` exists for this. Emitting a temporary and an
        // `operation =` would be two commands to do one thing.
        let mc = run("fn main() { let x = 5; }");
        assert_eq!(cost(&mc), 1);
    }

    #[test]
    fn adding_a_constant_costs_one_command() {
        let mc = run("fn main() { let mut x = 5; x += 1; }");
        assert_eq!(cost(&mc), 2);
    }

    #[test]
    fn comparing_against_a_constant_costs_one_command() {
        // `matches` takes a range, so no temporary has to be materialised for the
        // right-hand side.
        // And it stores straight into the binding: `execute store success score <x>`
        // already names a destination, so no temporary is needed either.
        let mc = run("fn main() { let a = 1; let x = a < 5; }");
        assert_eq!(cost(&mc), 2);
    }
}
