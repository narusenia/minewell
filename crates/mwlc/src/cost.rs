// SPDX-License-Identifier: MIT

//! How many commands a function costs to run (requirements section 16.1).
//!
//! `maxCommandChainLength` (65536 by default) caps the commands one tick may run.
//! Going over it does not raise an error: Minecraft stops the chain and the rest of
//! the work silently does not happen. Counting beforehand is the only way to see it
//! coming, and the count is derivable because one MIR instruction is one command.
//!
//! The number is a worst case: a guarded command is counted as if the guard held.
//! That is the number `maxCommandChainLength` cares about.

use crate::mir::{ExecuteAs, Function, Inst, Mir};

/// Minecraft's default `maxCommandChainLength`.
pub const MAX_COMMAND_CHAIN: u64 = 65536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cost {
    /// The datapack id, as a command would name it.
    pub path: String,
    /// Commands one call runs, callees included.
    pub commands: u64,
    /// Whether a loop or a recursive call is reachable from here. Then `commands` is
    /// one pass through it and the real total depends on the data.
    pub loops: bool,
}

/// The cost of every function in the program.
pub fn costs(mir: &Mir) -> Vec<Cost> {
    let own: Vec<u64> = mir.functions.iter().map(own_cost).collect();
    let repeated: Vec<bool> = mir.functions.iter().map(runs_per_entity).collect();
    let calls: Vec<Vec<usize>> = mir
        .functions
        .iter()
        .map(|f| callees(f, &mir.functions))
        .collect();
    mir.functions
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let mut stack = Vec::new();
            let (commands, exact) = walk(i, &own, &repeated, &calls, &mut stack);
            Cost {
                path: f.path.clone(),
                commands,
                loops: !exact,
            }
        })
        .collect()
}

/// The report `mwl build` writes to `target/cost.txt`.
pub fn table(costs: &[Cost]) -> String {
    let width = costs.iter().map(|c| c.path.len()).max().unwrap_or(0);
    let mut out = String::from(
        "# commands per call, callees included, counting every guard as taken.\n\
         # '+' means the number is one pass: a loop, a recursive call or an\n\
         # 'execute as' over several entities repeats part of it, and how often\n\
         # depends on the data.\n\
         # maxCommandChainLength is 65536 per tick by default; over it, Minecraft stops\n\
         # the chain without saying so.\n\n",
    );
    for cost in costs {
        let mark = if cost.loops { " +" } else { "" };
        out.push_str(&format!("{:width$}  {}{mark}\n", cost.path, cost.commands));
    }
    let over: Vec<&Cost> = costs
        .iter()
        .filter(|c| !c.loops && c.commands > MAX_COMMAND_CHAIN)
        .collect();
    if !over.is_empty() {
        out.push_str("\n# over maxCommandChainLength:\n");
        for cost in over {
            out.push_str(&format!("#   {} ({})\n", cost.path, cost.commands));
        }
    }
    out
}

/// The commands this function issues itself, not counting what its callees do.
fn own_cost(function: &Function) -> u64 {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .map(inst_cost)
        .sum()
}

/// One instruction is one command, plus whatever it wraps.
///
/// `execute … run <cmd>` is two: the `execute` and the command it ran
/// (`crates/tinymcf/SPEC.md` section 5). A `$` macro line is still one command, and so
/// is an `execute store success … if …` with no `run` at all.
fn inst_cost(inst: &Inst) -> u64 {
    match inst {
        Inst::Macro { inst } => inst_cost(inst),
        Inst::StoreResult { inst, .. }
        | Inst::ReturnRun { inst }
        | Inst::StoreData { inst, .. }
        | Inst::StoreScaled { inst, .. }
        | Inst::StoreBoth { inst, .. }
        | Inst::Guarded { inst, .. }
        | Inst::Otherwise { inst, .. }
        | Inst::Context { inst, .. } => 1 + inst_cost(inst),
        _ => 1,
    }
}

/// The functions this one hands control to, by index.
fn callees(function: &Function, all: &[Function]) -> Vec<usize> {
    let mut paths = Vec::new();
    for block in &function.blocks {
        for inst in &block.insts {
            collect(inst, &mut paths);
        }
    }
    paths
        .into_iter()
        .filter_map(|path| all.iter().position(|f| f.path == path))
        .collect()
}

fn collect(inst: &Inst, out: &mut Vec<String>) {
    match inst {
        Inst::Call { path } | Inst::CallWithArgs { path } => out.push(path.clone()),
        Inst::StoreResult { inst, .. }
        | Inst::ReturnRun { inst }
        | Inst::Macro { inst }
        | Inst::StoreData { inst, .. }
        | Inst::StoreScaled { inst, .. }
        | Inst::StoreBoth { inst, .. }
        | Inst::Guarded { inst, .. }
        | Inst::Otherwise { inst, .. }
        | Inst::Context { inst, .. } => collect(inst, out),
        _ => {}
    }
}

/// One pass through this function and everything below it.
///
/// A call that would go back into something already on the way down is a loop: it is
/// counted once and the answer stops being exact. So is an `execute as` over a
/// selector that can find more than one entity, which runs its body once per match.
///
/// Walked rather than memoised, because the answer depends on what is above it. A
/// datapack's call graph is small enough that this is not worth being clever about.
fn walk(
    i: usize,
    own: &[u64],
    repeated: &[bool],
    calls: &[Vec<usize>],
    stack: &mut Vec<usize>,
) -> (u64, bool) {
    stack.push(i);
    let mut commands = own[i];
    let mut exact = !repeated[i];
    for callee in &calls[i] {
        if stack.contains(callee) {
            exact = false;
            continue;
        }
        let (cost, callee_exact) = walk(*callee, own, repeated, calls, stack);
        commands += cost;
        exact &= callee_exact;
    }
    stack.pop();
    (commands, exact)
}

/// Whether the function runs something once per entity a selector finds.
fn runs_per_entity(function: &Function) -> bool {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .any(per_entity)
}

fn per_entity(inst: &Inst) -> bool {
    match inst {
        Inst::Context { clause, inst } => many(clause) || per_entity(inst),
        Inst::StoreResult { inst, .. }
        | Inst::ReturnRun { inst }
        | Inst::Macro { inst }
        | Inst::StoreData { inst, .. }
        | Inst::StoreScaled { inst, .. }
        | Inst::StoreBoth { inst, .. }
        | Inst::Guarded { inst, .. }
        | Inst::Otherwise { inst, .. } => per_entity(inst),
        _ => false,
    }
}

/// Whether a selector can find more than one entity.
fn many(clause: &ExecuteAs) -> bool {
    let (ExecuteAs::As(selector) | ExecuteAs::At(selector)) = clause;
    let head = selector.split('[').next().unwrap_or(selector);
    if matches!(head, "@s" | "@p" | "@r") {
        return false;
    }
    !selector
        .split(['[', ',', ']'])
        .any(|part| part.trim() == "limit=1")
}
