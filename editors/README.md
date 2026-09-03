# エディタ支援

ハイライトと、保存時の診断（LSP）。

置いてあるもの:

| 場所 | 何 |
|---|---|
| `tree-sitter-mwl/` | `.mwl` の tree-sitter grammar とハイライトクエリ。nvim と zed が指す |
| `vscode/` | VS Code 拡張。TextMate grammar（同じ字句規則から起こした 2 本目）と LSP クライアント |
| `zed/` | Zed 拡張。grammar は `tree-sitter-mwl/` を指し、クエリはシンボリックリンク。LSP は WASM 拡張 |
| `nvim/` | → **別リポジトリ**（[minewell-nvim](https://github.com/narusenia/minewell-nvim)） |

**ハイライトの側はコンパイラに依存しない。** 依存させた瞬間に版の同期が発生する。
LSP はもちろん依存する（それが仕事）ので、別物として扱う。

## 診断（LSP）

`mwl lsp` が stdin / stdout で喋る。**診断だけ**で、定義ジャンプと補完はまだ無い。
3 つの拡張はどれも**`mwl` を名前で探して起動するだけ**なので、
コンパイラのどの版とも結びつかない。

| エディタ | 何をするか | 要るもの |
|---|---|---|
| nvim | [minewell-nvim](https://github.com/narusenia/minewell-nvim) を入れるだけ | 無し |
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

生成物（`tree-sitter-mwl/src/`）は**コミットしている。** Zed は
`tree-sitter generate` を走らせず、clone した中の `src/parser.c` を直接
コンパイルするだけなので、コミットされていないと grammar が組めない。
minewell-nvim が持ち込んでいるのも同じ理由。作り直すのは 1 コマンド:

```
mise run grammar   # tree-sitter generate して、examples/ を全部パースする
mise run editors   # VS Code の JS と Zed の WASM をビルドする
```

`tree-sitter generate` を走らせてから、`examples/` の `.mwl` を全部パースして
エラーが無いことを確かめる。**これが grammar のテスト**で、CI にも入っている。

### grammar を直したら

Zed は `extension.toml` の `commit` に書いたリビジョンを **remote から取る。**
ローカルのワーキングツリーは見ない。だから手順は 3 つあり、**push まで終えないと
Zed には何も反映されない**:

1. `mise run grammar` で `src/` を作り直す
2. `src/` の変更をコミットして push する
3. `editors/zed/extension.toml` の `commit` を、そのコミットの SHA に書き換える

`commit` は **40 桁の SHA でなければならない。** `HEAD` やブランチ名を書くと、
Zed の shallow fetch がローカルに ref を作らないまま `git checkout` に渡すので
`pathspec 'HEAD' did not match any file(s) known to git` で落ちる。

## 字句の罠

同じ規則を 2 本の grammar に書いているので、直すときは両方を直す。
仕様 §2 が正典で、罠は 3 つ:

- **リソースロケーション** — `minecraft:block.note_block.pling` は 1 トークン。
  `.` と `-` を含む。型注釈の `:` と区別できるのは**両側に空白が無い**ことだけ（§2.8）
- **セレクタ** — `@e[type=zombie, distance=..8]` は 1 トークン。
  中身は構造化せず、**文字列の中の `]` では閉じない**（§2.7）
- **ターボフィッシュ** — `fix::<1000>` の `::<` は型引数の位置にしか出ない（§3.16）
