// SPDX-License-Identifier: MIT

//! The command surface of one Minecraft version, read from its `commands.json`.
//!
//! The compiler knows the *language*; a toolchain knows the *commands*
//! (`docs/01-requirements.md` section 1.2). Nothing about Minecraft's command set is
//! written into this crate — it is all derived from the brigadier tree Mojang's data
//! generator emits, so a new version is a data refresh rather than a code change.

use std::collections::BTreeMap;

use serde::Deserialize;

/// A node of the brigadier command tree, as `commands.json` spells it.
#[derive(Debug, Clone, Deserialize)]
struct Node {
    #[serde(rename = "type")]
    kind: NodeKind,
    #[serde(default)]
    children: BTreeMap<String, Node>,
    #[serde(default)]
    executable: bool,
    /// Present on `argument` nodes: the brigadier parser that reads it.
    parser: Option<String>,
    /// A node can forward to another, which is how `/execute run` loops back to root.
    redirect: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum NodeKind {
    Root,
    Literal,
    Argument,
}

/// One callable command signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// The generated name: the literal path joined with underscores.
    pub name: String,
    /// The literal words the command is made of, in order.
    pub literals: Vec<String>,
    pub params: Vec<Param>,
    /// The command as it has to be written: words and arguments interleaved. A literal
    /// can follow an argument (`playsound <sound> master <targets>`), so the order
    /// cannot be recovered from the two lists above.
    pub parts: Vec<Part>,
}

/// One piece of a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Literal(String),
    /// An argument, by its index in `params`.
    Arg(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub ty: ArgType,
    /// The brigadier parser this came from. Kept because the type alone cannot say
    /// whether a resource location names a function, a predicate or a block.
    pub parser: String,
}

/// What a command argument accepts, in minewell's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    I32,
    Bool,
    Str,
    Selector,
    Pos,
    Resource,
    Nbt,
    Component,
    /// A brigadier parser with no counterpart here. Accepts a string literal and does
    /// not look inside it.
    Raw,
}

impl ArgType {
    pub fn name(&self) -> &'static str {
        match self {
            ArgType::I32 => "i32",
            ArgType::Bool => "bool",
            ArgType::Str => "String",
            ArgType::Selector => "selector",
            ArgType::Pos => "Pos",
            ArgType::Resource => "ResourceLocation",
            ArgType::Nbt => "Nbt",
            ArgType::Component => "TextComponent",
            ArgType::Raw => "RawArg",
        }
    }
}

/// Brigadier parser name to argument type. Requirements section 1.4.
fn arg_type(parser: &str) -> Option<ArgType> {
    Some(match parser {
        "brigadier:integer" | "brigadier:long" => ArgType::I32,
        "brigadier:bool" => ArgType::Bool,
        "brigadier:string" | "minecraft:message" => ArgType::Str,
        "brigadier:double" | "brigadier:float" => ArgType::Raw,
        "minecraft:entity" | "minecraft:game_profile" | "minecraft:score_holder" => {
            ArgType::Selector
        }
        "minecraft:block_pos" | "minecraft:vec3" | "minecraft:vec2" | "minecraft:column_pos" => {
            ArgType::Pos
        }
        "minecraft:resource_location"
        | "minecraft:function"
        | "minecraft:block_state"
        | "minecraft:item_stack"
        | "minecraft:entity_summon" => ArgType::Resource,
        "minecraft:nbt_compound_tag" | "minecraft:nbt_tag" | "minecraft:nbt_path" => ArgType::Nbt,
        "minecraft:component" | "minecraft:style" => ArgType::Component,
        _ => return None,
    })
}

#[derive(Debug, Clone, Default)]
pub struct Schema {
    /// By generated name.
    pub commands: BTreeMap<String, Signature>,
    /// Parser names that had no counterpart, so a toolchain build can say what it
    /// approximated rather than leaving it to be discovered later.
    pub unknown_parsers: Vec<String>,
}

impl Schema {
    /// Reads a `commands.json`.
    ///
    /// An unknown parser is a warning, not an error: a snapshot that adds one argument
    /// type must not make the whole toolchain unbuildable.
    pub fn parse(json: &str) -> Result<Schema, serde_json::Error> {
        let root: Node = serde_json::from_str(json)?;
        let mut schema = Schema::default();
        let mut unknown = std::collections::BTreeSet::new();
        walk(
            &root,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
            &mut schema,
            &mut unknown,
        );
        schema.unknown_parsers = unknown.into_iter().collect();
        Ok(schema)
    }

    pub fn get(&self, name: &str) -> Option<&Signature> {
        self.commands.get(name)
    }

    /// Applies hand-written renames.
    ///
    /// Machine-generated names come from the literal path, which for the deeper
    /// commands produces things nobody would type
    /// (`data_modify_storage_target_path_set_from_entity`). Renaming those is the one
    /// piece of judgement the generator cannot supply. Nothing else is overridable:
    /// changing a signature by hand would put the command set back in human keeping,
    /// which is what generating it was meant to avoid.
    pub fn rename(&mut self, overrides: &Overrides) -> Vec<String> {
        let mut unmatched = Vec::new();
        for (from, to) in &overrides.rename {
            match self.commands.remove(from) {
                Some(mut signature) => {
                    signature.name = to.clone();
                    self.commands.insert(to.clone(), signature);
                }
                // A rename for a command this version does not have is not an error:
                // one file covers several versions.
                None => unmatched.push(from.clone()),
            }
        }
        unmatched
    }
}

/// `overrides.toml`, beside a toolchain's `commands.json`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Overrides {
    #[serde(default)]
    pub rename: BTreeMap<String, String>,
}

fn walk(
    node: &Node,
    literals: &mut Vec<String>,
    params: &mut Vec<Param>,
    parts: &mut Vec<Part>,
    out: &mut Schema,
    unknown: &mut std::collections::BTreeSet<String>,
) {
    if node.executable && !literals.is_empty() {
        let name = literals.join("_");
        // The first path to a name wins. Later ones are longer overloads of the same
        // literals, which `overrides.toml` is for.
        out.commands.entry(name.clone()).or_insert(Signature {
            name,
            literals: literals.clone(),
            params: params.clone(),
            parts: parts.clone(),
        });
    }
    // A redirect points back into the tree; following it would not terminate.
    if node.redirect.is_some() {
        return;
    }
    for (name, child) in &node.children {
        match child.kind {
            NodeKind::Root => {}
            NodeKind::Literal => {
                literals.push(name.clone());
                parts.push(Part::Literal(name.clone()));
                walk(child, literals, params, parts, out, unknown);
                parts.pop();
                literals.pop();
            }
            NodeKind::Argument => {
                let parser = child.parser.as_deref().unwrap_or("");
                let ty = match arg_type(parser) {
                    Some(ty) => ty,
                    None => {
                        unknown.insert(parser.to_owned());
                        ArgType::Raw
                    }
                };
                params.push(Param {
                    name: name.clone(),
                    ty,
                    parser: parser.to_owned(),
                });
                parts.push(Part::Arg(params.len() - 1));
                walk(child, literals, params, parts, out, unknown);
                parts.pop();
                params.pop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../../tests/fixtures/commands.json");

    fn schema() -> Schema {
        Schema::parse(SAMPLE).expect("the fixture parses")
    }

    #[test]
    fn a_command_with_only_literals() {
        let reload = schema().get("reload").expect("reload").clone();
        assert_eq!(reload.literals, vec!["reload"]);
        assert!(reload.params.is_empty());
    }

    #[test]
    fn names_come_from_the_literal_path() {
        let s = schema();
        assert!(s.get("setblock").is_some(), "{:?}", s.commands.keys());
        assert!(
            s.get("data_get_entity").is_some(),
            "{:?}",
            s.commands.keys()
        );
    }

    #[test]
    fn arguments_keep_their_order_and_get_a_type() {
        let setblock = schema().get("setblock").expect("setblock").clone();
        assert_eq!(
            setblock
                .params
                .iter()
                .map(|p| (p.name.as_str(), p.ty))
                .collect::<Vec<_>>(),
            vec![("pos", ArgType::Pos), ("block", ArgType::Resource)]
        );
    }

    #[test]
    fn a_shorter_overload_does_not_lose_to_a_longer_one() {
        // `/data get entity <target>` and `... <target> <path>` share a literal path;
        // the first executable node found is the one that keeps the name.
        let get = schema().get("data_get_entity").expect("present").clone();
        assert_eq!(get.params.len(), 1, "{:?}", get.params);
    }

    #[test]
    fn an_unknown_parser_becomes_raw_and_is_reported() {
        let s = schema();
        assert!(
            s.unknown_parsers.iter().any(|p| p == "minecraft:invented"),
            "{:?}",
            s.unknown_parsers
        );
        let odd = s.get("experiment").expect("present").clone();
        assert_eq!(odd.params[0].ty, ArgType::Raw);
    }

    #[test]
    fn a_rename_moves_a_command_to_a_usable_name() {
        let mut s = schema();
        let overrides = Overrides {
            rename: [("data_get_entity".to_owned(), "data_get".to_owned())]
                .into_iter()
                .collect(),
        };
        assert!(s.rename(&overrides).is_empty());
        assert!(s.get("data_get_entity").is_none());
        assert_eq!(s.get("data_get").expect("renamed").name, "data_get");
    }

    #[test]
    fn a_rename_for_a_command_this_version_lacks_is_reported_not_fatal() {
        // One overrides file covers several versions, so a name that is not here yet
        // (or not here any more) is information, not a failure.
        let mut s = schema();
        let overrides = Overrides {
            rename: [("nope".to_owned(), "whatever".to_owned())]
                .into_iter()
                .collect(),
        };
        assert_eq!(s.rename(&overrides), vec!["nope"]);
    }

    #[test]
    fn a_redirect_does_not_send_the_walk_round_forever() {
        // `/execute run` redirects to root. Following it would never terminate.
        assert!(schema().get("execute_run").is_some());
    }
}
