// SPDX-License-Identifier: MIT

//! The `mwl` command-line interface.

use std::path::PathBuf;
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
    mwl build [--release]
        compile the project into target/datapack, and write the per-function
        command counts to target/cost.txt

    mwl toolchain list
        show the installed Minecraft versions

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
        ["build", rest @ ..] => {
            let profile = if rest.contains(&"--release") {
                Profile::Release
            } else {
                Profile::Debug
            };
            match build(profile) {
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
