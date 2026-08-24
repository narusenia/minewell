# minewell-lang 詳細仕様

要件定義: [`01-requirements.md`](./01-requirements.md) — **何を・なぜ**の決定はすべてあちら。
本書は**どう書くか**だけを定める。両者が矛盾したら要件定義が正しい。

各節に **確定** / *未定* を付ける。*未定*の節は、それを実装するタスクの直前に確定させる
（[`03-plan.md`](./03-plan.md) の運用ルール）。

---

## 1. 記法 — 確定

文法は EBNF で書く。

| 記法 | 意味 |
|---|---|
| `"..."` | 終端記号 |
| `A B` | 連接 |
| <code>A \| B</code> | 選択 |
| `[A]` | 省略可 |
| `{A}` | 0 回以上の繰り返し |
| `(A)` | グループ |
| `A - B` | A のうち B を除く |

字句規則は `UPPER_SNAKE`、構文規則は `lower_snake` で書く。

---

## 2. 字句構造 — 確定

### 2.1 ソース

UTF-8。BOM は先頭に 1 つだけ許し、読み飛ばす。改行は `\n` と `\r\n`。

### 2.2 空白とコメント

```
WHITESPACE := " " | "\t" | "\n" | "\r"
LINE_COMMENT  := "//" {ANY - "\n"}
BLOCK_COMMENT := "/*" {BLOCK_COMMENT | ANY} "*/"
```

ブロックコメントは**ネストする**（Rust と同じ）。コメントは空白として扱う。

`///` と `//!` は将来のドキュメントコメント用に**予約**する。v1 では通常のコメントとして
読み飛ばすが、字句解析器は種別を区別して保持する。

### 2.3 識別子

```
IDENT := (ALPHA | "_") {ALPHA | DIGIT | "_"}
ALPHA := "a".."z" | "A".."Z"
```

**ASCII のみ。** Unicode 識別子を許さないのは、生成される mcfunction の関数名・
objective 名・NBT キーに識別子が現れ、Minecraft 側の許容文字が ASCII に限られるため。
非 ASCII を許すと、コンパイラがどこかで必ずマングリングを強いられる。

`_` 単独は識別子ではなくワイルドカードパターン（[§2.6](#26-記号)）。

### 2.4 キーワード

**使用中:**

```
fn let mut const  if else match  while loop for in break continue return
as at  struct enum impl  mod use pub  true false
```

`as` と `at` は実行コンテキストのブロック構文（要件定義 §6.1）であり、
Rust の型キャストの `as` ではない。**minewell に型キャストの `as` は無い** —
数値変換はすべて明示的な関数で行う（[§5](#5-型-未定) 参照）。

**予約のみ**（使うとエラー。要件定義 §19）:

```
async await  trait dyn impl_trait  macro_rules  Self self  where  unsafe  static
type  ref  move  box  yield  do  try
```

予約語をエラーにするのは、後から追加したときに既存コードが壊れないようにするため。

### 2.5 リテラル

```
INT     := DEC | HEX | BIN
DEC     := DIGIT {DIGIT | "_"}
HEX     := "0x" HEXDIGIT {HEXDIGIT | "_"}
BIN     := "0b" ("0" | "1") {"0" | "1" | "_"}
BOOL    := "true" | "false"
STRING  := '"' {STRING_CHAR | ESCAPE} '"'
ESCAPE  := "\\" ("\\" | '"' | "n" | "t" | "r" | "0")
```

- **浮動小数点リテラルは無い。** 実数は `fix<S>` であり、`1.5` のような字面は
  スケールが決まらないので書けない。`fix::<1000>(1500)` のように整数から作る
  （[§5](#5-型-未定)）。これは要件定義 §4.1 の「スケールを型で持つ」の当然の帰結。
- 整数リテラルは `i32` の範囲。溢れる字面はコンパイルエラー。
- 文字リテラル (`'a'`) は無い。`char` 型が無いため。

### 2.6 記号

```
+  -  *  /  %
== != <  <= >  >=
&& || !
=  += -= *= /= %=
&  &&                     参照（&& は二重参照ではなく && トークンとして扱い、
                          パーサが必要に応じて分割する）
.  ,  ;  :  ::  ->  =>  ..  ..=  #  @  ~  ^  ?  _
(  )  [  ]  {  }
<  >                      ジェネリクスの区切りにも使う
```

**ビット演算子は無い。** `& | ^ << >> ~` をビット演算として使わない理由:
scoreboard にビット演算が無く、実装すればループを吐くことになる。要件定義の設計原則 3
（生成コマンド数は性能要件）に照らして、1 演算子が数十コマンドに化けるのは許容できない。

その結果 `^` と `~` は演算子として空いており、座標リテラルのトークンとして使える。
ただし座標は `pos!(~ ~1 ~)` とマクロの中に閉じ込める（要件定義 §10.2）。
`pos!` の引数はトークン列として受け取り、専用の文法で解釈する。

### 2.7 セレクタ

```
SELECTOR := "@" ("a" | "e" | "p" | "r" | "s") ["[" SELECTOR_BODY "]"]
```

`SELECTOR_BODY` は括弧の対応が取れた任意のトークン列で、字句解析器はここを
**構造化せずそのまま保持**する。中身の解釈は M5 のセレクタパーサが行う。

`[` `]` `{` `}` の対応と、引用文字列の中の括弧は字句段階で正しく数える。
セレクタ内には空白を書いてよい（`@e[type=zombie, distance=..8]`）。

### 2.8 リソースロケーション

```
RESOURCE := IDENT {"/" IDENT} ":" SEG {"/" SEG}
SEG      := (letter | digit | "_" | "." | "-")+
```

**`:` の後ろのセグメントは `.` と `-` を含める。** バニラの ID がそう作られているため
（`minecraft:block.note_block.pling` は 1 つの音声 ID）。リソースロケーションに
フィールドは無いので、直後の `.` がフィールドアクセスと衝突することもない。

**`:` の両側に空白を書けない。** これが型注釈の `:` と区別する唯一の規則
（要件定義 §10.2）。`minecraft:stone` は 1 トークン、`x: i32` は 3 トークン、
`x:i32` は……**リソースロケーションとして字句解析される。**

この曖昧性は実在する。解決規則:

> `IDENT ":" IDENT` の形が空白なしで現れたとき、それは常にリソースロケーションである。
> 型注釈で `:` の直後に空白を置かないのは**構文エラー**とし、
> 「型注釈の `:` の後には空白が必要」という診断を出す。

`let x:i32 = 1;` を黙って通さないのは、通せば `let block:minecraft:stone` のような
入力で診断が意味不明になるため。空白 1 つを強制するほうが、両方の記法を守れる。

### 2.9 マクロ呼び出し

```
MACRO_CALL := IDENT "!" ("(" TOKENS ")" | "[" TOKENS "]" | "{" TOKENS "}")
```

`TOKENS` は括弧の対応が取れたトークン列。**中身は字句段階で解釈しない。**
組み込みマクロ（`raw!` `pos!` `nbt!` `text!` `debug_assert!`）ごとに専用の文法で
解釈する。ユーザ定義マクロは v1 に無い（要件定義 §10.3）。

### 2.10 属性

```
ATTRIBUTE := "#" "[" TOKENS "]"
```

内側属性 (`#![...]`) は v1 では持たない。

### 2.11 スパン

すべてのトークンはソース内のバイト範囲を保持する。診断（`miette`）と、
debug ビルドで生成 mcfunction に埋める `# src/foo.mwl:42` の両方が同じスパンを使う
（要件定義 §15）。

---

## 3. 構文 — M6 と `struct` の範囲まで確定

M1 は `fn main() { raw!("say hi"); }` を全レイヤ貫通させることだけを目的とする
（[`03-plan.md`](./03-plan.md) M1）。ここで確定させるのはその範囲に限る。

```
source_file := {item}

item        := {ATTRIBUTE} fn_item
fn_item     := "fn" IDENT "(" ")" block

block       := "{" {stmt} "}"
stmt        := expr_stmt
expr_stmt   := expr ";"

expr        := macro_call
macro_call  := MACRO_CALL
```

### 3.2 `let` と式 — 確定（M2）

```
stmt        := let_stmt | expr_stmt
let_stmt    := "let" ["mut"] IDENT [":" type] "=" expr ";"
expr_stmt   := expr ";"

type        := IDENT

expr        := assign
assign      := or [assign_op or]
assign_op   := "=" | "+=" | "-=" | "*=" | "/=" | "%="
or          := and {"||" and}
and         := compare {"&&" compare}
compare     := sum [compare_op sum]
compare_op  := "==" | "!=" | "<" | "<=" | ">" | ">="
sum         := product {("+" | "-") product}
product     := unary {("*" | "/" | "%") unary}
unary       := ["-" | "!"] primary
primary     := INT | BOOL | IDENT | macro_call | "(" expr ")"
```

優先順位は上の階層がそのまま表す。低い順に
代入 → `||` → `&&` → 比較 → `+ -` → `* /` `%` → 単項 → 一次式。

- **比較は連鎖しない。** `a < b < c` は構文エラー（Rust と同じ）。`(a < b) < c` と書けば通るが、
  それは書き手が本当にそう書いたときだけ。
- 代入は右結合。代入は**文としてのみ**意味を持ち、値を返さない（[§4.3](#43-代入)）。
- 単項 `-` は `i32`、`!` は `bool`。
- `mut` の無い束縛への代入はエラー。

### 3.4 制御フロー — 確定（M3）

```
stmt        := let_stmt | expr_stmt | if_stmt | while_stmt | loop_stmt
             | "break" ";" | "continue" ";" | "return" ";"
if_stmt     := "if" expr block ["else" (block | if_stmt)]
while_stmt  := "while" expr block
loop_stmt   := "loop" block
```

- 条件は `bool` でなければならない。**`i32` の 0 / 非 0 を条件にできない。**
- `if` は**文**であって式ではない。値を持つ `if` は合流点で値を作る必要があり、
  それは M4 で関数の戻り値を入れるときに一緒に考える。
- `break` / `continue` はループの外で使うとエラー。
- `return` は M3 では値を取らない。関数がまだ値を返さないため。
- `else if` は `else` の後に `if_stmt` を置く形なので、追加の規則は要らない。

### 3.6 関数 — 確定（M4）

```
fn_item     := "fn" IDENT "(" [params] ")" ["->" type] block
params      := param {"," param} [","]
param       := IDENT ":" type
call        := IDENT "(" [args] ")"
args        := expr {"," expr} [","]
return_stmt := "return" [expr] ";"
```

- 引数の型注釈は必須。戻り値の型は省略すると「返さない」。
- 呼び出しは一次式なので、`f(1) + g(2)` のように式の中に書ける。
- 値を返さない関数の呼び出しを式の中に置くとエラー。文としてなら書ける。
- 値を返す関数で、`return` せずに末尾まで到達したらエラー。
  **末尾式による暗黙の戻り値は無い** — `if` が式でない以上、末尾式だけを特別扱いしても
  一貫しない。M4 では `return` だけ。

`if` を式にするのは引き続き**未定**。合流点で値を作る仕組みが要り、
`match` を入れる M7 と一緒に考えるほうが小さい。

### 3.8 実行コンテキスト — 確定（M5）

```
stmt     += as_stmt | at_stmt | for_stmt
as_stmt  := "as" expr block
at_stmt  := "at" expr block
for_stmt := "for" IDENT "in" expr block
primary  += SELECTOR
attribute+= "#" "[" "ctx" "(" ctx_kind {"," ctx_kind} ")" "]"
ctx_kind := "entity" | "position"
```

**`at self` ではなく `at @s` と書く。** 要件定義 §6.1 の例は `at self` だったが、
`self` は M7 の `impl` のレシーバに予約してある（§2.4）。同じ語を 2 つの意味に使うより、
セレクタは全部 `@` で始まるほうが一貫する。

`for` が束縛する名前は、その反復のエンティティを指すセレクタで、値は `@s`。

`dimension` は `ctx_kind` に含めない。次元を切り替える手段（`in`）が
まだインタプリタに無く、検査できない要求を宣言できるようにしても意味が無い。

### 3.9 コマンド呼び出しとドメインリテラル — 確定（M6）

```
primary  += RESOURCE | pos_macro
pos_macro:= "pos" "!" "(" coord coord coord ")"
coord    := ["~" | "^"] [INT]
```

コマンドは**関数呼び出しの形**で書く。名前は toolchain が生成する（[§6.16](#616-コマンド呼び出し)）。

```rust
setblock(pos!(~ ~1 ~), minecraft:stone);
```

- `minecraft:stone` はリソースロケーションのリテラル（§2.8）。型は `ResourceLocation`
- `pos!` は座標リテラル。3 つの座標の記法は揃っていなければならない
  （絶対 / `~` 相対 / `^` ローカルの混在は不可）
- ユーザ定義関数とコマンドは名前空間を共有する。同名の関数を定義するとコマンドを覆い隠す

### 3.10 `struct` — 確定（M7）

```
item        += struct_item
struct_item := "struct" IDENT "{" [field_defs] "}"
field_defs  := field_def {"," field_def} [","]
field_def   := {ATTRIBUTE} IDENT ":" type

primary     += struct_lit | field_access
struct_lit  := IDENT "{" [field_inits] "}"
field_inits := field_init {"," field_init} [","]
field_init  := IDENT ":" expr
field_access:= primary "." IDENT

assign      := place [assign_op or] | ...
place       := IDENT {"." IDENT}
```

- 構築はフィールドを**全部**書く。省略も既定値も無い（[§4.8](#48-struct--確定m7)）
- 代入の左辺はフィールドまで辿れる（`o.inner.a = 1;`）。**辿れるのは束縛から始まる名前の連なりだけ** —
  式の結果のフィールドは書けない。メソッド呼び出し（`p.bump()`）は M7-9
- **`if` / `while` の条件と `as` / `at` / `for` のセレクタには構造体リテラルを書けない。**
  `if p { .. }` の `{` がブロックなのかリテラルなのか決まらないため。Rust と同じ制限で、
  括弧の中でなら書ける

### 3.11 `enum` — 確定（M7）

```
item        += enum_item
enum_item   := "enum" IDENT "{" [variants] "}"
variants    := variant {"," variant} [","]
variant     := IDENT ["{" [field_defs] "}"]

primary     += variant_lit
variant_lit := IDENT "::" IDENT ["{" [field_inits] "}"]
```

- **バリアントのフィールドには名前を付ける。** タプル型バリアント（`Chasing(i32)`）は無い —
  compound のキーは名前であって位置ではなく、`_0` のような綴りを発明しても読めるものにならない
- 構築は `State::Idle` と `State::Chasing { target: 3 }`。フィールドの規則は `struct` と同じ

### 3.12 `match` — 確定（M7）

```
stmt        += match_stmt
match_stmt  := "match" expr "{" {match_arm} "}"
match_arm   := pattern "=>" block [","]
pattern     := IDENT "::" IDENT ["{" [binds] "}"] | "_"
binds       := IDENT {"," IDENT} [","]
```

- 腕の本体は**ブロックのみ**。式の腕は無い
- ペイロードの束縛はフィールド名そのまま（`State::Chasing { target }`）。名前の付け替えは無い
- **網羅していなければエラー。** `_` は最後にだけ置ける。全バリアントを挙げた後の `_` も
  エラー（到達しない腕を黙って受け取らない）

**`match` も `if` も式にしない。決定（M7）。** 合流点で値を作るには宛先駆動の lowering
（積み残し、M9-10）が要る。それが入るまでの間、`let mut x = 0;` と各腕での代入が
同じ意味を同じコマンド数で書ける。値を返す構文を先に入れると、テンポラリ経由の
コピーが 1 つ増えたまま固定されてしまう。

### 3.13 `Vec<T>` — 確定（M7）

```
type        := IDENT ["<" type {"," type} ">"]

primary     += list_lit | index | method_call
list_lit    := "[" [expr {"," expr} [","]] "]"
index       := primary "[" expr "]"
method_call := primary "." IDENT "(" [args] ")"
```

- **構築はリストリテラル。** `let v = [1, 2, 3];` / `let mut v: Vec<i32> = [];`
  - 空リストだけは注釈が要る。要素が無いので型を決める材料が無い（§4.2 のとおり推論はしない）
  - `Vec::new()` は入れない。`[]` と同じものを 2 通りで書けるようにしても増えるものが無い
- メソッドは `v.len()` と `v.push(x)` の 2 つ。`impl` の固有メソッドは M7-9
- `for x in v { }` は `for` 文（[§3.8](#38-実行コンテキスト--確定m5)）と同じ構文。
  対象がセレクタか `Vec` かで lowering が変わる（[§6.22](#622-for-x-in-vec--確定m7)）
- 添字は定数でも実行時の値でもよい（コストは違う。[§6.21](#621-vect--確定m7)）

### 3.14 ジェネリクス — 確定（M7）

```
fn_item     := "fn" IDENT [generics] "(" [params] ")" ["->" type] block
struct_item := "struct" IDENT [generics] "{" [field_defs] "}"
generics    := "<" IDENT {"," IDENT} [","] ">"
```

- **型引数は書かない。呼び出しの引数から決まる。** `f(v)` の `v` の型が `T` を決める。
  turbofish（`f::<i32>(v)`）は無い — 決まらない書き方をエラーにするほうが読む側が楽
- `struct` の型引数は注釈（`let p: Pair<i32> = ...`）かフィールドの値から決まる
- **単相化のみ**（要件定義 §4.2）。同じ型引数の組に対して実体は 1 つ
- const パラメータ（`<const N: i32>`）は `fix<S>` が入る M8 で足す。
  使い道が 1 つも無いうちに構文だけ増やしても検証できない
- `enum` の型引数も M8 まで無し。`Option<T>` を入れるときに一緒に決める

### 3.15 参照と `impl` — 確定（M7）

```
type        += ["&" ["mut"]] type
impl_item   := "impl" IDENT "{" {fn_item} "}"
fn_item     += "fn" IDENT [generics] "(" [self_param ["," params]] ")" ...
self_param  := ["&" ["mut"]] "self"
primary     += "&" ["mut"] expr | method_call
```

- **参照は引数にだけ書ける。** `let r = &x;` は不可 — 束縛にすると生存期間の話が始まり、
  ライフタイムを持たない（要件定義 §5）以上、そこで嘘をつくことになる
- 参照を返すこと、`struct` のフィールドに持つことも不可（要件定義 §5）
- `impl` は固有メソッドだけ。`trait` は v1 スコープ外
- レシーバは `&self` / `&mut self` / `self`。`self` は値渡し（複製）

### 3.5 未定

以降のタスクが、実装の直前に確定させる。

| 節 | 内容 | タスク |
|---|---|---|
| 3.13 | ジェネリクス / `impl` | M7 |
| 3.12 | `mod` / `use` / `pub` / `extern fn` | 完全版は M7 |

---

## 4. 意味論 — M6 と `struct` の範囲まで確定

### 4.1 名前解決

ローカル束縛はブロックスコープ。内側の `let` は外側の同名束縛を**覆い隠す**
（シャドーイング可）。未定義の名前はエラー。

### 4.2 型

M2 が持つ型は 2 つだけ。

| 型 | 値 |
|---|---|
| `i32` | 32 ビット符号付き整数 |
| `bool` | `true` / `false` |

- **推論はしない。** `let` の型は、注釈があればそれ、無ければ初期化式の型。
  それ以上の推論（後続の使用から遡って決める、といったこと）は行わない。
  推論器を持たないのは、この言語の型が最後まで少数の具体型に留まるためで、
  単一化を入れても返ってくるものが無い。
- 暗黙変換は無い。`i32` と `bool` は混ざらない。

型付け規則:

| 式 | 要求 | 結果 |
|---|---|---|
| `INT` | — | `i32` |
| `true` / `false` | — | `bool` |
| `-a` | `a: i32` | `i32` |
| `!a` | `a: bool` | `bool` |
| `a + b` `-` `*` `/` `%` | 両辺 `i32` | `i32` |
| `a < b` `<=` `>` `>=` | 両辺 `i32` | `bool` |
| `a == b` `!=` | 両辺が同じ型 | `bool` |
| `a && b` <code>\|\|</code> | 両辺 `bool` | `bool` |
| `a = b`、`a += b` 等 | `a` は `mut` な束縛、両辺同型（複合代入は `i32`） | 値を返さない |

### 4.3 代入

代入は**文**である。`let x = (y = 1);` は書けない。

Rust では代入式は `()` を返すが、minewell に `()` 型は無い。値を返さない構文を
式の位置に置けないようにするほうが、無い型を 1 つ導入するより小さい。

### 4.4 制御フロー

- `if` / `while` の条件は `bool`。
- `break` / `continue` は最も内側のループに作用する。ループの外ではエラー。
- ブロックはスコープを作る。ループ本体の `let` は反復ごとに新しい束縛……ではなく、
  **同じフェイクプレイヤーを再利用する**。反復間で値が残るが、`let` が必ず初期化するので
  観測できない。ここを分けるとループごとに未使用のレジスタが増えるだけで、得るものが無い。

### 4.5 関数

- 引数は値渡し。呼び出し側で評価してから呼ぶ。
- 呼び出せるのは同じソースに定義された関数だけ（`mod` は M7）。
- 未定義の関数を呼ぶとエラー。引数の個数と型が合わないとエラー。
- 再帰は許す。相互再帰も許す。

### 4.6 実行コンテキスト

型 `Selector` を追加する。**コンパイル時にしか存在しない型**で、実行時の表現を持たない。

- セレクタリテラルと `for` の束縛が `Selector`。`let s = @e[type=zombie];` で別名を付けられる
- `Selector` に対してできるのは、`as` / `at` / `for` に渡すことだけ。
  算術・比較・関数の引数・戻り値に使うとエラー

#### 要求と提供

関数が要求するコンテキストは、**その関数が `#[ctx]` で宣言したものがすべて**。
推論しない。

> なぜ宣言か — 推論にすると、`h` が `@s` を使ったせいで `f` の呼び出しが落ちる、
> という「原因から遠いエラー」が出る。宣言なら、足りないことは常に 1 段で分かる。
> `raw!` の中身は見えないので、どのみち著者の申告が要る。

ブロックが提供するもの:

| 構文 | 提供 |
|---|---|
| 関数の本体 | その関数の `#[ctx]` |
| `as <sel> { }` | `entity` |
| `for e in <sel> { }` | `entity` |
| `at <sel> { }` | `position` |

検査:

- 関数を呼ぶとき、呼び出し先の要求がその場所で提供されていなければエラー。
  診断は**不足している種別**と、**呼び出し先がどこでそれを宣言したか**を示し、
  `as` で囲むか `#[ctx]` を足すかを提案する
- `as @s` / `at @s` は `entity` を要求する
- **`#[tick]` / `#[load]` の関数は `#[ctx]` を宣言できない。** function タグは
  実行者なしで呼ばれるので、宣言した時点で実行時に黙って何もしないことが確定する。
  これはバニラでは決して検出できない

### 4.7 コマンド

型 `ResourceLocation` と `Pos` を追加する。`Selector` と同じく**コンパイル時にしか
存在しない型**で、実行時の表現を持たない。

コマンド呼び出しの引数は**すべてコンパイル時の値**でなければならない。

> なぜ — コマンドは文字列であり、実行時の値を埋め込むにはマクロ関数への昇格が要る
> （要件定義 §10.1）。その仕組みは M9 で入る。それまで、実行時の値を渡そうとしたら
> 「マクロで包む必要がある、まだ実装されていない」と言って止まる。黙って動かないより
> 止まるほうがいい。

`RawArg` は、brigadier の引数型のうち minewell に対応物が無いものを受ける型。
文字列リテラルだけを受け付け、中身は検査しない。

### 4.8 `struct` — 確定（M7）

`struct` は storage 上の NBT compound（要件定義 §4.2）。型は `Struct(<定義>)` を追加する。

| 式 | 要求 | 結果 |
|---|---|---|
| `S { f: e, .. }` | `S` は `struct`、フィールドが過不足なく、それぞれ同型 | `S` |
| `e.f` | `e` は `struct` の束縛（またはそのフィールド）、`f` はそのフィールド | `f` の型 |
| `e.f = v` | 束縛が `mut`、`v` は `f` と同型 | 値を返さない |

- **フィールドは全部初期化する。** 足りない・知らない・同じ名前が 2 度、いずれもエラー。
  NBT は欠けたフィールドを黙って無視するので、省略を許した時点で
  「書いたつもりの値が無い」が実行時まで分からなくなる
- フィールドの型は `i32` / `bool` / 他の `struct`。既定の NBT タグは `i32`→`Int`、
  `bool`→`Byte`（要件定義 §4.2）

**フィールドの NBT 表現は `#[nbt(...)]` で変えられる。**

| 書き方 | 意味 |
|---|---|
| `#[nbt(byte)]` / `short` / `int` / `long` | タグ型 |
| `#[nbt(rename = "Health")]` | NBT 上のキー名。バニラの NBT は PascalCase が多い |

- `i32` に付けられるのは整数タグだけ。範囲外の値の畳み込みは
  バニラの `execute store` に従う（切り捨てて wrap する）
- **`bool` は `Byte` 固定。** バニラの真偽値が Byte なので、他のタグを選ぶ意味が無い
- `struct` のフィールドは compound なので、タグを指定できない
- rename 後のキーが他のフィールドと衝突したらエラー
- `float` / `double` / `string` は、対応する型（`f32` / `f64` / `String`）が入る M8 で足す
- `#[nbt(optional)]` は `Option<T>` を必要とするので `enum` と `match` の後
- **自分を含む `struct` はエラー。** 有限の値を構築できない
- **`==` / `!=` で比較できない。** 実行時の compound どうしを比べる手段がバニラに無い
- **戻り値にできない。** バニラの関数の戻り値は整数 1 つで、compound を返す場所が無い
- 引数にはできる。呼び出し側の storage から呼び出し先の storage への 1 コマンドの複製

### 4.9 `enum` — 確定（M7）

`enum` は storage 上の**タグ付き union**。要件定義 §4.2 のとおり `{tag:"Idle"}` の形で、
バリアントのフィールドは同じ compound に並ぶ（`{tag:"Chasing",target:3}`）。

| 式 | 要求 | 結果 |
|---|---|---|
| `E::V` / `E::V { f: e }` | `E` は `enum`、`V` はそのバリアント、フィールドが過不足なく同型 | `E` |

- `tag` という名前のフィールドは書けない。タグの置き場所と衝突する
- **中身を読むには `match`。** フィールドアクセスはできない — どのバリアントかは
  実行時にしか分からず、`s.target` が存在するかどうかを静的に言えない
- 比較・戻り値・算術は `struct` と同じ扱い（[§4.8](#48-struct--確定m7)）。
  引数渡しと複製は 1 コマンドでできる

### 4.10 `match` — 確定（M7）

- 対象は `enum` の**束縛かそのフィールド**。式の結果は `match` できない —
  compound を置く場所（storage 上のテンポラリ）を持っていないため
- 束縛（`State::Chasing { target }` の `target`）はその腕の中だけで有効。
  値はコピーで、書き換えても元の compound には戻らない
- 腕が同じバリアントを 2 度挙げたらエラー

### 4.11 `Vec<T>` — 確定（M7）

`Vec<T>` は storage 上の NBT list。要素は `T` の表現をそのまま並べる。

| 式 | 要求 | 結果 |
|---|---|---|
| `[a, b, c]` | 要素が全部同型 | `Vec<その型>` |
| `[]` | 注釈が `Vec<T>` | `Vec<T>` |
| `v[e]` | `v: Vec<T>`、`e: i32` | `T` |
| `v.len()` | `v: Vec<T>` | `i32` |
| `v.push(e)` | `v` は `mut`、`e: T` | 値を返さない |

- **NBT の list は同型でなければならない。** `Vec<Vec<T>>` も `Vec<struct>` も持てるが、
  混在した list は作れない — 型が同じであることが構文から保証されている
- `Vec` は `struct` / `enum` と同じ storage 常駐型。比較・戻り値・算術は同じく不可
- **範囲外の添字は何も起きない。** バニラの `data` がそう振る舞う。実行時の添字を
  静的に検査する手段は無く、検査コマンドを毎回吐くのは §0 の原則 3 に反する。
  debug ビルドでの検査は将来の選択肢として残す

### 4.12 ジェネリクス — 確定（M7）

- 型パラメータは**その関数・その `struct` の中だけ**で名前として通る
- 呼び出しでは、引数の型を宣言された型と**構造的に突き合わせて**型引数を決める。
  `fn f<T>(v: Vec<T>)` に `Vec<i32>` を渡せば `T` は `i32`。
  決まらない（引数に現れない型パラメータがある）ときはエラー
- 同じ型引数の組に対して実体は 1 つ。**2 度目の呼び出しは 1 度目と同じ関数を呼ぶ**
- 実体は元の関数と同じ規則で検査される。型パラメータに制約は無く、`T` に対して
  できることは「渡す・複製する・比較する」だけ — 演算は具体型の上でしか書けない

### 4.13 参照 — 確定（M7）

**`&T` / `&mut T` はコンパイル時のみの概念**（要件定義 §5）。実行時表現を持たない。

| 式 | 可否 |
|---|---|
| `f(&mut p)` / `f(&p)` | OK |
| `f(&mut p.inner)` | OK |
| `&v[0]`（定数添字） | OK |
| `p.bump()`（`&mut self`） | OK |
| `&mut v[i]`（実行時添字） | **エラー**。パスがコンパイル時に決まらない |
| `let r = &p;` | **エラー**。参照は引数にだけ書ける |
| 参照を返す / フィールドに持つ | **エラー** |

- 借用先は**静的にパスが決まる場所**でなければならない。`&mut v[i]` が不可なのは
  マクロ関数への昇格が必要になり、「参照は無料」という前提（原則 1）が崩れるため。
  代わりに `v[i] = x` を書く（要件定義 §4.4 のとおり、代入は参照の取得ではない）
- `&` 越しの書き込みはエラー。`&mut` を要求する
- **借用チェッカは持たない。** 同じ場所を 2 つの引数で借りても検出しない（要件定義 §5）

## 5. 型の表現 — M6 と `struct` の範囲まで確定

| 型 | 置き場所 | 表現 |
|---|---|---|
| `i32` | scoreboard | そのまま |
| `bool` | scoreboard | `0` または `1` |
| `struct` | storage | NBT compound（[§6.18](#618-struct-の配置と構築--確定m7)） |
| `enum` | storage | `tag` を持つ NBT compound |
| `Vec<T>` | storage | NBT list |
| `Selector` / `ResourceLocation` / `Pos` | どこにも置かない | コンパイル時のみ |

置き場所は **score / storage / コンパイル時のみ** の 3 分類になる。2 分類（実行時か否か）
では `struct` が入らない — 実行時の型でありながらレジスタに乗らないため。

`bool` を scoreboard の 0/1 で持つのは、`execute store success` が 0/1 を書き、
`execute if score ... matches 1` が読めるため。バニラの真偽値の扱いがそもそもこれ。

`enum` / `Vec<T>` / `String` は M7 の後続タスク、`fix<S>` と NBT 相互運用の数値型は M8。

## 6. lowering — M6 と `struct` の範囲まで確定

各構文から mcfunction への写像。生成コマンド数は `tinymcf` の計測 API で検証する
（[`../crates/tinymcf/SPEC.md`](../crates/tinymcf/SPEC.md) §5）。

### 6.1 名前

| 対象 | 名前 |
|---|---|
| ローカル束縛 | フェイクプレイヤー `$<関数名>.<束縛名>`、objective `<ns>.v` |
| テンポラリ | フェイクプレイヤー `$t<n>`、objective `<ns>.t` |

`n` は**プログラム全体で単調増加**する。同じ名前のテンポラリが二度と現れないので、
生存期間を考えずに正しい。縮めるのは M9-7 の生存解析の仕事であって、
それまでは正しさを優先する（要件定義 §15）。

ローカルを関数名で修飾するのは、M4 で関数呼び出しが入ったときに
別々の関数の同名ローカルが踏み合わないようにするため。

### 6.2 式

式は左から右へ評価し、結果をレジスタに置く。

| 式 | コマンド |
|---|---|
| `5` | `scoreboard players set $t0 <ns>.t 5` |
| `x`（ローカル） | レジスタを増やさず、束縛のフェイクプレイヤーをそのまま使う |
| `a + b` | `... operation $t = $a` / `... operation $t += $b` |
| `-a` | `... set $t 0` / `... operation $t -= $a` |
| `a < b` | `execute store success score $t <ns>.t if score $a ... < $b ...` |
| `a == b` | 同上、`= ` 比較 |
| `a != b` | `execute store success score $t ... unless score $a ... = $b ...` |
| `!a` | `execute store success score $t ... if score $a ... matches 0` |

`/` と `%` は scoreboard の floor 除算・floor 剰余になる。Rust の切り捨て除算とは
**負数で結果が異なる**（`-7 / 2` は Rust で `-3`、minewell で `-4`）。
バニラに合わせるほうを選んだのは、生成コマンドが 1 つで済むからで、
Rust に合わせるには符号を見て補正するコマンドを毎回吐くことになる。
この差は言語仕様として明記し、診断では触れない（正しい挙動なので）。

### 6.3 短絡評価

`a && b` は `min(a, b)`、`a || b` は `max(a, b)` に落とす
（`scoreboard players operation` の `<` と `>`）。

**これは M2 の式が副作用を持たないから正しい。** 短絡評価と非短絡評価が観測上
区別できるのは右辺に副作用があるときだけで、M2 の式は関数呼び出しを含まない。
1 コマンドで済むほうを取る。

**M4 で関数呼び出しが入った時点でこの規則は見直す。** そのとき右辺が呼び出しを
含みうる `&&` / `||` は分岐に落とす必要がある。ここに書いてあるのはそのための覚書でもある。

### 6.4 代入

| 文 | コマンド |
|---|---|
| `let x = <expr>;` | `<expr>` の結果を `$<fn>.x` へ `operation =`。定数なら `set` 1 つ |
| `x = <expr>;` | 同上 |
| `x += <expr>;` | `<expr>` を評価して `operation +=`。右辺が定数なら `players add` 1 つ |

定数を特別扱いするのは最適化ではなく、`scoreboard players set` と
`players add` がそのために存在するコマンドだから。1 つで書けるものを 2 つで書かない。

同じ理由で、比較と `!` の結果は**テンポラリを経由せず直接束縛に書き込む** —
`execute store success score <dst> ... if ...` は書き込み先を自分で持っているので、
テンポラリに入れてからコピーすると 1 コマンドで済む仕事を 2 コマンドでやることになる。
`dst` が被演算子を兼ねていても安全（条件の評価が store より先）。

### 6.5 objective の作成

生成関数 `<ns>:__init` が `<ns>.v` と `<ns>.t` を作り、`minecraft:load` タグに載る。

利用者に任せない理由 — objective が無いと `scoreboard` コマンドはバニラに**拒否される**。
コンパイラのバグと区別がつかない形で全部が動かなくなるので、これは選択肢ではない。

`#[tick]` / `#[load]` を付けた関数もそれぞれのタグに載る。

---

### 6.6 制御フローの表現

**MIR にジャンプ命令は無い。** ターゲットにジャンプが無いため、基本ブロックを辺で
繋いだ CFG を持つ意味が無い。制御フローは 2 つだけで表す:

- **生成関数** — 切り出したブロックは 1 つの mcfunction になる
- **ガード付き命令** — `execute <条件> run <コマンド>`

「1 命令 = 1 コマンド」は保たれる。ガード付き命令も 1 行だから。

### 6.7 条件

条件式は可能なら `execute if` に**直接埋め込む**。

| 条件式 | 生成 |
|---|---|
| `a < b`（両方レジスタ） | `execute if score $a ... < $b ... run ...` |
| `a < 5` | `execute if score $a ... matches ..4 run ...` |
| `!c` | `execute if score $c ... matches 0 run ...` |
| `flag`（`bool` の束縛） | `execute if score $flag ... matches 1 run ...` |
| それ以外 | レジスタに評価してから `matches 1` |

比較をいったんレジスタに書いてから `matches 1` で読み直すと 2 コマンドかかる。
`execute if score` が比較を直接書けるので、1 コマンドで済む。

### 6.8 `if` / `else`

**単文で、制御フローを含まないブロックはインライン展開する。**

```
if x > 0 { raw!("say hi"); }
→  execute if score $x ... matches 1.. run say hi        (1 コマンド)
```

そうでない場合は関数に切り出す:

```
if c { A } else { B }
→  execute if <cond> run function <親>/if_0
   execute unless <cond> run function <親>/else_0
```

`else` を `unless` で書けるのは、条件が同じ式だから。条件が
レジスタ経由のときはそのレジスタを 2 回読む。

`#[inline]` / `#[no_inline]` を文に付けると判定を上書きできる。

### 6.9 ループ

`while c { B }` は**自己末尾再帰する 1 つの関数**になる。

```
→  function <親>/while_0

<親>/while_0:
   execute unless <cond> run return 0
   <B をインライン展開>
   function <親>/while_0
```

`loop { B }` は条件ガードが無いだけで同じ。

### 6.10 `break` / `continue` / `return`

生成関数から抜ける手段は `return` しか無く、`return` は呼び出し元まで**伝播しない**。
そこで**制御レジスタ** `$<fn>.ctl` を 1 本使う。

| 値 | 意味 |
|---|---|
| 0 | 通常 |
| 1 | `break` |
| 2 | `continue` |
| 3 | `return` |

```
break     →  scoreboard players set $<fn>.ctl <ns>.v 1
             return 0
continue  →  ... 2 / return 0
return    →  ... 3 / return 0
```

抜けうるブロックを呼んだ直後には、伝播のガードが 1 つ付く:

```
execute if score $<fn>.ctl <ns>.v matches 1.. run return 0
```

ループがそれを消費する:

- **`continue` を含む本体は別関数に切り出す。** 本体をインラインにしたままだと
  `continue` の `return` がループ関数ごと終わらせてしまい、次の反復に行けない。
  切り出したうえで、呼び出し直後に `matches 2` を 0 に戻す
- ループの呼び出し元は、戻った直後に `matches 1`（`break`）を 0 に戻す
- `matches 3`（`return`）はどこでも消費されず、関数の先頭まで伝播する

**この仕組みは必要なときにしか出てこない。** ブロックから制御が抜けないなら
`$ctl` は 1 度も現れない。抜けうるかどうかはブロックごとに静的に分かる。

制御レジスタを使う関数は、**入口で 0 に戻す**。関数の最上位まで届いた `return` は
レジスタを 3 のまま残して抜けるので、次の呼び出しがそれを自分のものと読んでしまう。
`#[tick]` の関数なら、一度早期 return しただけで以降ずっと何もしなくなる。
消せるのは値がもう意味を持たない入口だけなので、そこで消す。

### 6.11 生成関数の命名

親のサブディレクトリに置く（要件定義 §12.2）。

| 構文 | 名前 |
|---|---|
| `if` の then | `<親>/if_<n>` |
| `else` | `<親>/else_<n>` |
| `while` / `loop` | `<親>/while_<n>` / `<親>/loop_<n>` |
| `continue` を含むループ本体 | `<親>/while_<n>/body` |

`<n>` は親の中で 0 から数える。`__gen/` に平置きしないのは、生成物を目で追えることが
要件（要件定義 §12.2）だから。

---

### 6.12 呼び出し規約

引数は**呼び出し先のローカルと同じフェイクプレイヤー**に書く。引数は初期値を
呼び出し側が入れるローカルにすぎない。

```
f(1, x)
→  scoreboard players set $f.a <ns>.v 1
   scoreboard players operation $f.b <ns>.v = $main.x <ns>.v
   function <ns>:f
```

戻り値はバニラの関数戻り値をそのまま使う。

```
return 5;        →  return 5
return <式>;     →  return run scoreboard players get $t0 <ns>.t
let y = f();     →  execute store result score $main.y <ns>.v run function <ns>:f
```

`return <定数>` が 1 コマンドで済むのは、`return` が整数リテラルを取るから。
式の場合は `return run` で値を取り出すコマンドを走らせる。

**非再帰の呼び出しに追加コストは無い。** 引数の書き込みと `function` だけで、
退避も復帰も出てこない。

### 6.13 再帰

呼び出しグラフの強連結成分を Tarjan で求める。**同じ成分内への呼び出しだけ**が
再帰呼び出しで、そこだけがフレームの退避を払う。

呼び出し側で退避する:

```
1. 現在の関数が使うレジスタを storage のスタックに push
2. 引数を書く
3. 呼ぶ（戻り値は新しいテンポラリへ）
4. スタックから pop して復帰（戻り値のテンポラリは除く）
```

呼び出し側で退避するのは、呼び出し先が引数を受け取る**前**に退避しないと、
呼び出し側の値がもう上書きされているため。呼び出し先の入口では手遅れになる。

退避する範囲は、**その関数（と切り出した生成関数）が書き込むレジスタ全部**。
生存解析で絞るのは M9-7 の仕事で、それまでは正しさを優先する。

スタックは `<ns>:mw` の `mw.stack`、1 フレームが 1 つの compound。

```
data modify storage <ns>:mw mw.stack append value {}
execute store result storage <ns>:mw mw.stack[-1].<名前> int 1 run scoreboard players get <reg>
...
（呼び出し）
execute store result score <reg> ... run data get storage <ns>:mw mw.stack[-1].<名前>
...
data remove storage <ns>:mw mw.stack[-1]
```

### 6.14 短絡評価（§6.3 の見直し）

M2 では `&&` / `||` を `min` / `max` 1 コマンドに落とした。式が純粋で、短絡と
非短絡が観測上区別できなかったため。**関数呼び出しが入ったので、その前提は
右辺が呼び出しを含むときに崩れる。**

そこで右辺を見て分ける:

| 右辺 | 生成 |
|---|---|
| 純粋（呼び出しを含まない） | `min` / `max` 1 コマンド |
| 呼び出しを含みうる | 分岐して本当に短絡させる |

```
a && b（b が非純粋）
→  <a を dst へ>
   execute if score $dst ... matches 1 run function <親>/and_<n>
      # and_<n>: b を dst へ

a || b
→  execute if score $dst ... matches 0 run function <親>/or_<n>
```

純粋性はコンパイル時に分かる。**呼び出しを書かない大多数の `&&` は 1 コマンドのまま。**


### 6.15 実行コンテキスト

```
as <sel> { B }      →  execute as <sel> run function <親>/as_<n>
at <sel> { B }      →  execute at <sel> run function <親>/at_<n>
for e in <sel> { B }→  execute as <sel> run function <親>/for_<n>
```

`if` と同じく、単文で制御が抜けないブロックはインラインにする
（`execute as <sel> run <コマンド>`）。

`for` の束縛はコンパイル時の別名で、本体の中では `@s` を意味する。実行時表現は無い。

#### 反復と制御フロー

本体は**エンティティ 1 体につき 1 回**呼ばれる。`return` で本体を抜けても、
残りのエンティティの番は来る。そこで:

| 文 | 生成 |
|---|---|
| `continue` | `return 0` だけ。本体から戻ること自体が「次のエンティティへ」 |
| `break` | 制御レジスタに 1 を立てて `return 0` |
| `return` | 制御レジスタに 3 を立てて `return 0` |

`break` と `return` があるときだけ、本体の**先頭**にガードが 1 つ付く:

```
execute if score $<fn>.ctl <ns>.v matches 1.. run return 0
```

以降のエンティティは即座に戻る。`execute as` の行を止める手段がバニラに無いので、
「残りは走るが何もしない」に落とす。**反復回数分のコマンドは消えない** —
コスト表に出るので、`break` を書けば安くなるという誤解は生まれない。

`continue` が `break` と違う扱いになるのは `for` / `as` の中だけで、
`while` / `loop` の中では §6.10 のまま。lowering は最も内側のループの種類を見て決める。


### 6.16 コマンド呼び出し

toolchain の `commands.json`（brigadier のコマンドツリー）から、実行可能な葉ごとに
1 つの関数シグネチャを作る。

**名前は literal 経路を `snake_case` で繋いだもの。**

```
/data get entity <target> <path>   →  data_get_entity(target, path)
/setblock <pos> <block>            →  setblock(pos, block)
```

引数型は brigadier の parser 名から引く（要件定義 §1.4 の対応表）。**知らない parser
型は `RawArg` にフォールバックして警告する** — エラーにするとスナップショットで
引数型が 1 つ増えただけで toolchain 全体が生成できなくなる。

`overrides.toml` で名前とシグネチャを上書きできる。対象は頻出コマンドだけで、
生成物の 9 割以上は機械生成のまま。

呼び出しは 1 コマンドになる。

```rust
setblock(pos!(~ ~1 ~), minecraft:stone)
→  setblock ~ ~1 ~ minecraft:stone
```

### 6.17 toolchain

`minewell.toml` の `toolchain = "1.21.4"` が `~/.minewell/toolchains/1.21.4/` を指す。

```
~/.minewell/toolchains/1.21.4/
    toolchain.json     pack_format など
    commands.json      brigadier のコマンドツリー
    registries.json    ブロック ID・アイテム ID など
```

**`toolchain` を書かないこともできる。** その場合 `pack_format` は暫定値になり、
コマンド呼び出しは「toolchain が設定されていない」というエラーになる。`raw!` は使える。

toolchain 無しでも動くようにするのは、コンパイラにコマンド表を埋め込まないため。
埋め込めば「版非依存のコンパイラ」（要件定義 §1.2）が嘘になる。

---

### 6.18 `struct` の配置と構築 — 確定（M7）

| 対象 | 場所 |
|---|---|
| `struct` のローカル束縛 | `<ns>:mw` の `mw.vars.<関数名>.<束縛名>` |
| そのフィールド | 上に `.<NBT キー>` を継ぎ足す。ネストも同じ |

scoreboard 側の `$<関数名>.<束縛名>`（[§6.1](#61-名前)）と同じく関数名で修飾する。理由も同じで、
別々の関数の同名ローカルが踏み合わないようにするため。

構築は**コンパイル時に分かる部分を 1 コマンドで置き**、残りだけを後から書く。

```
let p = Point { x: 1, y: true };
→  data modify storage <ns>:mw mw.vars.main.p set value {x:1,y:1b}

let q = Point { x: n, y: true };     // n は実行時の値
→  data modify storage <ns>:mw mw.vars.main.q set value {x:0,y:1b}
   execute store result storage <ns>:mw mw.vars.main.q.x int 1 \
       run scoreboard players get $main.n <ns>.v
```

実行時の値のフィールドも `set value` に**プレースホルダとして書く。** キーの無い compound を
作ってから書き足してもコマンド数は変わらず、書き損じたときに静かに欠けるだけになる。

束縛どうしの複製と引数渡しは 1 コマンド。

```
let q = p;   →  data modify storage <ns>:mw mw.vars.main.q set from storage <ns>:mw mw.vars.main.p
f(p)         →  data modify storage <ns>:mw mw.vars.f.p    set from storage <ns>:mw mw.vars.main.p
```

フィールドの読み書きも 1 コマンド。パスは束縛のパスに `.<フィールド名>` を継ぎ足しただけで、
ネストしても同じ規則が続く。

```
let a = o.inner.a;
→  execute store result score $main.a <ns>.v run data get storage <ns>:mw mw.vars.main.o.inner.a

o.inner.a = 3;
→  data modify storage <ns>:mw mw.vars.main.o.inner.a set value 3

o.inner.a = n;
→  execute store result storage <ns>:mw mw.vars.main.o.inner.a int 1 \
       run scoreboard players get $main.n <ns>.v

o.b = i;              // struct のフィールドどうし
→  data modify storage <ns>:mw mw.vars.main.o.b set from storage <ns>:mw mw.vars.main.i
```

**複合代入（`o.a += 1`）だけは 3 命令。** score に読み出し、演算し、書き戻す。
storage の値に対する算術がバニラに無いため、これは削れない。読み出しと書き戻しは
どちらも `execute … run` なので、実際に走るのは 5 コマンド
（[`../crates/tinymcf/SPEC.md`](../crates/tinymcf/SPEC.md) §5）。

読み出し先が束縛やフィールドなら**テンポラリを経由しない** — `execute store result` は
書き込み先を自分で持っているため（[§6.4](#64-代入) と同じ理由）。

---

### 6.19 `enum` の構築 — 確定（M7）

置き場所は `struct` と同じ（[§6.18](#618-struct-の配置と構築--確定m7)）。構築も同じく、
分かっている部分を 1 コマンドで置く。

```
let s = State::Idle;
→  data modify storage <ns>:mw mw.vars.main.s set value {tag:"Idle"}

let s = State::Chasing { target: n };
→  data modify storage <ns>:mw mw.vars.main.s set value {tag:"Chasing",target:0}
   execute store result storage <ns>:mw mw.vars.main.s.target int 1 \
       run scoreboard players get $main.n <ns>.v
```

**バリアントを変えるときも 1 コマンドで書き換わる。** `set value` は compound を丸ごと
置き換えるので、前のバリアントのフィールドが残ることはない。

---

### 6.20 `match` — 確定（M7）

腕は**それぞれ独立したガード**になる。ただし判定するのは**評価対象の控え**で、元の値ではない。

```
match s {
    State::Idle => { .. }
    State::Chasing { target } => { .. }
}
→  data modify storage <ns>:mw mw.tmp.m0 set from storage <ns>:mw mw.vars.main.s
   execute if data storage <ns>:mw mw.tmp.m0{tag:"Idle"} run function <親>/match_0/idle
   execute if data storage <ns>:mw mw.tmp.m0{tag:"Chasing"} run function <親>/match_0/chasing

<親>/match_0/chasing:
   execute store result score $main.target <ns>.v \
       run data get storage <ns>:mw mw.vars.main.s.target
   ...
```

- **控えを取るのは 1 コマンド、そして削れない。** ガードは順に評価されるので、腕が
  評価対象を書き換えると（状態機械はまさにそれをする）後続の腕が新しいタグに一致して
  もう一度走ってしまう。控えを見ていれば、走る腕はつねに 1 つ
- ペイロードの束縛は**元のパス**から読む。走る腕は 1 つなので、その時点の値は入ってきた値
- 生成関数は `<親>/match_<n>/<バリアント名を小文字にしたもの>`。データパックのパスは
  小文字しか受け付けないため。小文字にすると衝突するバリアントの組はエラーにする
- `_` の腕は `<親>/match_<n>/other`。ガードは挙がっているタグを全部 `unless` で並べた
  **1 コマンド**
- 腕から `break` / `continue` / `return` が出るときは `if` と同じ伝播ガードが付く
  （[§6.10](#610-break--continue--return)）

---

### 6.21 `Vec<T>` — 確定（M7）

置き場所は `struct` と同じ。要素は NBT パスの添字で指す。

| 式 | コマンド |
|---|---|
| `let v = [1, 2];` | `data modify storage <ns>:mw <path> set value [1,2]` |
| `v.len()` | `execute store result score <dst> <ns>.t run data get storage <ns>:mw <path>` |
| `v.push(3)` | `data modify storage <ns>:mw <path> append value 3` |
| `v.push(x)` | `append value 0` してから `execute store result storage <ns>:mw <path>[-1] int 1 run …` |
| `v[0]`（定数） | パスに `[0]` を継ぎ足すだけ。読み書きとも 1 命令 |

**実行時の添字だけがマクロ関数になる**（要件定義 §10.1）。パスの一部が実行時にしか
決まらないので、文字列としてのコマンドを組み立てる手段がこれしか無い。

```
let x = v[i];
→  execute store result storage <ns>:mw mw.args.i int 1 run scoreboard players get $main.i <ns>.v
   execute store result score $t0 <ns>.t \
       run function <親>/index_0 with storage <ns>:mw mw.args

<親>/index_0:
   $return run data get storage <ns>:mw mw.vars.main.v[$(i)]
```

```
v[i] = x;
→  execute store result storage <ns>:mw mw.args.i int 1 run scoreboard players get $main.i <ns>.v
   function <親>/index_1 with storage <ns>:mw mw.args

<親>/index_1:
   $execute store result storage <ns>:mw mw.vars.main.v[$(i)] int 1 \
       run scoreboard players get $main.x <ns>.v
```

- **マクロ関数に渡すのは添字だけ。** 値のほうは scoreboard に載っていて、
  フェイクプレイヤー名はコンパイル時に決まっているので、マクロ側から直接読める
- **マクロ性は呼び出し側に伝染しない**（要件定義 §10.1）。`$` の行は生成した補助関数の
  中だけにあり、`#[tick]` の関数がマクロ関数になることはない
- 実行時の添字は**最後の段でだけ**使える。`v[i].field` のように後ろが続く形はエラーにする —
  マクロ 1 回では書けず、テンポラリを増やして隠すより、束縛に取り出させるほうが読める

---

### 6.22 `for x in vec` — 確定（M7）

要件定義 §7.1 のとおり**破壊的反復**。コピーを作り、`[0]` を読んでは消す。

```
for x in v { .. }
→  data modify storage <ns>:mw mw.iter.i0 set from storage <ns>:mw mw.vars.main.v
   function <親>/for_0

<親>/for_0:
   execute unless data storage <ns>:mw mw.iter.i0[0] run return 0
   execute store result score $main.x <ns>.v run data get storage <ns>:mw mw.iter.i0[0]
   data remove storage <ns>:mw mw.iter.i0[0]
   .. 本体 ..
   function <親>/for_0
```

- **マクロを使わない。** 添字はつねに `[0]` なので、パスはコンパイル時に決まる。
  要素数に関わらず生成コマンド数は一定
- コピーを取るので**元の `Vec` は変わらない**。`for x in &mut vec` は v1 では持たない
  （破壊的反復と両立しない。要件定義 §7.1）
- 反復ごとのコストは、空判定 1 + 取り出し 2 + 削除 1 + 本体 + 末尾呼び出し 1
- コピー置き場は `mw.iter.i<n>`。`break` / `continue` / `return` は `while` と同じ
  （[§6.9](#69-ループ)・[§6.10](#610-break--continue--return)）。再帰時は退避対象に入る

---

### 6.23 単相化 — 確定（M7）

実体はふつうの関数として出力する。名前は元の名前に型引数を繋いだもの。

```
fn hold<T>(x: T) -> i32 { .. }
hold(1); hold(true); hold(1);
→  turret:hold_i32   （2 回の `hold(1)` は同じ関数）
   turret:hold_bool
```

- 型名の綴りは小文字（データパックのパスの制約）。`Vec<i32>` は `vec_i32`
- 実体は要求された順に作られ、実体の中からの要求も同じ列に積まれる。
  同じ組み合わせを 2 度作らないのは表で保証する
- **テンプレート自体は出力しない。** 型パラメータのままでは置き場所が決まらない
- 呼び出しグラフの SCC 解析（[§6.13](#613-再帰)）は実体に対して回る。
  再帰するジェネリック関数は、同じ型引数のときだけ同じ成分に入る

---

### 6.24 参照の単相化 — 確定（M7）

参照は**呼び出し箇所ごとに単相化**し、借用先のパスを本体へ直接展開する
（要件定義 §5）。コピーもマクロも起きない。

```
fn bump(p: &mut Point) { p.x += 1; }
fn main() { let mut a = Point { x: 0 }; bump(&mut a); }

→  main:
     data modify storage <ns>:mw mw.vars.main.a set value {x:0}
     function <ns>:bump_main_a

   bump_main_a:
     execute store result score $t0 <ns>.t run data get storage <ns>:mw mw.vars.main.a.x
     scoreboard players add $t0 <ns>.t 1
     execute store result storage <ns>:mw mw.vars.main.a.x int 1 \
         run scoreboard players get $t0 <ns>.t
```

- **引数の書き込みが 1 つも出ない。** 借用は名前の付け替えであって値の移動ではない
- 実体の名前は `<関数名>_<借用元の関数名>_<束縛名>`。同じ場所を借りる 2 つの呼び出しは
  同じ実体を共有する
- 借用したスカラがフェイクプレイヤーに載っている（`&mut i32` に束縛を渡した）場合は、
  そのフェイクプレイヤーをそのまま読み書きする
- メソッド呼び出し（`p.bump()`）は `bump(&mut p)` と同じものに落ちる
