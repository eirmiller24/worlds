//! Entities in the editor's command layer (no GPU): definition changes
//! that bake nothing, the entity tool (click, drag, Shift resize, Ctrl
//! pixel placement, Alt sampling, limits), the canvas geometry, disk
//! changes to `entities/*.json`, and `--synthetic-entities`. Runs on a temp
//! copy of `worlds-core`'s `mini.worlds` fixture.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;
use worlds_core::{Command, EntityInstance, Slug};
use worlds_editor::app::watch::{disk_entity_command, entity_files};
use worlds_editor::doc::Document;
use worlds_editor::edit::Editing;
use worlds_editor::script::{GestureStep, cell_hit, gesture_events};
use worlds_editor::tools::{
    CommandContext, EditorCommands, EntityBrush, Grab, Invalidation, Tool, ToolEvent,
};
use worlds_editor::view::ViewState;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

fn slug(s: &str) -> Slug {
    Slug::new(s).unwrap()
}

struct Rig {
    doc: Document,
    view: ViewState,
    ed: Editing,
    inv: Vec<Invalidation>,
    dir: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        Self::new_with(|_| {})
    }

    /// The fixture, changed on disk by `edit` before it is opened.
    fn new_with(edit: impl FnOnce(&Path)) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let src =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../worlds-core/tests/fixtures/mini.worlds");
        let dir = tmp.path().join("mini.worlds");
        copy_dir(&src, &dir);
        std::fs::create_dir_all(dir.join("art")).unwrap();
        for (name, px) in [("castle_main", 256), ("ssw_set5", 2 + 8 * 33)] {
            image::RgbaImage::from_pixel(px, px, image::Rgba([90, 80, 70, 255]))
                .save(dir.join(format!("art/{name}.png")))
                .unwrap();
        }
        edit(&dir);
        Self::open(tmp, dir)
    }

    fn open(tmp: tempfile::TempDir, dir: PathBuf) -> Self {
        let mut doc = Document::open(&dir).unwrap();
        doc.bake_all_blocking();
        let mut view = ViewState::new();
        view.sync_layers(&doc, None);
        view.active_level = doc.level_ix("crevasse").or(Some(0));
        let mut r = Rig {
            doc,
            view,
            ed: Editing::new(),
            inv: Vec::new(),
            dir,
            _tmp: tmp,
        };
        // Warm the cache with the worker bakes (as the first frame does).
        r.events(&[]);
        r
    }

    fn cx(&mut self) -> CommandContext<'_> {
        CommandContext {
            doc: &mut self.doc,
            view: &mut self.view,
            invalidate: &mut self.inv,
        }
    }

    fn events(&mut self, events: &[ToolEvent]) {
        let mut ed = std::mem::take(&mut self.ed);
        ed.tool_events(&mut self.cx(), events);
        self.ed = ed;
    }

    fn gesture(&mut self, json: serde_json::Value) {
        let g: GestureStep = serde_json::from_value(json).unwrap();
        let plan = gesture_events(
            &self.doc,
            &g,
            self.view.active_layer,
            self.view.entities.brush.as_ref(),
        )
        .unwrap();
        if let Some(b) = plan.entity_brush {
            self.view.entities.brush = Some(b);
        }
        self.view.active_level = Some(plan.level);
        self.view.active_layer = Some(plan.layer);
        self.view.status = None;
        self.events(&plan.events);
    }

    fn apply(&mut self, cmd: Command) -> Result<(), String> {
        let mut ed = std::mem::take(&mut self.ed);
        let r = ed.apply(&mut self.cx(), cmd);
        self.ed = ed;
        r
    }

    fn undo(&mut self) {
        let mut ed = std::mem::take(&mut self.ed);
        ed.undo(&mut self.cx());
        self.ed = ed;
    }

    fn redo(&mut self) {
        let mut ed = std::mem::take(&mut self.ed);
        ed.redo(&mut self.cx());
        self.ed = ed;
    }

    fn entities(&self, level: &str) -> Vec<EntityInstance> {
        self.doc
            .project
            .level(level)
            .unwrap()
            .all_entities()
            .cloned()
            .collect()
    }

    fn of_type(&self, level: &str, ty: &str) -> Vec<EntityInstance> {
        self.entities(level)
            .into_iter()
            .filter(|e| e.ty == ty)
            .collect()
    }

    fn entries(&self) -> usize {
        self.ed.history().undo_entries().len()
    }

    fn last_label(&self) -> String {
        self.ed.undo_label().unwrap_or_default()
    }

    fn status_error(&self) -> Option<String> {
        self.view
            .status
            .as_ref()
            .filter(|m| m.error)
            .map(|m| m.text.clone())
    }

    /// Every level's bake of every auto layer, as pointers.
    fn bake_ptrs(&self) -> Vec<(usize, String, usize)> {
        let mut v = Vec::new();
        for (ix, b) in self.doc.bakes.iter().enumerate() {
            for (l, bake) in b.as_ref().unwrap().iter() {
                v.push((ix, l.to_string(), Arc::as_ptr(bake) as usize));
            }
        }
        v
    }

    fn layer(&self, slug: &str) -> usize {
        self.doc
            .project
            .layers
            .iter()
            .position(|l| l.slug == slug)
            .unwrap()
    }
}

// ---- definition changes bake nothing ----

#[test]
fn entity_and_enum_def_changes_keep_every_bake() {
    let mut r = Rig::new();
    // A stroke first, so the cache holds crevasse's bakes too.
    r.gesture(
        json!({ "gesture": "brush", "level": "crevasse", "layer": "collision",
        "path": [[3, 3]] }),
    );
    let before = r.bake_ptrs();
    assert!(!before.is_empty());
    let door = r.doc.project.entity_def("locked_door").unwrap().clone();
    let mut door2 = door.clone();
    door2.name = "Heavy Door".into();
    door2.color = worlds_core::Color::new("#123456").unwrap();
    let mut facing = r.doc.project.enum_def("facing").unwrap().clone();
    facing.values.push(worlds_core::EnumValue {
        slug: slug("up"),
        name: None,
        color: None,
        icon: None,
    });
    let cmds = [
        Command::SetEntityDef {
            entity: slug("locked_door"),
            def: door2,
        },
        Command::RenameEntityType {
            from: slug("floor_button"),
            to: slug("pressure_plate"),
        },
        Command::SetEnumDef {
            enum_: slug("facing"),
            def: facing,
            index: None,
        },
        Command::SetProjectSetting {
            setting: worlds_core::command::ProjectSetting::CollisionLayer(None),
        },
    ];
    let n = cmds.len();
    for cmd in cmds {
        let gen0 = r.doc.defs_gen;
        r.inv.clear();
        r.apply(cmd).unwrap();
        assert!(!r.inv.contains(&Invalidation::All), "{:?}", r.inv);
        assert!(
            r.inv
                .iter()
                .all(|i| !matches!(i, Invalidation::Layer { .. } | Invalidation::Tilesets)),
            "{:?}",
            r.inv
        );
        assert_eq!(r.bake_ptrs(), before, "bakes kept");
        assert_eq!(r.doc.layer_jobs_pending, 0);
        let _ = gen0;
    }
    assert_eq!(
        r.entries(),
        1 + n,
        "every edit is an entry, the stroke kept"
    );
    assert!(r.doc.defs_gen >= 3);
    for _ in 0..n {
        r.inv.clear();
        r.undo();
        assert!(!r.inv.contains(&Invalidation::All), "{:?}", r.inv);
        assert_eq!(r.bake_ptrs(), before);
    }
    assert_eq!(r.doc.project.entity_def("locked_door"), Some(&door));
    assert!(r.doc.project.entity_def("floor_button").is_some());
    for _ in 0..n {
        r.redo();
        assert_eq!(r.bake_ptrs(), before);
    }
    assert_eq!(r.entries(), 1 + n);
    assert!(r.doc.project.entity_def("pressure_plate").is_some());
    // The stroke is still there under them.
    for _ in 0..n {
        r.undo();
    }
    assert_eq!(r.last_label(), "Paint collision");
}

#[test]
fn level_field_changes_rebake_only_layers_reading_level_fields() {
    // mini's main_tiling has a group requiring the `biome` level field:
    // renaming the biome enum rewrites that field's definition, so
    // main_tiling rebakes (as a layer, never `All`).
    let mut r = Rig::new();
    let crev = r.doc.level_ix("crevasse").unwrap();
    r.inv.clear();
    r.apply(Command::RenameEnum {
        from: slug("biome"),
        to: slug("terrain"),
    })
    .unwrap();
    assert!(!r.inv.contains(&Invalidation::All), "{:?}", r.inv);
    for i in &r.inv {
        match i {
            Invalidation::Layer { layer, .. } | Invalidation::Cells { layer, .. } => {
                assert_eq!(layer.as_str(), "main_tiling")
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(r.inv.contains(&Invalidation::Layer {
        level: crev,
        layer: slug("main_tiling")
    }));
    r.doc.finish_layer_bakes();
    // Without such a group, nothing rebakes at all.
    let mut r = Rig::new_with(|dir| {
        let p = dir.join("rules/main_tiling.json");
        let text = std::fs::read_to_string(&p).unwrap();
        let text: String = text
            .lines()
            .filter(|l| !l.contains("requires_level_field"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&p, text).unwrap();
    });
    let before = r.bake_ptrs();
    r.inv.clear();
    r.apply(Command::RenameEnum {
        from: slug("biome"),
        to: slug("terrain"),
    })
    .unwrap();
    assert!(r.inv.is_empty(), "{:?}", r.inv);
    assert_eq!(r.bake_ptrs(), before);
    assert_eq!(r.doc.layer_jobs_pending, 0);
}

#[test]
fn renaming_a_type_follows_into_the_palette() {
    let mut r = Rig::new();
    let p = &mut r.view.entities;
    p.favorites = vec![slug("floor_button"), slug("locked_door")];
    p.recent = vec![slug("locked_door"), slug("floor_button")];
    p.brush = Some(EntityBrush::of(slug("floor_button")));
    let gen0 = r.doc.defs_gen;
    r.apply(Command::RenameEntityType {
        from: slug("floor_button"),
        to: slug("plate"),
    })
    .unwrap();
    assert!(r.doc.defs_gen > gen0, "palette caches see the change");
    let p = &r.view.entities;
    assert_eq!(p.favorites, [slug("plate"), slug("locked_door")]);
    assert_eq!(p.recent, [slug("locked_door"), slug("plate")]);
    assert_eq!(p.brush.as_ref().unwrap().ty, "plate");
    r.undo();
    let p = &r.view.entities;
    assert_eq!(p.favorites[0], "floor_button");
    assert_eq!(p.brush.as_ref().unwrap().ty, "floor_button");
}

// ---- the entity tool ----

#[test]
fn a_click_places_the_brush_as_one_entry() {
    let mut r = Rig::new();
    let n0 = r.entities("crevasse").len();
    let e0 = r.entries();
    r.gesture(
        json!({ "gesture": "entity", "level": "crevasse", "path": [[5, 4]],
        "type": "floor_button", "fields": { "channel": 7 } }),
    );
    assert_eq!(r.status_error(), None);
    let placed = r.of_type("crevasse", "floor_button");
    let e = placed.last().unwrap();
    assert_eq!(e.cell, [5, 4]);
    assert_eq!(e.offset_px, [0, 0]);
    assert_eq!(e.size, None);
    assert_eq!(e.fields.get("channel"), Some(&json!(7)));
    assert_eq!(r.entities("crevasse").len(), n0 + 1);
    assert_eq!(r.entries(), e0 + 1);
    assert_eq!(r.last_label(), "Place floor_button");
    assert_eq!(r.view.entities.recent, [slug("floor_button")]);
    // The default layer is the entities layer.
    assert_eq!(r.view.active_layer, Some(r.layer("objects")));
    // Ids are fresh and valid.
    assert!(worlds_core::Id::is_valid(e.id.as_str()));
    r.undo();
    assert_eq!(r.entities("crevasse").len(), n0);
}

#[test]
fn a_drag_places_at_the_release_cell() {
    let mut r = Rig::new();
    r.gesture(
        json!({ "gesture": "entity", "level": "crevasse", "path": [[2, 2], [3, 2], [6, 4]],
        "type": "floor_button" }),
    );
    let e = r.of_type("crevasse", "floor_button").pop().unwrap();
    assert_eq!(e.cell, [6, 4]);
    // A cancelled drag places nothing.
    let n = r.entities("crevasse").len();
    let e0 = r.entries();
    r.gesture(
        json!({ "gesture": "entity", "level": "crevasse", "path": [[2, 2], [4, 4]],
        "cancel": true }),
    );
    assert_eq!((r.entities("crevasse").len(), r.entries()), (n, e0));
    assert!(!r.ed.in_gesture());
}

#[test]
fn shift_drag_sizes_resizable_axes_in_project_cells() {
    let mut r = Rig::new();
    // locked_door: 1 x 3, resizable in y, pivot bottom center; at most 4
    // per level (crevasse has one).
    r.gesture(
        json!({ "gesture": "entity", "level": "crevasse", "path": [[10, 0], [12, 4]],
        "type": "locked_door", "shift": true }),
    );
    let e = r.of_type("crevasse", "locked_door").pop().unwrap();
    // x isn't resizable: the type's width, pivot in the start column; y
    // spans rows 0..=4, so the box is 5 high and its bottom (the pivot) is
    // the bottom of row 4.
    assert_eq!(e.size, Some([1, 5]));
    let g = 16.0;
    let def = r.doc.project.entity_def("locked_door").unwrap();
    let b = worlds_editor::tools::entity_box(def, &e, 16, 16);
    assert_eq!(b, [10.0 * g, 0.0, g, 5.0 * g], "{e:?}");
    assert_eq!(e.cell, [10, 4]);
    // Upward drags span the same way.
    r.gesture(
        json!({ "gesture": "entity", "level": "crevasse", "path": [[15, 4], [15, 2]],
        "type": "locked_door", "shift": true }),
    );
    let e = r.of_type("crevasse", "locked_door").pop().unwrap();
    assert_eq!(e.size, None, "3 rows: the type's own size");
    assert_eq!(e.cell, [15, 4]);
    // Without Shift, no size.
    r.gesture(
        json!({ "gesture": "entity", "level": "crevasse", "path": [[17, 2], [17, 4]],
        "type": "locked_door" }),
    );
    assert_eq!(
        r.of_type("crevasse", "locked_door").pop().unwrap().size,
        None
    );
}

#[test]
fn ctrl_places_by_pixel_in_the_containing_cell() {
    let mut r = Rig::new();
    r.view.entities.brush = Some(EntityBrush::of(slug("floor_button")));
    let ix = r.doc.level_ix("crevasse").unwrap();
    let layer = r.layer("objects");
    r.view.active_level = Some(ix);
    r.view.active_layer = Some(layer);
    let mut at = cell_hit(&r.doc, ix, layer, [4, 3]);
    let lp = r.doc.level(ix).world_px;
    // 5 px right of the cell's left edge, 2 px below its top.
    at.world_px = [
        lp[0] as f64 + 4.0 * 16.0 + 5.0,
        lp[1] as f64 + 3.0 * 16.0 + 2.0,
    ];
    let ctrl = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    r.events(&[
        ToolEvent::Begin {
            tool: Tool::Entity,
            at,
            modifiers: ctrl,
            grab: Grab::None,
        },
        ToolEvent::End {
            tool: Tool::Entity,
            start: at,
            at,
            modifiers: ctrl,
        },
    ]);
    let e = r.of_type("crevasse", "floor_button").pop().unwrap();
    // Pivot (0.5, 1.0): the pivot point is the pointer pixel.
    assert_eq!(e.cell, [4, 3]);
    assert_eq!(e.offset_px, [5 - 8, 2 - 16]);
    let def = r.doc.project.entity_def("floor_button").unwrap();
    let b = worlds_editor::tools::entity_box(def, &e, 16, 16);
    assert_eq!(
        [b[0] + 8.0, b[1] + 16.0],
        [4.0 * 16.0 + 5.0, 3.0 * 16.0 + 2.0]
    );
}

#[test]
fn alt_click_samples_type_and_fields() {
    let mut r = Rig::new();
    r.view.entities.brush = Some(EntityBrush::of(slug("locked_door")));
    let e0 = r.entries();
    // btncrev222: a floor button at (12, 3) with channel 3.
    r.gesture(json!({ "gesture": "entity", "level": "crevasse", "path": [[12, 3]], "alt": true }));
    let b = r.view.entities.brush.clone().unwrap();
    assert_eq!(b.ty, "floor_button");
    assert_eq!(b.fields.get("channel"), Some(&json!(3)));
    assert_eq!(b.fields.get("target"), Some(&json!("doorcrev22")));
    assert_eq!(r.entries(), e0, "sampling edits nothing");
    // The next click places a copy with those fields.
    r.gesture(json!({ "gesture": "entity", "level": "crevasse", "path": [[16, 3]] }));
    let e = r.of_type("crevasse", "floor_button").pop().unwrap();
    assert_eq!(e.cell, [16, 3]);
    assert_eq!(e.fields.get("channel"), Some(&json!(3)));
}

#[test]
fn the_brush_drops_values_its_type_no_longer_fits() {
    let mut r = Rig::new();
    r.view.entities.brush = Some(EntityBrush::of(slug("locked_door")));
    // btncrev222: channel 3, latching true, target doorcrev22.
    r.gesture(json!({ "gesture": "entity", "level": "crevasse", "path": [[12, 3]], "alt": true }));
    let keys = |r: &Rig| -> Vec<String> {
        let b = r.view.entities.brush.as_ref().unwrap();
        b.fields.keys().map(|k| k.to_string()).collect()
    };
    assert_eq!(keys(&r), ["channel", "latching", "target"]);
    // A renamed field: the old key would be written as an unknown one.
    r.apply(Command::RenameEntityField {
        entity: slug("floor_button"),
        from: slug("channel"),
        to: slug("ch"),
    })
    .unwrap();
    assert_eq!(keys(&r), ["latching", "target"]);
    // A retyped field (bool to int): `true` would be an invalid value.
    let def: worlds_core::FieldDef =
        serde_json::from_value(json!({ "slug": "latching", "type": "int", "default": 0 })).unwrap();
    r.apply(Command::SetEntityFieldDef {
        entity: slug("floor_button"),
        field: slug("latching"),
        def,
        index: None,
    })
    .unwrap();
    assert_eq!(keys(&r), ["target"]);
    // A removed field, even one whose stored values stay.
    r.apply(Command::RemoveEntityFieldDef {
        entity: slug("floor_button"),
        field: slug("target"),
        keep_values: true,
    })
    .unwrap();
    assert!(keys(&r).is_empty());
    // Placing writes nothing stale.
    r.gesture(json!({ "gesture": "entity", "level": "crevasse", "path": [[16, 3]] }));
    let e = r.of_type("crevasse", "floor_button").pop().unwrap();
    assert_eq!(e.cell, [16, 3]);
    assert!(e.fields.is_empty(), "{:?}", e.fields);
}

#[test]
fn refusals_leave_a_status_and_no_entry() {
    let mut r = Rig::new();
    let e0 = r.entries();
    // No brush yet.
    r.gesture(json!({ "gesture": "entity", "level": "crevasse", "path": [[1, 1]] }));
    assert!(r.status_error().unwrap().contains("Pick an entity type"));
    // A non-entities layer.
    r.gesture(
        json!({ "gesture": "entity", "level": "crevasse", "layer": "collision",
        "path": [[1, 1]], "type": "floor_button" }),
    );
    assert!(
        r.status_error().unwrap().contains("entities layers"),
        "{:?}",
        r.status_error()
    );
    // Outside the level.
    r.gesture(json!({ "gesture": "entity", "level": "crevasse", "path": [[1, 1], [-3, 1]] }));
    assert!(r.status_error().unwrap().contains("outside"));
    // A locked layer.
    let l = r.layer("objects");
    r.view.layers[l].locked = true;
    r.gesture(json!({ "gesture": "entity", "level": "crevasse", "path": [[1, 1]] }));
    assert!(r.status_error().unwrap().contains("locked"));
    assert_eq!(r.entries(), e0);
    assert!(!r.ed.in_gesture());
    // Script checks: an unknown type or field, entity keys on other tools.
    let bad = |v: serde_json::Value| {
        let g: GestureStep = serde_json::from_value(v).unwrap();
        format!("{:#}", gesture_events(&r.doc, &g, None, None).unwrap_err())
    };
    assert!(
        bad(json!({ "gesture": "entity", "level": "crevasse", "path": [[1, 1]], "type": "nope" }))
            .contains("no entity type")
    );
    assert!(
        bad(
            json!({ "gesture": "entity", "level": "crevasse", "path": [[1, 1]],
        "type": "floor_button", "fields": { "chanel": 1 } })
        )
        .contains("no field")
    );
    assert!(
        bad(json!({ "gesture": "brush", "level": "crevasse", "path": [[1, 1]], "ctrl": true }))
            .contains("entity gestures")
    );
}

/// Set locked_door's limit.
fn set_limit(r: &mut Rig, max: u32, scope: &str, behavior: &str) {
    let mut def = r.doc.project.entity_def("locked_door").unwrap().clone();
    def.limit = Some(
        serde_json::from_value(json!({ "max": max, "scope": scope, "behavior": behavior }))
            .unwrap(),
    );
    r.apply(Command::SetEntityDef {
        entity: slug("locked_door"),
        def,
    })
    .unwrap();
}

fn place_door(r: &mut Rig, level: &str, cell: [i32; 2]) {
    r.gesture(
        json!({ "gesture": "entity", "level": level, "path": [cell], "type": "locked_door" }),
    );
}

#[test]
fn discard_oldest_removes_the_oldest_in_the_same_entry() {
    let mut r = Rig::new();
    // The fixture's limit: 4 per level, discard_oldest; crevasse has one
    // (doorcrev22).
    for x in [2, 4, 6] {
        place_door(&mut r, "crevasse", [x, 4]);
    }
    assert_eq!(r.of_type("crevasse", "locked_door").len(), 4);
    let e0 = r.entries();
    place_door(&mut r, "crevasse", [8, 4]);
    let doors = r.of_type("crevasse", "locked_door");
    assert_eq!(doors.len(), 4);
    assert!(
        doors.iter().all(|d| d.id.as_str() != "doorcrev22"),
        "the oldest went"
    );
    assert_eq!(doors.last().unwrap().cell, [8, 4]);
    assert_eq!(r.entries(), e0 + 1, "one entry");
    // shaft's door is another level: untouched.
    assert_eq!(r.of_type("shaft", "locked_door").len(), 1);
    r.undo();
    let doors = r.of_type("crevasse", "locked_door");
    assert_eq!(doors.len(), 4);
    assert!(doors.iter().any(|d| d.id.as_str() == "doorcrev22"));
}

#[test]
fn prevent_adding_refuses() {
    let mut r = Rig::new();
    set_limit(&mut r, 1, "level", "prevent_adding");
    let e0 = r.entries();
    place_door(&mut r, "crevasse", [14, 4]);
    assert!(
        r.status_error().unwrap().contains("at most 1"),
        "{:?}",
        r.status_error()
    );
    assert_eq!(r.entries(), e0);
    assert_eq!(r.of_type("crevasse", "locked_door").len(), 1);
    // Another level has room.
    set_limit(&mut r, 2, "level", "prevent_adding");
    place_door(&mut r, "shaft", [3, 7]);
    assert_eq!(r.status_error(), None);
    assert_eq!(r.of_type("shaft", "locked_door").len(), 2);
}

#[test]
fn move_last_one_moves_the_newest() {
    let mut r = Rig::new();
    set_limit(&mut r, 2, "level", "move_last_one");
    place_door(&mut r, "crevasse", [14, 4]);
    let newest = r.of_type("crevasse", "locked_door").pop().unwrap();
    let e0 = r.entries();
    place_door(&mut r, "crevasse", [10, 2]);
    let doors = r.of_type("crevasse", "locked_door");
    assert_eq!(doors.len(), 2);
    let moved = doors.iter().find(|d| d.id == newest.id).unwrap();
    assert_eq!(moved.cell, [10, 2]);
    assert_eq!(r.entries(), e0 + 1);
    assert_eq!(r.last_label(), "Place locked_door");
    // Project scope: the newest in file order (levels in project order)
    // is shaft's door, in another level, so it is taken out there and put
    // here, keeping its id and fields.
    set_limit(&mut r, 3, "world", "move_last_one");
    place_door(&mut r, "crevasse", [12, 4]);
    assert_eq!(r.status_error(), None);
    assert_eq!(r.of_type("shaft", "locked_door").len(), 0);
    let doors = r.of_type("crevasse", "locked_door");
    assert_eq!(doors.len(), 3);
    let d = doors
        .iter()
        .find(|d| d.id.as_str() == "doorshaft2")
        .unwrap();
    assert_eq!(d.cell, [12, 4]);
}

#[test]
fn limits_by_layer_count_only_that_layer() {
    let mut r = Rig::new();
    // A second entities layer.
    r.apply(
        serde_json::from_value(json!({ "op": "set_layer_def", "layer": "props",
            "def": { "slug": "props", "name": "Props", "kind": "entities" } }))
        .unwrap(),
    )
    .unwrap();
    set_limit(&mut r, 1, "layer", "prevent_adding");
    r.gesture(
        json!({ "gesture": "entity", "level": "crevasse", "layer": "props",
        "path": [[14, 4]], "type": "locked_door" }),
    );
    assert_eq!(
        r.status_error(),
        None,
        "objects' door doesn't count on props"
    );
    r.gesture(
        json!({ "gesture": "entity", "level": "crevasse", "layer": "props",
        "path": [[16, 4]] }),
    );
    assert!(r.status_error().is_some());
}

// ---- canvas geometry ----

#[test]
fn entity_boxes_use_the_project_grid_for_size() {
    // An 8 px entities layer in a 16 px project: the pivot sits on the
    // layer grid, the size is in project cells.
    let mut r = Rig::new();
    r.apply(
        serde_json::from_value(json!({ "op": "set_layer_def", "layer": "fine",
            "def": { "slug": "fine", "name": "Fine", "kind": "entities", "grid_size": 8 } }))
        .unwrap(),
    )
    .unwrap();
    r.gesture(
        json!({ "gesture": "entity", "level": "crevasse", "layer": "fine",
        "path": [[10, 6]], "type": "locked_door" }),
    );
    let e = r
        .doc
        .project
        .level("crevasse")
        .unwrap()
        .entities_layer("fine")
        .unwrap()
        .entities[0]
        .clone();
    let def = r.doc.project.entity_def("locked_door").unwrap();
    let b = worlds_editor::tools::entity_box(def, &e, 8, 16);
    // The editor's box is the core helper's, the one source of truth.
    assert_eq!(def.pixel_rect(&e, 8, 16), [76, 8, 16, 48]);
    // Pivot (0.5, 1): (10.5 * 8, 7 * 8) = (84, 56); box 16 x 48 px.
    assert_eq!(b, [84.0 - 8.0, 56.0 - 48.0, 16.0, 48.0]);
    // The status bar readout (and hit-testing) finds it by that box.
    let ix = r.doc.level_ix("crevasse").unwrap();
    let lp = r.doc.level(ix).world_px;
    let fine = r.layer("fine");
    let mut h = cell_hit(&r.doc, ix, fine, [10, 3]);
    h.world_px = [lp[0] as f64 + 80.0, lp[1] as f64 + 20.0];
    let what = worlds_editor::viewport::describe_cell(&r.doc, h);
    assert!(what.contains("locked_door"), "{what}");
    assert_eq!(
        worlds_editor::viewport::entity_at(&r.doc, &r.view, ix, h.world_px),
        Some(e.id.clone())
    );
}

// ---- disk changes ----

#[test]
fn entity_files_on_disk_apply_as_one_undoable_entry() {
    let mut r = Rig::new();
    let dir = r.dir.clone();
    let path = dir.join("entities/locked_door.json");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("Locked Door", "Vault Door")).unwrap();
    // Only entity files: the editor's path; anything else reloads.
    let rels = entity_files(&dir, std::slice::from_ref(&path)).unwrap();
    assert_eq!(rels, [PathBuf::from("entities/locked_door.json")]);
    assert!(entity_files(&dir, &[path.clone(), dir.join("layers.json")]).is_none());
    assert!(entity_files(&dir, &[dir.join("worlds/main/world.json")]).is_none());
    let before = r.bake_ptrs();
    let d = disk_entity_command(&r.doc, &rels);
    assert!(d.problems.is_empty());
    r.apply(d.command.unwrap()).unwrap();
    for f in &d.files {
        r.doc.project.mark_clean(f);
    }
    r.ed.sync_unsaved(&r.doc.project);
    assert_eq!(
        r.doc.project.entity_def("locked_door").unwrap().name,
        "Vault Door"
    );
    assert_eq!(r.last_label(), "Disk: entities/locked_door.json");
    assert!(!r.ed.has_unsaved(), "disk already holds it");
    assert_eq!(r.bake_ptrs(), before);
    // Undo: the old definition, and the file is dirty again (a save writes
    // it back).
    r.undo();
    assert_eq!(
        r.doc.project.entity_def("locked_door").unwrap().name,
        "Locked Door"
    );
    assert!(r.ed.has_unsaved());
    assert!(
        r.doc
            .project
            .files
            .dirty()
            .contains(Path::new("entities/locked_door.json"))
    );
    r.redo();
    // A file the same as the project changes nothing.
    std::fs::write(&path, &text).unwrap();
    r.undo();
    let d = disk_entity_command(&r.doc, &rels);
    assert!(d.command.is_none() && d.problems.is_empty());
    // A deleted file removes the type, keeping its instances.
    std::fs::remove_file(dir.join("entities/floor_button.json")).unwrap();
    let rels = entity_files(&dir, &[dir.join("entities/floor_button.json")]).unwrap();
    let d = disk_entity_command(&r.doc, &rels);
    r.apply(d.command.unwrap()).unwrap();
    assert!(r.doc.project.entity_def("floor_button").is_none());
    assert_eq!(r.of_type("crevasse", "floor_button").len(), 1);
    // A broken file: a problem, the old definition stays.
    std::fs::write(&path, "{ \"$schema\": \"x\", \"format\": 1, \"slug\": 3 }").unwrap();
    let rels = entity_files(&dir, std::slice::from_ref(&path)).unwrap();
    let d = disk_entity_command(&r.doc, &rels);
    assert!(d.command.is_none());
    assert_eq!(d.problems.len(), 1);
    assert_eq!(d.problems[0].file, "entities/locked_door.json");
    assert!(d.problems[0].message.contains("old definition stays"));
    // Two files: one entry.
    std::fs::write(&path, text.replace("Locked Door", "A")).unwrap();
    std::fs::write(
        dir.join("entities/gem.json"),
        r##"{ "$schema": "../schema/entity.json", "format": 1, "slug": "gem", "name": "Gem",
            "size": [1, 1], "color": "#40c0c0" }"##,
    )
    .unwrap();
    let rels = entity_files(&dir, &[path.clone(), dir.join("entities/gem.json")]).unwrap();
    let d = disk_entity_command(&r.doc, &rels);
    let e0 = r.entries();
    r.apply(d.command.unwrap()).unwrap();
    assert_eq!(r.entries(), e0 + 1);
    assert_eq!(r.last_label(), "Disk: 2 entity files");
    assert!(r.doc.project.entity_def("gem").is_some());
}

// ---- synthetic entities ----

#[test]
fn synthetic_entities_are_deterministic_and_cover_every_field_form() {
    let make = |levels: Option<usize>| {
        let r = Rig::new();
        let mut p = r.doc.project.clone();
        if let Some(n) = levels {
            worlds_editor::doc::synthesize(&mut p, n);
        }
        let atlas = worlds_editor::doc::SynthAtlas::of_size([256, 256]);
        worlds_editor::doc::synthesize_entities(&mut p, 37, Some(&atlas));
        p
    };
    let a = make(None);
    let b = make(None);
    let files = |p: &worlds_core::Project| worlds_core::format::render_files(p).unwrap();
    assert_eq!(files(&a), files(&b), "same N, same bytes");
    let syn: Vec<&worlds_core::EntityDef> = a
        .entities
        .iter()
        .filter(|d| d.slug.as_str().starts_with("syn_"))
        .collect();
    assert_eq!(syn.len(), 37);
    let mut forms = std::collections::BTreeSet::new();
    for d in &syn {
        assert!((3..=14).contains(&d.fields.len()));
        assert_eq!(d.category.as_deref().unwrap().split('/').count(), 3);
        assert!((1..=3).contains(&d.tags.len()));
        assert!(d.icon.is_some());
        for f in &d.fields {
            forms.insert((f.ty.name(), f.array));
        }
    }
    assert_eq!(forms.len(), 24, "every type, single and array: {forms:?}");
    let enums: std::collections::BTreeSet<bool> = syn
        .iter()
        .flat_map(|d| &d.fields)
        .filter_map(|f| match &f.ty {
            worlds_core::FieldType::Enum(worlds_core::EnumSource::Project(_)) => Some(true),
            worlds_core::FieldType::Enum(worlds_core::EnumSource::Inline(_)) => Some(false),
            _ => None,
        })
        .collect();
    assert_eq!(enums.len(), 2, "project and inline enums");
    // ceil(37 / 4) = 10 per level, in bounds, unique valid ids.
    let mut ids = std::collections::HashSet::new();
    for l in a.levels() {
        let syn: Vec<&EntityInstance> = l
            .all_entities()
            .filter(|e| e.ty.as_str().starts_with("syn_"))
            .collect();
        assert_eq!(syn.len(), 10, "{}", l.slug);
        for e in syn {
            assert!(e.cell[0] >= 0 && e.cell[1] >= 0);
            assert!((e.cell[0] as u32) < l.size[0] && (e.cell[1] as u32) < l.size[1]);
            assert!(worlds_core::Id::is_valid(e.id.as_str()));
            assert!(ids.insert(e.id.clone()));
        }
    }
    // With --synthetic: every generated level gets its share.
    let c = make(Some(9));
    assert_eq!(c.levels().count(), 9);
    for l in c.levels() {
        assert_eq!(
            l.all_entities()
                .filter(|e| e.ty.as_str().starts_with("syn_"))
                .count(),
            10
        );
    }
}

// ---- the game fixture ----

/// A temp copy of the `game` fixture (definitions only matter here).
fn game() -> Rig {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/game/game.worlds");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("game.worlds");
    copy_dir(&src, &dir);
    Rig::open(tmp, dir)
}

#[test]
fn room_entry_id_field_and_room_exit_resize() {
    let mut r = game();
    let level = r.doc.level(0).slug.to_string();
    // room_entry's field slugged `id` is a field like any other.
    r.gesture(
        json!({ "gesture": "entity", "level": level, "path": [[4, 4]],
        "type": "room_entry", "fields": { "id": "west" } }),
    );
    assert_eq!(r.status_error(), None);
    let e = r.of_type(&level, "room_entry").pop().unwrap();
    assert_eq!(e.fields.get("id"), Some(&json!("west")));
    assert_ne!(e.id.as_str(), "west");
    // room_exit resizes on both axes.
    r.gesture(
        json!({ "gesture": "entity", "level": level, "path": [[6, 2], [8, 3]],
        "type": "room_exit", "shift": true }),
    );
    assert_eq!(r.status_error(), None);
    let e = r.of_type(&level, "room_exit").pop().unwrap();
    assert_eq!(e.size, Some([3, 2]));
    assert_eq!(e.cell, [6, 2], "pivot (0, 0): the top-left cell");
    // The palette lists every type, all uncategorized.
    let (rows, _) =
        worlds_editor::app::entities::build_rows(&r.doc.project, &Default::default(), "", None, 4);
    assert!(rows.iter().any(|row| matches!(row,
        worlds_editor::app::entities::Row::Category { path, count: 10, .. } if path == "Uncategorized")));
}
