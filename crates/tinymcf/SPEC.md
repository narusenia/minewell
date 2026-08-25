# tinymcf — modelled subset of mcfunction

What this interpreter promises to model, and where it knowingly departs from vanilla.
Anything not listed here is out of scope; if a caller needs it, this document changes
first.

Status markers: **done** / *pending*. Pending items name the task in
`docs/03-plan.md` of the parent repository.

## 1. Purpose and non-goals

tinymcf exists so a transpiler targeting mcfunction can assert on **behaviour**
(`fact(5) == 120`) rather than on generated text, and can **count commands**.
Generated command count is a performance requirement for datapacks, so it must be
measurable in a unit test.

Explicit non-goals:

- Being a Minecraft server. There is no world, no entities, no blocks, no ticks.
- Command coverage for its own sake. Only what a compiler needs to prove its output.
- Byte-compatible NBT serialisation. The in-memory model and SNBT are the interface.

## 2. Outcome model

Every command produces an outcome:

```
Outcome { success: u32, result: i32 }
```

`success` is the count `execute store success` observes; `result` is what
`execute store result` observes. A `success` of 0 means the command failed.

**A failed command does not abort anything.** Execution continues with the next line,
exactly as in vanilla — only `return` ends a function early. A command that vanilla
would reject with red text (an unknown objective, a score that is not set) is a
failure, not an abort, and `execute store success` writes 0 for it. Modelling such a
command as an early exit would make generated code look correct that vanilla runs
straight past.

Failures also append a message to the run's diagnostic log, so a test can assert *why*
something did nothing. This is the interpreter's answer to mcfunction failing silently:
nothing is silent here.

`Err` is reserved for what cannot be attempted at all — a line that does not parse.

## 3. Data model

### 3.1 NBT — **done**

Twelve tags, each distinct: `byte` `short` `int` `long` `float` `double` `string`
`list` `compound` `byte_array` `int_array` `long_array`.

`Byte(1) != Int(1)`. The tag is load-bearing: vanilla silently ignores data written
with the wrong tag, so collapsing the numeric tags into one type would erase the bug.

Compound fields are stored in a `BTreeMap`. Consequences, both deliberate:

- Field order is **key order**, not insertion order. Output and snapshots are
  deterministic.
- Compound equality ignores the order fields were written in.

List equality respects order.

### 3.2 SNBT — **done**

Grammar as accepted by `snbt::parse`:

```
value    := compound | list | array | quoted | bare
compound := '{' [ key ':' value { ',' key ':' value } ] '}'
key      := quoted | bare-word
list     := '[' [ value { ',' value } ] ']'
array    := '[' ('B'|'I'|'L') ';' [ value { ',' value } ] ']'
quoted   := '"' char* '"' | "'" char* "'"
bare     := bare-word
bare-word:= [A-Za-z0-9_.+-]+
```

- Escapes inside quoted strings: `\\`, `\"`, `\'`. Any other escape is an error.
- **`:` is not a bare-word character.** Resource locations must be quoted:
  `{id:"minecraft:stone"}`, not `{id:minecraft:stone}`. This matches vanilla and is
  what lets `key:value` parse unambiguously.
- A bare word is a number when it parses as one and a string otherwise. A type suffix
  counts only when what precedes it is numeric, so `1b` is `Byte(1)` and `b` is
  `String("b")`.
- `true` / `false` are `Byte(1)` / `Byte(0)`.
- A typed array whose elements are not all of its tag is an error.
- Trailing input after a complete value is an error.

Formatting is canonical and round-trips: `parse(v.to_string()) == v`. Floats and
doubles always carry a decimal point (`20f` formats as `20.0f`). Compound keys are
left unquoted when every character is a bare-word character.

### 3.3 Scoreboard — **done**

Keyed by `(objective, holder)`, values `i32`.

- An objective must be added before use. Reading or writing an unknown objective is
  `Err(NoSuchObjective)`, never a silent 0.
- "No score" and "score 0" are different states. `get` returns `Option<i32>`.
- Removing an objective drops its scores.

Holders are opaque strings. Fake players (names beginning `$`) need no special
handling here — they are invalid as real player names, which is precisely why the
compiler uses them.

### 3.4 Storage — **done**

`namespace -> NbtValue`, where the root of every namespace is a compound. An absent
namespace reads as an empty compound, as in vanilla.

### 3.5 NBT paths — **done**

```
path     := head { '.' seg }
head     := filter | seg
seg      := name [ filter ] { index }
name     := quoted | bare-name
bare-name:= any run of characters other than . [ ] { } " ' and whitespace
filter   := compound            -- partial match, applied to the current values
index    := '[' ']'             -- every element
          | '[' int ']'         -- by position, negative counts from the end
          | '[' compound ']'    -- elements matching
```

A path resolves to **zero or more** values. Commands that require exactly one target
report a success count of 0 when the path matches none; they are not errors.

Filter matching is **partial and recursive**: the target must be a compound containing
at least the given keys, each of whose values matches by this same rule. Non-compound
values match only by equality, tag included.

Indexing applies to lists and to the typed arrays. `[-1]` is the last element. An index
outside the bounds matches nothing.

Writes create missing values along `name` steps only; **indices and filters never
create**. The created value takes the shape the *following* step needs — a list when
that step is an index, a compound otherwise. This is vanilla's "preferred parent" rule,
and it means a failed write can leave a partially built structure behind: `a[0].b` on
an empty root creates `a:[]`, then matches nothing and writes nothing. `a.b.c` creates
two compounds and writes.

Typed-array elements are readable through a path but not writable; `data modify` into
an `[I;…]` is out of scope.

A trailing filter step (`a{k:1}`) cannot be removed, since removal needs the parent of
the addressed value and a filter does not descend.

Removal detaches every matched value from its parent, and reports how many.

## 4. Commands

Command lines are split by a single balanced scanner: whitespace separates arguments
unless it sits inside brackets, braces or quotes. That makes selectors
(`@e[type=zombie, distance=..8]`), SNBT (`{Health: 20f}`) and quoted strings single
arguments without each command needing to know. A trailing greedy argument (`say hi
there`) takes the rest of the line.

### 4.1 `scoreboard` — **done**

```
scoreboard objectives add <name> <criteria> [<display name>]
scoreboard objectives remove <name>
scoreboard players get <target> <objective>
scoreboard players set|add|remove <target> <objective> <int>
scoreboard players reset <target> [<objective>]
scoreboard players operation <target> <objective> <op> <source> <objective>
```

Operators: `=` `+=` `-=` `*=` `/=` `%=` `<` (min) `>` (max) `><` (swap).

- Arithmetic is Java `int`: it **wraps** on overflow rather than panicking.
- `/=` and `%=` are **floored**, not truncating: `-7 /= 2` is `-4` and `-7 %= 2` is `1`.
  Rust's `/` and `%` would give `-3` and `-1`, so both are written out explicitly.
- Division or modulo by zero fails and leaves the target untouched.
- `set`, `add`, `remove` and `operation` create a missing score as 0 before acting —
  including the *source* of an operation. `get` does not: reading a score that is not
  set is a failure.
- Adding an objective that already exists is a failure, as in vanilla. A `#[load]`
  function that runs twice will log two of these; that is what vanilla does too.
- The criteria and display name are parsed and discarded. Only `dummy` is meaningful
  to a compiler, and nothing here observes the others.

`result` is the score after the command for `get`, `set`, `add`, `remove` and
`operation`, and 1 for the objective commands.

### 4.2 `data` — **done**

```
data get <target> [<path>] [<scale>]
data merge <target> <nbt>
data remove <target> <path>
data modify <target> <path> <operation>

operation := set value <nbt>
           | set|append|prepend|merge from <target> [<path>]
           | insert <index> from <target> [<path>]
           | set|append|prepend|merge string <target> [<path>] [<start>] [<end>]
           | insert <index> string ...
           | append|prepend|merge value <nbt>
           | insert <index> value <nbt>
target    := storage <id> | entity <selector> | block <pos>
```

**Only `storage` targets execute.** `entity` and `block` parse, so that a compiler's
output is still recognised, but running one fails with a diagnostic saying so. There is
no world here (§1); registering stub entity NBT is deferred to the task that first
needs it.

`data get` with a path returns, scaled by `scale` (default 1.0) and floored:

| Target | Value |
|---|---|
| Numeric tag | its numeric value |
| String | its length |
| List, array, compound | its number of elements |

The string case is what makes `String::len()` free in the source language, so it is
tested rather than assumed. `data get` with no path returns 1.

A path matching **nothing** fails, and **records no diagnostic**: a path that is not
there is an answer in vanilla's data model, the same way an unset score is (§4.4), and
it is what `Option<T>` is made of in the source language. For `get` and for the *source*
of a `from` or `string` operation, a path matching **more than one** value also fails —
that one *is* a diagnostic, because nothing sensible was asked for. Everywhere else all
matches are acted on.

`string` converts the source to text — the characters themselves for a string tag, its
SNBT otherwise — and takes `[start, end)`. Negative bounds count from the end; an
omitted `end` runs to the end.

A missing target path is created by `set` and by the list operations, the latter as an
empty list. This follows the same preferred-parent rule as §3.5.

`data remove` reports how many values it detached as its success count.

### 4.3 `function` and `return` — **done**

```
function <id>
return <value>
return run <command>
return fail
```

A function is loaded from text. Blank lines and lines whose first non-blank character
is `#` are dropped; everything else is parsed **at load time**, so a syntax error
surfaces when the pack is loaded rather than when the line happens to run.

`return` ends the current function at once; the lines after it do not run. It does not
propagate: the caller carries on with its next line.

A function call's outcome is:

| Case | success | result |
|---|---|---|
| `return <value>` | 1 | the value |
| `return run <command>` | the command's | the command's |
| `return fail` | 0 | 0 |
| fell off the end | 1 | the number of commands the function ran |

Calling an unknown function fails.

### 4.4 `execute` — **done**, except the entity clauses (M0-8b)

```
execute <clause>* [run <command>]

clause := if|unless score <holder> <obj> <|<=|=|>=|> <holder> <obj>
        | if|unless score <holder> <obj> matches <range>
        | if|unless data <target> <path>
        | store result|success score <holder> <obj>
        | store result|success storage <id> <path> <type> <scale>
range  := <int> | <min>.. | ..<max> | <min>..<max>
type   := byte | short | int | long | float | double
```

With `run`, the outcome is the command's. Without it, the outcome is 1 when every
condition holds and 0 otherwise.

Conditions are evaluated left to right. A score that is not set makes a condition
false; it is not an error.

`store` clauses apply **after** the command, and apply even when it failed — a failed
command stores `success` 0 and `result` 0. Code that reads back a store therefore sees
a definite value either way, which is what makes `Option<T>` cheap in the source
language. The stored number is `value × scale`, converted to the named tag; `byte`,
`short`, `int` and `long` truncate toward zero and wrap.

Nesting works: `run` takes a whole command, `execute` included.

#### Context clauses — **done** for `as`, `at` and `if entity`

```
clause += as <selector> | at <selector>
cond   += entity <selector>
```

There is no world (§1), so **the harness declares what a selector finds**:

```rust
world.spawn("zombie-1", [8.0, 64.0, 0.0]);
world.bind_selector("@e[type=zombie]", ["zombie-1", "zombie-2"]);
```

A selector with no binding finds nothing. `@s` is the current executor and needs no
binding.

`as` and `at` **fork**: the rest of the execute runs once per entity found, and the
command's success count is how many of those runs succeeded. `as` changes the executor
and not the position; `at` changes the position and not the executor. An execute that
finds no entities runs its command zero times and reports 0.

Every logged side effect records the executor it ran as, so a test can assert not only
that something happened but who it happened for.

#### Still deferred

`positioned`, `in`, `if block` and `if predicate` parse and fail with a diagnostic
naming themselves. Nothing needs them yet: `positioned` and `in` want coordinate and
dimension models, and the two conditions want a block and predicate registry. They will
arrive with the first task that has a use for them.

### 4.5 Macro functions — **done**

```
function <id> <compound>
function <id> with storage <id> [<path>]
```

A line whose first character is `$` is a **macro line**. The `$` is dropped and every
`$(name)` in the rest is replaced by the argument of that name, after which the line is
parsed and run. A function containing at least one macro line is a macro function.

- Macro lines are parsed **per call**, after substitution — a load-time parse is
  impossible, so a macro line with a syntax error only fails when it runs.
- Calling a macro function **without** arguments fails. This is what makes "a `#[tick]`
  function must not be a macro function" a rule a compiler can be tested against:
  function tags invoke without arguments.
- Referring to an argument that was not supplied fails.
- Passing arguments to a function with no macro lines is allowed and does nothing.

Substitution renders a value as vanilla does: a string inserts its characters with no
quotes, an integer or a decimal inserts its number with no tag suffix, and anything
else inserts its SNBT.

`with entity` and `with block` parse and fail, like every other entity target (§4.2).

### 4.6 Side-effecting commands — **done**

`say`, `tellraw`, `setblock`, `summon`, `kill` and the like are **not** simulated. Each
is appended to an ordered log as `(name, arguments)`, with the arguments exactly as
written, and reports success.

Only commands that actually run are logged, so the log doubles as a trace: a command
skipped by a false `execute if` leaves nothing behind.

Unrecognised commands are retained verbatim rather than rejected, so that a compiler
emitting something outside this subset still produces a runnable trace.

## 5. Limits and measurement — **done**

A run has a command budget, `maxCommandChainLength`, default 65536. Executing more
commands than that stops the run and records a diagnostic. This is not only fidelity to
vanilla: it is what stops a runaway recursion in a test from hanging.

There is also a **call depth limit**, default 256, which exceeding fails with a
diagnostic. This one is *not* vanilla — vanilla's executor is a queue and has no depth
of its own. tinymcf walks calls recursively, so without a limit a runaway recursion
overflows the native stack and takes the test process with it. Raising the limit means
giving the interpreter a bigger thread stack to run on.

A report is available at any point:

| Field | Meaning |
|---|---|
| `commands` | every command executed, at any depth |
| `per_function` | commands charged to each function |
| `max_depth` | deepest nesting of function calls reached |
| `over_budget` | whether the budget stopped the run |

Accounting rules, so the numbers are derivable rather than magic:

- **One command, one charge**, wherever it ran.
- A command is charged to the **innermost function** it ran inside. Commands typed at
  the top level are counted in `commands` but charged to no function.
- `execute ... run <command>` charges **both**: the `execute` itself and the command it
  ran. That mirrors what the game does, and it is why an `execute` chain is not free.
- A `function` command is charged where it appears; the body's commands are charged to
  the callee.

These numbers are the evidence for optimisation work: a pass is justified when the
count drops and the semantics tests still pass.

## 6. Determinism

Identical input produces identical output, always. No clocks, no randomness, no hash
iteration order. Commands that are random in vanilla are out of scope rather than
approximated.
