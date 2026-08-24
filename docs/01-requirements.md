# minewell-lang 要件定義

Rust-like な構文を持ち、Minecraft Java Edition のデータパック（`.mcfunction`）へ
トランスパイルする言語 **minewell**（拡張子 `.mwl`）の要件定義。

- 実装言語: Rust
- 開発手法: 仕様書駆動 + TDD
- 本書のステータス: 要件確定。詳細仕様（文法 EBNF、lowering 規則、型規則）は別書。

---

## 0. 設計原則

本書のすべての決定は、次の 3 原則から導出されている。

1. **静的に決まるものは無料にする。**
   セレクタ、参照、座標、`raw!` の定数補間 — コンパイル時に確定する値は
   実行時表現を持たず、コマンドを 1 つも消費しない。
2. **バニラで検出不能なバグを、コンパイル時に潰す。**
   mcfunction の最大の問題は「失敗しても黙る」こと。`@s` の不在、ID の typo、
   固定小数点のスケール不一致は、すべて静的に検出できる。ここが minewell の存在意義。
3. **生成コマンド数は機能ではなく性能要件。**
   tick あたりの実行コマンド数がそのまま TPS ラグになる。
   出力量に無関心な設計はデータパックとして使い物にならない。

---

## 1. ターゲットとツールチェーン

### 1.1 対象

- **Minecraft Java Edition 1.21 以降。** Bedrock は対象外。
- 1.21 未満は対象外。理由: マクロ `$(x)`（1.20.2）、`return` 文（1.20.2）、
  `function ... with storage`（1.21）が揃わないと、引数と戻り値を持つ関数が実装できない。

### 1.2 バージョン管理: toolchain モデル

MC バージョンごとの差分は **toolchain** として配布する（rustup 型）。

- **コンパイラ本体（`mwlc`）はバニラコマンドを一切知らない。** 版非依存。
- toolchain の中身:
  - その版の `commands.json`（brigadier コマンドツリー）から生成した型付きコマンドシグネチャ
  - registries（ブロック ID、アイテム ID、エンティティ型など）
  - `pack_format`
  - stdlib
- 新バージョン対応 = データ再生成のみ。コンパイラのコード変更ゼロ。

### 1.3 toolchain の配布

- **事前生成方式。** 本プロジェクトの CI が `server.jar` の data generator を実行し、
  生成物を GitHub Releases へ公開する。
- `mwl toolchain install 1.21.4` は数百 KB のダウンロードのみ。**利用者に Java を要求しない。**
- 逃げ道: `mwl toolchain build-from-jar <path>`（ローカル生成。スナップショット検証用）
- プロジェクトの `minewell.toml` に `toolchain = "1.21.4"` を記述。省略時は最新安定版。

### 1.4 コマンド API の生成規則

`commands.json` は brigadier のコマンドツリー（literal ノードと argument ノードの木）。
木の各 executable 葉が 1 オーバーロードに対応する。

**機械生成を既定とし、頻出コマンドのみ手書きで命名を上書きする。**

- 既定の関数名 = ルートからの literal 経路を `snake_case` で連結
  （`/data get entity <target> <path>` → `data_get_entity(target, path)`）
- `overrides.toml` で人間が命名・シグネチャを上書き。**対象は頻出の 30 コマンド程度に限る**
  （`execute` / `data` / `scoreboard` / `tp` / `summon` / `tellraw` など）
- brigadier の parser 型 → minewell の型は固定の対応表で写す

| brigadier parser | minewell 型 |
|---|---|
| `brigadier:integer` | `i32` |
| `brigadier:bool` | `bool` |
| `brigadier:string` | `String` |
| `brigadier:double` / `float` | `f64` / `f32` |
| `minecraft:entity` / `game_profile` | `Selector` |
| `minecraft:block_pos` / `vec3` | `Pos` |
| `minecraft:resource_location` | `ResourceLocation` |
| `minecraft:nbt_compound_tag` / `nbt_tag` | `Nbt` |
| `minecraft:component` | `TextComponent` |
| 未知の parser 型 | `RawArg`（文字列）にフォールバック + 警告 |

- 未知の parser 型を**エラーにせず警告 + フォールバック**にする理由:
  スナップショットで新しい引数型が追加されても、toolchain の生成が止まらない。
  該当コマンドだけ型が緩くなり、他は通常どおり使える

全部機械生成にしない理由 — `data_modify_storage_target_path_set_from_entity` のような
名前は誰も使わない。全部手書きにしない理由 — バージョンごとの保守コストが戻ってきて
§1.2 の利点が消える。**上書きは高頻度コマンドだけで、生成物の 9 割以上は機械生成のまま。**

---

## 2. 言語の性格

**汎用命令型言語 + セレクタ第一級。**

含む: 式、`let`、関数、再帰、`struct`、`enum`、`match`、ジェネリクス（単相化）、
セレクタ・座標・NBT のドメイン型。

含まない: 所有権、借用チェッカ、ライフタイム、GC。
理由: Minecraft 側に「解放すべきリソース」が存在しないため、守るものがない。

---

## 3. メモリモデル

### 3.1 配置

物理制約により、置き場所は 2 つしかない。

| 置き場所 | 対象 | 可能な操作 |
|---|---|---|
| `scoreboard` フェイクプレイヤー | `i32` / `bool` / `fix<S>` | 算術・比較（i32 のみ） |
| `data storage` | `struct` / `enum` / `Vec<T>` / `String` / NBT 型 | 構造の保持・コピー・パス参照 |

両者の往復は `execute store` / `data modify ... from` の 1 コマンド。
コンパイラが型から判断して自動挿入する。

### 3.2 手動制御

配置は属性で上書きできる。

```rust
#[score]    // 強制的に scoreboard に置く
#[storage]  // 強制的に storage に置く
```

### 3.3 命名規約

- 内部フェイクプレイヤー: `$` プレフィクス（バニラのプレイヤー名として不正なため、実プレイヤーと衝突しない）
- objective: **`<namespace>.t`**（テンポラリ） / **`<namespace>.v`**（ユーザ変数）の 2 本のみ
- storage: **`<namespace>:mw`** の 1 本のみ。用途はルート直下のパスで分ける
  （`mw.vars` / `mw.stack` / `mw.args` / `mw.iter` / `mw.tmp`）
  - `mw.tmp` は M7 で追加。`match` は評価対象の compound を先に控えてから腕を判定する
    （仕様 §6.20）。score のテンポラリ（`$t<n>`）に相当するものが storage 側にも要る
- `namespace` は `minewell.toml` で指定。他人が作った minewell 製データパックを
  同一ワールドに入れても衝突しない。長すぎる場合は警告
- objective 登録は `#[load]` 関数に自動生成する

---

## 4. 型システム

### 4.1 数値

| 型 | 配置 | 算術 |
|---|---|---|
| `i32` | score | 可 |
| `bool` | score | 論理演算のみ |
| `fix<S>` | score | 可。`S` は const ジェネリックなスケール（例 `fix<1000>`） |
| `f32` / `f64` | storage | **不可**（NBT 相互運用専用） |
| `i8` / `i16` / `i64` | storage | **不可**（NBT 相互運用専用） |

- `fix<S>` の `+` / `-` は素の整数演算。`*` / `/` はスケール補正を自動挿入。
- **異なるスケール同士の演算はコンパイルエラー。** 明示変換を要求する。
- `i8` / `i16` / `i64` / `f32` / `f64` が必要な理由: NBT では `Byte(1)` と `Int(1)` が
  別物であり、間違えると Minecraft が黙って無視する。型で区別しないと検出できない。
- 浮動小数点のソフトウェアエミュレーションは行わない（乗算 1 回で数百コマンド）。

### 4.2 複合型

- `struct` — storage 上の NBT compound。フィールドの NBT 表現は `#[nbt(...)]` で制御する。
  - `#[nbt(byte)]` / `short` / `int` / `long` / `float` / `double` / `string` — タグ型指定
  - `#[nbt(rename = "Health")]` — フィールド名（バニラの NBT は PascalCase が多い）
  - `#[nbt(optional)]` — 欠損可。`Option<T>` として読む
  - 既定: `i32`→`Int`, `bool`→`Byte(0/1)`, `String`→`String`, `f64`→`Double`, `f32`→`Float`
  - タグ型の指定が必要な理由: NBT では `Byte(1)` と `Int(1)` が別物であり、
    間違えると Minecraft が黙って無視する
- `enum` — タグ付き union（`{tag:"Idle"}` / `{tag:"Chasing", target:"..."}`）。
  `match` は `execute if data storage ... {tag:"Idle"}` の連鎖に落ちる。
- `Vec<T>` — storage 上の NBT list。
- ジェネリクス — 型パラメータと const パラメータ。**単相化のみ。**
- `impl` ブロックと固有メソッドを持つ。

### 4.3 v1 で持たないもの

- `trait` / `dyn` — 予約語のみ確保。
  単相化しかできない以上、trait は「コンパイル時のオーバーロード解決」以上のことをせず、
  それは `impl` の固有メソッドで足りる。NBT 変換は `#[derive]` 相当の組み込み属性で解決。
  動的ディスパッチ（`function $(name)` マクロ）は呼び出しコストが高すぎる。
- クロージャ
- ユーザ定義マクロ

### 4.4 `String` と `Vec<T>` の操作

バニラで実現可能な操作のみを提供する。`String` は**不変値型**。

| 操作 | コスト | 実現手段 |
|---|---|---|
| リテラル代入・コピー | 無料 | `data modify ... set value` / `set from` |
| `s.len()` / `v.len()` | 無料 | `data get`（文字列は長さ、list は要素数を返す） |
| `s.slice(a..b)`（定数添字） | 無料 | `data modify ... set string <path> <start> <end>`（1.19.4+） |
| `s == "literal"` | 無料 | `execute if data ... {f:"x"}` |
| `s == other`（実行時同士） | マクロ昇格 | `$execute if data ... {f:"$(o)"}` |
| `s + other` | マクロ昇格 | `$data modify ... set value "$(a)$(b)"` |
| `v[0]`（定数添字） | 無料 | NBT パスの直接指定 |
| `v[i]` / `v[i] = x`（実行時添字） | マクロ昇格 | `$... storage ns list[$(i)]` |
| `v.push(x)` | 無料 | `data modify ... append` |
| 文字列の検索・置換・分割 | **提供しない** | バニラに対応物が存在しない |

`v[i] = x` が可能で `&mut v[i]` が不可なのは矛盾ではない。代入は値の書き込みであり、
参照の取得ではない（§5）。

### 4.5 ドメイン型

- `Selector` — **コンパイル時のみの型。** 実行時に変数へ格納できない。
- `EntityRef` — UUID を storage に保持し `@e[...]` で引き直す。**オプトイン。**
  デフォルトで払わせるにはコストが高い。
- `Pos` — 座標（絶対 / `~` 相対 / `^` ローカル）
- `ResourceLocation` — `minecraft:stone` 等の ID
- `TextComponent` — `tellraw` 等の JSON テキスト

---

## 5. 参照

**`&T` / `&mut T` はコンパイル時のみの概念。実行時表現を持たない。**

借用先の storage パスが静的に決まる場合のみ許可し、呼び出し箇所ごとに単相化して
パスへ直接展開する。マクロ関数への昇格もコピーも発生しない。

| 式 | 可否 |
|---|---|
| `s.bump()` (`&mut self`) | OK |
| `f(&mut self.inner)` | OK |
| `&v[0]`（定数添字） | OK |
| `&mut v[i]`（実行時添字） | **コンパイルエラー**。`v.set(i, x)` を使う |
| 参照を struct のフィールドに持つ | 不可 |
| 参照を返す | 不可 |

ライフタイムを導入しない代償としてこれらを禁止する。借用チェッカは持たない
（エイリアス違反は検出しない）。

---

## 6. 実行コンテキスト

mcfunction の全コマンドは暗黙のコンテキスト（実行者 `@s`・位置・回転・次元）で動く。
**mcfunction 最大のバグ源は「`@s` が存在しないコンテキストから `@s` を使う関数を呼び、
Minecraft が黙って何もしない」こと。** これを静的に潰す。

### 6.1 構文

```rust
as zombies { ... }          // execute as @e[type=zombie] run
at self { ... }             // execute at @s run
for e in zombies { ... }    // as + 各エンティティで本体実行
```

### 6.2 静的検査

```rust
#[ctx(entity)]              // この関数は実行者を要求する
fn take_damage() { ... }
```

呼び出し側のコンテキストが不足していれば**コンパイルエラー**。
要求は呼び出しグラフを伝播する。

**これは minewell が mcfunction に勝つ最大の理由であり、実装優先度が最も高い。**

### 6.3 実行中のエンティティ消失

`as e { ... }` の途中で `e` が死ぬケースはバニラ準拠とする。
以降 `@s` を対象とするコマンドは黙って no-op になる。

- **release ビルド**: 何もしない（バニラと同じ挙動、追加コストゼロ）
- **debug ビルド**: ブロック先頭に `execute if entity @s` のガードを挿入し、
  不在なら `tellraw` で発生箇所を報告する
- `EntityRef::resolve()` は `Option<...>` を返す（明示的に扱わせる）

常時ガードを入れない理由 — 実行者を使うすべてのブロックに 1 コマンド追加することになり、
`for e in sel` の中では反復回数だけ増える。設計原則 3 に反する。

---

## 7. 制御フローの lowering

mcfunction にジャンプは無い。使える原始命令は `execute if ... run function` と
`return`（1.20.2+）のみ。

**ハイブリッド方式:**

| 構文 | 出力 |
|---|---|
| 単文の `if` | `execute if score ... run <cmd>` 1 行にインライン。ファイルを作らない |
| 複合ブロック / `else` | 別 mcfunction に切り出し |
| `while` / `loop` | 別 mcfunction。末尾で `execute if <cond> run function <self>` の自己再帰 |
| `break` | ループ本体からの `return 1` + 呼び出し側の `execute if` ガード |
| `continue` | `return 0` |

`#[inline]` / `#[no_inline]` で明示上書き可。

### 7.1 `for` の 3 形態

**すべて明示。暗黙の振る舞いを持たせない。**

| 構文 | lowering | 備考 |
|---|---|---|
| `for i in 0..N` | 実行時ループ（`while` と同じ） | `#[unroll]` で明示的に展開。要素数による暗黙の切り替えはしない |
| `for e in sel` | `execute as <sel> run function <body>` | **位置は移動しない。** 移したい場合は本体に `at self { }` を書く |
| `for x in vec` | 破壊的反復 | `vec` のコピーを作り、`[0]` を読んで `data remove [0]` を繰り返す。**マクロ不要**。コストは list コピー 1 回 |

`for e in sel` に `at` を含めない理由 — 位置が暗黙に動くのはデータパックの
古典的なバグ源。9 割のケースで `at` が欲しいとしても、残り 1 割が
「原因が分からないまま座標がずれる」形で潰れる。

`for x in &mut vec`（要素の変更）は v1 では非対応。破壊的反復と両立しないため。

`for i in 0..N` を暗黙に展開しない理由 — 閾値による切り替えは、
定数を 1 つ変えただけで生成コマンド数が桁で変わることを意味する。
Minecraft では生成量が性能要件（設計原則 3）なので、予測可能性を優先する。

**再帰について:** mcfunction の関数再帰はバニラで動作する（制限は
`maxCommandChainLength` に基づく tick 内総コマンド数のみ）。

---

## 8. 呼び出し規約

scoreboard のフェイクプレイヤー名はグローバルであり、素朴に割り当てると再帰で
自分のローカル変数を踏み潰す。

**SCC 解析方式:**

- Tarjan で呼び出しグラフの強連結成分を求める
- **非再帰関数** → 固定フェイクプレイヤー。追加コストゼロ
- **再帰に参加する関数** → storage 上のスタックへフレームを push/pop

データパックの関数の大半は非再帰であり、その全てが 1 コマンドも余分に払わない。

---

## 9. 失敗の扱い

バニラのコマンドは失敗しても黙る。一方、全コマンドは `execute store success` で
0/1 を取得できる（**失敗の検出自体は 1 コマンドで無料**）。

- 失敗しうる stdlib は `Option<T>` を返す（`get_data`, `find_entity`, `get_block` 等）
- `?` 演算子は `Option` に対してのみ。`if let Some(x)` / `match` をサポート
- **`Result<T, E>` は v1 では持たない。** エラー値の受け渡しコストが呼び出しごとに乗る
- `debug_assert!` / `expect()` — debug ビルドでは失敗時に `tellraw @a` で
  発生箇所（ファイル:行）を報告。release ビルドでは消える

---

## 10. マクロと生コマンド

### 10.1 エスケープハッチ

```rust
raw!("setblock ~ ~1 ~ minecraft:stone")
```

- コンパイル時定数の補間 `{X}` は**無料**（単なる文字列連結）
- 実行時値の補間は、その関数を**マクロ関数**（`$(v)` を含み
  `function ... with storage` で呼ぶ必要がある関数）へ**自動昇格**させる

**昇格は伝播しない。** マクロ性は「呼ばれ方」の属性であり、呼び出し側に感染しない。
呼び出し側は引数を storage へ書いて `function ns:f with storage <ns>:mw args` するだけで、
自身はマクロ関数にならない。コストは呼び出しごとの引数マーシャル数コマンドのみ、局所。

ただし **`#[tick]` / `#[load]` の関数がマクロ関数になった場合はコンパイルエラー。**
function tag は引数なしで呼ぶため、実行時に沈黙して失敗する。静的に弾ける。
- 中身の構文検査は行わない（それが存在理由）。ただし**コマンド名のみ**スキーマ照合して
  typo を弾く（無効化可能）

### 10.2 組み込みマクロ

Rust の文法と Minecraft の記法が衝突する箇所だけマクロにする。

| 記法 | 扱い | 理由 |
|---|---|---|
| `@e[type=zombie, distance=..8]` | **素の構文** | `@` は Rust で未使用 |
| `minecraft:stone` | **素の構文** | 字句規則「`:` の両側に空白なし」で型注釈と区別 |
| `pos!(~ ~1 ~)` | マクロ | `^`（ローカル座標）が XOR と衝突 |
| `nbt!({ Health: 20f })` | マクロ | `{}` が struct リテラル / ブロックと衝突 |
| `text!("HP: ", hp, color = red)` | マクロ | JSON を手書きさせない |

`pos!` をマクロにする代償（座標は頻出）を払う理由:
**文法の曖昧性は一度入れると二度と取れない。** 糖衣は後から足せる。

`nbt!` は文脈の struct 型と照合し、フィールドを検査する。

### 10.3 `text!` の仕様

**`text!` は連結のみを担当し、装飾は `TextComponent` のコンパイル時メソッドチェーンで行う。**
マクロに専用の名前付き引数構文を発明しない。

```rust
tellraw(@a, text!("Danger".red().bold(), " HP: ", hp));
tellraw(@a, text!("Click".underlined().on_click(run_command("/spawn"))));
```

- 引数は左から連結され、JSON の配列コンポーネントになる
- 各引数の自動変換:
  - 文字列リテラル → `{"text":"..."}`
  - `i32` / `fix<S>` / `bool`（score 常駐） → `{"score":{"name":"$v","objective":"<ns>.v"}}`
  - `String`（storage 常駐） → `{"nbt":"...","storage":"<ns>:mw"}`
  - `TextComponent` → そのまま埋め込み（ネスト可）
- メソッド: 色（`.red()` 等 + `.color(hex)`）、装飾（`.bold()` `.italic()`
  `.underlined()` `.strikethrough()` `.obfuscated()`）、イベント
  （`.on_click(...)` `.on_hover(...)`）、翻訳（`translate!("key", args...)`）
- すべてコンパイル時に解決され、実行時コストはゼロ

これが minewell の実用上の目玉の一つになる。バニラで `tellraw` に実行時値を混ぜるには
`{"score":{"name":"$x","objective":"obj"}}` を手書きする必要があり、
内部フェイクプレイヤー名（`$` プレフィクス、§3.3）を人間が知っている前提になってしまう。

### 10.3 ユーザ定義マクロ

v1 では持たない。単相化ジェネリクスと const で大半は足りる。

---

## 11. 時間軸

**v1 ではマルチ tick 実行を言語機能にしない。**

- `#[tick]` / `#[load]` 属性 → `minecraft:tick` / `minecraft:load` の function tag を自動生成
- `schedule(f, 1t)` は stdlib 関数
- tick 跨ぎのループは利用者が書く
- **`async` / `await` は予約語のみ確保**（後方互換で追加可能にしておく）

切る理由: `sleep()` の状態機械化は、ローカル変数の storage 退避、継続の識別、
tick を跨いだ `@s` コンテキストの復元（実行者が死んでいる可能性がある）を
すべて解く必要がある。言語の基盤が動く前に着手すべきではない。

---

## 12. プロジェクト構成と出力

### 12.1 レイアウト（Cargo 準拠）

```
minewell.toml          # name, namespace, toolchain, deps
src/
  lib.mwl              # crate root
  combat/
    mod.mwl            # mod combat
    damage.mwl         # mod combat::damage
data/                  # 手書き JSON（advancement 等）。パススルーでコピー
target/datapack/       # 出力
```

### 12.2 名前の写像

- `combat::damage::apply` → `myns:combat/damage/apply`
- 内部生成関数 → `myns:combat/damage/apply/if_0`
  （**親のサブディレクトリに置く。** `zz_internal/` へ平置きするとデバッグ不能になる）
- 外部データパックの関数は `extern fn` 宣言で呼び出す（シグネチャは人間が書く）

### 12.3 依存管理とビルド単位

- **v1 ではパス依存のみ**（`{ path = "../mylib" }`）。レジストリは作らない
- **全プログラム一括コンパイル。** 分割コンパイルを行わない

一括コンパイルにする理由 — 依存がパス依存のみである以上、全ソースが常に手元にある。
分割しないことで:

- regalloc（§15）と SCC 解析（§8）が**プログラム全体に効く**。
  crate 境界で保守的に妥協する必要がない
- objective が `<namespace>.t` / `<namespace>.v` の 2 本で足りる（§3.3）。
  crate ごとに objective を分ける必要がない
- crate 間の呼び出し規約を固定する必要がない

データパックの規模でビルド時間が問題になることはない。
問題になったときに分割コンパイルへ移行するのは、regalloc に crate 境界の
妥協を入れる作業であり、後からでも可能。

---

## 13. データパックの他リソース

**v1 では関数と function tag のみ生成する。**

- advancement / predicate / loot_table / recipe 等は `data/` の手書き JSON をパススルーでコピー
- ただし **`.mwl` から参照した ID の存在は検査する。**
  `execute_if_predicate(ns:in_rain)` と書いて `data/ns/predicate/in_rain.json` が
  無ければコンパイルエラー。ほぼ無料で、バニラでは検出不能
- `#[on_advancement(...)]` は属性名のみ予約

predicate 生成を切る理由: JSON スキーマが巨大な上、minewell の式から predicate へ落とすのは
「実行時に評価できない条件を静的に切り出す」という新しい解析問題になる。
（ただし将来の目玉機能として意識しておく。）

---

## 14. コンパイラ構成

### 14.1 crate

```
crates/
  tinymcf/     # mcfunction インタプリタ。コンパイラに一切依存しない。単体公開可能
  mwlc/        # コンパイラ本体
    syntax/    #   lexer → parser → AST
    hir/       #   名前解決 → 型検査 → 単相化          [AST → HIR]
    mir/       #   CFG 構築 → SCC 解析 → regalloc      [HIR → MIR]
    emit/      #   MIR → mcfunction + datapack 出力
    schema/    #   commands.json ローダ（toolchain）
  mwl/         # CLI
```

- **`mwlc` は `tinymcf` に依存しない。** 依存は逆方向にも存在しない。
  両者が出会うのはテスト（dev-dependency）のみ。これで `tinymcf` の独立公開を保証する
- コンパイラ内部を 6 crate に割らない理由: 存在しないコードの境界を先に引くのは投機。
  module で分けておけば crate への昇格は機械的作業

### 14.2 IR 2 段の理由

- **HIR** = 型が付いた minewell。型検査・`fix<S>` スケール検査・`#[ctx]` 検査はここでしか書けない
- **MIR** = 仮想レジスタと基本ブロックを持つ、mcfunction にほぼ 1:1 の低レベル表現。
  レジスタ割り付けとインライン判定はここでしか書けない

1 段に潰すと両方が同じ pass に混ざる。

### 14.3 診断

`miette` を使用。span 情報は debug ビルドの行番号コメント埋め込みと共用する。

---

## 15. ビルドプロファイルと最適化

### `debug`（既定）

- 生成 mcfunction の各行前に `# src/combat.mwl:42` を挿入
- `debug_assert!` / `expect()` を有効化
- 最適化なし。ソースと出力が 1:1 で追える

### `release`

3 パスのみ実装する。

1. **定数畳み込み** — `2 + 3 * X` の畳み込み、`raw!` の定数補間
2. **デッドコード除去** — 未到達関数・未使用 objective を出力しない
3. **scoreboard レジスタ再利用** — 生存解析でテンポラリのフェイクプレイヤーを使い回す。
   これが無いと式ごとに `$tmp_0`, `$tmp_1`, ... が増え続け `minewell.tmp` が肥大化する

**それ以上を v1 で行わない理由:** `execute` 連鎖のマージや関数インライン化は、
効果を測る前に入れると検証コストだけ払う。**`tinymcf` が消費コマンド数を返せるので、
最適化は数字が出てから足す。**

---

## 16. テスト戦略

トランスパイラの TDD は golden file 地獄に堕ちやすい。`fact(5)` が本当に 120 を返すことを
Minecraft を起動せずに検証できないと TDD が回らない。

| 層 | 手段 | 検証対象 |
|---|---|---|
| lexer / parser / typeck | 素の `#[test]` | TDD の主戦場。全体の約 8 割 |
| codegen | `insta` スナップショット | 出力の形。差分レビュー用であって正しさの根拠ではない |
| **意味論** | **`tinymcf`** | `fact(5) == 120` を Rust のテスト内で実行して確認 |
| 統合 | 実サーバ（手動 / nightly） | `tinymcf` のモデルが現実とズレていないか |

### tinymcf の実装範囲

- 実装する: `scoreboard players` 系、`data` 系、`function`、
  `execute if` / `store` / `as` / `at`、`return`、マクロ関数呼び出し
- スタブ: ブロック配置、エンティティ生成、その他の副作用コマンド（呼ばれた記録のみ残す）
- 追加機能: **実行コマンド数のカウント**（最適化の効果測定と、
  `maxCommandChainLength` 超過の検出に使う）

`tinymcf` があることで、最適化パスを「意味論が変わらないこと」で検証できる。
golden file は最適化のたびに全滅するため、これが無いとリファクタが不可能になる。

### 16.1 コマンド数の静的検査

`maxCommandChainLength`（既定 65536）は 1 tick あたりの実行コマンド数を制限する。
超過すると Minecraft がチェーンを打ち切り、**エラーを出さずに処理が途中で止まる。**

- **ループ・再帰を含まない関数** → 正確なコマンド数を静的に計算し、閾値超過を警告
- **ループ・再帰を含む関数** → 「1 反復あたりのコスト」と「固定部分のコスト」を報告
- `mwl build` が `target/cost.txt` に関数別コスト表を出力
- `tinymcf` の実測カウント（§16）と突き合わせて、静的計算の妥当性を検証する

---

## 17. CLI

```
mwl new <name>              プロジェクト生成
mwl check                   型検査のみ（将来の LSP と共通経路）
mwl build [--release]       target/datapack/ へ出力
mwl test                    tinymcf でテスト実行
mwl install <world_path>    ワールドの datapacks/ へ配置
mwl toolchain install|list|build-from-jar
```

`mwl test` が `tinymcf` を呼ぶのが TDD ループの実体。
**ここが 1 秒以内であることが開発体験を決める。**

---

## 18. マイルストーン

| # | 内容 | 完了条件 |
|---|---|---|
| **M0** | `tinymcf` | 手書き mcfunction を実行できる。コンパイラゼロの状態で単体完成・単体テスト可能 |
| **M1** | Hello World 縦断 | `fn main() { raw!("say hi") }` が `.mwl` → AST → HIR → MIR → `.mcfunction` → `tinymcf` 実行まで貫通 |
| **M2** | 値と式 | `i32` / `bool` / `let` / 算術 / 比較 / scoreboard 割り付け |
| **M3** | 制御フロー | `if` / `else` / `while` / `loop` / `break` / `continue` |
| **M4** | 関数 | 引数 / 戻り値 / SCC 解析 / 再帰 |
| **M5** | コンテキスト ★ | `as` / `at` / `for e in sel` / `#[ctx]` 検査 |
| **M6** | スキーマ統合 | `commands.json` ロード / 型付きコマンド呼び出し / toolchain |
| **M7** | 複合型 | `struct` / `enum` / `match` / `Vec` / storage 割り付け |
| **M8** | 数値拡張 | `fix<S>` / NBT 相互運用型 |
| **M9** | 仕上げ | `Option` / `?` / `debug_assert` / release 最適化 / CLI 完成 |

**M0 を最初に置く理由:** 測定器が無い状態で TDD を始めると M1〜M4 のテストが
全部 golden file になり、M5 以降で必ず書き直しになる。

**M5 を M6 より前に置く理由:** `#[ctx]` 検査が動いた時点で minewell は
「使う価値のある道具」になる。逆に M6（コマンド網羅）を先にやると、
「機能は多いが誰も嬉しくない」状態が長く続く。

---

## 19. v1 スコープ外（予約のみ）

| 項目 | 予約する識別子 |
|---|---|
| マルチ tick 実行 | `async` / `await` |
| トレイト | `trait` / `dyn` / `impl Trait for` |
| エラー型 | `Result` |
| ユーザ定義マクロ | `macro_rules!` |
| 借用チェッカ | — |
| advancement / predicate 生成 | `#[on_advancement(...)]` |
| LSP | — |
| パッケージレジストリ | — |
| 動的ディスパッチ | — |

---

## 20. 未解決事項

**なし。** 初版で挙げた 10 件はすべて解決済み（対応節は下表）。

| 旧未解決事項 | 決定 | 節 |
|---|---|---|
| `String` の意味論 | 不変値型。`len`/定数 `slice`/リテラル比較は無料、連結と実行時比較はマクロ昇格。検索・置換・分割は提供しない | §4.4 |
| `for` の 3 形態 | すべて明示。`0..N` は実行時ループ（`#[unroll]` で展開）、`for e in sel` は `as` のみで位置は動かさない、`for x in vec` は破壊的反復でマクロ不要 | §7.1 |
| `Vec<T>` の動的インデックス | `v[i]` / `v[i] = x` を許しマクロ昇格。`&mut v[i]` は引き続きエラー | §4.4, §5 |
| マクロ昇格の伝播規則 | **伝播しない。** 局所。`#[tick]`/`#[load]` がマクロ関数になった場合のみエラー | §10.1 |
| `text!` の詳細仕様 | 連結のみ担当。装飾はコンパイル時メソッドチェーン | §10.3 |
| エンティティ消失 | バニラ準拠。debug ビルドでのみ生存ガードを挿入 | §6.3 |
| crate 間の名前空間共存 | **全プログラム一括コンパイル。** objective 2 本 + storage 1 本、`<namespace>` 前置 | §3.3, §12.3 |
| `maxCommandChainLength` の静的検出 | ループなしは正確に計算、ありは反復あたりコストを報告。`target/cost.txt` へ出力 | §16.1 |
| brigadier → 型の写像 | 機械生成 + `overrides.toml` で頻出 30 コマンドのみ手書き。未知の parser 型は `RawArg` + 警告 | §1.4 |
| `#[nbt(...)]` の網羅 | タグ型指定 / `rename` / `optional` の 3 種のみ | §4.2 |

以降の未解決事項は詳細仕様（`02-spec.md`）と実装計画（`03-plan.md`）で管理する。
