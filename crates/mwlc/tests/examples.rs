// SPDX-License-Identifier: MIT

//! Every example in `examples/` must compile.
//!
//! Examples are documentation, and documentation that no longer compiles is worse than
//! none: it teaches the wrong thing with the authority of being checked in. This is the
//! cheapest way to keep them honest.

use std::path::PathBuf;

use mwlc::driver;
use mwlc::emit::Profile;
use mwlc::toolchain::Toolchains;

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

/// The command set the examples build against.
///
/// Checked in beside them rather than installed: the real `commands.json` needs
/// Minecraft's data generator, and an example that cannot be compiled by anyone who
/// clones the repository is not documentation (`examples/toolchains/README.md`).
fn toolchains() -> Toolchains {
    Toolchains {
        root: examples_dir().join("toolchains"),
    }
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

/// The biggest example, run rather than only compiled.
///
/// It is the one that puts the whole language together, so it is the one worth
/// checking still means what its comments say.
#[test]
fn the_arena_example_does_what_it_says() {
    let pack = driver::build_with(&examples_dir().join("arena"), Profile::Debug, &toolchains())
        .expect("compiles");
    let mut mc = tinymcf::Interpreter::default();
    for (path, text) in &pack.files {
        if let Some(rest) = path.strip_prefix("data/arena/function/") {
            let id = format!(
                "arena:{}",
                rest.strip_suffix(".mcfunction").expect("a function")
            );
            mc.load(&id, text).expect("parses as mcfunction");
        }
    }
    mc.call("arena:__init");

    let zombies: Vec<String> = (0..7).map(|i| format!("z{i}")).collect();
    for (i, id) in zombies.iter().enumerate() {
        mc.world.spawn(id, [i as f64, 64.0, 0.0]);
    }
    // The first one is nearly dead; the rest have no Health key at all, which is
    // what `Option` reads as `None`.
    mc.world.entity_mut("z0").expect("spawned").nbt =
        tinymcf::snbt::parse("{Health:2.5f}").expect("snbt");
    mc.world
        .bind_selector("@e[type=zombie, distance=..12]", zombies);
    mc.call("arena:tick");

    // Nothing here may fail: entity NBT is modelled now, so silence is the whole
    // assertion (`crates/tinymcf/SPEC.md` section 4.2).
    assert!(mc.diagnostics.is_empty(), "{:?}", mc.diagnostics);

    let effects: Vec<(&str, &str)> = mc
        .effects
        .iter()
        .map(|e| (e.name.as_str(), e.args.as_str()))
        .collect();
    // Three of the seven are sealed, and only three: the fourth iteration hits the cap
    // and skips the rest of the body.
    assert_eq!(
        effects
            .iter()
            .filter(|(name, _)| *name == "setblock")
            .count(),
        3,
        "{effects:?}"
    );
    // The view wrote through to the entity, and the one with a Health below 5 made
    // the pling sound.
    let z0 = mc.world.entity("z0").expect("spawned").nbt.to_string();
    assert!(z0.contains("Glowing:1b"), "{z0}");
    assert!(
        effects.contains(&("playsound", "minecraft:block.note_block.pling master @a")),
        "{effects:?}"
    );
    // Seven is a swarm, so the loud arm runs and the quiet one does not.
    assert!(
        effects.contains(&("playsound", "minecraft:entity.wither.spawn master @a")),
        "{effects:?}"
    );
    // `text!` named the score rather than reading it, so the JSON carries the fake
    // player the binding lives under (spec section 6.32).
    let told: Vec<&str> = effects
        .iter()
        .filter(|(name, _)| *name == "tellraw")
        .map(|(_, args)| *args)
        .collect();
    assert_eq!(told.len(), 1, "{effects:?}");
    assert!(
        told[0].contains(r#"{"score":{"name":"$tick.count","objective":"arena.v"}}"#),
        "{told:?}"
    );
    assert!(told[0].contains(r#""color":"red""#), "{told:?}");

    // The load function is where the fixed-point arithmetic and the nbt! literal are.
    // A radius of 8 gives an area of about 201.088, which is over the threshold, and
    // the name matches the literal it is compared against.
    mc.effects.clear();
    mc.commands_run = 0;
    mc.call("arena:setup");
    let said: Vec<&str> = mc
        .effects
        .iter()
        .filter(|e| e.name == "say")
        .map(|e| e.args.as_str())
        .collect();
    assert!(said.contains(&"a big arena"), "{said:?}");
    assert!(said.contains(&"the pit is open"), "{said:?}");

    // The static count is what `target/cost.txt` reports, and it assumes every guard
    // holds. `setup` has no loop and every one of its guards does hold here, so the
    // two numbers are the same (requirements section 16.1).
    let stated = pack
        .costs
        .iter()
        .find(|cost| cost.path == "arena:setup")
        .expect("setup is in the table");
    assert!(!stated.loops, "setup has no loop in it");
    assert_eq!(stated.commands, mc.commands_run, "{:?}", stated);
}

#[test]
fn there_are_examples_to_check() {
    // Otherwise the test below passes by finding nothing.
    assert!(projects().len() >= 5, "{:?}", projects());
}

#[test]
fn every_example_compiles() {
    for project in projects() {
        let name = project.file_name().expect("a directory name").to_owned();
        for profile in [Profile::Debug, Profile::Release] {
            match driver::build_with(&project, profile, &toolchains()) {
                Ok(pack) => assert!(
                    !pack.files.is_empty(),
                    "{name:?} produced nothing in {profile:?}"
                ),
                Err(report) => panic!("{name:?} does not compile in {profile:?}:\n{report:?}"),
            }
        }
    }
}
