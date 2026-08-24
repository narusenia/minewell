// SPDX-License-Identifier: MIT

//! The `mwl` command-line interface.

use std::path::PathBuf;
use std::process::ExitCode;

use mwlc::driver::{self, OUTPUT_DIR};
use mwlc::emit::Profile;

const USAGE: &str = "\
mwl — the minewell compiler

usage:
    mwl build [--release]    compile the project into target/datapack
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
        _ => {
            eprint!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn build(profile: Profile) -> miette::Result<String> {
    let root = PathBuf::from(".");
    let pack = driver::build(&root, profile)?;
    let out = root.join(OUTPUT_DIR);
    pack.write_to(&out)
        .map_err(|source| miette::miette!("could not write to {}: {source}", out.display()))?;
    Ok(format!(
        "wrote {} file(s) to {}",
        pack.files.len(),
        out.display()
    ))
}
