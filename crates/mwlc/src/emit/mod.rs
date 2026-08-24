// SPDX-License-Identifier: MIT

//! MIR to a datapack on disk.
//!
//! Emitting builds the whole pack in memory first. That keeps I/O at one edge, makes
//! the output snapshot-testable without a filesystem, and means a failed compile never
//! leaves a half-written datapack in someone's world.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use crate::hir::Attr;
use crate::mir::{Cmp, Cond, Function, Inst, Mir, Op, Reg, RegKind};
use crate::syntax::lexer::Span;

/// The datapack layout Minecraft 1.21+ expects. `function` is singular; it was
/// `functions` before 1.21.
const FUNCTION_DIR: &str = "function";

/// A placeholder until toolchains supply the real one (M6). 48 is 1.21 and 1.21.1.
pub const PLACEHOLDER_PACK_FORMAT: u32 = 48;

/// The generated function that creates the objectives everything else needs. It has no
/// parent to sit under, so it takes a name no source function can produce.
pub const INIT_FUNCTION: &str = "__init";

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

    let mut namespace = "minecraft";
    for function in &mir.functions {
        let (ns, path) = split_id(&function.path);
        namespace = ns;
        files.insert(
            format!("data/{ns}/{FUNCTION_DIR}/{path}.mcfunction"),
            function_body(function, options, ns),
        );
    }

    // Objectives have to exist before anything touches them; vanilla rejects the
    // command outright otherwise, and the failure is easy to mistake for a compiler
    // bug. Creating them is therefore not optional and not the author's job.
    files.insert(
        format!("data/{namespace}/{FUNCTION_DIR}/{INIT_FUNCTION}.mcfunction"),
        init_body(namespace),
    );

    let load = std::iter::once(format!("{namespace}:{INIT_FUNCTION}"))
        .chain(tagged(mir, &Attr::Load))
        .collect::<Vec<_>>();
    files.insert(
        format!("data/minecraft/tags/{FUNCTION_DIR}/load.json"),
        function_tag(&load),
    );

    let tick = tagged(mir, &Attr::Tick).collect::<Vec<_>>();
    if !tick.is_empty() {
        files.insert(
            format!("data/minecraft/tags/{FUNCTION_DIR}/tick.json"),
            function_tag(&tick),
        );
    }

    Datapack { files }
}

fn tagged<'a>(mir: &'a Mir, attr: &'a Attr) -> impl Iterator<Item = String> + 'a {
    mir.functions
        .iter()
        .filter(move |f| f.attrs.contains(attr))
        .map(|f| f.path.clone())
}

fn init_body(namespace: &str) -> String {
    format!(
        "scoreboard objectives add {namespace}.v dummy\nscoreboard objectives add {namespace}.t dummy\n"
    )
}

fn function_tag(values: &[String]) -> String {
    let entries = values
        .iter()
        .map(|value| format!("    \"{value}\""))
        .collect::<Vec<_>>()
        .join(",\n");
    format!("{{\n  \"values\": [\n{entries}\n  ]\n}}\n")
}

fn objective(namespace: &str, reg: &Reg) -> String {
    match reg.kind {
        RegKind::Var => format!("{namespace}.v"),
        RegKind::Temp => format!("{namespace}.t"),
    }
}

fn pack_mcmeta(options: &Options) -> String {
    let format = options.pack_format;
    let description = escape_json(&options.description);
    format!(
        "{{\n  \"pack\": {{\n    \"pack_format\": {format},\n    \"description\": \"{description}\"\n  }}\n}}\n"
    )
}

fn function_body(function: &Function, options: &Options, namespace: &str) -> String {
    let mut out = String::new();
    for block in &function.blocks {
        for inst in &block.insts {
            if let Inst::Raw { span, .. } = inst
                && let Some(comment) = source_comment(options, *span)
            {
                out.push_str(&comment);
            }
            out.push_str(&command(inst, namespace));
            out.push('\n');
        }
    }
    out
}

/// One instruction, one command. This is what makes a function's cost countable
/// before it is written out.
fn command(inst: &Inst, ns: &str) -> String {
    match inst {
        Inst::Raw { text, .. } => text.clone(),
        Inst::Const { dst, value } => {
            let (holder, obj) = (&dst.holder, objective(ns, dst));
            format!("scoreboard players set {holder} {obj} {value}")
        }
        Inst::AddConst { dst, value } => {
            let (holder, obj) = (&dst.holder, objective(ns, dst));
            // `remove` rather than `add` with a minus sign: vanilla has both, and
            // `add -2147483648` would not round-trip.
            match value.is_negative() {
                true => format!(
                    "scoreboard players remove {holder} {obj} {}",
                    value.unsigned_abs()
                ),
                false => format!("scoreboard players add {holder} {obj} {value}"),
            }
        }
        Inst::Op { dst, op, src } => {
            let (d, dobj) = (&dst.holder, objective(ns, dst));
            let (s, sobj) = (&src.holder, objective(ns, src));
            let op = match op {
                Op::Assign => "=",
                Op::Add => "+=",
                Op::Sub => "-=",
                Op::Mul => "*=",
                Op::Div => "/=",
                Op::Rem => "%=",
                Op::Min => "<",
                Op::Max => ">",
            };
            format!("scoreboard players operation {d} {dobj} {op} {s} {sobj}")
        }
        Inst::Cmp {
            dst,
            cmp,
            negated,
            lhs,
            rhs,
        } => {
            let (d, dobj) = (&dst.holder, objective(ns, dst));
            let (l, lobj) = (&lhs.holder, objective(ns, lhs));
            let (r, robj) = (&rhs.holder, objective(ns, rhs));
            let keyword = if *negated { "unless" } else { "if" };
            let cmp = match cmp {
                Cmp::Lt => "<",
                Cmp::Le => "<=",
                Cmp::Eq => "=",
                Cmp::Ge => ">=",
                Cmp::Gt => ">",
            };
            format!(
                "execute store success score {d} {dobj} {keyword} score {l} {lobj} {cmp} {r} {robj}"
            )
        }
        Inst::Matches {
            dst,
            src,
            min,
            max,
            negated,
        } => {
            let (d, dobj) = (&dst.holder, objective(ns, dst));
            let (s, sobj) = (&src.holder, objective(ns, src));
            let keyword = if *negated { "unless" } else { "if" };
            format!(
                "execute store success score {d} {dobj} {keyword} score {s} {sobj} matches {}",
                range(*min, *max)
            )
        }
        Inst::Call { path } => format!("function {path}"),
        Inst::Return { value } => format!("return {value}"),
        Inst::Guarded { cond, inst } => {
            format!("execute {} run {}", condition(cond, ns), command(inst, ns))
        }
    }
}

/// `if score $a obj matches 1..`, ready to follow an `execute`.
fn condition(cond: &Cond, ns: &str) -> String {
    match cond {
        Cond::Score {
            lhs,
            cmp,
            rhs,
            negated,
        } => {
            let keyword = if *negated { "unless" } else { "if" };
            let (l, lobj) = (&lhs.holder, objective(ns, lhs));
            let (r, robj) = (&rhs.holder, objective(ns, rhs));
            let cmp = match cmp {
                Cmp::Lt => "<",
                Cmp::Le => "<=",
                Cmp::Eq => "=",
                Cmp::Ge => ">=",
                Cmp::Gt => ">",
            };
            format!("{keyword} score {l} {lobj} {cmp} {r} {robj}")
        }
        Cond::Matches {
            src,
            min,
            max,
            negated,
        } => {
            let keyword = if *negated { "unless" } else { "if" };
            let (s, sobj) = (&src.holder, objective(ns, src));
            format!("{keyword} score {s} {sobj} matches {}", range(*min, *max))
        }
    }
}

/// `5`, `1..`, `..5`, `1..5` — the form vanilla accepts.
fn range(min: Option<i32>, max: Option<i32>) -> String {
    match (min, max) {
        (Some(a), Some(b)) if a == b => a.to_string(),
        (Some(a), Some(b)) => format!("{a}..{b}"),
        (Some(a), None) => format!("{a}.."),
        (None, Some(b)) => format!("..{b}"),
        (None, None) => "..".to_owned(),
    }
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
            vec![
                "data/minecraft/tags/function/load.json",
                "data/myns/function/__init.mcfunction",
                "data/myns/function/main.mcfunction",
                "pack.mcmeta",
            ]
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
    fn the_objectives_are_created_by_a_generated_load_function() {
        // Nothing works if these do not exist, and vanilla rejects the command rather
        // than failing quietly, so creating them cannot be left to the author.
        let pack = compile("fn main() {}", &release());
        let init = &pack.files["data/myns/function/__init.mcfunction"];
        assert!(
            init.contains("scoreboard objectives add myns.v dummy"),
            "{init}"
        );
        assert!(
            init.contains("scoreboard objectives add myns.t dummy"),
            "{init}"
        );
        assert!(
            pack.files["data/minecraft/tags/function/load.json"].contains("myns:__init"),
            "the init function has to be in the load tag"
        );
    }

    #[test]
    fn tick_and_load_attributes_reach_the_function_tags() {
        let pack = compile("#[tick] fn t() {} #[load] fn l() {}", &release());
        assert!(pack.files["data/minecraft/tags/function/tick.json"].contains("myns:t"));
        assert!(pack.files["data/minecraft/tags/function/load.json"].contains("myns:l"));
    }

    #[test]
    fn there_is_no_tick_tag_when_nothing_wants_one() {
        let pack = compile("fn main() {}", &release());
        assert!(
            !pack
                .files
                .contains_key("data/minecraft/tags/function/tick.json")
        );
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
