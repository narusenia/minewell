//! NBT paths: the `foo.bar[0]{k:1}` notation `data` commands address values with.
//!
//! A path resolves to zero or more values, which is why every operation here reports a
//! count rather than an `Option`. Commands turn "matched nothing" into a success count
//! of 0, never into an error.

use crate::nbt::{Compound, NbtValue};
use crate::snbt::{self, SnbtError};

#[derive(Debug, Clone, PartialEq)]
pub struct NbtPath(Vec<Step>);

#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// A named child of a compound.
    Child(String),
    /// Keeps the current value only if it matches the filter.
    Match(Compound),
    /// One element by position; negative counts from the end.
    Index(i32),
    /// Every element.
    All,
    /// The elements matching a filter.
    Elements(Compound),
}

impl NbtPath {
    pub fn parse(src: &str) -> Result<Self, SnbtError> {
        let mut steps = Vec::new();
        let mut at = 0usize;

        // The head may be a bare filter, matching the root itself.
        if src[at..].starts_with('{') {
            let (filter, next) = snbt::parse_compound_at(src, at)?;
            steps.push(Step::Match(filter));
            at = next;
        } else {
            at = segment(src, at, &mut steps)?;
        }
        while at < src.len() {
            if !src[at..].starts_with('.') {
                return Err(err(at, "expected '.'"));
            }
            at = segment(src, at + 1, &mut steps)?;
        }
        if steps.is_empty() {
            return Err(err(0, "empty path"));
        }
        Ok(NbtPath(steps))
    }

    /// Every value the path matches, cloned.
    ///
    /// Cloning keeps typed-array elements (`[I;1,2]`) expressible as values, which a
    /// borrow of the array cannot be. The cost is irrelevant at the sizes an
    /// interpreter used from unit tests deals with.
    pub fn resolve(&self, root: &NbtValue) -> Vec<NbtValue> {
        let mut current = vec![root.clone()];
        for step in &self.0 {
            let mut next = Vec::new();
            for value in &current {
                step.read(value, &mut next);
            }
            current = next;
        }
        current
    }

    /// Applies `f` to every matched value, creating missing compounds along named
    /// steps. Returns how many values were visited.
    pub fn modify(&self, root: &mut NbtValue, f: &mut impl FnMut(&mut NbtValue)) -> usize {
        self.modify_creating(root, NbtValue::Compound(Compound::new()), f)
    }

    /// As [`NbtPath::modify`], but a created leaf takes the shape of `leaf` rather
    /// than an empty compound. `data modify ... append` needs an empty list there.
    pub fn modify_creating(
        &self,
        root: &mut NbtValue,
        leaf: NbtValue,
        f: &mut impl FnMut(&mut NbtValue),
    ) -> usize {
        walk_inner(&self.0, root, Some(&leaf), f)
    }

    /// Writes `value` to every match. Returns how many were written.
    pub fn set(&self, root: &mut NbtValue, value: NbtValue) -> usize {
        self.modify(root, &mut |slot| *slot = value.clone())
    }

    /// Detaches every match from its parent. Returns how many were removed.
    pub fn remove(&self, root: &mut NbtValue) -> usize {
        let Some((last, init)) = self.0.split_last() else {
            return 0;
        };
        let mut removed = 0;
        walk_existing(init, root, &mut |parent| removed += detach(parent, last));
        removed
    }
}

fn segment(src: &str, mut at: usize, steps: &mut Vec<Step>) -> Result<usize, SnbtError> {
    let (name, next) = name(src, at)?;
    steps.push(Step::Child(name));
    at = next;
    if src[at..].starts_with('{') {
        let (filter, next) = snbt::parse_compound_at(src, at)?;
        steps.push(Step::Match(filter));
        at = next;
    }
    while src[at..].starts_with('[') {
        at = index(src, at, steps)?;
    }
    Ok(at)
}

fn name(src: &str, at: usize) -> Result<(String, usize), SnbtError> {
    if src[at..].starts_with('"') {
        let (value, next) = quoted(src, at)?;
        return Ok((value, next));
    }
    let end = src[at..]
        .find(|c: char| c.is_whitespace() || matches!(c, '.' | '[' | ']' | '{' | '}' | '"' | '\''))
        .map_or(src.len(), |i| at + i);
    if end == at {
        return Err(err(at, "expected a path element"));
    }
    Ok((src[at..end].to_owned(), end))
}

fn quoted(src: &str, at: usize) -> Result<(String, usize), SnbtError> {
    let mut out = String::new();
    let mut chars = src[at + 1..].char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some((_, escaped @ ('\\' | '"'))) => out.push(escaped),
                _ => return Err(err(at + i, "bad escape")),
            },
            '"' => return Ok((out, at + 1 + i + 1)),
            _ => out.push(c),
        }
    }
    Err(err(at, "unterminated quoted name"))
}

fn index(src: &str, at: usize, steps: &mut Vec<Step>) -> Result<usize, SnbtError> {
    let inner = at + 1;
    if src[inner..].starts_with(']') {
        steps.push(Step::All);
        return Ok(inner + 1);
    }
    if src[inner..].starts_with('{') {
        let (filter, next) = snbt::parse_compound_at(src, inner)?;
        if !src[next..].starts_with(']') {
            return Err(err(next, "expected ']'"));
        }
        steps.push(Step::Elements(filter));
        return Ok(next + 1);
    }
    let end = src[inner..]
        .find(']')
        .map(|i| inner + i)
        .ok_or_else(|| err(at, "unterminated '['"))?;
    let n = src[inner..end]
        .parse::<i32>()
        .map_err(|_| err(inner, "expected an integer index"))?;
    steps.push(Step::Index(n));
    Ok(end + 1)
}

fn err(at: usize, message: &str) -> SnbtError {
    SnbtError {
        at,
        message: message.to_owned(),
    }
}

impl Step {
    fn read(&self, value: &NbtValue, out: &mut Vec<NbtValue>) {
        match self {
            Step::Child(name) => {
                if let NbtValue::Compound(fields) = value
                    && let Some(child) = fields.get(name)
                {
                    out.push(child.clone());
                }
            }
            Step::Match(filter) => {
                if matches(value, filter) {
                    out.push(value.clone());
                }
            }
            Step::Index(n) => {
                let items = elements(value);
                if let Some(i) = normalize(*n, items.len()) {
                    out.push(items[i].clone());
                }
            }
            Step::All => out.extend(elements(value)),
            Step::Elements(filter) => {
                out.extend(elements(value).into_iter().filter(|v| matches(v, filter)));
            }
        }
    }
}

/// Elements of a list or a typed array, as values.
fn elements(value: &NbtValue) -> Vec<NbtValue> {
    match value {
        NbtValue::List(items) => items.clone(),
        NbtValue::ByteArray(items) => items.iter().copied().map(NbtValue::Byte).collect(),
        NbtValue::IntArray(items) => items.iter().copied().map(NbtValue::Int).collect(),
        NbtValue::LongArray(items) => items.iter().copied().map(NbtValue::Long).collect(),
        _ => Vec::new(),
    }
}

fn normalize(index: i32, len: usize) -> Option<usize> {
    let i = if index < 0 {
        len.checked_sub(index.unsigned_abs() as usize)?
    } else {
        index as usize
    };
    (i < len).then_some(i)
}

/// Partial, recursive match. A non-compound filter value must be equal, tag included.
fn matches(value: &NbtValue, filter: &Compound) -> bool {
    let NbtValue::Compound(fields) = value else {
        return false;
    };
    filter.iter().all(|(key, expected)| {
        fields.get(key).is_some_and(|actual| match expected {
            NbtValue::Compound(nested) => matches(actual, nested),
            other => actual == other,
        })
    })
}

/// Walks to every match without creating anything.
fn walk_existing(steps: &[Step], value: &mut NbtValue, f: &mut impl FnMut(&mut NbtValue)) -> usize {
    walk_inner(steps, value, None, f)
}

/// `leaf` is `Some` when missing values may be created; it is the shape to give the
/// final one.
fn walk_inner(
    steps: &[Step],
    value: &mut NbtValue,
    leaf: Option<&NbtValue>,
    f: &mut impl FnMut(&mut NbtValue),
) -> usize {
    let Some((step, rest)) = steps.split_first() else {
        f(value);
        return 1;
    };
    match step {
        Step::Child(name) => {
            let NbtValue::Compound(fields) = value else {
                return 0;
            };
            if let Some(leaf) = leaf
                && !fields.contains_key(name)
            {
                // Vanilla picks the created tag from what the *next* step addresses:
                // an index wants a list, anything else wants a compound. At the end of
                // the path the operation decides instead.
                let created = match rest.first() {
                    None => leaf.clone(),
                    next => empty_parent_for(next),
                };
                fields.insert(name.clone(), created);
            }
            match fields.get_mut(name) {
                Some(child) => walk_inner(rest, child, leaf, f),
                None => 0,
            }
        }
        Step::Match(filter) => {
            if matches(value, filter) {
                walk_inner(rest, value, leaf, f)
            } else {
                0
            }
        }
        // Positions and filters address values that must already exist; a write can
        // extend an object graph but cannot conjure list elements.
        Step::Index(n) => {
            let NbtValue::List(items) = value else {
                return 0;
            };
            match normalize(*n, items.len()) {
                Some(i) => walk_inner(rest, &mut items[i], leaf, f),
                None => 0,
            }
        }
        Step::All => {
            let NbtValue::List(items) = value else {
                return 0;
            };
            items
                .iter_mut()
                .map(|item| walk_inner(rest, item, leaf, f))
                .sum()
        }
        Step::Elements(filter) => {
            let NbtValue::List(items) = value else {
                return 0;
            };
            items
                .iter_mut()
                .filter(|item| matches(item, filter))
                .map(|item| walk_inner(rest, item, leaf, f))
                .sum()
        }
    }
}

/// The empty value to create so that `next` has something of the right shape to
/// address. Mirrors vanilla's "preferred parent" rule.
fn empty_parent_for(next: Option<&Step>) -> NbtValue {
    match next {
        Some(Step::Index(_) | Step::All | Step::Elements(_)) => NbtValue::List(Vec::new()),
        _ => NbtValue::Compound(Compound::new()),
    }
}

/// Removes what `step` addresses from `parent`, reporting how many went.
fn detach(parent: &mut NbtValue, step: &Step) -> usize {
    match step {
        Step::Child(name) => match parent {
            NbtValue::Compound(fields) => fields.remove(name).is_some() as usize,
            _ => 0,
        },
        Step::Index(n) => {
            let NbtValue::List(items) = parent else {
                return 0;
            };
            match normalize(*n, items.len()) {
                Some(i) => {
                    items.remove(i);
                    1
                }
                None => 0,
            }
        }
        Step::All => {
            let NbtValue::List(items) = parent else {
                return 0;
            };
            std::mem::take(items).len()
        }
        Step::Elements(filter) => {
            let NbtValue::List(items) = parent else {
                return 0;
            };
            let before = items.len();
            items.retain(|item| !matches(item, filter));
            before - items.len()
        }
        // Removing a filtered value would mean removing it from its own parent, which
        // this step no longer has a handle on. Out of scope; see SPEC.md.
        Step::Match(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nbt::NbtValue::*;
    use crate::snbt;

    fn v(src: &str) -> NbtValue {
        snbt::parse(src).unwrap()
    }

    fn path(src: &str) -> NbtPath {
        NbtPath::parse(src).unwrap()
    }

    fn read(p: &str, root: &str) -> Vec<NbtValue> {
        path(p).resolve(&v(root))
    }

    #[test]
    fn nested_children() {
        assert_eq!(read("a.b.c", "{a:{b:{c:7}}}"), vec![Int(7)]);
        assert_eq!(read("a.missing", "{a:{b:1}}"), vec![]);
    }

    #[test]
    fn indexing_a_list() {
        let root = "{l:[10,20,30]}";
        assert_eq!(read("l[0]", root), vec![Int(10)]);
        assert_eq!(read("l[-1]", root), vec![Int(30)]);
        assert_eq!(read("l[9]", root), vec![]);
        assert_eq!(read("l[]", root), vec![Int(10), Int(20), Int(30)]);
    }

    #[test]
    fn indexing_reaches_into_typed_arrays() {
        assert_eq!(read("a[1]", "{a:[I;5,6]}"), vec![Int(6)]);
        assert_eq!(read("a[0]", "{a:[B;7b]}"), vec![Byte(7)]);
    }

    #[test]
    fn filters_match_partially_and_recursively() {
        let root = r#"{l:[{id:"a",n:{k:1,extra:2}},{id:"b"}]}"#;
        assert_eq!(read(r#"l[{id:"a"}].id"#, root), vec![String("a".into())]);
        assert_eq!(read("l[{n:{k:1}}].id", root), vec![String("a".into())]);
        assert_eq!(read("l[{n:{k:2}}].id", root), vec![]);
    }

    #[test]
    fn a_filter_distinguishes_tags() {
        // Byte(1) must not satisfy a filter asking for Int(1).
        assert_eq!(read("l[{k:1}]", "{l:[{k:1b}]}"), vec![]);
        assert_eq!(read("l[{k:1b}]", "{l:[{k:1b}]}").len(), 1);
    }

    #[test]
    fn filter_on_a_named_step_and_at_the_root() {
        assert_eq!(read("a{k:1}.k", "{a:{k:1}}"), vec![Int(1)]);
        assert_eq!(read("a{k:2}.k", "{a:{k:1}}"), vec![]);
        assert_eq!(read("{k:1}.k", "{k:1}"), vec![Int(1)]);
        assert_eq!(read("{k:2}.k", "{k:1}"), vec![]);
    }

    #[test]
    fn quoted_names_allow_awkward_keys() {
        assert_eq!(read(r#""a.b".c"#, r#"{"a.b":{c:1}}"#), vec![Int(1)]);
    }

    #[test]
    fn writing_creates_missing_compounds_along_named_steps() {
        let mut root = v("{}");
        let n = path("a.b.c").set(&mut root, Int(5));
        assert_eq!(n, 1);
        assert_eq!(root, v("{a:{b:{c:5}}}"));
    }

    #[test]
    fn writing_never_creates_list_elements() {
        // Vanilla creates the container the next step wants — here a list — and then
        // matches nothing, leaving the half-built structure behind. Modelled as-is.
        let mut root = v("{}");
        assert_eq!(path("a[0].b").set(&mut root, Int(5)), 0);
        assert_eq!(root, v("{a:[]}"));
    }

    #[test]
    fn writing_hits_every_match() {
        let mut root = v("{l:[{k:1},{k:1},{k:2}]}");
        assert_eq!(path("l[{k:1}].k").set(&mut root, Int(9)), 2);
        assert_eq!(root, v("{l:[{k:9},{k:9},{k:2}]}"));
    }

    #[test]
    fn removal_detaches_and_counts() {
        let mut root = v("{l:[1,2,3],a:{b:1}}");
        assert_eq!(path("l[1]").remove(&mut root), 1);
        assert_eq!(root, v("{l:[1,3],a:{b:1}}"));
        assert_eq!(path("a.b").remove(&mut root), 1);
        assert_eq!(root, v("{l:[1,3],a:{}}"));
        assert_eq!(path("a.missing").remove(&mut root), 0);
    }

    #[test]
    fn removing_several_elements_at_once() {
        let mut root = v("{l:[{k:1},{k:2},{k:1}]}");
        assert_eq!(path("l[{k:1}]").remove(&mut root), 2);
        assert_eq!(root, v("{l:[{k:2}]}"));
    }

    #[test]
    fn malformed_paths_are_rejected() {
        assert!(NbtPath::parse("").is_err());
        assert!(NbtPath::parse("a[").is_err());
        assert!(NbtPath::parse("a..b").is_err());
        assert!(NbtPath::parse("a.").is_err());
    }
}
