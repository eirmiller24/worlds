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
    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for i in (0..doc.project.layers.len()).rev() {
                let def = &doc.project.layers[i];
                let Some(v) = st.view.layers.get_mut(i) else {
                    continue;
                };
                let active = st.view.active_layer == Some(i);
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
                                "{} ({})",
                                def.slug.as_str(),
                                def.kind.name()
                            ));
                            if r.clicked() {
                                st.view.active_layer = Some(i);
                            }
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
        });
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
    let Some(doc) = &st.doc else {
        ui.weak("No project");
        return;
    };
    let Some(li) = st.view.active_layer else {
        ui.weak("No active layer");
        return;
    };
    let def = &doc.project.layers[li];
    match &def.kind {
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
                        st.view.palette.brushes.insert(
                            slug.clone(),
                            Brush::Tiles {
                                tileset: ts.slug.clone(),
                                rect,
                            },
                        );
                    }
                    if resp.drag_stopped() || resp.clicked() {
                        st.atlas_drag = None;
                    }
                    let p = ui.painter_at(resp.rect);
                    if let Some(Brush::Tiles { tileset, rect }) = st.view.palette.brush(&slug)
                        && tileset == &ts.slug
                    {
                        let a = ts.tile_px([rect[0], rect[1]]);
                        let b = ts.tile_px([rect[0] + rect[2] - 1, rect[1] + rect[3] - 1]);
                        let r = egui::Rect::from_min_max(
                            resp.rect.min + Vec2::new(a[0] as f32, a[1] as f32) * scale,
                            resp.rect.min
                                + Vec2::new(
                                    (b[0] + ts.tile_size[0]) as f32,
                                    (b[1] + ts.tile_size[1]) as f32,
                                ) * scale,
                        );
                        p.rect_stroke(r, 0.0, Stroke::new(2.0, st.accent), StrokeKind::Outside);
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
        if ui.button("Delete level…").clicked() {
            actions.push(Action::DeleteLevel);
        }
    });

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
