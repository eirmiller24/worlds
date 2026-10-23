//! One-way import of an LDtk project (`.ldtk`, plus `.ldtkl` external levels)
//! into a Worlds [`Project`].
//!
//! The importer carries the *meaning* of the project: definitions, level
//! content, rules and seeds. LDtk's uids, derived `__*` fields, caches
//! (`cachedPixelData`, `toc`, baked `autoLayerTiles`) and editor-only state are
//! dropped. Anything with meaning that the Worlds model can't hold becomes a
//! warning in [`ImportReport`]. See `docs/importing_ldtk.md`.
//!
//! ```no_run
//! use std::path::Path;
//! let out = Path::new("mygame.worlds");
//! let imported = worlds_core::ldtk::import(Path::new("world_map.ldtk"), out)?;
//! for w in &imported.report.warnings {
//!     eprintln!("warning: {w}");
//! }
//! worlds_core::ldtk::write(&imported, out)?;
//! # Ok::<(), worlds_core::ldtk::Error>(())
//! ```

mod defs;
mod json;
mod layers;
mod levels;
mod names;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::id::{Id, Slug};
use crate::model::{FieldDef, Flip, Project};

pub use names::snake_case;

/// The result of [`import`].
#[derive(Debug, Clone)]
pub struct Imported {
    pub project: Project,
    pub report: ImportReport,
    /// LDtk's own output, for the parity harness. Never saved.
    pub reference: LdtkReference,
}

/// What the import changed or couldn't carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// One line per thing with meaning that the Worlds model can't hold, or
    /// that was approximated.
    pub warnings: Vec<String>,
    /// Every LDtk identifier that became a different slug, as
    /// `(ldtk identifier, slug)`, in first-seen order, without repeats.
    pub renamed: Vec<(String, String)>,
}

/// LDtk's saved results, converted to Worlds names, for cross-checking a bake.
/// In memory only; not part of the project.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LdtkReference {
    /// Level slug -> auto layer slug -> LDtk's saved `autoLayerTiles`, in
    /// LDtk's file order.
    pub autotiles: BTreeMap<Slug, BTreeMap<Slug, Vec<RefTile>>>,
    /// Tileset slug -> tiles LDtk had cached as fully opaque
    /// (`cachedPixelData.opaqueTiles`). Tilesets without the cache are absent.
    pub opaque_tiles: BTreeMap<Slug, BTreeSet<[u32; 2]>>,
}

impl LdtkReference {
    /// Total number of reference autotiles across all levels and layers.
    pub fn autotile_count(&self) -> usize {
        self.autotiles
            .values()
            .flat_map(|layers| layers.values())
            .map(Vec::len)
            .sum()
    }
}

/// One tile LDtk placed by a rule.
#[derive(Debug, Clone, PartialEq)]
pub struct RefTile {
    /// Pixel position of the tile's top-left corner in the level.
    pub px: [i32; 2],
    pub tileset: Slug,
    pub tile: [u32; 2],
    pub flip: Option<Flip>,
    pub alpha: f64,
    /// The imported rule's slug (`r_<ldtk uid>`).
    pub rule: Slug,
    /// The cell the rule matched at (not necessarily where the tile is drawn).
    pub anchor_cell: [i32; 2],
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: not a readable LDtk file: {message}", path.display())]
    Json { path: PathBuf, message: String },
    /// The project uses something the importer refuses to approximate.
    #[error("{0}")]
    Unsupported(String),
    #[error(transparent)]
    Format(#[from] crate::format::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Read an LDtk project and convert it. `out_dir` is where the Worlds project
/// will be written; tileset image paths are made relative to it. Nothing is
/// written; call [`write`] for that.
pub fn import(ldtk_path: &Path, out_dir: &Path) -> Result<Imported> {
    let mut src: json::Project = read_json(ldtk_path)?;
    let ldtk_dir = ldtk_path.parent().unwrap_or(Path::new("")).to_path_buf();
    load_external_levels(&mut src, &ldtk_dir)?;
    let name = ldtk_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ldtk".into());
    let mut ctx = Ctx::new(&src, ldtk_dir, out_dir)?;
    let project = ctx.convert(&src, name)?;
    Ok(Imported {
        project,
        report: ctx.report,
        reference: ctx.reference,
    })
}

/// Save an imported project to `out_dir` (see [`crate::save`]) and write its
/// `schema/` directory. Images are referenced in place, never copied. A
/// fresh import knows no files on disk, so nothing already in `out_dir` is
/// deleted.
pub fn write(imported: &Imported, out_dir: &Path) -> Result<()> {
    crate::format::save(&mut imported.project.clone(), out_dir)?;
    crate::schema::write_all(out_dir)?;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let de = &mut serde_json::Deserializer::from_slice(&bytes);
    serde_path_to_error::deserialize(de).map_err(|e| Error::Json {
        path: path.to_path_buf(),
        message: if e.path().to_string() == "." {
            e.inner().to_string()
        } else {
            format!("at {}: {}", e.path(), e.inner())
        },
    })
}

/// Every level of the file, top-level and in worlds.
fn all_levels(src: &json::Project) -> impl Iterator<Item = &json::Level> {
    src.levels
        .iter()
        .chain(src.worlds.iter().flat_map(|w| w.levels.iter()))
}

fn load_external_levels(src: &mut json::Project, ldtk_dir: &Path) -> Result<()> {
    let levels = src
        .levels
        .iter_mut()
        .chain(src.worlds.iter_mut().flat_map(|w| w.levels.iter_mut()));
    for level in levels {
        if level.layer_instances.is_some() {
            continue;
        }
        let Some(rel) = &level.external_rel_path else {
            continue;
        };
        let full: json::Level = read_json(&ldtk_dir.join(rel))?;
        *level = full;
    }
    Ok(())
}

// ---- shared conversion state ----

/// A tileset as the rest of the import needs it.
#[derive(Debug, Clone)]
struct TilesetInfo {
    slug: Slug,
    /// Tiles per atlas row, computed the way LDtk does for tile ids.
    c_wid: u32,
    /// The imported definition, for pixel <-> tile arithmetic.
    def: crate::model::TilesetDef,
}

impl TilesetInfo {
    fn tile(&self, tid: u32) -> [u32; 2] {
        if self.c_wid == 0 {
            [tid, 0]
        } else {
            [tid % self.c_wid, tid / self.c_wid]
        }
    }
}

#[derive(Debug, Clone)]
struct EnumInfo {
    slug: Slug,
    /// The first value, LDtk's implicit default for a non-nullable field.
    first: Option<Slug>,
    /// LDtk value id -> value slug.
    values: HashMap<String, Slug>,
}

#[derive(Debug, Clone)]
struct FieldInfo {
    def: FieldDef,
    /// For enum fields, the LDtk enum uid.
    enum_uid: Option<i64>,
    /// LDtk identifier, for messages.
    ldtk_name: String,
}

#[derive(Debug, Clone)]
struct EntityInfo {
    slug: Slug,
    ldtk_name: String,
    px_size: [u32; 2],
    size: [u32; 2],
    pivot: [f64; 2],
    fields: HashMap<i64, FieldInfo>,
}

/// One IntGrid value as rule patterns and grid files need it.
#[derive(Debug, Clone)]
struct ValueInfo {
    slug: Slug,
    /// 1-based position in the layer definition's values (the cell index).
    index: u16,
    glyph: char,
    /// Pattern glyph for `!slug`.
    not_glyph: char,
    group_uid: i64,
}

#[derive(Debug, Clone)]
struct GroupInfo {
    slug: Slug,
    glyph: char,
    not_glyph: char,
}

/// The imported side of an LDtk auto-tiling layer (a pure AutoLayer, or the
/// rules half of an IntGrid layer).
#[derive(Debug, Clone)]
struct AutoInfo {
    slug: Slug,
    /// LDtk rule group uid -> group slug.
    groups: HashMap<i64, Slug>,
    /// LDtk rule uid -> rule slug.
    rules: HashMap<i64, Slug>,
    /// Every tileset the rules draw from (for level tileset overrides).
    tilesets: BTreeSet<Slug>,
}

#[derive(Debug, Clone)]
struct LayerInfo {
    ldtk_name: String,
    ldtk_type: String,
    slug: Slug,
    /// Cell size in pixels.
    grid: u32,
    tileset_uid: Option<i64>,
    /// IntGrid layers: values by LDtk value.
    values: BTreeMap<i64, ValueInfo>,
    /// IntGrid layers: groups by LDtk group uid.
    groups: BTreeMap<i64, GroupInfo>,
    auto: Option<AutoInfo>,
}

struct Ctx {
    grid: u32,
    ldtk_dir: PathBuf,
    out_dir: PathBuf,
    report: ImportReport,
    reference: LdtkReference,
    renamed_seen: BTreeSet<(String, String)>,
    warned: BTreeSet<String>,
    tilesets: HashMap<i64, TilesetInfo>,
    /// Tilesets skipped because they're LDtk's built-in atlas (warned once).
    skipped_tilesets: BTreeSet<i64>,
    enums: HashMap<i64, EnumInfo>,
    entities: HashMap<i64, EntityInfo>,
    level_fields: HashMap<i64, FieldInfo>,
    layers: HashMap<i64, LayerInfo>,
    /// LDtk entity iid -> new instance id.
    entity_ids: HashMap<String, Id>,
    /// Rule group uids some level lists in `optionalRules`.
    enabled_groups: BTreeSet<i64>,
}

impl Ctx {
    fn new(src: &json::Project, ldtk_dir: PathBuf, out_dir: &Path) -> Result<Self> {
        let mut ctx = Ctx {
            grid: 0,
            ldtk_dir,
            out_dir: out_dir.to_path_buf(),
            report: ImportReport::default(),
            reference: LdtkReference::default(),
            renamed_seen: BTreeSet::new(),
            warned: BTreeSet::new(),
            tilesets: HashMap::new(),
            skipped_tilesets: BTreeSet::new(),
            enums: HashMap::new(),
            entities: HashMap::new(),
            level_fields: HashMap::new(),
            layers: HashMap::new(),
            entity_ids: HashMap::new(),
            enabled_groups: all_levels(src)
                .flat_map(|l| l.layer_instances.iter().flatten())
                .flat_map(|li| li.optional_rules.iter().copied())
                .collect(),
        };
        ctx.grid = ctx.grid_size(src)?;
        Ok(ctx)
    }

    fn convert(&mut self, src: &json::Project, name: String) -> Result<Project> {
        if !src.json_version.starts_with("1.5") && !src.json_version.is_empty() {
            self.warn(format!(
                "file is LDtk {}; the importer is written against 1.5",
                src.json_version
            ));
        }
        let mut project = Project::new(name);
        project.grid_size = self.grid;
        project.bg_color = src
            .default_level_bg_color
            .as_deref()
            .and_then(|c| self.color(c, "project defaultLevelBgColor"));
        if src.flags.iter().any(|f| f == "MultiWorlds") && src.worlds.is_empty() {
            self.warn("MultiWorlds flag is set but the file has no worlds".to_string());
        }
        project.tilesets = self.tilesets(&src.defs.tilesets)?;
        project.enums = self.enums(&src.defs)?;
        self.claim_entity_slugs(&src.defs.entities);
        project.level_fields = self.level_fields(&src.defs.level_fields);
        project.entities = self.entity_defs(&src.defs.entities);
        let (layers, rules) = self.layers(&src.defs)?;
        project.layers = layers;
        project.rules = rules;
        project.collision_layer = self.collision_layer(&src.defs.layers);
        project.blocking_values = project.default_blocking_values();
        project.worlds = self.worlds(src)?;
        project.normalize();
        Ok(project)
    }

    /// The project grid. Layers may use their own grid when it divides or is
    /// a multiple of the project grid, and every level must be a whole
    /// number of project cells. Candidates are the layer grids, most common
    /// first (ties: `defaultGridSize`, then larger); the first that fits
    /// wins. With one grid everywhere that is simply it.
    fn grid_size(&mut self, src: &json::Project) -> Result<u32> {
        let layers = &src.defs.layers;
        if layers.is_empty() {
            return Ok(src.default_grid_size.max(1));
        }
        let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
        for l in layers {
            *counts.entry(l.grid_size.max(1)).or_default() += 1;
        }
        let mut candidates: Vec<u32> = counts.keys().copied().collect();
        candidates.sort_by_key(|g| std::cmp::Reverse((counts[g], *g == src.default_grid_size, *g)));
        let fits = |g: u32, l: u32| g.is_multiple_of(l) || l.is_multiple_of(g);
        let mut why = String::new();
        for &grid in &candidates {
            if let Some(&other) = counts.keys().find(|&&l| !fits(grid, l)) {
                why = format!(
                    "layer grids {grid} px and {other} px: neither divides the other, so they \
                     can't share a project grid"
                );
                continue;
            }
            if let Some(l) = all_levels(src)
                .find(|l| !l.px_wid.is_multiple_of(grid) || !l.px_hei.is_multiple_of(grid))
            {
                why = format!(
                    "level {:?}: {}x{} px is not a whole number of {grid} px cells",
                    l.identifier, l.px_wid, l.px_hei
                );
                continue;
            }
            if counts.len() > 1 {
                let others: Vec<String> = layers
                    .iter()
                    .filter(|l| l.grid_size != grid)
                    .map(|l| format!("{:?} ({} px)", l.identifier, l.grid_size))
                    .collect();
                self.warn(format!(
                    "project grid is {grid} px; layers with their own grid: {}",
                    others.join(", ")
                ));
            } else if grid != src.default_grid_size {
                self.warn(format!(
                    "project grid is {grid} px (every layer uses it), not defaultGridSize {} px",
                    src.default_grid_size
                ));
            }
            return Ok(grid);
        }
        Err(Error::Unsupported(format!("no usable project grid: {why}")))
    }

    /// The IntGrid layer named `collision` (any case), whose values are taken
    /// to be the collision kinds. LDtk has no such notion, so nothing else
    /// is guessed.
    fn collision_layer(&self, src: &[json::LayerDef]) -> Option<Slug> {
        src.iter()
            .find(|l| l.ty == "IntGrid" && l.identifier.eq_ignore_ascii_case("collision"))
            .map(|l| self.layers[&l.uid].slug.clone())
    }

    fn warn(&mut self, msg: String) {
        if self.warned.insert(msg.clone()) {
            self.report.warnings.push(msg);
        }
    }

    /// Record a rename if `slug` differs from `orig`.
    fn renamed(&mut self, orig: &str, slug: &Slug) {
        if orig != slug.as_str() {
            let pair = (orig.to_string(), slug.to_string());
            if self.renamed_seen.insert(pair.clone()) {
                self.report.renamed.push(pair);
            }
        }
    }

    fn color(&mut self, c: &str, what: &str) -> Option<crate::model::Color> {
        match crate::model::Color::new(c) {
            Ok(c) => Some(c),
            Err(e) => {
                self.warn(format!("{what}: {e}; dropped"));
                None
            }
        }
    }

    /// Path to an LDtk-relative file, relative to the output project directory.
    fn image_path(&self, rel: &str) -> String {
        let target = normalize(&absolute(&self.ldtk_dir.join(rel)));
        let base = normalize(&absolute(&self.out_dir));
        relative(&base, &target)
    }
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Resolve `.` and `..` lexically.
fn normalize(p: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir if matches!(out.last(), Some(Component::Normal(_))) => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out.iter().collect()
}

/// `target` relative to the directory `base`, `/`-separated. Both absolute.
fn relative(base: &Path, target: &Path) -> String {
    let b: Vec<Component> = base.components().collect();
    let t: Vec<Component> = target.components().collect();
    if b.first() != t.first() {
        // Different roots (other drive): nothing relative exists.
        return target.to_string_lossy().replace('\\', "/");
    }
    let common = b.iter().zip(&t).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<String> = vec!["..".into(); b.len() - common];
    parts.extend(
        t[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        let r = |b: &str, t: &str| relative(Path::new(b), Path::new(t));
        assert_eq!(
            r("/game/mygame.worlds", "/game/assets/tilesets/a.png"),
            "../assets/tilesets/a.png"
        );
        assert_eq!(r("/game", "/game/a.png"), "a.png");
        assert_eq!(
            normalize(Path::new("/game/x/../assets/./a.png")),
            Path::new("/game/assets/a.png")
        );
    }
}
