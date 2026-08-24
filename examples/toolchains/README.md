# A toolchain for the examples

`mwlc` knows no Minecraft commands of its own: the command set comes from a toolchain
(`docs/01-requirements.md` §1.2). The examples that call commands need one, so this
directory is a minimal stand-in.

- **`1.21.4/commands.json` is hand-written and covers only what the examples call.**
  The real file is produced by Minecraft's data generator and is about a megabyte:
  ```
  java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports
  ```
  and lands in `generated/reports/commands.json`.
- `overrides.toml` renames two commands whose generated names read badly.

To build an example by hand, point `MINEWELL_HOME` at this directory's parent:

```
cd examples/arena && MINEWELL_HOME=.. mwl build
```

Once real toolchains are published (`mwl toolchain install 1.21.4`), this goes away.
