//! LDtk identifiers to slugs, and glyph choice for IntGrid values.

use std::collections::BTreeSet;

use crate::id::Slug;
use crate::model::glyph_problem;

/// `Main_Tiling` -> `main_tiling`, `FloorButton` -> `floor_button`,
/// `HTTPServer` -> `http_server`, `Heavy sword` -> `heavy_sword`.
/// A result that doesn't start with a letter gets `fallback` in front
/// (`8px` -> `n_8px` with fallback `n`); an empty one becomes `fallback`.
pub fn snake_case(s: &str, fallback: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() && i > 0 {
                let prev = chars[i - 1];
                let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
                // fooBar, foo2Bar -> foo_bar; HTTPServer -> http_server.
                if prev.is_ascii_lowercase()
                    || prev.is_ascii_digit()
                    || (prev.is_ascii_uppercase() && next_lower)
                {
                    out.push('_');
                }
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    // Collapse and trim underscores.
    let mut collapsed = String::with_capacity(out.len());
    for c in out.chars() {
        if c == '_' && (collapsed.is_empty() || collapsed.ends_with('_')) {
            continue;
        }
        collapsed.push(c);
    }
    while collapsed.ends_with('_') {
        collapsed.pop();
    }
    if collapsed.is_empty() {
        return fallback.to_string();
    }
    if !collapsed.starts_with(|c: char| c.is_ascii_lowercase()) {
        collapsed = format!("{fallback}_{collapsed}");
    }
    collapsed
}

/// Hands out unique slugs within one namespace, suffixing `_2`, `_3` ... on
/// collisions (LDtk's `fixUnique` style).
#[derive(Debug, Default)]
pub struct Namer {
    used: BTreeSet<String>,
}

impl Namer {
    /// Claim a unique slug based on `base` (already snake_case).
    pub fn claim(&mut self, base: &str) -> Slug {
        let mut candidate = base.to_string();
        let mut n = 2;
        while self.used.contains(&candidate) {
            candidate = format!("{base}_{n}");
            n += 1;
        }
        self.used.insert(candidate.clone());
        Slug::new(candidate).expect("snake_case output is a valid slug")
    }
}

/// Symbols tried after letters, in order. None of them is reserved
/// (`model::RESERVED_GLYPHS`).
const SYMBOL_POOL: &str = "#=~^%&$?/<>:;-_'`,(){}0123456789";

/// Glyphs for common value names, so imported grids read well.
const PREFERRED: &[(&str, char)] = &[
    ("solid", '#'),
    ("oneway", '-'),
    ("one_way", '-'),
    ("water", '~'),
    ("hazard", 'x'),
    ("breakable", 'b'),
    ("fortified", 'f'),
    ("ramp_r", '/'),
    ("ramp_l", 'L'),
];

/// Picks one glyph per IntGrid value: a preferred glyph for a common name
/// (`solid` is `#`, `water` is `~` ...), else the first letter of the name,
/// else the first letter of a later word, else the uppercase first letter,
/// else the next free symbol from a fixed pool. Never a reserved glyph.
#[derive(Debug, Default)]
pub struct GlyphPicker {
    used: BTreeSet<char>,
}

impl GlyphPicker {
    pub fn is_free(&self, c: char) -> bool {
        glyph_problem(c).is_none() && !self.used.contains(&c)
    }

    pub fn take(&mut self, c: char) -> char {
        self.used.insert(c);
        c
    }

    /// A glyph for a value whose slug is `name`.
    pub fn for_name(&mut self, name: &str) -> char {
        if let Some(&(_, c)) = PREFERRED.iter().find(|(n, _)| *n == name)
            && self.is_free(c)
        {
            return self.take(c);
        }
        let words: Vec<&str> = name.split('_').filter(|w| !w.is_empty()).collect();
        let firsts = words.iter().filter_map(|w| w.chars().next());
        let upper = words
            .first()
            .and_then(|w| w.chars().next())
            .map(|c| c.to_ascii_uppercase());
        for c in firsts.chain(upper) {
            if c.is_ascii_alphabetic() && self.is_free(c) {
                return self.take(c);
            }
        }
        self.next_free()
    }

    /// A glyph for "not `name`" when `name` has the glyph `base`: the other
    /// case of `base` if it's a letter, else the uppercase first letter of
    /// a word of `name` (`!solid` for `#` is `S`), else the next free one.
    pub fn related(&mut self, base: char, name: &str) -> char {
        let other = if base.is_ascii_lowercase() {
            base.to_ascii_uppercase()
        } else {
            base.to_ascii_lowercase()
        };
        let upper = name
            .split('_')
            .filter_map(|w| w.chars().next())
            .map(|c| c.to_ascii_uppercase());
        for c in std::iter::once(other).filter(|&c| c != base).chain(upper) {
            if c.is_ascii_alphabetic() && self.is_free(c) {
                return self.take(c);
            }
        }
        self.next_free()
    }

    pub fn next_free(&mut self) -> char {
        let letters = ('a'..='z').chain('A'..='Z');
        let c = SYMBOL_POOL
            .chars()
            .chain(letters)
            .chain((33u8..127).map(char::from))
            .find(|&c| self.is_free(c))
            .unwrap_or('?');
        self.take(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake() {
        assert_eq!(snake_case("Main_Tiling", "x"), "main_tiling");
        assert_eq!(snake_case("FloorButton", "x"), "floor_button");
        assert_eq!(snake_case("HTTPServer", "x"), "http_server");
        assert_eq!(snake_case("Heavy sword", "x"), "heavy_sword");
        assert_eq!(snake_case("IntGrid_8px_grid", "x"), "int_grid_8px_grid");
        assert_eq!(snake_case("Level_1", "x"), "level_1");
        assert_eq!(snake_case("ramp_r", "x"), "ramp_r");
        assert_eq!(snake_case("isAwaken", "x"), "is_awaken");
        assert_eq!(snake_case("8px", "n"), "n_8px");
        assert_eq!(snake_case("__", "v"), "v");
        assert_eq!(snake_case("SSW_CastlePack", "x"), "ssw_castle_pack");
    }

    #[test]
    fn namer() {
        let mut n = Namer::default();
        assert_eq!(n.claim("a"), "a");
        assert_eq!(n.claim("a"), "a_2");
        assert_eq!(n.claim("a"), "a_3");
    }

    #[test]
    fn glyphs() {
        let mut g = GlyphPicker::default();
        let names = [
            "solid", "oneway", "ramp_r", "ramp_l", "ramp_x", "rope", "water", "hazard",
        ];
        let got: Vec<char> = names.iter().map(|n| g.for_name(n)).collect();
        assert_eq!(got, vec!['#', '-', '/', 'L', 'r', 'R', '~', 'x']);
        assert_eq!(g.related('#', "solid"), 'S');
        assert_eq!(g.related('r', "ramp_x"), 'X', "'R' is taken");
        assert_eq!(g.related('~', "wet_ramp"), 'W');
        assert_eq!(g.related('x', "hazard"), 'H', "'X' is taken");
        assert_eq!(g.for_name("solid"), 's', "preferred glyph taken");
        // Nothing reserved is ever handed out, however many are needed.
        let all: Vec<char> = (0..90).map(|_| g.next_free()).collect();
        assert!(all.iter().all(|&c| glyph_problem(c).is_none() || c == '?'));
    }
}
