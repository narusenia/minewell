// SPDX-License-Identifier: MIT

// Starts `mwl lsp` and hands VS Code the diagnostics it publishes.
//
// The binary is looked up by name rather than bundled: the extension carries a grammar
// and a client, and pinning it to one build of the compiler is exactly the version
// sync this repository avoids everywhere else.

const { workspace, window } = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;

function activate() {
  const configured = workspace.getConfiguration("mwl").get("path") || "mwl";
  const server = { command: configured, args: ["lsp"], transport: TransportKind.stdio };

  client = new LanguageClient(
    "mwl",
    "minewell",
    { run: server, debug: server },
    {
      documentSelector: [{ scheme: "file", language: "mwl" }],
      // A change to the manifest changes the command table and the namespace, so the
      // server wants to hear about it.
      synchronize: { fileEvents: workspace.createFileSystemWatcher("**/minewell.toml") },
    },
  );

  // Not being able to start is worth one message and nothing more: highlighting still
  // works without the compiler, and that is most of what this extension is.
  return client.start().catch((error) => {
    window.showWarningMessage(
      `minewell: could not start '${configured} lsp' (${error.message}). ` +
        "Highlighting still works; install mwl for diagnostics.",
    );
  });
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
