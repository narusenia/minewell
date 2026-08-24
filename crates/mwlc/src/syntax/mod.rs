// SPDX-License-Identifier: MIT

//! Source text to AST.

pub mod ast;
pub mod lexer;
pub mod parser;

pub use lexer::Span;

/// Something wrong with the source, at a place in it.
///
/// Lexing and parsing both collect these rather than stopping, so one mistake does not
/// hide the rest of the file. M1-4 renders them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    pub span: Span,
    pub message: String,
}

impl SyntaxError {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        SyntaxError {
            span,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.span.start)
    }
}

impl std::error::Error for SyntaxError {}
