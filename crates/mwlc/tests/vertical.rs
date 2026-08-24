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

/// Control flow, checked by what the program computes rather than by what it emits.
/// The lowering is in spec section 6.6 onwards; these say it means the right thing.
mod control_flow {
    use super::harness::{cost, load, local, run};

    fn value(src: &str) -> i32 {
        let mc = run(&format!("fn main() {{ {src} }}"));
        local(&mc, "main", "x").expect("x is set")
    }

    #[test]
    fn an_if_runs_only_when_its_condition_holds() {
        assert_eq!(value("let mut x = 0; if true { x = 1; }"), 1);
        assert_eq!(value("let mut x = 0; if false { x = 1; }"), 0);
    }

    #[test]
    fn an_else_runs_when_the_condition_does_not() {
        assert_eq!(
            value("let mut x = 0; if false { x = 1; } else { x = 2; }"),
            2
        );
        assert_eq!(
            value("let mut x = 0; if true { x = 1; } else { x = 2; }"),
            1
        );
    }

    #[test]
    fn else_if_chains() {
        let src = "let n = 2; let mut x = 0;
                   if n == 1 { x = 10; } else if n == 2 { x = 20; } else { x = 30; }";
        assert_eq!(value(src), 20);
    }

    #[test]
    fn conditions_can_be_comparisons_of_bindings() {
        assert_eq!(
            value("let a = 3; let b = 4; let mut x = 0; if a < b { x = 1; }"),
            1
        );
    }

    #[test]
    fn a_while_loop_runs_until_its_condition_fails() {
        assert_eq!(value("let mut x = 0; while x < 5 { x += 1; }"), 5);
    }

    #[test]
    fn a_while_loop_whose_condition_starts_false_never_runs() {
        assert_eq!(value("let mut x = 7; while x < 5 { x += 1; }"), 7);
    }

    #[test]
    fn break_leaves_the_loop() {
        assert_eq!(
            value("let mut x = 0; loop { x += 1; if x == 3 { break; } }"),
            3
        );
    }

    #[test]
    fn break_leaves_only_the_innermost_loop() {
        let src = "let mut x = 0; let mut i = 0;
                   while i < 3 {
                       i += 1;
                       let mut j = 0;
                       loop { j += 1; if j == 2 { break; } }
                       x += j;
                   }";
        assert_eq!(
            value(src),
            6,
            "the inner loop ran twice on each of three passes"
        );
    }

    #[test]
    fn continue_skips_the_rest_of_the_iteration() {
        let src = "let mut x = 0; let mut i = 0;
                   while i < 5 {
                       i += 1;
                       if i == 3 { continue; }
                       x += i;
                   }";
        assert_eq!(value(src), 12, "1 + 2 + 4 + 5");
    }

    #[test]
    fn return_leaves_the_function_from_inside_a_loop() {
        let src = "let mut x = 0;
                   while x < 10 {
                       x += 1;
                       if x == 4 { return; }
                   }
                   x = 99;";
        assert_eq!(value(src), 4, "the assignment after the loop never ran");
    }

    #[test]
    fn return_leaves_the_function_from_inside_a_branch() {
        let src = "let mut x = 1; if x == 1 { x = 2; return; } x = 3;";
        assert_eq!(value(src), 2);
    }

    #[test]
    fn loops_nest_and_both_conditions_are_respected() {
        let src = "let mut x = 0; let mut i = 0;
                   while i < 3 {
                       let mut j = 0;
                       while j < 4 { j += 1; x += 1; }
                       i += 1;
                   }";
        assert_eq!(value(src), 12);
    }

    #[test]
    fn a_single_statement_if_stays_inline() {
        // No extra function, and the whole thing is one command.
        let mc = load(r#"fn main() { let a = 1; if a == 1 { raw!("say hi"); } }"#);
        let mut mc = mc;
        mc.call("test:main");
        // `set`, the `execute`, and the `say` it runs. An `execute ... run` is
        // charged for both halves, in tinymcf and in the game.
        assert_eq!(cost(&mc), 3);
        assert_eq!(mc.effects.len(), 1);
    }

    #[test]
    fn no_inline_forces_a_function_even_for_one_statement() {
        let mut mc = load(r#"fn main() { let a = 1; #[no_inline] if a == 1 { raw!("say hi"); } }"#);
        mc.call("test:main");
        // One more than the inline version: `set`, `execute`, `function`, `say`.
        assert_eq!(cost(&mc), 4);
        assert_eq!(mc.effects.len(), 1);
    }

    #[test]
    fn a_loop_with_no_escapes_costs_nothing_extra_per_iteration() {
        // `set` and the call that starts it; then three iterations of
        // (guard, body, tail call); then the guard that fails and the `return` it
        // runs. Nothing else: no control register, no bookkeeping.
        let mut mc = load("fn main() { let mut x = 0; while x < 3 { x += 1; } }");
        mc.call("test:main");
        assert_eq!(cost(&mc), 2 + 3 * 3 + 2);
    }

    #[test]
    fn the_control_register_never_appears_when_nothing_escapes() {
        // Paying for `break` bookkeeping in a loop that has no `break` would be a tax
        // on every loop.
        let mc = load("fn main() { let mut x = 0; while x < 3 { x += 1; } }");
        assert_eq!(
            mc.world
                .scoreboard
                .get("test.v", "$main.ctl")
                .expect("objective exists"),
            None
        );
    }
}

/// Not an assertion: a way to read the generated commands while working on lowering.
///
/// `cargo test -p mwlc --test vertical print_generated -- --ignored --nocapture`
#[test]
#[ignore = "prints rather than asserting"]
fn print_generated_commands() {
    for src in [
        "fn main() { let mut x = 0; while x < 3 { x += 1; } }",
        "fn fact(n: i32) -> i32 { if n <= 1 { return 1; } return n * fact(n - 1); }
         fn main() { let x = fact(3); }",
    ] {
        println!("=== {src}");
        let options = mwlc::emit::Options {
            profile: mwlc::emit::Profile::Release,
            ..Default::default()
        };
        let pack = mwlc::driver::compile(src, "test", &options).unwrap();
        for (path, text) in &pack.files {
            if path.ends_with(".mcfunction") && !path.contains("__init") {
                println!("--- {path}\n{text}");
            }
        }
    }
}

/// Functions, calls and recursion. Spec sections 3.6, 6.12 and 6.13.
mod functions {
    use super::harness::{cost, load, local, run};

    fn value(src: &str) -> i32 {
        let mc = run(src);
        local(&mc, "main", "x").expect("x is set")
    }

    #[test]
    fn a_call_passes_arguments_and_returns_a_value() {
        assert_eq!(
            value(
                "fn add(a: i32, b: i32) -> i32 { return a + b; } fn main() { let x = add(2, 3); }"
            ),
            5
        );
    }

    #[test]
    fn a_function_can_be_called_before_it_is_written() {
        assert_eq!(
            value("fn main() { let x = one(); } fn one() -> i32 { return 1; }"),
            1
        );
    }

    #[test]
    fn calls_nest_inside_expressions() {
        assert_eq!(
            value(
                "fn dbl(n: i32) -> i32 { return n * 2; }
                 fn main() { let x = dbl(3) + dbl(4); }"
            ),
            14
        );
    }

    #[test]
    fn a_call_can_be_a_statement() {
        let mc = run(r#"fn shout() { raw!("say hi"); } fn main() { shout(); }"#);
        assert_eq!(mc.effects.len(), 1);
    }

    #[test]
    fn arguments_are_evaluated_in_the_callers_frame() {
        assert_eq!(
            value(
                "fn id(n: i32) -> i32 { return n; }
                 fn main() { let a = 7; let x = id(a); }"
            ),
            7
        );
    }

    #[test]
    fn returning_early_from_a_branch() {
        assert_eq!(
            value(
                "fn sign(n: i32) -> i32 { if n < 0 { return -1; } return 1; }
                 fn main() { let x = sign(-5); }"
            ),
            -1
        );
    }

    #[test]
    fn factorial_by_recursion() {
        // M4's completion criterion.
        let src = "fn fact(n: i32) -> i32 {
                       if n <= 1 { return 1; }
                       return n * fact(n - 1);
                   }
                   fn main() { let x = fact(5); }";
        assert_eq!(value(src), 120);
    }

    #[test]
    fn recursion_restores_the_callers_locals() {
        let src = "fn f(n: i32) -> i32 {
                       if n == 0 { return 0; }
                       let doubled = n * 2;
                       let rest = f(n - 1);
                       return doubled + rest;
                   }
                   fn main() { let x = f(4); }";
        assert_eq!(value(src), 20, "2 + 4 + 6 + 8");
    }

    #[test]
    fn mutual_recursion() {
        let src = "fn even(n: i32) -> i32 { if n == 0 { return 1; } return odd(n - 1); }
                   fn odd(n: i32) -> i32 { if n == 0 { return 0; } return even(n - 1); }
                   fn main() { let x = even(7); }";
        assert_eq!(value(src), 0);
    }

    #[test]
    fn a_recursive_call_leaves_the_frame_stack_empty() {
        let mc = run(
            "fn fact(n: i32) -> i32 { if n <= 1 { return 1; } return n * fact(n - 1); }
             fn main() { let x = fact(4); }",
        );
        let stack = tinymcf::path::NbtPath::parse("mw.stack")
            .unwrap()
            .resolve(mc.world.storage("test:mw"));
        assert!(
            stack.is_empty() || stack == vec![tinymcf::nbt::NbtValue::List(vec![])],
            "frames were not popped: {stack:?}"
        );
    }

    #[test]
    fn a_non_recursive_call_costs_nothing_but_the_call() {
        // No frame, no save, no restore: an argument write and the call itself.
        let mut mc = load("fn id(n: i32) -> i32 { return n; } fn main() { let x = id(1); }");
        mc.call("test:main");
        // Caller: write the argument, `execute store result` + `function`, copy the
        // result out of its temporary. Callee: `return run` + the `get` it runs.
        // The copy is a temporary the destination-driven lowering (M9-10) removes.
        assert_eq!(cost(&mc), 6);
    }

    #[test]
    fn a_short_circuit_with_a_pure_right_hand_side_stays_one_command() {
        let mut mc = load("fn main() { let a = true; let b = false; let x = a && b; }");
        mc.call("test:main");
        // Two `set`s, a copy into a temporary, the `min`, and a copy back out. The two
        // copies are what M9-10 removes; the point here is that there is no branch.
        assert_eq!(cost(&mc), 5);
    }

    #[test]
    fn a_call_on_the_right_of_and_is_not_run_when_the_left_is_false() {
        // Spec section 6.14: with a call on the right, short-circuiting is observable,
        // so it has to actually happen.
        let mc = run(r#"fn noisy() -> i32 { raw!("say ran"); return 1; }
               fn main() { let f = false; let x = f && noisy() == 1; }"#);
        assert!(mc.effects.is_empty(), "the right side should not have run");
    }

    #[test]
    fn a_call_on_the_right_of_and_does_run_when_the_left_is_true() {
        let mc = run(r#"fn noisy() -> i32 { raw!("say ran"); return 1; }
               fn main() { let t = true; let x = t && noisy() == 1; }"#);
        assert_eq!(mc.effects.len(), 1);
    }

    #[test]
    fn a_call_on_the_right_of_or_is_skipped_when_the_left_is_true() {
        let mc = run(r#"fn noisy() -> i32 { raw!("say ran"); return 1; }
               fn main() { let t = true; let x = t || noisy() == 1; }"#);
        assert!(mc.effects.is_empty());
    }
}
