//! Reference images: a sketch, a concept painting or a screenshot drawn
//! under the layer stack to build a level against. Stored in `level.json`
//! (level pixels) or `world.json` (world pixels) `references`; never
//! exported.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::Id;

/// One entry of `references` in `level.json` or `world.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub id: Id,
    /// Path to the PNG, relative to the project directory,
    /// `/`-separated, like a tileset's `image`.
    pub image: String,
    /// Top-left corner in pixels: level pixels in `level.json`, world
    /// pixels in `world.json`.
    pub px: [i64; 2],
    /// Drawn size over the image's own size in pixels. Default 1.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub scale: f64,
    /// 0 (invisible) to 1 (opaque). Default 1.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub opacity: f64,
    /// The editor's tools don't move or scale it. Default false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    /// Drawn at all. Default true.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub visible: bool,
}

impl Reference {
    /// A reference at `px` with every default: scale 1, opaque, unlocked,
    /// visible.
    pub fn new(id: Id, image: impl Into<String>, px: [i64; 2]) -> Self {
        Reference {
            id,
            image: image.into(),
            px,
            scale: 1.0,
            opacity: 1.0,
            locked: false,
            visible: true,
        }
    }
}

fn one() -> f64 {
    1.0
}

fn is_one(v: &f64) -> bool {
    *v == 1.0
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_left_out() {
        let r = Reference::new(Id::new("refsketch2").unwrap(), "art/sketch.png", [0, -32]);
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "id": "refsketch2", "image": "art/sketch.png", "px": [0, -32] })
        );
        let back: Reference = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
        let mut r2 = r.clone();
        r2.scale = 2.0;
        r2.opacity = 0.5;
        r2.locked = true;
        r2.visible = false;
        let v = serde_json::to_value(&r2).unwrap();
        assert_eq!(v["scale"], 2.0);
        assert_eq!(v["opacity"], 0.5);
        assert_eq!(v["locked"], true);
        assert_eq!(v["visible"], false);
    }
}
