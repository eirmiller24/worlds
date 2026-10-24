//! Dock panels other than the viewport.

use egui::{Color32, RichText, Sense, Stroke, StrokeKind, Vec2};
use worlds_core::LayerKind;

use worlds_core::Command;

use super::EditorState;
use super::actions::Action;
use crate::tools::Brush;
use crate::viewport::scene::parse_color;

/// A derived layer's palette after its values: `Suppress (!)` and what the
/// tools do there.
fn derived_palette_tail(ui: &mut egui::Ui, view: &mut crate::view::ViewState, slug: &str) {
    let selected = view.palette.brush(slug) == Some(&Brush::Suppress);
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
    } else if resp.hovered() {
        p.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let sw = egui::Rect::from_min_size(rect.min + Vec2::new(4.0, 2.0), Vec2::splat(20.0));
    p.rect_stroke(
        sw,
        2.0,
        Stroke::new(1.0, Color32::from_gray(150)),
        StrokeKind::Inside,
    );
    let grey = Color32::from_gray(175);
    let (a, b) = (sw.shrink(5.0).min, sw.shrink(5.0).max);
    p.line_segment([a, b], Stroke::new(2.0, grey));
    p.line_segment(
        [egui::pos2(b.x, a.y), egui::pos2(a.x, b.y)],
        Stroke::new(2.0, grey),
    );
    p.text(
        sw.right_center() + Vec2::new(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "Suppress (!)",
        egui::FontId::proportional(13.0),
        ui.visuals().text_color(),
    );
    let resp = resp.on_hover_text("Override cells to empty, whatever the rules put there");
    if resp.clicked() {
        view.palette
            .brushes
            .insert(slug.to_string(), Brush::Suppress);
    }
    ui.add_space(4.0);
    ui.weak(
        "Derived layer: painting sets overrides (solid, outlined); the eraser removes them \
         (back to the rules, drawn faint). Fill floods what the map shows. Alt+click picks \
         the stored override.",
    );
}

fn rgb(c: [u8; 4]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Layer list, top layer first: visibility, lock, active, opacity.
pub fn layers(ui: &mut egui::Ui, st: &mut EditorState) {
    let Some(doc) = &st.doc else {
        ui.weak("No project");
        return;
    };
    // Adding and editing layers is the Project pane's (`project_pane`).
    let mut edit: Option<Option<worlds_core::Slug>> = None;
    if doc.project.layers.is_empty() {
        ui.label("No layers yet.");
        ui.weak("A layer is what you paint on: start with an int_grid layer for collision.");
        if ui
            .button("+ Add a layer…")
            .on_hover_text("The Project tab's Layers")
            .clicked()
        {
            edit = Some(None);
        }
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for i in (0..doc.project.layers.len()).rev() {
                let def = &doc.project.layers[i];
                let Some(v) = st.view.layers.get_mut(i) else {
                    continue;
                };
                let active = st.view.active_layer == Some(i) && !st.view.references.active;
                ui.horizontal(|ui| {
                    let eye = RichText::new(if v.visible { "👁" } else { "–" }).size(15.0);
                    ui.toggle_value(&mut v.visible, eye)
                        .on_hover_text("Visible");
                    let lock = if v.locked {
                        RichText::new("🔒").size(15.0)
                    } else {
                        RichText::new("🔓").size(15.0).weak()
                    };
                    ui.toggle_value(&mut v.locked, lock)
                        .on_hover_text("Locked: painting tools skip this layer");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().slider_width = 64.0;
                        ui.add(egui::Slider::new(&mut v.opacity, 0.0..=1.0).show_value(false))
                            .on_hover_text(format!(
                                "View opacity {:.0}% (definition: {:.0}%)",
                                v.opacity * 100.0,
                                def.opacity * 100.0
                            ));
                        // Name takes what's left, truncated.
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                            let name = RichText::new(&def.name);
                            let r = ui.selectable_label(active, name).on_hover_text(format!(
                                "{} ({}). Double click or right click to edit it",
                                def.slug.as_str(),
                                def.kind.name()
                            ));
                            if r.clicked() {
                                st.view.active_layer = Some(i);
                                st.view.references.active = false;
                            }
                            if r.double_clicked() {
                                edit = Some(Some(def.slug.clone()));
                            }
                            r.context_menu(|ui| {
                                if ui.button("Edit layer…").clicked() {
                                    edit = Some(Some(def.slug.clone()));
                                    ui.close();
                                }
                                ui.separator();
                                layer_ops_menu(ui, doc, &st.view.selection, def, &mut st.actions);
                            });
                            // A derived layer says so where the kind goes.
                            if def.is_derived() {
                                derived_badge(ui, doc, def, st.accent);
                            } else {
                                ui.weak(def.kind.name());
                            }
                        });
                    });
                });
            }
            if !doc.project.layers.is_empty() {
                ui.add_space(4.0);
                if ui
                    .small_button("Edit layers…")
                    .on_hover_text("Add, remove, reorder and edit layers: the Project tab")
                    .clicked()
                {
                    edit = Some(
                        st.view
                            .active_layer
                            .and_then(|i| doc.project.layers.get(i).map(|l| l.slug.clone())),
                    );
                }
            }
        });
    super::references::layers_row(ui, st);
    match edit {
        Some(Some(slug)) => st.project.show_layer(slug),
        Some(None) => st.project.show(super::project_pane::Section::Layers),
        None => {}
    }
}

/// The layer operations of a layer's right-click menu: duplicate, merge
/// down (greyed out, saying why, unless the layer below is a tiles layer on
/// the same grid), move the selection here (tile stacks to a tiles layer,
/// entities to an entities layer).
fn layer_ops_menu(
    ui: &mut egui::Ui,
    doc: &crate::doc::Document,
    selection: &crate::view::Selection,
    def: &worlds_core::LayerDef,
    actions: &mut Vec<Action>,
) {
    let slug = def.slug.clone();
    if ui
        .button("Duplicate layer")
        .on_hover_text(format!(
            "A copy of {} and its content in every level, right above it",
            def.name
        ))
        .clicked()
    {
        actions.push(Action::DuplicateLayer(Some(slug.clone())));
        ui.close();
    }
    if def.is_tiles() {
        let target = worlds_core::command::layer_ops::merge_target(&doc.project, &slug);
        let r = ui.add_enabled(target.is_ok(), egui::Button::new("Merge down"));
        let r = match &target {
            Ok(lower) => r.on_hover_text(format!(
                "Put its tiles on top of {}'s in every level, then remove it",
                lower.name
            )),
            Err(e) => r.on_disabled_hover_text(e.to_string()),
        };
        if r.clicked() {
            actions.push(Action::MergeLayerDown(Some(slug.clone())));
            ui.close();
        }
    }
    let n = if def.is_tiles() {
        selection.tiles().filter(|(_, l, _)| **l != slug).count()
    } else if def.is_entities() {
        selection
            .entities()
            .filter(|id| doc.find_entity(id).is_some_and(|(_, l, _)| *l != slug))
            .count()
    } else {
        return;
    };
    let what = match (def.is_tiles(), n == 1) {
        (true, true) => "tile stack",
        (true, false) => "tile stacks",
        (false, true) => "entity",
        (false, false) => "entities",
    };
    let text = if n == 0 {
        "Move selection here".to_string()
    } else {
        format!("Move {n} selected {what} here")
    };
    let r = ui
        .add_enabled(n > 0, egui::Button::new(text))
        .on_disabled_hover_text(if def.is_tiles() {
            "Select tile stacks on another tiles layer first"
        } else {
            "Select entities on another entities layer first"
        });
    if r.clicked() {
        actions.push(Action::MoveSelectionToLayer(Some(slug)));
        ui.close();
    }
}

/// The `derived` badge of a derived int_grid layer, with its writer.
fn derived_badge(
    ui: &mut egui::Ui,
    doc: &crate::doc::Document,
    def: &worlds_core::LayerDef,
    accent: Color32,
) {
    let tip = match doc.project.derived_writer(def.slug.as_str()) {
        Some(w) => format!("computed by {w}'s rules; painting sets overrides"),
        None => "no auto layer's rules write it yet; painting sets overrides".to_string(),
    };
    egui::Frame::NONE
        .stroke(Stroke::new(1.0, accent))
        .corner_radius(3.0)
        .inner_margin(egui::Margin::symmetric(3, 0))
        .show(ui, |ui| {
            ui.label(RichText::new("derived").small().color(accent));
        })
        .response
        .on_hover_text(tip);
}

/// Palette for the active layer.
pub fn palette(ui: &mut egui::Ui, st: &mut EditorState) {
    if st.doc.is_some() && crate::library::switch(ui, &mut st.view) {
        crate::library::library(ui, st);
        return;
    }
    let Some(doc) = &st.doc else {
        ui.weak("No project");
        return;
    };
    let Some(li) = st.view.active_layer else {
        if doc.project.layers.is_empty() {
            ui.weak("No layers yet: the palette shows what the active layer can hold.");
            if ui.button("+ Add a layer…").clicked() {
                st.project.show(super::project_pane::Section::Layers);
            }
        } else {
            ui.weak("No active layer: click one in Layers.");
        }
        return;
    };
    let def = &doc.project.layers[li];
    match &def.kind {
        LayerKind::IntGrid(g) if g.values.is_empty() => {
            ui.weak(format!("{} has no values yet.", def.name));
            if ui.button("Add values…").clicked() {
                st.project.show_layer(def.slug.clone());
            }
        }
        LayerKind::IntGrid(_) => {
            let slug = def.slug.to_string();
            if !st.view.palette.brushes.contains_key(&slug) && !def.values().is_empty() {
                st.view
                    .palette
                    .brushes
                    .insert(slug.clone(), Brush::Value(1));
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (i, v) in def.values().iter().enumerate() {
                        let idx = i as u16 + 1;
                        let selected = st.view.palette.brush(&slug) == Some(&Brush::Value(idx));
                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 24.0),
                            Sense::click(),
                        );
                        let p = ui.painter();
                        if selected {
                            p.rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
                        } else if resp.hovered() {
                            p.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
                        }
                        let sw = egui::Rect::from_min_size(
                            rect.min + Vec2::new(4.0, 2.0),
                            Vec2::splat(20.0),
                        );
                        let c = rgb(parse_color(v.color.as_str()));
                        p.rect_filled(sw, 2.0, c);
                        p.rect_stroke(
                            sw,
                            2.0,
                            Stroke::new(1.0, Color32::from_black_alpha(120)),
                            StrokeKind::Inside,
                        );
                        let text_col = if c.intensity() > 0.5 {
                            Color32::BLACK
                        } else {
                            Color32::WHITE
                        };
                        p.text(
                            sw.center(),
                            egui::Align2::CENTER_CENTER,
                            v.glyph,
                            egui::FontId::monospace(13.0),
                            text_col,
                        );
                        p.text(
                            sw.right_center() + Vec2::new(8.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            &v.name,
                            egui::FontId::proportional(13.0),
                            ui.visuals().text_color(),
                        );
                        if resp.clicked() {
                            st.view
                                .palette
                                .brushes
                                .insert(slug.clone(), Brush::Value(idx));
                        }
                    }
                    if def.is_derived() {
                        derived_palette_tail(ui, &mut st.view, &slug);
                    }
                });
        }
        LayerKind::Tiles(td) => {
            let slug = def.slug.to_string();
            let tilesets = &doc.project.tilesets;
            if tilesets.is_empty() {
                ui.weak("The project has no tilesets.");
                if ui
                    .button("+ Add a tileset…")
                    .on_hover_text("The Project tab's Tilesets: pick a PNG")
                    .clicked()
                {
                    st.project.show(super::project_pane::Section::Tilesets);
                }
                return;
            }
            let default = td
                .tileset
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_else(|| tilesets[0].slug.to_string());
            let current = st
                .palette_tab
                .entry(slug.clone())
                .or_insert(default)
                .clone();
            egui::ScrollArea::horizontal()
                .id_salt("tileset tabs")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for t in tilesets {
                            if ui
                                .selectable_label(current == t.slug.as_str(), t.slug.as_str())
                                .clicked()
                            {
                                st.palette_tab.insert(slug.clone(), t.slug.to_string());
                            }
                        }
                    });
                });
            ui.separator();
            let Some(ts) = doc.project.tileset(&current) else {
                return;
            };
            random_section(
                ui,
                &mut st.view,
                &mut st.edit_bufs,
                &mut st.palette_tab,
                &slug,
            );
            let tex = atlas_texture(&mut st.atlas, ui.ctx(), doc, ts);
            let Some(tex) = tex else {
                ui.colored_label(
                    Color32::LIGHT_RED,
                    "Tileset image failed to load (see log).",
                );
                return;
            };
            let img = tex.size_vec2();
            egui::ScrollArea::both()
                .id_salt("atlas")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let scale = (ui.available_width() / img.x).floor().clamp(1.0, 4.0);
                    let size = img * scale;
                    let resp =
                        ui.add(egui::Image::new((tex.id(), size)).sense(Sense::click_and_drag()));
                    let to_tile = |p: egui::Pos2| {
                        let local = (p - resp.rect.min) / scale;
                        ts.tile_at_px([local.x.max(0.0) as u32, local.y.max(0.0) as u32])
                    };
                    let pointer = resp.interact_pointer_pos();
                    if resp.drag_started() || resp.clicked() {
                        st.atlas_drag = pointer.map(to_tile);
                    }
                    let random = matches!(st.view.palette.brush(&slug), Some(Brush::Random { .. }));
                    if random && (resp.drag_started() || resp.clicked()) {
                        // A random brush keeps what it had before this drag
                        // (Ctrl and Shift add to it).
                        st.random_base = st.view.palette.brush(&slug).cloned();
                    }
                    if let (Some(a), Some(p)) = (st.atlas_drag, pointer)
                        && (resp.dragged() || resp.drag_stopped() || resp.clicked())
                    {
                        let b = to_tile(p);
                        let rect = [
                            a[0].min(b[0]),
                            a[1].min(b[1]),
                            a[0].abs_diff(b[0]) + 1,
                            a[1].abs_diff(b[1]) + 1,
                        ];
                        let m = ui.input(|i| i.modifiers);
                        let brush = if random {
                            random_pick(st.random_base.as_ref(), ts.slug.clone(), rect, m)
                        } else {
                            Brush::Tiles {
                                tileset: ts.slug.clone(),
                                rect,
                            }
                        };
                        st.view.palette.brushes.insert(slug.clone(), brush);
                    }
                    if resp.drag_stopped() || resp.clicked() {
                        st.atlas_drag = None;
                    }
                    let p = ui.painter_at(resp.rect);
                    let outline = |rect: [u32; 4]| {
                        let a = ts.tile_px([rect[0], rect[1]]);
                        let b = ts.tile_px([rect[0] + rect[2] - 1, rect[1] + rect[3] - 1]);
                        egui::Rect::from_min_max(
                            resp.rect.min + Vec2::new(a[0] as f32, a[1] as f32) * scale,
                            resp.rect.min
                                + Vec2::new(
                                    (b[0] + ts.tile_size[0]) as f32,
                                    (b[1] + ts.tile_size[1]) as f32,
                                ) * scale,
                        )
                    };
                    match st.view.palette.brush(&slug) {
                        Some(Brush::Tiles { tileset, rect }) if tileset == &ts.slug => {
                            p.rect_stroke(
                                outline(*rect),
                                0.0,
                                Stroke::new(2.0, st.accent),
                                StrokeKind::Outside,
                            );
                        }
                        // A random brush: every alternative's tiles, dashed
                        // look by a thinner stroke and a tint.
                        Some(Brush::Random {
                            tileset,
                            alternatives,
                        }) if tileset == &ts.slug => {
                            for a in alternatives {
                                let rects: Vec<[u32; 4]> = match a.as_rect() {
                                    Some(r) => vec![r],
                                    None => a.tiles.iter().map(|t| [t[0], t[1], 1, 1]).collect(),
                                };
                                for r in rects {
                                    let r = outline(r);
                                    p.rect_filled(r, 0.0, st.accent.gamma_multiply(0.18));
                                    p.rect_stroke(
                                        r,
                                        0.0,
                                        Stroke::new(1.5, st.accent),
                                        StrokeKind::Inside,
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                    if let Some(h) = resp.hover_pos() {
                        let t = to_tile(h);
                        resp.on_hover_text(format!("[{}, {}]", t[0], t[1]));
                    }
                });
        }
        LayerKind::Auto(a) => {
            ui.label(format!(
                "Auto layer: tiles come from the rules in rules/{}.json, read from \"{}\".",
                def.slug.as_str(),
                a.source.as_str()
            ));
            let layer = def.slug.clone();
            super::rules::palette_list(ui, st, &layer);
        }
        LayerKind::Entities => super::entities::palette(ui, st),
    }
}

/// The random brush an atlas drag over `rect` makes: each tile of it one
/// alternative, replacing `base`'s (a plain drag), added to them (Ctrl),
/// or the whole rect added as one stamp alternative (Shift). A `base` of
/// another tileset is replaced.
pub fn random_pick(
    base: Option<&Brush>,
    tileset: worlds_core::Slug,
    rect: [u32; 4],
    m: egui::Modifiers,
) -> Brush {
    let mut alternatives = match base {
        Some(Brush::Random {
            tileset: t,
            alternatives,
        }) if *t == tileset && (m.shift || crate::tools::pixel_mod(m)) => alternatives.clone(),
        _ => Vec::new(),
    };
    let fresh: Vec<worlds_core::TileAlt> = if m.shift {
        vec![worlds_core::TileAlt::rect(
            rect[0], rect[1], rect[2], rect[3],
        )]
    } else {
        match Brush::random_of_rect(tileset.clone(), rect) {
            Brush::Random { alternatives, .. } => alternatives,
            _ => Vec::new(),
        }
    };
    for a in fresh {
        if !alternatives.iter().any(|b| b.tiles == a.tiles) {
            alternatives.push(a);
        }
    }
    Brush::Random {
        tileset,
        alternatives,
    }
}

/// The palette's random brush part, above the atlas of tiles layer
/// `layer`: the current random brush's alternatives with their weights
/// (`×` drops one) and Save as preset; then the saved presets (a click
/// makes one the brush, `×` forgets it).
fn random_section(
    ui: &mut egui::Ui,
    view: &mut crate::view::ViewState,
    edit_bufs: &mut std::collections::HashMap<String, String>,
    palette_tab: &mut std::collections::HashMap<String, String>,
    layer: &str,
) {
    let presets = &mut view.palette.presets;
    let brush = view.palette.brushes.get_mut(layer);
    let mut load = None;
    if let Some(Brush::Random {
        tileset,
        alternatives,
    }) = brush
    {
        ui.horizontal(|ui| {
            ui.strong("Random");
            ui.weak(format!(
                "{} alternative{} from {tileset}",
                alternatives.len(),
                if alternatives.len() == 1 { "" } else { "s" }
            ));
        });
        let mut remove = None;
        egui::Grid::new("random alternatives")
            .num_columns(3)
            .show(ui, |ui| {
                for (i, a) in alternatives.iter_mut().enumerate() {
                    let what = match (a.as_rect(), a.tiles.as_slice()) {
                        (Some([c, r, 1, 1]), _) => format!("[{c}, {r}]"),
                        (Some([c, r, w, h]), _) => format!("[{c}, {r}] {w}x{h}"),
                        (None, t) => format!("stack of {}", t.len()),
                    };
                    ui.label(what);
                    let r = ui
                        .add(
                            egui::DragValue::new(&mut a.weight)
                                .range(0..=1000)
                                .prefix("w "),
                        )
                        .on_hover_text("Weight: picked in proportion to it (0: never)");
                    super::fields::mark(ui, &format!("random:weight:{i}"), r.rect);
                    if ui.small_button("×").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        if let Some(i) = remove {
            alternatives.remove(i);
        }
        ui.horizontal(|ui| {
            let name = edit_bufs.entry("random:preset_name".into()).or_default();
            let r = ui.add(
                egui::TextEdit::singleline(name)
                    .hint_text("preset name")
                    .desired_width(110.0),
            );
            super::fields::mark(ui, "random:preset_name", r.rect);
            let ok = !name.trim().is_empty() && !alternatives.is_empty();
            let b = ui.add_enabled(ok, egui::Button::new("Save preset"));
            super::fields::mark(ui, "random:save", b.rect);
            if b.clicked() {
                let name = name.trim().to_string();
                let p = crate::tools::BrushPreset {
                    name: name.clone(),
                    tileset: tileset.clone(),
                    alternatives: alternatives.clone(),
                };
                match presets.iter_mut().find(|q| q.name == name) {
                    Some(q) => *q = p,
                    None => presets.push(p),
                }
                edit_bufs.remove("random:preset_name");
            }
        });
    }
    if !presets.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.weak("Presets");
            let mut forget = None;
            for (i, p) in presets.iter().enumerate() {
                let r = ui.button(&p.name).on_hover_text(format!(
                    "{} alternatives from {} (right click: forget)",
                    p.alternatives.len(),
                    p.tileset
                ));
                super::fields::mark(ui, &format!("random:preset:{}", p.name), r.rect);
                if r.clicked() {
                    load = Some(i);
                }
                if r.secondary_clicked() {
                    forget = Some(i);
                }
            }
            if let Some(i) = forget {
                presets.remove(i);
            }
        });
    }
    if let Some(i) = load
        && let Some(p) = view.palette.presets.get(i).cloned()
    {
        palette_tab.insert(layer.to_string(), p.tileset.to_string());
        view.palette.brushes.insert(
            layer.to_string(),
            Brush::Random {
                tileset: p.tileset,
                alternatives: p.alternatives,
            },
        );
    }
    ui.separator();
}

/// The egui texture of a tileset's atlas (nearest sampling), loaded on
/// first use and cached by slug in `atlas` (`None`: it failed to load,
/// logged once). Shared by the palette and the Rules pane.
pub(crate) fn atlas_texture(
    atlas: &mut std::collections::HashMap<String, Option<egui::TextureHandle>>,
    ctx: &egui::Context,
    doc: &crate::doc::Document,
    ts: &worlds_core::TilesetDef,
) -> Option<egui::TextureHandle> {
    atlas
        .entry(ts.slug.to_string())
        .or_insert_with(|| {
            let path = doc.dir.join(&ts.image);
            match image::open(&path) {
                Ok(img) => {
                    let img = img.to_rgba8();
                    let ci = egui::ColorImage::from_rgba_unmultiplied(
                        [img.width() as usize, img.height() as usize],
                        img.as_raw(),
                    );
                    Some(ctx.load_texture(
                        format!("atlas {}", ts.slug),
                        ci,
                        egui::TextureOptions::NEAREST,
                    ))
                }
                Err(e) => {
                    tracing::error!("cannot load {}: {e}", path.display());
                    None
                }
            }
        })
        .clone()
}

/// The Inspector's Resize section: a new size in project cells, a 3x3
/// anchor picker (the dot stays put in the world; the default is the
/// center, as for `resize_level`), Resize, and Crop to content. A resize
/// that would leave an entity outside is refused in the status bar.
fn level_resize(
    ui: &mut egui::Ui,
    doc: &crate::doc::Document,
    view: &mut crate::view::ViewState,
    pending: &mut Vec<Command>,
    bufs: &mut std::collections::HashMap<String, String>,
    ix: crate::doc::LevelIx,
) {
    let l = doc.level(ix);
    let world = &doc.project.worlds[doc.levels[ix].world].slug;
    let key_size = format!("{}:resize", l.id);
    let key_anchor = format!("{}:anchor", l.id);
    let pair = |k: &str| -> Option<[i64; 2]> {
        let (a, b) = bufs.get(k)?.split_once(',')?;
        Some([a.parse().ok()?, b.parse().ok()?])
    };
    let mut size = pair(&key_size).map_or(l.size, |p| p.map(|n| n.clamp(1, 100_000) as u32));
    let mut anchor = pair(&key_anchor).map_or([0, 0], |p| p.map(|n| n.clamp(-1, 1) as i8));
    ui.add_space(8.0);
    ui.strong("Resize");
    ui.horizontal(|ui| {
        anchor_picker(ui, &mut anchor);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut size[0]).range(1..=100_000));
                ui.label("x");
                ui.add(egui::DragValue::new(&mut size[1]).range(1..=100_000));
                ui.weak("cells");
            });
            ui.horizontal(|ui| {
                let b = ui
                    .add_enabled(size != l.size, egui::Button::new("Resize"))
                    .on_hover_text(
                        "The anchor (the dot) stays put in the world; content keeps its place",
                    );
                super::fields::mark(ui, "level:resize", b.rect);
                if b.clicked() {
                    match crate::tools::resize_command(&doc.project, world, &l.slug, size, anchor) {
                        Ok(cmd) => {
                            pending.push(cmd);
                            size = l.size;
                        }
                        Err(e) => view.set_status(e, true),
                    }
                }
                let b = ui
                    .button("Crop to content")
                    .on_hover_text("Shrink to the bounding box of the cells, tiles and entities");
                super::fields::mark(ui, "level:crop", b.rect);
                if b.clicked() {
                    match crate::tools::crop_command(&doc.project, world, &l.slug) {
                        Ok(cmd) => pending.push(cmd),
                        Err(e) => view.set_status(e, true),
                    }
                }
            });
        });
    });
    if size == l.size {
        bufs.remove(&key_size);
    } else {
        bufs.insert(key_size, format!("{},{}", size[0], size[1]));
    }
    bufs.insert(key_anchor, format!("{},{}", anchor[0], anchor[1]));
}

/// A 3x3 grid of buttons as in image editors' canvas size dialogs: the
/// dot marks the anchor, what stays put; arrows point where the level
/// grows.
pub fn anchor_picker(ui: &mut egui::Ui, anchor: &mut [i8; 2]) {
    let names = |a: [i8; 2]| -> String {
        let x = ["left", "center", "right"][(a[0] + 1) as usize];
        let y = ["top", "middle", "bottom"][(a[1] + 1) as usize];
        match (a[0], a[1]) {
            (0, 0) => "the center".to_string(),
            (_, 0) => format!("the {x} edge"),
            (0, _) => format!("the {y} edge"),
            _ => format!("the {y}-{x} corner"),
        }
    };
    // Drawn, not glyphs: the default fonts lack some arrows.
    egui::Grid::new(ui.id().with("anchor_picker"))
        .spacing([2.0, 2.0])
        .show(ui, |ui| {
            for ay in -1i8..=1 {
                for ax in -1i8..=1 {
                    let d = [ax - anchor[0], ay - anchor[1]];
                    let on = d == [0, 0];
                    let (rect, b) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), Sense::click());
                    let b = b.on_hover_text(format!("Keep {} in place", names([ax, ay])));
                    let v = ui.style().interact_selectable(&b, on);
                    let p = ui.painter();
                    p.rect(rect, 3.0, v.bg_fill, v.bg_stroke, StrokeKind::Inside);
                    let c = rect.center();
                    if on {
                        p.circle_filled(c, 4.0, v.fg_stroke.color);
                    } else if d[0].abs() <= 1 && d[1].abs() <= 1 {
                        let dir = egui::vec2(d[0] as f32, d[1] as f32).normalized();
                        p.arrow(
                            c - dir * 5.0,
                            dir * 10.0,
                            Stroke::new(1.5, v.fg_stroke.color),
                        );
                    }
                    super::fields::mark(ui, &format!("level:anchor:{ax},{ay}"), b.rect);
                    if b.clicked() {
                        *anchor = [ax, ay];
                    }
                }
                ui.end_row();
            }
        });
}

/// The active level's part of the Inspector (or the project's when no
/// level is active): rename, delete, and the level fields, every type
/// editable through [`super::fields`] (`×` removes the stored value, `null`
/// stores null).
pub fn level_inspector(ui: &mut egui::Ui, st: &mut EditorState) {
    let EditorState {
        doc,
        view,
        pending,
        edit_bufs: bufs,
        atlas,
        actions,
        ..
    } = st;
    let Some(doc) = doc.as_ref() else {
        return;
    };
    let Some(ix) = view.active_level.filter(|&i| i < doc.levels.len()) else {
        ui.heading(&doc.project.name);
        egui::Grid::new("project")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                ui.weak("Directory");
                ui.label(doc.dir.display().to_string());
                ui.end_row();
                ui.weak("Grid");
                ui.label(format!("{} px", doc.project.grid_size));
                ui.end_row();
                ui.weak("Levels");
                ui.label(doc.levels.len().to_string());
                ui.end_row();
                if doc.project.worlds.len() > 1 {
                    ui.weak("Worlds");
                    ui.label(doc.project.worlds.len().to_string());
                    ui.end_row();
                }
                ui.weak("Layers");
                ui.label(doc.project.layers.len().to_string());
                ui.end_row();
                ui.weak("Tilesets");
                ui.label(doc.project.tilesets.len().to_string());
                ui.end_row();
                ui.weak("Entity types");
                ui.label(doc.project.entities.len().to_string());
                ui.end_row();
                ui.weak("Export on save");
                ui.label(if doc.project.export_on_save {
                    "yes"
                } else {
                    "no"
                });
                ui.end_row();
            });
        ui.add_space(8.0);
        ui.weak("Click a level to inspect it.");
        return;
    };
    let l = doc.level(ix);
    let world = &doc.project.worlds[doc.levels[ix].world].slug;
    let g = doc.project.grid_size;
    ui.heading(if l.name.is_empty() {
        l.slug.as_str()
    } else {
        &l.name
    });

    // Rename: slug and name, committed together.
    let key_slug = format!("{}:slug", l.id);
    let key_name = format!("{}:name", l.id);
    egui::Grid::new("level")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.weak("Slug");
            let b = bufs
                .entry(key_slug.clone())
                .or_insert_with(|| l.slug.to_string());
            ui.text_edit_singleline(b);
            ui.end_row();
            ui.weak("Name");
            let b = bufs
                .entry(key_name.clone())
                .or_insert_with(|| l.name.clone());
            ui.text_edit_singleline(b);
            ui.end_row();
            let mut row = |k: &str, v: String| {
                ui.weak(k);
                ui.label(v);
                ui.end_row();
            };
            row("World", world.to_string());
            row("Id", l.id.as_str().to_string());
            row("World px", format!("{}, {}", l.world_px[0], l.world_px[1]));
            row(
                "Size",
                format!(
                    "{} x {} cells ({} x {} px)",
                    l.size[0],
                    l.size[1],
                    l.size[0] * g,
                    l.size[1] * g
                ),
            );
            row("Depth", l.depth.to_string());
            row(
                "Background",
                l.bg_color
                    .as_ref()
                    .map_or("project default".into(), |c| c.to_string()),
            );
            let n: usize = l.entities.iter().map(|e| e.entities.len()).sum();
            row("Entities", n.to_string());
            if let Some(b) = &doc.bakes[ix] {
                let n: usize = b.values().map(|b| b.placements.len()).sum();
                row("Baked tiles", n.to_string());
            }
        });
    super::note_inspector::level_tags_row(ui, doc, pending, bufs, ix);
    let new_slug = bufs[&key_slug].trim().to_string();
    let new_name = bufs[&key_name].clone();
    let changed = new_slug != l.slug.as_str() || new_name != l.name;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(changed, egui::Button::new("Rename"))
            .on_hover_text("Renames the level directory too; note links follow")
            .clicked()
        {
            match worlds_core::Slug::new(&new_slug) {
                Ok(slug) => {
                    pending.push(Command::RenameLevel {
                        world: world.clone(),
                        level: l.slug.clone(),
                        slug,
                        name: new_name,
                    });
                }
                Err(e) => view.set_status(format!("Bad slug: {e}"), true),
            }
            bufs.remove(&key_slug);
            bufs.remove(&key_name);
        }
        if changed && ui.button("Revert").clicked() {
            bufs.remove(&key_slug);
            bufs.remove(&key_name);
        }
        if ui
            .button("Duplicate")
            .on_hover_text("A copy just right of it (Ctrl+D with the level selected)")
            .clicked()
        {
            actions.push(Action::DuplicateLevel(ix));
        }
        if ui.button("Delete level…").clicked() {
            actions.push(Action::DeleteLevel);
        }
    });
    level_resize(ui, doc, view, pending, bufs, ix);

    if !doc.project.level_fields.is_empty() {
        ui.add_space(8.0);
        ui.strong("Fields");
        let mut fx = super::fields::FieldCx {
            doc,
            bufs,
            atlas,
            view,
            level: Some(ix),
            pick_cells: true,
            point_grid: doc.project.grid_size,
        };
        let problems = super::fields::field_problems(
            &doc.problems,
            &format!("levels/{}/level.json", l.slug),
            "fields.",
            true,
        );
        egui::Grid::new("fields")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                for f in &doc.project.level_fields {
                    super::fields::field_label(ui, f);
                    let cur = l.fields.get(&f.slug);
                    let key = format!("{}:field:{}", l.id, f.slug);
                    if let Some(edit) = super::fields::field_row_noted(
                        ui,
                        &mut fx,
                        f,
                        cur,
                        &key,
                        true,
                        problems.get(f.slug.as_str()),
                    ) {
                        let value = match edit {
                            super::fields::FieldEdit::Set(v) => Some(v),
                            super::fields::FieldEdit::Remove => None,
                        };
                        super::inspector::push(
                            pending,
                            format!("Set {} on {}", f.slug, l.slug),
                            vec![Command::SetLevelField {
                                world: world.clone(),
                                level: l.slug.clone(),
                                field: f.slug.clone(),
                                value,
                                allow_unknown: false,
                            }],
                        );
                    }
                    ui.end_row();
                }
            });
    }
    if !l.layers.is_empty() {
        ui.add_space(8.0);
        ui.strong("Rule state");
        for (layer, s) in &l.layers {
            ui.label(format!(
                "{}: seed {}{}",
                layer.as_str(),
                s.seed,
                if s.optional_groups.is_empty() {
                    String::new()
                } else {
                    format!(
                        ", optional groups {}",
                        s.optional_groups
                            .iter()
                            .map(|g| g.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            ));
        }
    }
}

/// A color button. The picker changes the color every frame of a drag, so
/// the picked color waits in `bufs` while the popup is open and is returned
/// once, when it closes (and only if it differs from `stored`): one undo
/// entry per pick, like the text fields.
pub(crate) fn color_field(
    ui: &mut egui::Ui,
    bufs: &mut std::collections::HashMap<String, String>,
    key: &str,
    stored: Option<&str>,
) -> Option<String> {
    // The id `color_edit_button_srgb` gives its popup.
    let popup_id = ui.auto_id_with("popup");
    let c = crate::viewport::scene::parse_color(
        bufs.get(key).map_or(stored.unwrap_or("#000000"), |s| s),
    );
    let mut rgb = [c[0], c[1], c[2]];
    if ui.color_edit_button_srgb(&mut rgb).changed() {
        let hex = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
        bufs.insert(key.to_string(), hex);
    }
    if egui::Popup::is_id_open(ui.ctx(), popup_id) {
        return None;
    }
    bufs.remove(key)
        .filter(|hex| stored.is_none_or(|s| !s.eq_ignore_ascii_case(hex)))
}

/// What clicking a problem goes to.
#[derive(Debug, Clone, PartialEq)]
pub enum ProblemTarget {
    /// An entity (`...entities[<id>]...`): select it and frame its level.
    Entity(worlds_core::Id),
    /// An entity type's file (`entities/<slug>.json`): open it in Types.
    Type(worlds_core::Slug),
    /// `project.json`'s `level_fields`: the Types pane's Level fields.
    LevelFields,
    /// Something in a level's directory: make it active and frame it.
    Level(String),
}

/// What a problem names, most specific first.
pub fn problem_target(p: &worlds_core::Problem) -> Option<ProblemTarget> {
    if let Some(rest) = p.path.split("entities[").nth(1)
        && let Some(id) = rest.split(']').next()
        && let Ok(id) = worlds_core::Id::new(id)
    {
        return Some(ProblemTarget::Entity(id));
    }
    if let Some(stem) = p
        .file
        .strip_prefix("entities/")
        .and_then(|f| f.strip_suffix(".json"))
        && let Ok(slug) = worlds_core::Slug::new(stem)
    {
        return Some(ProblemTarget::Type(slug));
    }
    if p.file == "project.json" && p.path.starts_with("level_fields") {
        return Some(ProblemTarget::LevelFields);
    }
    // The level directory may be in `file` or in `path`.
    let full = format!("{}/{}", p.file, p.path);
    let rest = full.split("levels/").nth(1)?;
    rest.split('/')
        .next()
        .map(|s| ProblemTarget::Level(s.to_string()))
}

/// Validation problems, kept up to date while editing (see the README,
/// "Live problems"). Clicking one that names an entity selects it and
/// frames its level; one naming an entity type opens it in the Types pane;
/// one in a level's directory frames that level.
pub fn problems(ui: &mut egui::Ui, st: &mut EditorState) {
    let Some(doc) = &st.doc else {
        ui.weak("No project");
        return;
    };
    if doc.problems.is_empty() {
        ui.weak("No problems.");
        return;
    }
    let mut clicked = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for p in &doc.problems {
                let text = if p.path.is_empty() {
                    format!("{}: {}", p.file, p.message)
                } else {
                    format!("{} {}: {}", p.file, p.path, p.message)
                };
                let r = ui.add(
                    egui::Label::new(RichText::new(text).color(Color32::from_rgb(230, 190, 90)))
                        .sense(Sense::click()),
                );
                if r.clicked() {
                    clicked = problem_target(p);
                }
            }
        });
    match clicked {
        Some(ProblemTarget::Entity(id)) => match doc.find_entity(&id) {
            Some((ix, _, _)) => {
                st.view.active_level = Some(ix);
                st.view.selection.set_entities([id]);
                st.actions.push(Action::FitLevel);
            }
            None => st.view.set_status(format!("No entity {id} any more"), true),
        },
        Some(ProblemTarget::Type(slug)) => st.actions.push(Action::OpenTypes(Some(slug))),
        Some(ProblemTarget::LevelFields) => st.actions.push(Action::OpenTypes(None)),
        Some(ProblemTarget::Level(slug)) => {
            if let Some(ix) = doc.level_ix(&slug) {
                st.view.active_level = Some(ix);
                st.actions.push(Action::FitLevel);
            }
        }
        None => {}
    }
}

/// The tracing ring buffer.
pub fn log(ui: &mut egui::Ui, st: &mut EditorState) {
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("log level")
            .selected_text(st.log_level.as_str())
            .show_ui(ui, |ui| {
                for l in [
                    tracing::Level::DEBUG,
                    tracing::Level::INFO,
                    tracing::Level::WARN,
                    tracing::Level::ERROR,
                ] {
                    ui.selectable_value(&mut st.log_level, l, l.as_str());
                }
            });
        if ui.button("Clear").clicked() {
            st.actions.push(Action::ClearLog);
        }
    });
    let min = st.log_level;
    st.log.with_lines(|lines, _| {
        let shown: Vec<&crate::log::LogLine> = lines.iter().filter(|l| l.level <= min).collect();
        let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show_rows(ui, row_h, shown.len(), |ui, range| {
                for l in &shown[range] {
                    let color = match l.level {
                        tracing::Level::ERROR => Color32::from_rgb(240, 100, 100),
                        tracing::Level::WARN => Color32::from_rgb(230, 190, 90),
                        tracing::Level::INFO => ui.visuals().text_color(),
                        _ => ui.visuals().weak_text_color(),
                    };
                    let secs = l.time.rem_euclid(86400.0);
                    let t = format!(
                        "{:02}:{:02}:{:02}",
                        (secs / 3600.0) as u32,
                        (secs / 60.0) as u32 % 60,
                        secs as u32 % 60
                    );
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("{t} {:5} {}", l.level.as_str(), l.message))
                                .monospace()
                                .color(color),
                        )
                        .extend(),
                    );
                }
            });
    });
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// The palette's random brush: an atlas drag replaces, Ctrl adds tiles,
    /// Shift adds a stamp; the weights, Save preset and a preset's click.
    #[test]
    fn random_brush_in_the_palette() {
        use super::super::testkit::{Driver, state};
        use worlds_core::{Slug, TileAlt};
        let ts = Slug::new("castle_main").unwrap();
        let none = egui::Modifiers::default();
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let alts = |b: &Brush| match b {
            Brush::Random { alternatives, .. } => alternatives.clone(),
            _ => panic!("{b:?}"),
        };
        let a = random_pick(None, ts.clone(), [0, 0, 2, 1], none);
        assert_eq!(alts(&a), [TileAlt::single(0, 0), TileAlt::single(1, 0)]);
        let b = random_pick(Some(&a), ts.clone(), [1, 0, 2, 1], ctrl);
        assert_eq!(
            alts(&b),
            [
                TileAlt::single(0, 0),
                TileAlt::single(1, 0),
                TileAlt::single(2, 0)
            ]
        );
        let c = random_pick(Some(&b), ts.clone(), [4, 4, 2, 2], shift);
        assert_eq!(alts(&c).last(), Some(&TileAlt::rect(4, 4, 2, 2)));
        assert_eq!(
            alts(&random_pick(Some(&c), ts.clone(), [9, 9, 1, 1], none)).len(),
            1
        );

        let (_tmp, mut st) = state();
        let li = st
            .doc
            .as_ref()
            .unwrap()
            .project
            .layers
            .iter()
            .position(|l| l.slug == "decor_back")
            .unwrap();
        st.view.active_layer = Some(li);
        st.view
            .palette
            .brushes
            .insert("decor_back".into(), c.clone());
        let mut d = Driver::new(palette);
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // Name it and save it.
        d.click(&mut st, d.rect("random:preset_name").center());
        d.frame(&mut st, vec![egui::Event::Text("ledges".into())]);
        d.click(&mut st, d.rect("random:save").center());
        assert_eq!(st.view.palette.presets.len(), 1);
        assert_eq!(st.view.palette.presets[0].name, "ledges");
        assert_eq!(st.view.palette.presets[0].alternatives, alts(&c));
        // Another brush, then the preset brings it back.
        st.view.palette.brushes.insert(
            "decor_back".into(),
            Brush::Tiles {
                tileset: ts.clone(),
                rect: [0, 0, 1, 1],
            },
        );
        d.frame(&mut st, vec![]);
        d.click(&mut st, d.rect("random:preset:ledges").center());
        assert_eq!(st.view.palette.brush("decor_back"), Some(&c));
        // The presets go to ui.json and come back.
        let json = serde_json::to_string(&st.view.palette).unwrap();
        let back: crate::tools::PaletteSelection = serde_json::from_str(&json).unwrap();
        assert_eq!(back.presets, st.view.palette.presets);
        // A ui.json palette from before presets: density 100, no presets.
        let old: crate::tools::PaletteSelection =
            serde_json::from_str(r#"{ "brushes": {} }"#).unwrap();
        assert_eq!((old.density, old.presets.len()), (100, 0));
    }

    /// The Inspector's level Resize section: the anchor picker and the size
    /// make one `resize_level`; Crop to content queues the crop batch; a
    /// size that would strand the door is refused in the status bar.
    #[test]
    fn level_resize_with_an_anchor_and_crop() {
        use super::super::testkit::{Driver, batches, state};
        let (_tmp, mut st) = state();
        let ix = st.doc.as_ref().unwrap().level_ix("shaft").unwrap();
        st.view.active_level = Some(ix);
        let id = st.doc.as_ref().unwrap().level(ix).id.clone();
        let mut d = Driver::new(super::super::inspector::inspector);
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        // Bottom-right anchor, then a bigger size.
        d.click(&mut st, d.rect("level:anchor:1,1").center());
        st.edit_bufs.insert(format!("{id}:resize"), "8,9".into());
        d.frame(&mut st, vec![]);
        d.click(&mut st, d.rect("level:resize").center());
        let b = batches(&mut st);
        assert_eq!(b.len(), 1, "{b:?}");
        assert_eq!(
            b[0].1,
            [Command::ResizeLevel {
                world: worlds_core::Slug::new("main").unwrap(),
                level: worlds_core::Slug::new("shaft").unwrap(),
                size: [8, 9],
                anchor: [1, 1],
            }]
        );
        // The size field goes back to the level's own size.
        assert!(!st.edit_bufs.contains_key(&format!("{id}:resize")));
        // Too small from the top-left: the door would be outside.
        d.click(&mut st, d.rect("level:anchor:-1,-1").center());
        st.edit_bufs.insert(format!("{id}:resize"), "2,2".into());
        d.frame(&mut st, vec![]);
        d.click(&mut st, d.rect("level:resize").center());
        assert!(batches(&mut st).is_empty());
        let status = st.view.status.as_ref().unwrap().text.clone();
        assert!(status.contains("doorshaft2"), "{status}");
        d.click(&mut st, d.rect("level:crop").center());
        let b = batches(&mut st);
        assert_eq!(b.len(), 1, "{b:?}");
        assert_eq!(b[0].0, "Crop level shaft");
    }

    /// One egui frame with a color field at the top left; `events` are this
    /// frame's input. Returns what the field committed and the button rect.
    fn frame(
        ctx: &egui::Context,
        t: f64,
        events: Vec<egui::Event>,
        bufs: &mut HashMap<String, String>,
    ) -> (Option<String>, egui::Rect) {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            time: Some(t),
            focused: true,
            events,
            ..Default::default()
        };
        let mut out = (None, egui::Rect::NOTHING);
        let mut full = ctx.run_ui(raw, |ui| {
            let at = ui.next_widget_position();
            out.0 = color_field(ui, bufs, "level:tint", Some("#102030"));
            out.1 = egui::Rect::from_min_size(at, ui.spacing().interact_size);
        });
        full.textures_delta.clear();
        out
    }

    fn click(p: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(p),
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            },
        ]
    }

    #[test]
    fn color_field_commits_once_when_the_picker_closes() {
        let ctx = egui::Context::default();
        let mut bufs = HashMap::new();
        let mut t = 0.0;
        let mut step = |events, bufs: &mut HashMap<String, String>| {
            t += 0.1;
            frame(&ctx, t, events, bufs)
        };
        let (_, button) = step(vec![], &mut bufs);
        let c = button.center();
        step(click(c, true), &mut bufs);
        let (committed, _) = step(click(c, false), &mut bufs);
        assert_eq!(committed, None);
        // Picking (dragging over the picker) only updates the buffer while
        // the popup is open: nothing is committed per frame.
        for hex in ["#ff0000", "#ee0000", "#dd0000"] {
            bufs.insert("level:tint".into(), hex.into());
            let (committed, _) = step(vec![], &mut bufs);
            assert_eq!(committed, None, "{hex}");
        }
        // Clicking outside closes the picker: one commit of the last pick.
        let outside = egui::pos2(700.0, 500.0);
        let mut commits = Vec::new();
        for pressed in [true, false] {
            commits.extend(step(click(outside, pressed), &mut bufs).0);
        }
        commits.extend(step(vec![], &mut bufs).0);
        assert_eq!(commits, ["#dd0000"]);
        assert!(bufs.is_empty());
        // Picking the stored color back commits nothing.
        bufs.insert("level:tint".into(), "#102030".into());
        assert_eq!(step(vec![], &mut bufs).0, None);
        assert!(bufs.is_empty());
    }
}
