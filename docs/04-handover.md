# 引き継ぎ

2026-08-24 時点。**61 / 90 タスク完了、テスト 317、コミット 52。**
M0〜M6 完了、次は M7（複合型）。

---

## まず読むもの

| 順 | 文書 | 何が書いてあるか |
|---|---|---|
| 1 | [`../AGENTS.md`](../AGENTS.md) | 規約・不変条件・作業手順・**過去に踏んだ罠** |
| 2 | [`01-requirements.md`](./01-requirements.md) | 設計決定と**その理由**。ここが正典 |
| 3 | [`03-plan.md`](./03-plan.md) | タスク一覧と進捗。各タスクに実装後の判断メモ |
| 4 | [`02-spec.md`](./02-spec.md) | 文法・型規則・lowering。M6 の範囲まで確定 |
| 5 | [`../crates/tinymcf/SPEC.md`](../crates/tinymcf/SPEC.md) | インタプリタがモデル化する mcfunction の範囲 |

**実装が仕様と食い違ったら仕様が正しい。** 仕様を変えるなら、変えてから実装する。

---

## 今どこまで動くか

```rust
#[ctx(entity)]
fn ignite() { raw!("data merge entity @s {Fire: 100s}"); }

#[tick]
fn tick() {
    let mut n = 0;
    for z in @e[type=zombie, distance=..8] {
        if n >= 4 { break; }
        ignite();
        n += 1;
    }
    if n > 0 { setblock(pos!(~ ~1 ~), minecraft:stone); }
}
```

動くもの: `let` / 可変性 / `i32` / `bool` / 全演算子 / `if`・`else`・`while`・`loop` /
`break`・`continue`・`return` / 関数・引数・戻り値・再帰（相互再帰含む）/
`as`・`at`・`for` と `#[ctx]` 検査 / セレクタ・`pos!`・リソースロケーション /
toolchain 由来のコマンド呼び出し / `#[tick]`・`#[load]` タグ / `data/` パススルー /
存在しない関数参照の検出 / debug の行番号コメントと実行者ガード。

動かないもの: `struct`・`enum`・`match`・`Vec`・ジェネリクス・`String` の値・
`fix<S>`・`Option`・`text!`・`nbt!`・release 最適化・`mwl new`/`check`/`test`/`install`。

---

## 次の作業（M7: 複合型）

9 タスク。`storage` が初めて本格的に使われる。

**着手前に確定させること**（[`02-spec.md`](./02-spec.md) に節を足す）:

1. **`struct` の storage レイアウト。** `<ns>:mw` の `mw.vars` 配下のどこに、どの名前で置くか。
   関数ローカルと同じく関数名で修飾するのか、それとも別の割り付けか
2. **`enum` のタグ表現。** 要件定義 §4.2 は `{tag:"Idle"}`。`match` は
   `execute if data storage ... {tag:"Idle"}` の連鎖になる
3. **`if` を式にするか。** 仕様 §3.5 で「`match` と一緒に考える」と保留してある。
   合流点で値を作る仕組みが要る
4. **`String` の値。** 仕様では今 `Type::Resource` に相乗りしている（コマンド引数専用）。
   M7 か M8 で本物の storage 上の文字列型に分ける必要がある

**既知の落とし穴:**

- `Type::is_compile_time()` が `Selector`/`Resource`/`Pos` を弾いている。
  `struct` は**実行時型**なので、この分類にそのまま乗らない。storage 常駐という
  第 3 のカテゴリが要る
- MIR の `Reg` は scoreboard 前提。storage 上の値には別の場所指定（NBT パス）が要る
- 再帰時の退避（`live_registers()`）は scoreboard レジスタしか見ていない。
  storage 上のローカルを持つ関数が再帰したら**壊れる**。M7 で必ず対処すること

---

## 積み残し（理由つき）

| 項目 | なぜ後回しか | 予定 |
|---|---|---|
| **宛先駆動の式 lowering** | 呼び出し・算術・`&&` の結果がテンポラリ経由でコピーされ 1 コマンド余分。M4 で 3 箇所に散らすと一貫性を失うため 1 変更にまとめる | M9-10 |
| **`positioned`/`in`/`if block`/`if predicate`**（tinymcf） | 座標・次元・ブロック・predicate のモデルが要り、まだ誰も使わない | 必要になった時 |
| **`mwl toolchain install`** | 取得先の Release がまだ存在せず、HTTP クライアントを足しても検証できない | X-2 の後 |
| **predicate / loot table の ID 検査** | parser 名が版で揺れる。実物の `commands.json` を見てから | 実データ入手後 |
| **`entity`/`block` の `data` 対象**（tinymcf） | M8-4（`Pos[0]` を `fix<1000>` に読む）で要る | M8 |
| **`async`/`trait`/`Result`/ユーザ定義マクロ/借用検査/LSP** | v1 スコープ外。予約語だけ確保済み | v1 後 |

---

## 実装中に決めた、要件定義に書いていなかったこと

要件定義の「Q&A で決めたこと」からは導けず、実装して初めて必要になった判断。
すべて仕様書に反映済みだが、経緯はここにしかない。

1. **MIR にジャンプ命令を入れない。** ターゲットにジャンプが無いので、辺で繋いだ CFG を
   持つ意味が無い。制御フローは生成関数とガード付き命令の 2 つで表す
2. **制御レジスタは必要なときだけ。** ブロックから制御が抜けないなら `$ctl` は
   1 度も現れない。抜けるかどうかはブロックごとに静的に分かる
3. **`#[ctx]` は宣言のみで推論しない。** 推論だと原因から遠い場所でエラーが出る。
   `raw!` の中身は見えないのでどのみち申告が要る
4. **`at self` ではなく `at @s`。** `self` は M7 の `impl` レシーバに予約済み
5. **`&&`/`||` は右辺が純粋なら短絡させない。** 観測上区別できないので 1 コマンドで済ませる。
   呼び出しを含む右辺だけ分岐する
6. **コマンド引数はコンパイル時の値のみ。** 実行時の値にはマクロ関数が要る（M9）
7. **objective 作成関数 `<ns>:__init` を生成し `minecraft:load` に載せる。**
   objective が無いとバニラが全 `scoreboard` コマンドを拒否するので選択肢が無い
8. **コンパイラにコマンド表を埋め込まない。** 埋め込めば「版非依存」が嘘になる。
   toolchain 未設定でも `raw!` だけで動くようにしてある

---

## 見つけたバグ（同種を繰り返さないために）

いずれも**生成された mcfunction を読んで**見つかった。単体テストは通っていた。

1. **生成ブロックからの `return` が呼び出し元に届かない。** mcfunction の `return` は
   自分が書かれた関数しか抜けない。`if n <= 1 { return 1; }` が `if_0` を抜けるだけで
   `fact` が次行へ進み、無限再帰した
2. **制御レジスタが呼び出し間で残る。** 最上位まで届いた `return` はレジスタを 3 のまま
   残す。`#[tick]` 関数なら一度早期 return しただけで以降ずっと何もしなくなる
3. **退避対象に未初期化のローカルが混ざる。** `let rest = f(n-1);` の途中で退避すると
   まだ空の `rest` を読もうとしてコマンドが失敗する

**教訓は AGENTS.md の「Things that have already caught people out」に集約してある。**

---

## 作業の型

```
仕様の該当節を確定させる  →  失敗するテストを書く（落ちるのを見る）
                          →  通す  →  mise run ci && git commit
                          →  03-plan.md のチェックボックスと集計を更新
```

- コミットは 1 タスク 1 コミット。仕様 + 実装 + 進捗更新は同一コミット（AGENTS.md の DoD）
- **`mise run ci && git commit` と繋ぐ。** 並べて書くと CI 失敗でも commit が通る（2 回やった）
- push 前に `mise run ci` の終了ステータスを見る。出力の grep では lint 失敗を見落とす

## 環境

- `mise install` → rust（rustfmt/clippy 込み）と cargo-insta
- `mise run test` / `lint` / `ci` / `snap`
- CI は GitHub Actions（mise-action + `Swatinem/rust-cache`）、push と PR で走る
- toolchain 置き場は `MINEWELL_HOME` で変更可。テストは各自の一時ディレクトリを使う
