//! Worlds, levels and per-level layer content, plus LDtk's baked autotiles
//! for the parity reference.

use std::collections::{BTreeMap, BTreeSet};

use super::names::{Namer, snake_case};
use super::{Ctx, Error, RefTile, Result, json as j};
use crate::id::{Id, Slug, slug};
use crate::model::{
    EntitiesLayer, EntityInstance, Flip, IntGridLayer, LayerState, Level, TilePlacement,
    TilesLayer, World,
};

/// Cells of `grid` px needed to cover `px` pixels (LDtk's `cWid`/`cHei`).
fn cells(px: u32, grid: u32) -> u32 {
    px.div_ceil(grid.max(1))
}

impl Ctx {
    pub(super) fn worlds(&mut self, src: &j::Project) -> Result<Vec<World>> {
        self.assign_entity_ids(src);
        let mut level_names = Namer::default();
        if src.worlds.is_empty() {
            let mut w = World::new(slug("main"));
            w.levels = self.levels(&src.levels, src.world_layout.as_deref(), &mut level_names)?;
            return Ok(vec![w]);
        }
        if !src.levels.is_empty() {
            self.warn(format!(
                "{} top-level levels ignored: the file uses worlds[]",
                src.levels.len()
            ));
        }
        let mut world_names = Namer::default();
        let mut out = Vec::new();
        for w in &src.worlds {
            let s = world_names.claim(&snake_case(&w.identifier, "world"));
            self.renamed(&w.identifier, &s);
            let mut world = World::new(s);
            world.name = Some(w.identifier.clone());
            let layout = w.world_layout.as_deref().or(src.world_layout.as_deref());
            world.levels = self.levels(&w.levels, layout, &mut level_names)?;
            out.push(world);
        }
        Ok(out)
    }

    /// Give every entity instance in the file a fresh id first, so references
    /// resolve whatever order levels come in.
    fn assign_entity_ids(&mut self, src: &j::Project) {
        let mut used = BTreeSet::new();
        let levels = src
            .levels
            .iter()
            .chain(src.worlds.iter().flat_map(|w| &w.levels));
        for l in levels {
            for li in l.layer_instances.iter().flatten() {
                for e in &li.entity_instances {
                    let id = loop {
                        let id = Id::generate();
                        if used.insert(id.clone()) {
                            break id;
                        }
                    };
                    self.entity_ids.insert(e.iid.clone(), id);
                }
            }
        }
    }

    fn levels(
        &mut self,
        src: &[j::Level],
        layout: Option<&str>,
        names: &mut Namer,
    ) -> Result<Vec<Level>> {
        let mut out = Vec::new();
        let mut cursor = 0i64;
        for l in src {
            let world_px = match layout {
                // Linear layouts store -1 and position levels on the fly.
                Some("LinearHorizontal") => {
                    let p = [cursor, 0];
                    cursor += l.px_wid as i64;
                    p
                }
                Some("LinearVertical") => {
                    let p = [0, cursor];
                    cursor += l.px_hei as i64;
                    p
                }
                _ => [l.world_x, l.world_y],
            };
            out.push(self.level(l, world_px, names)?);
        }
        Ok(out)
    }

    fn level(&mut self, l: &j::Level, world_px: [i64; 2], names: &mut Namer) -> Result<Level> {
        let what = format!("level {:?}", l.identifier);
        let g = self.grid;
        if !l.px_wid.is_multiple_of(g) || !l.px_hei.is_multiple_of(g) {
            return Err(Error::Unsupported(format!(
                "{what}: {}x{} px is not a whole number of {g} px cells",
                l.px_wid, l.px_hei
            )));
        }
        let s = names.claim(&snake_case(&l.identifier, "level"));
        self.renamed(&l.identifier, &s);
        let size = [l.px_wid / g, l.px_hei / g];
        let mut level = Level::new(s.clone(), l.identifier.clone(), size);
        level.world_px = world_px;
        level.depth = l.world_depth;
        level.bg_color = l
            .bg_color
            .as_deref()
            .and_then(|c| self.color(c, &format!("{what} bgColor")));
        if l.bg_rel_path.is_some() {
            self.warn(format!("{what}: background image dropped"));
        }
        for fi in &l.field_instances {
            let Some(info) = self.level_fields.get(&fi.def_uid).cloned() else {
                continue;
            };
            if let Some(v) = self.field_value(&info, fi, &what) {
                level.fields.insert(info.def.slug.clone(), v);
            }
        }
        let Some(instances) = &l.layer_instances else {
            self.warn(format!("{what}: no layer data; imported empty"));
            return Ok(level);
        };
        for li in instances {
            self.layer_instance(li, &mut level, &what)?;
        }
        Ok(level)
    }

    fn layer_instance(
        &mut self,
        li: &j::LayerInstance,
        level: &mut Level,
        what: &str,
    ) -> Result<()> {
        let Some(info) = self.layers.get(&li.layer_def_uid).cloned() else {
            return Ok(());
        };
        let lwhat = format!("{what} layer {:?}", info.ldtk_name);
        if li.px_offset_x != 0 || li.px_offset_y != 0 {
            self.warn(format!("{lwhat}: pixel offset dropped"));
        }
        // This layer's grid; the level is a whole number of project cells.
        let lg = info.grid;
        let [w, h] = [
            cells(level.size[0] * self.grid, lg),
            cells(level.size[1] * self.grid, lg),
        ];
        let tileset_uid = li.override_tileset_uid.or(info.tileset_uid);
        if let Some(auto) = &info.auto {
            let mut optional_groups = Vec::new();
            for uid in &li.optional_rules {
                match auto.groups.get(uid) {
                    Some(s) => optional_groups.push(s.clone()),
                    None => self.warn(format!("{lwhat}: unknown optional rule group {uid}")),
                }
            }
            // The override replaces every tileset the rules draw from; one
            // equal to a rule's own tileset changes nothing for it.
            let tilesets = match li.override_tileset_uid.and_then(|u| self.tilesets.get(&u)) {
                Some(o) => auto
                    .tilesets
                    .iter()
                    .filter(|t| **t != o.slug)
                    .map(|t| (t.clone(), o.slug.clone()))
                    .collect(),
                None => BTreeMap::new(),
            };
            // LDtk keeps the rules' tile *ids* and reads them in the override
            // tileset; Worlds keeps `[col, row]`. Same thing unless the rows differ.
            if let (Some(o), Some(own)) = (
                li.override_tileset_uid.and_then(|u| self.tilesets.get(&u)),
                info.tileset_uid.and_then(|u| self.tilesets.get(&u)),
            ) && o.c_wid != own.c_wid
            {
                let msg = format!(
                    "{lwhat}: tileset override {} has {} tiles per row, the layer's {} has {}; \
                     rule tiles keep their [col, row], where LDtk kept tile ids",
                    o.slug, o.c_wid, own.slug, own.c_wid
                );
                self.warn(msg);
            }
            level.layers.insert(
                auto.slug.clone(),
                LayerState {
                    seed: li.seed,
                    optional_groups,
                    tilesets,
                },
            );
            self.reference_tiles(li, level, w, &auto.rules, &auto.slug, tileset_uid, &lwhat);
        }
        match info.ldtk_type.as_str() {
            "IntGrid" => {
                let expected = (w * h) as usize;
                if li.int_grid_csv.len() != expected {
                    self.warn(format!(
                        "{lwhat}: {} IntGrid cells for a {w}x{h} level; missing cells are empty",
                        li.int_grid_csv.len()
                    ));
                }
                let mut grid = Vec::with_capacity(expected);
                let mut unknown = BTreeSet::new();
                for i in 0..expected {
                    let v = li.int_grid_csv.get(i).copied().unwrap_or(0);
                    grid.push(match v {
                        0 => 0,
                        v => match info.values.get(&v) {
                            Some(vi) => vi.index,
                            None => {
                                unknown.insert(v);
                                0
                            }
                        },
                    });
                }
                if !unknown.is_empty() {
                    self.warn(format!(
                        "{lwhat}: cells with undefined values {unknown:?} imported as empty"
                    ));
                }
                let grid = IntGridLayer::from_cells(info.slug.clone(), [w, h], grid);
                if !grid.is_empty() {
                    level.int_grids.push(grid);
                }
            }
            "Tiles" => {
                let mut tiles = Vec::new();
                let ts = tileset_uid.and_then(|u| self.tilesets.get(&u)).cloned();
                for t in &li.grid_tiles {
                    let Some(ts) = &ts else {
                        self.warn(format!(
                            "{lwhat}: tiles from a tileset that wasn't imported dropped"
                        ));
                        break;
                    };
                    let g = lg as i32;
                    let [px, py] = t.px;
                    if px % g != 0 || py % g != 0 {
                        self.warn(format!("{lwhat}: off-grid tiles snapped to their cell"));
                    }
                    let (cx, cy) = (px.div_euclid(g), py.div_euclid(g));
                    if cx < 0 || cy < 0 || cx >= w as i32 || cy >= h as i32 {
                        self.warn(format!("{lwhat}: tiles outside the level dropped"));
                        continue;
                    }
                    tiles.push(TilePlacement {
                        cell: [cx as u32, cy as u32],
                        tileset: ts.slug.clone(),
                        tile: ts.tile(t.t),
                        flip: flip(t.f),
                        alpha: t.a.unwrap_or(1.0),
                    });
                }
                if !tiles.is_empty() {
                    let mut layer = TilesLayer {
                        layer: info.slug.clone(),
                        tiles,
                    };
                    layer.sort();
                    level.tiles.push(layer);
                }
            }
            "Entities" => {
                let mut entities = Vec::new();
                for e in &li.entity_instances {
                    if let Some(inst) = self.entity_instance(e, lg, [w, h], &lwhat) {
                        entities.push(inst);
                    }
                }
                if !entities.is_empty() {
                    level.entities.push(EntitiesLayer {
                        layer: info.slug.clone(),
                        entities,
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// One entity on a layer with a `lg` px grid, `cells` big.
    fn entity_instance(
        &mut self,
        e: &j::EntityInstance,
        lg: u32,
        cells: [u32; 2],
        what: &str,
    ) -> Option<EntityInstance> {
        let Some(def) = self.entities.get(&e.def_uid).cloned() else {
            self.warn(format!("{what}: entity of unknown type dropped"));
            return None;
        };
        let ewhat = format!("{what} entity {:?}", def.ldtk_name);
        let g = lg as f64;
        // LDtk `px` is the pivot point. Worlds: pivot = (cell + pivot) * grid + offset.
        let mut cell = [0i32; 2];
        let mut offset = [0i32; 2];
        for a in 0..2 {
            let base = e.px[a] as f64 - def.pivot[a] * g;
            let c = (base / g).floor();
            let mut off = (base - c * g).round() as i32;
            let mut c = c as i32;
            if off == lg as i32 {
                c += 1;
                off = 0;
            }
            cell[a] = c;
            offset[a] = off;
        }
        if offset != [0, 0] {
            self.warn(format!(
                "{ewhat}: placed off the grid; kept as a pixel offset"
            ));
        }
        if cell[0] < 0 || cell[1] < 0 || cell[0] >= cells[0] as i32 || cell[1] >= cells[1] as i32 {
            self.warn(format!(
                "{ewhat}: pivot outside the level (allowOutOfBounds); entity dropped"
            ));
            return None;
        }
        let px_size = [e.width, e.height];
        let mut size = def.size;
        for a in 0..2 {
            if px_size[a] != def.px_size[a] {
                size[a] = px_size[a].div_ceil(self.grid).max(1);
                if !px_size[a].is_multiple_of(self.grid) {
                    self.warn(format!(
                        "{ewhat}: resized to {}x{} px, not whole cells; rounded up",
                        px_size[0], px_size[1]
                    ));
                }
            }
        }
        let mut fields = BTreeMap::new();
        for fi in &e.field_instances {
            let Some(info) = def.fields.get(&fi.def_uid).cloned() else {
                continue;
            };
            if let Some(v) = self.field_value(&info, fi, &ewhat) {
                fields.insert(info.def.slug.clone(), v);
            }
        }
        Some(EntityInstance {
            id: self.entity_ids[&e.iid].clone(),
            ty: def.slug.clone(),
            cell,
            offset_px: offset,
            size: (size != def.size).then_some(size),
            fields,
        })
    }

    /// LDtk's baked autotiles for one auto layer in one level.
    #[allow(clippy::too_many_arguments)]
    fn reference_tiles(
        &mut self,
        li: &j::LayerInstance,
        level: &Level,
        c_wid: u32,
        rules: &std::collections::HashMap<i64, Slug>,
        layer: &Slug,
        tileset_uid: Option<i64>,
        what: &str,
    ) {
        if li.auto_layer_tiles.is_empty() {
            return;
        }
        let Some(ts) = tileset_uid.and_then(|u| self.tilesets.get(&u)).cloned() else {
            self.warn(format!(
                "{what}: baked autotiles use a tileset that wasn't imported"
            ));
            return;
        };
        let c_wid = c_wid.max(1) as i64;
        let mut out = Vec::with_capacity(li.auto_layer_tiles.len());
        for t in &li.auto_layer_tiles {
            let rule_uid = t.d.first().copied().unwrap_or(-1);
            let coord = t.d.get(1).copied().unwrap_or(0);
            let rule = match rules.get(&rule_uid) {
                Some(s) => s.clone(),
                None => {
                    self.warn(format!(
                        "{what}: baked autotiles from rule {rule_uid}, which wasn't imported"
                    ));
                    Slug::new(format!("r_{}", rule_uid.max(0))).expect("valid slug")
                }
            };
            out.push(RefTile {
                px: t.px,
                tileset: ts.slug.clone(),
                tile: ts.tile(t.t),
                flip: flip(t.f),
                alpha: t.a.unwrap_or(1.0),
                rule,
                anchor_cell: [(coord % c_wid) as i32, (coord / c_wid) as i32],
            });
        }
        self.reference
            .autotiles
            .entry(level.slug.clone())
            .or_default()
            .insert(layer.clone(), out);
    }
}

fn flip(f: u8) -> Option<Flip> {
    match f & 3 {
        1 => Some(Flip::X),
        2 => Some(Flip::Y),
        3 => Some(Flip::XY),
        _ => None,
    }
}
