# エディタ支援

ハイライトと、保存時の診断（LSP）。

置いてあるもの:

| 場所 | 何 |
|---|---|
| `tree-sitter-mwl/` | `.mwl` の tree-sitter grammar とハイライトクエリ。nvim と zed が指す |
| `vscode/` | VS Code 拡張。TextMate grammar が別に要るので同じ字句規則から 2 本目を書いてある |
| `zed/` | Zed 拡張。grammar は `tree-sitter-mwl/` を指し、クエリはシンボリックリンク |
| `nvim/` | nvim-treesitter へ登録する設定断片 |

**ハイライトの側はコンパイラに依存しない。** 依存させた瞬間に版の同期が発生する。
LSP はもちろん依存する（それが仕事）ので、別物として扱う。

## 診断（LSP）

`mwl lsp` が stdin / stdout で喋る。**診断だけ**で、定義ジャンプと補完はまだ無い。

```lua
-- nvim
vim.lsp.config.mwl = {
  cmd = { "mwl", "lsp" },
  filetypes = { "mwl" },
  root_markers = { "minewell.toml" },
}
vim.lsp.enable("mwl")
```

```json
// VS Code や Zed から起動するときも、コマンドは同じ
{ "command": "mwl", "args": ["lsp"] }
```

近くの `minewell.toml` を見て namespace と toolchain を決めるので、
**プロジェクトの中なら実物のコマンド表で検査する。** 外にある `.mwl` でも
構文と型のエラーは出る。

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
