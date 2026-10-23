//! Tilesets, enums, entity definitions and field definitions/values.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Value, json};

use super::names::{Namer, snake_case};
use super::{Ctx, EntityInfo, EnumInfo, FieldInfo, Result, TilesetInfo, json as j};
use crate::id::Slug;
use crate::model::{
    Color, EntityDef, EnumDef, EnumSource, EnumValue, FieldDef, FieldDisplay, FieldType, Limit,
    LimitBehavior, LimitScope, TileRef, TilesetDef,
};

impl Ctx {
    pub(super) fn tilesets(&mut self, src: &[j::Tileset]) -> Result<Vec<TilesetDef>> {
        let mut names = Namer::default();
        let mut out = Vec::new();
        for t in src {
            let what = format!("tileset {:?}", t.identifier);
            let Some(rel) = &t.rel_path else {
                self.skipped_tilesets.insert(t.uid);
                match &t.embed_atlas {
                    Some(atlas) => self.warn(format!(
                        "{what}: LDtk's built-in {atlas} atlas has no image file; tileset and \
                         every icon using it dropped"
                    )),
                    None => self.warn(format!("{what}: no image; tileset dropped")),
                }
                continue;
            };
            let tgs = t.tile_grid_size.max(1);
            let slug = names.claim(&snake_case(&t.identifier, "tileset"));
            self.renamed(&t.identifier, &slug);
            let def = TilesetDef {
                slug: slug.clone(),
                image: self.image_path(rel),
                tile_size: [tgs, tgs],
                spacing: t.spacing,
                padding: t.padding,
            };
            let c_wid = def.columns(t.px_wid);
            if let Some(saved) = t.c_wid
                && saved != c_wid
            {
                self.warn(format!(
                    "{what}: computed {c_wid} tiles per row but the file says {saved}; tile ids \
                     use {c_wid}"
                ));
            }
            if !t.tags.is_empty() {
                self.warn(format!(
                    "{what}: tags {:?} dropped (no tileset tags)",
                    t.tags
                ));
            }
            if !t.enum_tags.is_empty() {
                self.warn(format!(
                    "{what}: per-tile enum tags dropped (no tile tags in Worlds yet)"
                ));
            }
            if !t.custom_data.is_empty() {
                self.warn(format!("{what}: per-tile custom data dropped"));
            }
            if let Some(opaque) = t
                .cached_pixel_data
                .as_ref()
                .and_then(|c| c.opaque_tiles.as_ref())
                && c_wid > 0
            {
                let set: BTreeSet<[u32; 2]> = opaque
                    .bytes()
                    .enumerate()
                    .filter(|(_, b)| *b == b'1')
                    .map(|(i, _)| [i as u32 % c_wid, i as u32 / c_wid])
                    .collect();
                self.reference.opaque_tiles.insert(slug.clone(), set);
            }
            self.tilesets.insert(
                t.uid,
                TilesetInfo {
                    slug,
                    c_wid,
                    def: def.clone(),
                },
            );
            out.push(def);
        }
        Ok(out)
    }

    /// A tile ref for an LDtk tile rectangle (its top-left tile), if the
    /// tileset was imported. The rect is in atlas pixels, so padding and
    /// spacing come off before dividing by the tile step.
    pub(super) fn tile_rect(&mut self, r: &j::TileRect, what: &str) -> Option<TileRef> {
        let Some(ts) = self.tilesets.get(&r.tileset_uid) else {
            // Skipped tilesets were already reported, icons included.
            if !self.skipped_tilesets.contains(&r.tileset_uid) {
                self.warn(format!("{what}: its tileset wasn't imported; tile dropped"));
            }
            return None;
        };
        Some(TileRef {
            tileset: ts.slug.clone(),
            tile: ts.def.tile_at_px([r.x, r.y]),
        })
    }

    pub(super) fn enums(&mut self, defs: &j::Defs) -> Result<Vec<EnumDef>> {
        let mut names = Namer::default();
        let mut out = Vec::new();
        for (e, external) in defs
            .enums
            .iter()
            .map(|e| (e, false))
            .chain(defs.external_enums.iter().map(|e| (e, true)))
        {
            let what = format!("enum {:?}", e.identifier);
            let slug = names.claim(&snake_case(&e.identifier, "enum"));
            self.renamed(&e.identifier, &slug);
            if external {
                self.warn(format!(
                    "{what}: imported from {:?} as a plain enum; it no longer syncs with that file",
                    e.external_rel_path.as_deref().unwrap_or("?")
                ));
            }
            if !e.tags.is_empty() {
                self.warn(format!("{what}: tags {:?} dropped", e.tags));
            }
            let mut value_names = Namer::default();
            let mut map = HashMap::new();
            let mut values = Vec::new();
            for v in &e.values {
                let vslug = value_names.claim(&snake_case(&v.id, "v"));
                self.renamed(&v.id, &vslug);
                map.insert(v.id.clone(), vslug.clone());
                let icon = match (&v.tile_rect, e.icon_tileset_uid) {
                    (Some(r), _) => self.tile_rect(r, &format!("{what} value {:?}", v.id)),
                    _ => None,
                };
                values.push(EnumValue {
                    name: (vslug.as_str() != v.id).then(|| v.id.clone()),
                    slug: vslug,
                    // LDtk stores 0 for "no color".
                    color: (v.color != 0).then(|| int_color(v.color)),
                    icon,
                });
            }
            self.enums.insert(
                e.uid,
                EnumInfo {
                    slug: slug.clone(),
                    first: values.first().map(|v: &EnumValue| v.slug.clone()),
                    values: map,
                },
            );
            out.push(EnumDef {
                name: (slug.as_str() != e.identifier).then(|| e.identifier.clone()),
                slug,
                values,
            });
        }
        Ok(out)
    }

    pub(super) fn claim_entity_slugs(&mut self, src: &[j::EntityDef]) {
        let mut names = Namer::default();
        for e in src {
            let slug = names.claim(&snake_case(&e.identifier, "entity"));
            self.renamed(&e.identifier, &slug);
            let g = self.grid;
            let size = [e.width.div_ceil(g).max(1), e.height.div_ceil(g).max(1)];
            self.entities.insert(
                e.uid,
                EntityInfo {
                    slug,
                    ldtk_name: e.identifier.clone(),
                    px_size: [e.width, e.height],
                    size,
                    pivot: [e.pivot_x, e.pivot_y],
                    fields: HashMap::new(),
                },
            );
        }
    }

    pub(super) fn level_fields(&mut self, src: &[j::FieldDef]) -> Vec<FieldDef> {
        let mut names = Namer::default();
        let mut out = Vec::new();
        for f in src {
            let info = self.field_def(f, None, &mut names, "level field");
            out.push(info.def.clone());
            self.level_fields.insert(f.uid, info);
        }
        out
    }

    pub(super) fn entity_defs(&mut self, src: &[j::EntityDef]) -> Vec<EntityDef> {
        let mut out = Vec::new();
        for e in src {
            let what = format!("entity {:?}", e.identifier);
            let info = self.entities[&e.uid].clone();
            let g = self.grid;
            if !e.width.is_multiple_of(g) || !e.height.is_multiple_of(g) {
                self.warn(format!(
                    "{what}: {}x{} px is not a whole number of {g} px cells; size rounded up to \
                     {}x{} cells",
                    e.width, e.height, info.size[0], info.size[1]
                ));
            }
            let mut dropped = Vec::new();
            if e.min_width.is_some()
                || e.max_width.is_some()
                || e.min_height.is_some()
                || e.max_height.is_some()
            {
                dropped.push("min/max size");
            }
            if e.keep_aspect_ratio {
                dropped.push("keepAspectRatio");
            }
            if e.allow_out_of_bounds {
                dropped.push("allowOutOfBounds");
            }
            if !dropped.is_empty() {
                self.warn(format!("{what}: {} not carried", dropped.join(", ")));
            }
            let icon = match e.ui_tile_rect.as_ref().or(e.tile_rect.as_ref()) {
                Some(r) => self.tile_rect(r, &what),
                None => None,
            };
            let color = self
                .color(&e.color, &format!("{what} color"))
                .unwrap_or_else(|| Color::new("#808080").expect("valid"));
            let limit = (e.max_count > 0).then_some(Limit {
                max: e.max_count,
                scope: match e.limit_scope.as_str() {
                    "PerLayer" => LimitScope::Layer,
                    "PerWorld" => LimitScope::World,
                    _ => LimitScope::Level,
                },
                behavior: match e.limit_behavior.as_str() {
                    "DiscardOldOnes" => LimitBehavior::DiscardOldest,
                    "MoveLastOne" => LimitBehavior::MoveLastOne,
                    _ => LimitBehavior::PreventAdding,
                },
            });
            let mut names = Namer::default();
            let mut fields = Vec::new();
            let mut infos = HashMap::new();
            for f in &e.field_defs {
                let fi = self.field_def(f, Some(&info.slug), &mut names, &what);
                fields.push(fi.def.clone());
                infos.insert(f.uid, fi);
            }
            if let Some(ent) = self.entities.get_mut(&e.uid) {
                ent.fields = infos;
            }
            out.push(EntityDef {
                slug: info.slug.clone(),
                name: e.identifier.clone(),
                category: None,
                tags: e.tags.clone(),
                doc: e.doc.clone().filter(|d| !d.is_empty()),
                size: info.size,
                resizable: [e.resizable_x, e.resizable_y],
                pivot: [e.pivot_x, e.pivot_y],
                icon,
                color,
                limit,
                fields,
            });
        }
        out
    }

    /// Convert one field definition. `owner` is the entity slug (for "only
    /// same type" references).
    fn field_def(
        &mut self,
        f: &j::FieldDef,
        owner: Option<&Slug>,
        names: &mut Namer,
        owner_what: &str,
    ) -> FieldInfo {
        let what = format!("{owner_what} field {:?}", f.identifier);
        let slug = names.claim(&snake_case(&f.identifier, "field"));
        self.renamed(&f.identifier, &slug);
        let (base, arg) = match f.ty.split_once('(') {
            Some((b, rest)) => (b, rest.trim_end_matches(')').parse::<i64>().ok()),
            None => (f.ty.as_str(), None),
        };
        let mut enum_uid = None;
        let ty = match base {
            "F_Int" => FieldType::Int {
                min: f.min.clone(),
                max: f.max.clone(),
            },
            "F_Float" => FieldType::Float {
                min: f.min.clone(),
                max: f.max.clone(),
            },
            "F_Bool" => FieldType::Bool,
            "F_String" => FieldType::String {
                regex: f.regex.as_deref().map(strip_regex),
            },
            "F_Text" => FieldType::Text {
                regex: f.regex.as_deref().map(strip_regex),
            },
            "F_Color" => FieldType::Color,
            "F_Enum" => match arg.and_then(|uid| self.enums.get(&uid).map(|e| (uid, e))) {
                Some((uid, e)) => {
                    enum_uid = Some(uid);
                    FieldType::Enum(EnumSource::Project(e.slug.clone()))
                }
                None => {
                    self.warn(format!("{what}: unknown enum; imported as a string field"));
                    FieldType::string()
                }
            },
            "F_Point" => FieldType::Point,
            "F_EntityRef" => {
                let mut allowed_types = Vec::new();
                let mut allowed_tags = Vec::new();
                match f.allowed_refs.as_str() {
                    "OnlySame" => {
                        if let Some(o) = owner {
                            allowed_types = vec![o.clone()];
                        }
                    }
                    "OnlyTags" => allowed_tags = f.allowed_ref_tags.clone(),
                    "OnlySpecificEntity" => {
                        match f
                            .allowed_refs_entity_uid
                            .and_then(|u| self.entities.get(&u))
                        {
                            Some(t) => allowed_types = vec![t.slug.clone()],
                            None => self.warn(format!(
                                "{what}: allowed entity type not found; any type allowed"
                            )),
                        }
                    }
                    _ => {}
                }
                if f.symmetrical_ref {
                    self.warn(format!(
                        "{what}: symmetricalRef (two-way links) not carried"
                    ));
                }
                FieldType::EntityRef {
                    allowed_types,
                    allowed_tags,
                    cross_level: f.allow_out_of_level_ref,
                }
            }
            "F_Path" => FieldType::Path {
                extensions: f
                    .accept_file_types
                    .iter()
                    .flatten()
                    .map(|e| {
                        e.trim_start_matches('*')
                            .trim_start_matches('.')
                            .to_string()
                    })
                    .filter(|e| !e.is_empty())
                    .collect(),
            },
            "F_Tile" => FieldType::Tile {
                tileset: f
                    .tileset_uid
                    .and_then(|u| self.tilesets.get(&u))
                    .map(|t| t.slug.clone()),
            },
            other => {
                self.warn(format!(
                    "{what}: unknown LDtk type {other:?}, imported as json"
                ));
                FieldType::Json
            }
        };
        let is_ref_or_point = matches!(ty, FieldType::EntityRef { .. } | FieldType::Point);
        let mut d = FieldDef::new(slug, ty);
        if d.slug.as_str() != f.identifier {
            d.name = Some(f.identifier.clone());
        }
        d.array = f.is_array;
        d.nullable = f.can_be_null;
        d.doc = f.doc.clone().filter(|s| !s.is_empty());
        if f.array_min_length.is_some() || f.array_max_length.is_some() {
            self.warn(format!("{what}: array length limits not carried"));
        }
        let mode = f.editor_display_mode.as_str();
        let canvas = !mode.is_empty() && mode != "Hidden";
        d.display = FieldDisplay {
            canvas,
            arrow: is_ref_or_point
                && matches!(
                    mode,
                    "RefLinkBetweenPivots"
                        | "RefLinkBetweenCenters"
                        | "Points"
                        | "PointStar"
                        | "PointPath"
                        | "PointPathLoop"
                ),
            world: canvas && f.editor_show_in_world,
        };
        let mut info = FieldInfo {
            def: d,
            enum_uid,
            ldtk_name: f.identifier.clone(),
        };
        info.def.default = self.field_default(f, &info, &what);
        info
    }

    /// The default for a field: `defaultOverride` if set, else what LDtk
    /// implicitly uses for a non-nullable scalar.
    fn field_default(&mut self, f: &j::FieldDef, info: &FieldInfo, what: &str) -> Option<Value> {
        let d = &info.def;
        if let Some(ov) = &f.default_override {
            if d.array {
                self.warn(format!(
                    "{what}: default for new array elements not carried (Worlds defaults are \
                     whole values)"
                ));
                return None;
            }
            let p = ov.params.first().cloned().unwrap_or(Value::Null);
            let v = match (&d.ty, ov.id.as_str()) {
                (FieldType::Color, "V_Int") => p.as_i64().map(|c| json!(int_color(c).as_str())),
                (FieldType::Enum(_), "V_String") => {
                    let s = p.as_str().unwrap_or_default();
                    self.enum_value(info, s, what)
                }
                (FieldType::Path { .. }, "V_String") => {
                    p.as_str().map(|s| json!(self.rebase_path(s)))
                }
                (_, "V_Int" | "V_Float" | "V_Bool" | "V_String") => Some(p),
                (_, other) => {
                    self.warn(format!("{what}: default of kind {other:?} not carried"));
                    None
                }
            };
            return v;
        }
        if d.array || d.nullable {
            return None;
        }
        let (min, max) = d.ty.bounds();
        let clamp = |x: f64| {
            let lo = min.and_then(|n| n.as_f64());
            let hi = max.and_then(|n| n.as_f64());
            let x = lo.map_or(x, |lo| x.max(lo));
            hi.map_or(x, |hi| x.min(hi))
        };
        match &d.ty {
            FieldType::Int { .. } => Some(json!(clamp(0.0).ceil() as i64)),
            FieldType::Float { .. } => {
                let x = clamp(0.0);
                Some(if x == 0.0 { json!(0) } else { json!(x) })
            }
            FieldType::Bool => Some(json!(false)),
            FieldType::String { .. } | FieldType::Text { .. } | FieldType::Path { .. } => {
                Some(json!(""))
            }
            FieldType::Color => Some(json!("#000000")),
            FieldType::Enum(_) => {
                let e = info.enum_uid.and_then(|u| self.enums.get(&u))?;
                e.first.as_ref().map(|s| json!(s.as_str()))
            }
            _ => None,
        }
    }

    fn enum_value(&mut self, info: &FieldInfo, id: &str, what: &str) -> Option<Value> {
        let e = info.enum_uid.and_then(|u| self.enums.get(&u));
        match e.and_then(|e| e.values.get(id)) {
            Some(s) => Some(json!(s.as_str())),
            None => {
                self.warn(format!(
                    "{what}: {id:?} is not a value of its enum; dropped"
                ));
                None
            }
        }
    }

    /// A field instance's value in Worlds form, or None to leave it out
    /// (LDtk is using the default).
    ///
    /// For an array, `realEditorValues` has one entry per element and `null`
    /// means "this element is at the element default", not "unset": the
    /// array is set whenever it has elements, and `__value` holds them
    /// resolved. Only an empty array is left to the field default.
    pub(super) fn field_value(
        &mut self,
        info: &FieldInfo,
        fi: &j::FieldInstance,
        what: &str,
    ) -> Option<Value> {
        let what = format!("{what} field {:?}", info.ldtk_name);
        if info.def.array {
            let items = fi.value.as_array().cloned().unwrap_or_default();
            if items.is_empty() && fi.real_editor_values.is_empty() {
                return None;
            }
            let out: Vec<Value> = items
                .iter()
                .filter_map(|v| self.scalar_value(info, v, &what))
                .collect();
            Some(Value::Array(out))
        } else {
            if fi.real_editor_values.iter().all(Value::is_null) {
                return None;
            }
            let v = self.scalar_value(info, &fi.value, &what);
            match v {
                Some(v) => Some(v),
                None if info.def.nullable => Some(Value::Null),
                None => None,
            }
        }
    }

    fn scalar_value(&mut self, info: &FieldInfo, v: &Value, what: &str) -> Option<Value> {
        if v.is_null() {
            return Some(Value::Null).filter(|_| info.def.nullable);
        }
        match &info.def.ty {
            FieldType::Enum(_) => {
                let s = v.as_str()?.to_string();
                self.enum_value(info, &s, what)
            }
            FieldType::Point => {
                let cx = v.get("cx")?.as_i64()?;
                let cy = v.get("cy")?.as_i64()?;
                Some(json!([cx, cy]))
            }
            FieldType::EntityRef { .. } => {
                let iid = v.get("entityIid")?.as_str()?;
                match self.entity_ids.get(iid) {
                    Some(id) => Some(json!(id.as_str())),
                    None => {
                        self.warn(format!(
                            "{what}: reference to missing entity {iid}; dropped"
                        ));
                        None
                    }
                }
            }
            FieldType::Tile { .. } => {
                let r: j::TileRect = serde_json::from_value(v.clone()).ok()?;
                let t = self.tile_rect(&r, what)?;
                Some(serde_json::to_value(t).expect("tile ref serializes"))
            }
            FieldType::Path { .. } => match v.as_str() {
                Some(s) => Some(json!(self.rebase_path(s))),
                None => Some(v.clone()),
            },
            _ => Some(v.clone()),
        }
    }

    /// An LDtk `path` field value (relative to the `.ldtk` file) made
    /// relative to the Worlds project directory, like tileset images. Empty
    /// stays empty (LDtk's "no file").
    fn rebase_path(&self, s: &str) -> String {
        if s.is_empty() {
            String::new()
        } else {
            self.image_path(s)
        }
    }
}

/// LDtk regexes are stored as `/pattern/flags`. The `i` flag becomes a
/// `(?i)` prefix; the others don't apply to matching one whole value (`g`)
/// or have no Worlds equivalent, and are dropped.
fn strip_regex(r: &str) -> String {
    match r.strip_prefix('/').and_then(|s| s.rsplit_once('/')) {
        Some((body, flags)) if flags.contains('i') => format!("(?i){body}"),
        Some((body, _)) => body.to_string(),
        None => r.to_string(),
    }
}

/// `0xRRGGBB` -> `#RRGGBB`.
fn int_color(c: i64) -> Color {
    Color::new(format!("#{:06X}", c & 0xFF_FFFF)).expect("6 hex digits")
}

#[cfg(test)]
mod tests {
    use super::strip_regex;

    #[test]
    fn ldtk_regex_flags() {
        assert_eq!(strip_regex("/^key_\\d+$/g"), "^key_\\d+$");
        assert_eq!(strip_regex("/brass|iron/gi"), "(?i)brass|iron");
        assert_eq!(strip_regex("/door/i"), "(?i)door");
        assert_eq!(strip_regex("plain"), "plain");
    }
}
