//! Semantic checks that the type system and the parser can't make: dangling
//! references, grid shape, glyphs, bounds, duplicates and field values.
//!
//! Problem paths name list items by what identifies them, never by their
//! position in a sorted list: entities and notes by id
//! (`layers[objects].entities[btncrev222].cell`, `notes[notecrev22].links[0]`),
//! tile placements by cell (`tiles[4,2]`). Lists whose order is part of the
//! meaning (layer definitions, values, rules, fields) use indices.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::Value;

use crate::format::{RESERVED_LAYER_SLUGS, RESERVED_RULES_SLUG};
use crate::id::{Id, Slug};
use crate::model::{
    ANCHOR_GLYPH, Annotation, CellMatch, Color, EntityDef, EntityInstance, EnumSource, ExpandError,
    ExpandedRule, FieldDef, FieldType, GroupMode, LayerDef, LayerKind, Legend, Level, LimitScope,
    Macros, MatchSelect, OutOfBounds, Output, Pattern, Project, RegionSelect, Rows, Rule,
    RuleEntry, RuleOutput, RuleSet, Select, Shape, Spacing, Symmetry, TemplateOutput, TemplateRef,
    TileMode, TileRef, World, Write, glyph_problem,
};
use crate::model::{Endpoint, Link, Override, parse_link};

/// One problem found by [`validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// File path relative to the project directory, e.g. `worlds/main/levels/a/collision.json`.
    pub file: String,
    /// Location inside the file, e.g. `rows[3]`, `layers[1].values[0].glyph`
    /// or `layers[objects].entities[btncrev222].cell`. Empty for the whole file.
    pub path: String,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}: {}", self.file, self.message)
        } else {
            write!(f, "{}: {}: {}", self.file, self.path, self.message)
        }
    }
}

/// Check a whole project. An empty result means valid.
pub fn validate(project: &Project) -> Vec<Problem> {
    let mut v = Validator {
        p: project,
        out: Vec::new(),
        entities: BTreeMap::new(),
        regexes: BTreeMap::new(),
    };
    v.index();
    v.project();
    v.layers();
    v.tilesets();
    v.enums();
    v.entity_defs();
    v.rules();
    v.worlds();
    v.references();
    v.out
}

struct Validator<'a> {
    p: &'a Project,
    out: Vec<Problem>,
    /// Every entity instance by id, with its level.
    entities: BTreeMap<&'a Id, (&'a Level, &'a EntityInstance)>,
    /// Every field `regex` of the project, compiled once (anchored to match
    /// the whole value), or why it doesn't compile.
    regexes: BTreeMap<&'a str, Result<regex::Regex, String>>,
}

/// Compile a field's `regex` so that it must match the whole value.
///
/// The pattern is compiled on its own first, so a malformed one (say
/// `a)|(b`) is reported rather than spliced into an anchoring wrapper where
/// its stray parenthesis would close the wrapper's group. The anchors are
/// then added around the parsed pattern, printed back in canonical form, so
/// nothing the user wrote (an extended-mode `# comment` running to the end,
/// for one) can swallow them.
pub(crate) fn compile_regex(pattern: &str) -> Result<regex::Regex, String> {
    fn why(e: impl std::fmt::Display) -> String {
        // A syntax error is several lines (the pattern, a caret, the
        // reason); the reason is the last one.
        let text = e.to_string();
        let last = text.lines().rev().find(|l| !l.trim().is_empty());
        let why = last.map_or(text.as_str(), |l| l.trim());
        why.strip_prefix("error: ").unwrap_or(why).to_string()
    }
    regex::Regex::new(pattern).map_err(why)?;
    let hir = regex_syntax::Parser::new().parse(pattern).map_err(why)?;
    regex::Regex::new(&format!("^(?:{hir})$")).map_err(why)
}

/// Why a `category` palette path is malformed, if it is.
fn category_problem(c: &str) -> Option<String> {
    if c.starts_with('/') || c.ends_with('/') {
        return Some(format!("{c:?} starts or ends with \"/\""));
    }
    for seg in c.split('/') {
        if seg.is_empty() {
            return Some(format!("{c:?} has an empty segment"));
        }
        if seg.trim() != seg {
            return Some(format!("segment {seg:?} of {c:?} has whitespace around it"));
        }
    }
    None
}

/// Where a field value lives, for resolving `point` and `entity_ref`.
#[derive(Clone, Copy)]
struct FieldScope<'a> {
    level: Option<&'a Level>,
    /// Size of the grid `point` values are cells of.
    cells: [u32; 2],
}

/// Glyphs a pattern legend can't define: they are pattern syntax, or need
/// escaping in JSON. `*`, `+` and `.` may be redefined, and `@` (the anchor)
/// may be given a test in a pattern whose rows contain it.
const RESERVED_LEGEND_GLYPHS: [char; 7] = ['!', '@', '[', ']', '|', '\\', '"'];

impl<'a> Validator<'a> {
    fn push(&mut self, file: &str, path: impl Into<String>, message: impl Into<String>) {
        self.out.push(Problem {
            file: file.into(),
            path: path.into(),
            message: message.into(),
        });
    }

    fn index(&mut self) {
        let p = self.p;
        for l in p.levels() {
            for e in l.all_entities() {
                self.entities.entry(&e.id).or_insert((l, e));
            }
        }
        let fields = p
            .level_fields
            .iter()
            .chain(p.entities.iter().flat_map(|e| &e.fields));
        for d in fields {
            if let FieldType::String { regex: Some(r) } | FieldType::Text { regex: Some(r) } = &d.ty
            {
                self.regexes
                    .entry(r.as_str())
                    .or_insert_with(|| compile_regex(r));
            }
        }
    }

    /// Report each slug that appears more than once.
    fn dups<'s>(
        &mut self,
        file: &str,
        what: &str,
        items: impl IntoIterator<Item = (String, &'s str)>,
    ) {
        let mut seen = BTreeSet::new();
        for (path, slug) in items {
            if !seen.insert(slug) {
                self.push(file, path, format!("duplicate {what} {slug:?}"));
            }
        }
    }

    /// Report each key that appears more than once, in the file and at the
    /// path of the repeat: items are (file, path, key).
    fn dups_in<'s>(
        &mut self,
        what: &str,
        items: impl IntoIterator<Item = (String, String, &'s str)>,
    ) {
        let mut seen = BTreeSet::new();
        for (file, path, key) in items {
            if !seen.insert(key) {
                self.push(&file, path, format!("duplicate {what} {key:?}"));
            }
        }
    }

    fn tileset_ref(&mut self, file: &str, path: &str, slug: &Slug) {
        if self.p.tileset(slug.as_str()).is_none() {
            self.push(file, path, format!("unknown tileset {slug:?}"));
        }
    }

    fn tile_ref(&mut self, file: &str, path: &str, t: &TileRef) {
        self.tileset_ref(file, &format!("{path}.tileset"), &t.tileset);
    }

    fn unit_range(&mut self, file: &str, path: &str, x: f64) {
        if !(0.0..=1.0).contains(&x) {
            self.push(file, path, format!("{x} is outside 0..1"));
        }
    }

    // ---- project.json ----

    fn project(&mut self) {
        let p = self.p;
        let f = "project.json";
        if p.grid_size == 0 {
            self.push(f, "grid_size", "must be at least 1");
        }
        let collision = p.collision_layer.as_ref().and_then(|c| p.layer(c.as_str()));
        for (i, v) in p.blocking_values.iter().flatten().enumerate() {
            let path = format!("blocking_values[{i}]");
            match collision {
                None if p.collision_layer.is_none() => {
                    self.push(f, path, "set without a collision_layer")
                }
                Some(l) if l.is_int_grid() && l.value(v.as_str()).is_none() => self.push(
                    f,
                    path,
                    format!(
                        "{:?} is not a value of collision layer {:?}",
                        v.as_str(),
                        l.slug.as_str()
                    ),
                ),
                _ => {} // a bad collision_layer is reported below
            }
        }
        self.dups(
            f,
            "blocking value",
            p.blocking_values
                .iter()
                .flatten()
                .enumerate()
                .map(|(i, v)| (format!("blocking_values[{i}]"), v.as_str())),
        );
        if let Some(c) = &p.collision_layer {
            match p.layer(c.as_str()) {
                Some(l) if l.is_int_grid() => {
                    if l.is_derived() {
                        self.push(
                            f,
                            "collision_layer",
                            format!(
                                "{:?} is a derived layer; the collision layer is painted, it \
                                 can't be computed by rules",
                                c.as_str()
                            ),
                        );
                    }
                    if p.blocking_values.is_none() && p.default_blocking_values().is_none() {
                        self.push(
                            f,
                            "blocking_values",
                            format!(
                                "missing, and collision layer {:?} has none of the default \
                                 blocking kinds, so every border cell is open in the export; \
                                 list the values that block, or [] if none do",
                                c.as_str()
                            ),
                        );
                    }
                }
                Some(_) => self.push(
                    f,
                    "collision_layer",
                    format!("{:?} is not an int_grid layer", c.as_str()),
                ),
                None => self.push(
                    f,
                    "collision_layer",
                    format!("unknown layer {:?}", c.as_str()),
                ),
            }
        }
        self.dups(
            f,
            "level field",
            p.level_fields
                .iter()
                .enumerate()
                .map(|(i, d)| (format!("level_fields[{i}]"), d.slug.as_str())),
        );
        for (i, d) in p.level_fields.iter().enumerate() {
            self.field_def(f, &format!("level_fields[{i}]"), d);
        }
    }

    // ---- layers.json ----

    fn layers(&mut self) {
        let p = self.p;
        let f = "layers.json";
        self.dups(
            f,
            "layer",
            p.layers
                .iter()
                .enumerate()
                .map(|(i, l)| (format!("layers[{i}]"), l.slug.as_str())),
        );
        for (i, l) in p.layers.iter().enumerate() {
            let at = format!("layers[{i}]");
            self.layer_def(f, &at, l);
        }
        if let Err(cycle) = p.auto_layer_order() {
            let i = cycle
                .path
                .first()
                .and_then(|s| p.layers.iter().position(|l| l.slug == *s))
                .unwrap_or(0);
            self.push(f, format!("layers[{i}]"), cycle.to_string());
        }
    }

    fn layer_def(&mut self, f: &str, at: &str, l: &LayerDef) {
        // Every layer kind that owns a file must not collide with a fixed
        // file name: int_grid and tiles content is `<level>/<slug>.json`, an
        // auto layer's rules are `rules/<slug>.json` (next to `macros.json`).
        let s = l.slug.as_str();
        let reserved = match &l.kind {
            LayerKind::IntGrid(_) | LayerKind::Tiles(_) => RESERVED_LAYER_SLUGS
                .contains(&s)
                .then_some("int_grid and tiles layers are saved as <level>/<slug>.json"),
            LayerKind::Auto(_) => (s == RESERVED_RULES_SLUG)
                .then_some("auto layers keep their rules in rules/<slug>.json"),
            LayerKind::Entities => None,
        };
        if let Some(why) = reserved {
            self.push(f, format!("{at}.slug"), format!("{s:?} is reserved: {why}"));
        }
        self.unit_range(f, &format!("{at}.opacity"), l.opacity);
        let pg = self.p.grid_size.max(1);
        if let Some(g) = l.grid_size {
            if g == 0 {
                self.push(f, format!("{at}.grid_size"), "must be at least 1");
            } else if !(pg.is_multiple_of(g) || g.is_multiple_of(pg)) {
                self.push(
                    f,
                    format!("{at}.grid_size"),
                    format!(
                        "{g} px neither divides nor is a multiple of the project grid ({pg} px)"
                    ),
                );
            }
        }
        match &l.kind {
            LayerKind::Auto(a) => {
                let s = &a.source;
                match self.p.layer(s.as_str()) {
                    Some(src) if src.is_int_grid() => {
                        let sg = self.p.layer_grid(src);
                        if let Some(g) = l.grid_size
                            && g != sg
                        {
                            self.push(
                                f,
                                format!("{at}.grid_size"),
                                format!(
                                    "an auto layer uses its source's grid ({sg} px for {s:?}); \
                                     leave grid_size out"
                                ),
                            );
                        }
                    }
                    Some(_) => self.push(
                        f,
                        format!("{at}.source"),
                        format!("{s:?} is not an int_grid layer"),
                    ),
                    None => self.push(f, format!("{at}.source"), format!("unknown layer {s:?}")),
                }
                self.unit_range(f, &format!("{at}.tile_pivot[0]"), a.tile_pivot[0]);
                self.unit_range(f, &format!("{at}.tile_pivot[1]"), a.tile_pivot[1]);
            }
            LayerKind::Tiles(t) => {
                if let Some(ts) = &t.tileset {
                    self.tileset_ref(f, &format!("{at}.tileset"), ts);
                }
            }
            LayerKind::Entities => {}
            LayerKind::IntGrid(g) => {
                // Values and groups share one namespace, since patterns name either.
                let names = g
                    .values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (format!("{at}.values[{i}]"), v.slug.as_str()))
                    .chain(
                        g.groups
                            .iter()
                            .enumerate()
                            .map(|(i, x)| (format!("{at}.groups[{i}]"), x.slug.as_str())),
                    );
                self.dups(f, "value or group", names.collect::<Vec<_>>());
                // So do their glyphs.
                let glyphs = g
                    .values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (format!("{at}.values[{i}].glyph"), v.glyph))
                    .chain(g.groups.iter().enumerate().filter_map(|(i, x)| {
                        x.glyph.map(|c| (format!("{at}.groups[{i}].glyph"), c))
                    }));
                let mut seen = BTreeSet::new();
                for (path, c) in glyphs.collect::<Vec<_>>() {
                    if let Some(msg) = glyph_problem(c) {
                        self.push(f, &path, msg);
                    }
                    if !seen.insert(c) {
                        self.push(f, &path, format!("duplicate glyph {c:?}"));
                    }
                }
                for (i, v) in g.values.iter().enumerate() {
                    if let Some(gs) = &v.group
                        && !g.groups.iter().any(|x| &x.slug == gs)
                    {
                        self.push(
                            f,
                            format!("{at}.values[{i}].group"),
                            format!("unknown group {gs:?}"),
                        );
                    }
                }
                if g.values.len() >= u16::MAX as usize {
                    self.push(f, format!("{at}.values"), "too many values");
                }
            }
        }
    }

    // ---- tilesets.json / enums.json ----

    fn tilesets(&mut self) {
        let p = self.p;
        let f = "tilesets.json";
        self.dups(
            f,
            "tileset",
            p.tilesets
                .iter()
                .enumerate()
                .map(|(i, t)| (format!("tilesets[{i}]"), t.slug.as_str())),
        );
        for (i, t) in p.tilesets.iter().enumerate() {
            if t.tile_size[0] == 0 || t.tile_size[1] == 0 {
                self.push(
                    f,
                    format!("tilesets[{i}].tile_size"),
                    "must be at least 1x1",
                );
            }
            if t.image.is_empty() {
                self.push(f, format!("tilesets[{i}].image"), "must not be empty");
            }
        }
    }

    fn enums(&mut self) {
        let p = self.p;
        let f = "enums.json";
        self.dups(
            f,
            "enum",
            p.enums
                .iter()
                .enumerate()
                .map(|(i, e)| (format!("enums[{i}]"), e.slug.as_str())),
        );
        for (i, e) in p.enums.iter().enumerate() {
            self.dups(
                f,
                "enum value",
                e.values
                    .iter()
                    .enumerate()
                    .map(|(j, v)| (format!("enums[{i}].values[{j}]"), v.slug.as_str()))
                    .collect::<Vec<_>>(),
            );
            for (j, v) in e.values.iter().enumerate() {
                if let Some(icon) = &v.icon {
                    self.tile_ref(f, &format!("enums[{i}].values[{j}].icon"), icon);
                }
            }
        }
    }

    // ---- entities/*.json ----

    fn entity_defs(&mut self) {
        let p = self.p;
        self.dups(
            "entities",
            "entity type",
            p.entities
                .iter()
                .map(|e| (format!("{}.json", e.slug), e.slug.as_str())),
        );
        for e in &p.entities {
            let f = format!("entities/{}.json", e.slug);
            if e.size[0] == 0 || e.size[1] == 0 {
                self.push(&f, "size", "must be at least 1x1");
            }
            if let Some(why) = e.category.as_deref().and_then(category_problem) {
                self.push(
                    &f,
                    "category",
                    format!("{why}; a category is a path like \"Puzzle/Doors\""),
                );
            }
            self.dups(
                &f,
                "tag",
                e.tags
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (format!("tags[{i}]"), t.as_str()))
                    .collect::<Vec<_>>(),
            );
            self.unit_range(&f, "pivot[0]", e.pivot[0]);
            self.unit_range(&f, "pivot[1]", e.pivot[1]);
            if let Some(icon) = &e.icon {
                self.tile_ref(&f, "icon", icon);
            }
            if let Some(limit) = &e.limit
                && limit.max == 0
            {
                self.push(
                    &f,
                    "limit.max",
                    "must be at least 1 (omit limit for unlimited)",
                );
            }
            self.dups(
                &f,
                "field",
                e.fields
                    .iter()
                    .enumerate()
                    .map(|(i, d)| (format!("fields[{i}]"), d.slug.as_str()))
                    .collect::<Vec<_>>(),
            );
            for (i, d) in e.fields.iter().enumerate() {
                self.field_def(&f, &format!("fields[{i}]"), d);
            }
        }
    }

    fn field_def(&mut self, f: &str, at: &str, d: &FieldDef) {
        if d.display.arrow && !matches!(d.ty, FieldType::EntityRef { .. } | FieldType::Point) {
            self.push(
                f,
                format!("{at}.display.arrow"),
                format!("not allowed on a {} field", d.ty.name()),
            );
        }
        match &d.ty {
            FieldType::Enum(EnumSource::Project(e)) if self.p.enum_def(e.as_str()).is_none() => {
                self.push(f, format!("{at}.enum"), format!("unknown enum {e:?}"))
            }
            FieldType::EntityRef { allowed_types, .. } => {
                for (i, t) in allowed_types.iter().enumerate() {
                    if self.p.entity_def(t.as_str()).is_none() {
                        self.push(
                            f,
                            format!("{at}.allowed_types[{i}]"),
                            format!("unknown entity type {t:?}"),
                        );
                    }
                }
            }
            FieldType::Tile { tileset: Some(t) } => {
                self.tileset_ref(f, &format!("{at}.tileset"), t);
            }
            FieldType::String { regex: Some(r) } | FieldType::Text { regex: Some(r) } => {
                if let Some(Err(why)) = self.regexes.get(r.as_str()) {
                    let msg = format!("regex {r:?} doesn't compile: {why}");
                    self.push(f, format!("{at}.regex"), msg);
                }
            }
            _ => {}
        }
        if let Some(def) = &d.default {
            self.field_value(
                f,
                &format!("{at}.default"),
                d,
                def,
                FieldScope {
                    level: None,
                    cells: [0, 0],
                },
            );
        }
    }

    /// Check one field value (or array of values) against its definition.
    fn field_value(&mut self, f: &str, at: &str, d: &FieldDef, v: &Value, scope: FieldScope<'a>) {
        if v.is_null() {
            if !d.nullable {
                self.push(f, at, "null is not allowed (field is not nullable)");
            }
            return;
        }
        if d.array {
            let Some(items) = v.as_array() else {
                self.push(f, at, format!("expected an array of {}", d.ty.name()));
                return;
            };
            for (i, item) in items.iter().enumerate() {
                // `nullable` also covers the elements (LDtk's nullable arrays
                // hold null slots).
                let msg = if item.is_null() {
                    (!d.nullable).then(|| "null is not allowed (field is not nullable)".into())
                } else {
                    self.scalar_error(d, item, scope)
                };
                if let Some(msg) = msg {
                    self.push(f, format!("{at}[{i}]"), msg);
                }
            }
        } else if let Some(msg) = self.scalar_error(d, v, scope) {
            self.push(f, at, msg);
        }
    }

    fn scalar_error(&self, d: &FieldDef, v: &Value, scope: FieldScope<'a>) -> Option<String> {
        let (min, max) = d.ty.bounds();
        let num_bounds = |x: f64| -> Option<String> {
            if let Some(min) = min.and_then(|n| n.as_f64())
                && x < min
            {
                return Some(format!("{x} is below min {min}"));
            }
            if let Some(max) = max.and_then(|n| n.as_f64())
                && x > max
            {
                return Some(format!("{x} is above max {max}"));
            }
            None
        };
        let expect = |what: &str| Some(format!("expected {what}, got {v}"));
        match &d.ty {
            FieldType::Int { .. } => match v.as_i64() {
                Some(x) => num_bounds(x as f64),
                None => expect("an integer"),
            },
            FieldType::Float { .. } => match v.as_f64() {
                Some(x) => num_bounds(x),
                None => expect("a number"),
            },
            FieldType::Bool => {
                (!v.is_boolean()).then(|| format!("expected true or false, got {v}"))
            }
            FieldType::String { regex } | FieldType::Text { regex } => {
                let Some(s) = v.as_str() else {
                    return expect("a string");
                };
                let r = regex.as_deref()?;
                // A pattern that doesn't compile is reported on the definition.
                let re = self.regexes.get(r)?.as_ref().ok()?;
                (!re.is_match(s)).then(|| {
                    format!("{s:?} doesn't match regex {r:?} (the whole value must match)")
                })
            }
            FieldType::Path { extensions } => {
                let Some(s) = v.as_str() else {
                    return expect("a path string");
                };
                let ok = extensions.is_empty()
                    || extensions.iter().any(|e| {
                        s.rsplit_once('.')
                            .is_some_and(|(_, x)| x.eq_ignore_ascii_case(e))
                    });
                (!ok).then(|| format!("{s:?} does not have one of the extensions {extensions:?}"))
            }
            FieldType::Color => match v.as_str() {
                Some(s) if Color::is_valid(s) => None,
                _ => expect("a #rrggbb color"),
            },
            FieldType::Enum(src) => {
                let Some(s) = v.as_str() else {
                    return expect("an enum value string");
                };
                let ok = match src {
                    EnumSource::Project(e) => {
                        self.p.enum_def(e.as_str()).is_none_or(|e| e.has_value(s))
                    }
                    EnumSource::Inline(values) => values.iter().any(|x| x == s),
                };
                (!ok).then(|| format!("{s:?} is not a value of this enum"))
            }
            FieldType::Point => {
                let xy: Option<[i64; 2]> = serde_json::from_value(v.clone()).ok();
                let Some([x, y]) = xy else {
                    return expect("[x, y]");
                };
                scope.level?;
                let [w, h] = scope.cells;
                let inside = x >= 0 && y >= 0 && x < w as i64 && y < h as i64;
                (!inside).then(|| format!("point [{x}, {y}] is outside the level"))
            }
            FieldType::EntityRef {
                allowed_types,
                allowed_tags,
                cross_level,
            } => {
                let Some(s) = v.as_str() else {
                    return expect("an entity id string");
                };
                if !Id::is_valid(s) {
                    return Some(format!("{s:?} is not a valid id"));
                }
                // Defaults can't point at a real entity.
                scope.level?;
                let id = Id::new(s).ok()?;
                let Some((target_level, target)) = self.entities.get(&id) else {
                    return Some(format!("no entity with id {s:?}"));
                };
                if !cross_level && scope.level.is_some_and(|l| l.id != target_level.id) {
                    return Some(format!(
                        "entity {s:?} is in level {:?} and cross_level is off",
                        target_level.slug.as_str()
                    ));
                }
                let type_ok = allowed_types.is_empty() && allowed_tags.is_empty()
                    || allowed_types.contains(&target.ty)
                    || self
                        .p
                        .entity_def(target.ty.as_str())
                        .is_some_and(|def| def.tags.iter().any(|t| allowed_tags.contains(t)));
                (!type_ok).then(|| {
                    format!(
                        "entity {s:?} has type {:?}, which this field doesn't allow",
                        target.ty.as_str()
                    )
                })
            }
            FieldType::Tile { tileset } => {
                let t: Option<TileRef> = serde_json::from_value(v.clone()).ok();
                let Some(t) = t else {
                    return expect("{ \"tileset\": ..., \"tile\": [col, row] }");
                };
                if self.p.tileset(t.tileset.as_str()).is_none() {
                    return Some(format!("unknown tileset {:?}", t.tileset.as_str()));
                }
                match tileset {
                    Some(only) if only != &t.tileset => {
                        Some(format!("tile must come from tileset {:?}", only.as_str()))
                    }
                    _ => None,
                }
            }
            FieldType::Json => None,
        }
    }

    // ---- rules/*.json ----

    fn rules(&mut self) {
        let p = self.p;
        self.dups(
            "rules",
            "rule set for layer",
            p.rules
                .iter()
                .map(|r| (format!("{}.json", r.layer), r.layer.as_str())),
        );
        self.macros();
        for set in &p.rules {
            self.rule_set(set);
        }
    }

    fn rule_set(&mut self, set: &'a RuleSet) {
        let f = format!("rules/{}.json", set.layer);
        let layer = match self.p.layer(set.layer.as_str()) {
            Some(l) if l.is_auto() => Some(l),
            Some(_) => {
                self.push(
                    &f,
                    "layer",
                    format!("{:?} is not an auto layer", set.layer.as_str()),
                );
                None
            }
            None => {
                self.push(
                    &f,
                    "layer",
                    format!("unknown layer {:?}", set.layer.as_str()),
                );
                None
            }
        };
        self.dups(
            &f,
            "stamp",
            set.stamps
                .iter()
                .enumerate()
                .map(|(i, s)| (format!("stamps[{i}]"), s.slug.as_str()))
                .collect::<Vec<_>>(),
        );
        for (i, s) in set.stamps.iter().enumerate() {
            self.tileset_ref(&f, &format!("stamps[{i}].tileset"), &s.tileset);
            if s.rect[2] == 0 || s.rect[3] == 0 {
                self.push(
                    &f,
                    format!("stamps[{i}].rect"),
                    "width and height must be at least 1",
                );
            }
            self.unit_range(&f, &format!("stamps[{i}].pivot[0]"), s.pivot[0]);
            self.unit_range(&f, &format!("stamps[{i}].pivot[1]"), s.pivot[1]);
        }
        self.dups(
            &f,
            "group",
            set.groups
                .iter()
                .enumerate()
                .map(|(i, g)| (format!("groups[{i}]"), g.slug.as_str()))
                .collect::<Vec<_>>(),
        );
        // Effective slugs: inline rules, references and every expanded
        // rule share one namespace (expanded slugs feed the hash).
        let mut slugs: Vec<(String, String)> = Vec::new();
        let mut expanded: Vec<Vec<Result<Vec<ExpandedRule<'a>>, ExpandError>>> = Vec::new();
        for (i, g) in set.groups.iter().enumerate() {
            let mut row = Vec::new();
            for (j, e) in g.rules.iter().enumerate() {
                let at = format!("groups[{i}].rules[{j}]");
                slugs.push((at.clone(), e.slug().to_string()));
                let x = self.p.expand_entry(e, j);
                if let (RuleEntry::Use(_), Ok(rules)) = (e, &x) {
                    for r in rules {
                        slugs.push((
                            format!("{at}.expanded[{}]", r.rule.slug),
                            r.rule.slug.to_string(),
                        ));
                    }
                }
                row.push(x);
            }
            expanded.push(row);
        }
        self.dups(
            &f,
            "rule slug",
            slugs
                .iter()
                .map(|(a, s)| (a.clone(), s.as_str()))
                .collect::<Vec<_>>(),
        );
        let mut highest: Option<u32> = None;
        for (i, g) in set.groups.iter().enumerate() {
            let gat = format!("groups[{i}]");
            if let Some(h) = highest
                && g.pass < h
            {
                self.push(
                    &f,
                    format!("{gat}.pass"),
                    format!(
                        "group {} (pass {}) comes after a pass {h} group; keep the file in pass \
                         order",
                        g.slug, g.pass
                    ),
                );
            }
            highest = Some(highest.map_or(g.pass, |h| h.max(g.pass)));
            if let Some(req) = &g.requires_level_tag {
                let rat = format!("{gat}.requires_level_tag");
                if req.tags.is_empty() {
                    self.push(&f, format!("{rat}.tags"), "needs at least one tag");
                }
                self.tags(&f, &format!("{rat}."), &req.tags);
            }
            if let Some(icon) = &g.icon {
                self.tile_ref(&f, &format!("{gat}.icon"), icon);
            }
            if let Some(req) = &g.requires_level_field {
                let rat = format!("{gat}.requires_level_field");
                match self.p.level_fields.iter().find(|d| d.slug == req.field) {
                    None => self.push(
                        &f,
                        format!("{rat}.field"),
                        format!("unknown level field {:?}", req.field.as_str()),
                    ),
                    Some(d) => match &d.ty {
                        FieldType::Enum(src) => {
                            for (k, val) in req.values.iter().enumerate() {
                                let ok = match src {
                                    EnumSource::Project(e) => self
                                        .p
                                        .enum_def(e.as_str())
                                        .is_none_or(|e| e.has_value(val.as_str())),
                                    EnumSource::Inline(values) => values.contains(val),
                                };
                                if !ok {
                                    self.push(
                                        &f,
                                        format!("{rat}.values[{k}]"),
                                        format!(
                                            "{:?} is not a value of the field's enum",
                                            val.as_str()
                                        ),
                                    );
                                }
                            }
                        }
                        _ => self.push(&f, format!("{rat}.field"), "must be an enum level field"),
                    },
                }
            }
            for (j, e) in g.rules.iter().enumerate() {
                let at = format!("{gat}.rules[{j}]");
                let x = std::mem::replace(&mut expanded[i][j], Ok(Vec::new()));
                match e {
                    RuleEntry::Rule(r) => match x {
                        Ok(rules) => {
                            for x in &rules {
                                self.rule(&f, &at, set, layer, &x.rule, Some(r));
                            }
                        }
                        Err(err) => self.expand_problem(&f, &at, r, &err),
                    },
                    RuleEntry::Use(u) => self.reference(&f, &at, set, layer, u, x),
                }
            }
        }
    }

    /// Where a named row or legend that doesn't resolve is reported.
    fn expand_problem(&mut self, f: &str, at: &str, r: &Rule, err: &ExpandError) {
        let key = |p: &Pattern| match err {
            ExpandError::UnknownPattern { name, .. } => {
                matches!(&p.rows, Rows::Named(n) if n == name).then_some("rows")
            }
            ExpandError::UnknownLegend { name, .. } => {
                matches!(&p.legend, Legend::Named(n) if n == name).then_some("legend")
            }
            _ => None,
        };
        let n = r.as_match().map_or(0, |m| m.require.len());
        let path = r
            .patterns()
            .enumerate()
            .find_map(|(i, p)| {
                let k = key(p)?;
                Some(match i {
                    0 => format!("{at}.match.{k}"),
                    _ if n == 1 => format!("{at}.require.{k}"),
                    i => format!("{at}.require[{}].{k}", i - 1),
                })
            })
            .unwrap_or_else(|| at.to_string());
        self.push(f, path, err.to_string());
    }

    /// A `{ "use": ... }` entry: the reference's own keys, then every rule
    /// it expands to, checked like an inline rule against this layer.
    fn reference(
        &mut self,
        f: &str,
        at: &str,
        set: &RuleSet,
        layer: Option<&'a LayerDef>,
        u: &TemplateRef,
        x: Result<Vec<ExpandedRule<'_>>, ExpandError>,
    ) {
        if let Some(ts) = &u.tileset {
            self.tileset_ref(f, &format!("{at}.tileset"), ts);
        }
        for glyph in u.legend.keys() {
            if (RESERVED_LEGEND_GLYPHS.contains(glyph) && *glyph != ANCHOR_GLYPH)
                || !glyph.is_ascii_graphic()
            {
                self.push(
                    f,
                    format!("{at}.legend.{glyph}"),
                    format!("{glyph:?} can't be a pattern glyph"),
                );
            }
        }
        if let Some(t) = self.p.template(u.template.as_str()) {
            let known = |s: &Slug| t.rules.iter().any(|r| r.slug == *s);
            for s in u.tiles.keys() {
                match t.rules.iter().find(|r| r.slug == *s) {
                    None => self.push(
                        f,
                        format!("{at}.tiles.{s}"),
                        format!(
                            "template {:?} has no rule {:?}",
                            u.template.as_str(),
                            s.as_str()
                        ),
                    ),
                    Some(r) if matches!(r.out, TemplateOutput::Stamp(_)) => self.push(
                        f,
                        format!("{at}.tiles.{s}"),
                        format!("template rule {:?} places a stamp, not tiles", s.as_str()),
                    ),
                    Some(_) => {}
                }
            }
            for (k, s) in u.skip.iter().enumerate() {
                if !known(s) {
                    self.push(
                        f,
                        format!("{at}.skip[{k}]"),
                        format!(
                            "template {:?} has no rule {:?}",
                            u.template.as_str(),
                            s.as_str()
                        ),
                    );
                }
            }
        }
        match x {
            Err(err) => {
                let path = match &err {
                    ExpandError::UnknownTemplate { .. } => format!("{at}.use"),
                    ExpandError::NoTileset { .. } => format!("{at}.tileset"),
                    _ => at.to_string(),
                };
                self.push(f, path, err.to_string());
            }
            Ok(rules) => {
                for x in &rules {
                    let xat = format!("{at}.expanded[{}]", x.rule.slug);
                    self.rule(f, &xat, set, layer, &x.rule, None);
                }
            }
        }
    }

    /// The int_grid layer a pattern reads, reporting a bad `grid`.
    fn pattern_grid(
        &mut self,
        f: &str,
        pat_at: &str,
        pat: &Pattern,
        layer: Option<&'a LayerDef>,
    ) -> (Option<&'a LayerDef>, String) {
        self.grid_ref(f, &format!("{pat_at}.grid"), pat.grid.as_ref(), layer)
    }

    /// The int_grid layer a `grid` key (at `key_at`) names, defaulting to
    /// the auto layer's source; reports an unknown one, a layer that isn't
    /// int_grid, and one on another grid than the auto layer.
    fn grid_ref(
        &mut self,
        f: &str,
        key_at: &str,
        given: Option<&Slug>,
        layer: Option<&'a LayerDef>,
    ) -> (Option<&'a LayerDef>, String) {
        let source = layer.and_then(|l| l.auto()).map(|a| &a.source);
        let grid_slug = given.or(source);
        let grid = match grid_slug {
            None => None,
            Some(s) => match self.p.layer(s.as_str()) {
                Some(l) if l.is_int_grid() => Some(l),
                Some(_) => {
                    self.push(
                        f,
                        key_at,
                        format!("{:?} is not an int_grid layer", s.as_str()),
                    );
                    None
                }
                None => {
                    // An unknown default grid is reported on the auto layer.
                    if given.is_some() {
                        self.push(f, key_at, format!("unknown layer {:?}", s.as_str()));
                    }
                    None
                }
            },
        };
        if let (Some(g), Some(l)) = (grid, layer) {
            let (gg, lg) = (self.p.layer_grid(g), self.p.layer_grid(l));
            if gg != lg {
                self.push(
                    f,
                    key_at,
                    format!(
                        "{:?} is on a {gg} px grid, but this layer bakes on {lg} px",
                        g.slug.as_str()
                    ),
                );
            }
        }
        (grid, grid_slug.map_or("?", |s| s.as_str()).to_string())
    }

    /// The value and group slugs of a cell test that `grid` doesn't have.
    fn cell_test_slugs(
        &mut self,
        f: &str,
        at: &str,
        test: &CellMatch,
        grid: Option<&LayerDef>,
        grid_name: &str,
    ) {
        let Some(g) = grid else { return };
        for s in test.slugs() {
            if g.value(s.as_str()).is_none() && g.group(s.as_str()).is_none() {
                self.push(
                    f,
                    at,
                    format!("{:?} is not a value or group of {grid_name:?}", s.as_str()),
                );
            }
        }
    }

    /// One resolved pattern (`match` or `require`). `written` is the
    /// pattern as the file has it, for an inline rule: its legend keys are
    /// checked; an expanded rule's merged legend may hold unused keys.
    fn pattern(
        &mut self,
        f: &str,
        pat_at: &str,
        pat: &Pattern,
        written: Option<&Pattern>,
        layer: Option<&'a LayerDef>,
    ) -> Option<&'a LayerDef> {
        let (grid, grid_name) = self.pattern_grid(f, pat_at, pat, layer);
        let known = |slug: &Slug| {
            grid.is_none_or(|g| {
                g.value(slug.as_str()).is_some() || g.group(slug.as_str()).is_some()
            })
        };
        let rows = pat.inline_rows();
        let used: BTreeSet<char> = rows.iter().flat_map(|r| r.chars()).collect();
        let has_anchor = used.contains(&ANCHOR_GLYPH);
        let legend = pat.inline_legend().cloned().unwrap_or_default();
        let written_legend = written.and_then(|w| w.inline_legend());
        if let Some(wl) = written_legend {
            for glyph in wl.keys() {
                if *glyph == ANCHOR_GLYPH {
                    if !has_anchor {
                        self.push(
                            f,
                            format!("{pat_at}.legend.{glyph}"),
                            "'@' is only a legend key in a pattern whose rows contain @",
                        );
                    }
                } else if RESERVED_LEGEND_GLYPHS.contains(glyph) || !glyph.is_ascii_graphic() {
                    self.push(
                        f,
                        format!("{pat_at}.legend.{glyph}"),
                        format!("{glyph:?} can't be a pattern glyph"),
                    );
                }
            }
        }
        for (glyph, m) in &legend {
            // An expanded rule's merged legend may carry entries its rows
            // don't use; only the used ones must make sense.
            if written_legend.is_none() && !used.contains(glyph) {
                continue;
            }
            for s in m.slugs() {
                if !known(s) {
                    self.push(
                        f,
                        format!("{pat_at}.legend.{glyph}"),
                        format!("{:?} is not a value or group of {grid_name:?}", s.as_str()),
                    );
                }
            }
        }
        let [w, h] = pat.size();
        let anchors = pat.anchor_count();
        if rows.is_empty() || w == 0 {
            self.push(f, format!("{pat_at}.rows"), "pattern is empty");
        } else if anchors > 1 {
            self.push(
                f,
                format!("{pat_at}.rows"),
                format!("pattern has {anchors} @ glyphs; at most one cell is the anchor"),
            );
        } else if anchors == 0 && (w % 2 == 0 || h % 2 == 0) {
            self.push(
                f,
                format!("{pat_at}.rows"),
                format!(
                    "pattern is {w}x{h}; without an @ anchor, width and height must be odd \
                     (the center is the anchor)"
                ),
            );
        }
        for (y, row) in rows.iter().enumerate() {
            if row.chars().count() != w as usize {
                self.push(
                    f,
                    format!("{pat_at}.rows[{y}]"),
                    format!("row has {} glyphs, expected {w}", row.chars().count()),
                );
            }
            if grid.is_none() {
                continue;
            }
            for c in row.chars().collect::<BTreeSet<_>>() {
                if pat.resolve(c, grid).is_none() {
                    self.push(
                        f,
                        format!("{pat_at}.rows[{y}]"),
                        format!(
                            "glyph {c:?} means nothing: not in the legend, not a built-in \
                             (* + . @), not a value or group glyph of {grid_name:?}"
                        ),
                    );
                }
            }
        }
        grid
    }

    /// One rule as it runs (an inline rule with names resolved, or an
    /// expanded template rule). `written` is the inline rule as the file
    /// has it, `None` for an expanded rule.
    fn rule(
        &mut self,
        f: &str,
        at: &str,
        set: &RuleSet,
        layer: Option<&'a LayerDef>,
        r: &Rule,
        written: Option<&Rule>,
    ) {
        match &r.select {
            Select::Match(m) => {
                self.match_select(f, at, layer, m, written.and_then(Rule::as_match));
            }
            Select::Region(g) => self.region_select(f, at, layer, g),
        }
        match &r.output {
            Output::Draw(out) => {
                let rotates = r
                    .as_match()
                    .is_some_and(|m| m.symmetry.contains(&Symmetry::Rotate));
                self.draw_output(f, at, set, out, rotates);
            }
            Output::Write(w) => self.write_output(f, at, set, layer, w),
        }
    }

    /// A pattern rule's `match`, `require` patterns and their options.
    fn match_select(
        &mut self,
        f: &str,
        at: &str,
        layer: Option<&'a LayerDef>,
        m: &MatchSelect,
        written: Option<&MatchSelect>,
    ) {
        let mat = format!("{at}.match");
        let grid = self.pattern(f, &mat, &m.pattern, written.map(|w| &w.pattern), layer);
        let mut grids = vec![grid];
        let n = m.require.len();
        for (k, req) in m.require.iter().enumerate() {
            let rat = if n == 1 {
                format!("{at}.require")
            } else {
                format!("{at}.require[{k}]")
            };
            let w = written.and_then(|w| w.require.get(k));
            grids.push(self.pattern(f, &rat, req, w, layer));
        }
        if let Some(OutOfBounds::Value(s)) = &m.out_of_bounds {
            for g in grids.iter().flatten() {
                if g.value(s.as_str()).is_none() {
                    self.push(
                        f,
                        format!("{at}.out_of_bounds"),
                        format!("{:?} is not a value of {:?}", s.as_str(), g.slug.as_str()),
                    );
                }
            }
        }
        self.unit_range(f, &format!("{at}.chance"), m.chance);
        if let Some(md) = &m.modulo
            && (md.x == 0 || md.y == 0)
        {
            self.push(f, format!("{at}.modulo"), "x and y must be at least 1");
        }
    }

    /// A region rule's `region` and `place`.
    fn region_select(&mut self, f: &str, at: &str, layer: Option<&'a LayerDef>, g: &RegionSelect) {
        let rat = format!("{at}.region");
        let (grid, grid_name) =
            self.grid_ref(f, &format!("{rat}.grid"), g.region.grid.as_ref(), layer);
        if g.region.cells == CellMatch::Any {
            self.push(
                f,
                format!("{rat}.cells"),
                "\"*\" matches every cell: every cell would be one region",
            );
        }
        self.cell_test_slugs(
            f,
            &format!("{rat}.cells"),
            &g.region.cells,
            grid,
            &grid_name,
        );
        for (min_key, min, max_key, max) in g.region.bound_pairs() {
            if min > max {
                self.push(
                    f,
                    format!("{rat}.{min_key}"),
                    format!("{min_key} {min} is greater than {max_key} {max}"),
                );
            }
        }
        let pat = format!("{at}.place");
        if let Some(against) = &g.place.against {
            self.cell_test_slugs(f, &format!("{pat}.against"), against, grid, &grid_name);
        }
        if g.place.spacing != Spacing::Even && g.place.align != Default::default() {
            self.push(
                f,
                format!("{pat}.align"),
                "align only applies to spacing \"even\"",
            );
        }
        if g.place.spacing != Spacing::Random && g.place.jitter != 0 {
            self.push(
                f,
                format!("{pat}.jitter"),
                "jitter only applies to spacing \"random\"",
            );
        }
    }

    /// A `write` output: a value of a derived layer on this layer's grid,
    /// and this layer its only writer.
    fn write_output(
        &mut self,
        f: &str,
        at: &str,
        set: &RuleSet,
        layer: Option<&'a LayerDef>,
        w: &Write,
    ) {
        let wat = format!("{at}.write");
        let (grid, _) = self.grid_ref(f, &format!("{wat}.grid"), Some(&w.grid), layer);
        let Some(g) = grid else { return };
        if !g.is_derived() {
            self.push(
                f,
                format!("{wat}.grid"),
                format!(
                    "{:?} is not a derived layer (only a layer with \"derived\": true can be \
                     written)",
                    w.grid.as_str()
                ),
            );
        } else if let Some(writer) = self.p.derived_writer(w.grid.as_str())
            && *writer != set.layer
        {
            self.push(
                f,
                format!("{wat}.grid"),
                format!("{} is already written by layer {writer}", w.grid),
            );
        }
        if g.value(w.value.as_str()).is_none() {
            let msg = if g.group(w.value.as_str()).is_some() {
                format!(
                    "{:?} is a group of {:?}; write needs a value",
                    w.value.as_str(),
                    w.grid.as_str()
                )
            } else {
                format!(
                    "{:?} is not a value of {:?}",
                    w.value.as_str(),
                    w.grid.as_str()
                )
            };
            self.push(f, format!("{wat}.value"), msg);
        }
    }

    /// An `out`: tileset, tiles, stamp and drawing options.
    fn draw_output(&mut self, f: &str, at: &str, set: &RuleSet, out: &RuleOutput, rotates: bool) {
        let oat = format!("{at}.out");
        let draw = out.draw();
        self.unit_range(f, &format!("{oat}.alpha"), draw.alpha);
        let out_tileset = match out {
            RuleOutput::Tiles(t) => {
                self.tileset_ref(f, &format!("{oat}.tileset"), &t.tileset);
                if t.tiles.is_empty() {
                    self.push(f, format!("{oat}.tiles"), "needs at least one alternative");
                }
                for (k, alt) in t.tiles.iter().enumerate() {
                    if alt.weight == 0 {
                        self.push(f, format!("{oat}.tiles[{k}]"), "weight must be at least 1");
                    }
                }
                if t.tile_mode != TileMode::Stamp && t.pivot != [0.0, 0.0] {
                    self.push(
                        f,
                        format!("{oat}.pivot"),
                        "pivot only applies with tile_mode \"stamp\"",
                    );
                }
                Some(&t.tileset)
            }
            RuleOutput::Stamp(s) => match set.stamps.iter().find(|x| x.slug == s.stamp) {
                None => {
                    self.push(
                        f,
                        format!("{oat}.stamp"),
                        format!("unknown stamp {:?}", s.stamp.as_str()),
                    );
                    None
                }
                Some(st) => Some(&st.tileset),
            },
        };
        if let Some(ro) = &draw.random_offset_px
            && (ro.x[0] > ro.x[1] || ro.y[0] > ro.y[1])
        {
            self.push(
                f,
                format!("{oat}.random_offset_px"),
                "each range is [min, max] with min <= max",
            );
        }
        if rotates && let Some(ts) = out_tileset {
            self.rotate_tiles(f, &format!("{at}.symmetry"), &set.layer, ts);
        }
    }

    /// `rotate` turns tiles by 90 degrees, so every tileset the rule can
    /// draw from (its own and each level's substitute) needs square tiles.
    fn rotate_tiles(&mut self, f: &str, at: &str, layer: &Slug, ts: &Slug) {
        let square = |s: &Slug| {
            self.p
                .tileset(s.as_str())
                .is_none_or(|d| d.tile_size[0] == d.tile_size[1])
        };
        let size = |s: &Slug| self.p.tileset(s.as_str()).map_or([0, 0], |d| d.tile_size);
        if !square(ts) {
            let [w, h] = size(ts);
            self.push(
                f,
                at,
                format!(
                    "rotate needs square tiles, but tileset {:?} has {w}x{h} tiles",
                    ts.as_str()
                ),
            );
        }
        let subs: Vec<(Slug, Slug)> = self
            .p
            .levels()
            .filter_map(|l| {
                let to = l.layers.get(layer)?.tilesets.get(ts)?;
                (!square(to)).then(|| (l.slug.clone(), to.clone()))
            })
            .collect();
        for (level, to) in subs {
            let [w, h] = size(&to);
            self.push(
                f,
                at,
                format!(
                    "rotate needs square tiles, but level {:?} draws {:?} from tileset {:?} \
                     ({w}x{h} tiles)",
                    level.as_str(),
                    ts.as_str(),
                    to.as_str()
                ),
            );
        }
    }

    /// `rules/macros.json`: names, legend keys and template rule slugs.
    /// Template rules are checked where they are used, against the
    /// referencing layer (they are layer-agnostic on their own).
    fn macros(&mut self) {
        let Some(m) = &self.p.macros else {
            return;
        };
        let f = "rules/macros.json";
        for (name, rows) in &m.patterns {
            let pat = Pattern {
                grid: None,
                legend: Legend::default(),
                rows: Rows::Inline(rows.clone()),
            };
            self.pattern(f, &format!("patterns.{name}"), &pat, None, None);
        }
        let legend_keys = |v: &mut Self, at: &str, l: &BTreeMap<char, CellMatch>| {
            for glyph in l.keys() {
                if (RESERVED_LEGEND_GLYPHS.contains(glyph) && *glyph != ANCHOR_GLYPH)
                    || !glyph.is_ascii_graphic()
                {
                    v.push(
                        f,
                        format!("{at}.{glyph}"),
                        format!("{glyph:?} can't be a pattern glyph"),
                    );
                }
            }
        };
        for (name, l) in &m.legends {
            legend_keys(self, &format!("legends.{name}"), l);
        }
        for (name, t) in &m.templates {
            let tat = format!("templates.{name}");
            if Macros::is_builtin(name.as_str()) {
                self.push(
                    f,
                    tat.clone(),
                    format!(
                        "{:?} is a built-in template; a project template can't shadow it \
                         (pick another name)",
                        name.as_str()
                    ),
                );
            }
            match &t.legend {
                Legend::Inline(l) => legend_keys(self, &format!("{tat}.legend"), l),
                Legend::Named(n) => {
                    if self.p.named_legend(n.as_str()).is_none() {
                        self.push(
                            f,
                            format!("{tat}.legend"),
                            format!("no legend {:?} in legends", n.as_str()),
                        );
                    }
                }
            }
            self.dups(
                f,
                "template rule slug",
                t.rules
                    .iter()
                    .enumerate()
                    .map(|(i, r)| (format!("{tat}.rules[{i}]"), r.slug.as_str()))
                    .collect::<Vec<_>>(),
            );
            for (i, r) in t.rules.iter().enumerate() {
                let rat = format!("{tat}.rules[{i}]");
                let n = r.require.len();
                let keys = std::iter::once("match".to_string()).chain((0..n).map(|k| {
                    if n == 1 {
                        "require".to_string()
                    } else {
                        format!("require[{k}]")
                    }
                }));
                for (key, p) in keys.zip(r.patterns()) {
                    if let Rows::Named(n) = &p.rows
                        && self.p.named_pattern(n.as_str()).is_none()
                    {
                        self.push(
                            f,
                            format!("{rat}.{key}.rows"),
                            format!("no pattern {:?} in patterns", n.as_str()),
                        );
                    }
                    match &p.legend {
                        Legend::Named(n) => {
                            if self.p.named_legend(n.as_str()).is_none() {
                                self.push(
                                    f,
                                    format!("{rat}.{key}.legend"),
                                    format!("no legend {:?} in legends", n.as_str()),
                                );
                            }
                        }
                        Legend::Inline(l) => {
                            legend_keys(self, &format!("{rat}.{key}.legend"), l);
                            let rows: Vec<String> = match &p.rows {
                                Rows::Inline(r) => r.clone(),
                                Rows::Named(n) => self
                                    .p
                                    .named_pattern(n.as_str())
                                    .cloned()
                                    .unwrap_or_default(),
                            };
                            if l.contains_key(&ANCHOR_GLYPH)
                                && !rows.iter().any(|r| r.contains(ANCHOR_GLYPH))
                            {
                                self.push(
                                    f,
                                    format!("{rat}.{key}.legend.@"),
                                    "'@' is only a legend key in a pattern whose rows contain @",
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // ---- worlds ----

    fn worlds(&mut self) {
        let p = self.p;
        // The display order in project.json.
        for (i, s) in p.world_order.iter().enumerate() {
            let path = format!("worlds[{i}]");
            if p.world(s.as_str()).is_none() {
                self.push("project.json", path, format!("no world {s:?}"));
            } else if p.world_order[..i].contains(s) {
                self.push("project.json", path, format!("world {s:?} is listed twice"));
            }
        }
        if !p.world_order.is_empty() {
            for w in p.worlds.iter().filter(|w| !p.world_order.contains(&w.slug)) {
                self.push(
                    "project.json",
                    "worlds",
                    format!(
                        "world {:?} isn't listed; it is shown after the listed worlds",
                        w.slug.as_str()
                    ),
                );
            }
        }
        self.dups(
            "worlds",
            "world",
            p.worlds
                .iter()
                .map(|w| (w.slug.to_string(), w.slug.as_str())),
        );
        let levels: Vec<_> = p
            .worlds
            .iter()
            .flat_map(|w| {
                w.levels
                    .iter()
                    .map(move |l| (format!("{}/levels/{}", w.slug, l.slug), l))
            })
            .collect();
        self.dups(
            "worlds",
            "level slug",
            levels
                .iter()
                .map(|(at, l)| (at.clone(), l.slug.as_str()))
                .collect::<Vec<_>>(),
        );
        self.dups(
            "worlds",
            "level id",
            levels
                .iter()
                .map(|(at, l)| (at.clone(), l.id.as_str()))
                .collect::<Vec<_>>(),
        );
        // Ids are unique across the project; each repeat is reported in the
        // file that holds it, at the instance (as every other problem is).
        let ents: Vec<_> = p
            .worlds
            .iter()
            .flat_map(|w| {
                w.levels.iter().flat_map(move |l| {
                    l.entities.iter().flat_map(move |el| {
                        el.entities.iter().map(move |e| {
                            (
                                format!("worlds/{}/levels/{}/entities.json", w.slug, l.slug),
                                format!("layers[{}].entities[{}].id", el.layer, e.id),
                                e.id.as_str(),
                            )
                        })
                    })
                })
            })
            .collect();
        self.dups_in("entity id", ents);
        let notes: Vec<_> = p
            .worlds
            .iter()
            .flat_map(|w| {
                w.notes
                    .iter()
                    .map(move |n| {
                        (
                            format!("worlds/{}/world.json", w.slug),
                            format!("notes[{}].id", n.id),
                            n.id.as_str(),
                        )
                    })
                    .chain(w.levels.iter().flat_map(move |l| {
                        l.notes.iter().map(move |n| {
                            (
                                format!("worlds/{}/levels/{}/notes.json", w.slug, l.slug),
                                format!("notes[{}].id", n.id),
                                n.id.as_str(),
                            )
                        })
                    }))
            })
            .collect();
        self.dups_in("note id", notes);

        let mut world_counts: BTreeMap<&Slug, u32> = BTreeMap::new();
        for e in p.levels().flat_map(|l| l.all_entities()) {
            *world_counts.entry(&e.ty).or_default() += 1;
        }
        for (ty, n) in world_counts {
            if let Some(def) = p.entity_def(ty.as_str())
                && let Some(limit) = &def.limit
                && limit.scope == LimitScope::World
                && n > limit.max
            {
                self.push(
                    &format!("entities/{ty}.json"),
                    "limit",
                    format!("{n} instances in the project, limit is {}", limit.max),
                );
            }
        }

        for w in &p.worlds {
            self.world(w);
        }
    }

    fn world(&mut self, w: &'a World) {
        let f = format!("worlds/{}/world.json", w.slug);
        self.dups(
            &f,
            "guide group",
            w.guide_groups
                .iter()
                .enumerate()
                .map(|(i, g)| (format!("guide_groups[{i}].slug"), g.slug.as_str())),
        );
        for n in &w.notes {
            let at = format!("notes[{}]", n.id);
            self.annotation(&f, &at, n, None);
            if let Shape::Guide { group: Some(g), .. } = &n.shape
                && w.guide_group(g.as_str()).is_none()
            {
                self.push(
                    &f,
                    format!("{at}.group"),
                    format!(
                        "guide group {:?} is not defined in world.json guide_groups",
                        g.as_str()
                    ),
                );
            }
        }
        for l in &w.levels {
            let dir = format!("worlds/{}/levels/{}", w.slug, l.slug);
            self.level(&dir, l);
            self.level_regions(&format!("{dir}/level.json"), w, l);
        }
        for n in &w.notes {
            self.connector_ends(&f, &format!("notes[{}]", n.id), n, w);
        }
        for l in &w.levels {
            let f = format!("worlds/{}/levels/{}/notes.json", w.slug, l.slug);
            for n in &l.notes {
                self.connector_ends(&f, &format!("notes[{}]", n.id), n, w);
            }
        }
    }

    /// A connector's `from` and `to` must name a level or an entity of the
    /// connector's own world.
    fn connector_ends(&mut self, f: &str, at: &str, n: &Annotation, w: &World) {
        let Shape::Connector { from, to, .. } = &n.shape else {
            return;
        };
        for (key, end) in [("from", from), ("to", to)] {
            let what = match end {
                Endpoint::Level(s) => format!("level {:?}", s.as_str()),
                Endpoint::Entity(id) => format!("entity {id}"),
            };
            match self.p.endpoint_anchor(end) {
                None => self.push(
                    f,
                    format!("{at}.{key}"),
                    format!("{what} does not exist: the connector's end dangles"),
                ),
                Some((_, ew, _)) if ew.slug != w.slug => self.push(
                    f,
                    format!("{at}.{key}"),
                    format!(
                        "{what} is in world {:?}, not the connector's world {:?}",
                        ew.slug.as_str(),
                        w.slug.as_str()
                    ),
                ),
                Some(_) => {}
            }
        }
    }

    /// A level tagged `T` must sit (its center) inside a world region
    /// tagged `T`, when the world has any region tagged `T`.
    fn level_regions(&mut self, f: &str, w: &World, l: &Level) {
        let g = self.p.grid_size as i64;
        let center = [
            l.world_px[0] + l.size[0] as i64 * g / 2,
            l.world_px[1] + l.size[1] as i64 * g / 2,
        ];
        for (i, tag) in l.tags.iter().enumerate() {
            let mut regions = w
                .notes
                .iter()
                .filter(|n| matches!(n.shape, Shape::Region { .. }) && n.tags.contains(tag))
                .peekable();
            if regions.peek().is_none() {
                continue;
            }
            if !regions.any(|n| region_contains(&n.shape, center)) {
                self.push(
                    f,
                    format!("tags[{i}]"),
                    format!(
                        "level tagged {tag:?} sits outside every region tagged {tag:?} \
                         (its center is [{}, {}])",
                        center[0], center[1]
                    ),
                );
            }
        }
    }

    /// A stored grid of `actual` cells where the level needs `need` cells of
    /// `g` px.
    fn grid_size(&mut self, f: &str, need: [u32; 2], g: u32, actual: [u32; 2]) {
        let [w, h] = need;
        let [gw, gh] = actual;
        let cell = if g == self.p.grid_size {
            String::new()
        } else {
            format!(" of {g} px")
        };
        if gh != h {
            self.push(
                f,
                "rows",
                format!("{gh} rows, level is {h} cells{cell} tall"),
            );
        }
        if gw != w {
            self.push(
                f,
                "rows",
                format!("rows are {gw} glyphs wide, level is {w} cells{cell} wide"),
            );
        }
    }

    /// Empty tags and tags given twice in one list.
    fn tags(&mut self, f: &str, at: &str, tags: &[String]) {
        let mut seen = BTreeSet::new();
        for (i, t) in tags.iter().enumerate() {
            if t.is_empty() {
                self.push(f, format!("{at}tags[{i}]"), "empty tag");
            } else if !seen.insert(t.as_str()) {
                self.push(f, format!("{at}tags[{i}]"), format!("duplicate tag {t:?}"));
            }
        }
    }

    fn level(&mut self, dir: &str, l: &'a Level) {
        let p = self.p;
        let f = format!("{dir}/level.json");
        if l.size[0] == 0 || l.size[1] == 0 {
            self.push(&f, "size", "must be at least 1x1");
        }
        self.tags(&f, "", &l.tags);
        self.inherited_defaults(
            &f,
            "fields",
            &p.level_fields,
            &l.fields,
            FieldScope {
                level: Some(l),
                cells: l.size,
            },
        );
        for (slug, v) in &l.fields {
            match p.level_fields.iter().find(|d| &d.slug == slug) {
                Some(d) => self.field_value(
                    &f,
                    &format!("fields.{slug}"),
                    d,
                    v,
                    FieldScope {
                        level: Some(l),
                        cells: l.size,
                    },
                ),
                None => self.push(
                    &f,
                    format!("fields.{slug}"),
                    "no such level field in project.json",
                ),
            }
        }
        for (slug, st) in &l.layers {
            let at = format!("layers.{slug}");
            match p.layer(slug.as_str()) {
                None => {
                    self.push(&f, &at, format!("unknown layer {:?}", slug.as_str()));
                    continue;
                }
                Some(def) if !def.is_auto() => {
                    self.push(
                        &f,
                        &at,
                        format!(
                            "{:?} is a {} layer; only auto layers have per-level rule state",
                            slug.as_str(),
                            def.kind.name()
                        ),
                    );
                    continue;
                }
                Some(_) => {}
            }
            let groups = p
                .rules_for(slug.as_str())
                .map_or(&[][..], |r| &r.groups[..]);
            for (i, g) in st.optional_groups.iter().enumerate() {
                let msg = match groups.iter().find(|x| &x.slug == g) {
                    Some(x) if x.mode == GroupMode::Optional => continue,
                    Some(x) => format!(
                        "rule group {:?} is {}, not optional; only optional groups can be \
                         switched on per level",
                        g.as_str(),
                        if x.mode == GroupMode::Off {
                            "off"
                        } else {
                            "always on"
                        }
                    ),
                    None => format!("no rule group {:?} in this layer", g.as_str()),
                };
                self.push(&f, format!("{at}.optional_groups[{i}]"), msg);
            }
            for (from, to) in &st.tilesets {
                self.tileset_ref(&f, &format!("{at}.tilesets.{from}"), from);
                self.tileset_ref(&f, &format!("{at}.tilesets.{from}"), to);
            }
        }

        let layer_of =
            |this: &mut Self, file: &str, slug: &Slug, kind: &str| -> Option<&'a LayerDef> {
                match p.layer(slug.as_str()) {
                    Some(d) if d.kind.name() == kind => Some(d),
                    Some(d) => {
                        this.push(
                            file,
                            "layer",
                            format!(
                                "{:?} is a {} layer, expected {kind}",
                                slug.as_str(),
                                d.kind.name()
                            ),
                        );
                        None
                    }
                    None => {
                        this.push(file, "layer", format!("unknown layer {:?}", slug.as_str()));
                        None
                    }
                }
            };

        self.dups(
            dir,
            "int_grid layer",
            l.int_grids
                .iter()
                .map(|g| (format!("{}.json", g.layer), g.layer.as_str()))
                .chain(
                    l.overrides
                        .iter()
                        .map(|o| (format!("{}.json", o.layer), o.layer.as_str())),
                )
                .collect::<Vec<_>>(),
        );
        for o in &l.overrides {
            let f = format!("{dir}/{}.json", o.layer);
            let Some(def) = layer_of(self, &f, &o.layer, "int_grid") else {
                continue;
            };
            if !def.is_derived() {
                self.push(
                    &f,
                    "overrides",
                    format!(
                        "{:?} isn't derived, so its file can't hold overrides",
                        o.layer.as_str()
                    ),
                );
            }
            self.grid_size(&f, p.layer_cells(def, l), p.layer_grid(def), o.size());
            let n = def.values().len() as u16;
            let [gw, _] = o.size();
            if let Some(i) = o.values().iter().position(|&c| c > n) {
                let (x, y) = (i as u32 % gw.max(1), i as u32 / gw.max(1));
                self.push(
                    &f,
                    format!("rows[{y}]"),
                    format!(
                        "cell [{x}, {y}] holds value #{}, but the layer has {n} values",
                        o.values()[i]
                    ),
                );
            }
        }
        for g in &l.int_grids {
            let f = format!("{dir}/{}.json", g.layer);
            let Some(def) = layer_of(self, &f, &g.layer, "int_grid") else {
                continue;
            };
            if def.is_derived() {
                self.push(
                    &f,
                    "overrides",
                    format!(
                        "{:?} is derived: its file holds overrides and says \"overrides\": true",
                        g.layer.as_str()
                    ),
                );
            }
            let [w, h] = p.layer_cells(def, l);
            let [gw, gh] = g.size();
            let cell = if p.layer_grid(def) == p.grid_size {
                String::new()
            } else {
                format!(" of {} px", p.layer_grid(def))
            };
            if gh != h {
                self.push(
                    &f,
                    "rows",
                    format!("{gh} rows, level is {h} cells{cell} tall"),
                );
            }
            if gw != w {
                self.push(
                    &f,
                    "rows",
                    format!("rows are {gw} glyphs wide, level is {w} cells{cell} wide"),
                );
            }
            let n = def.values().len() as u16;
            if let Some(i) = g.cells().iter().position(|&c| c > n) {
                let x = i as u32 % gw.max(1);
                self.push(
                    &f,
                    format!("rows[{}]", i as u32 / gw.max(1)),
                    format!(
                        "cell [{x}, {}] holds value #{}, but the layer has {n} values",
                        i as u32 / gw.max(1),
                        g.cells()[i]
                    ),
                );
            }
        }

        self.dups(
            dir,
            "tiles layer",
            l.tiles
                .iter()
                .map(|t| (format!("{}.json", t.layer), t.layer.as_str()))
                .collect::<Vec<_>>(),
        );
        for t in &l.tiles {
            let f = format!("{dir}/{}.json", t.layer);
            let def = layer_of(self, &f, &t.layer, "tiles");
            let [w, h] = def.map_or(l.size, |d| p.layer_cells(d, l));
            for tp in &t.tiles {
                let at = format!("tiles[{},{}]", tp.cell[0], tp.cell[1]);
                if tp.cell[0] >= w || tp.cell[1] >= h {
                    self.push(
                        &f,
                        &at,
                        format!(
                            "[{}, {}] is outside the level ({w}x{h})",
                            tp.cell[0], tp.cell[1]
                        ),
                    );
                }
                self.tileset_ref(&f, &format!("{at}.tileset"), &tp.tileset);
                self.unit_range(&f, &format!("{at}.alpha"), tp.alpha);
            }
        }

        let f = format!("{dir}/entities.json");
        self.dups(
            &f,
            "entities layer",
            l.entities
                .iter()
                .map(|e| (format!("layers[{}]", e.layer), e.layer.as_str()))
                .collect::<Vec<_>>(),
        );
        let mut level_counts: BTreeMap<&Slug, u32> = BTreeMap::new();
        for el in &l.entities {
            let lat = format!("layers[{}]", el.layer);
            let cells = match p.layer(el.layer.as_str()) {
                Some(d) if d.is_entities() => p.layer_cells(d, l),
                Some(_) => {
                    self.push(
                        &f,
                        format!("{lat}.layer"),
                        format!("{:?} is not an entities layer", el.layer.as_str()),
                    );
                    l.size
                }
                None => {
                    self.push(
                        &f,
                        format!("{lat}.layer"),
                        format!("unknown layer {:?}", el.layer.as_str()),
                    );
                    l.size
                }
            };
            let mut layer_counts: BTreeMap<&Slug, u32> = BTreeMap::new();
            for e in &el.entities {
                let at = format!("{lat}.entities[{}]", e.id);
                *layer_counts.entry(&e.ty).or_default() += 1;
                *level_counts.entry(&e.ty).or_default() += 1;
                match p.entity_def(e.ty.as_str()) {
                    Some(def) => self.entity(&f, &at, l, cells, def, e),
                    None => self.push(
                        &f,
                        format!("{at}.type"),
                        format!("unknown entity type {:?}", e.ty.as_str()),
                    ),
                }
            }
            self.limits(&f, &lat, &layer_counts, LimitScope::Layer);
        }
        self.limits(&f, "", &level_counts, LimitScope::Level);

        let f = format!("{dir}/notes.json");
        for n in &l.notes {
            self.annotation(&f, &format!("notes[{}]", n.id), n, Some(l));
        }
        if l.has_void() {
            self.level_shape(dir, l);
        }
    }

    /// A non-rectangular level: its void rects fit the box and leave one
    /// 4-connected piece, and no content sits in the void.
    fn level_shape(&mut self, dir: &str, l: &'a Level) {
        let p = self.p;
        let f = format!("{dir}/level.json");
        let [w, h] = l.size;
        for (i, r) in l.void.iter().enumerate() {
            let shown = [r.x, r.y, r.w, r.h];
            if r.is_empty() {
                self.push(&f, format!("void[{i}]"), format!("{shown:?} is empty"));
            } else if r.x as u64 + r.w as u64 > w as u64 || r.y as u64 + r.h as u64 > h as u64 {
                self.push(
                    &f,
                    format!("void[{i}]"),
                    format!("{shown:?} reaches outside the level ({w}x{h})"),
                );
            }
        }
        let pieces = l.pieces();
        match pieces.len() {
            0 => self.push(
                &f,
                "void",
                "the void covers the whole level; no cell is left",
            ),
            1 => {}
            n => {
                let firsts: Vec<String> = pieces
                    .iter()
                    .map(|c| format!("[{}, {}]", c[0] as u32 % w, c[0] as u32 / w))
                    .collect();
                self.push(
                    &f,
                    "void",
                    format!(
                        "the void splits the level into {n} pieces that don't touch (starting \
                         at {}); a level is one 4-connected room",
                        firsts.join(", ")
                    ),
                );
            }
        }
        let planes = l
            .int_grids
            .iter()
            .map(|g| (&g.layer, g.size(), g.cells().to_vec()))
            .chain(l.overrides.iter().map(|o| {
                (
                    &o.layer,
                    o.size(),
                    o.cells()
                        .map(|c| c != Override::None)
                        .map(u16::from)
                        .collect(),
                )
            }));
        for (layer, size, codes) in planes {
            let Some(def) = p.layer(layer.as_str()) else {
                continue;
            };
            if size != p.layer_cells(def, l) {
                continue;
            }
            let Some(mask) = p.layer_void(def, l) else {
                continue;
            };
            let mut hits = codes
                .iter()
                .enumerate()
                .filter(|&(i, &c)| c != 0 && mask[i])
                .map(|(i, _)| i as u32);
            if let Some(first) = hits.next() {
                let n = 1 + hits.count();
                let (x, y) = (first % size[0], first / size[0]);
                self.push(
                    &format!("{dir}/{layer}.json"),
                    format!("rows[{y}]"),
                    format!(
                        "{n} cell(s) in the level's void hold content (first [{x}, {y}]); clear \
                         them or change the void"
                    ),
                );
            }
        }
        for t in &l.tiles {
            let Some(def) = p.layer(t.layer.as_str()) else {
                continue;
            };
            for tp in &t.tiles {
                if p.layer_cell_void(def, l, tp.cell[0] as i64, tp.cell[1] as i64) {
                    self.push(
                        &format!("{dir}/{}.json", t.layer),
                        format!("tiles[{},{}]", tp.cell[0], tp.cell[1]),
                        format!("[{}, {}] is in the level's void", tp.cell[0], tp.cell[1]),
                    );
                }
            }
        }
        let f = format!("{dir}/entities.json");
        for el in &l.entities {
            let Some(def) = p.layer(el.layer.as_str()) else {
                continue;
            };
            for e in &el.entities {
                if p.layer_cell_void(def, l, e.cell[0] as i64, e.cell[1] as i64) {
                    self.push(
                        &f,
                        format!("layers[{}].entities[{}].cell", el.layer, e.id),
                        format!("[{}, {}] is in the level's void", e.cell[0], e.cell[1]),
                    );
                }
            }
        }
    }

    fn limits(&mut self, f: &str, at: &str, counts: &BTreeMap<&Slug, u32>, scope: LimitScope) {
        for (ty, &n) in counts {
            if let Some(def) = self.p.entity_def(ty.as_str())
                && let Some(limit) = &def.limit
                && limit.scope == scope
                && n > limit.max
            {
                self.push(
                    f,
                    at,
                    format!(
                        "{n} {:?} entities, limit is {} per {scope:?}",
                        ty.as_str(),
                        limit.max
                    ),
                );
            }
        }
    }

    fn entity(
        &mut self,
        f: &str,
        at: &str,
        l: &'a Level,
        cells: [u32; 2],
        def: &EntityDef,
        e: &EntityInstance,
    ) {
        let [x, y] = e.cell;
        let [w, h] = cells;
        if x < 0 || y < 0 || x as i64 >= w as i64 || y as i64 >= h as i64 {
            self.push(
                f,
                format!("{at}.cell"),
                format!("[{x}, {y}] is outside the level ({w}x{h})"),
            );
        }
        if let Some(size) = e.size {
            let mut axes = size.iter().zip(def.size).zip(def.resizable);
            if axes.any(|((&s, d), r)| s != d && !r) {
                self.push(
                    f,
                    format!("{at}.size"),
                    format!("{:?} is not resizable on that axis", def.slug.as_str()),
                );
            }
            if size.contains(&0) {
                self.push(f, format!("{at}.size"), "must be at least 1x1");
            }
        }
        for (slug, v) in &e.fields {
            match def.field(slug.as_str()) {
                Some(d) => self.field_value(
                    f,
                    &format!("{at}.fields.{slug}"),
                    d,
                    v,
                    FieldScope {
                        level: Some(l),
                        cells,
                    },
                ),
                None => self.push(
                    f,
                    format!("{at}.fields.{slug}"),
                    format!("{:?} has no field {:?}", def.slug.as_str(), slug.as_str()),
                ),
            }
        }
        self.inherited_defaults(
            f,
            &format!("{at}.fields"),
            &def.fields,
            &e.fields,
            FieldScope {
                level: Some(l),
                cells,
            },
        );
    }

    /// Fields an instance leaves out take their definition's default. Its
    /// shape was checked once on the definition, but a `point` default's
    /// bounds and an `entity_ref` default's target depend on the level, so
    /// check those again in the scope of each instance that inherits them.
    fn inherited_defaults(
        &mut self,
        f: &str,
        at: &str,
        defs: &[FieldDef],
        set: &BTreeMap<Slug, Value>,
        scope: FieldScope<'a>,
    ) {
        let unscoped = FieldScope {
            level: None,
            cells: [0, 0],
        };
        for d in defs {
            if set.contains_key(&d.slug)
                || !matches!(d.ty, FieldType::Point | FieldType::EntityRef { .. })
            {
                continue;
            }
            let Some(v) = &d.default else {
                continue;
            };
            let items: Vec<(String, &Value)> = match (d.array, v.as_array()) {
                (true, Some(a)) => a
                    .iter()
                    .enumerate()
                    .map(|(i, x)| (format!("{at}.{}[{i}]", d.slug), x))
                    .collect(),
                (true, None) => continue, // reported on the definition
                (false, _) => vec![(format!("{at}.{}", d.slug), v)],
            };
            for (path, item) in items {
                // Nulls and shape errors were reported on the definition.
                if item.is_null() || self.scalar_error(d, item, unscoped).is_some() {
                    continue;
                }
                if let Some(msg) = self.scalar_error(d, item, scope) {
                    self.push(f, path, format!("inherited default: {msg}"));
                }
            }
        }
    }

    /// One annotation; `level` is its level for level-scope ones.
    fn annotation(&mut self, f: &str, at: &str, n: &Annotation, level: Option<&Level>) {
        let need = match &n.shape {
            Shape::Note { .. } | Shape::Pin { .. } => None,
            Shape::Path { points } if points.len() < 2 => Some("needs at least 2 points"),
            Shape::Guide { points, .. } if points.len() < 2 => Some("needs at least 2 points"),
            Shape::Region { points, rect } => match (rect, points.len()) {
                (Some(_), 0) | (None, 3..) => None,
                (Some(_), _) => Some("takes rect or points, not both"),
                (None, _) => Some("needs either rect or at least 3 points"),
            },
            _ => None,
        };
        if let Some(need) = need {
            self.push(f, at, format!("a {} {need}", n.shape.name()));
        }
        let guide = matches!(n.shape, Shape::Guide { .. });
        if guide && level.is_some() {
            self.push(
                f,
                format!("{at}.kind"),
                "guides are world scope only (put them in world.json)",
            );
        }
        if guide && n.export {
            self.push(
                f,
                format!("{at}.export"),
                "guides are never exported (export is ignored)",
            );
        }
        if let Shape::Region {
            rect: Some([_, _, w, h]),
            ..
        } = n.shape
            && (w <= 0 || h <= 0)
        {
            self.push(f, format!("{at}.rect"), "needs a positive width and height");
        }
        if let (Some(l), Shape::Note { px } | Shape::Pin { px, .. }) = (level, &n.shape) {
            let g = self.p.grid_size as i64;
            let (w, h) = (l.size[0] as i64 * g, l.size[1] as i64 * g);
            if px[0] < 0 || px[1] < 0 || px[0] >= w || px[1] >= h {
                self.push(
                    f,
                    format!("{at}.px"),
                    format!("outside its level (level is {w}x{h} px)"),
                );
            }
        }
        self.tags(f, &format!("{at}."), &n.tags);
        for (i, link) in n.links.iter().enumerate() {
            let p = self.p;
            let target = match parse_link(link) {
                Link::Level(s) => Some(p.level(s).is_some()),
                Link::Entity(s) => Some(Id::new(s).is_ok_and(|id| self.entities.contains_key(&id))),
                Link::Note(s) => p
                    .worlds
                    .iter()
                    .flat_map(|w| w.notes.iter())
                    .chain(p.levels().flat_map(|l| l.notes.iter()))
                    .find(|x| x.id.as_str() == s)
                    .map(|x| {
                        if n.export
                            && !guide
                            && (!x.export || matches!(x.shape, Shape::Guide { .. }))
                        {
                            self.out.push(Problem {
                                file: f.into(),
                                path: format!("{at}.links[{i}]"),
                                message: "links a note that is not exported: the game can't \
                                          resolve it"
                                    .into(),
                            });
                        }
                        true
                    })
                    .or(Some(false)),
                Link::Url(_) => None,
            };
            if target == Some(false) {
                self.push(
                    f,
                    format!("{at}.links[{i}]"),
                    format!("{link:?} does not resolve"),
                );
            }
        }
    }
}

/// Whether a region holds a point: a rect half-open (`[x, x + w)`), a
/// polygon by the even-odd rule with points on an edge inside. Other
/// shapes hold nothing.
fn region_contains(shape: &Shape, p: [i64; 2]) -> bool {
    match shape {
        Shape::Region {
            rect: Some([x, y, w, h]),
            ..
        } => p[0] >= *x && p[0] < x + w && p[1] >= *y && p[1] < y + h,
        Shape::Region { points, rect: None } if points.len() >= 3 => {
            let (px, py) = (p[0] as i128, p[1] as i128);
            let mut inside = false;
            for (k, a) in points.iter().enumerate() {
                let b = points[(k + 1) % points.len()];
                let (ax, ay, bx, by) = (a[0] as i128, a[1] as i128, b[0] as i128, b[1] as i128);
                // On the edge: collinear and within its box.
                let cross = (bx - ax) * (py - ay) - (by - ay) * (px - ax);
                if cross == 0
                    && px >= ax.min(bx)
                    && px <= ax.max(bx)
                    && py >= ay.min(by)
                    && py <= ay.max(by)
                {
                    return true;
                }
                if (ay > py) != (by > py) {
                    // p is left of the crossing x: (px - ax) / (bx - ax) < (py - ay) / (by - ay).
                    let lhs = (px - ax) * (by - ay);
                    let rhs = (py - ay) * (bx - ax);
                    if (by > ay && lhs < rhs) || (by < ay && lhs > rhs) {
                        inside = !inside;
                    }
                }
            }
            inside
        }
        _ => false,
    }
}

// ---- reference images ----

impl<'a> Validator<'a> {
    /// `references` of every `world.json` and `level.json`: the image is a
    /// path inside the project that exists (when the project was loaded
    /// from a directory), scale above 0, opacity in 0..1, ids unique across
    /// the project.
    fn references(&mut self) {
        let p = self.p;
        let mut all = Vec::new();
        for w in &p.worlds {
            let f = format!("worlds/{}/world.json", w.slug);
            all.extend(w.references.iter().map(|r| (f.clone(), r)));
            for l in &w.levels {
                let f = format!("worlds/{}/levels/{}/level.json", w.slug, l.slug);
                all.extend(l.references.iter().map(|r| (f.clone(), r)));
            }
        }
        let root = p.files.root();
        for (f, r) in &all {
            let at = format!("references[{}]", r.id);
            let path = std::path::Path::new(&r.image);
            if r.image.is_empty() {
                self.push(f, format!("{at}.image"), "must not be empty");
            } else if path.is_absolute()
                || r.image.contains('\\')
                || path
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                self.push(
                    f,
                    format!("{at}.image"),
                    format!(
                        "{:?} must be a /-separated path inside the project, like a tileset's image",
                        r.image
                    ),
                );
            } else if let Some(root) = root
                && !root.join(path).is_file()
            {
                self.push(
                    f,
                    format!("{at}.image"),
                    format!("image file {:?} doesn't exist", r.image),
                );
            }
            if !(r.scale.is_finite() && r.scale > 0.0) {
                self.push(
                    f,
                    format!("{at}.scale"),
                    format!("{} must be above 0", r.scale),
                );
            }
            self.unit_range(f, &format!("{at}.opacity"), r.opacity);
        }
        self.dups_in(
            "reference image id",
            all.iter()
                .map(|(f, r)| (f.clone(), format!("references[{}].id", r.id), r.id.as_str())),
        );
    }
}
