# nvim

`nvim-treesitter` に grammar を登録する。パスは手元の clone を指す。

```lua
vim.filetype.add({ extension = { mwl = "mwl" } })

require("nvim-treesitter.parsers").get_parser_configs().mwl = {
  install_info = {
    url = vim.fn.expand("~/src/minewell/editors/tree-sitter-mwl"),
    files = { "src/parser.c" },
    generate_requires_npm = false,
    requires_generate_from_grammar = true,
  },
  filetype = "mwl",
}
```

`:TSInstall mwl` のあと、ハイライトクエリを置く:

```
ln -s ~/src/minewell/editors/tree-sitter-mwl/queries/highlights.scm \
      ~/.config/nvim/queries/mwl/highlights.scm
```

`requires_generate_from_grammar = true` なのは、生成物をコミットしていないから
（`editors/README.md`）。
