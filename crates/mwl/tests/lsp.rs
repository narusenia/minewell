// SPDX-License-Identifier: MIT

//! The language server, driven the way an editor drives it.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn framed(message: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{message}", message.len())
}

/// Reads one framed message off the server's stdout.
fn read(out: &mut impl BufRead) -> String {
    let mut length = 0;
    loop {
        let mut line = String::new();
        out.read_line(&mut line).expect("a header");
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse().expect("a number");
        }
    }
    let mut body = vec![0u8; length];
    out.read_exact(&mut body).expect("a body");
    String::from_utf8(body).expect("utf8")
}

/// Feeds the server a document and returns the initialize reply and what it published.
fn opened(text: &str) -> (String, String) {
    let mut server = Command::new(env!("CARGO_BIN_EXE_mwl"))
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the server starts");

    let document = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": "file:///tmp/x.mwl", "text": text } },
    })
    .to_string();

    let mut input = server.stdin.take().expect("stdin");
    for message in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        &document,
        r#"{"jsonrpc":"2.0","method":"exit"}"#,
    ] {
        input.write_all(framed(message).as_bytes()).expect("write");
    }
    drop(input);

    let mut out = BufReader::new(server.stdout.take().expect("stdout"));
    let initialised = read(&mut out);
    let published = read(&mut out);
    server.wait().expect("the server exits");
    (initialised, published)
}

/// The line and UTF-16 column of a byte offset, the way an editor would send it.
fn spot(text: &str, offset: usize) -> serde_json::Value {
    let before = &text[..offset];
    let line = before.matches('\n').count();
    let column = before.rsplit_once('\n').map_or(before, |(_, rest)| rest);
    serde_json::json!({ "line": line, "character": column.encode_utf16().count() })
}

/// Opens the arena example, which has a `minewell.toml` naming a real toolchain, and
/// asks about a position in it. The toolchain lives in `examples/`, passed to the
/// child rather than set in this process.
fn asked(offset_of: &str, nudge: usize) -> Asked {
    let examples = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    let file = format!("{examples}/arena/src/lib.mwl");
    let text = std::fs::read_to_string(&file).expect("the example is there");
    let at = spot(
        &text,
        text.find(offset_of).expect("the example says it") + nudge,
    );

    let mut server = Command::new(env!("CARGO_BIN_EXE_mwl"))
        .arg("lsp")
        .env("MINEWELL_HOME", examples)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the server starts");

    let uri = format!("file://{file}");
    let document = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": &uri, "text": text } },
    });
    let position = serde_json::json!({ "textDocument": { "uri": &uri }, "position": at });
    let completion = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/completion", "params": &position,
    });
    let hover = serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "textDocument/hover", "params": &position,
    });
    let hints = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "textDocument/inlayHint",
        "params": {
            "textDocument": { "uri": &uri },
            "range": { "start": { "line": 0, "character": 0 }, "end": spot(&text, text.len()) },
        },
    });

    let mut input = server.stdin.take().expect("stdin");
    for message in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        &document.to_string(),
        &completion.to_string(),
        &hover.to_string(),
        &hints.to_string(),
        r#"{"jsonrpc":"2.0","method":"exit"}"#,
    ] {
        input.write_all(framed(message).as_bytes()).expect("write");
    }
    drop(input);

    let mut out = BufReader::new(server.stdout.take().expect("stdout"));
    read(&mut out); // initialize
    read(&mut out); // the diagnostics for the document
    let completed = read(&mut out);
    let hovered = read(&mut out);
    let hinted = read(&mut out);
    server.wait().expect("the server exits");
    Asked {
        completed,
        hovered,
        hinted,
    }
}

struct Asked {
    completed: String,
    hovered: String,
    hinted: String,
}

#[test]
fn a_command_is_completed_from_the_toolchain() {
    // Halfway through the word: an editor asks with the cursor inside it.
    let completed = asked("play_sound(minecraft:block", 4).completed;
    assert!(completed.contains(r#""label":"play_sound""#), "{completed}");
    // Not everything the table has — the prefix narrows it.
    assert!(!completed.contains(r#""label":"setblock""#), "{completed}");
}

#[test]
fn hovering_a_command_spells_it_the_way_vanilla_wants() {
    let hovered = asked("play_sound(minecraft:block", 4).hovered;
    assert!(
        hovered.contains("playsound <sound> master <targets>"),
        "{hovered}"
    );
    // And how it is called from here, since the order is not the same.
    assert!(hovered.contains("play_sound("), "{hovered}");
}

#[test]
fn hovering_something_that_is_not_a_name_or_a_command_says_nothing() {
    // The `fn` keyword: not a binding, not a command.
    let hovered = asked("fn tick()", 1).hovered;
    assert!(hovered.contains(r#""result":null"#), "{hovered}");
}

#[test]
fn a_name_in_scope_is_completed_and_a_name_out_of_it_is_not() {
    // Inside `setup`, where `arena` is bound and `size` follows it.
    let completed = asked("arena.radius", 3).completed;
    assert!(completed.contains(r#""label":"arena""#), "{completed}");
    // The function is in scope everywhere, and its name shares the prefix.
    assert!(completed.contains(r#""label":"area""#), "{completed}");
    // `seen` is a binding in `tick`, a different function.
    assert!(!completed.contains(r#""label":"seen""#), "{completed}");
}

#[test]
fn hovering_a_binding_gives_its_type() {
    let hovered = asked("arena.radius", 3).hovered;
    assert!(hovered.contains("arena: Arena"), "{hovered}");
}

#[test]
fn hovering_a_function_gives_its_signature() {
    let hovered = asked("area(fix::<1000>(arena", 2).hovered;
    assert!(
        hovered.contains("fn area(r: fix<1000>) -> fix<1000>"),
        "{hovered}"
    );
}

#[test]
fn an_inferred_binding_gets_an_inlay_hint_and_a_written_one_does_not() {
    let hinted = asked("fn tick()", 3).hinted;
    // `let size = area(...)` says nothing about its own type.
    assert!(hinted.contains(r#""label":": fix<1000>""#), "{hinted}");
    // `let arena: Arena = ...` already says it; repeating it is noise.
    assert!(!hinted.contains(r#""label":": Arena""#), "{hinted}");
}

#[test]
fn a_broken_document_comes_back_as_a_diagnostic() {
    let (initialised, published) = opened("fn main() { let = 1; }");
    assert!(initialised.contains("textDocumentSync"), "{initialised}");
    assert!(published.contains("publishDiagnostics"), "{published}");
    assert!(published.contains("expected a name"), "{published}");
    // Where, not just what: an editor puts the squiggle on the range.
    assert!(published.contains(r#""line":0"#), "{published}");
}

#[test]
fn a_document_that_compiles_comes_back_empty() {
    let (_, published) = opened("fn main() { let a = 1; }");
    assert!(published.contains(r#""diagnostics":[]"#), "{published}");
}
