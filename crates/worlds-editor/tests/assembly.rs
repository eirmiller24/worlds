//! Assemblies and transforms in the editor without a GPU: the paste
//! ghost's X, Y and R, the tile stamp's, the selection transforms,
//! arranging levels, level-edge snapping, the Library's rows, the save
//! dialog and the thumbnails, world layouts' lattice and strips under
//! those, grouping a placed assembly, through the same functions the keys, menus
//! and `--script` use. On a temp copy of `worlds-core`'s `mini.worlds`
//! (crevasse at (0, 0), 20 x 5 cells of 16; shaft at (320, 80), 6 x 8).

use std::path::Path;

use worlds_core::assembly::{Assembly, AssemblyInfo};
use worlds_core::fragment::{self, CopySelection};
use worlds_core::transform::{ArrangeOp, TransformOp};
use worlds_core::{Command, Flip, Id, Slug};
use worlds_editor::clipboard::PasteGhost;
use worlds_editor::doc::Document;
use worlds_editor::edit::Editing;
use worlds_editor::library::{self, LibraryView, Row, SaveDialog};
use worlds_editor::script::{GestureStep, gesture_events};
use worlds_editor::tools::{CommandContext, EditorCommands, Invalidation, ToolEvent};
use worlds_editor::view::{SelItem, ViewState};

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

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}

fn slug(s: &str) -> Slug {
    Slug::new(s).unwrap()
}

struct Rig {
    doc: Document,
    view: ViewState,
    ed: Editing,
    inv: Vec<Invalidation>,
    _tmp: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
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
        let mut doc = Document::open(&dir).unwrap();
        doc.bake_all_blocking();
        let mut view = ViewState::new();
        view.sync_layers(&doc, None);
        view.active_level = doc.level_ix("crevasse");
        view.active_layer = doc.project.layers.iter().position(|l| l.is_entities());
        view.zoom = 1.0;
        Rig {
            doc,
            view,
            ed: Editing::new(),
            inv: Vec::new(),
            _tmp: tmp,
        }
    }

    fn cx(&mut self) -> CommandContext<'_> {
        CommandContext {
            doc: &mut self.doc,
            view: &mut self.view,
            invalidate: &mut self.inv,
        }
    }

    fn apply(&mut self, cmd: Command) -> Result<(), String> {
        let mut ed = std::mem::take(&mut self.ed);
        let r = ed.apply(&mut self.cx(), cmd);
        self.ed = ed;
        r
    }

    fn events(&mut self, events: &[ToolEvent]) {
        let mut ed = std::mem::take(&mut self.ed);
        ed.tool_events(&mut self.cx(), events);
        self.ed = ed;
    }

    fn gesture(&mut self, json: &str) {
        let g: GestureStep = serde_json::from_str(json).unwrap();
        let plan = gesture_events(
            &self.doc,
            &g,
            self.view.active_layer,
            self.view.entities.brush.as_ref(),
        )
        .unwrap();
        if let Some(b) = plan.brush {
            let l = self.doc.project.layers[plan.layer].slug.to_string();
            self.view.palette.brushes.insert(l, b);
        }
        self.view.tool = worlds_editor::script::parse_tool(&g.gesture).unwrap();
        self.view.active_level = Some(plan.level);
        self.view.active_layer = Some(plan.layer);
        self.events(&plan.events);
    }

    fn label(&self) -> String {
        self.ed
            .history()
            .undo_label()
            .unwrap_or_default()
            .to_string()
    }

    fn entity_cell(&self, s: &str) -> [i32; 2] {
        self.doc.find_entity(&id(s)).unwrap().2.cell
    }
}

fn pair() -> Vec<SelItem> {
    vec![
        SelItem::Entity(id("btncrev222")),
        SelItem::Entity(id("doorcrev22")),
    ]
}

#[test]
fn the_ghost_turns_and_lands_turned() {
    let mut r = Rig::new();
    r.view.selection.set(pair());
    let c = worlds_editor::clipboard::copy_selection(&r.doc, &r.view).unwrap();
    let mut g = PasteGhost::new(c.fragment.clone());
    let crev = r.doc.level_ix("crevasse").unwrap();
    g.at = Some((crev, [2, 1]));
    g.turn(&r.doc.project, Flip::X).unwrap();
    assert_eq!(g.transform.describe(), "mirrored left-right");
    let plan = g.plan(&r.doc, crev, [2, 1]).unwrap();
    r.apply(plan.command()).unwrap();
    // The door (6 cells right of the button) is on the left now.
    let door = plan.ids[&id("doorcrev22")].clone();
    let btn = plan.ids[&id("btncrev222")].clone();
    assert_eq!(r.entity_cell(door.as_str()), [2, 1]);
    assert_eq!(r.entity_cell(btn.as_str()), [8, 1]);
    // A quarter turn swaps the size; the middle stays put.
    let size = g.fragment().size;
    g.turn(&r.doc.project, Flip::XT).unwrap();
    assert_eq!(g.fragment().size, [size[1], size[0]]);
    assert_eq!(g.at, Some((crev, [5, -2])));
    // X twice is as copied.
    let mut h = PasteGhost::new(c.fragment.clone());
    h.turn(&r.doc.project, Flip::X).unwrap();
    h.turn(&r.doc.project, Flip::X).unwrap();
    assert_eq!(h.transform.flip, None);
    assert_eq!(*h.fragment(), c.fragment);
}

#[test]
fn an_assembly_ghost_is_labelled_place_assembly() {
    let mut r = Rig::new();
    r.view.selection.set(pair());
    let c = worlds_editor::clipboard::copy_selection(&r.doc, &r.view).unwrap();
    let a = Assembly::new(
        AssemblyInfo {
            slug: slug("doorway"),
            name: "Doorway".into(),
            category: None,
            tags: vec![],
            doc: None,
        },
        c.fragment,
    );
    r.apply(Command::SetAssembly {
        assembly: a.clone(),
    })
    .unwrap();
    let g = PasteGhost::assembly(slug("doorway"), a.fragment());
    let crev = r.doc.level_ix("crevasse").unwrap();
    let plan = g.plan(&r.doc, crev, [2, 1]).unwrap();
    r.apply(plan.command()).unwrap();
    assert_eq!(r.label(), "Place assembly doorway");
}

#[test]
fn the_tile_stamp_turns_with_flip_bits() {
    let mut r = Rig::new();
    // decor_back: 8 px cells, castle_main 16 px tiles (stride 2). A 2x1
    // stamp, mirrored: the right tile goes left, both flipped x.
    r.view.brush_flip = Some(Flip::X);
    r.gesture(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "decor_back", "path": [[0, 6]],
             "brush": { "tileset": "castle_main", "rect": [3, 4, 2, 1] } }"#,
    );
    let t = &r
        .doc
        .project
        .level("crevasse")
        .unwrap()
        .tiles_layer("decor_back")
        .unwrap()
        .tiles;
    let at = |c: [u32; 2]| t.iter().find(|p| p.cell == c).cloned().unwrap();
    assert_eq!((at([0, 6]).tile, at([0, 6]).flip), ([4, 4], Some(Flip::X)));
    assert_eq!((at([2, 6]).tile, at([2, 6]).flip), ([3, 4], Some(Flip::X)));
    // A quarter turn: the 2x1 stamp stands up, 1x2.
    r.view.brush_flip = Some(Flip::XT);
    r.gesture(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "decor_back", "path": [[10, 0]] }"#,
    );
    let t = &r
        .doc
        .project
        .level("crevasse")
        .unwrap()
        .tiles_layer("decor_back")
        .unwrap()
        .tiles;
    let at = |c: [u32; 2]| t.iter().find(|p| p.cell == c).cloned();
    assert_eq!(
        at([10, 0]).map(|p| (p.tile, p.flip)),
        Some(([3, 4], Some(Flip::XT)))
    );
    assert_eq!(
        at([10, 2]).map(|p| (p.tile, p.flip)),
        Some(([4, 4], Some(Flip::XT)))
    );
    assert!(at([12, 0]).is_none());
}

#[test]
fn rotating_a_stamp_of_tall_tiles_is_refused() {
    let mut r = Rig::new();
    let mut ts = r.doc.project.tileset("castle_main").unwrap().clone();
    ts.tile_size = [16, 32];
    r.apply(Command::SetTilesetDef {
        tileset: ts.slug.clone(),
        def: ts,
        index: None,
    })
    .unwrap();
    r.view.brush_flip = Some(Flip::XT);
    let before = r.doc.project.clone();
    r.gesture(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "decor_back", "path": [[0, 6]],
             "brush": { "tileset": "castle_main", "rect": [3, 4, 1, 1] } }"#,
    );
    let s = r.view.status.as_ref().unwrap();
    assert!(s.error && s.text.contains("square tiles"), "{}", s.text);
    assert_eq!(r.doc.project, before);
}

#[test]
fn selection_transforms_keep_ids_and_say_what_looks_directional() {
    let mut r = Rig::new();
    r.view.selection.set(pair());
    let t = worlds_editor::arrange::plan_transform(&r.doc, &r.view, TransformOp::MirrorX).unwrap();
    assert!(t.message.contains("locked_door.facing"), "{}", t.message);
    r.apply(t.command).unwrap();
    assert_eq!(r.label(), "Mirror 2 items left-right");
    assert_eq!(r.entity_cell("doorcrev22"), [12, 3]);
    assert_eq!(r.entity_cell("btncrev222"), [18, 3]);
    assert_eq!(t.selection, pair());
    // Nothing selected: the status says what to select.
    r.view.selection.clear();
    let e =
        worlds_editor::arrange::plan_transform(&r.doc, &r.view, TransformOp::Rotate90).unwrap_err();
    assert!(e.starts_with("Nothing to turn"), "{e}");
}

#[test]
fn a_grid_marquee_rotates_and_follows() {
    let mut r = Rig::new();
    let crev = r.doc.level_ix("crevasse").unwrap();
    r.view.active_layer = r
        .doc
        .project
        .layers
        .iter()
        .position(|l| l.slug == "collision");
    r.view.grid_marquee = Some(worlds_editor::clipboard::GridMarquee {
        level: r.doc.ids[crev].clone(),
        layer: slug("collision"),
        rect: worlds_core::command::Rect::new(8, 2, 2, 1),
    });
    let t = worlds_editor::arrange::plan_transform(&r.doc, &r.view, TransformOp::Rotate90).unwrap();
    r.apply(t.command).unwrap();
    let m = t.marquee.unwrap();
    assert_eq!(<[u32; 4]>::from(m.rect), [9, 2, 1, 2]);
}

#[test]
fn arranging_needs_two_levels_and_is_one_entry() {
    let mut r = Rig::new();
    let crev = r.doc.level_ix("crevasse").unwrap();
    let shaft = r.doc.level_ix("shaft").unwrap();
    r.view
        .selection
        .set([SelItem::Level(r.doc.ids[shaft].clone())]);
    let e =
        worlds_editor::arrange::plan_arrange(&r.doc, &r.view, ArrangeOp::AlignLeft).unwrap_err();
    assert!(e.contains("two or more levels"), "{e}");
    r.view.selection.set([
        SelItem::Level(r.doc.ids[crev].clone()),
        SelItem::Level(r.doc.ids[shaft].clone()),
    ]);
    let cmd = worlds_editor::arrange::plan_arrange(&r.doc, &r.view, ArrangeOp::AlignTop).unwrap();
    r.apply(cmd).unwrap();
    assert_eq!(r.label(), "Align 2 levels top");
    assert_eq!(r.doc.project.level("shaft").unwrap().world_px, [320, 0]);
}

#[test]
fn a_level_drag_snaps_to_other_levels_edges() {
    let r = Rig::new();
    let shaft = r.doc.level_ix("shaft").unwrap();
    // The shaft's left edge (320) dragged 5 px short of crevasse's right
    // edge... it is already on it; drag its top (80) to 5 px below
    // crevasse's top (0): snaps to 0 within 8 screen px at zoom 1.
    let (d, hits) = worlds_editor::annot::drag_delta(&r.doc, &r.view, &[shaft], [0.0, -75.0], true);
    assert_eq!(d, [0, -80]);
    assert!(hits.iter().any(|h| h.axis == 1 && h.at == 0), "{hits:?}");
    // Further than 8 px: no snap (pixel moves, Ctrl).
    let (d, _) = worlds_editor::annot::drag_delta(&r.doc, &r.view, &[shaft], [0.0, -60.0], true);
    assert_eq!(d, [0, -60]);
    // With guide snapping off, level edges still snap.
    let mut view = r.view.clone();
    view.annotations.snap_to_guides = false;
    let (d, _) = worlds_editor::annot::drag_delta(&r.doc, &view, &[shaft], [-3.0, 0.0], true);
    assert_eq!(
        d,
        [0, 0],
        "the shaft's left edge stays on crevasse's right edge"
    );
}

#[test]
fn the_library_has_search_favorites_and_categories() {
    let mut r = Rig::new();
    let frag = fragment::copy(
        &r.doc.project,
        &CopySelection {
            entities: vec![id("btncrev222")],
            ..Default::default()
        },
    )
    .unwrap();
    for (s, name, cat) in [
        ("doorway", "Doorway", Some("Puzzle/Doors")),
        ("ledge", "Ledge", None),
        ("gate", "Gate", Some("Puzzle/Doors")),
    ] {
        r.apply(Command::SetAssembly {
            assembly: Assembly::new(
                AssemblyInfo {
                    slug: slug(s),
                    name: name.into(),
                    category: cat.map(String::from),
                    tags: vec![],
                    doc: None,
                },
                frag.clone(),
            ),
        })
        .unwrap();
    }
    let mut lib = LibraryView::default();
    let rows = library::rows(&r.doc.project, &lib, "");
    let paths: Vec<String> = rows
        .iter()
        .filter_map(|r| match r {
            Row::Category { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(paths, ["Puzzle", "Puzzle/Doors", "Uncategorized"]);
    lib.toggle_favorite(&slug("ledge"));
    let rows = library::rows(&r.doc.project, &lib, "gat");
    let items: Vec<&Slug> = rows
        .iter()
        .filter_map(|r| match r {
            Row::Item { slug, .. } => Some(slug),
            _ => None,
        })
        .collect();
    assert_eq!(items, [&slug("gate")]);
    let rows = library::rows(&r.doc.project, &lib, "");
    assert!(matches!(&rows[1], Row::Item { slug, .. } if slug == "ledge"));
    // A rename is followed.
    lib.renamed(&slug("ledge"), &slug("shelf"));
    assert_eq!(lib.favorites, [slug("shelf")]);
}

#[test]
fn the_save_dialog_checks_slug_name_and_replace() {
    let r = Rig::new();
    let mut d = SaveDialog {
        slug: "Door Way".into(),
        name: "x".into(),
        ..Default::default()
    };
    assert!(d.info(&r.doc.project).unwrap_err().contains("isn't a slug"));
    d.slug = "doorway".into();
    d.name = " ".into();
    assert!(d.info(&r.doc.project).is_err());
    d.name = "Doorway".into();
    d.tags = "door, , puzzle".into();
    d.category = "Puzzle/Doors".into();
    let (info, replace) = d.info(&r.doc.project).unwrap();
    assert_eq!(info.tags, ["door", "puzzle"]);
    assert_eq!(info.category.as_deref(), Some("Puzzle/Doors"));
    assert!(!replace);
}

#[test]
fn the_save_dialog_is_filled_in_from_the_selection() {
    let mut r = Rig::new();
    let copy = |r: &Rig, sel: CopySelection| fragment::copy(&r.doc.project, &sel).unwrap();
    // One entity type: its slug, name, category and tags.
    let one = copy(
        &r,
        CopySelection {
            level: Some(slug("crevasse")),
            entities: vec![id("doorcrev22")],
            ..Default::default()
        },
    );
    let d = SaveDialog::suggest(&r.doc.project, &one);
    assert_eq!(
        (
            d.slug.as_str(),
            d.name.as_str(),
            d.category.as_str(),
            d.tags.as_str()
        ),
        (
            "locked_door",
            "Locked Door",
            "Puzzle/Doors",
            "object, puzzle"
        )
    );
    // Two types: both slugs, "A and B", no category (they differ), all tags.
    let two = copy(
        &r,
        CopySelection {
            level: Some(slug("crevasse")),
            entities: vec![id("doorcrev22"), id("btncrev222")],
            ..Default::default()
        },
    );
    let d = SaveDialog::suggest(&r.doc.project, &two);
    assert_eq!(d.slug, "locked_door_floor_button");
    assert_eq!(d.name, "Locked Door and Floor Button");
    assert_eq!(d.category, "");
    assert_eq!(d.tags, "object, puzzle");
    // Tiles alone: the layer's name, the tilesets as tags.
    let tiles = copy(
        &r,
        CopySelection {
            level: Some(slug("crevasse")),
            tiles: [(slug("decor_back"), vec![[4, 1], [4, 2], [5, 2]])]
                .into_iter()
                .collect(),
            ..Default::default()
        },
    );
    assert!(!tiles.tiles.is_empty());
    let d = SaveDialog::suggest(&r.doc.project, &tiles);
    assert_eq!(
        (d.slug.as_str(), d.name.as_str()),
        ("decor_back", "Decor (back)")
    );
    assert!(
        d.tags.contains("castle_main") && d.tags.contains("ssw_set5"),
        "{}",
        d.tags
    );
    // A taken slug counts up, so Save needs no Replace by accident.
    let (info, _) = d.info(&r.doc.project).unwrap();
    r.apply(Command::SetAssembly {
        assembly: Assembly::new(info, tiles.clone()),
    })
    .unwrap();
    assert_eq!(
        SaveDialog::suggest(&r.doc.project, &tiles).slug,
        "decor_back_2"
    );
    // And the suggestion is always a valid save.
    assert!(
        SaveDialog::suggest(&r.doc.project, &two)
            .info(&r.doc.project)
            .is_ok()
    );
}

#[test]
fn the_turn_glyph_turns_as_a_tile_does() {
    use worlds_editor::arrange::glyph_strokes;
    let upright = glyph_strokes(None);
    let at = |s: &Vec<Vec<[f32; 2]>>, i: usize, j: usize| s[i][j];
    // X mirrors every point left-right.
    let x = glyph_strokes(Some(Flip::X));
    for (i, s) in upright.iter().enumerate() {
        for (j, p) in s.iter().enumerate() {
            let q = at(&x, i, j);
            assert!((q[0] - (1.0 - p[0])).abs() < 1e-6 && (q[1] - p[1]).abs() < 1e-6);
        }
    }
    // R (xt) is a quarter turn clockwise: (x, y) -> (1 - y, x).
    let r = glyph_strokes(Some(Flip::XT));
    for (i, s) in upright.iter().enumerate() {
        for (j, p) in s.iter().enumerate() {
            let q = at(&r, i, j);
            assert!((q[0] - (1.0 - p[1])).abs() < 1e-6 && (q[1] - p[0]).abs() < 1e-6);
        }
    }
    // The eight turns are eight different pictures.
    let all: Vec<_> = [
        None,
        Some(Flip::X),
        Some(Flip::Y),
        Some(Flip::XY),
        Some(Flip::T),
        Some(Flip::XT),
        Some(Flip::YT),
        Some(Flip::XYT),
    ]
    .into_iter()
    .map(|f| format!("{:?}", glyph_strokes(f)))
    .collect();
    for (i, a) in all.iter().enumerate() {
        assert!(all[..i].iter().all(|b| b != a));
    }
}

#[test]
fn thumbnails_are_rendered_once_into_the_cache() {
    let r = Rig::new();
    let frag = fragment::copy(
        &r.doc.project,
        &CopySelection {
            level: Some(slug("crevasse")),
            rect: Some(worlds_core::command::Rect::new(0, 0, 6, 5)),
            layers: vec![slug("collision"), slug("decor_back")],
            ..Default::default()
        },
    )
    .unwrap();
    let a = Assembly::new(
        AssemblyInfo {
            slug: slug("corner"),
            name: "Corner".into(),
            category: None,
            tags: vec![],
            doc: None,
        },
        frag,
    );
    use worlds_editor::thumbs::{ASSEMBLY_MAX_SIDE, AssemblyThumb, Budget, Source, Thumbs};
    // No frames run here (one egui pass throughout): no budget.
    let ctx = egui::Context::default();
    let mut thumbs = Thumbs::default();
    thumbs.budget = Budget::UNLIMITED;
    let src = AssemblyThumb {
        doc: &r.doc,
        assembly: &a,
    };
    let tex = thumbs.get(&ctx, &src).unwrap();
    assert!(tex.size()[0] as u32 <= ASSEMBLY_MAX_SIDE && tex.size()[1] as u32 <= ASSEMBLY_MAX_SIDE);
    let path = src.file().unwrap().path();
    assert!(path.exists(), "{}", path.display());
    assert!(path.starts_with(r.doc.dir.join(".worlds/thumbs")));
    // The solid cell at [0, 0] is the collision layer's color.
    let img = image::open(&path).unwrap().to_rgba8();
    let c = img.get_pixel(1, 1).0;
    assert!(c[0] < 0x60 && c[1] < 0x60, "{c:?}");
    // The next session reads the file instead of drawing it.
    let mut next = Thumbs::default();
    let _ = next.get(&ctx, &src).unwrap();
    assert_eq!((thumbs.rendered, next.rendered, next.loaded), (1, 0, 1));
    // An edited assembly gets a new file; the old one goes.
    let mut b = a.clone();
    b.name = "Corner 2".into();
    let src_b = AssemblyThumb {
        doc: &r.doc,
        assembly: &b,
    };
    let _ = thumbs.get(&ctx, &src_b).unwrap();
    assert!(!path.exists());
    assert!(src_b.file().unwrap().path().exists());
    assert_eq!((thumbs.rendered, thumbs.len()), (2, 1));
}

/// Level thumbnails: one pixel per cell, the void transparent, and keyed
/// per level, so painting one level redraws only its picture.
#[test]
fn level_thumbnails_are_keyed_per_level_and_cut_the_void() {
    use worlds_editor::thumbs::{LayerLook, LevelThumb, Source, Thumbs};
    let mut r = Rig::new();
    r.apply(Command::SetLevelVoid {
        level: slug("crevasse"),
        void: vec![worlds_core::command::Rect::new(0, 0, 3, 2)],
        restore: false,
    })
    .unwrap();
    let ctx = egui::Context::default();
    let mut thumbs = Thumbs::default();
    let crev = r.doc.level_ix("crevasse").unwrap();
    let shaft = r.doc.level_ix("shaft").unwrap();
    let look = LayerLook::of(&r.doc, &r.view);
    let key = |doc: &Document, ix| {
        LevelThumb {
            doc,
            ix,
            look: &look,
        }
        .key()
    };
    let (k_crev, k_shaft) = (key(&r.doc, crev), key(&r.doc, shaft));
    // One pixel per cell (20 x 5), memory only, the void cut out.
    let src = LevelThumb {
        doc: &r.doc,
        ix: crev,
        look: &look,
    };
    assert!(src.file().is_none());
    let img = src.render(&mut Default::default());
    assert_eq!(img.dimensions(), (20, 5));
    assert_eq!(img.get_pixel(1, 1).0[3], 0, "void is transparent");
    assert_eq!(img.get_pixel(3, 1).0[3], 255);
    assert_eq!(img.get_pixel(1, 2).0[3], 255);
    let _ = thumbs.get(&ctx, &src).unwrap();
    // Painting crevasse moves its key only.
    let fill: Command = serde_json::from_str(
        r#"{ "op": "fill_rect", "level": "crevasse", "layer": "collision", "rect": [5, 1, 3, 1], "value": "water" }"#,
    )
    .unwrap();
    r.apply(fill).unwrap();
    assert_ne!(key(&r.doc, crev), k_crev);
    assert_eq!(key(&r.doc, shaft), k_shaft);
    // Hiding a layer moves every key (what the pictures show changed).
    let mut hidden = r.view.clone();
    hidden.layers[0].visible = false;
    let look2 = LayerLook::of(&r.doc, &hidden);
    assert_ne!(
        LevelThumb {
            doc: &r.doc,
            ix: shaft,
            look: &look2
        }
        .key(),
        k_shaft
    );
    // A project setting may touch every level: both move.
    let (k_crev, k_shaft) = (key(&r.doc, crev), key(&r.doc, shaft));
    let seed: Command = serde_json::from_str(
        r#"{ "op": "set_project_setting", "setting": { "key": "seed", "value": 1234 } }"#,
    )
    .unwrap();
    r.apply(seed).unwrap();
    assert_ne!(key(&r.doc, crev), k_crev);
    assert_ne!(key(&r.doc, shaft), k_shaft);
    // A bake arriving moves its level's key.
    let k = key(&r.doc, shaft);
    r.doc.bake_all_blocking();
    assert_ne!(key(&r.doc, shaft), k);
}

#[test]
fn a_random_brush_turns_each_alternative() {
    let mut r = Rig::new();
    // One alternative, a 2x1 block: mirrored, its right tile goes left and
    // both tiles flip x, as for a stamp.
    r.view.palette.brushes.insert(
        "decor_back".into(),
        worlds_editor::tools::Brush::Random {
            tileset: slug("castle_main"),
            alternatives: vec![worlds_core::TileAlt::rect(3, 4, 2, 1)],
        },
    );
    r.view.brush_flip = Some(Flip::X);
    r.gesture(
        r#"{ "gesture": "brush", "level": "crevasse", "layer": "decor_back", "path": [[0, 6]] }"#,
    );
    let t = &r
        .doc
        .project
        .level("crevasse")
        .unwrap()
        .tiles_layer("decor_back")
        .unwrap()
        .tiles;
    let at = |c: [u32; 2]| t.iter().find(|p| p.cell == c).cloned().unwrap();
    assert_eq!((at([0, 6]).tile, at([0, 6]).flip), ([4, 4], Some(Flip::X)));
    assert_eq!((at([2, 6]).tile, at([2, 6]).flip), ([3, 4], Some(Flip::X)));
}

#[test]
fn edges_snap_only_within_the_shown_world_and_a_world_switch_drops_the_ghost() {
    let mut r = Rig::new();
    // Guides off: the world's wall guide has a vertex at crevasse's top-left
    // corner, which would explain the snap on its own.
    r.view.annotations.snap_to_guides = false;
    let crev = r.doc.level_ix("crevasse").unwrap();
    // Crevasse's bottom (80) moved 5 px down lands back on the shaft's top.
    let (d, _) = worlds_editor::annot::drag_delta(&r.doc, &r.view, &[crev], [0.0, 5.0], true);
    assert_eq!(d, [0, 0]);
    r.apply(Command::CreateWorld {
        slug: slug("under"),
        name: None,
    })
    .unwrap();
    r.apply(Command::MoveLevelToWorld {
        level: slug("shaft"),
        world: slug("under"),
        world_px: None,
        keep_layout: false,
    })
    .unwrap();
    let crev = r.doc.level_ix("crevasse").unwrap();
    let (d, hits) = worlds_editor::annot::drag_delta(&r.doc, &r.view, &[crev], [0.0, 5.0], true);
    assert_eq!(d, [0, 5], "the shaft is in another world now: {hits:?}");
    // An armed assembly ghost belongs to the map it was armed on.
    r.view.active_level = Some(crev);
    r.view.paste = Some(PasteGhost::assembly(
        slug("doorway"),
        worlds_core::fragment::Fragment {
            worlds_fragment: 1,
            grid_size: 16,
            size: [1, 1],
            source: None,
            grids: Default::default(),
            tiles: Default::default(),
            entities: vec![],
            notes: vec![],
        },
    ));
    let under = r
        .doc
        .project
        .worlds
        .iter()
        .position(|w| w.slug == "under")
        .unwrap();
    r.view.set_world(&r.doc, under);
    assert!(r.view.paste.is_none());
}

#[test]
fn the_history_lists_a_placed_assembly_with_its_file() {
    let mut r = Rig::new();
    r.view.selection.set(pair());
    let c = worlds_editor::clipboard::copy_selection(&r.doc, &r.view).unwrap();
    let a = Assembly::new(
        AssemblyInfo {
            slug: slug("doorway"),
            name: "Doorway".into(),
            category: None,
            tags: vec![],
            doc: None,
        },
        c.fragment,
    );
    r.apply(Command::SetAssembly {
        assembly: a.clone(),
    })
    .unwrap();
    let g = PasteGhost::assembly(slug("doorway"), a.fragment());
    let crev = r.doc.level_ix("crevasse").unwrap();
    r.apply(g.plan(&r.doc, crev, [2, 1]).unwrap().command())
        .unwrap();
    let entries = r.ed.history().undo_entries();
    let e = entries.last().unwrap();
    assert_eq!(e.label, "Place assembly doorway");
    assert!(
        e.files
            .iter()
            .any(|f| f.ends_with("worlds/main/levels/crevasse/entities.json")),
        "{:?}",
        e.files
    );
    let saved = &entries[entries.len() - 2];
    assert!(
        saved
            .files
            .iter()
            .any(|f| f.ends_with("assemblies/doorway.json")),
        "{:?}",
        saved.files
    );
}

fn set_layout(r: &mut Rig, mode: worlds_core::LayoutMode, cell: Option<[u32; 2]>) {
    r.apply(Command::SetWorldLayout {
        world: slug("main"),
        layout: worlds_core::WorldLayout {
            mode,
            gap: None,
            cell,
        },
        restore: false,
    })
    .unwrap();
}

#[test]
fn the_lattice_wins_over_edge_snaps_and_strips_refuse_arranging() {
    use worlds_core::LayoutMode;
    let mut r = Rig::new();
    let shaft = r.doc.level_ix("shaft").unwrap();
    let crev = r.doc.level_ix("crevasse").unwrap();
    // Free: 5 px down snaps the shaft's top back onto crevasse's bottom (80).
    let (d, hits) = worlds_editor::annot::drag_delta(&r.doc, &r.view, &[shaft], [0.0, 5.0], true);
    assert_eq!(d, [0, 0]);
    assert!(hits.iter().any(|h| h.axis == 1 && h.at == 80), "{hits:?}");
    // Grid vania 64 px: the edge snap's 80 isn't a lattice point; the
    // lattice step comes last and puts the top on 64, dropping that hit.
    set_layout(&mut r, LayoutMode::GridVania, Some([64, 64]));
    let (d, hits) = worlds_editor::annot::drag_delta(&r.doc, &r.view, &[shaft], [0.0, 5.0], true);
    assert_eq!(d, [0, -16]);
    assert!(hits.iter().all(|h| h.axis != 1), "{hits:?}");
    assert!(worlds_editor::annot::strip_refusal(&r.doc, shaft).is_none());
    // Arranging in grid vania lands on the lattice.
    r.view.selection.set([
        SelItem::Level(r.doc.ids[crev].clone()),
        SelItem::Level(r.doc.ids[shaft].clone()),
    ]);
    let cmd = worlds_editor::arrange::plan_arrange(&r.doc, &r.view, ArrangeOp::AlignTop).unwrap();
    r.apply(cmd).unwrap();
    assert_eq!(r.doc.project.level("shaft").unwrap().world_px, [320, 0]);
    // A strip: drags refused, arranging refused with the reason.
    set_layout(&mut r, LayoutMode::Horizontal, None);
    assert!(worlds_editor::annot::strip_refusal(&r.doc, shaft).is_some());
    let e =
        worlds_editor::arrange::plan_arrange(&r.doc, &r.view, ArrangeOp::AlignLeft).unwrap_err();
    assert!(e.contains("horizontal layout"), "{e}");
}

#[test]
fn a_placed_assembly_can_be_grouped() {
    let mut r = Rig::new();
    r.view.selection.set(pair());
    let c = worlds_editor::clipboard::copy_selection(&r.doc, &r.view).unwrap();
    let a = Assembly::new(
        AssemblyInfo {
            slug: slug("doorway"),
            name: "Doorway".into(),
            category: None,
            tags: vec![],
            doc: None,
        },
        c.fragment,
    );
    r.apply(Command::SetAssembly {
        assembly: a.clone(),
    })
    .unwrap();
    let before: Vec<Id> = r
        .doc
        .project
        .level("crevasse")
        .unwrap()
        .all_entities()
        .map(|e| e.id.clone())
        .collect();
    let g = PasteGhost::assembly(slug("doorway"), a.fragment());
    let crev = r.doc.level_ix("crevasse").unwrap();
    r.apply(g.plan(&r.doc, crev, [2, 1]).unwrap().command())
        .unwrap();
    let placed: Vec<Id> = r
        .doc
        .project
        .level("crevasse")
        .unwrap()
        .all_entities()
        .map(|e| e.id.clone())
        .filter(|i| !before.contains(i))
        .collect();
    assert_eq!(placed.len(), 2);
    // The placed copies plus one of the originals: one group.
    let mut sel: Vec<SelItem> = placed.iter().cloned().map(SelItem::Entity).collect();
    sel.push(SelItem::Entity(id("btncrev222")));
    r.view.selection.set(sel);
    let cmd = worlds_editor::app::groups::group_command(&r.doc, &r.view).unwrap();
    r.apply(cmd).unwrap();
    let l = r.doc.project.level("crevasse").unwrap();
    assert_eq!(l.groups.len(), 1);
    assert_eq!(l.groups[0].members.len(), 3);
    assert!(placed.iter().all(|i| l.groups[0].members.contains(i)));
    assert!(
        worlds_core::validate(&r.doc.project)
            .iter()
            .all(|p| !p.path.starts_with("groups")),
        "{:?}",
        worlds_core::validate(&r.doc.project)
    );
}
