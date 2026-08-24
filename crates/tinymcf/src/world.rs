// SPDX-License-Identifier: MIT

//! Mutable state a running datapack can observe: the scoreboard and command storage.
//!
//! Entities and blocks are deliberately absent. Commands that touch them are recorded
//! as side effects rather than simulated.

use std::collections::{BTreeMap, BTreeSet};

use crate::Error;
use crate::nbt::NbtValue;

#[derive(Debug, Default, Clone)]
pub struct World {
    pub scoreboard: Scoreboard,
    storage: BTreeMap<String, NbtValue>,
    entities: BTreeMap<String, Entity>,
    /// What each selector text finds. There is no world to search, so the harness
    /// says. An unbound selector finds nothing, which is honest: nothing is there.
    selectors: BTreeMap<String, Vec<String>>,
}

/// A stub entity. Enough to be an executor and to stand at a position.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub id: String,
    pub pos: [f64; 3],
    pub nbt: NbtValue,
}

impl World {
    /// The root compound of a storage namespace. Absent namespaces read as empty,
    /// which is what vanilla does.
    pub fn storage(&self, namespace: &str) -> &NbtValue {
        static EMPTY: std::sync::LazyLock<NbtValue> =
            std::sync::LazyLock::new(|| NbtValue::Compound(Default::default()));
        self.storage.get(namespace).unwrap_or(&EMPTY)
    }

    /// Declares an entity the harness can then bind selectors to.
    pub fn spawn(&mut self, id: &str, pos: [f64; 3]) -> &mut Entity {
        self.entities.entry(id.to_owned()).or_insert(Entity {
            id: id.to_owned(),
            pos,
            nbt: NbtValue::Compound(Default::default()),
        })
    }

    /// Declares what a selector text finds, in order.
    pub fn bind_selector<I, S>(&mut self, selector: &str, ids: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.selectors.insert(
            selector.to_owned(),
            ids.into_iter().map(Into::into).collect(),
        );
    }

    pub fn entity(&self, id: &str) -> Option<&Entity> {
        self.entities.get(id)
    }

    /// The entities a selector finds. `@s` is the executor and needs no binding.
    pub fn resolve(&self, selector: &str, executor: Option<&str>) -> Vec<String> {
        if selector == "@s" {
            return executor.map(|id| vec![id.to_owned()]).unwrap_or_default();
        }
        self.selectors.get(selector).cloned().unwrap_or_default()
    }

    pub fn storage_mut(&mut self, namespace: &str) -> &mut NbtValue {
        self.storage
            .entry(namespace.to_owned())
            .or_insert_with(|| NbtValue::Compound(Default::default()))
    }
}

#[derive(Debug, Default, Clone)]
pub struct Scoreboard {
    objectives: BTreeSet<String>,
    /// Keyed by (objective, holder). A missing key means the holder has no score,
    /// which is distinct from having the score 0.
    scores: BTreeMap<(String, String), i32>,
}

impl Scoreboard {
    /// Idempotent, like `scoreboard objectives add`.
    pub fn add_objective(&mut self, objective: &str) {
        self.objectives.insert(objective.to_owned());
    }

    pub fn remove_objective(&mut self, objective: &str) {
        self.objectives.remove(objective);
        self.scores.retain(|(obj, _), _| obj != objective);
    }

    pub fn has_objective(&self, objective: &str) -> bool {
        self.objectives.contains(objective)
    }

    pub fn objectives(&self) -> impl Iterator<Item = &str> {
        self.objectives.iter().map(String::as_str)
    }

    pub fn get(&self, objective: &str, holder: &str) -> Result<Option<i32>, Error> {
        self.check(objective)?;
        Ok(self.scores.get(&key(objective, holder)).copied())
    }

    pub fn set(&mut self, objective: &str, holder: &str, value: i32) -> Result<(), Error> {
        self.check(objective)?;
        self.scores.insert(key(objective, holder), value);
        Ok(())
    }

    /// Reads a score, creating it as 0 if absent. This is what every writing command
    /// does in vanilla — including the *source* side of `players operation`.
    pub fn get_or_create(&mut self, objective: &str, holder: &str) -> Result<i32, Error> {
        self.check(objective)?;
        Ok(*self.scores.entry(key(objective, holder)).or_insert(0))
    }

    pub fn reset(&mut self, objective: &str, holder: &str) {
        self.scores.remove(&key(objective, holder));
    }

    /// `scoreboard players reset <holder>` with no objective.
    pub fn reset_all(&mut self, holder: &str) {
        self.scores.retain(|(_, h), _| h != holder);
    }

    fn check(&self, objective: &str) -> Result<(), Error> {
        if self.objectives.contains(objective) {
            Ok(())
        } else {
            Err(Error::NoSuchObjective(objective.to_owned()))
        }
    }
}

fn key(objective: &str, holder: &str) -> (String, String) {
    (objective.to_owned(), holder.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nbt::NbtValue;

    fn world_with_obj() -> World {
        let mut w = World::default();
        w.scoreboard.add_objective("obj");
        w
    }

    #[test]
    fn score_set_then_get() {
        let mut w = world_with_obj();
        w.scoreboard.set("obj", "$a", 7).unwrap();
        assert_eq!(w.scoreboard.get("obj", "$a"), Ok(Some(7)));
    }

    #[test]
    fn unset_holder_has_no_score() {
        let w = world_with_obj();
        assert_eq!(w.scoreboard.get("obj", "$a"), Ok(None));
    }

    #[test]
    fn undeclared_objective_is_an_error_not_a_silent_zero() {
        // Vanilla rejects the command outright. Modelling it as 0 would hide a
        // compiler bug where `scoreboard objectives add` was never emitted.
        let mut w = World::default();
        assert_eq!(
            w.scoreboard.set("nope", "$a", 1),
            Err(Error::NoSuchObjective("nope".into()))
        );
        assert_eq!(
            w.scoreboard.get("nope", "$a"),
            Err(Error::NoSuchObjective("nope".into()))
        );
    }

    #[test]
    fn removing_an_objective_drops_its_scores() {
        let mut w = world_with_obj();
        w.scoreboard.set("obj", "$a", 7).unwrap();
        w.scoreboard.remove_objective("obj");
        w.scoreboard.add_objective("obj");
        assert_eq!(w.scoreboard.get("obj", "$a"), Ok(None));
    }

    #[test]
    fn reset_drops_one_holder() {
        let mut w = world_with_obj();
        w.scoreboard.set("obj", "$a", 7).unwrap();
        w.scoreboard.set("obj", "$b", 8).unwrap();
        w.scoreboard.reset("obj", "$a");
        assert_eq!(w.scoreboard.get("obj", "$a"), Ok(None));
        assert_eq!(w.scoreboard.get("obj", "$b"), Ok(Some(8)));
    }

    #[test]
    fn storage_starts_as_an_empty_compound() {
        let w = World::default();
        assert_eq!(w.storage("ns:mw"), &NbtValue::Compound(Default::default()));
    }

    #[test]
    fn storage_is_per_namespace() {
        let mut w = World::default();
        *w.storage_mut("a:mw") = NbtValue::compound([("x", NbtValue::Int(1))]);
        assert_eq!(w.storage("b:mw"), &NbtValue::Compound(Default::default()));
    }
}
