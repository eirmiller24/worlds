//! Every LDtk sample and the mini fixture bake exactly as recorded.
//!
//! `fixture_bakes.txt` holds the recorded bakes; regenerate it with
//! `WORLDS_WRITE_FIXTURE_BAKES=<path>` only when a change to the engine's
//! output is intended. Each line is one (project, level, auto layer): the
//! placement count and FNV-1a digests of the placements (every field, in
//! draw order) and of `fired`. The bake goes through both
//! [`worlds_rules::bake_level`] and a fresh [`worlds_rules::BakeCache`].
//!
//! Projects: every LDtk sample (opacity from the sample PNGs) and
//! worlds-core's `mini.worlds` (no PNGs: every tile opaque). Imported
//! projects get seed 1234 and level ids derived from the level slugs (the
//! importer draws random ones, and every random roll is keyed on both).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use worlds_core::{Flip, Id, Project, ldtk};
use worlds_rules::{BakeCache, LayerBake, TileOpacity, TilesetImages};

const SAMPLES: &[&str] = &[
    "Test_file_for_API_showing_all_features.ldtk",
    "AutoLayers_1_basic.ldtk",
    "AutoLayers_2_stamps.ldtk",
    "AutoLayers_3_Mosaic.ldtk",
    "AutoLayers_4_Assistant.ldtk",
    "AutoLayers_5_Advanced.ldtk",
    "AutoLayers_6_OptionalRules.ldtk",
    "AutoLayers_7_Biomes.ldtk",
    "Entities.ldtk",
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn snapshot_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture_bakes.txt")
}

struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn put(&mut self, s: &str) {
        for b in s.bytes() {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// `n placements_digest fired_digest`.
fn digest(b: &LayerBake) -> String {
    let mut h = Fnv::new();
    let mut line = String::new();
    for p in &b.placements {
        line.clear();
        writeln!(
            line,
            "{:?} {} {:?} {} {:016x} {} {} {:?}",
            p.px,
            p.tileset,
            p.tile,
            Flip::bits_of(p.flip),
            p.alpha.to_bits(),
            p.rule,
            p.group,
            p.anchor
        )
        .unwrap();
        h.put(&line);
    }
    let mut f = Fnv::new();
    for (cell, rules) in &b.fired {
        line.clear();
        write!(line, "{cell:?}").unwrap();
        for r in rules {
            write!(line, " {r}").unwrap();
        }
        line.push('\n');
        f.put(&line);
    }
    format!("{} {:016x} {:016x}", b.placements.len(), h.0, f.0)
}

/// `project \t level \t layer` -> digest, for one project.
fn bake_all(name: &str, project: &Project, images: &TilesetImages) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut cache = BakeCache::new();
    for level in project.levels() {
        let bakes = worlds_rules::bake_level(project, level, images)
            .unwrap_or_else(|e| panic!("{name} {}: {e}", level.slug));
        cache
            .bake_level(project, level.slug.as_str(), images)
            .unwrap_or_else(|e| panic!("{name} {} (cache): {e}", level.slug));
        for (layer, bake) in &bakes {
            let d = digest(bake);
            let cached = cache
                .get(level.slug.as_str(), layer.as_str())
                .unwrap_or_else(|| panic!("{name} {} {layer}: not cached", level.slug));
            assert_eq!(
                digest(cached),
                d,
                "{name} {} {layer}: BakeCache differs from bake_level",
                level.slug
            );
            out.insert(format!("{name}\t{}\t{layer}", level.slug), d);
        }
    }
    out
}

/// The LDtk importer gives the project a random seed and levels fresh
/// random ids, both mixed into every layer seed: pin them so random rolls
/// are reproducible.
fn pin_ids(name: &str, project: &mut Project) {
    project.seed = 1234;
    for w in &mut project.worlds {
        for l in &mut w.levels {
            l.id = Id::derived(&format!("{name}/{}", l.slug));
        }
    }
}

/// Every available project's digests, and the project names covered.
fn current() -> (BTreeMap<String, String>, BTreeSet<String>) {
    let mut out = BTreeMap::new();
    let mut names = BTreeSet::new();
    let samples = fixtures().join("ldtk-samples");
    for name in SAMPLES {
        let mut imported = ldtk::import(&samples.join(name), &samples).unwrap();
        pin_ids(name, &mut imported.project);
        let images = TilesetImages::new(&samples, &imported.project.tilesets);
        out.extend(bake_all(name, &imported.project, &images));
        names.insert(name.to_string());
    }

    // worlds-core's `mini.worlds`: its `pillar` stamp `occupies` and its
    // rule has `avoid_overlap`.
    let name = "mini.worlds";
    let src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../worlds-core/tests/fixtures/mini.worlds");
    let project = worlds_core::load(&src).unwrap_or_else(|e| panic!("{name}: {e}"));
    // It has no PNGs: every tile of a 64x64 atlas counts as opaque.
    let images = TilesetImages::in_memory();
    for ts in &project.tilesets {
        images.insert(
            ts.slug.clone(),
            TileOpacity::from_flags(64, 64, vec![true; 64 * 64]),
        );
    }
    out.extend(bake_all(name, &project, &images));
    names.insert(name.to_string());

    (out, names)
}

#[test]
fn fixture_bakes_unchanged() {
    let (now, names) = current();
    if let Some(path) = std::env::var_os("WORLDS_WRITE_FIXTURE_BAKES") {
        let mut s = String::from("# project\tlevel\tlayer\tplacements placements_fnv fired_fnv\n");
        for (k, v) in &now {
            writeln!(s, "{k}\t{v}").unwrap();
        }
        std::fs::write(&path, s).unwrap();
        eprintln!("wrote {} lines to {path:?}", now.len());
        return;
    }
    let text = std::fs::read_to_string(snapshot_path()).unwrap();
    let mut want = BTreeMap::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let (key, digest) = line.rsplit_once('\t').expect("key \\t digest");
        let project = key.split('\t').next().unwrap();
        if names.contains(project) {
            want.insert(key.to_string(), digest.to_string());
        }
    }
    assert!(
        !want.is_empty(),
        "snapshot has no lines for the projects here"
    );
    let mut diffs = Vec::new();
    for (k, v) in &want {
        match now.get(k) {
            Some(n) if n == v => {}
            Some(n) => diffs.push(format!("{k}: was {v}, now {n}")),
            None => diffs.push(format!("{k}: no longer baked")),
        }
    }
    for k in now.keys().filter(|k| !want.contains_key(*k)) {
        diffs.push(format!("{k}: new layer bake"));
    }
    assert!(diffs.is_empty(), "bakes changed:\n{}", diffs.join("\n"));
}
