# minewell

A Rust-like language that compiles to Minecraft Java Edition datapacks.

```rust
#[tick]
fn burn_the_undead() {
    for z in @e[type=zombie, distance=..8] {
        at self {
            summon(minecraft:small_fireball, pos!(~ ~1 ~));
        }
    }
}
```

Source files are `.mwl`. Output is a datapack full of `.mcfunction`.

> **Status: early.** The language does not compile anything yet. What exists today is
> `tinymcf`, the mcfunction interpreter the compiler will be tested against. See
> [the plan](docs/03-plan.md) for where things are.

## Why

Writing datapacks means writing mcfunction, and mcfunction has one dominant failure
mode: **it fails silently.** A command run without an executor does nothing. A typo in
an ID does nothing. Writing `Byte(1)` where the game wanted `Int(1)` does nothing. No
error, no log line, no clue.

Almost all of it is statically detectable. minewell's job is to detect it.

```rust
#[ctx(entity)]              // this function needs an executor
fn take_damage() { ... }

fn on_load() {
    take_damage();          // compile error: no executor in this context
}
```

That check cannot be expressed in vanilla, and it is the single largest source of lost
hours in datapack development.

## Design in one page

Three principles decide everything:

1. **Anything statically known is free.** Selectors, references, coordinates and
   constant string interpolation have no runtime representation and cost zero commands.
2. **Catch what vanilla cannot.** Missing executors, unknown IDs, fixed-point scale
   mismatches — all compile errors.
3. **Generated command count is a performance requirement.** Commands per tick are TPS
   lag. The compiler reports what each function costs.

Consequences worth knowing before you read further:

- **`i32` and `bool` live on the scoreboard; structs, lists and strings live in
  storage.** That is not a design choice, it is the only split the game allows —
  arithmetic exists only on scoreboards, structure only in NBT.
- **No floating point.** `fix<S>` is a fixed-point integer with its scale in the type,
  so `fix<100> + fix<1000>` is a compile error. `f32`/`f64` exist for reading and
  writing game NBT and cannot be used in arithmetic.
- **No ownership, no borrow checker.** There is nothing to free. `&mut` exists but is
  purely a compile-time name for a storage path.
- **`raw!("...")`** is always there when the language is in your way.

The full reasoning is in [the requirements](docs/01-requirements.md).

## Repository

| Path | What |
|---|---|
| [`crates/tinymcf`](crates/tinymcf) | mcfunction interpreter. Depends on nothing else here; independently publishable |
| `crates/mwlc` | the compiler *(not started)* |
| `crates/mwl` | the CLI *(not started)* |
| [`docs/01-requirements.md`](docs/01-requirements.md) | every design decision and why |
| [`docs/03-plan.md`](docs/03-plan.md) | task list and progress |

### tinymcf

The compiler is developed test-first, and a transpiler tested only on the text it emits
cannot be refactored — every optimisation invalidates every expected string. So the
tests assert on **behaviour**:

```rust
let mut mc = Interpreter::default();
mc.load("test:fact", include_str!("fact.mcfunction"))?;
mc.run_line("function test:fact");
assert_eq!(mc.world.scoreboard.get("obj", "$result"), Ok(Some(120)));
```

It models scoreboards, storage, NBT paths, `execute`, `function`/`return` and macro
functions — and counts commands, because that number is a requirement. It does not
model a world: entities and blocks are recorded, not simulated.
[`SPEC.md`](crates/tinymcf/SPEC.md) states exactly what it promises and where it
knowingly departs from vanilla.

## Building

Tools and tasks come from [mise](https://mise.jdx.dev).

```sh
mise install
mise run test
mise run ci      # what CI runs
```

## Contributing

Read [AGENTS.md](AGENTS.md) first — it holds the conventions, the invariants that must
not be broken, and the spec-driven, test-first workflow. It is written for AI agents
and applies just as well to people.

## Licence

MIT. See [LICENSE](LICENSE).
