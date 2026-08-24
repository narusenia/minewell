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
}

/// Where a project's crate root lives.
pub const ROOT_SOURCE: &str = "src/lib.mwl";
pub const MANIFEST: &str = "minewell.toml";
pub const OUTPUT_DIR: &str = "target/datapack";

pub fn manifest(root: &Path) -> Result<Manifest, BuildError> {
    let path = root.join(MANIFEST);
    let text = read(&path)?;
    toml::from_str(&text).map_err(|source| BuildError::Manifest { path, source })
}

/// Compiles a project into a datapack, without writing anything.
pub fn build(root: &Path, profile: Profile) -> Result<Datapack, BuildError> {
    let manifest = manifest(root)?;
    let path = root.join(ROOT_SOURCE);
    let text = read(&path)?;

    // Without a toolchain the compiler simply does not know the command set, so
    // command calls are an error and `pack_format` falls back. `raw!` still works, so
    // a project that never calls a command needs no toolchain at all.
    let toolchain = match &manifest.package.toolchain {
        Some(version) => Some(crate::toolchain::Toolchains::default().load(version)?),
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
    Ok(compile_with(
        &text,
        manifest.package.namespace(),
        &options,
        toolchain.as_ref().map(|t| &t.schema),
    )?)
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
    Ok(emit::emit(&crate::mir::lower(&hir), options))
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
