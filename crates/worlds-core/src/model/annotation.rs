//! Annotations: notes, pins, paths, regions and guides. World-scope ones live
//! in `world.json`, level-scope ones in the level's `notes.json`.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use super::common::{Color, VariantKeys, is_default, variant_schema};
use super::layer::misplaced;
use crate::id::{Id, Slug};

/// A named set of guides with its own offset: `world.json` `guide_groups`.
/// Guides store their points as designed; the editor draws a grouped guide
/// at `points + offset` ("as built") unless the view shows guides as designed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuideGroup {
    pub slug: Slug,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// World pixels added to every point of the group's guides when shown as built.
    #[serde(default, skip_serializing_if = "is_default")]
    pub offset: [i64; 2],
    /// Drawing color of the group's guides (over each guide's own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
}

impl GuideGroup {
    /// A group with no name, no offset and no color.
    pub fn new(slug: Slug) -> Self {
        GuideGroup {
            slug,
            name: None,
            offset: [0, 0],
            color: None,
        }
    }
}

/// What an annotation link points at, from its string form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link<'a> {
    /// `level:<slug>`.
    Level(&'a str),
    /// `entity:<id>`.
    Entity(&'a str),
    /// `note:<id>`.
    Note(&'a str),
    /// Anything else.
    Url(&'a str),
}

/// Read a link string: `level:<slug>`, `entity:<id>`, `note:<id>`, or
/// anything else as a URL.
pub fn parse_link(s: &str) -> Link<'_> {
    if let Some(x) = s.strip_prefix("level:") {
        Link::Level(x)
    } else if let Some(x) = s.strip_prefix("entity:") {
        Link::Entity(x)
    } else if let Some(x) = s.strip_prefix("note:") {
        Link::Note(x)
    } else {
        Link::Url(s)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NoteStatus {
    #[default]
    Open,
    Done,
}

/// One annotation. Coordinates are pixels: world pixels at world scope,
/// pixels from the level's top-left corner at level scope.
///
/// On disk the shape's kind is the `kind` key and its geometry keys sit next
/// to the common ones; geometry keys of another kind are a load error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "AnnotationRepr", into = "AnnotationRepr")]
pub struct Annotation {
    pub id: Id,
    pub title: Option<String>,
    /// Markdown.
    pub body: Option<String>,
    pub tags: Vec<String>,
    pub color: Option<Color>,
    /// References: `level:<slug>`, `entity:<id>`, `note:<id>`, or a URL.
    pub links: Vec<String>,
    pub status: NoteStatus,
    /// Included in `export/` for the game.
    pub export: bool,
    /// Not movable in the editor.
    pub locked: bool,
    pub shape: Shape,
}

/// What an annotation is, with its geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// Point + text.
    Note { px: [i64; 2] },
    /// Point + icon + label.
    Pin {
        px: [i64; 2],
        icon: Option<String>,
        label: Option<String>,
    },
    /// Polyline, 2+ points.
    Path { points: Vec<[i64; 2]> },
    /// Polygon (3+ `points`) or `rect` (`[x, y, width, height]`), not both.
    Region {
        points: Vec<[i64; 2]>,
        rect: Option<[i64; 4]>,
    },
    /// Locked reference polyline (or polygon with `closed`), world scope
    /// only, 2+ points. `group` names a `world.json` guide group.
    Guide {
        points: Vec<[i64; 2]>,
        closed: bool,
        group: Option<Slug>,
    },
    /// A line joining two rooms or entities, through `waypoints`. The end
    /// points are computed from what `from` and `to` name (see
    /// [`crate::model::connector_polyline`]), never stored, so the line
    /// follows a moved room or entity.
    Connector {
        from: Endpoint,
        to: Endpoint,
        waypoints: Vec<[i64; 2]>,
        label: Option<String>,
        style: ConnectorStyle,
    },
}

/// What a connector end is anchored to: `{ "level": slug }` or
/// `{ "entity": id }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Endpoint {
    /// A level (room): the end sits on its edge.
    Level(Slug),
    /// An entity instance: the end sits on its pivot.
    Entity(Id),
}

impl Endpoint {
    /// `level:<slug>` or `entity:<id>`, the link form.
    pub fn link(&self) -> String {
        match self {
            Endpoint::Level(s) => format!("level:{s}"),
            Endpoint::Entity(id) => format!("entity:{id}"),
        }
    }
}

/// How a connector is drawn: an arrow at `to` (the default), a plain line,
/// or arrows at both ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorStyle {
    #[default]
    Arrow,
    Line,
    Double,
}

impl ConnectorStyle {
    pub const ALL: [ConnectorStyle; 3] = [
        ConnectorStyle::Arrow,
        ConnectorStyle::Line,
        ConnectorStyle::Double,
    ];

    /// The `style` string.
    pub fn name(self) -> &'static str {
        match self {
            ConnectorStyle::Arrow => "arrow",
            ConnectorStyle::Line => "line",
            ConnectorStyle::Double => "double",
        }
    }
}

impl Shape {
    /// The `kind` string.
    pub fn name(&self) -> &'static str {
        match self {
            Shape::Note { .. } => "note",
            Shape::Pin { .. } => "pin",
            Shape::Path { .. } => "path",
            Shape::Region { .. } => "region",
            Shape::Guide { .. } => "guide",
            Shape::Connector { .. } => "connector",
        }
    }

    /// Move every coordinate by `d` pixels.
    pub fn translate(&mut self, d: [i64; 2]) {
        let shift = |p: &mut [i64; 2]| {
            p[0] += d[0];
            p[1] += d[1];
        };
        match self {
            Shape::Note { px } | Shape::Pin { px, .. } => shift(px),
            Shape::Path { points } | Shape::Guide { points, .. } => {
                points.iter_mut().for_each(shift)
            }
            Shape::Region { points, rect } => {
                points.iter_mut().for_each(shift);
                if let Some(r) = rect {
                    r[0] += d[0];
                    r[1] += d[1];
                }
            }
            Shape::Connector { waypoints, .. } => waypoints.iter_mut().for_each(shift),
        }
    }

    /// Every point of the geometry: the point of a note or pin, the points
    /// of a path, guide or polygon region, the 4 corners of a rect region
    /// (clockwise from the top-left; the far corners are `x + w`, `y + h`),
    /// a connector's waypoints (its ends are computed, not stored:
    /// [`crate::model::connector_polyline`]).
    pub fn points(&self) -> Vec<[i64; 2]> {
        match self {
            Shape::Note { px } | Shape::Pin { px, .. } => vec![*px],
            Shape::Path { points } | Shape::Guide { points, .. } => points.clone(),
            Shape::Connector { waypoints, .. } => waypoints.clone(),
            Shape::Region { points, rect } => match rect {
                Some([x, y, w, h]) => vec![[*x, *y], [x + w, *y], [x + w, y + h], [*x, y + h]],
                None => points.clone(),
            },
        }
    }
}

impl Annotation {
    /// The bounding box of the geometry as `[x, y, w, h]`, in the
    /// annotation's own pixels (world or level), with `x + w` and `y + h`
    /// the largest coordinates (a vertical line has `w` 0, a pin is
    /// `[x, y, 0, 0]`). A rect region's box is its rect. `None` when there
    /// are no points.
    pub fn bbox(&self) -> Option<[i64; 4]> {
        bbox_of(&self.shape.points())
    }

    /// An annotation with a fresh id and every optional field empty.
    pub fn new(shape: Shape) -> Self {
        Annotation {
            id: Id::generate(),
            title: None,
            body: None,
            tags: Vec::new(),
            color: None,
            links: Vec::new(),
            status: NoteStatus::Open,
            export: false,
            locked: false,
            shape,
        }
    }
}

/// `[x, y, w, h]` around some points (see [`Annotation::bbox`]).
pub fn bbox_of(points: &[[i64; 2]]) -> Option<[i64; 4]> {
    let first = points.first()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first[0], first[1], first[0], first[1]);
    for p in points {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    Some([x0, y0, x1 - x0, y1 - y0])
}

#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum KindTag {
    Note,
    Pin,
    Path,
    Region,
    Guide,
    Connector,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AnnotationRepr {
    id: Id,
    /// `note`, `pin`, `path`, `region`, `guide` or `connector`.
    kind: KindTag,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    /// Markdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<Color>,
    /// `level:<slug>`, `entity:<id>`, `note:<id>`, or a URL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    links: Vec<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    status: NoteStatus,
    /// Included in `export/` for the game.
    #[serde(default, skip_serializing_if = "is_default")]
    export: bool,
    /// Not movable in the editor.
    #[serde(default, skip_serializing_if = "is_default")]
    locked: bool,
    /// note, pin (required): the point, in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    px: Option<[i64; 2]>,
    /// path, guide (2+), region polygon (3+): `[[x, y], ...]` in pixels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    points: Vec<[i64; 2]>,
    /// region: `[x, y, width, height]` instead of `points`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rect: Option<[i64; 4]>,
    /// guide: join the last point back to the first.
    #[serde(default, skip_serializing_if = "is_default")]
    closed: bool,
    /// pin: icon name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    /// pin: short label drawn next to the icon. connector: text drawn
    /// along the line ("needs double jump").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    /// guide: the `world.json` guide group it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<Slug>,
    /// connector (required): where it starts, `{ "level": slug }` or
    /// `{ "entity": id }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from: Option<Endpoint>,
    /// connector (required): where it ends, as `from`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    to: Option<Endpoint>,
    /// connector: points the line passes through between its ends,
    /// `[[x, y], ...]` in pixels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    waypoints: Vec<[i64; 2]>,
    /// connector: `arrow` (the default: an arrowhead at `to`), `line` or
    /// `double` (arrowheads at both ends).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    style: Option<ConnectorStyle>,
}

const ANNOTATION_VARIANTS: &[VariantKeys] = &[
    VariantKeys {
        name: "note",
        keys: &["px"],
        required: &["px"],
    },
    VariantKeys {
        name: "pin",
        keys: &["px", "icon", "label"],
        required: &["px"],
    },
    VariantKeys {
        name: "path",
        keys: &["points"],
        required: &["points"],
    },
    VariantKeys {
        name: "region",
        keys: &["points", "rect"],
        required: &[],
    },
    VariantKeys {
        name: "guide",
        keys: &["points", "closed", "group"],
        required: &["points"],
    },
    VariantKeys {
        name: "connector",
        keys: &["label", "from", "to", "waypoints", "style"],
        required: &["from", "to"],
    },
];

impl TryFrom<AnnotationRepr> for Annotation {
    type Error = String;
    fn try_from(r: AnnotationRepr) -> Result<Self, String> {
        let px = ("px", r.px.is_some());
        let points = ("points", !r.points.is_empty());
        let rect = ("rect", r.rect.is_some());
        let closed = ("closed", r.closed);
        let icon = ("icon", r.icon.is_some());
        let label = ("label", r.label.is_some());
        let group = ("group", r.group.is_some());
        let from = ("from", r.from.is_some());
        let to = ("to", r.to.is_some());
        let waypoints = ("waypoints", !r.waypoints.is_empty());
        let style = ("style", r.style.is_some());
        let conn = [from, to, waypoints, style];
        let not_conn = |k: &str| misplaced(k, "annotation", &conn);
        let need_px = |kind: &str| r.px.ok_or(format!("a {kind} needs \"px\""));
        let shape = match r.kind {
            KindTag::Note => {
                not_conn("note")?;
                misplaced(
                    "note",
                    "annotation",
                    &[points, rect, closed, icon, label, group],
                )?;
                Shape::Note {
                    px: need_px("note")?,
                }
            }
            KindTag::Pin => {
                not_conn("pin")?;
                misplaced("pin", "annotation", &[points, rect, closed, group])?;
                Shape::Pin {
                    px: need_px("pin")?,
                    icon: r.icon,
                    label: r.label,
                }
            }
            KindTag::Path => {
                not_conn("path")?;
                misplaced(
                    "path",
                    "annotation",
                    &[px, rect, closed, icon, label, group],
                )?;
                Shape::Path { points: r.points }
            }
            KindTag::Region => {
                not_conn("region")?;
                misplaced("region", "annotation", &[px, closed, icon, label, group])?;
                Shape::Region {
                    points: r.points,
                    rect: r.rect,
                }
            }
            KindTag::Guide => {
                not_conn("guide")?;
                misplaced("guide", "annotation", &[px, rect, icon, label])?;
                Shape::Guide {
                    points: r.points,
                    closed: r.closed,
                    group: r.group,
                }
            }
            KindTag::Connector => {
                misplaced(
                    "connector",
                    "annotation",
                    &[px, points, rect, closed, icon, group],
                )?;
                Shape::Connector {
                    from: r.from.ok_or("a connector needs \"from\"")?,
                    to: r.to.ok_or("a connector needs \"to\"")?,
                    waypoints: r.waypoints,
                    label: r.label,
                    style: r.style.unwrap_or_default(),
                }
            }
        };
        Ok(Annotation {
            id: r.id,
            title: r.title,
            body: r.body,
            tags: r.tags,
            color: r.color,
            links: r.links,
            status: r.status,
            export: r.export,
            locked: r.locked,
            shape,
        })
    }
}

impl From<Annotation> for AnnotationRepr {
    fn from(a: Annotation) -> Self {
        let mut r = AnnotationRepr {
            id: a.id,
            kind: KindTag::Note,
            title: a.title,
            body: a.body,
            tags: a.tags,
            color: a.color,
            links: a.links,
            status: a.status,
            export: a.export,
            locked: a.locked,
            px: None,
            points: Vec::new(),
            rect: None,
            closed: false,
            icon: None,
            label: None,
            group: None,
            from: None,
            to: None,
            waypoints: Vec::new(),
            style: None,
        };
        match a.shape {
            Shape::Note { px } => r.px = Some(px),
            Shape::Pin { px, icon, label } => {
                r.kind = KindTag::Pin;
                r.px = Some(px);
                r.icon = icon;
                r.label = label;
            }
            Shape::Path { points } => {
                r.kind = KindTag::Path;
                r.points = points;
            }
            Shape::Region { points, rect } => {
                r.kind = KindTag::Region;
                r.points = points;
                r.rect = rect;
            }
            Shape::Guide {
                points,
                closed,
                group,
            } => {
                r.kind = KindTag::Guide;
                r.points = points;
                r.closed = closed;
                r.group = group;
            }
            Shape::Connector {
                from,
                to,
                waypoints,
                label,
                style,
            } => {
                r.kind = KindTag::Connector;
                r.from = Some(from);
                r.to = Some(to);
                r.waypoints = waypoints;
                r.label = label;
                r.style = (style != ConnectorStyle::Arrow).then_some(style);
            }
        }
        r
    }
}

impl JsonSchema for Annotation {
    fn schema_name() -> Cow<'static, str> {
        "Annotation".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        variant_schema(
            AnnotationRepr::json_schema(generator),
            Some("kind"),
            ANNOTATION_VARIANTS,
            "An annotation. Geometry keys depend on `kind`; coordinates are pixels.",
        )
    }
}
