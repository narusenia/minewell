// SPDX-License-Identifier: MIT

//! MIR to a datapack on disk.
//!
//! Emitting builds the whole pack in memory first. That keeps I/O at one edge, makes
//! the output snapshot-testable without a filesystem, and means a failed compile never
//! leaves a half-written datapack in someone's world.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use crate::mir::{Function, Inst, Mir};
use crate::syntax::lexer::Span;

/// The datapack layout Minecraft 1.21+ expects. `function` is singular; it was
/// `functions` before 1.21.
const FUNCTION_DIR: &str = "function";

/// A placeholder until toolchains supply the real one (M6). 48 is 1.21 and 1.21.1.
pub const PLACEHOLDER_PACK_FORMAT: u32 = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// Source line comments, assertions kept, no optimisation. What you develop with.
    #[default]
    Debug,
    Release,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub pack_format: u32,
    pub description: String,
    pub profile: Profile,
    /// The source a debug build quotes line numbers from.
    pub source: Option<Source>,
}

#[derive(Debug, Clone)]
pub struct Source {
    pub path: String,
    pub text: String,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            pack_format: PLACEHOLDER_PACK_FORMAT,
            description: String::new(),
            profile: Profile::default(),
            source: None,
        }
    }
}

/// A datapack held in memory: relative path to contents.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Datapack {
    pub files: BTreeMap<String, String>,
}

impl Datapack {
    pub fn write_to(&self, root: &Path) -> io::Result<()> {
        for (path, contents) in &self.files {
            let path = root.join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, contents)?;
        }
        Ok(())
    }
}

pub fn emit(mir: &Mir, options: &Options) -> Datapack {
    let mut files = BTreeMap::new();
    files.insert("pack.mcmeta".to_owned(), pack_mcmeta(options));
    for function in &mir.functions {
        let (namespace, path) = split_id(&function.path);
        files.insert(
            format!("data/{namespace}/{FUNCTION_DIR}/{path}.mcfunction"),
            function_body(function, options),
        );
    }
    Datapack { files }
}

fn pack_mcmeta(options: &Options) -> String {
    let format = options.pack_format;
    let description = escape_json(&options.description);
    format!(
        "{{\n  \"pack\": {{\n    \"pack_format\": {format},\n    \"description\": \"{description}\"\n  }}\n}}\n"
    )
}

fn function_body(function: &Function, options: &Options) -> String {
    let mut out = String::new();
    for block in &function.blocks {
        for inst in &block.insts {
            match inst {
                Inst::Raw { text, span } => {
                    if let Some(comment) = source_comment(options, *span) {
                        out.push_str(&comment);
                    }
                    out.push_str(text);
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// `# src/main.mwl:12`, in debug builds only. Requirements section 15: the generated
/// output has to be traceable back to the line that produced it.
fn source_comment(options: &Options, span: Span) -> Option<String> {
    if options.profile != Profile::Debug {
        return None;
    }
    let source = options.source.as_ref()?;
    let line = line_of(&source.text, span.start);
    Some(format!("# {}:{line}\n", source.path))
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

/// `myns:combat/apply` into its namespace and path.
fn split_id(id: &str) -> (&str, &str) {
    id.split_once(':').unwrap_or(("minecraft", id))
}

fn escape_json(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parser::parse;

    fn compile(src: &str, options: &Options) -> Datapack {
        let (file, errors) = parse(src);
        assert!(errors.is_empty(), "{errors:?}");
        let (hir, errors) = crate::hir::lower(&file, "myns");
        assert!(errors.is_empty(), "{errors:?}");
        emit(&crate::mir::lower(&hir), options)
    }

    fn release() -> Options {
        Options {
            profile: Profile::Release,
            description: "a test pack".to_owned(),
            ..Options::default()
        }
    }

    #[test]
    fn the_layout_is_what_minecraft_expects() {
        let pack = compile(r#"fn main() { raw!("say hi"); }"#, &release());
        assert_eq!(
            pack.files.keys().collect::<Vec<_>>(),
            vec!["data/myns/function/main.mcfunction", "pack.mcmeta"]
        );
    }

    #[test]
    fn the_whole_pack() {
        let pack = compile(
            r#"fn main() { raw!("say hi"); raw!("say bye"); } fn other() { raw!("say x"); }"#,
            &release(),
        );
        insta::assert_debug_snapshot!(pack.files);
    }

    #[test]
    fn a_debug_build_says_which_line_each_command_came_from() {
        let src = "fn main() {\n    raw!(\"say hi\");\n}";
        let pack = compile(
            src,
            &Options {
                source: Some(Source {
                    path: "src/main.mwl".to_owned(),
                    text: src.to_owned(),
                }),
                ..Options::default()
            },
        );
        assert_eq!(
            pack.files["data/myns/function/main.mcfunction"],
            "# src/main.mwl:2\nsay hi\n"
        );
    }

    #[test]
    fn a_release_build_does_not() {
        let src = "fn main() {\n    raw!(\"say hi\");\n}";
        let pack = compile(
            src,
            &Options {
                source: Some(Source {
                    path: "src/main.mwl".to_owned(),
                    text: src.to_owned(),
                }),
                ..release()
            },
        );
        assert_eq!(pack.files["data/myns/function/main.mcfunction"], "say hi\n");
    }

    #[test]
    fn the_description_is_escaped_into_the_metadata() {
        let pack = compile(
            "fn main() {}",
            &Options {
                description: r#"a "quoted" pack"#.to_owned(),
                ..release()
            },
        );
        assert!(
            pack.files["pack.mcmeta"].contains(r#"a \"quoted\" pack"#),
            "{}",
            pack.files["pack.mcmeta"]
        );
    }

    #[test]
    fn writing_creates_the_directories() {
        let pack = compile(r#"fn main() { raw!("say hi"); }"#, &release());
        let dir = tempfile::tempdir().expect("temp dir");
        pack.write_to(dir.path()).expect("write");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("data/myns/function/main.mcfunction")).unwrap(),
            "say hi\n"
        );
        assert!(dir.path().join("pack.mcmeta").exists());
    }

    #[test]
    fn line_numbers_count_from_one() {
        assert_eq!(line_of("a\nb\nc", 0), 1);
        assert_eq!(line_of("a\nb\nc", 2), 2);
        assert_eq!(line_of("a\nb\nc", 4), 3);
        assert_eq!(
            line_of("a", 999),
            1,
            "an offset past the end must not panic"
        );
    }
}
