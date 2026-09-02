# nvim

**別リポジトリ**: [narusenia/minewell-nvim](https://github.com/narusenia/minewell-nvim)

```lua
add({ source = "narusenia/minewell-nvim" })   -- mini.deps
{ "narusenia/minewell-nvim" }                 -- lazy.nvim
```

filetype・tree-sitter ハイライト・`mwl lsp` の診断がまとめて入る。**ビルド手順は無い**
（parser は初回に C コンパイラで作られる）。

## なぜ分けたか

nvim のプラグインは**リポジトリのルートが runtimepath に載る**前提で作られていて、
`plugin/` `queries/` `parser/` をそこに置く必要がある。プラグインマネージャの多くは
サブディレクトリを指せない。コンパイラのリポジトリのルートにそれを置くのは筋が悪いので、
**プラグインだけ別リポジトリにした**。

VS Code と Zed の拡張はサブディレクトリでよいので、ここに残っている。

## grammar はこちらが正典

`grammar.js` と `queries/highlights.scm` は
[`tree-sitter-mwl/`](tree-sitter-mwl/) にある。仕様（`docs/02-spec.md` §2）と、
grammar を突き合わせる `examples/` がここにあるからで、`mise run grammar` が
「examples を全部パースできること」を CI で見ている。

minewell-nvim は**生成物（`parser.c`）と query を持ち込んでいる**。
grammar を変えたら向こうで:

```sh
scripts/vendor.sh ~/ghq/github.com/NaruseNia/minewell
```

を走らせて、生成物を取り直してコミットする。
