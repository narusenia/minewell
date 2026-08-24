// SPDX-License-Identifier: MIT

//! The mid-level intermediate representation: basic blocks and, from M2, virtual
//! registers.
//!
//! MIR is close enough to mcfunction that emitting is mechanical, and abstract enough
//! that register allocation and the inline-or-split decision (`docs/01-requirements.md`
//! section 7) can be made here rather than in the emitter.
//!
//! Today every function is one block. Control flow arrives in M3, which is when the
//! block graph starts earning its name.

use crate::hir::{self, FnId, Hir};
use crate::syntax::lexer::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mir {
    pub functions: Vec<Function>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub id: FnId,
    /// The datapack id this block set is written to.
    pub path: String,
    pub attrs: Vec<hir::Attr>,
    pub blocks: Vec<Block>,
}

impl Function {
    /// The block a call to this function enters.
    pub fn entry(&self) -> &Block {
        &self.blocks[0]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub id: BlockId,
    pub insts: Vec<Inst>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inst {
    /// A command to emit as written. Everything is one of these until M2.
    Raw { text: String, span: Span },
}

pub fn lower(hir: &Hir) -> Mir {
    Mir {
        functions: hir
            .functions
            .iter()
            .map(|f| Function {
                id: f.id,
                path: f.path.clone(),
                attrs: f.attrs.clone(),
                blocks: vec![Block {
                    id: BlockId(0),
                    insts: f
                        .body
                        .iter()
                        .map(|stmt| match stmt {
                            hir::Stmt::Raw(raw) => Inst::Raw {
                                text: raw.text.clone(),
                                span: raw.span,
                            },
                        })
                        .collect(),
                }],
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parser::parse;

    fn mir(src: &str) -> Mir {
        let (file, errors) = parse(src);
        assert!(errors.is_empty(), "{errors:?}");
        let (hir, errors) = crate::hir::lower(&file, "myns");
        assert!(errors.is_empty(), "{errors:?}");
        lower(&hir)
    }

    #[test]
    fn one_raw_becomes_one_instruction() {
        let mir = mir(r#"fn main() { raw!("say hi"); }"#);
        assert_eq!(mir.functions.len(), 1);
        assert_eq!(mir.functions[0].blocks.len(), 1);
        assert_eq!(
            mir.functions[0].entry().insts.len(),
            1,
            "{:?}",
            mir.functions[0].entry()
        );
    }

    #[test]
    fn instructions_keep_their_span_for_debug_output() {
        let src = r#"fn main() { raw!("say hi"); }"#;
        let mir = mir(src);
        let Inst::Raw { span, .. } = mir.functions[0].entry().insts[0];
        assert_eq!(&src[span.range()], r#"raw!("say hi")"#);
    }

    #[test]
    fn an_empty_function_is_still_one_block() {
        let mir = mir("fn main() {}");
        assert_eq!(mir.functions[0].blocks.len(), 1);
        assert!(mir.functions[0].entry().insts.is_empty());
    }

    #[test]
    fn the_datapack_path_comes_through() {
        let mir = mir("fn main() {}");
        assert_eq!(mir.functions[0].path, "myns:main");
    }
}
