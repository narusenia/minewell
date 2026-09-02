# nvim

ハイライトと診断。**どちらも入れるのが前提**だが、片方だけでも動く。

## ハイライト

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

```sh
ln -s ~/src/minewell/editors/tree-sitter-mwl/queries/highlights.scm \
      ~/.config/nvim/queries/mwl/highlights.scm
```

`requires_generate_from_grammar = true` なのは、生成物をコミットしていないから
（[`../README.md`](../README.md)）。

## 診断（LSP）

`mwl` が PATH にあれば、これだけ。nvim 0.11 以降:

```lua
vim.lsp.config.mwl = {
  cmd = { "mwl", "lsp" },
  filetypes = { "mwl" },
  root_markers = { "minewell.toml" },
}
vim.lsp.enable("mwl")
```

0.10 以前は `vim.lsp.start` を `FileType` の autocmd から:

```lua
vim.api.nvim_create_autocmd("FileType", {
  pattern = "mwl",
  callback = function(args)
    vim.lsp.start({
      name = "mwl",
      cmd = { "mwl", "lsp" },
      root_dir = vim.fs.root(args.buf, { "minewell.toml" }),
    })
  end,
})
```

**`mwl` は名前で起動するだけ**なので、プラグインはコンパイラのどの版とも結びつかない。
```
