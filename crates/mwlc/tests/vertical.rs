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
        "fn main() { let mut v = [1, 2, 3]; let i = 2; v[i] = 9; let x = v[i]; v.push(x);
                     let n = v.len(); }",
        "struct Point { x: i32 }
         fn bump(p: &mut Point) { p.x += 1; }
         fn double(n: &mut i32) { n = n * 2; }
         fn read(p: &Point) -> i32 { return p.x; }
         fn main() { let mut a = Point { x: 0 }; bump(&mut a); let mut k = 21; double(&mut k);
                     let n = read(&a); }",
        "fn main() { let v = [1, 2, 3]; let mut sum = 0; for x in v { sum += x; } }",
        "struct Pair<T> { a: T, b: T }
         fn first<T>(p: Pair<T>) -> i32 { return 1; }
         fn main() { let p = Pair { a: 1, b: 2 }; let n = first(p);
                     let q = Pair { a: true, b: false }; let m = first(q); }",
        "enum State { Idle, Chasing { target: i32 } }
         fn main() { let mut s = State::Chasing { target: 3 }; let mut x = 0;
                     match s { State::Idle => { x = 1; }
                               State::Chasing { target } => { x = target; } } }",
        r#"fn main() { let a = "ab"; let b = a + "cd"; let c = b.slice(1..3);
                      let x = a == "ab"; let y = a == b; let n = b.len(); }"#,
        "struct Mob { pos: f64 }
         fn main() { let a = fix::<1000>(1500); let m = Mob { pos: a.as_f64() };
                     let x = fix::<1000>(m.pos); }",
        "fn main() { let a = fix::<1000>(1500); let b = fix::<1000>(2000);
                     let x = a * b; let y = a / b; let z = fix::<100>(a);
                     let w = fix::<100>(fix::<1000>(1500)); }",
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

/// `match`. Spec sections 3.12, 4.10 and 6.20: one guard per arm, and the tag decides.
mod matching {
    use super::harness::{cost, local, run};

    #[test]
    fn the_arm_for_the_current_variant_runs() {
        let src = "enum State { Idle, Chasing { target: i32 } } \
                   fn main() { \
                       let s = State::Idle; \
                       let mut x = 0; \
                       match s { \
                           State::Idle => { x = 1; } \
                           State::Chasing { target } => { x = 2; } \
                       } \
                   }";
        assert_eq!(local(&run(src), "main", "x"), Some(1));
    }

    #[test]
    fn a_payload_is_bound_inside_its_arm() {
        let src = "enum State { Idle, Chasing { target: i32 } } \
                   fn main() { \
                       let n = 6; \
                       let s = State::Chasing { target: n * 7 }; \
                       let mut x = 0; \
                       match s { \
                           State::Idle => { x = 1; } \
                           State::Chasing { target } => { x = target; } \
                       } \
                   }";
        assert_eq!(local(&run(src), "main", "x"), Some(42));
    }

    #[test]
    fn a_wildcard_arm_runs_when_no_tag_matched() {
        let src = "enum State { Idle, Waking, Chasing { target: i32 } } \
                   fn main() { \
                       let s = State::Waking; \
                       let mut x = 0; \
                       match s { \
                           State::Idle => { x = 1; } \
                           _ => { x = 9; } \
                       } \
                   }";
        assert_eq!(local(&run(src), "main", "x"), Some(9));
    }

    #[test]
    fn only_one_arm_runs() {
        // The tags are exclusive, so no arm needs to stop the others.
        let src = "enum State { Idle, Waking } \
                   fn main() { \
                       let s = State::Idle; \
                       let mut x = 0; \
                       match s { \
                           State::Idle => { x += 1; } \
                           State::Waking => { x += 10; } \
                       } \
                   }";
        assert_eq!(local(&run(src), "main", "x"), Some(1));
    }

    #[test]
    fn an_arm_can_return_from_the_function() {
        let src = "enum State { Idle, Waking } \
                   fn pick(s: State) -> i32 { \
                       match s { \
                           State::Idle => { return 1; } \
                           State::Waking => { return 2; } \
                       } \
                       return 0; \
                   } \
                   fn main() { let s = State::Waking; let x = pick(s); }";
        assert_eq!(local(&run(src), "main", "x"), Some(2));
    }

    #[test]
    fn an_arm_can_break_out_of_a_loop() {
        let src = "enum State { Idle, Waking } \
                   fn main() { \
                       let s = State::Waking; \
                       let mut x = 0; \
                       while x < 10 { \
                           x += 1; \
                           match s { \
                               State::Idle => { } \
                               State::Waking => { break; } \
                           } \
                       } \
                   }";
        assert_eq!(local(&run(src), "main", "x"), Some(1));
    }

    #[test]
    fn a_match_over_a_field_reads_the_nested_path() {
        let src = "enum State { Idle, Chasing { target: i32 } } \
                   struct Mob { state: State } \
                   fn main() { \
                       let m = Mob { state: State::Chasing { target: 5 } }; \
                       let mut x = 0; \
                       match m.state { \
                           State::Idle => { x = 1; } \
                           State::Chasing { target } => { x = target; } \
                       } \
                   }";
        assert_eq!(local(&run(src), "main", "x"), Some(5));
    }

    /// The milestone's completion criterion: a state machine of `struct` and `enum`
    /// running on the interpreter.
    ///
    /// An arm that moves the machine on rewrites the value being matched, so the
    /// guards have to test what it was on the way in — otherwise the arm after it
    /// sees the new variant and runs too.
    #[test]
    fn a_state_machine_steps_one_arm_at_a_time() {
        let src = "enum State { Idle, Waking, Chasing { target: i32 } } \
                   fn main() { \
                       let mut s = State::Idle; \
                       let mut steps = 0; \
                       let mut caught = 0; \
                       while steps < 3 { \
                           steps += 1; \
                           match s { \
                               State::Idle => { s = State::Waking; } \
                               State::Waking => { s = State::Chasing { target: steps }; } \
                               State::Chasing { target } => { caught = target; } \
                           } \
                       } \
                   }";
        let mc = run(src);
        assert_eq!(
            local(&mc, "main", "caught"),
            Some(2),
            "reached Chasing on step 2"
        );
    }

    /// What a two-arm match costs, counted from the output rather than guessed.
    #[test]
    fn a_guard_that_fails_costs_only_itself() {
        let src = "enum State { Idle, Waking } \
                   fn main() { \
                       let s = State::Idle; \
                       match s { \
                           State::Idle => { } \
                           State::Waking => { } \
                       } \
                   }";
        let mc = run(src);
        // Build the value (1), copy it aside (1), the guard that fails (1), and the
        // guard that matches, which is an execute plus the function it runs (2).
        assert_eq!(cost(&mc), 5);
    }
}

/// Lists. Spec sections 3.13, 4.11 and 6.21: a `Vec<T>` is an NBT list, and only a
/// runtime index has to go through a macro function.
mod vectors {
    use super::harness::{at_path, cost, load, local, run, stored};
    use tinymcf::nbt::NbtValue;

    fn list(values: &[NbtValue]) -> NbtValue {
        NbtValue::List(values.to_vec())
    }

    #[test]
    fn a_literal_is_one_command() {
        let mc = run("fn main() { let v = [1, 2, 3]; }");
        assert_eq!(
            stored(&mc, "main", "v"),
            Some(list(&[
                NbtValue::Int(1),
                NbtValue::Int(2),
                NbtValue::Int(3)
            ]))
        );
        assert_eq!(cost(&mc), 1);
    }

    #[test]
    fn an_empty_list_takes_its_type_from_the_annotation() {
        let mc = run("fn main() { let v: Vec<bool> = []; }");
        assert_eq!(stored(&mc, "main", "v"), Some(list(&[])));
    }

    #[test]
    fn len_reads_the_element_count() {
        let mc = run("fn main() { let v = [4, 5, 6]; let n = v.len(); }");
        assert_eq!(local(&mc, "main", "n"), Some(3));
    }

    #[test]
    fn push_appends() {
        let mc = run("fn main() { let mut v = [1]; v.push(2); let x = 7; v.push(x); }");
        assert_eq!(
            stored(&mc, "main", "v"),
            Some(list(&[
                NbtValue::Int(1),
                NbtValue::Int(2),
                NbtValue::Int(7)
            ]))
        );
    }

    #[test]
    fn a_bool_list_keeps_the_byte_tag() {
        let mc = run("fn main() { let mut v = [true]; let b = false; v.push(b); }");
        assert_eq!(
            stored(&mc, "main", "v"),
            Some(list(&[NbtValue::Byte(1), NbtValue::Byte(0)]))
        );
    }

    #[test]
    fn a_constant_index_is_part_of_the_path() {
        let mc = run("fn main() { let mut v = [1, 2, 3]; v[1] = 9; let x = v[1]; }");
        assert_eq!(local(&mc, "main", "x"), Some(9));
        assert_eq!(at_path(&mc, "mw.vars.main.v[1]"), Some(NbtValue::Int(9)));
    }

    /// The task's "test to write first": only a runtime index needs a macro.
    #[test]
    fn a_runtime_index_generates_a_macro_function() {
        let src = "fn main() { let v = [10, 20, 30]; let i = 2; let x = v[i]; }";
        let mc = run(src);
        assert_eq!(local(&mc, "main", "x"), Some(30));

        let pack =
            mwlc::driver::compile(src, "test", &mwlc::emit::Options::default()).expect("compiles");
        let macros: Vec<&String> = pack
            .files
            .iter()
            .filter(|(path, text)| path.ends_with(".mcfunction") && text.contains("$("))
            .map(|(path, _)| path)
            .collect();
        assert_eq!(macros.len(), 1, "one macro helper, in its own function");
        // The promotion must not spread: the caller stays an ordinary function
        // (requirements section 10.1). Fake player names start with `$` too, so what
        // makes a function a macro function is a *line* that does.
        let main = &pack.files["data/test/function/main.mcfunction"];
        assert!(
            !main.lines().any(|line| line.starts_with('$')),
            "the caller must not be a macro function:\n{main}"
        );
    }

    #[test]
    fn a_runtime_index_can_be_written_through() {
        let mc = run("fn main() { let mut v = [1, 2, 3]; let i = 0; v[i] = 8; }");
        assert_eq!(at_path(&mc, "mw.vars.main.v[0]"), Some(NbtValue::Int(8)));
    }

    #[test]
    fn a_list_of_structs_is_a_list_of_compounds() {
        let mc = run("struct Point { x: i32 } \
             fn main() { let mut v = [Point { x: 1 }]; v.push(Point { x: 2 }); \
                         let p = v[1]; let n = p.x; }");
        assert_eq!(local(&mc, "main", "n"), Some(2));
    }

    #[test]
    fn a_vec_can_be_a_field_and_an_argument() {
        let mut mc = load(
            "struct Bag { items: Vec<i32> } \
             fn take(b: Bag) -> i32 { return b.items.len(); } \
             fn main() { let b = Bag { items: [1, 2] }; let n = take(b); }",
        );
        mc.call("test:main");
        assert!(mc.diagnostics.is_empty(), "{:?}", mc.diagnostics);
        assert_eq!(local(&mc, "main", "n"), Some(2));
    }
}

/// `for x in vec`. Spec section 6.22: destructive iteration over a copy, no macros.
mod iteration {
    use super::harness::{local, run, stored};
    use tinymcf::nbt::NbtValue;

    #[test]
    fn every_element_is_visited() {
        let mc = run("fn main() { let v = [1, 2, 3]; let mut sum = 0; \
                         for x in v { sum += x; } }");
        assert_eq!(local(&mc, "main", "sum"), Some(6));
    }

    /// The task's "test to write first": the index is always `[0]`, so nothing here
    /// needs a macro function.
    #[test]
    fn iterating_generates_no_macro_lines() {
        let src = "fn main() { let v = [1, 2, 3]; let mut sum = 0; \
                    for x in v { sum += x; } }";
        let pack =
            mwlc::driver::compile(src, "test", &mwlc::emit::Options::default()).expect("compiles");
        for (path, text) in &pack.files {
            assert!(
                !text.lines().any(|line| line.starts_with('$')),
                "{path} is a macro function:\n{text}"
            );
        }
    }

    #[test]
    fn the_original_list_is_left_alone() {
        let mc = run(
            "fn main() { let v = [1, 2]; let mut n = 0; for x in v { n += 1; } \
                         let left = v.len(); }",
        );
        assert_eq!(local(&mc, "main", "n"), Some(2));
        assert_eq!(local(&mc, "main", "left"), Some(2), "iteration copies");
        assert_eq!(
            stored(&mc, "main", "v"),
            Some(NbtValue::List(vec![NbtValue::Int(1), NbtValue::Int(2)]))
        );
    }

    #[test]
    fn break_and_continue_work_inside() {
        let mc = run("fn main() { let v = [1, 2, 3, 4]; let mut sum = 0; \
                         for x in v { \
                             if x == 2 { continue; } \
                             if x == 4 { break; } \
                             sum += x; \
                         } }");
        assert_eq!(local(&mc, "main", "sum"), Some(4), "1 + 3");
    }

    #[test]
    fn a_composite_element_is_bound_whole() {
        let mc = run("struct Point { x: i32 } \
             fn main() { let v = [Point { x: 2 }, Point { x: 5 }]; let mut sum = 0; \
                         for p in v { sum += p.x; } }");
        assert_eq!(local(&mc, "main", "sum"), Some(7));
    }

    #[test]
    fn a_return_inside_leaves_the_function() {
        let mc = run("fn first_big(v: Vec<i32>) -> i32 { \
                 for x in v { if x > 2 { return x; } } \
                 return 0; \
             } \
             fn main() { let v = [1, 5, 9]; let x = first_big(v); }");
        assert_eq!(local(&mc, "main", "x"), Some(5));
    }

    #[test]
    fn a_list_in_a_field_can_be_iterated() {
        let mc = run("struct Bag { items: Vec<i32> } \
             fn main() { let b = Bag { items: [3, 4] }; let mut sum = 0; \
                         for x in b.items { sum += x; } }");
        assert_eq!(local(&mc, "main", "sum"), Some(7));
    }

    #[test]
    fn the_binding_cannot_be_assigned_to() {
        let src = "fn main() { let v = [1]; for x in v { x = 2; } }";
        let report = mwlc::driver::compile(src, "test", &mwlc::emit::Options::default())
            .expect_err("x is not mutable");
        assert!(format!("{report:?}").contains("not mutable"), "{report:?}");
    }

    #[test]
    fn nested_iteration_keeps_its_own_copy() {
        let mc = run("fn main() { let v = [1, 2]; let mut sum = 0; \
                         for a in v { for b in v { sum += a * b; } } }");
        assert_eq!(local(&mc, "main", "sum"), Some(9), "(1+2) * (1+2)");
    }
}

/// Monomorphisation. Spec sections 3.14, 4.12 and 6.23: one instance per set of type
/// arguments, and the template itself is never emitted.
mod generics {
    use super::harness::{local, run, stored};
    use tinymcf::nbt::NbtValue;

    fn functions(src: &str) -> Vec<String> {
        let pack =
            mwlc::driver::compile(src, "test", &mwlc::emit::Options::default()).expect("compiles");
        pack.files
            .keys()
            .filter_map(|path| {
                path.strip_prefix("data/test/function/")?
                    .strip_suffix(".mcfunction")
                    .map(str::to_owned)
            })
            .collect()
    }

    /// The task's "test to write first": asking twice for the same type arguments does
    /// not make a second instance.
    #[test]
    fn one_instance_per_set_of_type_arguments() {
        let names = functions(
            "fn hold<T>(x: T) -> i32 { return 1; } \
             fn main() { let a = hold(1); let b = hold(2); let c = hold(true); }",
        );
        assert!(names.contains(&"hold_i32".to_owned()), "{names:?}");
        assert!(names.contains(&"hold_bool".to_owned()), "{names:?}");
        assert_eq!(
            names.iter().filter(|n| n.starts_with("hold")).count(),
            2,
            "{names:?}"
        );
        assert!(
            !names.contains(&"hold".to_owned()),
            "the template is not emitted"
        );
    }

    #[test]
    fn an_instance_computes_with_the_argument_it_was_given() {
        let mc = run("fn twice<T>(x: T) -> T { return x; } \
             fn main() { let a = twice(21); let b = twice(true); }");
        assert_eq!(local(&mc, "main", "a"), Some(21));
        assert_eq!(local(&mc, "main", "b"), Some(1));
    }

    #[test]
    fn a_type_parameter_can_be_nested_in_the_argument() {
        let mc = run("fn count<T>(v: Vec<T>) -> i32 { return v.len(); } \
             fn main() { let v = [1, 2, 3]; let n = count(v); }");
        assert_eq!(local(&mc, "main", "n"), Some(3));
    }

    #[test]
    fn a_generic_struct_is_instantiated_by_its_fields() {
        let mc = run("struct Pair<T> { a: T, b: T } \
             fn main() { let p = Pair { a: 1, b: 2 }; let x = p.b; }");
        assert_eq!(local(&mc, "main", "x"), Some(2));
    }

    #[test]
    fn a_generic_struct_can_be_annotated() {
        let mc = run("struct Pair<T> { a: T, b: T } \
             fn main() { let p: Pair<bool> = Pair { a: true, b: false }; }");
        assert_eq!(
            stored(&mc, "main", "p"),
            Some(NbtValue::compound([
                ("a", NbtValue::Byte(1)),
                ("b", NbtValue::Byte(0)),
            ]))
        );
    }

    #[test]
    fn a_generic_function_can_recurse() {
        let mc = run("fn countdown<T>(x: T, n: i32) -> i32 { \
                 if n <= 0 { return 0; } \
                 return 1 + countdown(x, n - 1); \
             } \
             fn main() { let n = countdown(true, 3); }");
        assert_eq!(local(&mc, "main", "n"), Some(3));
    }

    #[test]
    fn instances_of_two_types_do_not_share_registers() {
        let mc = run("fn keep<T>(x: T, tag: i32) -> i32 { return tag; } \
             fn main() { let a = keep(1, 10); let b = keep(true, 20); }");
        assert_eq!(local(&mc, "main", "a"), Some(10));
        assert_eq!(local(&mc, "main", "b"), Some(20));
    }
}

/// Compile-time references and `impl`. Spec sections 3.15, 4.13 and 6.24: a borrow is
/// a name for a path, so it costs nothing at runtime.
mod references {
    use super::harness::{at_path, cost, local, run};
    use tinymcf::nbt::NbtValue;

    fn errors(src: &str) -> String {
        let report = mwlc::driver::compile(src, "test", &mwlc::emit::Options::default())
            .expect_err("should not compile");
        format!("{report:?}")
    }

    #[test]
    fn a_mutable_borrow_writes_through_to_the_caller() {
        let mc = run("struct Point { x: i32 } \
             fn bump(p: &mut Point) { p.x += 1; } \
             fn main() { let mut a = Point { x: 0 }; bump(&mut a); bump(&mut a); }");
        assert_eq!(at_path(&mc, "mw.vars.main.a.x"), Some(NbtValue::Int(2)));
    }

    /// The task's "test to write first".
    #[test]
    fn a_runtime_index_cannot_be_borrowed() {
        let message = errors(
            "struct Point { x: i32 } \
             fn bump(p: &mut Point) { p.x += 1; } \
             fn main() { let mut v = [Point { x: 0 }]; let i = 0; bump(&mut v[i]); }",
        );
        assert!(message.contains("runtime"), "{message}");
    }

    #[test]
    fn borrowing_costs_no_commands_of_its_own() {
        let mc = run("struct Point { x: i32 } \
             fn read(p: &Point) -> i32 { return p.x; } \
             fn main() { let a = Point { x: 5 }; let n = read(&a); }");
        assert_eq!(local(&mc, "main", "n"), Some(5));
        // Build the compound (1), call it storing the result (2), read the field (2)
        // and return it (2), copy the result into the binding (1). Not one of those is
        // spent marshalling the argument, which is the whole point of a borrow.
        assert_eq!(cost(&mc), 8);
    }

    #[test]
    fn a_field_can_be_borrowed() {
        let mc = run("struct Inner { a: i32 } struct Outer { inner: Inner } \
             fn bump(i: &mut Inner) { i.a += 10; } \
             fn main() { let mut o = Outer { inner: Inner { a: 1 } }; bump(&mut o.inner); }");
        assert_eq!(
            at_path(&mc, "mw.vars.main.o.inner.a"),
            Some(NbtValue::Int(11))
        );
    }

    #[test]
    fn a_constant_index_can_be_borrowed() {
        let mc = run("struct Point { x: i32 } \
             fn bump(p: &mut Point) { p.x += 1; } \
             fn main() { let mut v = [Point { x: 0 }, Point { x: 9 }]; bump(&mut v[1]); }");
        assert_eq!(at_path(&mc, "mw.vars.main.v[1].x"), Some(NbtValue::Int(10)));
    }

    #[test]
    fn a_scalar_binding_can_be_borrowed() {
        let mc = run("fn double(n: &mut i32) { n = n * 2; } \
             fn main() { let mut x = 21; double(&mut x); }");
        assert_eq!(local(&mc, "main", "x"), Some(42));
    }

    #[test]
    fn writing_through_a_shared_borrow_is_reported() {
        let message = errors(
            "struct Point { x: i32 } \
             fn bump(p: &Point) { p.x += 1; } \
             fn main() { let mut a = Point { x: 0 }; bump(&a); }",
        );
        assert!(message.contains("not mutable"), "{message}");
    }

    #[test]
    fn a_reference_cannot_be_bound_or_returned() {
        assert!(errors("fn main() { let x = 1; let r = &x; }").contains("argument"));
        assert!(
            errors("struct P { x: i32 } fn f(p: &P) -> &P { return p; }").contains("reference")
        );
    }

    #[test]
    fn a_method_borrows_its_receiver() {
        let mc = run("struct Counter { n: i32 } \
             impl Counter { \
                 fn bump(&mut self) { self.n += 1; } \
                 fn get(&self) -> i32 { return self.n; } \
             } \
             fn main() { let mut c = Counter { n: 0 }; c.bump(); c.bump(); \
                         let n = c.get(); }");
        assert_eq!(local(&mc, "main", "n"), Some(2));
    }

    #[test]
    fn two_call_sites_borrowing_different_places_get_their_own_instance() {
        let mc = run("struct Point { x: i32 } \
             fn bump(p: &mut Point) { p.x += 1; } \
             fn main() { let mut a = Point { x: 0 }; let mut b = Point { x: 10 }; \
                         bump(&mut a); bump(&mut b); }");
        assert_eq!(at_path(&mc, "mw.vars.main.a.x"), Some(NbtValue::Int(1)));
        assert_eq!(at_path(&mc, "mw.vars.main.b.x"), Some(NbtValue::Int(11)));
    }
}

/// Fixed point: an integer with a scale, and the corrections `*` and `/` need to keep
/// the units right (spec section 6.25).
mod fixed_point {
    use super::harness::{cost, local, run};

    fn value(src: &str) -> i32 {
        let mc = run(&format!("fn main() {{ {src} }}"));
        local(&mc, "main", "x").expect("x is set")
    }

    #[test]
    fn multiplying_two_fixes_corrects_the_scale() {
        // 1.5 * 2.0 = 3.0, in thousandths throughout.
        assert_eq!(
            value("let a = fix::<1000>(1500); let b = fix::<1000>(2000); let x = a * b;"),
            3000
        );
        // 0.5 * 0.5 = 0.25: without the correction this would come out 250000.
        assert_eq!(
            value("let a = fix::<1000>(500); let b = fix::<1000>(500); let x = a * b;"),
            250
        );
    }

    #[test]
    fn dividing_two_fixes_corrects_the_scale() {
        // 1.5 / 2.0 = 0.75.
        assert_eq!(
            value("let a = fix::<1000>(1500); let b = fix::<1000>(2000); let x = a / b;"),
            750
        );
    }

    #[test]
    fn adding_needs_no_correction() {
        assert_eq!(
            value("let a = fix::<1000>(1500); let b = fix::<1000>(2000); let x = a + b;"),
            3500
        );
        assert_eq!(
            value("let a = fix::<1000>(1500); let b = fix::<1000>(2000); let x = b - a;"),
            500
        );
    }

    #[test]
    fn an_integer_multiplier_carries_no_scale() {
        assert_eq!(value("let a = fix::<1000>(1500); let x = a * 2;"), 3000);
        assert_eq!(value("let a = fix::<1000>(1500); let x = a / 2;"), 750);
        assert_eq!(value("let a = fix::<1000>(1500); let x = 2 * a;"), 3000);
    }

    #[test]
    fn a_cast_between_scales_restates_the_value() {
        // 1.5 in thousandths is 150 in hundredths.
        assert_eq!(
            value("let a = fix::<1000>(1500); let x = fix::<100>(a);"),
            150
        );
        assert_eq!(
            value("let a = fix::<100>(150); let x = fix::<1000>(a);"),
            1500
        );
    }

    #[test]
    fn compound_assignment_carries_the_correction() {
        assert_eq!(
            value("let mut x = fix::<1000>(1500); let b = fix::<1000>(2000); x *= b;"),
            3000
        );
        assert_eq!(
            value("let mut x = fix::<1000>(1500); x += fix::<1000>(500);"),
            2000
        );
    }

    #[test]
    fn a_constant_fix_costs_exactly_one_command() {
        // The integer is already the value in raw units, so the cast is free
        // (design principle 1).
        let mc = run("fn main() { let x = fix::<1000>(1500); }");
        assert_eq!(cost(&mc), 1);
    }

    #[test]
    fn a_generic_function_runs_at_the_scale_it_was_called_with() {
        let mc = run(
            "fn double<const S: i32>(x: fix<S>) -> fix<S> { return x * 2; } \
             fn main() { let a = double(fix::<1000>(1500)); let b = double(fix::<100>(150)); }",
        );
        assert_eq!(local(&mc, "main", "a"), Some(3000));
        assert_eq!(local(&mc, "main", "b"), Some(300));
    }
}

/// The score/storage round trip, and the scale that goes with it (spec section 6.26).
mod round_trip {
    use super::harness::{at_path, cost, local, run};
    use tinymcf::nbt::NbtValue;

    #[test]
    fn a_fix_goes_into_a_double_and_comes_back() {
        let mc = run("struct Mob { pos: f64 } \
             fn main() { let a = fix::<1000>(1500); let m = Mob { pos: a.as_f64() }; \
                         let x = fix::<1000>(m.pos); }");
        // Storage holds the real number, not the raw units.
        assert_eq!(
            at_path(&mc, "mw.vars.main.m.pos"),
            Some(NbtValue::Double(1.5))
        );
        assert_eq!(local(&mc, "main", "x"), Some(1500));
    }

    #[test]
    fn a_float_field_keeps_its_tag() {
        let mc = run("struct Mob { hp: f32, age: i64 } \
             fn main() { let h = fix::<100>(2050); let n = 7; \
                         let m = Mob { hp: h.as_f32(), age: n.as_i64() }; }");
        assert_eq!(
            at_path(&mc, "mw.vars.main.m.hp"),
            Some(NbtValue::Float(20.5))
        );
        assert_eq!(at_path(&mc, "mw.vars.main.m.age"), Some(NbtValue::Long(7)));
    }

    #[test]
    fn an_nbt_scalar_reads_back_as_an_integer() {
        let mc = run("struct Mob { age: i64 } \
             fn main() { let n = 7; let m = Mob { age: n.as_i64() }; \
                         let x = m.age.as_i32(); }");
        assert_eq!(local(&mc, "main", "x"), Some(7));
    }

    #[test]
    fn reading_a_double_floors_what_the_scale_cannot_hold() {
        // 1.5 read as hundredths is 150; as units it is 1.
        let mc = run("struct Mob { pos: f64 } \
             fn main() { let a = fix::<1000>(1500); let m = Mob { pos: a.as_f64() }; \
                         let x = fix::<100>(m.pos); let y = m.pos.as_i32(); }");
        assert_eq!(local(&mc, "main", "x"), Some(150));
        assert_eq!(local(&mc, "main", "y"), Some(1));
    }

    #[test]
    fn each_direction_costs_one_command() {
        // `set a`, `set value` for the compound, then one `execute store` each way.
        // An `execute ... run` is two commands, not one (tinymcf SPEC section 5).
        let mc = run("struct Mob { pos: f64 } \
             fn main() { let a = fix::<1000>(1500); let m = Mob { pos: a.as_f64() }; \
                         let x = fix::<1000>(m.pos); }");
        assert_eq!(cost(&mc), 1 + 1 + 2 + 2);
    }
}

/// Strings: what vanilla can do with one, and nothing more (spec section 4.17).
mod strings {
    use super::harness::{at_path, cost, local, run};
    use tinymcf::nbt::NbtValue;

    #[test]
    fn a_literal_lands_in_storage() {
        let mc = run(r#"fn main() { let s = "hi"; }"#);
        assert_eq!(
            at_path(&mc, "mw.vars.main.s"),
            Some(NbtValue::String("hi".to_owned()))
        );
        assert_eq!(cost(&mc), 1);
    }

    #[test]
    fn len_is_one_command_and_no_macro() {
        let mc = run(r#"fn main() { let s = "hello"; let x = s.len(); }"#);
        assert_eq!(local(&mc, "main", "x"), Some(5));
        // `set value` and the `execute store ... run data get` behind it.
        assert_eq!(cost(&mc), 1 + 2);
    }

    #[test]
    fn comparing_against_a_literal_is_one_command() {
        let mc = run(r#"fn main() { let s = "hi"; let x = s == "hi"; let y = s == "no"; }"#);
        assert_eq!(local(&mc, "main", "x"), Some(1));
        assert_eq!(local(&mc, "main", "y"), Some(0));
        // `set value`, then one `execute store success ... if data` each.
        assert_eq!(cost(&mc), 3);
    }

    #[test]
    fn two_literals_are_compared_while_compiling() {
        let mc = run(r#"fn main() { let x = "a" == "a"; let y = "a" != "a"; }"#);
        assert_eq!(local(&mc, "main", "x"), Some(1));
        assert_eq!(local(&mc, "main", "y"), Some(0));
        assert_eq!(cost(&mc), 2);
    }

    #[test]
    fn comparing_two_strings_at_runtime_works() {
        let mc = run(r#"fn main() { let a = "hi"; let b = "hi"; let c = "no";
                          let x = a == b; let y = a == c; }"#);
        assert_eq!(local(&mc, "main", "x"), Some(1));
        assert_eq!(local(&mc, "main", "y"), Some(0));
    }

    #[test]
    fn joining_two_literals_needs_no_macro() {
        let mc = run(r#"fn main() { let s = "ab" + "cd"; let x = s.len(); }"#);
        assert_eq!(local(&mc, "main", "x"), Some(4));
        assert_eq!(cost(&mc), 1 + 2);
    }

    #[test]
    fn joining_a_runtime_string_splices_it() {
        let mc =
            run(r#"fn main() { let a = "ab"; let s = a + "cd"; let t = s + a; let x = t.len(); }"#);
        assert_eq!(
            at_path(&mc, "mw.vars.main.s"),
            Some(NbtValue::String("abcd".to_owned()))
        );
        assert_eq!(
            at_path(&mc, "mw.vars.main.t"),
            Some(NbtValue::String("abcdab".to_owned()))
        );
        assert_eq!(local(&mc, "main", "x"), Some(6));
    }

    #[test]
    fn a_constant_slice_is_one_command() {
        let mc = run(
            r#"fn main() { let s = "abcdef"; let a = s.slice(1..3); let b = s.slice(2..);
                          let x = a.len(); }"#,
        );
        assert_eq!(
            at_path(&mc, "mw.vars.main.a"),
            Some(NbtValue::String("bc".to_owned()))
        );
        assert_eq!(
            at_path(&mc, "mw.vars.main.b"),
            Some(NbtValue::String("cdef".to_owned()))
        );
        assert_eq!(local(&mc, "main", "x"), Some(2));
    }

    #[test]
    fn a_string_survives_recursion() {
        // Every string here lives in storage, so the saves and restores around the
        // recursive call have to cover them (spec section 6.13).
        let mc = run(r#"fn grow(s: String, n: i32) -> i32 {
                   if n <= 0 { return 0; }
                   let t = s + "xy";
                   let rest = grow(t, n - 1);
                   return rest + s.len();
               }
               fn main() { let x = grow("a", 2); }"#);
        // The tails are "axy" and "a": 3 + 1.
        assert_eq!(local(&mc, "main", "x"), Some(4));
    }

    #[test]
    fn comparing_an_element_of_a_list() {
        let mc = run(r#"fn main() { let v = ["a", "b"]; let x = v[1] == "b"; }"#);
        assert_eq!(local(&mc, "main", "x"), Some(1));
    }

    #[test]
    fn a_string_copies_and_passes() {
        let mc = run(r#"struct Tag { name: String }
               fn take(t: Tag) -> i32 { return t.name.len(); }
               fn main() { let s = "abcd"; let t = Tag { name: s }; let x = take(t); }"#);
        assert_eq!(local(&mc, "main", "x"), Some(4));
    }
}

/// `nbt!`, checked against the type it is written into (spec section 4.18).
mod nbt_literals {
    use super::harness::{at_path, cost, local, run};
    use tinymcf::nbt::NbtValue;

    #[test]
    fn a_literal_compound_is_one_command() {
        let mc = run(r#"struct Mob { hp: i32, name: String }
               fn main() { let m: Mob = nbt!({ hp: 20, name: "bob" });
                           let x = m.hp; }"#);
        assert_eq!(at_path(&mc, "mw.vars.main.m.hp"), Some(NbtValue::Int(20)));
        assert_eq!(
            at_path(&mc, "mw.vars.main.m.name"),
            Some(NbtValue::String("bob".to_owned()))
        );
        assert_eq!(local(&mc, "main", "x"), Some(20));
        // The compound, then the read into x.
        assert_eq!(cost(&mc), 1 + 2);
    }

    #[test]
    fn the_tag_comes_from_the_field() {
        let mc = run(
            r#"struct Mob { #[nbt(short)] shots: i32, alive: bool, weight: f64 }
               fn main() { let m: Mob = nbt!({ shots: 3, alive: true, weight: 2 }); }"#,
        );
        assert_eq!(
            at_path(&mc, "mw.vars.main.m.shots"),
            Some(NbtValue::Short(3))
        );
        assert_eq!(
            at_path(&mc, "mw.vars.main.m.alive"),
            Some(NbtValue::Byte(1))
        );
        assert_eq!(
            at_path(&mc, "mw.vars.main.m.weight"),
            Some(NbtValue::Double(2.0))
        );
    }

    #[test]
    fn a_renamed_field_is_written_the_way_vanilla_spells_it() {
        let mc = run(r#"struct Mob { #[nbt(rename = "Health")] hp: i32 }
               fn main() { let m: Mob = nbt!({ Health: 20 }); }"#);
        assert_eq!(
            at_path(&mc, "mw.vars.main.m.Health"),
            Some(NbtValue::Int(20))
        );
    }

    #[test]
    fn nesting_and_lists_are_checked_too() {
        let mc = run(r#"struct Inner { a: i32 }
               struct Outer { inner: Inner, xs: Vec<i32> }
               fn main() { let o: Outer = nbt!({ inner: { a: 1 }, xs: [1, 2, 3] });
                           let x = o.xs.len(); }"#);
        assert_eq!(local(&mc, "main", "x"), Some(3));
        assert_eq!(
            at_path(&mc, "mw.vars.main.o.inner.a"),
            Some(NbtValue::Int(1))
        );
    }
}
