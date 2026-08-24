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
            args: "hi".into(),
            executor: None,
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
        "struct Inner { a: i32 } struct Outer { inner: Inner, b: bool }
         fn take(o: Outer) -> i32 { return o.inner.a; }
         fn main() { let n = 2; let mut o = Outer { inner: Inner { a: n }, b: true };
                     o.inner.a += 1; let x = take(o); }",
        "struct Acc { total: i32 }
         fn walk(n: i32) -> i32 { let acc = Acc { total: n }; if n <= 0 { return 0; }
                                  let rest = walk(n - 1); return acc.total + rest; }
         fn main() { let x = walk(2); }",
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

/// Execution contexts. Spec sections 3.8, 4.6 and 6.15.
mod contexts {
    use super::harness::{NS, load, zombies};

    #[test]
    fn an_as_block_runs_the_body_once_per_entity() {
        let mut mc = load(r#"fn main() { as @e[type=zombie] { raw!("say hi"); } }"#);
        zombies(&mut mc, &["z1", "z2", "z3"]);
        mc.call(&format!("{NS}:main"));
        assert_eq!(mc.effects.len(), 3);
        assert_eq!(
            mc.effects.iter().filter_map(|e| e.executor.clone()).count(),
            3,
            "each ran as one of them"
        );
    }

    #[test]
    fn a_for_loop_is_the_same_thing_with_a_name() {
        let mut mc = load(r#"fn main() { for z in @e[type=zombie] { raw!("say hi"); } }"#);
        zombies(&mut mc, &["z1", "z2"]);
        mc.call(&format!("{NS}:main"));
        assert_eq!(mc.effects.len(), 2);
    }

    #[test]
    fn no_entities_means_the_body_never_runs() {
        let mut mc = load(r#"fn main() { as @e[type=zombie] { raw!("say hi"); } }"#);
        mc.call(&format!("{NS}:main"));
        assert!(mc.effects.is_empty());
    }

    #[test]
    fn the_binding_stands_for_the_current_entity() {
        let mut mc = load(r#"fn main() { for z in @e[type=zombie] { at z { raw!("say hi"); } } }"#);
        zombies(&mut mc, &["z1", "z2"]);
        mc.call(&format!("{NS}:main"));
        assert_eq!(mc.effects.len(), 2);
    }

    #[test]
    fn continue_in_a_for_body_skips_only_that_entity() {
        // The body is one function per entity, so returning from it is what "next
        // entity" means — and it must not stop the rest.
        let mut mc = load(
            r#"fn main() {
                   let mut n = 0;
                   for z in @e[type=zombie] {
                       n += 1;
                       if n == 2 { continue; }
                       raw!("say hi");
                   }
               }"#,
        );
        zombies(&mut mc, &["z1", "z2", "z3"]);
        mc.call(&format!("{NS}:main"));
        assert_eq!(mc.effects.len(), 2, "the second entity skipped its body");
    }

    #[test]
    fn break_in_a_for_body_stops_the_remaining_entities_doing_anything() {
        let mut mc = load(
            r#"fn main() {
                   let mut n = 0;
                   for z in @e[type=zombie] {
                       n += 1;
                       if n == 2 { break; }
                       raw!("say hi");
                   }
               }"#,
        );
        zombies(&mut mc, &["z1", "z2", "z3"]);
        mc.call(&format!("{NS}:main"));
        assert_eq!(mc.effects.len(), 1, "only the first entity got to act");
    }

    #[test]
    fn return_from_inside_a_for_leaves_the_function() {
        let mut mc = load(
            r#"fn main() {
                   for z in @e[type=zombie] { return; }
                   raw!("say after");
               }"#,
        );
        zombies(&mut mc, &["z1", "z2"]);
        mc.call(&format!("{NS}:main"));
        assert!(
            mc.effects.is_empty(),
            "the statement after the loop should not run"
        );
    }

    #[test]
    fn a_function_that_needs_an_executor_gets_one_from_the_call_site() {
        let mut mc = load(
            r#"#[ctx(entity)] fn shout() { raw!("say hi"); }
               fn main() { as @e[type=zombie] { shout(); } }"#,
        );
        zombies(&mut mc, &["z1", "z2"]);
        mc.call(&format!("{NS}:main"));
        assert_eq!(mc.effects.len(), 2);
        assert_eq!(mc.effects[0].executor.as_deref(), Some("z1"));
    }

    #[test]
    fn a_single_statement_context_block_stays_inline() {
        let mut mc = load(r#"fn main() { as @e[type=zombie] { raw!("say hi"); } }"#);
        zombies(&mut mc, &["z1"]);
        mc.call(&format!("{NS}:main"));
        // The `execute as` and the `say` it runs. No function in between.
        assert_eq!(mc.commands_run, 2);
    }
}

/// The control register is per function and survives between invocations, so anything
/// left in it has to be cleaned up before the next one.
mod control_register {
    use super::harness::{NS, load, local};

    #[test]
    fn a_function_that_returned_early_behaves_the_same_the_next_time() {
        let mut mc = load(
            "fn main() {
                 let mut x = 0;
                 if true { x = 1; return; }
                 x = 2;
             }",
        );
        mc.call(&format!("{NS}:main"));
        assert_eq!(local(&mc, "main", "x"), Some(1));

        // A stale control register would make the second call take a different path.
        mc.run_line("scoreboard players set $main.x test.v 0");
        mc.call(&format!("{NS}:main"));
        assert_eq!(local(&mc, "main", "x"), Some(1), "the second call differed");
        assert!(mc.diagnostics.is_empty(), "{:?}", mc.diagnostics);
    }

    #[test]
    fn a_stale_return_does_not_make_the_next_call_return_too() {
        // Call one returns from inside the loop, leaving the register raised. Call two
        // finds no entities, so nothing raises it again — and must still reach the end.
        let mut mc = load(
            r#"fn main() {
                   let mut n = 0;
                   for z in @e[type=zombie] {
                       n += 1;
                       return;
                   }
                   raw!("say finished");
               }"#,
        );
        super::harness::zombies(&mut mc, &["z1", "z2"]);
        mc.call(&format!("{NS}:main"));
        assert!(mc.effects.is_empty(), "call one returned before the end");

        mc.world
            .bind_selector("@e[type=zombie]", Vec::<String>::new());
        mc.call(&format!("{NS}:main"));
        assert_eq!(mc.effects.len(), 1, "call two should have reached the end");
    }

    #[test]
    fn a_loop_after_an_early_return_still_runs_on_the_next_call() {
        let mut mc = load(
            "fn main() {
                 let mut hits = 0;
                 if hits == 99 { return; }
                 while hits < 3 { hits += 1; }
             }",
        );
        mc.call(&format!("{NS}:main"));
        assert_eq!(local(&mc, "main", "hits"), Some(3));
        mc.call(&format!("{NS}:main"));
        assert_eq!(local(&mc, "main", "hits"), Some(3));
    }
}

/// Composite values. Spec sections 3.10, 4.8 and 6.18: a `struct` is a compound in
/// storage, so these ask what is in storage rather than what is in a register.
mod structs {
    use super::harness::{at_path, cost, load, local, run, stored};
    use tinymcf::nbt::NbtValue;

    fn compound(fields: &[(&str, NbtValue)]) -> NbtValue {
        NbtValue::compound(fields.iter().map(|(k, v)| (*k, v.clone())))
    }

    #[test]
    fn a_constant_construction_is_one_command() {
        let mc = run("struct Point { x: i32, y: bool } \
             fn main() { let p = Point { x: 1, y: true }; }");
        assert_eq!(
            stored(&mc, "main", "p"),
            Some(compound(&[
                ("x", NbtValue::Int(1)),
                ("y", NbtValue::Byte(1))
            ]))
        );
        assert_eq!(cost(&mc), 1, "the whole compound is one 'data modify'");
    }

    #[test]
    fn a_runtime_field_is_written_after_the_constant_ones() {
        let mc = run("struct Point { x: i32, y: i32 } \
             fn main() { let n = 6; let p = Point { x: n * 7, y: 2 }; }");
        assert_eq!(
            stored(&mc, "main", "p"),
            Some(compound(&[
                ("x", NbtValue::Int(42)),
                ("y", NbtValue::Int(2))
            ]))
        );
    }

    #[test]
    fn a_bool_field_is_a_byte_even_when_it_is_computed() {
        // Vanilla treats Byte(1) and Int(1) as different values and silently ignores
        // the wrong one, so the tag is part of the answer.
        let mc = run("struct Flags { on: bool } \
             fn main() { let n = 3; let f = Flags { on: n > 1 }; }");
        assert_eq!(
            stored(&mc, "main", "f"),
            Some(compound(&[("on", NbtValue::Byte(1))]))
        );
    }

    #[test]
    fn a_struct_can_hold_another_struct() {
        let mc = run(
            "struct Inner { a: i32 } struct Outer { inner: Inner, b: i32 } \
             fn main() { let o = Outer { inner: Inner { a: 1 }, b: 2 }; }",
        );
        assert_eq!(
            stored(&mc, "main", "o"),
            Some(compound(&[
                ("inner", compound(&[("a", NbtValue::Int(1))])),
                ("b", NbtValue::Int(2)),
            ]))
        );
    }

    #[test]
    fn a_nested_struct_can_be_a_binding_of_its_own() {
        let mc = run("struct Inner { a: i32 } struct Outer { inner: Inner } \
             fn main() { let i = Inner { a: 7 }; let o = Outer { inner: i }; }");
        assert_eq!(
            stored(&mc, "main", "o"),
            Some(compound(&[("inner", compound(&[("a", NbtValue::Int(7))]))]))
        );
    }

    #[test]
    fn copying_a_binding_is_one_command() {
        let mc = run("struct Point { x: i32 } \
             fn main() { let p = Point { x: 5 }; let q = p; }");
        assert_eq!(
            stored(&mc, "main", "q"),
            Some(compound(&[("x", NbtValue::Int(5))]))
        );
        assert_eq!(cost(&mc), 2, "one command to build, one to copy");
    }

    #[test]
    fn a_struct_can_be_passed_to_a_function() {
        let mut mc = load(
            "struct Point { x: i32 } \
             fn take(p: Point) { raw!(\"say taken\"); } \
             fn main() { let p = Point { x: 9 }; take(p); }",
        );
        mc.call("test:main");
        assert!(mc.diagnostics.is_empty(), "{:?}", mc.diagnostics);
        assert_eq!(
            stored(&mc, "take", "p"),
            Some(compound(&[("x", NbtValue::Int(9))]))
        );
    }

    #[test]
    fn a_nested_field_is_addressed_by_its_own_path() {
        let mc = run(
            "struct Inner { a: i32 } struct Outer { inner: Inner, b: i32 } \
             fn main() { \
                 let mut o = Outer { inner: Inner { a: 1 }, b: 2 }; \
                 o.inner.a = 9; \
                 let x = o.inner.a; \
             }",
        );
        assert_eq!(
            at_path(&mc, "mw.vars.main.o.inner.a"),
            Some(NbtValue::Int(9))
        );
        assert_eq!(local(&mc, "main", "x"), Some(9));
    }

    #[test]
    fn reading_a_field_costs_one_command() {
        let mc = run("struct Point { x: i32, y: i32 } \
             fn main() { let p = Point { x: 4, y: 5 }; let x = p.y; }");
        assert_eq!(local(&mc, "main", "x"), Some(5));
        // `execute ... run` is two commands, not one (tinymcf SPEC section 5): the
        // execute and the `data get` it runs. There is no temporary in between.
        assert_eq!(cost(&mc), 3, "one to build, two to read into the register");
    }

    #[test]
    fn a_compound_assignment_on_a_field_reads_changes_and_writes_back() {
        // Three commands, and they cannot be fewer: the scoreboard is the only place
        // arithmetic happens, so the value has to make the round trip.
        let mc = run("struct Counter { n: i32 } \
             fn main() { let mut c = Counter { n: 1 }; c.n += 4; }");
        assert_eq!(at_path(&mc, "mw.vars.main.c.n"), Some(NbtValue::Int(5)));
        // Three instructions, five commands: both the read and the write-back are
        // an `execute ... run`.
        assert_eq!(cost(&mc), 6, "one to build, five for the read-modify-write");
    }

    #[test]
    fn a_composite_field_is_copied_whole() {
        let mc = run("struct Inner { a: i32 } struct Outer { inner: Inner } \
             fn main() { let o = Outer { inner: Inner { a: 3 } }; let i = o.inner; }");
        assert_eq!(
            stored(&mc, "main", "i"),
            Some(compound(&[("a", NbtValue::Int(3))]))
        );
    }

    #[test]
    fn a_bool_field_can_be_a_condition() {
        let mc = run("struct Flags { on: bool } \
             fn main() { let f = Flags { on: true }; let mut x = 0; if f.on { x = 1; } }");
        assert_eq!(local(&mc, "main", "x"), Some(1));
    }

    #[test]
    fn a_field_written_from_another_field() {
        let mc = run("struct Point { x: i32, y: i32 } \
             fn main() { let mut p = Point { x: 1, y: 2 }; p.x = p.y; }");
        assert_eq!(at_path(&mc, "mw.vars.main.p.x"), Some(NbtValue::Int(2)));
    }

    /// A recursive call saves the caller's frame. A composite local is part of that
    /// frame, and saving it as if it were a score would read a register that does not
    /// exist and leave the callee's compound in place of the caller's.
    #[test]
    fn a_composite_local_survives_a_recursive_call() {
        let mc = run("struct Acc { total: i32 } \
             fn walk(n: i32) -> i32 { \
                 let acc = Acc { total: n }; \
                 if n <= 0 { return 0; } \
                 let rest = walk(n - 1); \
                 return acc.total + rest; \
             } \
             fn main() { let x = walk(3); }");
        assert_eq!(local(&mc, "main", "x"), Some(6));
        assert!(mc.diagnostics.is_empty(), "{:?}", mc.diagnostics);
    }

    /// `#[nbt(..)]`, spec section 4.8. Vanilla silently ignores data written with the
    /// wrong tag, so choosing the tag is the whole point of the attribute.
    #[test]
    fn a_field_can_choose_its_nbt_tag() {
        let mc = run("struct Mob { #[nbt(byte)] hp: i32 } \
             fn main() { let m = Mob { hp: 3 }; }");
        assert_eq!(
            stored(&mc, "main", "m"),
            Some(compound(&[("hp", NbtValue::Byte(3))]))
        );
    }

    #[test]
    fn a_computed_field_is_written_with_its_chosen_tag() {
        let mc = run("struct Mob { #[nbt(short)] hp: i32 } \
             fn main() { let n = 20; let m = Mob { hp: n * 2 }; }");
        assert_eq!(
            stored(&mc, "main", "m"),
            Some(compound(&[("hp", NbtValue::Short(40))]))
        );
    }

    #[test]
    fn a_field_can_be_renamed_for_vanilla() {
        let mc = run("struct Mob { #[nbt(rename = \"Health\")] hp: i32 } \
             fn main() { let mut m = Mob { hp: 3 }; m.hp = 4; let x = m.hp; }");
        assert_eq!(
            at_path(&mc, "mw.vars.main.m.Health"),
            Some(NbtValue::Int(4))
        );
        assert_eq!(
            local(&mc, "main", "x"),
            Some(4),
            "reading follows the rename"
        );
    }

    #[test]
    fn a_mutable_binding_can_be_replaced_wholesale() {
        let mc = run("struct Point { x: i32 } \
             fn main() { let mut p = Point { x: 1 }; p = Point { x: 2 }; }");
        assert_eq!(
            stored(&mc, "main", "p"),
            Some(compound(&[("x", NbtValue::Int(2))]))
        );
    }
}

/// Tagged unions. Spec sections 3.11, 4.9 and 6.19: an `enum` is a compound with a
/// `tag`, and its payload sits alongside it.
mod enums {
    use super::harness::{cost, run, stored};
    use tinymcf::nbt::NbtValue;

    fn compound(fields: &[(&str, NbtValue)]) -> NbtValue {
        NbtValue::compound(fields.iter().map(|(k, v)| (*k, v.clone())))
    }

    #[test]
    fn a_unit_variant_is_a_tag_on_its_own() {
        let mc = run("enum State { Idle, Chasing { target: i32 } } \
             fn main() { let s = State::Idle; }");
        assert_eq!(
            stored(&mc, "main", "s"),
            Some(compound(&[("tag", NbtValue::String("Idle".into()))]))
        );
        assert_eq!(cost(&mc), 1);
    }

    #[test]
    fn a_payload_sits_next_to_the_tag() {
        let mc = run("enum State { Idle, Chasing { target: i32 } } \
             fn main() { let n = 4; let s = State::Chasing { target: n * 2 }; }");
        assert_eq!(
            stored(&mc, "main", "s"),
            Some(compound(&[
                ("tag", NbtValue::String("Chasing".into())),
                ("target", NbtValue::Int(8)),
            ]))
        );
    }

    /// A new variant replaces the compound, so nothing of the old one is left behind
    /// to be read as if it belonged to the new one.
    #[test]
    fn changing_variant_leaves_no_stale_payload() {
        let mc = run("enum State { Idle, Chasing { target: i32 } } \
             fn main() { let mut s = State::Chasing { target: 3 }; s = State::Idle; }");
        assert_eq!(
            stored(&mc, "main", "s"),
            Some(compound(&[("tag", NbtValue::String("Idle".into()))]))
        );
    }

    #[test]
    fn an_enum_can_be_a_field_and_an_argument() {
        let mc = run("enum State { Idle } struct Mob { state: State } \
             fn take(m: Mob) {} \
             fn main() { let m = Mob { state: State::Idle }; take(m); }");
        assert_eq!(
            stored(&mc, "take", "m"),
            Some(compound(&[(
                "state",
                compound(&[("tag", NbtValue::String("Idle".into()))])
            )]))
        );
    }
}
