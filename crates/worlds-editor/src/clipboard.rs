//! The clipboard: Copy (Ctrl+C), Cut (Ctrl+X) and Paste (Ctrl+V, or in
//! place with Ctrl+Shift+V) of [`Fragment`]s.
//!
//! Copy turns the selection (entities, tile stacks, annotations) plus the
//! last Select marquee on an int_grid layer ([`GridMarquee`]) into a
//! fragment with [`worlds_core::fragment::copy`]; the app puts its JSON on
//! the system clipboard as text, so it pastes into another editor or a chat.
//! Paste reads it back and arms a [`PasteGhost`]: the fragment follows the
//! pointer, drawn with approval mode's ghost overlay, until a click lands it
//! (one batch, one undo entry, [`worlds_core::fragment::paste`]) or Escape
//! cancels. Like the other plans these are plain functions of the document
//! and the view, so `--script` drives exactly what the keys do.

use std::borrow::Cow;

use egui::Color32;
use worlds_core::command::{Command, GridValue, Rect};
use worlds_core::fragment::{self, CopySelection, Fragment, PasteOptions, PastePlan};
use worlds_core::{Id, Slug};

use crate::annot::{At, find_note};
use crate::doc::{Document, LevelIx};
use crate::ghost::Ghost;
use crate::view::{SelItem, ViewState};
use crate::viewport::camera::WRect;

/// How the paste ghost is turned before it lands. Only the identity for
/// now: mirror and rotate (roadmap 2.4) add their variants here and in
/// [`Transform::apply`], and every paste goes through it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Transform {
    #[default]
    Identity,
}

impl Transform {
    /// The fragment as it lands.
    pub fn apply<'a>(&self, f: &'a Fragment) -> Cow<'a, Fragment> {
        match self {
            Transform::Identity => Cow::Borrowed(f),
        }
    }
}

/// A paste waiting for its click (`ViewState::paste`).
#[derive(Debug, Clone, PartialEq)]
pub struct PasteGhost {
    /// The fragment as copied.
    pub fragment: Fragment,
    /// Applied to `fragment` before it is drawn or pasted.
    pub transform: Transform,
    /// Where it would land: the level and the fragment's origin there (in
    /// cells of the project grid), following the pointer.
    pub at: Option<(LevelIx, [i64; 2])>,
    /// Set by a script: the pointer doesn't move it.
    pub pinned: bool,
    /// The ghost drawn for `at` (worked out once per spot).
    pub ghost: Option<((LevelIx, [i64; 2]), Ghost)>,
}

impl PasteGhost {
    pub fn new(fragment: Fragment) -> Self {
        PasteGhost {
            fragment,
            transform: Transform::Identity,
            at: None,
            pinned: false,
            ghost: None,
        }
    }

    /// The fragment as it would land.
    pub fn fragment(&self) -> Cow<'_, Fragment> {
        self.transform.apply(&self.fragment)
    }

    /// The origin that puts the middle of the fragment on the project cell
    /// under world pixel `wp` in level `ix`.
    pub fn origin_under(&self, doc: &Document, ix: LevelIx, wp: [f64; 2]) -> [i64; 2] {
        let l = doc.level(ix);
        let g = doc.project.grid_size.max(1) as f64;
        let size = self.fragment().size;
        let cell = [
            ((wp[0] - l.world_px[0] as f64) / g).floor() as i64,
            ((wp[1] - l.world_px[1] as f64) / g).floor() as i64,
        ];
        [cell[0] - size[0] as i64 / 2, cell[1] - size[1] as i64 / 2]
    }

    /// Work out the ghost for where it is now (`at`), unless it is already.
    pub fn refresh(&mut self, doc: &Document) {
        let Some(at) = self.at.filter(|a| a.0 < doc.levels.len()) else {
            self.ghost = None;
            return;
        };
        if self.ghost.as_ref().is_some_and(|(k, _)| *k == at) {
            return;
        }
        let frag = self.fragment().into_owned();
        let mut g = match paste_plan(doc, &frag, at.0, at.1) {
            Ok(plan) => {
                Ghost::of_commands(&doc.project, "paste", &plan.label, "", &plan.commands, &[])
            }
            Err(e) => {
                let mut g = Ghost::of_commands(&doc.project, "paste", "Paste", "", &[], &[]);
                g.error = Some(e);
                g
            }
        };
        g.fills.extend(
            void_under(doc, &frag, at.0, at.1)
                .into_iter()
                .map(|r| (r, VOID_TINT)),
        );
        g.outlines.push(fragment_rect(doc, &frag, at.0, at.1));
        self.ghost = Some((at, g));
    }
}

/// Fill of the paste ghost's cells that can't land: the level's void.
pub const VOID_TINT: Color32 = Color32::from_rgb(230, 80, 80);

/// The world rects of a non-rectangular level's void under a fragment with
/// its origin at `origin` in `ix` (nothing lands there: the paste drops
/// those cells, tiles and entities).
pub fn void_under(doc: &Document, frag: &Fragment, ix: LevelIx, origin: [i64; 2]) -> Vec<WRect> {
    let l = doc.level(ix);
    let g = doc.project.grid_size as f64;
    let (fx0, fy0) = (origin[0], origin[1]);
    let (fx1, fy1) = (fx0 + frag.size[0] as i64, fy0 + frag.size[1] as i64);
    l.void
        .iter()
        .filter_map(|r| {
            let x0 = (r.x as i64).max(fx0);
            let y0 = (r.y as i64).max(fy0);
            let x1 = ((r.x + r.w).min(l.size[0]) as i64).min(fx1);
            let y1 = ((r.y + r.h).min(l.size[1]) as i64).min(fy1);
            (x0 < x1 && y0 < y1).then(|| {
                let o = [l.world_px[0] as f64, l.world_px[1] as f64];
                WRect::new(
                    [o[0] + x0 as f64 * g, o[1] + y0 as f64 * g],
                    [o[0] + x1 as f64 * g, o[1] + y1 as f64 * g],
                )
            })
        })
        .collect()
}

/// The world rect a fragment covers with its origin at `origin` in `ix`.
pub fn fragment_rect(doc: &Document, frag: &Fragment, ix: LevelIx, origin: [i64; 2]) -> WRect {
    let l = doc.level(ix);
    let g = doc.project.grid_size as f64;
    WRect::from_pos_size(
        [
            l.world_px[0] as f64 + origin[0] as f64 * g,
            l.world_px[1] as f64 + origin[1] as f64 * g,
        ],
        [frag.size[0] as f64 * g, frag.size[1] as f64 * g],
    )
}

/// The last Select marquee in a level with an int_grid layer active: the
/// cells Copy and Cut take from that layer (`ViewState::grid_marquee`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridMarquee {
    /// The level, by its `level.json` id.
    pub level: Id,
    pub layer: Slug,
    /// In cells of the project grid, inside the level.
    pub rect: Rect,
}

/// The [`GridMarquee`] of a marquee over world rect `r` in level `ix`, if
/// the active layer is an int_grid layer that isn't derived and the rect
/// covers at least one cell.
pub fn grid_marquee(
    doc: &Document,
    view: &ViewState,
    ix: LevelIx,
    r: &WRect,
) -> Option<GridMarquee> {
    let def = view.active_layer.and_then(|l| doc.project.layers.get(l))?;
    if !def.is_int_grid() || def.is_derived() || ix >= doc.levels.len() {
        return None;
    }
    let l = doc.level(ix);
    let g = doc.project.grid_size.max(1) as f64;
    let (ox, oy) = (l.world_px[0] as f64, l.world_px[1] as f64);
    let clamp = |v: f64, n: u32| v.clamp(0.0, n as f64) as u32;
    let x0 = clamp(((r.min[0] - ox) / g).floor(), l.size[0]);
    let y0 = clamp(((r.min[1] - oy) / g).floor(), l.size[1]);
    let x1 = clamp(((r.max[0] - ox) / g).ceil(), l.size[0]);
    let y1 = clamp(((r.max[1] - oy) / g).ceil(), l.size[1]);
    (x1 > x0 && y1 > y0).then(|| GridMarquee {
        level: l.id.clone(),
        layer: def.slug.clone(),
        rect: Rect::new(x0, y0, x1 - x0, y1 - y0),
    })
}

/// The grid marquee, if it still counts: its level exists and its layer
/// is the active one.
fn live_marquee(doc: &Document, view: &ViewState) -> Option<(LevelIx, GridMarquee)> {
    let m = view.grid_marquee.as_ref()?;
    let ix = doc.level_ix_by_id(&m.level)?;
    let active = view.active_layer.and_then(|l| doc.project.layers.get(l))?;
    (active.slug == m.layer).then(|| (ix, m.clone()))
}

/// What Copy took.
#[derive(Debug, Clone, PartialEq)]
pub struct Copied {
    pub fragment: Fragment,
    /// "Copied 3 items" (with what was left out, if anything).
    pub message: String,
    /// The selection items it took (Cut deletes these).
    pub items: Vec<SelItem>,
    /// The grid marquee it took, with its level.
    pub marquee: Option<(LevelIx, GridMarquee)>,
}

/// Copy: the selection and the grid marquee as a fragment. Everything
/// comes from one level (the marquee's, else the first selected item's);
/// selected items in other levels and selected levels are left out, and
/// the message says so.
pub fn copy_selection(doc: &Document, view: &ViewState) -> Result<Copied, String> {
    let marquee = live_marquee(doc, view);
    let item_level = |i: &SelItem| -> Option<Option<LevelIx>> {
        match i {
            SelItem::Entity(id) => doc.find_entity(id).map(|f| Some(f.0)),
            SelItem::Tiles { level, .. } => doc.level_ix_by_id(level).map(Some),
            SelItem::Note(id) => find_note(doc, id).map(|n| n.level()),
            SelItem::Level(_) => None,
        }
    };
    let level: Option<LevelIx> = marquee.as_ref().map(|m| m.0).or_else(|| {
        view.selection
            .items
            .iter()
            .find_map(|i| item_level(i).flatten())
    });
    let world = level.map(|ix| crate::annot::world_of(doc, ix));
    let mut sel = CopySelection {
        level: level.map(|ix| doc.level(ix).slug.clone()),
        ..Default::default()
    };
    let mut items = Vec::new();
    let mut elsewhere = 0;
    let mut levels = 0;
    for i in &view.selection.items {
        let Some(at) = item_level(i) else {
            if matches!(i, SelItem::Level(_)) {
                levels += 1;
            }
            continue;
        };
        let here = match (i, at) {
            (SelItem::Note(id), None) => {
                // A world note: it comes along with its own world's level.
                let w = find_note(doc, id).map(|n| match n.at {
                    At::World(w) => w,
                    At::Level(ix) => crate::annot::world_of(doc, ix),
                });
                world.is_none() || w == world
            }
            (_, at) => level.is_none() || at == level,
        };
        if !here {
            elsewhere += 1;
            continue;
        }
        items.push(i.clone());
        match i {
            SelItem::Entity(id) => sel.entities.push(id.clone()),
            SelItem::Note(id) => sel.notes.push(id.clone()),
            SelItem::Tiles { layer, cell, .. } => {
                sel.tiles.entry(layer.clone()).or_default().push(*cell)
            }
            SelItem::Level(_) => {}
        }
    }
    if let Some((_, m)) = &marquee {
        sel.rect = Some(m.rect);
        sel.layers = vec![m.layer.clone()];
    }
    if sel.entities.is_empty() && sel.notes.is_empty() && sel.tiles.is_empty() && sel.rect.is_none()
    {
        return Err(if levels > 0 {
            "Levels aren't copied: Ctrl+D duplicates a level".into()
        } else {
            "Nothing to copy: select entities, tile stacks or notes, or drag a marquee with an int_grid layer active".into()
        });
    }
    let fragment = fragment::copy(&doc.project, &sel).map_err(|e| format!("Can't copy: {e}"))?;
    let n = fragment.item_count();
    let mut message = format!("Copied {n} item{}", if n == 1 { "" } else { "s" });
    if elsewhere > 0 {
        message.push_str(&format!(
            "; {elsewhere} selected in other levels left out (a copy comes from one level)"
        ));
    }
    if levels > 0 {
        message.push_str("; levels aren't copied (Ctrl+D duplicates a level)");
    }
    Ok(Copied {
        fragment,
        message,
        items,
        marquee,
    })
}

/// Cut: what Copy takes, then one batch removing it (entities, unlocked
/// annotations and tile stacks as Delete does, the marquee's cells
/// emptied). Returns the copy, the commands and their label.
pub fn cut_selection(
    doc: &Document,
    view: &ViewState,
) -> Result<(Copied, Vec<Command>, String), String> {
    let copied = copy_selection(doc, view)?;
    let mut only = view.clone();
    only.selection.set(copied.items.iter().cloned());
    let plan = crate::annot::plan_delete(doc, &only);
    let mut commands = plan.commands;
    if let Some((ix, m)) = &copied.marquee
        && let Some(def) = doc.project.layer(m.layer.as_str())
    {
        let l = doc.level(*ix);
        let g = doc.project.grid_size as u64;
        let lg = doc.project.layer_grid(def) as u64;
        let cells = doc.project.layer_cells(def, l);
        let to = |v: u32, n: u32, up: bool| -> u32 {
            let px = v as u64 * g;
            let c = if up { px.div_ceil(lg) } else { px / lg };
            (c as u32).min(n)
        };
        let (x0, y0) = (to(m.rect.x, cells[0], false), to(m.rect.y, cells[1], false));
        let (x1, y1) = (
            to(m.rect.x + m.rect.w, cells[0], true),
            to(m.rect.y + m.rect.h, cells[1], true),
        );
        if x1 > x0 && y1 > y0 {
            commands.push(Command::FillRect {
                level: l.slug.clone(),
                layer: m.layer.clone(),
                rect: Rect::new(x0, y0, x1 - x0, y1 - y0),
                value: GridValue::Clear,
            });
        }
    }
    let n = copied.fragment.item_count();
    let label = format!("Cut {n} item{}", if n == 1 { "" } else { "s" });
    Ok((copied, commands, label))
}

/// A fragment from clipboard text: `Ok(None)` when the text isn't one.
pub fn parse(text: &str) -> Result<Option<Fragment>, String> {
    Fragment::from_text(text).transpose()
}

/// The paste of `frag` with its origin at `origin` in level `ix`.
pub fn paste_plan(
    doc: &Document,
    frag: &Fragment,
    ix: LevelIx,
    origin: [i64; 2],
) -> Result<PastePlan, String> {
    fragment::paste(
        &doc.project,
        frag,
        &PasteOptions {
            level: doc.level(ix).slug.clone(),
            origin,
            layer_map: Default::default(),
        },
    )
    .map_err(|e| format!("Can't paste: {e}"))
}

/// What a paste made, as the selection: the new entities, tile stacks and
/// annotations.
pub fn pasted_selection(doc: &Document, plan: &PastePlan, ix: LevelIx) -> Vec<SelItem> {
    let mut out: Vec<SelItem> = plan.entities.iter().cloned().map(SelItem::Entity).collect();
    for (layer, cells) in &plan.tiles {
        out.extend(cells.iter().map(|c| SelItem::Tiles {
            level: doc.ids[ix].clone(),
            layer: layer.clone(),
            cell: *c,
        }));
    }
    out.extend(plan.notes.iter().cloned().map(SelItem::Note));
    out
}

/// Where Paste in place puts `frag`: its source level (when the project
/// has it) at its source cells, else the active level at those cells.
pub fn in_place_target(
    doc: &Document,
    view: &ViewState,
    frag: &Fragment,
) -> Option<(LevelIx, [i64; 2])> {
    let src = frag.source.as_ref();
    let ix = src
        .and_then(|s| s.level.as_ref())
        .and_then(|l| doc.level_ix(l.as_str()))
        .or(view.active_level.filter(|&a| a < doc.levels.len()))?;
    Some((ix, src.map_or([0, 0], |s| s.origin)))
}

/// A slug for a copy of level `slug`: `<slug>_copy`, `<slug>_copy2`, ...,
/// the first that no level has and `taken` doesn't hold.
pub fn copy_slug(doc: &Document, slug: &Slug, taken: &[Slug]) -> Slug {
    let free = |s: &str| doc.project.level(s).is_none() && !taken.iter().any(|t| t == s);
    let mut n = 1;
    loop {
        let s = if n == 1 {
            format!("{slug}_copy")
        } else {
            format!("{slug}_copy{n}")
        };
        if free(&s)
            && let Ok(s) = Slug::new(s)
        {
            return s;
        }
        n += 1;
    }
}

/// The `duplicate_level` of level `ix` under a fresh slug (just right of
/// it), and that slug.
pub fn duplicate_level_command(doc: &Document, ix: LevelIx, taken: &[Slug]) -> (Command, Slug) {
    let src = doc.level(ix).slug.clone();
    let slug = copy_slug(doc, &src, taken);
    (
        Command::DuplicateLevel {
            level: src,
            slug: slug.clone(),
            name: None,
            world: None,
            world_px: None,
        },
        slug,
    )
}
