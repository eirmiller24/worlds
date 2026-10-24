//! Assemblies (prefabs): a named, reusable [`Fragment`] committed with the
//! project as `assemblies/<slug>.json`.
//!
//! The body is a fragment's content (`grid_size`, `size`, `grids`, `tiles`,
//! `entities`, `notes`) plus `slug`, `name`, `category`, `tags` and `doc`
//! like an entity type's. Placing one is a paste ([`place`]): one batch of
//! existing commands labelled `Place assembly <slug>`. Copies are
//! independent: editing a placed copy never touches the assembly. See
//! `docs/format.md` ("assemblies/<slug>.json").

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::CommandError;
use crate::fragment::{
    self, FRAGMENT_VERSION, Fragment, FragmentEntity, FragmentGrid, PasteOptions, PastePlan,
};
use crate::id::Slug;
use crate::model::{Annotation, Flip, Project, TilePlacement};

/// One assembly, one file: `assemblies/<slug>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Assembly {
    /// Must match the file name.
    pub slug: Slug,
    /// Display name.
    pub name: String,
    /// `/`-separated palette path, e.g. `Rooms/Doorways`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// The project grid (pixels) it was saved in. Placing it into a project
    /// with another grid is refused.
    pub grid_size: u32,
    /// `[width, height]` in cells of that grid.
    pub size: [u32; 2],
    /// int_grid layers by slug, as in a fragment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub grids: BTreeMap<Slug, FragmentGrid>,
    /// tiles layers by slug, as in a fragment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tiles: BTreeMap<Slug, Vec<TilePlacement>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<FragmentEntity>,
    /// Annotations, geometry in pixels from the origin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Annotation>,
}

/// What an assembly is called and where it sits in the palette: the parts
/// of an [`Assembly`] that aren't the fragment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssemblyInfo {
    pub slug: Slug,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

impl Assembly {
    /// An assembly holding `frag` (its `source` is dropped: an assembly
    /// belongs nowhere).
    pub fn new(info: AssemblyInfo, frag: Fragment) -> Self {
        Assembly {
            slug: info.slug,
            name: info.name,
            category: info.category.filter(|c| !c.trim().is_empty()),
            tags: info.tags,
            doc: info.doc.filter(|d| !d.trim().is_empty()),
            grid_size: frag.grid_size,
            size: frag.size,
            grids: frag.grids,
            tiles: frag.tiles,
            entities: frag.entities,
            notes: frag.notes,
        }
    }

    /// The fragment it holds (no `source`).
    pub fn fragment(&self) -> Fragment {
        Fragment {
            worlds_fragment: FRAGMENT_VERSION,
            grid_size: self.grid_size,
            size: self.size,
            source: None,
            grids: self.grids.clone(),
            tiles: self.tiles.clone(),
            entities: self.entities.clone(),
            notes: self.notes.clone(),
        }
    }

    /// Its name, category, tags and doc.
    pub fn info(&self) -> AssemblyInfo {
        AssemblyInfo {
            slug: self.slug.clone(),
            name: self.name.clone(),
            category: self.category.clone(),
            tags: self.tags.clone(),
            doc: self.doc.clone(),
        }
    }

    /// Items in it, as [`Fragment::item_count`].
    pub fn item_count(&self) -> usize {
        self.fragment().item_count()
    }
}

/// Work out placing assembly `slug` with its top-left corner at `origin` (in
/// cells of the project grid of `level`), turned by `flip` first (see
/// [`crate::transform::fragment`]): a paste labelled `Place assembly <slug>`.
pub fn place(
    project: &Project,
    slug: &Slug,
    level: &Slug,
    origin: [i64; 2],
    flip: Option<Flip>,
) -> Result<PastePlan, CommandError> {
    let a = project
        .assembly(slug.as_str())
        .ok_or_else(|| CommandError::UnknownAssembly(slug.clone()))?;
    let mut frag = a.fragment();
    if let Some(f) = flip {
        frag = crate::transform::fragment(project, &frag, f)?;
    }
    let mut plan = fragment::paste(
        project,
        &frag,
        &PasteOptions {
            level: level.clone(),
            origin,
            layer_map: BTreeMap::new(),
        },
    )?;
    plan.label = format!("Place assembly {slug}");
    Ok(plan)
}
