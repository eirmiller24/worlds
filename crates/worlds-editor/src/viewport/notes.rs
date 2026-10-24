//! Drawing annotations on the map: regions, guides, paths, connectors,
//! notes and pins, through egui's painter after the entity boxes and before the
//! selection outlines. The same function draws them in the editor and in
//! snapshots (`view.snapshot`, `--snapshot`), where no tag filter applies,
//! only open notes are drawn, and every guide.
//!
//! Cost is bounded: every line, fill and icon goes into one mesh (guides
//! too: one quad per segment, segments culled by their box against the
//! view), and text (cards, labels, titles) only shows from
//! [`crate::annot::CARD_CELL_PX`] screen px per cell and only where it
//! claims free space ([`super::LabelSpace`]), so it costs what the screen
//! holds.

use std::collections::HashSet;

use egui::{Align2, Color32, FontId, Mesh, Pos2, Rect, Stroke, StrokeKind, pos2, vec2};
use worlds_core::{Annotation, Id, NoteStatus, Shape};

use super::LabelSpace;
use super::camera::WRect;
use super::scene::parse_color;
use crate::annot::{self, At, NoteRef};
use crate::doc::Document;
use crate::view::ViewState;

/// Default colors per kind.
pub fn kind_color(kind: &str) -> Color32 {
    match kind {
        "note" => Color32::from_rgb(0xe8, 0xc5, 0x47),
        "pin" => Color32::from_rgb(0xff, 0x8a, 0x65),
        "path" => Color32::from_rgb(0x81, 0xc7, 0x84),
        "region" => Color32::from_rgb(0xba, 0x68, 0xc8),
        "connector" => Color32::from_rgb(0xf0, 0x92, 0x4f),
        _ => {
            let c = annot::GUIDE_CYAN;
            Color32::from_rgb(c[0], c[1], c[2])
        }
    }
}

fn color_of(c: &worlds_core::Color) -> Color32 {
    let c = parse_color(c.as_str());
    Color32::from_rgb(c[0], c[1], c[2])
}

/// The color an annotation is drawn in: a guide's group color, else its
/// own, else its kind's.
pub fn note_color(doc: &Document, r: NoteRef<'_>) -> Color32 {
    if let (Shape::Guide { group: Some(g), .. }, At::World(w)) = (&r.note.shape, r.at)
        && let Some(c) = doc.project.worlds[w]
            .guide_group(g.as_str())
            .and_then(|g| g.color.as_ref())
    {
        return color_of(c);
    }
    r.note
        .color
        .as_ref()
        .map_or_else(|| kind_color(r.note.shape.name()), color_of)
}

/// What a frame draws annotations with.
pub struct NotesFrame<'a> {
    pub doc: &'a Document,
    pub view: &'a ViewState,
    pub visible: &'a WRect,
    /// Screen px per world px.
    pub zoom: f64,
    pub to_screen: &'a dyn Fn([f64; 2]) -> Pos2,
    pub accent: Color32,
    /// Snapshot rules (no tag filter, open notes only, every guide; no
    /// selection).
    pub snapshot: bool,
}

/// A `w` wide quad from `a` to `b`.
pub fn seg_quad(mesh: &mut Mesh, a: Pos2, b: Pos2, w: f32, color: Color32) {
    let d = b - a;
    let len = d.length();
    if len < 1e-3 {
        return;
    }
    let n = vec2(-d.y, d.x) / len * (w / 2.0);
    let i = mesh.vertices.len() as u32;
    for p in [a + n, b + n, b - n, a - n] {
        mesh.colored_vertex(p, color);
    }
    mesh.add_triangle(i, i + 1, i + 2);
    mesh.add_triangle(i, i + 2, i + 3);
}

/// The part of segment `a`-`b` inside `clip`, as distances along it from
/// `a` (Liang-Barsky); `None` when it misses.
fn clip_span(a: Pos2, b: Pos2, clip: Rect) -> Option<(f32, f32)> {
    let d = b - a;
    let len = d.length();
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [
        (-d.x, a.x - clip.min.x),
        (d.x, clip.max.x - a.x),
        (-d.y, a.y - clip.min.y),
        (d.y, clip.max.y - a.y),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    (t0 <= t1).then_some((t0 * len, t1 * len))
}

/// Dashes of `w` along `a`-`b`, only where the segment crosses `clip`
/// (the view): the dash pattern keeps its phase from `a`, but no dash is
/// made off screen, so a long segment at a high zoom costs what the screen
/// holds (about `clip`'s diagonal / (dash + gap) dashes).
#[allow(clippy::too_many_arguments)]
pub fn dashed(
    mesh: &mut Mesh,
    a: Pos2,
    b: Pos2,
    w: f32,
    dash: f32,
    gap: f32,
    color: Color32,
    clip: Rect,
) {
    let d = b - a;
    let len = d.length();
    if len < 1e-3 || !len.is_finite() {
        return;
    }
    let Some((from, to)) = clip_span(a, b, clip.expand(w)) else {
        return;
    };
    let u = d / len;
    let period = dash + gap;
    // The first dash that reaches the clipped part, in phase with `a`.
    let mut t = (from / period).floor() * period;
    while t < to {
        let e = (t + dash).min(len);
        seg_quad(mesh, a + u * t, a + u * e, w, color);
        t += period;
    }
}

/// A filled circle.
pub fn disk(mesh: &mut Mesh, c: Pos2, r: f32, color: Color32) {
    let n = if r > 6.0 { 16 } else { 10 };
    let i = mesh.vertices.len() as u32;
    mesh.colored_vertex(c, color);
    for k in 0..n {
        let a = k as f32 / n as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(c + vec2(a.cos(), a.sin()) * r, color);
    }
    for k in 0..n {
        mesh.add_triangle(i, i + 1 + k, i + 1 + (k + 1) % n);
    }
}

/// A convex polygon, as a fan.
fn fan(mesh: &mut Mesh, pts: &[Pos2], color: Color32) {
    if pts.len() < 3 {
        return;
    }
    let i = mesh.vertices.len() as u32;
    for p in pts {
        mesh.colored_vertex(*p, color);
    }
    for k in 1..pts.len() as u32 - 1 {
        mesh.add_triangle(i, i + k, i + k + 1);
    }
}

/// Any simple polygon (concave too), by ear clipping.
pub fn poly_fill(mesh: &mut Mesh, pts: &[Pos2], color: Color32) {
    let n = pts.len();
    if n < 3 {
        return;
    }
    let area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    let base = mesh.vertices.len() as u32;
    for p in pts {
        mesh.colored_vertex(*p, color);
    }
    let mut idx: Vec<usize> = (0..n).collect();
    if area < 0.0 {
        idx.reverse();
    }
    let cross = |a: Pos2, b: Pos2, c: Pos2| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let mut guard = 0;
    while idx.len() > 3 && guard < n * n {
        guard += 1;
        let m = idx.len();
        let mut clipped = false;
        for k in 0..m {
            let (ia, ib, ic) = (idx[(k + m - 1) % m], idx[k], idx[(k + 1) % m]);
            let (a, b, c) = (pts[ia], pts[ib], pts[ic]);
            if cross(a, b, c) <= 0.0 {
                continue;
            }
            let inside = idx.iter().any(|&j| {
                if j == ia || j == ib || j == ic {
                    return false;
                }
                let p = pts[j];
                cross(a, b, p) >= 0.0 && cross(b, c, p) >= 0.0 && cross(c, a, p) >= 0.0
            });
            if inside {
                continue;
            }
            mesh.add_triangle(base + ia as u32, base + ib as u32, base + ic as u32);
            idx.remove(k);
            clipped = true;
            break;
        }
        if !clipped {
            break;
        }
    }
    if idx.len() == 3 {
        mesh.add_triangle(
            base + idx[0] as u32,
            base + idx[1] as u32,
            base + idx[2] as u32,
        );
    }
}

/// A built-in pin icon of `size` px centered at `c` (unknown names draw as
/// `pin`), over a dark disk.
pub fn pin_icon(mesh: &mut Mesh, c: Pos2, size: f32, name: Option<&str>, col: Color32) {
    let s = size / 16.0;
    let p = |x: f32, y: f32| c + vec2(x * s, y * s);
    let dark = Color32::from_rgba_unmultiplied(15, 15, 20, 200);
    disk(mesh, c, 8.5 * s, dark);
    let r = |mesh: &mut Mesh, x0: f32, y0: f32, x1: f32, y1: f32, col: Color32| {
        mesh.add_colored_rect(Rect::from_min_max(p(x0, y0), p(x1, y1)), col)
    };
    match name.unwrap_or("pin") {
        "statue" => {
            r(mesh, -5.0, 4.0, 5.0, 6.5, col);
            r(mesh, -2.5, -2.0, 2.5, 4.0, col);
            disk(mesh, p(0.0, -4.5), 2.5 * s, col);
        }
        "pool" => {
            fan(
                mesh,
                &[
                    p(-6.5, 1.0),
                    p(-4.0, -2.5),
                    p(4.0, -2.5),
                    p(6.5, 1.0),
                    p(4.0, 4.5),
                    p(-4.0, 4.5),
                ],
                col,
            );
            seg_quad(mesh, p(-3.5, 0.5), p(-1.0, -0.5), 1.0 * s, dark);
            seg_quad(mesh, p(-1.0, -0.5), p(1.5, 0.5), 1.0 * s, dark);
        }
        "door" => {
            r(mesh, -4.0, -2.0, 4.0, 6.5, col);
            disk(mesh, p(0.0, -2.0), 4.0 * s, col);
            disk(mesh, p(2.0, 2.5), 0.9 * s, dark);
        }
        "star" => {
            let pts: Vec<Pos2> = (0..10)
                .map(|k| {
                    let a = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / 5.0;
                    let rr = if k % 2 == 0 { 7.0 } else { 3.0 };
                    p(a.cos() * rr, a.sin() * rr + 0.5)
                })
                .collect();
            // Star-shaped from its center: a fan around it.
            let i = mesh.vertices.len() as u32;
            mesh.colored_vertex(p(0.0, 0.5), col);
            for q in &pts {
                mesh.colored_vertex(*q, col);
            }
            for k in 0..10u32 {
                mesh.add_triangle(i, i + 1 + k, i + 1 + (k + 1) % 10);
            }
        }
        "chest" => {
            r(mesh, -6.0, -1.0, 6.0, 5.5, col);
            r(mesh, -6.0, -4.5, 6.0, -2.0, col);
            r(mesh, -1.2, -1.5, 1.2, 1.5, dark);
        }
        "skull" => {
            disk(mesh, p(0.0, -1.0), 5.5 * s, col);
            r(mesh, -3.0, 3.0, 3.0, 6.5, col);
            disk(mesh, p(-2.2, -1.0), 1.4 * s, dark);
            disk(mesh, p(2.2, -1.0), 1.4 * s, dark);
        }
        "eye" => {
            fan(
                mesh,
                &[
                    p(-7.0, 0.0),
                    p(-3.5, -3.5),
                    p(3.5, -3.5),
                    p(7.0, 0.0),
                    p(3.5, 3.5),
                    p(-3.5, 3.5),
                ],
                col,
            );
            disk(mesh, c, 2.2 * s, dark);
        }
        "flag" => {
            r(mesh, -4.5, -6.5, -3.0, 6.5, col);
            fan(mesh, &[p(-3.0, -6.5), p(5.5, -3.5), p(-3.0, -0.5)], col);
        }
        "key" => {
            disk(mesh, p(-3.5, 0.0), 3.5 * s, col);
            disk(mesh, p(-3.5, 0.0), 1.5 * s, dark);
            r(mesh, -1.0, -1.0, 6.5, 1.0, col);
            r(mesh, 3.5, 1.0, 5.0, 3.5, col);
        }
        _ => {
            // A map pin: a round head over a point.
            fan(mesh, &[p(-3.6, -2.0), p(3.6, -2.0), p(0.0, 6.5)], col);
            disk(mesh, p(0.0, -2.5), 4.0 * s, col);
            disk(mesh, p(0.0, -2.5), 1.5 * s, dark);
        }
    }
}

/// An arrowhead at `b` pointing from `a`.
fn arrowhead(mesh: &mut Mesh, a: Pos2, b: Pos2, size: f32, col: Color32) {
    let d = b - a;
    let len = d.length();
    if len < 1e-3 {
        return;
    }
    let u = d / len;
    let n = vec2(-u.y, u.x);
    fan(
        mesh,
        &[
            b + u * 1.0,
            b - u * size + n * size * 0.5,
            b - u * size - n * size * 0.5,
        ],
        col,
    );
}

/// Whether annotation `r` is drawn anywhere near `near` (the view and a
/// margin): its box as built, or, for a grouped world guide while "Show
/// guides as designed" is on (not in snapshots), its box as designed,
/// where the dashed copy goes. Each place is culled on its own.
pub fn near_view(
    doc: &Document,
    view: &ViewState,
    snapshot: bool,
    r: NoteRef<'_>,
    near: &WRect,
) -> bool {
    if annot::world_bbox(doc, r).is_some_and(|b| annot::touches(&b, near)) {
        return true;
    }
    view.annotations.show_designed
        && !snapshot
        && matches!(r.at, At::World(_))
        && matches!(r.note.shape, Shape::Guide { group: Some(_), .. })
        && r.note.bbox().is_some_and(|[x, y, w, h]| {
            annot::touches(
                &WRect::from_pos_size([x as f64, y as f64], [w as f64, h as f64]),
                near,
            )
        })
}

/// Draw every visible annotation of the frame.
pub fn draw_annotations(painter: &egui::Painter, f: &NotesFrame<'_>) {
    let doc = f.doc;
    let card = annot::card_zoom(doc, f.zoom);
    let selected: HashSet<&Id> = if f.snapshot {
        HashSet::new()
    } else {
        f.view.selection.notes().collect()
    };
    let notes = annot::all_notes(doc);
    // A margin around the view for cards and icons hanging off a point.
    let slack = (annot::CARD_MAX[0].max(annot::CARD_MAX[1]) as f64 + 16.0) / f.zoom;
    let near = f.visible.expand(slack);
    let mut fills = Mesh::default();
    let mut lines = Mesh::default();
    let mut halos = Mesh::default();
    let mut text: Vec<Txt> = Vec::new();
    let mut outlines: Vec<egui::Shape> = Vec::new();
    let mut space = LabelSpace::default();
    let ts = f.to_screen;
    let mut drawn: Vec<(NoteRef<'_>, Color32, bool)> = Vec::new();
    for r in &notes {
        if !annot::shown(f.view, r.note, f.snapshot) {
            continue;
        }
        if !near_view(doc, f.view, f.snapshot, *r, &near) {
            continue;
        }
        let mut col = note_color(doc, *r);
        if r.note.status == NoteStatus::Done && !matches!(r.note.shape, Shape::Guide { .. }) {
            col = col.gamma_multiply(0.4);
        }
        drawn.push((*r, col, selected.contains(&r.note.id)));
    }
    // Regions, then guides, then paths, then notes and pins on top.
    for (r, col, sel) in drawn
        .iter()
        .filter(|d| matches!(d.0.note.shape, Shape::Region { .. }))
    {
        let pts: Vec<Pos2> = annot::world_points(doc, *r)
            .iter()
            .map(|p| ts([p[0] as f64, p[1] as f64]))
            .collect();
        poly_fill(&mut fills, &pts, col.gamma_multiply(0.12));
        for k in 0..pts.len() {
            seg_quad(&mut lines, pts[k], pts[(k + 1) % pts.len()], 1.5, *col);
            if *sel {
                seg_quad(&mut halos, pts[k], pts[(k + 1) % pts.len()], 4.0, f.accent);
            }
        }
        if card
            && let (Some(t), Some(min)) =
                (&r.note.title, pts.iter().copied().reduce(|a, b| a.min(b)))
        {
            let rect = Rect::from_min_size(
                min + vec2(4.0, 3.0),
                vec2(t.chars().count() as f32 * 6.2, 13.0),
            );
            if painter.clip_rect().intersects(rect) && space.claim(rect) {
                text.push(Txt::new(
                    rect.min,
                    Align2::LEFT_TOP,
                    t,
                    FontId::proportional(11.0),
                    col.gamma_multiply(1.0).to_opaque(),
                ));
            }
        }
    }
    let vis = f.visible;
    for (r, col, sel) in drawn
        .iter()
        .filter(|d| matches!(d.0.note.shape, Shape::Guide { .. }))
    {
        for (a, b) in annot::segments(doc, *r) {
            let sb = WRect::new(
                [a[0].min(b[0]), a[1].min(b[1])],
                [a[0].max(b[0]), a[1].max(b[1])],
            );
            if !annot::touches(&sb, vis) {
                continue;
            }
            let (sa, sb) = (ts(a), ts(b));
            if *sel {
                seg_quad(&mut halos, sa, sb, 5.0, f.accent);
            }
            seg_quad(&mut lines, sa, sb, 2.0, *col);
        }
        // As designed: grouped guides also dashed where they were laid out.
        if f.view.annotations.show_designed
            && !f.snapshot
            && let At::World(w) = r.at
        {
            let o = annot::group_offset(doc, w, r.note);
            if o != [0, 0] {
                let pts = r.note.shape.points();
                for k in 1..pts.len() {
                    let a = [pts[k - 1][0] as f64, pts[k - 1][1] as f64];
                    let b = [pts[k][0] as f64, pts[k][1] as f64];
                    let sb = WRect::new(
                        [a[0].min(b[0]), a[1].min(b[1])],
                        [a[0].max(b[0]), a[1].max(b[1])],
                    );
                    if annot::touches(&sb, vis) {
                        let clip = painter.clip_rect();
                        dashed(&mut lines, ts(a), ts(b), 1.0, 5.0, 4.0, *col, clip);
                    }
                }
            }
        }
    }
    for (r, col, sel) in drawn
        .iter()
        .filter(|d| matches!(d.0.note.shape, Shape::Path { .. }))
    {
        let wpts = annot::world_points(doc, *r);
        let pts: Vec<Pos2> = wpts
            .iter()
            .map(|p| ts([p[0] as f64, p[1] as f64]))
            .collect();
        for w in pts.windows(2) {
            if *sel {
                seg_quad(&mut halos, w[0], w[1], 5.0, f.accent);
            }
            seg_quad(&mut lines, w[0], w[1], 2.0, *col);
        }
        if let [.., a, b] = pts.as_slice() {
            arrowhead(&mut lines, *a, *b, 9.0, *col);
        }
        if *sel {
            for p in &pts {
                disk(&mut lines, *p, 3.0, f.accent);
            }
            let len = annot::path_length(&r.note.shape.points());
            let g = doc.project.grid_size.max(1) as f64;
            if let Some(mid) = midpoint(&pts) {
                text.push(Txt::new(
                    mid + vec2(0.0, -6.0),
                    Align2::CENTER_BOTTOM,
                    format!("{len:.0} px · {:.1} tiles", len / g),
                    FontId::proportional(11.0),
                    Color32::WHITE,
                ));
            }
        }
    }
    // Connectors: from their computed ends through the waypoints, an
    // arrowhead at `to` (both ends with `double`, none with `line`), the
    // label along the middle.
    for (r, col, sel) in drawn
        .iter()
        .filter(|d| matches!(d.0.note.shape, Shape::Connector { .. }))
    {
        let Shape::Connector { label, style, .. } = &r.note.shape else {
            continue;
        };
        let pts: Vec<Pos2> = annot::connector_line(doc, *r)
            .iter()
            .map(|p| ts([p[0] as f64, p[1] as f64]))
            .collect();
        for w in pts.windows(2) {
            if *sel {
                seg_quad(&mut halos, w[0], w[1], 5.0, f.accent);
            }
            seg_quad(&mut lines, w[0], w[1], 2.0, *col);
        }
        if *style != worlds_core::ConnectorStyle::Line
            && let [.., a, b] = pts.as_slice()
        {
            arrowhead(&mut lines, *a, *b, 9.0, *col);
        }
        if *style == worlds_core::ConnectorStyle::Double
            && let [a, b, ..] = pts.as_slice()
        {
            arrowhead(&mut lines, *b, *a, 9.0, *col);
        }
        for p in pts.iter().skip(1).take(pts.len().saturating_sub(2)) {
            disk(&mut lines, *p, 2.5, *col);
        }
        let text_of = label.as_deref().or(r.note.title.as_deref());
        if (card || *sel)
            && let (Some(t), Some(mid)) = (text_of, midpoint(&pts))
        {
            let rect = Rect::from_center_size(
                mid + vec2(0.0, -9.0),
                vec2(t.chars().count() as f32 * 6.2 + 6.0, 14.0),
            );
            if painter.clip_rect().intersects(rect) && space.claim(rect) {
                fills.add_colored_rect(rect, Color32::from_rgba_unmultiplied(20, 22, 30, 200));
                text.push(Txt::new(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    t,
                    FontId::proportional(11.0),
                    Color32::from_gray(235),
                ));
            }
        }
    }
    let psize = annot::pin_size(doc, f.zoom) as f32;
    for (r, col, sel) in drawn.iter() {
        let (px, pin) = match &r.note.shape {
            Shape::Note { px } => (*px, None),
            Shape::Pin { px, icon, label } => (*px, Some((icon, label))),
            _ => continue,
        };
        let o = annot::offset(doc, r.at, r.note);
        let c = ts([(px[0] + o[0]) as f64, (px[1] + o[1]) as f64]);
        match pin {
            Some((icon, label)) => {
                if *sel {
                    disk(&mut halos, c, psize * 0.5 + 4.0, f.accent);
                }
                pin_icon(&mut lines, c, psize, icon.as_deref(), *col);
                let label = label.as_deref().or(r.note.title.as_deref());
                if card && let Some(l) = label {
                    let rect = Rect::from_min_size(
                        c + vec2(psize * 0.5 + 3.0, -6.5),
                        vec2(l.chars().count() as f32 * 6.2, 13.0),
                    );
                    if painter.clip_rect().intersects(rect) && space.claim(rect) {
                        text.push(Txt::new(
                            rect.left_center(),
                            Align2::LEFT_CENTER,
                            l,
                            FontId::proportional(11.0),
                            Color32::from_gray(235),
                        ));
                    }
                }
            }
            None => {
                let size = annot::card_size(r.note);
                let rect = Rect::from_min_size(c, vec2(size[0], size[1]));
                if card && painter.clip_rect().intersects(rect) && space.claim(rect) {
                    note_card(
                        &mut fills,
                        &mut text,
                        &mut outlines,
                        r.note,
                        rect,
                        *col,
                        *sel,
                        f.accent,
                    );
                } else {
                    if *sel {
                        disk(&mut halos, c, 6.0, f.accent);
                    }
                    disk(&mut lines, c, 3.0, *col);
                }
            }
        }
    }
    painter.add(egui::Shape::mesh(halos));
    painter.add(egui::Shape::mesh(fills));
    painter.add(egui::Shape::mesh(lines));
    painter.extend(outlines);
    for t in text {
        painter.text(
            t.pos,
            t.align,
            t.text,
            FontId::proportional(t.size),
            t.color,
        );
    }
}

/// Text drawn after every mesh.
struct Txt {
    pos: Pos2,
    align: Align2,
    text: String,
    size: f32,
    color: Color32,
}

impl Txt {
    fn new(pos: Pos2, align: Align2, text: impl ToString, font: FontId, color: Color32) -> Self {
        Txt {
            pos,
            align,
            text: text.to_string(),
            size: font.size,
            color,
        }
    }
}

fn midpoint(pts: &[Pos2]) -> Option<Pos2> {
    let total: f32 = pts.windows(2).map(|w| (w[1] - w[0]).length()).sum();
    let mut left = total / 2.0;
    for w in pts.windows(2) {
        let l = (w[1] - w[0]).length();
        if l >= left && l > 0.0 {
            return Some(w[0] + (w[1] - w[0]) * (left / l));
        }
        left -= l;
    }
    pts.first().copied()
}

/// A note card: title, the body's first line, up to three tag chips.
#[allow(clippy::too_many_arguments)]
fn note_card(
    fills: &mut Mesh,
    text: &mut Vec<Txt>,
    outlines: &mut Vec<egui::Shape>,
    a: &Annotation,
    rect: Rect,
    col: Color32,
    sel: bool,
    accent: Color32,
) {
    let done = a.status == NoteStatus::Done;
    let bg = Color32::from_rgba_unmultiplied(28, 30, 38, if done { 140 } else { 225 });
    fills.add_colored_rect(rect, bg);
    fills.add_colored_rect(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), col);
    let fg = if done {
        Color32::from_gray(150)
    } else {
        Color32::from_gray(240)
    };
    let clip = |s: &str, max: usize| -> String {
        if s.chars().count() > max {
            format!("{}…", s.chars().take(max - 1).collect::<String>())
        } else {
            s.to_string()
        }
    };
    let max_chars = ((rect.width() - 12.0) / 6.2) as usize;
    let mut y = rect.min.y + 3.0;
    text.push(Txt::new(
        pos2(rect.min.x + 7.0, y),
        Align2::LEFT_TOP,
        clip(a.title.as_deref().unwrap_or("Note"), max_chars.max(4)),
        FontId::proportional(12.0),
        fg,
    ));
    y += 15.0;
    if let Some(b) = annot::first_line(a.body.as_deref()) {
        text.push(Txt::new(
            pos2(rect.min.x + 7.0, y),
            Align2::LEFT_TOP,
            clip(b, max_chars.max(4)),
            FontId::proportional(10.5),
            Color32::from_gray(185),
        ));
        y += 13.0;
    }
    let mut x = rect.min.x + 7.0;
    for t in a.tags.iter().take(3) {
        let w = t.chars().count() as f32 * 5.6 + 8.0;
        if x + w > rect.max.x - 2.0 {
            break;
        }
        let chip = Rect::from_min_size(pos2(x, y), vec2(w, 11.0));
        fills.add_colored_rect(chip, col.gamma_multiply(0.45));
        text.push(Txt::new(
            chip.center(),
            Align2::CENTER_CENTER,
            t,
            FontId::proportional(9.0),
            Color32::WHITE,
        ));
        x += w + 3.0;
    }
    if sel {
        outlines.push(egui::Shape::rect_stroke(
            rect,
            2.0,
            Stroke::new(2.0, accent),
            StrokeKind::Outside,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A1: a 10 million px segment at a high zoom with a few hundred px on
    /// screen makes only the dashes the screen holds, in phase with its
    /// start.
    #[test]
    fn dashes_are_clipped_to_the_view() {
        let clip = Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 900.0));
        let mut m = Mesh::default();
        dashed(
            &mut m,
            pos2(-5.0e6, 450.0),
            pos2(5.0e6, 450.0),
            1.0,
            5.0,
            4.0,
            Color32::WHITE,
            clip,
        );
        let dashes = m.vertices.len() / 4;
        assert!((170..=182).contains(&dashes), "{dashes} dashes");
        // Phase: dash starts are a whole number of periods from `a`.
        let x0 = m.vertices[0].pos.x as f64 + 5.0e6;
        assert!((x0 / 9.0 - (x0 / 9.0).round()).abs() < 0.01, "{x0}");
        // A diagonal one crossing a corner, and one missing the view.
        let mut m = Mesh::default();
        dashed(
            &mut m,
            pos2(-1.0e7, -1.0e7),
            pos2(1.0e7, 1.0e7),
            1.0,
            5.0,
            4.0,
            Color32::WHITE,
            clip,
        );
        assert!(m.vertices.len() / 4 <= 205, "{}", m.vertices.len() / 4);
        let mut m = Mesh::default();
        dashed(
            &mut m,
            pos2(-1.0e7, -50.0),
            pos2(1.0e7, -50.0),
            1.0,
            5.0,
            4.0,
            Color32::WHITE,
            clip,
        );
        assert!(m.vertices.is_empty());
        // A short one inside is drawn whole: 0-5, 9-14, 18-20.
        let mut m = Mesh::default();
        dashed(
            &mut m,
            pos2(10.0, 10.0),
            pos2(30.0, 10.0),
            1.0,
            5.0,
            4.0,
            Color32::WHITE,
            clip,
        );
        assert_eq!(m.vertices.len(), 12);
    }

    /// A4: with "Show guides as designed", a grouped guide whose built
    /// place is off screen but whose designed place is on screen is kept.
    #[test]
    fn designed_guides_are_culled_by_their_own_box() {
        let doc = crate::doc::Document::open(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../worlds-core/tests/fixtures/mini.worlds"),
        )
        .unwrap();
        let mut doc = doc;
        let g = worlds_core::Slug::new("far").unwrap();
        doc.project.worlds[0]
            .guide_groups
            .push(worlds_core::GuideGroup {
                offset: [100_000, 0],
                ..worlds_core::GuideGroup::new(g.clone())
            });
        if let Shape::Guide { group, .. } = &mut doc.project.worlds[0].notes[0].shape {
            *group = Some(g);
        }
        let mut view = ViewState::new();
        let r = annot::find_note(&doc, &Id::new("guidestem2").unwrap()).unwrap();
        let near = WRect::new([-10.0, -10.0], [400.0, 400.0]);
        assert!(!near_view(&doc, &view, false, r, &near));
        view.annotations.show_designed = true;
        assert!(near_view(&doc, &view, false, r, &near));
        assert!(
            !near_view(&doc, &view, true, r, &near),
            "snapshots draw as built only"
        );
        // Built on screen: always kept.
        let far = WRect::new([100_000.0, 0.0], [100_100.0, 100.0]);
        view.annotations.show_designed = false;
        assert!(near_view(&doc, &view, false, r, &far));
    }
}
