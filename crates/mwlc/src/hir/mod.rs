//! The high-level intermediate representation: names resolved, macros identified.
//!
//! HIR is where "what did the author mean" is settled. Type checking and
//! monomorphisation join it in M2 and M7; today it resolves function names to datapack
//! paths, turns built-in macro calls into the thing they mean, and rejects names it
//! does not know.
//!
//! Rejecting unknown attributes matters more than it looks. A misspelled `#[tik]` that
//! is quietly ignored is exactly the class of silent failure minewell exists to
//! remove, and it would be indistinguishable from the feature not working.

use crate::syntax::SyntaxError;
use crate::syntax::ast::{self, Expr, ItemKind, SourceFile};
use crate::syntax::lexer::{Span, TokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FnId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hir {
    pub functions: Vec<Function>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub id: FnId,
    pub name: String,
    /// Where this lands in the datapack: `<namespace>:<path>`.
    pub path: String,
    pub attrs: Vec<Attr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Raw(RawCommand),
}

/// A `raw!` command. Interpolation arrives in M9; today the text is literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCommand {
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attr {
    Tick,
    Load,
    Inline,
    NoInline,
}

impl Attr {
    fn parse(name: &str) -> Option<Attr> {
        Some(match name {
            "tick" => Attr::Tick,
            "load" => Attr::Load,
            "inline" => Attr::Inline,
            "no_inline" => Attr::NoInline,
            _ => return None,
        })
    }
}

/// Attributes the language will have but does not act on yet. Named so that the
/// diagnostic for one can say "not implemented" rather than "unknown", which is a
/// different problem for the author.
const PLANNED_ATTRS: &[&str] = &["ctx", "score", "storage", "nbt", "unroll", "derive"];

pub fn lower(file: &SourceFile, namespace: &str) -> (Hir, Vec<SyntaxError>) {
    let mut cx = Lowering {
        namespace,
        errors: Vec::new(),
    };
    let mut functions: Vec<Function> = Vec::new();
    for item in &file.items {
        let ItemKind::Fn(f) = &item.kind;
        if let Some(previous) = functions
            .iter()
            .find(|existing| existing.name == f.name.name)
        {
            let name = &f.name.name;
            let line = previous.span.start;
            cx.error(
                f.name.span,
                format!("a function named '{name}' is already defined (at byte {line})"),
            );
            continue;
        }
        let id = FnId(functions.len() as u32);
        functions.push(Function {
            id,
            name: f.name.name.clone(),
            path: format!("{namespace}:{}", f.name.name),
            attrs: cx.attrs(&item.attrs),
            body: cx.body(&f.body),
            span: item.span,
        });
    }
    (Hir { functions }, cx.errors)
}

struct Lowering<'a> {
    #[allow(dead_code, reason = "used once module paths exist, in M7")]
    namespace: &'a str,
    errors: Vec<SyntaxError>,
}

impl Lowering<'_> {
    fn attrs(&mut self, attrs: &[ast::Attribute]) -> Vec<Attr> {
        attrs
            .iter()
            .filter_map(|attr| {
                let Some(TokenKind::Ident(name)) = attr.tokens.first().map(|t| &t.kind) else {
                    self.error(attr.span, "expected an attribute name");
                    return None;
                };
                match Attr::parse(name) {
                    Some(attr) => Some(attr),
                    None if PLANNED_ATTRS.contains(&name.as_str()) => {
                        self.error(
                            attr.span,
                            format!("'{name}' is planned but not implemented yet"),
                        );
                        None
                    }
                    None => {
                        self.error(attr.span, format!("unknown attribute '{name}'"));
                        None
                    }
                }
            })
            .collect()
    }

    fn body(&mut self, block: &ast::Block) -> Vec<Stmt> {
        block
            .stmts
            .iter()
            .filter_map(|stmt| {
                let ast::Stmt::Expr(Expr::Macro(call)) = stmt;
                match call.name.name.as_str() {
                    "raw" => self.raw(call).map(Stmt::Raw),
                    other => {
                        self.error(call.span, format!("unknown macro '{other}!'"));
                        None
                    }
                }
            })
            .collect()
    }

    fn raw(&mut self, call: &ast::MacroCall) -> Option<RawCommand> {
        match call.tokens.as_slice() {
            [token] => match &token.kind {
                TokenKind::Str(text) => Some(RawCommand {
                    text: text.clone(),
                    span: call.span,
                }),
                _ => {
                    self.error(token.span, "raw! takes a string literal");
                    None
                }
            },
            [] => {
                self.error(call.span, "raw! takes a string literal");
                None
            }
            tokens => {
                let span = tokens[1].span;
                self.error(span, "raw! takes a single string literal");
                None
            }
        }
    }

    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(SyntaxError::new(span, message));
    }
}

// SPDX-License-Identifier: MIT

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parser::parse;

    fn lower_ok(src: &str) -> Hir {
        let (file, errors) = parse(src);
        assert!(errors.is_empty(), "{errors:?}");
        let (hir, errors) = lower(&file, "myns");
        assert!(errors.is_empty(), "{errors:?}");
        hir
    }

    fn lower_err(src: &str) -> Vec<SyntaxError> {
        let (file, errors) = parse(src);
        assert!(errors.is_empty(), "{errors:?}");
        lower(&file, "myns").1
    }

    #[test]
    fn a_function_gets_an_id_and_a_datapack_path() {
        let hir = lower_ok("fn main() {}");
        assert_eq!(hir.functions.len(), 1);
        assert_eq!(hir.functions[0].id, FnId(0));
        assert_eq!(hir.functions[0].name, "main");
        assert_eq!(hir.functions[0].path, "myns:main");
    }

    #[test]
    fn ids_are_assigned_in_source_order() {
        let hir = lower_ok("fn a() {} fn b() {}");
        assert_eq!(
            hir.functions.iter().map(|f| f.id).collect::<Vec<_>>(),
            vec![FnId(0), FnId(1)]
        );
    }

    #[test]
    fn raw_carries_the_command_text() {
        let hir = lower_ok(r#"fn main() { raw!("say hi"); }"#);
        let Stmt::Raw(raw) = &hir.functions[0].body[0];
        assert_eq!(raw.text, "say hi");
    }

    #[test]
    fn an_unknown_macro_is_reported() {
        let errors = lower_err("fn main() { nope!(1); }");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("nope"), "{errors:?}");
    }

    #[test]
    fn raw_wants_exactly_one_string() {
        assert!(!lower_err("fn main() { raw!(1); }").is_empty());
        assert!(!lower_err(r#"fn main() { raw!("a", "b"); }"#).is_empty());
        assert!(!lower_err("fn main() { raw!(); }").is_empty());
    }

    #[test]
    fn two_functions_with_one_name_collide() {
        let errors = lower_err("fn main() {} fn main() {}");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("main"), "{errors:?}");
    }

    #[test]
    fn known_attributes_are_recorded() {
        let hir = lower_ok("#[tick] fn main() {}");
        assert_eq!(hir.functions[0].attrs, vec![Attr::Tick]);
    }

    #[test]
    fn an_unknown_attribute_is_an_error_rather_than_being_ignored() {
        // Silently ignoring a typo'd attribute is the failure mode minewell exists to
        // remove. `#[tik]` must not quietly do nothing.
        let errors = lower_err("#[tik] fn main() {}");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("tik"), "{errors:?}");
    }

    #[test]
    fn lowering_reports_every_problem_it_finds() {
        let errors = lower_err("#[tik] fn main() { nope!(); }");
        assert_eq!(errors.len(), 2);
    }
}
