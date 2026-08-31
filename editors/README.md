# エディタ支援

**ハイライトだけ。LSP は v1 の後**（要件定義 §19）。

置いてあるもの:

| 場所 | 何 |
|---|---|
| `tree-sitter-mwl/` | `.mwl` の tree-sitter grammar とハイライトクエリ。nvim と zed が指す |
| `vscode/` | VS Code 拡張。TextMate grammar が別に要るので同じ字句規則から 2 本目を書いてある |
| `zed/` | Zed 拡張。grammar は `tree-sitter-mwl/` を指し、クエリはシンボリックリンク |
| `nvim/` | nvim-treesitter へ登録する設定断片 |

**どれもコンパイラに依存しない。** 依存させた瞬間に版の同期が発生する。

## 作り方

生成物（`tree-sitter-mwl/src/`）はコミットしていない。作るのは 1 コマンド:

```
mise run grammar
```

`tree-sitter generate` を走らせてから、`examples/` の `.mwl` を全部パースして
エラーが無いことを確かめる。**これが grammar のテスト**で、CI にも入っている。

## 字句の罠

同じ規則を 2 本の grammar に書いているので、直すときは両方を直す。
仕様 §2 が正典で、罠は 3 つ:

- **リソースロケーション** — `minecraft:block.note_block.pling` は 1 トークン。
  `.` と `-` を含む。型注釈の `:` と区別できるのは**両側に空白が無い**ことだけ（§2.8）
- **セレクタ** — `@e[type=zombie, distance=..8]` は 1 トークン。
  中身は構造化せず、**文字列の中の `]` では閉じない**（§2.7）
- **ターボフィッシュ** — `fix::<1000>` の `::<` は型引数の位置にしか出ない（§3.16）
