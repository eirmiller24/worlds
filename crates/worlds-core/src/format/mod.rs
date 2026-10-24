//! The on-disk project directory: load, save, and the file kinds.
//!
//! ```text
//! project.json  layers.json  tilesets.json  enums.json
//! entities/<entity>.json
//! rules/<auto layer>.json  rules/macros.json
//! worlds/<world>/world.json
//! assemblies/<assembly>.json
//! worlds/<world>/levels/<level>/level.json
//!                              /<int_grid or tiles layer>.json
//!                              /entities.json  /notes.json
//! ```
//!
//! Every file starts with `"$schema"` (relative path to `schema/<kind>.json`)
//! and `"format": 1`.

pub(crate) mod json;
mod load;
mod save;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use load::{Overlay, load, load_with_overlay, parse};
pub use save::{DirtyRender, render_dirty, render_file, render_files, save, save_all, save_dirty};
pub(crate) use save::{sync_dir, write_atomic};

use crate::id::Slug;
use crate::model::{
    Annotation, DerivedOverrides, EntitiesLayer, EnumDef, IntGridLayer, LayerDef, TilesetDef,
};

/// The only format version so far.
pub const FORMAT_VERSION: u32 = 1;

/// Slugs an `int_grid` or `tiles` layer can't use because they collide with
/// fixed file names.
pub const RESERVED_LAYER_SLUGS: [&str; 4] = ["level", "entities", "notes", "macros"];

/// The slug an `auto` layer can't use: `rules/macros.json` is taken.
pub const RESERVED_RULES_SLUG: &str = "macros";

/// One kind of project file. Each has a schema at `schema/<name>.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Project,
    Layers,
    Tilesets,
    Enums,
    Entity,
    Rules,
    Macros,
    World,
    Level,
    IntGrid,
    Tiles,
    Entities,
    Notes,
    Assembly,
}

impl FileKind {
    pub const ALL: [FileKind; 14] = [
        FileKind::Project,
        FileKind::Layers,
        FileKind::Tilesets,
        FileKind::Enums,
        FileKind::Entity,
        FileKind::Rules,
        FileKind::Macros,
        FileKind::World,
        FileKind::Level,
        FileKind::IntGrid,
        FileKind::Tiles,
        FileKind::Entities,
        FileKind::Notes,
        FileKind::Assembly,
    ];

    /// Schema file stem.
    pub fn name(self) -> &'static str {
        match self {
            FileKind::Project => "project",
            FileKind::Layers => "layers",
            FileKind::Tilesets => "tilesets",
            FileKind::Enums => "enums",
            FileKind::Entity => "entity",
            FileKind::Rules => "rules",
            FileKind::Macros => "macros",
            FileKind::World => "world",
            FileKind::Level => "level",
            FileKind::IntGrid => "intgrid",
            FileKind::Tiles => "tiles",
            FileKind::Entities => "entities",
            FileKind::Notes => "notes",
            FileKind::Assembly => "assembly",
        }
    }

    /// The `$schema` value for a file of this kind at `rel` (relative to the project dir).
    pub fn schema_ref(self, rel: &Path) -> String {
        let depth = rel.components().count().saturating_sub(1);
        format!("{}schema/{}.json", "../".repeat(depth), self.name())
    }
}

/// Body of `layers.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayersFile {
    /// Layer definitions in draw order, bottom first.
    pub layers: Vec<LayerDef>,
}

/// Body of `tilesets.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TilesetsFile {
    pub tilesets: Vec<TilesetDef>,
}

/// Body of `enums.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnumsFile {
    pub enums: Vec<EnumDef>,
}

/// Body of a level's `entities.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntitiesFile {
    /// One entry per entities layer that has instances in this level.
    pub layers: Vec<EntitiesLayer>,
}

/// Body of a level's `<int_grid layer>.json`, as it is on disk. In memory
/// the grid is an [`IntGridLayer`] (dense value indices), or for a derived
/// layer its [`DerivedOverrides`]; this is only the text form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IntGridFile {
    /// Layer slug. Same as the file name.
    pub layer: Slug,
    /// `true`: this is a derived layer's file, holding only hand-painted
    /// overrides (`.` no override, a value glyph, `!` suppress). Required on
    /// a derived layer's file, a load error on any other; never `false`.
    #[serde(
        default,
        deserialize_with = "overrides_flag",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "bool", extend("const" = true))]
    pub overrides: Option<bool>,
    /// Glyph to value slug, `null` for empty (`.`). Optional when reading:
    /// glyphs not listed mean what the layer definition says. Entries that
    /// are given must agree with the definition. Always written on save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legend: Option<BTreeMap<char, Option<Slug>>>,
    /// One string per row, top first, one glyph per cell.
    pub rows: Vec<String>,
}

/// `overrides` is `true` or absent.
fn overrides_flag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    match bool::deserialize(d)? {
        true => Ok(Some(true)),
        false => Err(serde::de::Error::custom(
            "\"overrides\" is either true (a derived layer's file) or left out",
        )),
    }
}

impl IntGridFile {
    /// The text form of `grid`, with `def`'s full legend.
    pub fn from_layer(grid: &IntGridLayer, def: &LayerDef) -> Result<Self, String> {
        Ok(IntGridFile {
            layer: grid.layer.clone(),
            overrides: None,
            legend: Some(def.legend()),
            rows: grid.to_rows(def)?,
        })
    }

    /// The text form of a derived layer's overrides, with `def`'s value
    /// glyphs as the legend (`.`, no override, and `!`, suppressed, are
    /// built in and never listed) and `"overrides": true`.
    pub fn from_overrides(o: &DerivedOverrides, def: &LayerDef) -> Result<Self, String> {
        let mut legend = def.legend();
        legend.remove(&IntGridLayer::EMPTY);
        Ok(IntGridFile {
            layer: o.layer.clone(),
            overrides: Some(true),
            legend: Some(legend),
            rows: o.to_rows(def)?,
        })
    }

    /// Parse the rows with `def`'s glyphs, after checking the legend agrees
    /// with `def`. `def` must not be derived (see [`IntGridFile::to_overrides`]).
    /// Errors are `(json path, message)`.
    pub fn to_layer(&self, def: &LayerDef) -> Result<IntGridLayer, (String, String)> {
        if self.overrides.is_some() {
            return Err((
                "overrides".into(),
                format!(
                    "\"overrides\" is only for a derived layer's file; layer {:?} isn't derived",
                    def.slug.as_str()
                ),
            ));
        }
        self.check_legend(def, false)?;
        IntGridLayer::from_rows(def, &self.rows).map_err(|(y, m)| (format!("rows[{y}]"), m))
    }

    /// A derived layer's overrides: the legend checked against `def`, then
    /// the rows read with `.` (no override), `!` (suppress) and `def`'s
    /// value glyphs. `required` asks for `"overrides": true` (a file on
    /// disk; command content may leave it out). Errors are `(json path,
    /// message)`.
    pub fn to_overrides(
        &self,
        def: &LayerDef,
        required: bool,
    ) -> Result<DerivedOverrides, (String, String)> {
        if required && self.overrides.is_none() {
            return Err((
                String::new(),
                format!(
                    "layer {:?} is derived: its file holds overrides and must say \
                     \"overrides\": true (after \"layer\")",
                    def.slug.as_str()
                ),
            ));
        }
        self.check_legend(def, true)?;
        DerivedOverrides::from_rows(def, &self.rows).map_err(|(y, m)| (format!("rows[{y}]"), m))
    }

    /// `derived`: a derived layer's file, where `.` (no override) is built
    /// in like `!` and isn't a legend entry.
    fn check_legend(&self, def: &LayerDef, derived: bool) -> Result<(), (String, String)> {
        let layer = def.slug.as_str();
        for (&glyph, value) in self.legend.iter().flatten() {
            let at = format!("legend.{glyph}");
            let fail = |m: String| Err((at.clone(), m));
            match (glyph, value) {
                (DerivedOverrides::SUPPRESS, _) => {
                    return fail(
                        "'!' is built in (a derived layer's suppress glyph); it isn't a legend \
                         entry"
                            .into(),
                    );
                }
                (IntGridLayer::EMPTY, _) if derived => {
                    return fail(
                        "'.' is built in (no override, in a derived layer's file); it isn't a \
                         legend entry"
                            .into(),
                    );
                }
                (IntGridLayer::EMPTY, None) => {}
                (IntGridLayer::EMPTY, Some(v)) => {
                    return fail(format!("'.' is the empty glyph; it can't stand for {v:?}"));
                }
                (_, Some(slug)) => match def.value(slug.as_str()) {
                    None => return fail(format!("{slug:?} is not a value of layer {layer:?}")),
                    Some(v) if v.glyph != glyph => {
                        return fail(format!(
                            "layer {layer:?} uses glyph {:?} for {slug:?}",
                            v.glyph
                        ));
                    }
                    Some(_) => {}
                },
                (_, None) => {
                    return fail(match def.value_by_glyph(glyph) {
                        Some(v) => format!(
                            "glyph {glyph:?} means {:?} in the layer definition, not empty",
                            v.slug
                        ),
                        None => "only '.' stands for empty".into(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Body of a level's `notes.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotesFile {
    pub notes: Vec<Annotation>,
}

/// A load or save failure. Always names the file.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file isn't valid JSON or doesn't match its schema. `json_path` is
    /// like `layers[2].values[0].glyph`, empty for the top level.
    #[error("{}: {}{message}", path.display(), if json_path.is_empty() { String::new() } else { format!("at {json_path}: ") })]
    Parse {
        path: PathBuf,
        json_path: String,
        message: String,
    },
    /// The file parsed but doesn't fit the directory layout (wrong slug for
    /// its file name, unknown layer file, duplicate names on save).
    #[error("{}: {message}", path.display())]
    Layout { path: PathBuf, message: String },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}
