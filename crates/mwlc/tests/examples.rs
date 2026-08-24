// SPDX-License-Identifier: MIT

//! Every example in `examples/` must compile.
//!
//! Examples are documentation, and documentation that no longer compiles is worse than
//! none: it teaches the wrong thing with the authority of being checked in. This is the
//! cheapest way to keep them honest.

use std::path::PathBuf;

use mwlc::driver;
use mwlc::emit::Profile;

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn projects() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(examples_dir())
        .expect("examples/ exists")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            path.join("minewell.toml").exists().then_some(path)
        })
        .collect();
    found.sort();
    found
}

#[test]
fn there_are_examples_to_check() {
    // Otherwise the test below passes by finding nothing.
    assert!(projects().len() >= 4, "{:?}", projects());
}

#[test]
fn every_example_compiles() {
    for project in projects() {
        let name = project.file_name().expect("a directory name").to_owned();
        for profile in [Profile::Debug, Profile::Release] {
            match driver::build(&project, profile) {
                Ok(pack) => assert!(
                    !pack.files.is_empty(),
                    "{name:?} produced nothing in {profile:?}"
                ),
                Err(report) => panic!("{name:?} does not compile in {profile:?}:\n{report:?}"),
            }
        }
    }
}
