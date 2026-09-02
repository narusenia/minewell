// SPDX-License-Identifier: MIT

//! The Zed side of `mwl lsp`.
//!
//! Zed asks a WebAssembly extension how to start a language server; this answers with
//! `mwl lsp`, looked up on the worktree's PATH. Nothing here knows anything else about
//! the compiler — the extension carries a grammar and a command line, and pinning it
//! to one build would be exactly the version sync this repository avoids.

use zed_extension_api::{self as zed, Command, LanguageServerId, Result, Worktree};

struct Minewell;

impl zed::Extension for Minewell {
    fn new() -> Self {
        Minewell
    }

    fn language_server_command(
        &mut self,
        _id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let path = worktree
            .which("mwl")
            .ok_or_else(|| "'mwl' is not on PATH; install it for diagnostics".to_owned())?;
        Ok(Command {
            command: path,
            args: vec!["lsp".to_owned()],
            env: worktree.shell_env(),
        })
    }
}

zed::register_extension!(Minewell);
