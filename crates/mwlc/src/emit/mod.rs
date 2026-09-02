// SPDX-License-Identifier: MIT

//! MIR to a datapack on disk.
//!
//! Emitting builds the whole pack in memory first. That keeps I/O at one edge, makes
//! the output snapshot-testable without a filesystem, and means a failed compile never
//! leaves a half-written datapack in someone's world.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use crate::hir::{Attr, Ctx};
use crate::mir::{
    Cmp, Cond, DataRef, DataTarget, ExecuteAs, Function, Inst, Mir, Op, Reg, RegKind,
};
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
    /// What each function costs to run (requirements section 16.1). Not part of the
    /// pack — `mwl build` writes it beside it, as `target/cost.txt`.
    pub costs: Vec<crate::cost::Cost>,
    /// The `#[test]` functions, by the id a `function` command would name
    /// (spec section 3.23). `mwl test` calls each one.
    pub tests: Vec<String>,
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
    let costs = crate::cost::costs(mir);
    let mut files = BTreeMap::new();
    files.insert("pack.mcmeta".to_owned(), pack_mcmeta(options));

    let namespace = mir.namespace.as_str();
    for function in &mir.functions {
        let (ns, path) = split_id(&function.path);
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
        init_body(namespace, options.profile, &files),
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

    Datapack {
        files,
        costs,
        tests: tagged(mir, &Attr::Test).collect(),
    }
}

fn tagged<'a>(mir: &'a Mir, attr: &'a Attr) -> impl Iterator<Item = String> + 'a {
    mir.functions
        .iter()
        .filter(move |f| f.attrs.contains(attr))
        .map(|f| f.path.clone())
}

/// The objectives to create, one `scoreboard objectives add` each.
///
/// A release build creates only the ones something names (requirements section 15).
/// The test is the rendered commands rather than the MIR: a `raw!` naming an objective
/// itself is a use, and reading the text is what sees it.
fn init_body(namespace: &str, profile: Profile, bodies: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    for objective in [
        crate::names::var_objective(namespace),
        crate::names::temp_objective(namespace),
    ] {
        // A whole word, not a substring: the objective is its own argument wherever it
        // is written, and it can be the last one on a line.
        let used = profile == Profile::Debug
            || bodies
                .values()
                .any(|body| body.split_whitespace().any(|word| word == objective));
        if used {
            out.push_str(&format!("scoreboard objectives add {objective} dummy\n"));
        }
    }
    out
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
        RegKind::Var => crate::names::var_objective(namespace),
        RegKind::Temp => crate::names::temp_objective(namespace),
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
    if let Some(guard) = executor_guard(function, options) {
        out.push_str(&guard);
    }
    for block in &function.blocks {
        for inst in &block.insts {
            if let Inst::Raw { span, .. } = inst
                && let Some(comment) = source_comment(options, *span)
            {
                out.push_str(&comment);
            }
            out.push_str(&command(inst, namespace, options));
            out.push('\n');
        }
    }
    out
}

/// One instruction, one command. This is what makes a function's cost countable
/// before it is written out.
fn command(inst: &Inst, ns: &str, options: &Options) -> String {
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
        Inst::StoreResult { dst, inst } => {
            let (d, dobj) = (&dst.holder, objective(ns, dst));
            format!(
                "execute store result score {d} {dobj} run {}",
                command(inst, ns, options)
            )
        }
        Inst::Get { src } => {
            let (s, sobj) = (&src.holder, objective(ns, src));
            format!("scoreboard players get {s} {sobj}")
        }
        Inst::ReturnRun { inst } => format!("return run {}", command(inst, ns, options)),
        Inst::PushFrame => format!("data modify storage {ns}:mw mw.stack append value {{}}"),
        Inst::PopFrame => format!("data remove storage {ns}:mw mw.stack[-1]"),
        Inst::Save { reg, slot } => {
            let (r, robj) = (&reg.holder, objective(ns, reg));
            format!(
                "execute store result storage {ns}:mw mw.stack[-1].r{slot} int 1 run scoreboard players get {r} {robj}"
            )
        }
        Inst::Restore { reg, slot } => {
            let (r, robj) = (&reg.holder, objective(ns, reg));
            format!(
                "execute store result score {r} {robj} run data get storage {ns}:mw mw.stack[-1].r{slot}"
            )
        }
        Inst::SaveData { path, slot } => {
            format!(
                "data modify storage {ns}:mw mw.stack[-1].r{slot} set from storage {ns}:mw {path}"
            )
        }
        Inst::RestoreData { path, slot } => {
            format!(
                "data modify storage {ns}:mw {path} set from storage {ns}:mw mw.stack[-1].r{slot}"
            )
        }
        Inst::SetValue { path, value } => {
            format!("data modify {} set value {value}", target(path, ns))
        }
        Inst::GetData { path } => format!("data get {}", target(path, ns)),
        Inst::GetScaled { path, scale } => {
            format!("data get {} {scale}", target(path, ns))
        }
        Inst::RemoveData { path } => format!("data remove {}", target(path, ns)),
        Inst::AppendValue { path, value } => {
            format!("data modify {} append value {value}", target(path, ns))
        }
        Inst::AppendFrom { dst, src } => {
            format!(
                "data modify {} append from {}",
                target(dst, ns),
                target(src, ns)
            )
        }
        Inst::CallWithArgs { path } => format!("function {path} with storage {ns}:mw mw.args"),
        // The `$` is what makes the line a macro line; vanilla substitutes the
        // arguments before parsing what follows.
        Inst::Macro { inst } => format!("${}", command(inst, ns, options)),
        Inst::CopyData { dst, src } => {
            format!(
                "data modify {} set from {}",
                target(dst, ns),
                target(src, ns)
            )
        }
        Inst::StoreData { path, tag, inst } => {
            format!(
                "execute store result {} {tag} 1 run {}",
                target(path, ns),
                command(inst, ns, options)
            )
        }
        // The register holds `scale`ths of a unit, so storage takes the reciprocal.
        // Vanilla multiplies by this before it converts to the tag.
        Inst::StoreScaled {
            path,
            tag,
            scale,
            inst,
        } => {
            let factor = 1.0 / f64::from(*scale);
            format!(
                "execute store result {} {tag} {factor} run {}",
                target(path, ns),
                command(inst, ns, options)
            )
        }
        Inst::Return { value } => format!("return {value}"),
        Inst::ReturnFail => "return fail".to_owned(),
        // The location comes from the span, the same way the source comments do
        // (spec section 6.30).
        Inst::Report { message, span } => {
            let at = match source_at(options, *span) {
                Some(at) => format!("{at} "),
                None => String::new(),
            };
            format!(
                "tellraw @a {{\"text\":\"minewell: {at}{}\",\"color\":\"red\"}}",
                escape_json(message)
            )
        }
        Inst::StoreBoth { ok, dst, inst } => {
            let (o, oobj) = (&ok.holder, objective(ns, ok));
            let (d, dobj) = (&dst.holder, objective(ns, dst));
            format!(
                "execute store success score {o} {oobj} store result score {d} {dobj} run {}",
                command(inst, ns, options)
            )
        }
        Inst::StoreCond { dst, cond } => {
            let (d, dobj) = (&dst.holder, objective(ns, dst));
            format!(
                "execute store success score {d} {dobj} {}",
                condition(cond, ns)
            )
        }
        Inst::SetString {
            dst,
            src,
            start,
            end,
        } => {
            let bounds = match (start, end) {
                (None, None) => String::new(),
                (start, None) => format!(" {}", start.unwrap_or(0)),
                (start, Some(end)) => format!(" {} {end}", start.unwrap_or(0)),
            };
            format!(
                "data modify {} set string {}{bounds}",
                target(dst, ns),
                target(src, ns)
            )
        }
        Inst::Guarded { cond, inst } => {
            format!(
                "execute {} run {}",
                condition(cond, ns),
                command(inst, ns, options)
            )
        }
        Inst::Otherwise { path, tags, inst } => {
            let clauses = tags
                .iter()
                .map(|tag| format!("unless data {}{{tag:\"{tag}\"}}", target(path, ns)))
                .collect::<Vec<_>>()
                .join(" ");
            format!("execute {clauses} run {}", command(inst, ns, options))
        }
        Inst::Context { clause, inst } => {
            let clause = match clause {
                ExecuteAs::As(selector) => format!("as {selector}"),
                ExecuteAs::At(selector) => format!("at {selector}"),
                ExecuteAs::Positioned(coords) => format!("positioned {coords}"),
            };
            format!("execute {clause} run {}", command(inst, ns, options))
        }
    }
}

/// `storage myns:mw mw.vars.main.x` or `entity @s Health`: where a data command
/// points, spelled as the command wants it (spec section 6.29).
fn target(reference: &DataRef, ns: &str) -> String {
    let path = &reference.path;
    match &reference.target {
        DataTarget::Storage => format!("storage {} {path}", crate::names::storage(ns)),
        DataTarget::Entity(selector) => format!("entity {selector} {path}"),
        DataTarget::Block(at) => format!("block {at} {path}"),
    }
}

/// `if score $a obj matches 1..`, ready to follow an `execute`.
fn condition(cond: &Cond, ns: &str) -> String {
    match cond {
        Cond::Block { at, id, negated } => {
            let keyword = if *negated { "unless" } else { "if" };
            format!("{keyword} block {at} {id}")
        }
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
        Cond::Data {
            path,
            filter,
            negated,
        } => {
            let keyword = if *negated { "unless" } else { "if" };
            format!("{keyword} data {}{filter}", target(path, ns))
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

/// A debug-build check that the executor a function requires is actually there.
///
/// `#[ctx(entity)]` says the caller must supply an executor, and the compiler enforces
/// that (`docs/02-spec.md` section 4.6). What it cannot know is whether the entity is
/// still alive by the time the function runs — an `as` block whose entity died partway
/// through keeps going, and every `@s` command in it quietly does nothing. In debug
/// builds that gets said out loud; in release it costs nothing because it is not
/// emitted (requirements section 6.3).
fn executor_guard(function: &Function, options: &Options) -> Option<String> {
    if options.profile != Profile::Debug {
        return None;
    }
    let needs_entity = function.attrs.iter().any(|attr| match attr {
        Attr::Ctx(kinds) => kinds.contains(&Ctx::Entity),
        _ => false,
    });
    if !needs_entity {
        return None;
    }
    let path = &function.path;
    Some(format!(
        "execute unless entity @s run tellraw @a {{\"text\":\"minewell: {path} ran with no executor\",\"color\":\"red\"}}\n"
    ))
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

/// `src/arena.mwl:42`, when the build was given its source.
fn source_at(options: &Options, span: Span) -> Option<String> {
    let source = options.source.as_ref()?;
    let line = line_of(&source.text, span.start);
    Some(format!("{}:{line}", source.path))
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
        let (hir, errors) = crate::hir::lower(&file, "myns", None);
        assert!(errors.is_empty(), "{errors:?}");
        emit(
            &crate::mir::lower(&hir, options.profile == Profile::Debug),
            options,
        )
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
        // Tagged: a release build keeps only what a function tag can reach.
        let pack = compile(r#"#[load] fn main() { raw!("say hi"); }"#, &release());
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
    fn an_uncalled_function_is_left_out_of_a_release_build() {
        let pack = compile(
            r#"#[tick] fn main() { raw!("say hi"); } fn other() { raw!("say x"); }"#,
            &release(),
        );
        assert!(
            !pack
                .files
                .contains_key("data/myns/function/other.mcfunction"),
            "{:?}",
            pack.files.keys().collect::<Vec<_>>()
        );
        assert!(
            pack.files
                .contains_key("data/myns/function/main.mcfunction")
        );
    }

    #[test]
    fn a_debug_build_keeps_every_function() {
        // Requirements section 15: debug keeps source and output one to one.
        let pack = compile(
            r#"#[tick] fn main() { raw!("say hi"); } fn other() { raw!("say x"); }"#,
            &Options::default(),
        );
        assert!(
            pack.files
                .contains_key("data/myns/function/other.mcfunction")
        );
    }

    #[test]
    fn a_function_named_only_in_a_raw_command_is_kept() {
        // `raw!` is text the compiler does not read, so anything it names counts.
        let pack = compile(
            r#"#[tick] fn main() { raw!("function myns:other"); } fn other() { raw!("say x"); }"#,
            &release(),
        );
        assert!(
            pack.files
                .contains_key("data/myns/function/other.mcfunction"),
            "{:?}",
            pack.files.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_objective_nothing_touches_is_not_created() {
        let pack = compile(r#"#[tick] fn main() { raw!("say hi"); }"#, &release());
        assert_eq!(pack.files["data/myns/function/__init.mcfunction"], "");
    }

    #[test]
    fn the_objective_a_binding_needs_is_created() {
        let pack = compile("#[tick] fn main() { let a = 1; }", &release());
        let init = &pack.files["data/myns/function/__init.mcfunction"];
        assert!(init.contains("myns.v"), "{init}");
        assert!(!init.contains("myns.t"), "{init}");
    }

    #[test]
    fn an_objective_named_last_on_a_line_still_counts() {
        // A `raw!` is why the objectives are looked for in the text: this one names
        // `myns.v` as the last word of a line that is not the last line.
        let pack = compile(
            r#"#[load] fn main() { raw!("scoreboard players get $x myns.v"); raw!("say hi"); }"#,
            &release(),
        );
        let init = &pack.files["data/myns/function/__init.mcfunction"];
        assert!(init.contains("myns.v"), "{init}");
    }

    #[test]
    fn tests_do_not_ship_in_a_release_build() {
        // Nothing calls them from a function tag, so dead code elimination takes them.
        let pack = compile("#[test] fn t() {} #[load] fn main() {}", &release());
        assert!(!pack.files.contains_key("data/myns/function/t.mcfunction"));

        let debug = compile(
            "#[test] fn t() {} #[load] fn main() {}",
            &Options::default(),
        );
        assert!(debug.files.contains_key("data/myns/function/t.mcfunction"));
        assert_eq!(debug.tests, vec!["myns:t".to_owned()]);
    }

    #[test]
    fn the_whole_pack() {
        let pack = compile(
            r#"#[load] fn main() { raw!("say hi"); raw!("say bye"); } #[tick] fn other() { raw!("say x"); }"#,
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
        let src = "#[load] fn main() {\n    raw!(\"say hi\");\n}";
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
        let pack = compile(r#"#[load] fn main() { raw!("say hi"); }"#, &release());
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
        // The nested sum on the right cannot be written through the destination, so
        // this one still takes a temporary (spec section 6.37).
        let pack = compile(
            "#[load] fn main() { let a = 1; let b = (a + a) * (a + a); }",
            &release(),
        );
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
    fn a_debug_build_checks_that_the_executor_is_really_there() {
        let src =
            "#[ctx(entity)] fn hurt() {} #[tick] fn main() { as @e[type=zombie] { hurt(); } }";
        let debug = compile(src, &Options::default()).files;
        assert!(
            debug["data/myns/function/hurt.mcfunction"].contains("unless entity @s"),
            "{}",
            debug["data/myns/function/hurt.mcfunction"]
        );

        let release = compile(src, &release()).files;
        assert_eq!(release["data/myns/function/hurt.mcfunction"], "");
    }

    #[test]
    fn a_function_with_no_context_requirement_gets_no_guard() {
        let pack = compile("fn main() {}", &Options::default());
        assert!(!pack.files["data/myns/function/main.mcfunction"].contains("unless entity"));
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
