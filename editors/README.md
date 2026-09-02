# エディタ支援

ハイライトと、保存時の診断（LSP）。

置いてあるもの:

| 場所 | 何 |
|---|---|
| `tree-sitter-mwl/` | `.mwl` の tree-sitter grammar とハイライトクエリ。nvim と zed が指す |
| `vscode/` | VS Code 拡張。TextMate grammar（同じ字句規則から起こした 2 本目）と LSP クライアント |
| `zed/` | Zed 拡張。grammar は `tree-sitter-mwl/` を指し、クエリはシンボリックリンク。LSP は WASM 拡張 |
| `nvim/` | nvim-treesitter と `vim.lsp` の設定断片 |

**ハイライトの側はコンパイラに依存しない。** 依存させた瞬間に版の同期が発生する。
LSP はもちろん依存する（それが仕事）ので、別物として扱う。

## 診断（LSP）

`mwl lsp` が stdin / stdout で喋る。**診断だけ**で、定義ジャンプと補完はまだ無い。
3 つの拡張はどれも**`mwl` を名前で探して起動するだけ**なので、
コンパイラのどの版とも結びつかない。

| エディタ | 何をするか | 要るもの |
|---|---|---|
| nvim | `vim.lsp.config` に 5 行（[`nvim/README.md`](nvim/README.md)） | 無し |
| VS Code | 拡張が `vscode-languageclient` で起動する | `npm install` |
| Zed | WASM 拡張が起動コマンドを答える（`zed/src/lib.rs`） | `wasm32-wasip1` |

`mwl` が PATH に無ければ診断が出ないだけで、**ハイライトは動く**。
VS Code は `mwl.path` でバイナリを指せる。

近くの `minewell.toml` を見て namespace と toolchain を決めるので、
**プロジェクトの中なら実物のコマンド表で検査する。** 外にある `.mwl` でも
構文と型のエラーは出る。

## 検査していること

`mise run editors`（`ci` に入っている）が見るのは:

- VS Code のクライアントが JavaScript として解析できること、`package.json` が JSON であること
- Zed の拡張が `wasm32-wasip1` にビルドできること

**エディタに読み込んだ状態までは自動で確かめていない。** そこは手で開くしかない。

## 作り方

生成物（`tree-sitter-mwl/src/`）はコミットしていない。作るのは 1 コマンド:

```
mise run grammar   # tree-sitter grammar を作り、examples/ を全部パースする
mise run editors   # VS Code の JS と Zed の WASM をビルドする
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
