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

use crate::schema::{Overrides, Schema};

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
    /// Renames in `overrides.toml` that matched nothing in this version.
    pub stale_overrides: Vec<String>,
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

    #[error("could not fetch {url}")]
    #[diagnostic(help(
        "if the version is not published yet, build it yourself:\n  \
         scripts/build-toolchain.sh <version>\n  \
         mwl toolchain add <version> <commands.json> --pack-format <n>"
    ))]
    Download { url: String },

    #[error("'{tool}' is not on the PATH")]
    #[diagnostic(help("installing a published toolchain needs curl and tar"))]
    MissingTool { tool: &'static str },

    #[error("{path} is not valid TOML")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// Where the published toolchains live (plan X-2).
pub const RELEASES: &str = "https://github.com/narusenia/minewell/releases/download";

const METADATA: &str = "toolchain.json";
const COMMANDS: &str = "commands.json";
const OVERRIDES: &str = "overrides.toml";

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
        let mut schema = Schema::parse(&commands).map_err(|source| Error::Json {
            path: dir.join(COMMANDS),
            source,
        })?;

        let overrides_path = dir.join(OVERRIDES);
        let mut stale_overrides = Vec::new();
        if overrides_path.exists() {
            let text = read(&overrides_path)?;
            let overrides: Overrides = toml::from_str(&text).map_err(|source| Error::Toml {
                path: overrides_path,
                source,
            })?;
            stale_overrides = schema.rename(&overrides);
        }

        Ok(Toolchain {
            version: version.to_owned(),
            metadata,
            schema,
            stale_overrides,
        })
    }

    /// Downloads a published toolchain and installs it.
    ///
    /// `curl` and `tar` do the work rather than a HTTP client and a gzip crate. The
    /// archive is what `scripts/build-toolchain.sh` and the workflow beside it
    /// produced with `tar -czf`, so unpacking it is the same operation backwards, and
    /// a compiler is a strange place to grow a TLS stack. Both tools ship with macOS,
    /// Linux and Windows 10 and later; where they do not, `add` is the way in.
    ///
    /// `base` is the release directory to fetch from, so a test can point at a
    /// `file://` copy instead of the network.
    pub fn install(&self, version: &str, base: &str) -> Result<PathBuf, Error> {
        for tool in ["curl", "tar"] {
            if which(tool).is_none() {
                return Err(Error::MissingTool { tool });
            }
        }
        let url = format!("{base}/toolchain-{version}/mwl-toolchain-{version}.tar.gz");
        std::fs::create_dir_all(&self.root).map_err(|source| Error::Io {
            path: self.root.clone(),
            source,
        })?;
        let archive = self.root.join(format!(".{version}.tar.gz"));

        let fetched = std::process::Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&archive)
            .arg(&url)
            // Its complaint would be the second one printed, and the worse of the two.
            .stderr(std::process::Stdio::null())
            .status();
        if !matches!(fetched, Ok(status) if status.success()) {
            let _ = std::fs::remove_file(&archive);
            return Err(Error::Download { url });
        }

        let unpacked = std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&self.root)
            .status();
        let _ = std::fs::remove_file(&archive);
        if !matches!(unpacked, Ok(status) if status.success()) {
            return Err(Error::Download { url });
        }

        // The archive said what it is; this proves it. A toolchain that does not load
        // is worse than one that is not there, because it fails later and elsewhere.
        self.load(version)?;
        Ok(self.root.join(version))
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

/// Whether a program is on the `PATH`, without running it.
fn which(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(tool))
        .find(|candidate| candidate.is_file())
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
    fn installing_unpacks_an_archive_and_checks_it() {
        // Served from disk rather than the network: the fetch is the same, and a test
        // that needs GitHub to be up is a test that fails for the wrong reason.
        let (_dir, tc) = empty();
        let published = tempfile::tempdir().expect("temp dir");
        let staged = published.path().join("1.21.4");
        std::fs::create_dir_all(&staged).expect("stage");
        std::fs::copy(commands(), staged.join(COMMANDS)).expect("commands");
        std::fs::write(
            staged.join(METADATA),
            r#"{"pack_format":61,"minecraft_version":"1.21.4"}"#,
        )
        .expect("metadata");
        let release = published.path().join("toolchain-1.21.4");
        std::fs::create_dir_all(&release).expect("release");
        assert!(
            std::process::Command::new("tar")
                .arg("-czf")
                .arg(release.join("mwl-toolchain-1.21.4.tar.gz"))
                .arg("-C")
                .arg(published.path())
                .arg("1.21.4")
                .status()
                .expect("tar runs")
                .success()
        );

        let base = format!("file://{}", published.path().display());
        tc.install("1.21.4", &base).expect("installs");
        assert_eq!(tc.installed(), vec!["1.21.4"]);
        assert_eq!(tc.load("1.21.4").expect("loads").metadata.pack_format, 61);
    }

    #[test]
    fn a_version_that_is_not_published_says_how_to_build_it() {
        let (_dir, tc) = empty();
        let published = tempfile::tempdir().expect("temp dir");
        let base = format!("file://{}", published.path().display());
        let err = tc.install("1.99.9", &base).expect_err("nothing to fetch");
        assert!(matches!(err, Error::Download { .. }), "{err:?}");
        // Nothing half-installed is left behind.
        assert!(tc.installed().is_empty());
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
    fn an_overrides_file_renames_commands_on_load() {
        let (_dir, tc) = empty();
        let installed = tc.add("1.21.4", &commands(), 61).expect("adds");
        std::fs::write(
            installed.join("overrides.toml"),
            "[rename]\ndata_get_entity = \"data_get\"\ngone = \"elsewhere\"\n",
        )
        .unwrap();

        let toolchain = tc.load("1.21.4").expect("loads");
        assert!(toolchain.schema.get("data_get").is_some());
        assert_eq!(toolchain.stale_overrides, vec!["gone"]);
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
