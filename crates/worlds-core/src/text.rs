//! The text view of a level: grids as glyph pictures with rulers and a
//! legend, then tiles, entities and notes one per line. This is what the
//! MCP `read_level` tool returns, so agents read it; the format is stable
//! and documented in `docs/format.md` ("Text view").

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::command::CommandError;
use crate::id::Slug;
use crate::model::{
    Annotation, IntGridLayer, LayerDef, LayerKind, Level, NoteStatus, Project, Shape, bbox_of,
};

/// Which notes the text view lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NoteFilter {
    /// Every note (the default).
    #[default]
    All,
    /// Only notes whose status is open; the `notes:` count is of those.
    Open,
    /// No notes section at all.
    None,
}

/// Render `level` as text. `layers` picks which `int_grid`, `tiles` and
/// `auto` layers to show, in `layers.json` order; `None` shows all of them.
/// Entities and notes are always listed.
pub fn render_level(
    project: &Project,
    level: &str,
    layers: Option<&[&str]>,
) -> Result<String, CommandError> {
    render_level_with(project, level, layers, NoteFilter::All)
}

/// [`render_level`] with a choice of which notes to list (the live API's
/// `level.get` `notes` parameter, the MCP `read_level` tool's open notes).
/// Derived layers show their stored overrides (nothing is baked here).
pub fn render_level_with(
    project: &Project,
    level: &str,
    layers: Option<&[&str]>,
    notes_shown: NoteFilter,
) -> Result<String, CommandError> {
    render(project, level, layers, notes_shown, None)
}

/// [`render_level`] with bake results: `derived` holds the effective grid
/// of derived layers (by layer slug), which the view draws instead of their
/// stored overrides.
pub fn render_level_baked(
    project: &Project,
    level: &str,
    layers: Option<&[&str]>,
    derived: &BTreeMap<Slug, IntGridLayer>,
) -> Result<String, CommandError> {
    render(project, level, layers, NoteFilter::All, Some(derived))
}

/// [`render_level_baked`] with a choice of which notes to list.
pub fn render_level_baked_with(
    project: &Project,
    level: &str,
    layers: Option<&[&str]>,
    notes_shown: NoteFilter,
    derived: &BTreeMap<Slug, IntGridLayer>,
) -> Result<String, CommandError> {
    render(project, level, layers, notes_shown, Some(derived))
}

fn render(
    project: &Project,
    level: &str,
    layers: Option<&[&str]>,
    notes_shown: NoteFilter,
    derived: Option<&BTreeMap<Slug, IntGridLayer>>,
) -> Result<String, CommandError> {
    let slug = Slug::new(level).map_err(|e| CommandError::Invalid(e.to_string()))?;
    let (world, lvl) = project
        .worlds
        .iter()
        .find_map(|w| w.level(level).map(|l| (&w.slug, l)))
        .ok_or(CommandError::UnknownLevel(slug))?;
    if let Some(wanted) = layers {
        for l in wanted {
            if project.layer(l).is_none() {
                return Err(CommandError::UnknownLayer(
                    Slug::new(*l).map_err(|e| CommandError::Invalid(e.to_string()))?,
                ));
            }
        }
    }
    let mut out = String::new();
    header(&mut out, project, world, lvl);
    for def in &project.layers {
        if layers.is_some_and(|w| !w.contains(&def.slug.as_str())) {
            continue;
        }
        match &def.kind {
            LayerKind::IntGrid(d) if d.derived => derived_grid(
                &mut out,
                project,
                lvl,
                def,
                derived.and_then(|m| m.get(&def.slug)),
            ),
            LayerKind::IntGrid(_) => grid(&mut out, project, lvl, def),
            LayerKind::Tiles(_) => tiles(&mut out, project, lvl, def),
            LayerKind::Auto(a) => {
                let _ = writeln!(
                    out,
                    "\nauto {}: generated from {} by rules/{}.json (not shown)",
                    def.slug, a.source, def.slug
                );
            }
            LayerKind::Entities => {}
        }
    }
    entities(&mut out, lvl);
    let shown = |n: &&Annotation| match notes_shown {
        NoteFilter::All => true,
        NoteFilter::Open => n.status == NoteStatus::Open,
        NoteFilter::None => false,
    };
    if notes_shown != NoteFilter::None {
        notes(&mut out, lvl.notes.iter().filter(shown));
        let w = project
            .world(world.as_str())
            .expect("the level was found in this world");
        let g = project.grid_size as i64;
        let rect = [
            lvl.world_px[0],
            lvl.world_px[1],
            lvl.size[0] as i64 * g,
            lvl.size[1] as i64 * g,
        ];
        let mut over = Vec::new();
        let mut guides = 0;
        for n in w.notes.iter().filter(shown) {
            let mut points = n.shape.points();
            if matches!(n.shape, Shape::Connector { .. }) {
                // A connector's ends are where its rooms and entities are.
                points = project
                    .connector_points(crate::model::NoteAt {
                        world: w,
                        level: None,
                        note: n,
                    })
                    .unwrap_or(points);
            }
            if let Shape::Guide {
                group: Some(gr), ..
            } = &n.shape
                && let Some(gg) = w.guide_group(gr.as_str())
            {
                // Guides count as built.
                for p in &mut points {
                    p[0] += gg.offset[0];
                    p[1] += gg.offset[1];
                }
            }
            if !bbox_of(&points).is_some_and(|b| meets(b, rect)) {
                continue;
            }
            if matches!(n.shape, Shape::Guide { .. }) {
                guides += 1;
            } else {
                over.push(n);
            }
        }
        let _ = writeln!(
            out,
            "\nworld notes over this level: {} (world pixels)",
            over.len()
        );
        for n in over {
            note_line(&mut out, n);
        }
        let _ = writeln!(
            out,
            "guides over this level: {guides} (list_notes kind guide)"
        );
    }
    Ok(out)
}

/// Whether a bbox (`[x, y, w, h]`, far edges inclusive) meets a level rect
/// (`[x, y, w, h]`, far edges exclusive).
fn meets(b: [i64; 4], r: [i64; 4]) -> bool {
    b[0] < r[0] + r[2] && b[0] + b[2] >= r[0] && b[1] < r[1] + r[3] && b[1] + b[3] >= r[1]
}

fn header(out: &mut String, project: &Project, world: &Slug, l: &Level) {
    let _ = writeln!(
        out,
        "level {} {} world {} id {}",
        l.slug,
        json(&l.name),
        world,
        l.id
    );
    if !l.tags.is_empty() {
        out.pop();
        let _ = writeln!(out, " tags {}", compact(&serde_json::json!(l.tags)));
    }
    let _ = write!(
        out,
        "size {}x{} cells of {} px, world_px [{}, {}], depth {}",
        l.size[0], l.size[1], project.grid_size, l.world_px[0], l.world_px[1], l.depth
    );
    if let Some(bg) = &l.bg_color {
        let _ = write!(out, ", bg {bg}");
    }
    out.push('\n');
    if !l.fields.is_empty() {
        let parts: Vec<String> = l
            .fields
            .iter()
            .map(|(k, v)| format!("{k} = {}", compact(v)))
            .collect();
        let _ = writeln!(out, "fields: {}", parts.join(", "));
    }
    for (layer, s) in &l.layers {
        let _ = write!(out, "rule state {layer}:");
        if s.seed != 0 {
            let _ = write!(out, " seed {}", s.seed);
        }
        if !s.optional_groups.is_empty() {
            let g: Vec<&str> = s.optional_groups.iter().map(Slug::as_str).collect();
            let _ = write!(out, " optional_groups [{}]", g.join(", "));
        }
        for (from, to) in &s.tilesets {
            let _ = write!(out, " tileset {from} -> {to}");
        }
        out.push('\n');
    }
}

fn grid(out: &mut String, project: &Project, l: &Level, def: &LayerDef) {
    let [w, h] = project.layer_cells(def, l);
    let g = project.layer_grid(def);
    let _ = writeln!(out, "\nint_grid {} ({w}x{h} cells of {g} px)", def.slug);
    legend(out, def);
    grid_rows(out, def, l.int_grid(def.slug.as_str()), [w, h]);
}

fn legend(out: &mut String, def: &LayerDef) {
    let mut legend = vec![". empty".to_string()];
    legend.extend(
        def.values()
            .iter()
            .map(|v| format!("{} {}", v.glyph, v.slug)),
    );
    let _ = writeln!(out, "legend: {}", legend.join(", "));
}

/// A grid's picture (all `.` without data), or why it can't be drawn.
fn grid_rows(out: &mut String, def: &LayerDef, data: Option<&IntGridLayer>, [w, h]: [u32; 2]) {
    let rows: Vec<String> = match data {
        Some(d) => match d.to_rows(def) {
            Ok(rows) => rows,
            Err(e) => {
                let _ = writeln!(out, "(unreadable: {e})");
                return;
            }
        },
        None => vec![".".repeat(w as usize); h as usize],
    };
    if let Some(d) = data
        && d.size() != [w, h]
    {
        let [dw, dh] = d.size();
        let _ = writeln!(out, "(stored grid is {dw}x{dh}, the level needs {w}x{h})");
    }
    picture(out, &rows);
}

/// A derived layer: the effective grid when baked, else the stored
/// overrides; then the stored overrides when the level has any (only their
/// counts when the picture already is them).
fn derived_grid(
    out: &mut String,
    project: &Project,
    l: &Level,
    def: &LayerDef,
    effective: Option<&IntGridLayer>,
) {
    let [w, h] = project.layer_cells(def, l);
    let g = project.layer_grid(def);
    let _ = write!(
        out,
        "\nint_grid {} ({w}x{h} cells of {g} px) derived",
        def.slug
    );
    if let Some(writer) = project.derived_writer(def.slug.as_str()) {
        let _ = write!(out, " by {writer}");
    }
    if effective.is_none() {
        out.push_str(" (not baked: overrides only)");
    }
    out.push('\n');
    legend(out, def);
    let stored = l.overrides(def.slug.as_str());
    let stored_rows = |out: &mut String| -> Option<Vec<String>> {
        let o = stored?;
        match o.to_rows(def) {
            Ok(rows) => Some(rows),
            Err(e) => {
                let _ = writeln!(out, "(unreadable: {e})");
                None
            }
        }
    };
    match effective {
        Some(e) => grid_rows(out, def, Some(e), [w, h]),
        None => match stored_rows(out) {
            Some(rows) => picture(out, &rows),
            None if stored.is_some() => {}
            None => picture(out, &vec![".".repeat(w as usize); h as usize]),
        },
    }
    if let Some(o) = stored.filter(|o| !o.is_empty()) {
        let (set, sup) = o.counts();
        let _ = writeln!(
            out,
            "overrides: {} cells ({set} set, {sup} suppressed; . = from the rules)",
            set + sup
        );
        if o.size() != [w, h] {
            let [dw, dh] = o.size();
            let _ = writeln!(out, "(stored grid is {dw}x{dh}, the level needs {w}x{h})");
        }
        // Not baked: the picture above already is the stored rows.
        if effective.is_some()
            && let Some(rows) = stored_rows(out)
        {
            picture(out, &rows);
        }
    }
}

/// Rows with a column ruler above and row numbers in front.
fn picture(out: &mut String, rows: &[String]) {
    let rw = rows.first().map_or(0, |r| r.chars().count());
    let label = (rows.len().saturating_sub(1)).to_string().len();
    let pad = " ".repeat(label + 1);
    if rw > 10 {
        let tens: String = (0..rw)
            .map(|x| {
                if x % 10 == 0 {
                    char::from_digit(((x / 10) % 10) as u32, 10).unwrap()
                } else {
                    ' '
                }
            })
            .collect();
        let _ = writeln!(out, "{pad}{}", tens.trim_end());
    }
    let ones: String = (0..rw)
        .map(|x| char::from_digit((x % 10) as u32, 10).unwrap())
        .collect();
    let _ = writeln!(out, "{pad}{ones}");
    for (y, row) in rows.iter().enumerate() {
        let _ = writeln!(out, "{y:>label$} {row}");
    }
}

fn tiles(out: &mut String, project: &Project, l: &Level, def: &LayerDef) {
    let [w, h] = project.layer_cells(def, l);
    let g = project.layer_grid(def);
    let list = l
        .tiles_layer(def.slug.as_str())
        .map_or(&[][..], |t| &t.tiles);
    let _ = writeln!(
        out,
        "\ntiles {} ({w}x{h} cells of {g} px): {} {}",
        def.slug,
        list.len(),
        if list.len() == 1 { "tile" } else { "tiles" }
    );
    for t in list {
        let _ = write!(
            out,
            "  [{}, {}] {} [{}, {}]",
            t.cell[0], t.cell[1], t.tileset, t.tile[0], t.tile[1]
        );
        if let Some(f) = t.flip {
            let _ = write!(out, " flip {}", f.as_str());
        }
        if t.alpha != 1.0 {
            let _ = write!(out, " alpha {}", t.alpha);
        }
        out.push('\n');
    }
}

fn entities(out: &mut String, l: &Level) {
    let n = l.all_entities().count();
    let _ = writeln!(out, "\nentities: {n}");
    for layer in &l.entities {
        for e in &layer.entities {
            let _ = write!(
                out,
                "  {} {} in {} at [{}, {}]",
                e.id, e.ty, layer.layer, e.cell[0], e.cell[1]
            );
            if e.offset_px != [0, 0] {
                let _ = write!(out, " offset_px [{}, {}]", e.offset_px[0], e.offset_px[1]);
            }
            if let Some([w, h]) = e.size {
                let _ = write!(out, " size [{w}, {h}]");
            }
            if !e.fields.is_empty() {
                let _ = write!(
                    out,
                    " fields {}",
                    compact(&serde_json::to_value(&e.fields).unwrap())
                );
            }
            out.push('\n');
        }
    }
}

fn notes<'a>(out: &mut String, notes: impl Iterator<Item = &'a Annotation>) {
    let notes: Vec<&Annotation> = notes.collect();
    let _ = writeln!(out, "\nnotes: {}", notes.len());
    for n in notes {
        note_line(out, n);
    }
}

/// One annotation, indented, ending with a newline.
fn note_line(out: &mut String, n: &Annotation) {
    let status = match n.status {
        NoteStatus::Open => "open",
        NoteStatus::Done => "done",
    };
    let _ = write!(out, "  {} {} {status}", n.id, n.shape.name());
    match &n.shape {
        Shape::Note { px } => {
            let _ = write!(out, " at {}", point(*px));
        }
        Shape::Pin { px, icon, label } => {
            let _ = write!(out, " at {}", point(*px));
            if let Some(i) = icon {
                let _ = write!(out, " icon {}", json(i));
            }
            if let Some(l) = label {
                let _ = write!(out, " label {}", json(l));
            }
        }
        Shape::Path { points } | Shape::Guide { points, .. } => {
            let p: Vec<String> = points.iter().map(|p| point(*p)).collect();
            let _ = write!(out, " points [{}]", p.join(", "));
            if matches!(n.shape, Shape::Guide { closed: true, .. }) {
                out.push_str(" closed");
            }
        }
        Shape::Region { points, rect } => {
            if let Some([x, y, w, h]) = rect {
                let _ = write!(out, " rect [{x}, {y}, {w}, {h}]");
            } else {
                let p: Vec<String> = points.iter().map(|p| point(*p)).collect();
                let _ = write!(out, " points [{}]", p.join(", "));
            }
        }
        Shape::Connector {
            from,
            to,
            waypoints,
            label,
            style,
        } => {
            let _ = write!(out, " from {} to {}", from.link(), to.link());
            if !waypoints.is_empty() {
                let p: Vec<String> = waypoints.iter().map(|p| point(*p)).collect();
                let _ = write!(out, " waypoints [{}]", p.join(", "));
            }
            if let Some(l) = label {
                let _ = write!(out, " label {}", json(l));
            }
            if *style != crate::model::ConnectorStyle::Arrow {
                let _ = write!(out, " style {}", style.name());
            }
        }
    }
    if let Some(t) = &n.title {
        let _ = write!(out, " title {}", json(t));
    }
    if !n.tags.is_empty() {
        let _ = write!(out, " tags {}", compact(&serde_json::json!(n.tags)));
    }
    if !n.links.is_empty() {
        let _ = write!(out, " links {}", compact(&serde_json::json!(n.links)));
    }
    if n.export {
        out.push_str(" export");
    }
    if n.locked {
        out.push_str(" locked");
    }
    if let Some(b) = &n.body {
        let _ = write!(out, " body {}", json(b));
    }
    if let Shape::Guide { group: Some(g), .. } = &n.shape {
        let _ = write!(out, " group {}", json(g.as_str()));
    }
    out.push('\n');
}

fn point(p: [i64; 2]) -> String {
    format!("[{}, {}]", p[0], p[1])
}

/// A JSON string literal (quotes and escapes).
fn json(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

/// Compact JSON with a space after `:` and `,`, keys sorted.
fn compact(v: &serde_json::Value) -> String {
    let s = serde_json::to_string(v).unwrap();
    let mut out = String::with_capacity(s.len() + 16);
    let mut in_str = false;
    let mut esc = false;
    for c in s.chars() {
        out.push(c);
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
        } else if c == ':' || c == ',' {
            out.push(' ');
        }
    }
    out
}
