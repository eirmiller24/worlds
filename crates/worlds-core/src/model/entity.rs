//! Entity definitions (`entities/<slug>.json`) and instances (`entities.json`).

use std::borrow::Cow;
use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use super::common::{Color, TileRef, VariantKeys, is_default, present, variant_schema};
use super::layer::misplaced;
use crate::id::{Id, Slug};

/// One entity type, one file: `entities/<slug>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityDef {
    pub slug: Slug,
    pub name: String,
    /// `/`-separated palette path, e.g. `Puzzle/Doors`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// `[width, height]` in cells of the project grid.
    pub size: [u32; 2],
    /// Whether width and height can be changed per instance. Default `[false, false]`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub resizable: [bool; 2],
    /// Anchor inside the box, 0 to 1 on each axis. `[0.5, 1.0]` is bottom center. Default `[0, 0]`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub pivot: [f64; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<TileRef>,
    pub color: Color,
    /// Maximum instance count, or none for unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<Limit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldDef>,
}

impl EntityDef {
    pub fn field(&self, slug: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.slug == slug)
    }

    /// The box of instance `inst` of this type, `[x, y, w, h]` in level
    /// pixels. The pivot point is at `(cell + pivot) * layer_grid +
    /// offset_px`; the box is the instance's `size` (else the type's) in
    /// cells of the project grid, placed so that the pivot sits at the same
    /// relative spot inside it. Fractions round to the nearest pixel.
    pub fn pixel_rect(
        &self,
        inst: &EntityInstance,
        layer_grid: u32,
        project_grid: u32,
    ) -> [i32; 4] {
        let size = inst.size.unwrap_or(self.size);
        let axis = |i: usize| {
            let pivot_px = (inst.cell[i] as f64 + self.pivot[i]) * layer_grid as f64
                + inst.offset_px[i] as f64;
            let len = size[i] as f64 * project_grid as f64;
            ((pivot_px - self.pivot[i] * len).round() as i32, len as i32)
        };
        let ((x, w), (y, h)) = (axis(0), axis(1));
        [x, y, w, h]
    }
}

/// Instance count limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Limit {
    pub max: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub scope: LimitScope,
    #[serde(default, skip_serializing_if = "is_default")]
    pub behavior: LimitBehavior,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LimitScope {
    /// Per layer per level.
    Layer,
    #[default]
    Level,
    /// Across the whole project.
    World,
}

/// What the editor does when a new instance would exceed the limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LimitBehavior {
    #[default]
    PreventAdding,
    DiscardOldest,
    MoveLastOne,
}

/// A field's type, with the options that only make sense for that type.
/// With `array: true` (on [`FieldDef`]) the value is a JSON array of these.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldType {
    /// JSON integer, with optional inclusive bounds.
    Int {
        min: Option<serde_json::Number>,
        max: Option<serde_json::Number>,
    },
    /// JSON number, with optional inclusive bounds.
    Float {
        min: Option<serde_json::Number>,
        max: Option<serde_json::Number>,
    },
    /// JSON boolean.
    Bool,
    /// Single-line string; `validate` checks that `regex`, when set, matches
    /// the whole value.
    String { regex: Option<String> },
    /// Multi-line string; `regex` as for `String`.
    Text { regex: Option<String> },
    /// `#rrggbb` string.
    Color,
    /// A value slug of a project enum or of an inline list.
    Enum(EnumSource),
    /// `[x, y]` cell in the same level.
    Point,
    /// Instance id string of another entity. With neither list set, any
    /// entity is allowed; otherwise the target's type must be in
    /// `allowed_types` or carry one of `allowed_tags`.
    EntityRef {
        allowed_types: Vec<Slug>,
        allowed_tags: Vec<String>,
        /// The target may be in another level.
        cross_level: bool,
    },
    /// File path string, relative to the project directory. `extensions`
    /// (without the dot) restricts it when non-empty.
    Path { extensions: Vec<String> },
    /// `{ "tileset": slug, "tile": [col, row] }`, optionally from one tileset.
    Tile { tileset: Option<Slug> },
    /// Any JSON value.
    Json,
}

/// Where an enum field's values come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnumSource {
    /// `"enum": "<slug>"`: a project enum from `enums.json`.
    Project(Slug),
    /// `"values": [...]`: an inline list of value slugs.
    Inline(Vec<Slug>),
}

impl FieldType {
    /// The `type` string.
    pub fn name(&self) -> &'static str {
        match self {
            FieldType::Int { .. } => "int",
            FieldType::Float { .. } => "float",
            FieldType::Bool => "bool",
            FieldType::String { .. } => "string",
            FieldType::Text { .. } => "text",
            FieldType::Color => "color",
            FieldType::Enum(_) => "enum",
            FieldType::Point => "point",
            FieldType::EntityRef { .. } => "entity_ref",
            FieldType::Path { .. } => "path",
            FieldType::Tile { .. } => "tile",
            FieldType::Json => "json",
        }
    }

    /// `int` with no bounds.
    pub fn int() -> Self {
        FieldType::Int {
            min: None,
            max: None,
        }
    }

    /// `float` with no bounds.
    pub fn float() -> Self {
        FieldType::Float {
            min: None,
            max: None,
        }
    }

    /// `string` with no regex.
    pub fn string() -> Self {
        FieldType::String { regex: None }
    }

    /// `entity_ref` to any entity in the same level.
    pub fn entity_ref() -> Self {
        FieldType::EntityRef {
            allowed_types: Vec::new(),
            allowed_tags: Vec::new(),
            cross_level: false,
        }
    }

    /// Inclusive `[min, max]` for `int` and `float`.
    pub fn bounds(&self) -> (Option<&serde_json::Number>, Option<&serde_json::Number>) {
        match self {
            FieldType::Int { min, max } | FieldType::Float { min, max } => {
                (min.as_ref(), max.as_ref())
            }
            _ => (None, None),
        }
    }

    pub fn is_entity_ref(&self) -> bool {
        matches!(self, FieldType::EntityRef { .. })
    }
}

/// A typed field on an entity type or a level.
///
/// On disk the type is the `type` key and the type's own options sit next
/// to the common keys; an option of another type is a load error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "FieldDefRepr", into = "FieldDefRepr")]
pub struct FieldDef {
    pub slug: Slug,
    pub name: Option<String>,
    pub ty: FieldType,
    /// The value is an array of `ty`.
    pub array: bool,
    /// `null` is a valid value.
    pub nullable: bool,
    /// Default value, same shape as an instance value.
    pub default: Option<serde_json::Value>,
    pub doc: Option<String>,
    pub display: FieldDisplay,
}

impl FieldDef {
    /// A field of type `ty` with every common option off.
    pub fn new(slug: Slug, ty: FieldType) -> Self {
        FieldDef {
            slug,
            name: None,
            ty,
            array: false,
            nullable: false,
            default: None,
            doc: None,
            display: FieldDisplay::default(),
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TypeTag {
    Int,
    Float,
    Bool,
    String,
    Text,
    Color,
    Enum,
    Point,
    EntityRef,
    Path,
    Tile,
    Json,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FieldDefRepr {
    slug: Slug,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(rename = "type")]
    ty: TypeTag,
    /// The value is an array of `type`.
    #[serde(default, skip_serializing_if = "is_default")]
    array: bool,
    /// `null` is a valid value.
    #[serde(default, skip_serializing_if = "is_default")]
    nullable: bool,
    /// Default value, same shape as an instance value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<serde_json::Value>,
    /// int and float: inclusive lower bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    min: Option<serde_json::Number>,
    /// int and float: inclusive upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max: Option<serde_json::Number>,
    /// string and text: a regex the whole value must match (Rust `regex`
    /// syntax; `(?i)` ignores case).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    regex: Option<String>,
    /// enum: a project enum slug. Give this or `values`.
    #[serde(rename = "enum", default, skip_serializing_if = "Option::is_none")]
    enum_ref: Option<Slug>,
    /// enum: inline allowed values. Give this or `enum`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    values: Option<Vec<Slug>>,
    // Type-specific keys are `Option`s so a key that is present at its
    // default value still counts as present (see `present`).
    /// entity_ref: allowed entity types.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<Slug>")]
    allowed_types: Option<Vec<Slug>>,
    /// entity_ref: allowed if the target type has any of these tags.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<String>")]
    allowed_tags: Option<Vec<String>>,
    /// entity_ref: the target may be in another level.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "bool")]
    cross_level: Option<bool>,
    /// path: allowed file extensions without the dot.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<String>")]
    extensions: Option<Vec<String>>,
    /// tile: restrict to this tileset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tileset: Option<Slug>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    doc: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    display: FieldDisplay,
}

const FIELD_VARIANTS: &[VariantKeys] = &[
    VariantKeys {
        name: "int",
        keys: &["min", "max"],
        required: &[],
    },
    VariantKeys {
        name: "float",
        keys: &["min", "max"],
        required: &[],
    },
    VariantKeys {
        name: "bool",
        keys: &[],
        required: &[],
    },
    VariantKeys {
        name: "string",
        keys: &["regex"],
        required: &[],
    },
    VariantKeys {
        name: "text",
        keys: &["regex"],
        required: &[],
    },
    VariantKeys {
        name: "color",
        keys: &[],
        required: &[],
    },
    // Exactly one of `enum` and `values`: two branches, each requiring its
    // key and (with `additionalProperties: false`) refusing the other.
    VariantKeys {
        name: "enum",
        keys: &["enum"],
        required: &["enum"],
    },
    VariantKeys {
        name: "enum",
        keys: &["values"],
        required: &["values"],
    },
    VariantKeys {
        name: "point",
        keys: &[],
        required: &[],
    },
    VariantKeys {
        name: "entity_ref",
        keys: &["allowed_types", "allowed_tags", "cross_level"],
        required: &[],
    },
    VariantKeys {
        name: "path",
        keys: &["extensions"],
        required: &[],
    },
    VariantKeys {
        name: "tile",
        keys: &["tileset"],
        required: &[],
    },
    VariantKeys {
        name: "json",
        keys: &[],
        required: &[],
    },
];

impl TryFrom<FieldDefRepr> for FieldDef {
    type Error = String;
    fn try_from(r: FieldDefRepr) -> Result<Self, String> {
        let keys = [
            ("min", r.min.is_some()),
            ("max", r.max.is_some()),
            ("regex", r.regex.is_some()),
            ("enum", r.enum_ref.is_some()),
            ("values", r.values.is_some()),
            ("allowed_types", r.allowed_types.is_some()),
            ("allowed_tags", r.allowed_tags.is_some()),
            ("cross_level", r.cross_level.is_some()),
            ("extensions", r.extensions.is_some()),
            ("tileset", r.tileset.is_some()),
        ];
        let (ty, own): (FieldType, &[&str]) = match r.ty {
            TypeTag::Int => (
                FieldType::Int {
                    min: r.min,
                    max: r.max,
                },
                &["min", "max"],
            ),
            TypeTag::Float => (
                FieldType::Float {
                    min: r.min,
                    max: r.max,
                },
                &["min", "max"],
            ),
            TypeTag::Bool => (FieldType::Bool, &[]),
            TypeTag::String => (FieldType::String { regex: r.regex }, &["regex"]),
            TypeTag::Text => (FieldType::Text { regex: r.regex }, &["regex"]),
            TypeTag::Color => (FieldType::Color, &[]),
            TypeTag::Enum => {
                let src = match (r.enum_ref, r.values) {
                    (Some(e), None) => EnumSource::Project(e),
                    (None, Some(v)) => EnumSource::Inline(v),
                    (Some(_), Some(_)) => {
                        return Err("an enum field takes \"enum\" or \"values\", not both".into());
                    }
                    (None, None) => {
                        return Err(
                            "an enum field needs \"enum\" (a project enum) or \"values\"".into(),
                        );
                    }
                };
                (FieldType::Enum(src), &["enum", "values"])
            }
            TypeTag::Point => (FieldType::Point, &[]),
            TypeTag::EntityRef => (
                FieldType::EntityRef {
                    allowed_types: r.allowed_types.unwrap_or_default(),
                    allowed_tags: r.allowed_tags.unwrap_or_default(),
                    cross_level: r.cross_level.unwrap_or_default(),
                },
                &["allowed_types", "allowed_tags", "cross_level"],
            ),
            TypeTag::Path => (
                FieldType::Path {
                    extensions: r.extensions.unwrap_or_default(),
                },
                &["extensions"],
            ),
            TypeTag::Tile => (FieldType::Tile { tileset: r.tileset }, &["tileset"]),
            TypeTag::Json => (FieldType::Json, &[]),
        };
        let foreign: Vec<(&str, bool)> = keys
            .iter()
            .copied()
            .filter(|(k, _)| !own.contains(k))
            .collect();
        misplaced(ty.name(), "field", &foreign)?;
        Ok(FieldDef {
            slug: r.slug,
            name: r.name,
            ty,
            array: r.array,
            nullable: r.nullable,
            default: r.default,
            doc: r.doc,
            display: r.display,
        })
    }
}

impl From<FieldDef> for FieldDefRepr {
    fn from(f: FieldDef) -> Self {
        let mut r = FieldDefRepr {
            slug: f.slug,
            name: f.name,
            ty: TypeTag::Json,
            array: f.array,
            nullable: f.nullable,
            default: f.default,
            min: None,
            max: None,
            regex: None,
            enum_ref: None,
            values: None,
            allowed_types: None,
            allowed_tags: None,
            cross_level: None,
            extensions: None,
            tileset: None,
            doc: f.doc,
            display: f.display,
        };
        r.ty = match f.ty {
            FieldType::Int { min, max } => {
                (r.min, r.max) = (min, max);
                TypeTag::Int
            }
            FieldType::Float { min, max } => {
                (r.min, r.max) = (min, max);
                TypeTag::Float
            }
            FieldType::Bool => TypeTag::Bool,
            FieldType::String { regex } => {
                r.regex = regex;
                TypeTag::String
            }
            FieldType::Text { regex } => {
                r.regex = regex;
                TypeTag::Text
            }
            FieldType::Color => TypeTag::Color,
            FieldType::Enum(EnumSource::Project(e)) => {
                r.enum_ref = Some(e);
                TypeTag::Enum
            }
            FieldType::Enum(EnumSource::Inline(v)) => {
                r.values = Some(v);
                TypeTag::Enum
            }
            FieldType::Point => TypeTag::Point,
            FieldType::EntityRef {
                allowed_types,
                allowed_tags,
                cross_level,
            } => {
                // Keys at their default are left out.
                r.allowed_types = (!allowed_types.is_empty()).then_some(allowed_types);
                r.allowed_tags = (!allowed_tags.is_empty()).then_some(allowed_tags);
                r.cross_level = cross_level.then_some(true);
                TypeTag::EntityRef
            }
            FieldType::Path { extensions } => {
                r.extensions = (!extensions.is_empty()).then_some(extensions);
                TypeTag::Path
            }
            FieldType::Tile { tileset } => {
                r.tileset = tileset;
                TypeTag::Tile
            }
            FieldType::Json => TypeTag::Json,
        };
        r
    }
}

impl JsonSchema for FieldDef {
    fn schema_name() -> Cow<'static, str> {
        "FieldDef".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        variant_schema(
            FieldDefRepr::json_schema(generator),
            Some("type"),
            FIELD_VARIANTS,
            "A typed field. Options after `default` depend on `type`.",
        )
    }
}

/// Where the editor shows a field's value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FieldDisplay {
    /// Show the value next to the entity on the canvas.
    #[serde(default, skip_serializing_if = "is_default")]
    pub canvas: bool,
    /// entity_ref and point: draw an arrow to the target.
    #[serde(default, skip_serializing_if = "is_default")]
    pub arrow: bool,
    /// Show in the zoomed-out world view.
    #[serde(default, skip_serializing_if = "is_default")]
    pub world: bool,
}

/// All entities of one entities layer in one level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntitiesLayer {
    /// Layer slug (an `entities` layer).
    pub layer: Slug,
    /// Instances, in insertion order (the order they were placed in).
    pub entities: Vec<EntityInstance>,
}

/// One placed entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityInstance {
    pub id: Id,
    /// Entity type slug.
    #[serde(rename = "type")]
    pub ty: Slug,
    /// `[x, y]` cell (of the entities layer's grid) that holds the entity's
    /// pivot. The pivot point sits at the same relative spot in this cell as
    /// in the entity box, so pixel position is
    /// `(cell + def.pivot) * layer grid + offset_px`.
    pub cell: [i32; 2],
    /// Extra pixel offset for off-grid placement. Default `[0, 0]`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub offset_px: [i32; 2],
    /// `[width, height]` in cells of the project grid when resized; omitted
    /// means the definition's size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[u32; 2]>,
    /// Field values by field slug. Missing fields take the definition's default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<Slug, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::slug;

    fn door() -> EntityDef {
        serde_json::from_value(serde_json::json!({
            "slug": "door", "name": "Door", "size": [1, 3], "pivot": [0.5, 1.0], "color": "#c08040"
        }))
        .unwrap()
    }

    fn at(cell: [i32; 2], offset_px: [i32; 2], size: Option<[u32; 2]>) -> EntityInstance {
        EntityInstance {
            id: Id::new("doorcrev22").unwrap(),
            ty: slug("door"),
            cell,
            offset_px,
            size,
            fields: BTreeMap::new(),
        }
    }

    #[test]
    fn pixel_rect_follows_the_position_math() {
        let d = door();
        // format.md's example: a [1, 3] door with pivot [0.5, 1.0] at cell
        // [18, 3] covers cells [18, 1] to [18, 3] on the project grid.
        assert_eq!(
            d.pixel_rect(&at([18, 3], [0, 0], None), 16, 16),
            [288, 16, 16, 48]
        );
        // An 8 px layer in a 16 px project: the cell is a layer cell, the
        // size is still in project cells.
        assert_eq!(
            d.pixel_rect(&at([37, 7], [0, 0], None), 8, 16),
            [292, 16, 16, 48]
        );
        // A 32 px layer: pivot at (2.5 * 32, 2 * 32) = (80, 64).
        assert_eq!(
            d.pixel_rect(&at([2, 1], [0, 0], None), 32, 16),
            [72, 16, 16, 48]
        );
        // Offset and a per-instance size.
        assert_eq!(
            d.pixel_rect(&at([37, 7], [3, -2], Some([2, 1])), 8, 16),
            [287, 46, 32, 16]
        );
        // A top-left pivot puts the box's corner on the cell's corner.
        let mut d = d;
        d.pivot = [0.0, 0.0];
        assert_eq!(
            d.pixel_rect(&at([5, 4], [0, 0], None), 8, 16),
            [40, 32, 16, 48]
        );
    }
}
