// SPDX-License-Identifier: MIT

//! NBT values.
//!
//! The tag distinction is load-bearing: vanilla treats `Byte(1)` and `Int(1)` as
//! different values, and silently ignores data written with the wrong tag. Modelling
//! them as one integer type would hide exactly the class of bug this interpreter
//! exists to catch.

use std::collections::BTreeMap;

/// A compound's fields. Ordered so that output and snapshots are deterministic.
pub type Compound = BTreeMap<String, NbtValue>;

#[derive(Debug, Clone, PartialEq)]
pub enum NbtValue {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    String(String),
    List(Vec<NbtValue>),
    Compound(Compound),
    ByteArray(Vec<i8>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl NbtValue {
    /// Convenience constructor for a compound, in vanilla field order or any other.
    pub fn compound<K, I>(fields: I) -> Self
    where
        K: Into<String>,
        I: IntoIterator<Item = (K, NbtValue)>,
    {
        NbtValue::Compound(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// The tag name as vanilla spells it, for diagnostics.
    pub fn tag_name(&self) -> &'static str {
        match self {
            NbtValue::Byte(_) => "byte",
            NbtValue::Short(_) => "short",
            NbtValue::Int(_) => "int",
            NbtValue::Long(_) => "long",
            NbtValue::Float(_) => "float",
            NbtValue::Double(_) => "double",
            NbtValue::String(_) => "string",
            NbtValue::List(_) => "list",
            NbtValue::Compound(_) => "compound",
            NbtValue::ByteArray(_) => "byte_array",
            NbtValue::IntArray(_) => "int_array",
            NbtValue::LongArray(_) => "long_array",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_and_int_of_the_same_number_are_different_values() {
        // Vanilla treats Byte(1) and Int(1) as different tags, and silently ignores
        // data written with the wrong one. Equality must not paper over that.
        assert_ne!(NbtValue::Byte(1), NbtValue::Int(1));
        assert_ne!(NbtValue::Int(1), NbtValue::Long(1));
        assert_ne!(NbtValue::Float(1.0), NbtValue::Double(1.0));
    }

    #[test]
    fn same_tag_and_number_are_equal() {
        assert_eq!(NbtValue::Int(1), NbtValue::Int(1));
    }

    #[test]
    fn compound_equality_ignores_insertion_order() {
        let a = NbtValue::compound([("x", NbtValue::Int(1)), ("y", NbtValue::Int(2))]);
        let b = NbtValue::compound([("y", NbtValue::Int(2)), ("x", NbtValue::Int(1))]);
        assert_eq!(a, b);
    }

    #[test]
    fn list_equality_respects_order() {
        let a = NbtValue::List(vec![NbtValue::Int(1), NbtValue::Int(2)]);
        let b = NbtValue::List(vec![NbtValue::Int(2), NbtValue::Int(1)]);
        assert_ne!(a, b);
    }

    #[test]
    fn tag_name_matches_vanilla_terms() {
        assert_eq!(NbtValue::Byte(0).tag_name(), "byte");
        assert_eq!(
            NbtValue::Compound(Default::default()).tag_name(),
            "compound"
        );
        assert_eq!(NbtValue::IntArray(vec![]).tag_name(), "int_array");
    }
}
