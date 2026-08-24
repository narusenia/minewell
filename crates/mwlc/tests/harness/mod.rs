// SPDX-License-Identifier: MIT

//! Compile `.mwl` source and run the result, in one call.
//!
//! Every milestone from here on asserts on behaviour through this: the question a test
//! should ask is "what does this program *do*", not "what text did we emit". Snapshots
//! are for reviewing the shape of output; this is for deciding whether it is right.

use mwlc::driver;
use mwlc::emit::{Options, Source};
use tinymcf::Interpreter;

/// The namespace compiled programs are placed in.
pub const NS: &str = "test";

/// Compiles `src` and loads it into a fresh interpreter, ready to `call`.
///
/// Compiles in debug profile on purpose: the source-line comments it inserts must not
/// change what the pack does, and running it here is what proves that.
pub fn load(src: &str) -> Interpreter {
    let options = Options {
        source: Some(Source {
            path: "test.mwl".to_owned(),
            text: src.to_owned(),
        }),
        ..Options::default()
    };
    let pack = match driver::compile(src, NS, &options) {
        Ok(pack) => pack,
        Err(report) => panic!("compiling failed:\n{report:?}"),
    };

    let mut mc = Interpreter::default();
    for (path, text) in &pack.files {
        if let Some(id) = function_id(path) {
            mc.load(&id, text)
                .unwrap_or_else(|e| panic!("{id} does not parse as mcfunction: {e}\n{text}"));
        }
    }
    // The pack's load tag runs this in a real world; here the harness stands in for it.
    mc.call(&format!("{NS}:{}", mwlc::emit::INIT_FUNCTION));
    assert!(mc.diagnostics.is_empty(), "{:?}", mc.diagnostics);
    // Setup is not the program, so cost measurements start from zero here.
    mc.commands_run = 0;
    mc
}

/// The value of a binding, by the name the author wrote.
pub fn local(mc: &Interpreter, function: &str, name: &str) -> Option<i32> {
    mc.world
        .scoreboard
        .get(&format!("{NS}.v"), &format!("${function}.{name}"))
        .expect("the objective exists")
}

/// Declares entities and what a selector finds, standing in for a world.
pub fn zombies(mc: &mut Interpreter, ids: &[&str]) {
    for (i, id) in ids.iter().enumerate() {
        mc.world.spawn(id, [i as f64, 64.0, 0.0]);
    }
    mc.world.bind_selector("@e[type=zombie]", ids.to_vec());
}

/// How many commands the last run cost. Generated command count is a requirement, not
/// a detail, so tests are allowed to care.
pub fn cost(mc: &Interpreter) -> u64 {
    mc.commands_run
}

/// Compiles and calls `test:main`, returning the interpreter to assert against.
pub fn run(src: &str) -> Interpreter {
    let mut mc = load(src);
    mc.call(&format!("{NS}:main"));
    assert!(mc.diagnostics.is_empty(), "{:?}", mc.diagnostics);
    mc
}

/// `data/test/function/a/b.mcfunction` into `test:a/b`.
fn function_id(path: &str) -> Option<String> {
    let rest = path.strip_prefix("data/")?;
    let (namespace, rest) = rest.split_once('/')?;
    let rest = rest.strip_prefix("function/")?;
    let name = rest.strip_suffix(".mcfunction")?;
    Some(format!("{namespace}:{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_ids_come_from_the_datapack_layout() {
        assert_eq!(
            function_id("data/test/function/main.mcfunction").as_deref(),
            Some("test:main")
        );
        assert_eq!(
            function_id("data/ns/function/a/b.mcfunction").as_deref(),
            Some("ns:a/b")
        );
        assert_eq!(function_id("pack.mcmeta"), None);
    }
}
