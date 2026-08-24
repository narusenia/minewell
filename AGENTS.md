# AGENTS.md

Guidance for AI coding agents working in this repository.

## What this project is

**minewell-lang** — a Rust-like language (`.mwl`) that transpiles to Minecraft Java
Edition datapacks (`.mcfunction`). The compiler is written in Rust.

- Target: Minecraft Java Edition 1.21+. Bedrock is out of scope.
- Per-MC-version differences live in **toolchains** (rustup-style), not in the compiler.

## Documents — read these before making decisions

| Document | Contents |
|---|---|
| [`docs/01-requirements.md`](docs/01-requirements.md) | Requirements. Every design decision and the reasoning behind it. **Authoritative.** |
| `docs/02-spec.md` | Detailed spec: grammar EBNF, typing rules, lowering rules. Written ahead of each milestone. |
| [`docs/03-plan.md`](docs/03-plan.md) | Implementation plan, task list, progress tracking. |
| [`crates/tinymcf/SPEC.md`](crates/tinymcf/SPEC.md) | Which subset of mcfunction the interpreter models, and where it departs from vanilla. Ships with the crate. |

`01-requirements.md` is the source of truth for *what* and *why*. If an implementation
detail contradicts it, the implementation is wrong — or the requirements need an
explicit, discussed update. Do not silently diverge.

## Three design principles

Every decision in this project derives from these. Check new code against them.

1. **Anything statically known is free.** Selectors, references, coordinates and
   constant `raw!` interpolation have no runtime representation and cost zero commands.
2. **Catch bugs vanilla cannot.** mcfunction fails *silently*. Missing `@s`, ID typos and
   fixed-point scale mismatches are all statically detectable. This is why minewell exists.
3. **Generated command count is a performance requirement, not a detail.** Commands
   executed per tick translate directly into TPS lag.

## Architecture

```
crates/
  tinymcf/     mcfunction interpreter. Depends on NOTHING in this repo. Independently publishable.
  mwlc/        compiler
    syntax/      lexer -> parser -> AST
    hir/         name resolution -> type check -> monomorphization   [AST -> HIR]
    mir/         CFG construction -> SCC analysis -> regalloc         [HIR -> MIR]
    emit/        MIR -> mcfunction + datapack output
    schema/      commands.json loader (toolchain)
  mwl/         CLI
```

### Invariants — do not break these

- **`mwlc` must not depend on `tinymcf`, in either direction.** They meet only in
  dev-dependencies. This keeps `tinymcf` independently publishable.
- **Type checking belongs in HIR. Register allocation and inlining belong in MIR.**
  Do not mix them into one pass.
- Do not split `mwlc` into more crates. Internal `mod` boundaries are enough; promoting
  a module to a crate later is mechanical, and drawing boundaries around code that does
  not exist yet is speculation.
- Whole-program compilation. There is no separate compilation, by design — it is what
  lets regalloc and SCC analysis work across the entire program.

## Workflow: spec-driven + TDD

The spec leads the implementation, not the other way around. Before implementing a
task, settle the corresponding section of its spec — `crates/tinymcf/SPEC.md` for
interpreter work, `docs/02-spec.md` for language work. Sections are marked **done** or
*pending*; moving one to done is part of the task, not a follow-up.

Every task in `docs/03-plan.md` names the test to write first.

```
1. Write the "test to write first"  -> confirm it fails
2. Write the smallest passing implementation -> confirm it passes
3. Refactor -> confirm tests still pass
4. Commit (at least one commit per task)
```

### Why `tinymcf` comes first

Without an interpreter, every transpiler test degenerates into a golden-file diff, and
refactoring becomes impossible. `tinymcf` lets tests assert `fact(5) == 120` in plain
Rust, and lets optimization passes be validated by "semantics unchanged" rather than by
eyeballing output. Do not start compiler work before M0 is done.

### Test layers

| Layer | Tool | What it proves |
|---|---|---|
| lexer / parser / typeck | plain `#[test]` | ~80% of tests live here |
| codegen | `insta` snapshots | Shape of output. For diff review, **not** proof of correctness |
| semantics | `tinymcf` | Actual behaviour |
| integration | real server (manual / nightly) | That `tinymcf`'s model matches reality |

Do not use a snapshot where a `tinymcf` assertion would do.

## Tooling — mise

Tools and tasks are managed by [`mise.toml`](mise.toml). Prefer `mise run <task>` over
raw cargo invocations.

```
mise install              install rust (with rustfmt + clippy) and cargo-insta
mise run test             all tests
mise run test:tinymcf     interpreter only (the loop for M0)
mise run snap             review insta snapshots
mise run lint             clippy, warnings denied
mise run fmt              format
mise run ci               fmt:check + lint + test. Run before pushing
```

## Definition of done

A task is complete when:

- The task's "test to write first" passes
- `mise run ci` exits 0. **Check the exit status, not the output** — the tasks run in
  parallel, so a green `test` line can sit above a failed `lint`
- Snapshots are updated if output changed (`mise run snap`)
- The checkbox and the summary table in `docs/03-plan.md` are both updated

## Conventions

### Commits

- One logical unit per commit. Never batch unrelated changes; never commit everything at the end
- Single line, English, Conventional prefix: `feat:` `fix:` `refactor:` `docs:` `test:`
  `chore:` `perf:` `ci:`
- Say what changed, not why it was found
  - Good: `feat: add NodeData trait hierarchy and concrete types`
  - Good: `test: add unit tests for topological sort`
  - Bad: `feat: implement M4-3`
  - Bad: `fix: review feedback`
- **Never reference plan task IDs (`M4-3`), issue numbers, or tickets in commit messages**

### Branches

- Conventional prefix + kebab-case, naming the actual feature or fix
  - Good: `feat/tarjan-scc-analysis`, `fix/wgpu-shader-compilation`
  - Bad: `feat/m4`, `chore/cleanup`

### Licensing

MIT. Every `.rs`, `.toml` and `.yml` file starts with an SPDX line:

```
// SPDX-License-Identifier: MIT
```

New files get one. Markdown is covered by the repository `LICENSE` and carries no
header, so that documents render clean.

### Naming inside generated datapacks

These are load-bearing; see `docs/01-requirements.md` §3.3.

- Internal fake players are prefixed `$` (invalid as a real player name, so they can
  never collide with an actual player)
- Objectives: `<namespace>.t` (temporaries) and `<namespace>.v` (user variables). Two, no more
- Storage: `<namespace>:mw`, one only. Split by root path (`mw.vars`, `mw.stack`,
  `mw.args`, `mw.iter`)
- Compiler-generated functions go in a subdirectory of their parent
  (`myns:combat/damage/apply/if_0`), never in a flat `__gen/` directory — debuggability
  of the output is a requirement

## Out of scope for v1

Do not implement these. Reserve the identifiers only.

`async` / `await`, `trait` / `dyn`, `Result`, user-defined macros, borrow checker,
advancement / predicate generation, LSP, package registry, dynamic dispatch,
separate compilation.

If a task seems to need one of them, that is a signal the approach is wrong. Raise it
rather than quietly widening the scope.
