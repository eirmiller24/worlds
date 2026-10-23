//! Small value types shared across the model.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::id::Slug;

/// A hex color, `#rrggbb` or `#rrggbbaa`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Color(String);

/// Debug prints as a plain quoted string, so messages read `"x"`, not `Color("x")`.
impl fmt::Debug for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl Color {
    pub const PATTERN: &'static str = "^#([0-9a-fA-F]{6}|[0-9a-fA-F]{8})$";

    pub fn new(s: impl Into<String>) -> Result<Self, String> {
        let s = s.into();
        if Self::is_valid(&s) {
            Ok(Color(s))
        } else {
            Err(format!(
                "invalid color {s:?}: expected #rrggbb or #rrggbbaa"
            ))
        }
    }

    pub fn is_valid(s: &str) -> bool {
        let Some(hex) = s.strip_prefix('#') else {
            return false;
        };
        matches!(hex.len(), 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Color {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        Color::new(s)
    }
}

impl From<Color> for String {
    fn from(c: Color) -> String {
        c.0
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl JsonSchema for Color {
    fn schema_name() -> Cow<'static, str> {
        "Color".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": Color::PATTERN,
            "description": "Hex color, #rrggbb or #rrggbbaa."
        })
    }
}

/// One tile of a tileset, addressed by atlas column and row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TileRef {
    /// Tileset slug.
    pub tileset: Slug,
    /// `[column, row]` in the tileset atlas.
    pub tile: [u32; 2],
}

/// How a tile is turned. `t` (transpose: swap the tile's x and y, a mirror
/// about its top-left to bottom-right diagonal) applies first, then `x`
/// mirrors left-right and `y` top-bottom, all inside the tile's own square.
/// So 90 degrees clockwise is `xt`, 180 is `xy`, 270 clockwise is `yt`.
/// Left out: not turned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Flip {
    #[serde(rename = "x")]
    X,
    #[serde(rename = "y")]
    Y,
    #[serde(rename = "xy")]
    XY,
    #[serde(rename = "t")]
    T,
    #[serde(rename = "xt")]
    XT,
    #[serde(rename = "yt")]
    YT,
    #[serde(rename = "xyt")]
    XYT,
}

impl Flip {
    /// Export flip bits: 1 = x, 2 = y, 4 = transpose (applied first).
    pub fn bits(self) -> u8 {
        match self {
            Flip::X => 1,
            Flip::Y => 2,
            Flip::XY => 3,
            Flip::T => 4,
            Flip::XT => 5,
            Flip::YT => 6,
            Flip::XYT => 7,
        }
    }

    /// The flip with these bits; `None` for 0 (no flip) and for anything
    /// above 7.
    pub fn from_bits(bits: u8) -> Option<Flip> {
        Some(match bits {
            1 => Flip::X,
            2 => Flip::Y,
            3 => Flip::XY,
            4 => Flip::T,
            5 => Flip::XT,
            6 => Flip::YT,
            7 => Flip::XYT,
            _ => return None,
        })
    }

    /// Bits of an optional flip (0 for none).
    pub fn bits_of(f: Option<Flip>) -> u8 {
        f.map_or(0, Flip::bits)
    }

    /// `f`, then mirrored left-right.
    pub fn mirror_x(f: Option<Flip>) -> Option<Flip> {
        Flip::from_bits(Flip::bits_of(f) ^ 1)
    }

    /// `f`, then mirrored top-bottom.
    pub fn mirror_y(f: Option<Flip>) -> Option<Flip> {
        Flip::from_bits(Flip::bits_of(f) ^ 2)
    }

    /// `f`, then transposed (swap x and y). A transpose after a mirror is
    /// the other mirror after a transpose, so the x and y bits swap.
    pub fn transpose(f: Option<Flip>) -> Option<Flip> {
        let b = Flip::bits_of(f);
        Flip::from_bits(((b & 1) << 1) | ((b & 2) >> 1) | ((b & 4) ^ 4))
    }

    /// The flip's name as written in files and the text view (`"xt"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Flip::X => "x",
            Flip::Y => "y",
            Flip::XY => "xy",
            Flip::T => "t",
            Flip::XT => "xt",
            Flip::YT => "yt",
            Flip::XYT => "xyt",
        }
    }
}

/// One variant of a kind-tagged type, for [`variant_schema`].
pub(crate) struct VariantKeys {
    /// The tag value (`"int_grid"`), or a label for an untagged type.
    pub name: &'static str,
    /// Keys only this variant (and maybe others) may use.
    pub keys: &'static [&'static str],
    /// Of `keys`, the ones this variant requires.
    pub required: &'static [&'static str],
}

/// The JSON Schema of a kind-tagged type, built from the schema of its flat
/// on-disk form: one `oneOf` branch per variant, each allowing the common
/// keys plus its own, with the tag (if any) pinned to the variant's name.
pub(crate) fn variant_schema(
    flat: Schema,
    tag: Option<&str>,
    variants: &[VariantKeys],
    description: &str,
) -> Schema {
    let flat = flat.to_value();
    let props = flat["properties"].as_object().cloned().unwrap_or_default();
    let required: Vec<String> = flat["required"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let specific: std::collections::BTreeSet<&str> = variants
        .iter()
        .flat_map(|v| v.keys.iter().copied())
        .collect();
    let branches: Vec<serde_json::Value> = variants
        .iter()
        .map(|v| {
            let mut p = serde_json::Map::new();
            for (k, s) in &props {
                if !specific.contains(k.as_str()) || v.keys.contains(&k.as_str()) {
                    p.insert(k.clone(), s.clone());
                }
            }
            let mut req: Vec<String> = required
                .iter()
                .filter(|r| !specific.contains(r.as_str()))
                .cloned()
                .collect();
            req.extend(v.required.iter().map(|r| r.to_string()));
            if let Some(tag) = tag {
                p.insert(tag.to_string(), serde_json::json!({ "const": v.name }));
                if !req.iter().any(|r| r == tag) {
                    req.insert(0, tag.to_string());
                }
            }
            serde_json::json!({
                "type": "object",
                "title": v.name,
                "properties": p,
                "required": req,
                "additionalProperties": false,
            })
        })
        .collect();
    json_schema!({ "description": description, "oneOf": branches })
}

/// `deserialize_with` for an `Option` field whose *presence* matters: a key
/// that is there is `Some`, even at its default value (`"values": []`,
/// `"derived": false`), so kind checks can reject it on the wrong kind. An
/// explicit `null` is an error, as for the plain field. Pair it with
/// `#[serde(default)]` and `#[schemars(with = "T")]`.
pub(crate) fn present<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

pub(crate) fn is_default<T: Default + PartialEq>(t: &T) -> bool {
    *t == T::default()
}

pub(crate) fn is_true(b: &bool) -> bool {
    *b
}

pub(crate) fn is_one(x: &f64) -> bool {
    *x == 1.0
}

pub(crate) fn yes() -> bool {
    true
}

pub(crate) fn one() -> f64 {
    1.0
}
