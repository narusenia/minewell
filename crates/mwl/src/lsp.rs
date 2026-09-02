// SPDX-License-Identifier: MIT

//! A language server, as small as one can be and still be worth running.
//!
//! Diagnostics only. `mwlc` already answers with problems and spans, so the server is
//! a loop: read a document, compile it, write the problems back. Definition jumps and
//! completion come after (requirements section 19).
//!
//! The wire format is `Content-Length` framing around JSON-RPC, which is little enough
//! to write out. A compiler is a strange place to grow an async runtime, and the same
//! reasoning kept a HTTP client out of `mwl toolchain install`.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use mwlc::driver;
use mwlc::emit::{Options, Profile, Source};
use mwlc::schema::Schema;
use mwlc::toolchain::Toolchains;
use serde_json::{Value, json};

/// Serves on stdin and stdout until the client says to stop.
pub fn serve() -> io::Result<()> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut server = Server::default();

    while let Some(message) = read(&mut input)? {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            continue;
        };
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                write(
                    &mut output,
                    &reply(
                        id,
                        json!({
                            "capabilities": {
                                // Full text every time: a `.mwl` file is small and the
                                // compiler is fast, so there is nothing to gain from
                                // tracking edits.
                                "textDocumentSync": 1,
                            },
                            "serverInfo": { "name": "mwl", "version": env!("CARGO_PKG_VERSION") },
                        }),
                    ),
                )?;
            }
            "textDocument/didOpen" => {
                let document = &params["textDocument"];
                server.publish(&mut output, &document["uri"], &document["text"])?;
            }
            "textDocument/didChange" => {
                // One full change, because that is the sync mode above.
                let text = &params["contentChanges"][0]["text"];
                server.publish(&mut output, &params["textDocument"]["uri"], text)?;
            }
            "textDocument/didSave" => {
                if let Some(text) = params.get("text") {
                    server.publish(&mut output, &params["textDocument"]["uri"], text)?;
                }
            }
            "shutdown" => write(&mut output, &reply(id, Value::Null))?,
            "exit" => return Ok(()),
            // A request must be answered even when it is not understood, or the
            // client waits for ever. A notification must not be.
            _ if id.is_some() => write(
                &mut output,
                &json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("{method} is not implemented") },
                }),
            )?,
            _ => {}
        }
    }
    Ok(())
}

#[derive(Default)]
struct Server {
    /// Command tables, by version. Reading one is a few hundred kilobytes of JSON, and
    /// a keystroke should not pay for that twice.
    toolchains: HashMap<String, Option<Schema>>,
}

impl Server {
    fn publish(&mut self, out: &mut impl Write, uri: &Value, text: &Value) -> io::Result<()> {
        let (Some(uri), Some(text)) = (uri.as_str(), text.as_str()) else {
            return Ok(());
        };
        let path = path_of(uri);
        let diagnostics = self.diagnose(path.as_deref(), text);
        write(
            out,
            &json!({
                "jsonrpc": "2.0",
                "method": "textDocument/publishDiagnostics",
                "params": { "uri": uri, "diagnostics": diagnostics },
            }),
        )
    }

    /// Compiles the text and turns what went wrong into LSP diagnostics.
    fn diagnose(&mut self, path: Option<&Path>, text: &str) -> Vec<Value> {
        let (namespace, toolchain) = self.project(path);
        let options = Options {
            profile: Profile::Debug,
            source: Some(Source {
                path: path.map_or_else(|| "<input>".to_owned(), |p| p.display().to_string()),
                text: text.to_owned(),
            }),
            ..Options::default()
        };
        let Err(report) = driver::compile_with(text, &namespace, &options, toolchain.as_ref())
        else {
            return Vec::new();
        };
        report
            .problems
            .iter()
            .map(|problem| {
                let (offset, len) = problem.range();
                json!({
                    "range": {
                        "start": position(text, offset),
                        "end": position(text, offset + len),
                    },
                    "severity": 1,
                    "source": "mwl",
                    "message": problem.message(),
                })
            })
            .collect()
    }

    /// The namespace and command table the file belongs to, from the nearest
    /// `minewell.toml`. A file outside a project still gets syntax and type errors.
    fn project(&mut self, path: Option<&Path>) -> (String, Option<Schema>) {
        let Some(root) = path.and_then(project_root) else {
            return ("mwl".to_owned(), None);
        };
        let Ok(manifest) = driver::manifest(&root) else {
            return ("mwl".to_owned(), None);
        };
        let namespace = manifest.package.namespace().to_owned();
        let Some(version) = manifest.package.toolchain.clone() else {
            return (namespace, None);
        };
        let schema = self
            .toolchains
            .entry(version.clone())
            .or_insert_with(|| {
                Toolchains::default()
                    .load(&version)
                    .ok()
                    .map(|toolchain| toolchain.schema)
            })
            .clone();
        (namespace, schema)
    }
}

/// The directory holding the `minewell.toml` this file belongs to.
fn project_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|dir| dir.join(driver::MANIFEST).is_file())
        .map(Path::to_path_buf)
}

/// `file:///a/b%20c.mwl` into a path. Only the escapes a path can actually contain.
fn path_of(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut out = String::with_capacity(rest.len());
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let hex: String = chars.by_ref().take(2).collect();
        match u8::from_str_radix(&hex, 16) {
            Ok(byte) => out.push(byte as char),
            Err(_) => return None,
        }
    }
    Some(PathBuf::from(out))
}

/// A byte offset as an LSP position.
///
/// Characters are UTF-16 code units, which is what the protocol counts in — not bytes
/// and not characters. Everything the lexer accepts is ASCII, but a string literal or
/// a comment can hold anything.
fn position(text: &str, offset: usize) -> Value {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count();
    let column = before.rsplit_once('\n').map_or(before, |(_, rest)| rest);
    json!({ "line": line, "character": column.encode_utf16().count() })
}

fn reply(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Reads one `Content-Length` framed message, or `None` at end of input.
fn read(input: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let Some(length) = length else {
        return Ok(None);
    };
    let mut body = vec![0u8; length];
    input.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body).ok())
}

fn write(out: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = message.to_string();
    write!(out, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broken_file_produces_one_diagnostic() {
        let mut server = Server::default();
        let found = server.diagnose(None, "fn main() { let = 1; }");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0]["severity"], 1);
        assert_eq!(found[0]["range"]["start"]["line"], 0);
    }

    #[test]
    fn a_file_that_compiles_produces_none() {
        let mut server = Server::default();
        assert!(server.diagnose(None, "fn main() { let a = 1; }").is_empty());
    }

    #[test]
    fn a_position_counts_lines_and_utf16_units() {
        let text = "fn main() {\n    let x = \"é\" + 1;\n}";
        let at = text.find('+').expect("there is one");
        let position = position(text, at);
        assert_eq!(position["line"], 1);
        // `é` is two bytes and one UTF-16 unit, so the column is not the byte offset.
        assert_eq!(position["character"], 16);
    }

    #[test]
    fn a_uri_becomes_a_path() {
        assert_eq!(
            path_of("file:///tmp/a%20b/lib.mwl"),
            Some(PathBuf::from("/tmp/a b/lib.mwl"))
        );
        assert_eq!(path_of("untitled:1"), None);
    }

    #[test]
    fn framing_round_trips() {
        let mut buffer = Vec::new();
        write(&mut buffer, &json!({"jsonrpc": "2.0", "method": "hi"})).expect("writes");
        let text = String::from_utf8(buffer.clone()).expect("utf8");
        assert!(text.starts_with("Content-Length: "), "{text}");

        let mut reader = io::BufReader::new(&buffer[..]);
        let back = read(&mut reader).expect("reads").expect("a message");
        assert_eq!(back["method"], "hi");
    }
}
