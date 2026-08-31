// SPDX-License-Identifier: MIT

//! What representative packs cost to run (plan X-4).
//!
//! Generated command count is a performance requirement, not a detail: commands per
//! tick turn straight into TPS. These snapshots are how a change to lowering shows its
//! effect — a diff here is the whole point, and reviewing it is the work.
//!
//! Release profile on purpose: that is what anyone ships.

use mwlc::cost;
use mwlc::emit::{Options, Profile};

/// A tagged state that moves on, which is what most datapack logic is.
const STATE_MACHINE: &str = r#"
enum Phase { Idle, Arming { left: i32 }, Firing }

fn advance(p: &Phase, out: &mut Phase) {
    match p {
        Phase::Idle => { out = Phase::Arming { left: 3 }; }
        Phase::Arming { left } => {
            if left <= 1 { out = Phase::Firing; }
            else { out = Phase::Arming { left: left - 1 }; }
        }
        Phase::Firing => { out = Phase::Idle; }
    }
}

#[tick]
fn tick() {
    let phase = Phase::Idle;
    let mut next = Phase::Idle;
    advance(&phase, &mut next);
    match next {
        Phase::Idle => {}
        Phase::Arming { left } => { raw!("say arming"); }
        Phase::Firing => { raw!("say firing"); }
    }
}
"#;

/// A list walked and added to: the shape inventory handling takes.
const INVENTORY: &str = r#"
#[tick]
fn tick() {
    let mut slots = [1, 2, 3, 4];
    let mut total = 0;
    for s in slots {
        total = total + s;
    }
    let count = slots.len();
    slots.push(total);
    if total > 6 { raw!("say heavy"); }
}
"#;

/// Recursion, where the frame handling shows up.
const RECURSION: &str = r#"
fn fib(n: i32) -> i32 {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}

#[tick]
fn tick() {
    let a = fib(6);
    if a > 5 { raw!("say big"); }
}
"#;

fn table(src: &str) -> String {
    let options = Options {
        profile: Profile::Release,
        ..Options::default()
    };
    let pack = mwlc::driver::compile(src, "bench", &options).expect("compiles");
    cost::table(&pack.costs)
}

#[test]
fn a_state_machine() {
    insta::assert_snapshot!(table(STATE_MACHINE));
}

#[test]
fn an_inventory() {
    insta::assert_snapshot!(table(INVENTORY));
}

#[test]
fn recursion() {
    insta::assert_snapshot!(table(RECURSION));
}
