// SPDX-License-Identifier: MIT

//! Reading a project and building its datapack.
//!
//! The one place that touches the filesystem on the way in, mirroring `emit`, which is
//! the one place that touches it on the way out. Everything between them is pure, so
//! the compiler can be driven from a test with strings alone.

use std::path::{Path, PathBuf};

use miette::Diagnostic;
use serde::Deserialize;
use thiserror::Error;

use crate::diagnostics::Report;
use crate::emit::{self, Datapack, Options, Profile};

/// `minewell.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub package: Package,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Package {
    pub name: String,
    /// The datapack namespace generated functions live under. Defaults to the name.
    pub namespace: Option<String>,
    pub description: Option<String>,
    /// The Minecraft version whose toolchain to build against. Honoured from M6.
    pub toolchain: Option<String>,
}

impl Package {
    pub fn namespace(&self) -> &str {
        self.namespace.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Error, Diagnostic)]
pub enum BuildError {
    #[error("could not read {path}")]
    #[diagnostic(help("is this a minewell project? it needs a minewell.toml and a src/lib.mwl"))]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse {path}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error(transparent)]
    #[diagnostic(transparent)]
    Source(#[from] Report),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Toolchain(#[from] crate::toolchain::Error),

    #[error("{path} is written by hand and also generated")]
    #[diagnostic(help("rename one of them; the compiler will not choose for you"))]
    Shadowed { path: String },
}

/// Where a project's crate root lives.
pub const ROOT_SOURCE: &str = "src/lib.mwl";
/// Hand-written datapack resources, copied through untouched (requirements §13).
pub const DATA_DIR: &str = "data";
pub const MANIFEST: &str = "minewell.toml";
pub const OUTPUT_DIR: &str = "target/datapack";

pub fn manifest(root: &Path) -> Result<Manifest, BuildError> {
    let path = root.join(MANIFEST);
    let text = read(&path)?;
    toml::from_str(&text).map_err(|source| BuildError::Manifest { path, source })
}

/// Compiles a project into a datapack, without writing anything.
pub fn build(root: &Path, profile: Profile) -> Result<Datapack, BuildError> {
    build_with(root, profile, &crate::toolchain::Toolchains::default())
}

/// As [`build`], looking for toolchains somewhere specific.
pub fn build_with(
    root: &Path,
    profile: Profile,
    toolchains: &crate::toolchain::Toolchains,
) -> Result<Datapack, BuildError> {
    let manifest = manifest(root)?;
    let path = root.join(ROOT_SOURCE);
    let text = read(&path)?;

    // Without a toolchain the compiler simply does not know the command set, so
    // command calls are an error and `pack_format` falls back. `raw!` still works, so
    // a project that never calls a command needs no toolchain at all.
    let toolchain = match &manifest.package.toolchain {
        Some(version) => Some(toolchains.load(version)?),
        None => None,
    };

    let options = Options {
        pack_format: toolchain
            .as_ref()
            .map_or(crate::emit::PLACEHOLDER_PACK_FORMAT, |t| {
                t.metadata.pack_format
            }),
        description: manifest
            .package
            .description
            .clone()
            .unwrap_or_else(|| manifest.package.name.clone()),
        profile,
        source: Some(emit::Source {
            path: path.display().to_string(),
            text: text.clone(),
        }),
    };
    // Hand-written resources are gathered first: a reference may resolve to one of
    // them, and the check below has to see them.
    let mut written = Datapack::default();
    copy_data_dir(root, &mut written)?;
    let existing: Vec<String> = written.files.keys().cloned().collect();

    let mut pack = compile_into(
        &text,
        manifest.package.namespace(),
        &options,
        toolchain.as_ref().map(|t| &t.schema),
        &existing,
    )?;
    for (path, contents) in written.files {
        if pack.files.insert(path.clone(), contents).is_some() {
            return Err(BuildError::Shadowed { path });
        }
    }
    Ok(pack)
}

/// Copies `data/` into the pack.
///
/// Advancements, predicates and loot tables are still written by hand (requirements
/// section 13), so they ride along rather than being regenerated. A hand-written file
/// landing on a generated one is an error: silently preferring either would be a
/// surprise, and the author can rename.
fn copy_data_dir(root: &Path, pack: &mut Datapack) -> Result<(), BuildError> {
    let dir = root.join(DATA_DIR);
    if !dir.exists() {
        return Ok(());
    }
    for entry in walk(&dir)? {
        let relative = entry
            .strip_prefix(root)
            .expect("under the project root")
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let contents = read(&entry)?;
        if pack.files.insert(relative.clone(), contents).is_some() {
            return Err(BuildError::Shadowed { path: relative });
        }
    }
    Ok(())
}

fn walk(dir: &Path) -> Result<Vec<PathBuf>, BuildError> {
    let mut files = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|source| BuildError::Io {
        path: dir.to_owned(),
        source,
    })?;
    for entry in entries {
        let path = entry
            .map_err(|source| BuildError::Io {
                path: dir.to_owned(),
                source,
            })?
            .path();
        if path.is_dir() {
            files.extend(walk(&path)?);
        } else {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Source text to datapack, touching nothing outside memory.
///
/// The whole compiler between the two I/O edges. Tests drive this directly, which is
/// what lets the vertical harness compile and run a program without a project on disk.
pub fn compile(text: &str, namespace: &str, options: &Options) -> Result<Datapack, Report> {
    compile_with(text, namespace, options, None)
}

/// As [`compile`], against a particular version's command set.
pub fn compile_with(
    text: &str,
    namespace: &str,
    options: &Options,
    toolchain: Option<&crate::schema::Schema>,
) -> Result<Datapack, Report> {
    compile_into(text, namespace, options, toolchain, &[])
}

/// As [`compile_with`], with the hand-written resources that references may resolve to.
pub fn compile_into(
    text: &str,
    namespace: &str,
    options: &Options,
    toolchain: Option<&crate::schema::Schema>,
    existing: &[String],
) -> Result<Datapack, Report> {
    let shown = options
        .source
        .as_ref()
        .map_or("<input>", |source| source.path.as_str());

    let (file, mut errors) = crate::syntax::parser::parse(text);
    let (hir, more) = crate::hir::lower(&file, namespace, toolchain);
    errors.extend(more);
    if let Some(report) = Report::of(shown, text, errors) {
        return Err(report);
    }

    let debug = options.profile == Profile::Debug;
    let pack = emit::emit(&crate::mir::lower(&hir, debug), options);
    let unresolved = unresolved_references(&hir, &pack, existing);
    if let Some(report) = Report::of(shown, text, unresolved) {
        return Err(report);
    }
    Ok(pack)
}

/// References to things that are nowhere in the finished pack.
///
/// Naming a function that does not exist is the archetypal mcfunction bug: the command
/// runs, nothing happens, and no error is printed anywhere. It costs one lookup to
/// turn that into a compile error.
fn unresolved_references(
    hir: &crate::hir::Hir,
    pack: &Datapack,
    existing: &[String],
) -> Vec<crate::syntax::SyntaxError> {
    hir.references
        .iter()
        .filter(|reference| {
            let Some((namespace, path)) = reference.id.split_once(':') else {
                return true;
            };
            let file = format!(
                "data/{namespace}/{}/{path}.{}",
                reference.kind.directory(),
                reference.kind.extension()
            );
            !pack.files.contains_key(&file) && !existing.contains(&file)
        })
        .map(|reference| {
            let id = &reference.id;
            crate::syntax::SyntaxError::new(
                reference.span,
                format!("nothing in this pack defines '{id}'"),
            )
        })
        .collect()
}

fn read(path: &Path) -> Result<String, BuildError> {
    std::fs::read_to_string(path).map_err(|source| BuildError::Io {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project on disk, since that is what the driver's job is.
    fn project(manifest: &str, source: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join(MANIFEST), manifest).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join(ROOT_SOURCE), source).unwrap();
        dir
    }

    const SIMPLE: &str = "[package]\nname = \"mypack\"\nnamespace = \"myns\"\n";

    #[test]
    fn a_project_builds_into_a_datapack() {
        let dir = project(SIMPLE, r#"fn main() { raw!("say hi"); }"#);
        let pack = build(dir.path(), Profile::Release).expect("builds");
        assert_eq!(pack.files["data/myns/function/main.mcfunction"], "say hi\n");
    }

    #[test]
    fn the_namespace_defaults_to_the_package_name() {
        let dir = project("[package]\nname = \"mypack\"\n", "fn main() {}");
        let pack = build(dir.path(), Profile::Release).expect("builds");
        assert!(
            pack.files
                .contains_key("data/mypack/function/main.mcfunction")
        );
    }

    #[test]
    fn the_description_defaults_to_the_package_name() {
        let dir = project(SIMPLE, "fn main() {}");
        let pack = build(dir.path(), Profile::Release).unwrap();
        assert!(pack.files["pack.mcmeta"].contains("mypack"));
    }

    #[test]
    fn a_missing_manifest_says_what_a_project_needs() {
        let dir = tempfile::tempdir().unwrap();
        let err = build(dir.path(), Profile::Release).unwrap_err();
        assert!(matches!(err, BuildError::Io { .. }), "{err:?}");
        assert!(err.help().is_some(), "the error should suggest something");
    }

    #[test]
    fn a_broken_manifest_names_the_file() {
        let dir = project("[package]\nnot_a_name = 1\n", "fn main() {}");
        let err = build(dir.path(), Profile::Release).unwrap_err();
        assert!(matches!(err, BuildError::Manifest { .. }), "{err:?}");
    }

    #[test]
    fn source_errors_come_back_as_a_report() {
        let dir = project(SIMPLE, "fn main() { nope!(); }");
        let err = build(dir.path(), Profile::Release).unwrap_err();
        assert!(matches!(err, BuildError::Source(_)), "{err:?}");
    }

    fn schema() -> crate::schema::Schema {
        crate::schema::Schema::parse(include_str!("../tests/fixtures/commands.json"))
            .expect("fixture")
    }

    #[test]
    fn hand_written_resources_are_copied_through() {
        let dir = project(SIMPLE, "fn main() {}");
        std::fs::create_dir_all(dir.path().join("data/myns/predicate")).unwrap();
        std::fs::write(
            dir.path().join("data/myns/predicate/in_rain.json"),
            "{\"condition\": \"minecraft:weather_check\"}",
        )
        .unwrap();

        let pack = build(dir.path(), Profile::Release).expect("builds");
        assert!(
            pack.files.contains_key("data/myns/predicate/in_rain.json"),
            "{:?}",
            pack.files.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_hand_written_file_cannot_shadow_a_generated_one() {
        let dir = project(SIMPLE, "fn main() {}");
        std::fs::create_dir_all(dir.path().join("data/myns/function")).unwrap();
        std::fs::write(
            dir.path().join("data/myns/function/main.mcfunction"),
            "say no",
        )
        .unwrap();
        let err = build(dir.path(), Profile::Release).unwrap_err();
        assert!(matches!(err, BuildError::Shadowed { .. }), "{err:?}");
    }

    #[test]
    fn calling_a_function_that_does_not_exist_is_a_compile_error() {
        // Vanilla runs this and does nothing at all, with no message anywhere.
        let err = compile_with(
            "fn main() { function(myns:nowhere); }",
            "myns",
            &Options::default(),
            Some(&schema()),
        )
        .unwrap_err();
        assert_eq!(err.problems.len(), 1);
    }

    #[test]
    fn a_reference_to_a_generated_function_resolves() {
        assert!(
            compile_with(
                "fn helper() {} fn main() { function(myns:helper); }",
                "myns",
                &Options::default(),
                Some(&schema()),
            )
            .is_ok()
        );
    }

    #[test]
    fn a_reference_to_a_hand_written_function_resolves() {
        let dir = project(
            "[package]\nname = \"mypack\"\nnamespace = \"myns\"\ntoolchain = \"test\"\n",
            "fn main() { function(myns:handwritten); }",
        );
        std::fs::create_dir_all(dir.path().join("data/myns/function")).unwrap();
        std::fs::write(
            dir.path().join("data/myns/function/handwritten.mcfunction"),
            "say hello",
        )
        .unwrap();

        let home = tempfile::tempdir().unwrap();
        let toolchains = crate::toolchain::Toolchains {
            root: home.path().join("toolchains"),
        };
        toolchains
            .add(
                "test",
                &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/commands.json"),
                61,
            )
            .unwrap();

        let built = build_with(dir.path(), Profile::Release, &toolchains);
        assert!(built.is_ok(), "{:?}", built.err());
    }

    #[test]
    fn syntax_and_resolution_errors_are_reported_together() {
        // One build, every problem: a parse error and a resolution error at once.
        let dir = project(SIMPLE, "fn a() { raw!(\"x\") } fn b() { nope!(); }");
        let BuildError::Source(report) = build(dir.path(), Profile::Release).unwrap_err() else {
            panic!("expected a source report");
        };
        assert_eq!(report.problems.len(), 2);
    }
}
