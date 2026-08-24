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

## 2. Failure model

Two distinct outcomes, which vanilla also distinguishes and which callers must not
conflate:

| Outcome | Vanilla | Here |
|---|---|---|
| Command rejected outright (red text, does not run) | e.g. unknown objective | `Err(Error)` |
| Command ran and did nothing | e.g. no entity matched | `Ok` with success count 0 |

`execute store success` observes the second, never the first. Modelling a rejected
command as "did nothing" would hide exactly the class of compiler bug this interpreter
exists to catch — for instance a missing `scoreboard objectives add`.

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

### 4.1 `scoreboard` — *pending (M0-5)*

`objectives add|remove`, `players get|set|add|remove|reset|operation`.

All `operation` operators: `=` `+=` `-=` `*=` `/=` `%=` `<` `>` `><`.

Integer division and modulo follow vanilla, which uses floored (Euclidean-style)
semantics rather than Rust's truncating `/` and `%`. Division by zero fails the
command rather than trapping.

### 4.2 `data` — *pending (M0-6)*

`get`, `merge`, `remove`, and `modify ... set value|set from|set string|append|prepend|insert|merge`.

`data get` returns, as the command's result value:

| Target | Result |
|---|---|
| Numeric tag | value × scale, truncated |
| String | its length |
| List, array, compound | its number of elements |

The string case is what makes `String::len()` free in the source language, so it is
tested rather than assumed.

`set string <source> <start> <end>` extracts a substring.

### 4.3 `function` and `return` — *pending (M0-7)*

`function <id>`, `function <id> with storage <id> <path>`, `return <value>`,
`return run <command>`, `return fail`.

`return` ends the current function immediately; later commands in it do not run.

### 4.4 `execute` — *pending (M0-8)*

- `if` / `unless`: `score`, `data`, `entity`, `block`, `predicate`
- `store result` / `store success` into `score` or `storage`, with scale and tag
- `as`, `at`, `positioned`, `in`

Context is executor, position, rotation and dimension. `as` changes the executor and
**not** the position; `at` changes the position.

Since there are no entities (§1), `as` and `if entity` operate on selector strings the
caller has registered with the harness rather than on a simulated world.

### 4.5 Macro functions — *pending (M0-9)*

Lines beginning `$` are macro lines. `$(name)` is substituted from the compound the
function was invoked with. Invoking a macro function without the arguments it
references fails, as in vanilla — this is what makes "a `#[tick]` function must not be
a macro function" a testable rule.

### 4.6 Side-effecting commands — *pending (M0-10)*

`say`, `tellraw`, `setblock`, `summon`, `kill` and the like are **not** simulated.
Each is recorded as `(name, arguments)` in an ordered log the test can assert on.

Unrecognised commands are retained verbatim rather than rejected, so that a compiler
emitting something outside this subset still produces a runnable trace.

## 5. Measurement — *pending (M0-11)*

Per run: total commands executed, a per-function breakdown, maximum call depth, and
whether `maxCommandChainLength` (default 65536) would have been exceeded.

These numbers are the evidence for optimisation work: a pass is justified when the
count drops and the semantics tests still pass.

## 6. Determinism

Identical input produces identical output, always. No clocks, no randomness, no hash
iteration order. Commands that are random in vanilla are out of scope rather than
approximated.
