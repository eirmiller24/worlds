//! Identity: `Slug` names definitions, `Id` names instances.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

/// A snake_case name for a definition (layer, tileset, entity type, rule, level, ...).
/// Matches `^[a-z][a-z0-9_]*$`. Also used as a file or directory name.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Slug(String);

/// Debug prints as a plain quoted string, so messages read `"x"`, not `Slug("x")`.
impl fmt::Debug for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl Slug {
    pub const PATTERN: &'static str = "^[a-z][a-z0-9_]*$";

    pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
        let s = s.into();
        if Self::is_valid(&s) {
            Ok(Slug(s))
        } else {
            Err(IdError::Slug(s))
        }
    }

    pub fn is_valid(s: &str) -> bool {
        let mut chars = s.chars();
        matches!(chars.next(), Some('a'..='z'))
            && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Shorthand for building a slug from a literal known to be valid.
/// Panics on an invalid slug; meant for tests and constants.
pub fn slug(s: &str) -> Slug {
    Slug::new(s).unwrap_or_else(|e| panic!("{e}"))
}

impl TryFrom<String> for Slug {
    type Error = IdError;
    fn try_from(s: String) -> Result<Self, IdError> {
        Slug::new(s)
    }
}

impl From<Slug> for String {
    fn from(s: Slug) -> String {
        s.0
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::borrow::Borrow<str> for Slug {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for Slug {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Slug {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl JsonSchema for Slug {
    fn schema_name() -> Cow<'static, str> {
        "Slug".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": Slug::PATTERN,
            "description": "snake_case name: a lowercase letter, then lowercase letters, digits or underscores."
        })
    }
}

/// A random instance id: 10 characters of lowercase base32 (`a-z`, `2-7`),
/// e.g. `k3v7q2mzta`. Stable for the life of the instance.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Id(String);

/// Debug prints as a plain quoted string, so messages read `"x"`, not `Id("x")`.
impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl Id {
    pub const LEN: usize = 10;
    pub const PATTERN: &'static str = "^[a-z2-7]{10}$";
    const ALPHABET: &'static [u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

    /// A fresh random id (50 bits of entropy).
    pub fn generate() -> Self {
        Self::from_bits(rand::random())
    }

    /// The id generated content gets from a seed string, the same on every
    /// machine and run: FNV-1a 64 over the UTF-8 bytes, encoded like
    /// [`Id::generate`] (low 50 bits, 5 at a time, low bits first), so
    /// generating the same content again gives the same ids.
    pub fn derived(seed: &str) -> Self {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in seed.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self::from_bits(h)
    }

    fn from_bits(mut bits: u64) -> Self {
        let mut s = String::with_capacity(Self::LEN);
        for _ in 0..Self::LEN {
            s.push(Self::ALPHABET[(bits & 31) as usize] as char);
            bits >>= 5;
        }
        Id(s)
    }

    pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
        let s = s.into();
        if Self::is_valid(&s) {
            Ok(Id(s))
        } else {
            Err(IdError::Id(s))
        }
    }

    pub fn is_valid(s: &str) -> bool {
        s.len() == Self::LEN && s.bytes().all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Id {
    type Error = IdError;
    fn try_from(s: String) -> Result<Self, IdError> {
        Id::new(s)
    }
}

impl From<Id> for String {
    fn from(id: Id) -> String {
        id.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl JsonSchema for Id {
    fn schema_name() -> Cow<'static, str> {
        "Id".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": Id::PATTERN,
            "description": "Instance id: 10 characters from a-z and 2-7."
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("invalid slug {0:?}: must match ^[a-z][a-z0-9_]*$")]
    Slug(String),
    #[error("invalid id {0:?}: must be 10 characters from a-z and 2-7")]
    Id(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert!(Slug::is_valid("floor_button"));
        assert!(Slug::is_valid("a1"));
        assert!(!Slug::is_valid(""));
        assert!(!Slug::is_valid("1a"));
        assert!(!Slug::is_valid("Floor"));
        assert!(!Slug::is_valid("floor-button"));
        assert!(serde_json::from_str::<Slug>("\"Bad\"").is_err());
    }

    #[test]
    fn ids() {
        for _ in 0..100 {
            let id = Id::generate();
            assert!(Id::is_valid(id.as_str()), "{id}");
        }
        assert!(Id::new("abcdefgh23").is_ok());
        assert!(Id::new("abcdefgh21").is_err());
        assert!(Id::new("abc").is_err());
    }

    #[test]
    fn derived_ids_are_stable() {
        // FNV-1a 64 of "" is the offset basis, 0xcbf29ce484222325.
        let mut bits: u64 = 0xcbf2_9ce4_8422_2325;
        let mut want = String::new();
        for _ in 0..10 {
            want.push(Id::ALPHABET[(bits & 31) as usize] as char);
            bits >>= 5;
        }
        assert_eq!(Id::derived("").as_str(), want);
        let a = Id::derived("main/guide/0/0");
        assert!(Id::is_valid(a.as_str()), "{a}");
        assert_eq!(a, Id::derived("main/guide/0/0"));
        assert_ne!(a, Id::derived("main/guide/0/1"));
    }
}
