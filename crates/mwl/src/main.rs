// SPDX-License-Identifier: MIT

//! The `mwl` command-line interface.

mod lsp;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use mwlc::cost;
use mwlc::driver::{self, OUTPUT_DIR};
use mwlc::emit::Profile;
use mwlc::toolchain::Toolchains;

/// Where the cost table lands, under `target/`.
const COST_FILE: &str = "cost.txt";

const USAGE: &str = "\
mwl — the minewell compiler

usage:
    mwl new <name>
        create a project in ./<name>

    mwl check
        compile and report problems, writing nothing

    mwl test
        run the #[test] functions in the interpreter

    mwl install <world> [--release]
        build and place the pack in <world>/datapacks

    mwl build [--release]
        compile the project into target/datapack, and write the per-function
        command counts to target/cost.txt

    mwl lsp
        run the language server on stdin and stdout

    mwl toolchain list
        show the installed Minecraft versions

    mwl toolchain install <version>
        download a published toolchain and install it

    mwl toolchain add <version> <commands.json> --pack-format <n>
        install a version's command set

        commands.json comes from Minecraft's own data generator:
            java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports
        it lands in generated/reports/commands.json
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flags: Vec<&str> = args.iter().map(String::as_str).collect();

    match flags.as_slice() {
        ["new", name] => report(new_project(name)),
        ["check"] => report(check()),
        ["lsp"] => match lsp::serve() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
        ["test"] => match run_tests() {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(report) => {
                eprintln!("{report:?}");
                ExitCode::FAILURE
            }
        },
        ["install", world, rest @ ..] => report(install(world, profile_of(rest))),
        ["build", rest @ ..] => report(build(profile_of(rest))),
        ["toolchain", "install", version] => report(install_toolchain(version)),
        ["toolchain", "list"] => {
            let installed = Toolchains::default().installed();
            if installed.is_empty() {
                println!("no toolchains installed");
            } else {
                for version in installed {
                    println!("{version}");
                }
            }
            ExitCode::SUCCESS
        }
        ["toolchain", "add", version, path, "--pack-format", format] => {
            match add_toolchain(version, path, format) {
                Ok(message) => {
                    println!("{message}");
                    ExitCode::SUCCESS
                }
                Err(report) => {
                    eprintln!("{report:?}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprint!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

/// Prints what a command has to say, and turns it into an exit status.
fn report(outcome: miette::Result<String>) -> ExitCode {
    match outcome {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(report) => {
            // `{:?}` is how miette renders a diagnostic for a person.
            eprintln!("{report:?}");
            ExitCode::FAILURE
        }
    }
}

fn profile_of(flags: &[&str]) -> Profile {
    if flags.contains(&"--release") {
        Profile::Release
    } else {
        Profile::Debug
    }
}

const TEMPLATE: &str = r#"//! {name}

#[load]
fn setup() {
    raw!("say {name} is loaded");
}

#[test]
fn arithmetic_holds() {
    let a = 2 + 3;
    debug_assert!(a == 5, "two and three make five");
}
"#;

fn new_project(name: &str) -> miette::Result<String> {
    new_project_in(Path::new("."), name)
}

/// As [`new_project`], somewhere other than the working directory. Tests pass the
/// location in rather than moving the process into it.
fn new_project_in(parent: &Path, name: &str) -> miette::Result<String> {
    // The name becomes the datapack namespace, and a datapack path may not have a
    // capital in it. Refusing now beats a pack Minecraft silently will not load.
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(miette::miette!(
            "'{name}' cannot be a namespace: use lowercase letters, digits, '_' and '-'"
        ));
    }
    let root = parent.join(name);
    if root.exists() {
        return Err(miette::miette!("{} already exists", root.display()));
    }
    let src = root.join("src");
    std::fs::create_dir_all(&src)
        .map_err(|e| miette::miette!("could not create {}: {e}", src.display()))?;
    write(
        &root.join(driver::MANIFEST),
        &format!("[package]\nname = \"{name}\"\n"),
    )?;
    write(
        &root.join(driver::ROOT_SOURCE),
        &TEMPLATE.replace("{name}", name),
    )?;
    Ok(format!(
        "created {}; `cd {name} && mwl build` to compile it",
        root.display()
    ))
}

fn write(path: &PathBuf, contents: &str) -> miette::Result<()> {
    std::fs::write(path, contents)
        .map_err(|e| miette::miette!("could not write {}: {e}", path.display()))
}

fn check() -> miette::Result<String> {
    let pack = driver::build(&PathBuf::from("."), Profile::Debug)?;
    Ok(format!("no problems found in {} file(s)", pack.files.len()))
}

fn install(world: &str, profile: Profile) -> miette::Result<String> {
    let world = PathBuf::from(world);
    if !world.join("level.dat").exists() {
        return Err(miette::miette!(
            "{} does not look like a world: it has no level.dat",
            world.display()
        ));
    }
    let root = PathBuf::from(".");
    let name = driver::manifest(&root)?.package.name;
    let pack = driver::build(&root, profile)?;
    let dest = world.join("datapacks").join(&name);
    pack.write_to(&dest)
        .map_err(|e| miette::miette!("could not write to {}: {e}", dest.display()))?;
    Ok(format!("installed {name} into {}", dest.display()))
}

/// Runs every `#[test]` function under the interpreter, in debug so the checks are
/// there to fail (requirements section 17).
///
/// Each test gets a world of its own: a test that leaves a score behind must not
/// decide whether the next one passes.
fn run_tests() -> miette::Result<bool> {
    run_tests_in(Path::new("."))
}

/// As [`run_tests`], for a project somewhere other than the working directory.
fn run_tests_in(root: &Path) -> miette::Result<bool> {
    let root = root.to_path_buf();
    let namespace = driver::manifest(&root)?.package.namespace().to_owned();
    let pack = driver::build(&root, Profile::Debug)?;
    if pack.tests.is_empty() {
        println!("no #[test] functions");
        return Ok(true);
    }
    println!("running {} test(s)", pack.tests.len());
    let mut failed = 0;
    for id in &pack.tests {
        let mut mc = tinymcf::Interpreter::default();
        for (path, text) in &pack.files {
            if let Some(function) = function_id(path) {
                mc.load(&function, text)
                    .map_err(|e| miette::miette!("{function} does not parse: {e}"))?;
            }
        }
        mc.call(&format!("{namespace}:{}", mwlc::emit::INIT_FUNCTION));
        mc.diagnostics.clear();
        mc.call(id);

        // A check that fails says so with a `tellraw`, and anything the interpreter
        // could not do says so as a diagnostic. Either one is a failure.
        let mut said: Vec<String> = mc
            .effects
            .iter()
            .filter(|effect| effect.name == "tellraw" && effect.args.contains("minewell: "))
            .map(|effect| effect.args.clone())
            .collect();
        said.extend(mc.diagnostics.iter().cloned());
        if said.is_empty() {
            println!("test {id} ... ok");
        } else {
            failed += 1;
            println!("test {id} ... FAILED");
            for line in said {
                println!("    {line}");
            }
        }
    }
    let passed = pack.tests.len() - failed;
    println!(
        "\ntest result: {}. {passed} passed; {failed} failed",
        if failed == 0 { "ok" } else { "FAILED" }
    );
    Ok(failed == 0)
}

/// `data/<ns>/function/a/b.mcfunction` into `<ns>:a/b`.
fn function_id(path: &str) -> Option<String> {
    let rest = path.strip_prefix("data/")?;
    let (namespace, rest) = rest.split_once('/')?;
    let name = rest
        .strip_prefix("function/")?
        .strip_suffix(".mcfunction")?;
    Some(format!("{namespace}:{name}"))
}

fn install_toolchain(version: &str) -> miette::Result<String> {
    let dir = Toolchains::default().install(version, mwlc::toolchain::RELEASES)?;
    Ok(format!("installed {version} into {}", dir.display()))
}

fn add_toolchain(version: &str, path: &str, format: &str) -> miette::Result<String> {
    let pack_format = format
        .parse()
        .map_err(|_| miette::miette!("'{format}' is not a pack format number"))?;
    let dir = Toolchains::default().add(version, &PathBuf::from(path), pack_format)?;
    Ok(format!("installed {version} into {}", dir.display()))
}

fn build(profile: Profile) -> miette::Result<String> {
    let root = PathBuf::from(".");
    let pack = driver::build(&root, profile)?;
    let out = root.join(OUTPUT_DIR);
    pack.write_to(&out)
        .map_err(|source| miette::miette!("could not write to {}: {source}", out.display()))?;

    // Going over `maxCommandChainLength` is not an error in Minecraft: the chain stops
    // and the rest of the tick silently does not happen. Saying so here is the only
    // warning anyone gets (requirements section 16.1).
    let costs = root.join("target").join(COST_FILE);
    std::fs::write(&costs, cost::table(&pack.costs))
        .map_err(|source| miette::miette!("could not write to {}: {source}", costs.display()))?;
    for over in pack
        .costs
        .iter()
        .filter(|cost| !cost.loops && cost.commands > cost::MAX_COMMAND_CHAIN)
    {
        eprintln!(
            "warning: {} runs {} commands, over maxCommandChainLength ({})",
            over.path,
            over.commands,
            cost::MAX_COMMAND_CHAIN
        );
    }

    Ok(format!(
        "wrote {} file(s) to {}, and costs to {}",
        pack.files.len(),
        out.display(),
        costs.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_project_builds_as_it_stands() {
        // The template is the first thing anyone sees; a broken one is a broken
        // first impression.
        let dir = tempfile::tempdir().expect("temp dir");
        new_project_in(dir.path(), "demo").expect("creates");
        let root = dir.path().join("demo");
        let pack = driver::build(&root, Profile::Debug).expect("builds");
        assert!(
            pack.files
                .contains_key("data/demo/function/setup.mcfunction")
        );
    }

    #[test]
    fn the_template_passes_its_own_test() {
        let dir = tempfile::tempdir().expect("temp dir");
        new_project_in(dir.path(), "demo").expect("creates");
        assert!(run_tests_in(&dir.path().join("demo")).expect("runs"));
    }

    #[test]
    fn a_failing_check_fails_the_test() {
        let dir = tempfile::tempdir().expect("temp dir");
        new_project_in(dir.path(), "demo").expect("creates");
        let root = dir.path().join("demo");
        std::fs::write(
            root.join(driver::ROOT_SOURCE),
            "#[test]\nfn wrong() { let a = 1; debug_assert!(a == 2, \"one is not two\"); }\n",
        )
        .expect("write");
        assert!(!run_tests_in(&root).expect("runs"));
    }

    #[test]
    fn a_name_that_cannot_be_a_namespace_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        // A capital would make a datapack path Minecraft will not load.
        assert!(new_project_in(dir.path(), "Demo").is_err());
    }

    #[test]
    fn function_ids_come_from_the_datapack_layout() {
        assert_eq!(
            function_id("data/demo/function/a/b.mcfunction").as_deref(),
            Some("demo:a/b")
        );
        assert_eq!(function_id("pack.mcmeta"), None);
    }
}
