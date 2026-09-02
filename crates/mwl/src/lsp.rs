// SPDX-License-Identifier: MIT

//! A language server, as small as one can be and still be worth running.
//!
//! Diagnostics, plus completion and hover. `mwlc` already answers
//! with problems and spans, so the diagnostics half is a loop: read a document, compile
//! it, write the problems back. The other half is the toolchain's command table
//! reformatted — which is the part nobody can hold in their head, and it costs the
//! compiler nothing. The names come from `driver::symbols`, which lowers the file and
//! answers with what it declared — best effort, because a file being typed into does
//! not compile. Definition jumps come after.
//!
//! The wire format is `Content-Length` framing around JSON-RPC, which is little enough
//! to write out. A compiler is a strange place to grow an async runtime, and the same
//! reasoning kept a HTTP client out of `mwl toolchain install`.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use mwlc::driver;
use mwlc::emit::{Options, Profile, Source};
use mwlc::hir::{Symbol, SymbolKind};
use mwlc::schema::{Part, Schema, Signature};
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
                                "completionProvider": {},
                                "hoverProvider": true,
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
            "textDocument/didClose" => {
                if let Some(uri) = params["textDocument"]["uri"].as_str() {
                    server.documents.remove(uri);
                }
            }
            "textDocument/completion" => {
                let completions = server.complete(&params);
                write(&mut output, &reply(id, completions))?;
            }
            "textDocument/hover" => {
                let hover = server.hover(&params);
                write(&mut output, &reply(id, hover))?;
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
    /// The text of every open document. Completion and hover ask about a position, and
    /// a position means nothing without the text it points into.
    documents: HashMap<String, String>,
}

impl Server {
    fn publish(&mut self, out: &mut impl Write, uri: &Value, text: &Value) -> io::Result<()> {
        let (Some(uri), Some(text)) = (uri.as_str(), text.as_str()) else {
            return Ok(());
        };
        self.documents.insert(uri.to_owned(), text.to_owned());
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

    /// The document a request points into, and where in it.
    fn spot(&self, params: &Value) -> Option<(Option<PathBuf>, String, usize)> {
        let uri = params["textDocument"]["uri"].as_str()?;
        // Cloned because answering needs the command table too, and that is behind
        // `&mut self`. A `.mwl` file is small.
        let text = self.documents.get(uri)?.clone();
        let at = offset(&text, &params["position"]);
        Some((path_of(uri), text, at))
    }

    /// The names in scope and the commands, narrowed to what has been typed.
    ///
    /// A client filters again on its own, but narrowing here is what makes the answer
    /// worth reading: the command table alone holds a few hundred entries.
    fn complete(&mut self, params: &Value) -> Value {
        let Some((path, text, at)) = self.spot(params) else {
            return json!([]);
        };
        let (start, _) = word(&text, at);
        let prefix = text[start..at].to_owned();
        let (namespace, schema) = self.project(path.as_deref());

        let mut items: Vec<Value> = visible(&text, &namespace, schema.as_ref(), at)
            .filter(|symbol| symbol.name.starts_with(&prefix))
            .map(|symbol| {
                json!({
                    "label": symbol.name,
                    // A function or a variable, as an editor counts them.
                    "kind": if symbol.kind == SymbolKind::Function { 3 } else { 6 },
                    "detail": symbol.ty,
                })
            })
            .collect();
        if let Some(schema) = &schema {
            items.extend(
                schema
                    .commands
                    .values()
                    .filter(|signature| signature.name.starts_with(&prefix))
                    .map(|signature| {
                        json!({
                            "label": signature.name,
                            "kind": 3,
                            "detail": spelling(signature),
                        })
                    }),
            );
        }
        json!(items)
    }

    /// What the word under the cursor is: a name the file declares, or a command.
    ///
    /// Names first. A binding that shares a command's name shadows it here as it does
    /// everywhere else.
    fn hover(&mut self, params: &Value) -> Value {
        let Some((path, text, at)) = self.spot(params) else {
            return Value::Null;
        };
        let (start, end) = word(&text, at);
        if start == end {
            return Value::Null;
        }
        let (namespace, schema) = self.project(path.as_deref());
        let word = &text[start..end];
        let range = json!({ "start": position(&text, start), "end": position(&text, end) });

        let named = visible(&text, &namespace, schema.as_ref(), at)
            .find(|symbol| symbol.name == word)
            .map(|symbol| match symbol.kind {
                SymbolKind::Function => symbol.ty.clone(),
                _ => format!("{}: {}", symbol.name, symbol.ty),
            });
        let value = match (named, schema.as_ref().and_then(|schema| schema.get(word))) {
            (Some(named), _) => format!("```mwl\n{named}\n```"),
            (None, Some(signature)) => describe(signature),
            (None, None) => return Value::Null,
        };
        json!({ "contents": { "kind": "markdown", "value": value }, "range": range })
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

/// The names that can be used at an offset, innermost first.
///
/// Declared before the cursor and still in scope at it. The scope of a binding is the
/// block it was declared in, which is what makes a binding from another function — or
/// from a block that has already closed — stay out of the list.
fn visible<'a>(
    text: &'a str,
    namespace: &str,
    schema: Option<&Schema>,
    at: usize,
) -> impl Iterator<Item = Symbol> + 'a {
    let mut found = driver::symbols(text, namespace, schema);
    // Innermost first: a shadowing `let` is the one that answers for the name.
    found.sort_by_key(|symbol| std::cmp::Reverse(symbol.scope.start));
    found.into_iter().filter(move |symbol| {
        symbol.scope.start <= at && at <= symbol.scope.end && symbol.span.start <= at
    })
}

/// How the command is called from minewell, and how vanilla wants it written.
///
/// Both, because they do not agree: the name is the literal path joined up, the
/// arguments come in the order the tree puts them, and a literal can sit between two
/// of them.
fn describe(signature: &Signature) -> String {
    let params: Vec<String> = signature
        .params
        .iter()
        .map(|param| format!("{}: {}", param.name, param.ty.name()))
        .collect();
    format!(
        "```mwl\n{}({})\n```\n---\n```mcfunction\n{}\n```",
        signature.name,
        params.join(", "),
        spelling(signature)
    )
}

/// `playsound <sound> master <targets>`.
fn spelling(signature: &Signature) -> String {
    signature
        .parts
        .iter()
        .map(|part| match part {
            Part::Literal(word) => word.clone(),
            Part::Arg(index) => format!("<{}>", signature.params[*index].name),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// An LSP position as a byte offset: the inverse of `position`, counting the character
/// as UTF-16 code units for the same reason.
fn offset(text: &str, position: &Value) -> usize {
    let line = position["line"].as_u64().unwrap_or(0) as usize;
    let character = position["character"].as_u64().unwrap_or(0) as usize;
    let mut start = 0;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(index) => start += index + 1,
            None => return text.len(),
        }
    }
    let mut units = 0;
    for (index, c) in text[start..].char_indices() {
        if units >= character || c == '\n' {
            return start + index;
        }
        units += c.len_utf16();
    }
    text.len()
}

/// The byte range of the word the offset sits in.
///
/// Lexical, and that is enough: a command name is one identifier, and asking the
/// compiler where the cursor is would need spans HIR does not carry yet.
fn word(text: &str, offset: usize) -> (usize, usize) {
    let part = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let offset = offset.min(text.len());
    let start = text[..offset]
        .char_indices()
        .rev()
        .find(|(_, c)| !part(*c))
        .map_or(0, |(index, c)| index + c.len_utf8());
    let end = text[offset..]
        .find(|c| !part(c))
        .map_or(text.len(), |index| offset + index);
    (start, end)
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
    fn an_offset_undoes_a_position() {
        let text = "fn main() {\n    let x = \"é\" + 1;\n}";
        let at = text.find('+').expect("there is one");
        assert_eq!(offset(text, &position(text, at)), at);
        // Past the end of a line stops at the newline rather than running on.
        assert_eq!(
            offset(text, &json!({ "line": 0, "character": 99 })),
            text.find('\n').expect("there is one")
        );
    }

    #[test]
    fn a_word_is_the_identifier_the_cursor_is_in() {
        let text = "    play_sound(minecraft:stone, @a);";
        let at = text.find("sound").expect("there");
        let (start, end) = word(text, at);
        assert_eq!(&text[start..end], "play_sound");
        // Not in a word at all: an empty range, and so an empty prefix.
        let space = word(text, 0);
        assert_eq!(space.0, space.1);
    }

    #[test]
    fn a_command_is_spelled_in_the_order_the_tree_gives() {
        // A literal after an argument, which is the case the two name lists lose.
        let schema = Schema::parse(
            r#"{"type":"root","children":{"playsound":{"type":"literal","children":{
                "sound":{"type":"argument","parser":"minecraft:resource_location","children":{
                "master":{"type":"literal","children":{
                "targets":{"type":"argument","parser":"minecraft:entity","executable":true}}}}}}}}}"#,
        )
        .expect("it parses");
        let signature = schema.get("playsound_master").expect("present");
        assert_eq!(spelling(signature), "playsound <sound> master <targets>");
        let described = describe(signature);
        assert!(
            described.contains("playsound_master(sound: ResourceLocation, targets: selector)"),
            "{described}"
        );
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
