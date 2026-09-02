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
