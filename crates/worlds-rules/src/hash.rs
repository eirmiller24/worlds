//! Deterministic randomness for the bake.
//!
//! Every random choice is one call to [`hash`]. It is plain integer math on
//! fixed-width types (splitmix64's finalizer), so the output is the same on
//! every platform and every Rust version. Nothing here uses `DefaultHasher`,
//! `rand` or floats until the very last step of [`unit()`].
//!
//! Inputs: the layer seed (see [`layer_seed`]: project seed, level id, layer
//! slug, the level's optional per-layer `seed`), the rule key (see
//! [`rule_key`]: rule slug and optional `salt`), the anchor cell and a
//! *stream* id saying which decision is being made. Different decisions at
//! the same cell use different streams so they don't correlate. Because only
//! the anchor cell feeds the hash, a one-cell edit can only change decisions
//! at cells whose pattern window covers the edit.

use worlds_core::{Level, Project};

/// Stream ids. Variant-dependent streams add the variant index (0 = as
/// written, 1 = mirror_x, 2 = mirror_y, 3 = both, 4 = quarter turn
/// clockwise, 5 = counterclockwise, 6 = transpose, 7 = anti-transpose) to
/// their base, so each base owns 16 ids and 0-7 are taken.
pub mod stream {
    /// `chance` roll. One per cell, shared by all variants.
    pub const CHANCE: u64 = 0x01;
    /// Perlin permutation seed (hashed once per rule with x = y = 0; the
    /// rule key is mixed with `perlin.seed`).
    pub const PERLIN: u64 = 0x02;
    /// Weighted tile-alternative pick, + variant.
    pub const ALT: u64 = 0x10;
    /// `random_offset_px.x` draw, + variant.
    pub const OFFSET_X: u64 = 0x20;
    /// `random_offset_px.y` draw, + variant.
    pub const OFFSET_Y: u64 = 0x30;
    /// Region rule, `random` spacing: the first position's phase
    /// (`p0 = lo + h mod every`), at the run's first cell after margin.
    /// The base `0x40` owns 0x40-0x4f.
    pub const REGION_PHASE: u64 = 0x40;
    /// Region rule, `random` spacing: the jitter added to the step after
    /// position `p` (`h mod (jitter + 1)`), at `p`'s cell.
    pub const REGION_STEP: u64 = 0x41;
}

/// splitmix64's output function: a strong 64-bit bijective mixer.
#[inline]
pub fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

const GOLDEN: u64 = 0x9e37_79b9_7f4a_7c15;

/// The one random function of the engine.
#[inline]
pub fn hash(layer_seed: u64, rule: u64, x: i32, y: i32, stream: u64) -> u64 {
    let xy = (x as u32 as u64) | ((y as u32 as u64) << 32);
    let mut h = mix(layer_seed.wrapping_add(GOLDEN));
    h = mix(h ^ rule.wrapping_add(GOLDEN));
    h = mix(h ^ xy.wrapping_add(GOLDEN));
    mix(h ^ stream.wrapping_add(GOLDEN))
}

/// A hash mapped to `[0, 1)` with 53 bits of precision.
#[inline]
pub fn unit(h: u64) -> f64 {
    (h >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

/// FNV-1a over bytes, then mixed. Used to fold strings into the seed.
fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    mix(h)
}

/// A rule's part of the hash: its slug and its `salt` (default 0). Two rules
/// never share random choices, and renaming a rule re-rolls it.
pub fn rule_key(slug: &str, salt: i64) -> u64 {
    mix(hash_str(slug) ^ (salt as u64).wrapping_add(GOLDEN))
}

/// The seed every random choice of `layer` in `level` starts from:
/// `project.seed`, `level.id`, the layer slug and the level's per-layer seed
/// (`level.json` `layers.<layer>.seed`, default 0), mixed together. Changing
/// any of them re-rolls the whole layer; nothing else does.
pub fn layer_seed(project: &Project, level: &Level, layer: &str) -> u64 {
    let mut h = mix(project.seed as u64 ^ GOLDEN);
    h = mix(h ^ hash_str(level.id.as_str()));
    h = mix(h ^ hash_str(layer));
    mix(h ^ (level.layer_seed(layer) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        // Pinned so an accidental change to the mixer is caught: baked output
        // of every project depends on these bits.
        assert_eq!(mix(0), 0);
        assert_eq!(mix(1), 0x5692_161d_100b_05e5);
        assert_eq!(hash(1, 2, 3, 4, 5), hash(1, 2, 3, 4, 5));
        assert_ne!(hash(1, 2, 3, 4, 5), hash(1, 2, 4, 3, 5));
        assert_ne!(hash(1, 2, -1, 0, 5), hash(1, 2, 0, -1, 5));
        assert_ne!(rule_key("a", 0), rule_key("b", 0));
        assert_ne!(rule_key("a", 0), rule_key("a", 1));
        let u = unit(u64::MAX);
        assert!(u < 1.0 && u > 0.999_999);
    }
}
