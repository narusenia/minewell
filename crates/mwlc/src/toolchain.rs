// SPDX-License-Identifier: MIT

//! Installed Minecraft versions: their command set and their pack format.
//!
//! The compiler is version-agnostic by design (`docs/01-requirements.md` section 1.2).
//! Everything that changes between Minecraft versions lives in a toolchain directory,
//! so supporting a new version is a data drop rather than a release of this crate.
//!
//! Nothing is embedded as a fallback. A built-in command table would quietly become
//! the version everyone compiles against, and the claim above would stop being true.

use std::path::{Path, PathBuf};

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::schema::Schema;

/// A directory of installed toolchains.
///
/// The location is a value rather than something read from the environment inside each
/// call, so tests can point at their own directory without racing each other over a
/// process-wide variable.
#[derive(Debug, Clone)]
pub struct Toolchains {
    pub root: PathBuf,
}

impl Default for Toolchains {
    /// `$MINEWELL_HOME/toolchains`, or `~/.minewell/toolchains`.
    fn default() -> Self {
        let home = std::env::var_os("MINEWELL_HOME").map_or_else(
            || {
                std::env::var_os("HOME")
                    .map_or_else(|| PathBuf::from("."), PathBuf::from)
                    .join(".minewell")
            },
            PathBuf::from,
        );
        Toolchains {
            root: home.join("toolchains"),
        }
    }
}

/// What a toolchain records about its version beyond the command set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    /// The `pack_format` this version's datapacks declare.
    pub pack_format: u32,
    pub minecraft_version: String,
}

#[derive(Debug, Clone)]
pub struct Toolchain {
    pub version: String,
    pub metadata: Metadata,
    pub schema: Schema,
}

#[derive(Debug, Error, Diagnostic)]
pub enum Error {
    #[error("no toolchain '{version}' is installed")]
    #[diagnostic(help(
        "installed: {}\nadd one with: mwl toolchain add {version} <commands.json> --pack-format <n>",
        if .installed.is_empty() { "none".to_owned() } else { .installed.join(", ") }
    ))]
    NotInstalled {
        version: String,
        installed: Vec<String>,
    },

    #[error("toolchain '{version}' is missing {file}")]
    Incomplete { version: String, file: String },

    #[error("could not read {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not valid JSON")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

const METADATA: &str = "toolchain.json";
const COMMANDS: &str = "commands.json";

impl Toolchains {
    pub fn installed(&self) -> Vec<String> {
        let mut found: Vec<String> = std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                entry
                    .path()
                    .join(METADATA)
                    .exists()
                    .then(|| entry.file_name().to_string_lossy().into_owned())
            })
            .collect();
        found.sort();
        found
    }

    pub fn load(&self, version: &str) -> Result<Toolchain, Error> {
        let dir = self.root.join(version);
        if !dir.join(METADATA).exists() {
            return Err(Error::NotInstalled {
                version: version.to_owned(),
                installed: self.installed(),
            });
        }
        let metadata: Metadata = read_json(&dir, METADATA, version)?;
        let commands = read(&dir.join(COMMANDS)).map_err(|_| Error::Incomplete {
            version: version.to_owned(),
            file: COMMANDS.to_owned(),
        })?;
        let schema = Schema::parse(&commands).map_err(|source| Error::Json {
            path: dir.join(COMMANDS),
            source,
        })?;
        Ok(Toolchain {
            version: version.to_owned(),
            metadata,
            schema,
        })
    }

    /// Installs a toolchain from a `commands.json` the caller already has.
    ///
    /// Producing that file needs Minecraft's data generator and a JVM:
    ///
    /// ```text
    /// java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports
    /// ```
    ///
    /// Keeping that step outside the compiler is deliberate: most datapack authors are not
    /// running servers, and requiring a JVM to compile would be a strange toll. Published
    /// toolchains (X-2) will make even this unnecessary.
    pub fn add(
        &self,
        version: &str,
        commands_json: &Path,
        pack_format: u32,
    ) -> Result<PathBuf, Error> {
        let commands = read(commands_json)?;
        // Fail before writing anything if the file is not what it claims to be.
        Schema::parse(&commands).map_err(|source| Error::Json {
            path: commands_json.to_owned(),
            source,
        })?;

        let dir = self.root.join(version);
        std::fs::create_dir_all(&dir).map_err(|source| Error::Io {
            path: dir.clone(),
            source,
        })?;
        write(&dir.join(COMMANDS), &commands)?;
        let metadata = Metadata {
            pack_format,
            minecraft_version: version.to_owned(),
        };
        write(
            &dir.join(METADATA),
            &serde_json::to_string_pretty(&metadata).expect("metadata serialises"),
        )?;
        Ok(dir)
    }
}

fn read(path: &Path) -> Result<String, Error> {
    std::fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_owned(),
        source,
    })
}

fn write(path: &Path, contents: &str) -> Result<(), Error> {
    std::fs::write(path, contents).map_err(|source| Error::Io {
        path: path.to_owned(),
        source,
    })
}

fn read_json<T: for<'de> Deserialize<'de>>(
    dir: &Path,
    file: &str,
    version: &str,
) -> Result<T, Error> {
    let path = dir.join(file);
    let text = read(&path).map_err(|_| Error::Incomplete {
        version: version.to_owned(),
        file: file.to_owned(),
    })?;
    serde_json::from_str(&text).map_err(|source| Error::Json { path, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A toolchain directory of this test's own.
    fn empty() -> (tempfile::TempDir, Toolchains) {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("toolchains");
        (dir, Toolchains { root })
    }

    fn commands() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/commands.json")
    }

    #[test]
    fn add_then_load_round_trips() {
        let (_dir, tc) = empty();
        tc.add("1.21.4", &commands(), 61).expect("adds");

        let toolchain = tc.load("1.21.4").expect("loads");
        assert_eq!(toolchain.metadata.pack_format, 61);
        assert!(toolchain.schema.get("setblock").is_some());
        assert_eq!(tc.installed(), vec!["1.21.4"]);
    }

    #[test]
    fn loading_something_absent_says_what_is_there_instead() {
        let (_dir, tc) = empty();
        let err = tc.load("1.21.4").unwrap_err();
        assert!(matches!(err, Error::NotInstalled { .. }), "{err:?}");
        assert!(
            err.help()
                .is_some_and(|h| h.to_string().contains("mwl toolchain add")),
            "the error should say how to fix itself"
        );
    }

    #[test]
    fn adding_a_file_that_is_not_a_command_tree_writes_nothing() {
        let (dir, tc) = empty();
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{ not json").unwrap();
        assert!(tc.add("1.21.4", &bad, 61).is_err());
        assert!(
            tc.installed().is_empty(),
            "a failed add must leave nothing behind"
        );
    }
}
