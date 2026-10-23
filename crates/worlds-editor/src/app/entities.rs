//! The entity palette: what the Palette pane shows on an entities layer.
//!
//! Four stacked parts, each collapsible:
//!
//! 1. **Search.** Fuzzy over name, slug, category and tags
//!    ([`super::actions::fuzzy_score`]), ranked name > slug > category > tag. It filters all
//!    the parts below. Enter picks the first hit as the brush (and gives the
//!    keyboard back to the map, where a second Enter places it). It gets the
//!    keyboard when the entity tool is entered and on T; Escape clears the
//!    text, a second Escape gives the keyboard back to the map.
//! 2. **Favorites** (★ on any row), then **Recent**: the last eight types
//!    placed, most recent first.
//! 3. **In this level**: every type in the active level with its count; the
//!    count button selects all of them.
//! 4. **Catalogue**: a tree by `category` (`Uncategorized` last), tag chips
//!    above it (an AND filter), grid or list view. With a search, the
//!    matching types instead, best first.
//!
//! Hover shows a type's doc, size and fields; right click has Favorite,
//! Edit type… (the Types pane) and Duplicate type.
//!
//! **Cost.** The rows are a flat `Vec<Row>` rebuilt only when what they
//! show changes (`RowsKey`: definitions, query, chips, collapsed parts,
//! view, favorites, recent, the level's counts, the grid's column count),
//! and drawn with `ScrollArea::show_rows`, so a frame costs the visible
//! rows only. The level's counts are cached per (edit revision, level).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, pos2};
use worlds_core::{EntityDef, Id, Project, Slug};

use super::EditorState;
use super::actions::{Action, fuzzy_query, fuzzy_score_chars};
use crate::tools::{EntityBrush, EntityPalette, PaletteView, Tool};
use crate::viewport::scene::parse_color;

/// Height of every palette row (list, headers and grid rows alike, so
/// `show_rows` can lay them out without measuring).
pub const ROW_H: f32 = 26.0;
/// Width of a grid cell.
const CELL_W: f32 = 30.0;

/// The keys of the collapsible parts in [`EntityPalette::collapsed`]
/// (category paths never start with `/`).
pub const PART_FAVORITES: &str = "/favorites";
pub const PART_RECENT: &str = "/recent";
pub const PART_LEVEL: &str = "/level";
pub const PART_CATALOGUE: &str = "/catalogue";
/// The category of types without one.
pub const UNCATEGORIZED: &str = "Uncategorized";

/// One palette row.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// A part's header: key (`/favorites`, ...), title, collapsed.
    Part {
        key: &'static str,
        title: String,
        collapsed: bool,
    },
    /// A catalogue category: full path, depth (0 = top), types under it,
    /// collapsed.
    Category {
        path: String,
        depth: usize,
        count: usize,
        collapsed: bool,
    },
    /// One type as a list row; `count` for "In this level".
    Type {
        slug: Slug,
        depth: usize,
        count: Option<usize>,
    },
    /// Grid view: up to `cols` types side by side.
    Icons { slugs: Vec<Slug>, depth: usize },
    /// A weak line of text ("No matches").
    Note(String),
}

/// (edit revision, level id): what a level's counts are good for.
type CountsKey = (u64, Id);

/// Pushes the types of one tree node as rows.
type PushTypes<'a> = dyn Fn(&mut Vec<Row>, &[&EntityDef], usize) + 'a;

/// Everything the rows depend on.
#[derive(Debug, Clone, PartialEq)]
struct RowsKey {
    defs_gen: u64,
    query: String,
    chips: Vec<String>,
    collapsed: BTreeSet<String>,
    view: PaletteView,
    favorites: Vec<Slug>,
    recent: Vec<Slug>,
    counts: Option<(u64, Id)>,
    cols: usize,
}

/// The palette's state that isn't saved (the saved part is
/// [`crate::view::ViewState::entities`]).
#[derive(Default)]
pub struct PaletteState {
    pub query: String,
    /// Give the search box the keyboard the next time it is drawn.
    pub focus: bool,
    /// Set by Enter when it also switches to the entity tool: that switch
    /// leaves the keyboard on the map (so the next Enter places) instead
    /// of giving it back to the search box.
    pub keep_map_keys: bool,
    /// The search box had the keyboard last frame (egui drops focus on
    /// Escape before widgets run, so the palette remembers it).
    had_focus: bool,
    rows: Vec<Row>,
    key: Option<RowsKey>,
    /// The best match (what Enter picks), as of the last rebuild.
    pub first_hit: Option<Slug>,
    /// Types in the active level with their counts, keyed by (edit
    /// revision, level id).
    counts: Option<(CountsKey, Vec<(Slug, usize)>)>,
    /// The edit revision as of this frame ([`refresh`]).
    rev: u64,
    /// Tilesets entity icons use, for [`refresh`] to load (by `defs_gen`).
    icon_tilesets: Option<(u64, Vec<Slug>)>,
    /// How many times the rows were rebuilt (tests: caching).
    pub rebuilds: usize,
    /// Where each type's row (or grid cell) and each header was drawn last
    /// frame, by slug or part / category key (tests).
    #[doc(hidden)]
    pub rects: HashMap<String, Rect>,
    /// The search box's rect last frame (tests).
    #[doc(hidden)]
    pub search_rect: Option<Rect>,
    /// Columns of the grid last frame.
    cols: usize,
    /// Every tag of every type (the chip menu), by `defs_gen`.
    tags: Option<(u64, Vec<String>)>,
    /// The last search's hits: (`defs_gen`, prepared query, indices into
    /// `project.entities`). A query that only appends characters scores
    /// just these.
    hits: Option<(u64, Vec<char>, Vec<usize>)>,
    /// Rebuilds that scored only the previous hits (tests).
    pub narrowed: usize,
    /// [`name_order`] of the types, by `defs_gen`.
    order: Option<(u64, Vec<u32>)>,
}

impl PaletteState {
    /// Forget everything built from the document (a reload replaced it:
    /// its `defs_gen` and edit revision start over, so cached keys could
    /// match again). The query and focus stay.
    pub fn document_replaced(&mut self) {
        self.rows.clear();
        self.key = None;
        self.first_hit = None;
        self.counts = None;
        self.icon_tilesets = None;
        self.tags = None;
        self.hits = None;
        self.order = None;
    }

    /// The rows as last built.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }
}

/// How well `query` matches a type: (tier, score), tier 3 name, 2 slug, 1
/// category, 0 a tag. `None`: no match. An empty query matches everything
/// at tier 0.
pub fn rank(def: &EntityDef, query: &str) -> Option<(u8, i32)> {
    rank_chars(def, &fuzzy_query(query))
}

/// [`rank`] with the query prepared once.
fn rank_chars(def: &EntityDef, q: &[char]) -> Option<(u8, i32)> {
    if q.is_empty() {
        return Some((0, 0));
    }
    if let Some(s) = fuzzy_score_chars(q, &def.name) {
        return Some((3, s));
    }
    if let Some(s) = fuzzy_score_chars(q, def.slug.as_str()) {
        return Some((2, s));
    }
    if let Some(s) = def
        .category
        .as_deref()
        .and_then(|c| fuzzy_score_chars(q, c))
    {
        return Some((1, s));
    }
    def.tags
        .iter()
        .filter_map(|t| fuzzy_score_chars(q, t))
        .max()
        .map(|s| (0, s))
}

/// Whether `def` carries every chip.
fn has_chips(def: &EntityDef, chips: &[String]) -> bool {
    chips.iter().all(|c| def.tags.contains(c))
}

/// The category path of a type (`Uncategorized` without one).
fn category_of(def: &EntityDef) -> &str {
    def.category
        .as_deref()
        .filter(|c| !c.trim().is_empty())
        .unwrap_or(UNCATEGORIZED)
}

/// A category tree node.
#[derive(Default)]
struct Node<'a> {
    children: BTreeMap<String, Node<'a>>,
    types: Vec<&'a EntityDef>,
    count: usize,
}

/// Top-level order: alphabetical, `Uncategorized` last.
fn ordered<'n, 'a>(
    children: &'n BTreeMap<String, Node<'a>>,
    top: bool,
) -> Vec<(&'n String, &'n Node<'a>)> {
    let mut v: Vec<_> = children.iter().collect();
    if top {
        v.sort_by_key(|(k, _)| (k.as_str() == UNCATEGORIZED, k.to_lowercase()));
    } else {
        v.sort_by_key(|(k, _)| k.to_lowercase());
    }
    v
}

/// Build the rows (see the module docs). `counts`: the active level's
/// types and counts, if a level is active. Returns the rows and the first
/// hit.
pub fn build_rows(
    project: &Project,
    pal: &EntityPalette,
    query: &str,
    counts: Option<&[(Slug, usize)]>,
    cols: usize,
) -> (Vec<Row>, Option<Slug>) {
    let (rows, first, _) = build_rows_from(project, pal, query, counts, cols, None, None);
    (rows, first)
}

/// Each type's place in name order (case-insensitive, then slug), by index
/// into `project.entities`: what equal-scoring hits and the catalogue sort
/// by. Cached per `defs_gen` by the palette.
pub fn name_order(project: &Project) -> Vec<u32> {
    let keys: Vec<(String, &Slug)> = project
        .entities
        .iter()
        .map(|d| (d.name.to_lowercase(), &d.slug))
        .collect();
    let mut ix: Vec<usize> = (0..keys.len()).collect();
    ix.sort_unstable_by(|&a, &b| keys[a].cmp(&keys[b]));
    let mut order = vec![0u32; keys.len()];
    for (pos, i) in ix.into_iter().enumerate() {
        order[i] = pos as u32;
    }
    order
}

/// [`build_rows`] scoring only the types at `candidates` (indices into
/// `project.entities`) when given: the hits of a query this one extends by
/// appended characters, since a subsequence match of the longer query is
/// one of the shorter. Also returns the indices of this query's hits.
pub fn build_rows_from(
    project: &Project,
    pal: &EntityPalette,
    query: &str,
    counts: Option<&[(Slug, usize)]>,
    cols: usize,
    candidates: Option<&[usize]>,
    order: Option<&[u32]>,
) -> (Vec<Row>, Option<Slug>, Vec<usize>) {
    let cols = cols.max(1);
    let own_order;
    let order = match order {
        Some(o) if o.len() == project.entities.len() => o,
        _ => {
            own_order = name_order(project);
            &own_order[..]
        }
    };
    let searching = !query.trim().is_empty();
    let collapsed = |k: &str| pal.collapsed.contains(k);
    // Matching types, best first (name order among equals).
    let q = fuzzy_query(query);
    let score = |i: usize| {
        let d = project.entities.get(i)?;
        rank_chars(d, &q).map(|(t, s)| (t, s, d, i))
    };
    let mut hits: Vec<(u8, i32, &EntityDef, usize)> = match candidates {
        Some(c) => c.iter().filter_map(|&i| score(i)).collect(),
        None => (0..project.entities.len()).filter_map(score).collect(),
    };
    let hit_ix: Vec<usize> = hits.iter().map(|h| h.3).collect();
    // Best first, then by name (case-insensitive), then slug.
    hits.sort_unstable_by_key(|h| (std::cmp::Reverse((h.0, h.1)), order[h.3]));
    let matching: std::collections::HashSet<&str> =
        hits.iter().map(|h| h.2.slug.as_str()).collect();
    let first_hit = hits
        .iter()
        .find(|h| has_chips(h.2, &pal.chips))
        .or(hits.first())
        .map(|h| h.2.slug.clone());
    let mut rows = Vec::new();
    let list_part = |rows: &mut Vec<Row>,
                     key: &'static str,
                     title: &str,
                     slugs: Vec<(Slug, Option<usize>)>,
                     empty: &str| {
        let shown: Vec<(Slug, Option<usize>)> = slugs
            .into_iter()
            .filter(|(s, _)| matching.contains(s.as_str()))
            .collect();
        if searching && shown.is_empty() {
            return;
        }
        let c = collapsed(key);
        rows.push(Row::Part {
            key,
            title: format!("{title} ({})", shown.len()),
            collapsed: c,
        });
        if c {
            return;
        }
        if shown.is_empty() {
            rows.push(Row::Note(empty.into()));
        }
        for (slug, count) in shown {
            rows.push(Row::Type {
                slug,
                depth: 0,
                count,
            });
        }
    };
    let known = |s: &Slug| project.entity_def(s.as_str()).is_some();
    list_part(
        &mut rows,
        PART_FAVORITES,
        "Favorites",
        pal.favorites
            .iter()
            .filter(|s| known(s))
            .map(|s| (s.clone(), None))
            .collect(),
        "Star a type (★) to keep it here",
    );
    list_part(
        &mut rows,
        PART_RECENT,
        "Recent",
        pal.recent
            .iter()
            .filter(|s| known(s))
            .map(|s| (s.clone(), None))
            .collect(),
        "Types you place show here",
    );
    if let Some(counts) = counts {
        list_part(
            &mut rows,
            PART_LEVEL,
            "In this level",
            counts.iter().map(|(s, n)| (s.clone(), Some(*n))).collect(),
            "No entities in this level",
        );
    }
    // The catalogue.
    let in_cat: Vec<&EntityDef> = if searching {
        hits.iter()
            .map(|h| h.2)
            .filter(|d| has_chips(d, &pal.chips))
            .collect()
    } else {
        let mut v: Vec<(u32, &EntityDef)> = project
            .entities
            .iter()
            .enumerate()
            .filter(|(_, d)| has_chips(d, &pal.chips))
            .map(|(i, d)| (order[i], d))
            .collect();
        v.sort_unstable_by_key(|(o, _)| *o);
        v.into_iter().map(|(_, d)| d).collect()
    };
    let c = collapsed(PART_CATALOGUE);
    rows.push(Row::Part {
        key: PART_CATALOGUE,
        title: if searching {
            format!("Matches ({})", in_cat.len())
        } else {
            format!("Catalogue ({})", in_cat.len())
        },
        collapsed: c,
    });
    if c {
        return (rows, first_hit, hit_ix);
    }
    let push_types = |rows: &mut Vec<Row>, types: &[&EntityDef], depth: usize| match pal.view {
        PaletteView::List => {
            for d in types {
                rows.push(Row::Type {
                    slug: d.slug.clone(),
                    depth,
                    count: None,
                });
            }
        }
        PaletteView::Grid => {
            for chunk in types.chunks(cols) {
                rows.push(Row::Icons {
                    slugs: chunk.iter().map(|d| d.slug.clone()).collect(),
                    depth,
                });
            }
        }
    };
    if in_cat.is_empty() {
        rows.push(Row::Note(if searching {
            "No matching types".into()
        } else if pal.chips.is_empty() {
            "The project has no entity types".into()
        } else {
            "No type has all these tags".into()
        }));
        return (rows, first_hit, hit_ix);
    }
    if searching {
        push_types(&mut rows, &in_cat, 0);
        return (rows, first_hit, hit_ix);
    }
    let mut root = Node::default();
    for d in &in_cat {
        let mut node = &mut root;
        node.count += 1;
        for seg in category_of(d)
            .split('/')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            node = node.children.entry(seg.to_string()).or_default();
            node.count += 1;
        }
        node.types.push(d);
    }
    fn walk(
        rows: &mut Vec<Row>,
        node: &Node<'_>,
        path: &str,
        depth: usize,
        collapsed: &dyn Fn(&str) -> bool,
        push_types: &PushTypes<'_>,
    ) {
        for (name, child) in ordered(&node.children, depth == 0) {
            let p = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            let c = collapsed(&p);
            rows.push(Row::Category {
                path: p.clone(),
                depth,
                count: child.count,
                collapsed: c,
            });
            if !c {
                walk(rows, child, &p, depth + 1, collapsed, push_types);
                push_types(rows, &child.types, depth + 1);
            }
        }
    }
    // Types right at the root (an empty category path) come first.
    push_types(&mut rows, &root.types, 0);
    walk(&mut rows, &root, "", 0, &collapsed, &push_types);
    (rows, first_hit, hit_ix)
}

/// The types in level `ix` with their counts, by name.
pub fn level_counts(doc: &crate::doc::Document, ix: usize) -> Vec<(Slug, usize)> {
    let mut m: BTreeMap<&Slug, usize> = BTreeMap::new();
    for e in doc.level(ix).all_entities() {
        *m.entry(&e.ty).or_default() += 1;
    }
    let name = |s: &Slug| {
        doc.project
            .entity_def(s.as_str())
            .map_or_else(|| s.to_string(), |d| d.name.to_lowercase())
    };
    let mut v: Vec<(Slug, usize)> = m.into_iter().map(|(s, n)| (s.clone(), n)).collect();
    v.sort_by_key(|(s, _)| name(s));
    v
}

/// Once per frame before the dock: the level's counts (per edit revision
/// and level) and the atlas textures entity icons need (the palette and
/// the canvas draw from them).
pub fn refresh(st: &mut EditorState, rev: u64, ctx: &egui::Context) {
    let EditorState {
        doc,
        view,
        entities: pal,
        atlas,
        ..
    } = st;
    pal.rev = rev;
    let Some(doc) = doc.as_ref() else {
        pal.counts = None;
        return;
    };
    let key = view
        .active_level
        .filter(|&i| i < doc.levels.len())
        .map(|ix| (rev, doc.ids[ix].clone()));
    match key {
        Some(k) if pal.counts.as_ref().map(|c| &c.0) != Some(&k) => {
            let ix = view.active_level.unwrap_or_default();
            pal.counts = Some((k, level_counts(doc, ix)));
        }
        Some(_) => {}
        None => pal.counts = None,
    }
    if pal.icon_tilesets.as_ref().map(|t| t.0) != Some(doc.defs_gen) {
        let mut ts: Vec<Slug> = doc
            .project
            .entities
            .iter()
            .filter_map(|d| d.icon.as_ref().map(|i| i.tileset.clone()))
            .collect();
        ts.sort();
        ts.dedup();
        pal.icon_tilesets = Some((doc.defs_gen, ts));
    }
    if let Some((_, ts)) = &pal.icon_tilesets {
        for slug in ts {
            if !atlas.contains_key(slug.as_str())
                && let Some(t) = doc.project.tileset(slug.as_str())
            {
                super::panels::atlas_texture(atlas, ctx, doc, t);
            }
        }
    }
}

/// Rebuild the rows if anything they show changed.
fn ensure_rows(pal: &mut PaletteState, project: &Project, defs_gen: u64, saved: &EntityPalette) {
    let key = RowsKey {
        defs_gen,
        query: pal.query.clone(),
        chips: saved.chips.clone(),
        collapsed: saved.collapsed.clone(),
        view: saved.view,
        favorites: saved.favorites.clone(),
        recent: saved.recent.clone(),
        counts: pal.counts.as_ref().map(|c| c.0.clone()),
        cols: pal.cols,
    };
    if pal.key.as_ref() == Some(&key) {
        return;
    }
    let counts = pal.counts.as_ref().map(|c| c.1.as_slice());
    let q = fuzzy_query(&pal.query);
    let prev = pal
        .hits
        .as_ref()
        .filter(|(g, pq, _)| *g == defs_gen && !pq.is_empty() && q.starts_with(pq))
        .map(|h| h.2.as_slice());
    if prev.is_some() {
        pal.narrowed += 1;
    }
    if pal.order.as_ref().map(|o| o.0) != Some(defs_gen) {
        pal.order = Some((defs_gen, name_order(project)));
    }
    let order = pal.order.as_ref().map(|o| o.1.as_slice());
    let (rows, first, hits) =
        build_rows_from(project, saved, &pal.query, counts, pal.cols, prev, order);
    pal.rows = rows;
    pal.first_hit = first;
    pal.hits = Some((defs_gen, q, hits));
    pal.key = Some(key);
    pal.rebuilds += 1;
}

/// Make `slug` the brush (a new type starts with no field values; picking
/// the brush's own type keeps them) and switch to the entity tool.
fn pick(view: &mut crate::view::ViewState, actions: &mut Vec<Action>, slug: &Slug) {
    if view.entities.brush.as_ref().map(|b| &b.ty) != Some(slug) {
        view.entities.brush = Some(EntityBrush::of(slug.clone()));
    }
    if view.tool != Tool::Entity {
        actions.push(Action::SetTool(Tool::Entity));
    }
}

/// Enter in the search box (and a script's `"enter": true`): the first hit
/// for the query becomes the brush and the entity tool is asked for. No
/// hit is an error, also shown in the status bar.
pub fn enter(
    pal: &mut PaletteState,
    doc: &crate::doc::Document,
    view: &mut crate::view::ViewState,
    actions: &mut Vec<Action>,
) -> Result<Slug, String> {
    // Before the pane was ever drawn (a script) the width is unknown.
    pal.cols = pal.cols.max(1);
    ensure_rows(pal, &doc.project, doc.defs_gen, &view.entities);
    match pal.first_hit.clone() {
        Some(s) => {
            pal.keep_map_keys = view.tool != Tool::Entity;
            pick(view, actions, &s);
            view.set_status(format!("Brush: {s} (Enter places it)"), false);
            Ok(s)
        }
        None => {
            let m = format!("No type matches the search {:?}", pal.query);
            view.set_status(m.clone(), true);
            Err(m)
        }
    }
}

/// Select every instance of `ty` in level `ix` (the Inspector shows their
/// common fields).
pub fn select_all_of(
    doc: &crate::doc::Document,
    view: &mut crate::view::ViewState,
    ix: usize,
    ty: &Slug,
) {
    let ids: Vec<Id> = doc
        .level(ix)
        .all_entities()
        .filter(|e| e.ty == *ty)
        .map(|e| e.id.clone())
        .collect();
    view.selection.set_entities(ids);
}

/// The hover card of a type: name, slug, category, tags, doc, size,
/// fields.
fn hover_card(ui: &mut egui::Ui, def: &EntityDef) {
    ui.strong(&def.name);
    ui.weak(format!(
        "{} · {}",
        def.slug,
        def.category.as_deref().unwrap_or(UNCATEGORIZED)
    ));
    if !def.tags.is_empty() {
        ui.label(format!("Tags: {}", def.tags.join(", ")));
    }
    if let Some(d) = &def.doc {
        ui.label(d);
    }
    let resizable = match def.resizable {
        [true, true] => ", resizable",
        [true, false] => ", resizable in x",
        [false, true] => ", resizable in y",
        _ => "",
    };
    ui.label(format!(
        "Size {} x {} cells{resizable}",
        def.size[0], def.size[1]
    ));
    if let Some(l) = &def.limit {
        ui.label(format!(
            "At most {} per {:?} ({:?})",
            l.max, l.scope, l.behavior
        ));
    }
    if !def.fields.is_empty() {
        ui.add_space(4.0);
        for f in &def.fields {
            ui.label(
                RichText::new(format!(
                    "{}: {}{}",
                    f.slug,
                    f.ty.name(),
                    if f.array { "[]" } else { "" }
                ))
                .monospace()
                .size(11.0),
            );
        }
    }
}

/// The Palette pane on an entities layer.
pub fn palette(ui: &mut egui::Ui, st: &mut EditorState) {
    let EditorState {
        doc,
        view,
        entities: pal,
        atlas,
        actions,
        accent,
        ..
    } = st;
    let Some(doc) = doc.as_ref() else {
        ui.weak("No project");
        return;
    };
    pal.rects.clear();
    // ---- search ----
    let id = egui::Id::new("entity palette search");
    let escape = pal.had_focus && ui.input(|i| i.key_pressed(egui::Key::Escape));
    let mut refocus = false;
    if escape && !pal.query.is_empty() {
        // First Escape: clear, keep typing. (egui already dropped the
        // focus; take it back.)
        pal.query.clear();
        refocus = true;
        ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    }
    let r = ui.add(
        egui::TextEdit::singleline(&mut pal.query)
            .id(id)
            .hint_text("Search types (T)…")
            .desired_width(f32::INFINITY),
    );
    pal.search_rect = Some(r.rect);
    if std::mem::take(&mut pal.focus) || refocus {
        r.request_focus();
    }
    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    pal.had_focus = r.has_focus() || refocus;
    // ---- brush, chips, view ----
    let saved = &mut view.entities;
    if pal.tags.as_ref().map(|t| t.0) != Some(doc.defs_gen) {
        let all: BTreeSet<&String> = doc.project.entities.iter().flat_map(|d| &d.tags).collect();
        pal.tags = Some((doc.defs_gen, all.into_iter().cloned().collect()));
    }
    let all_tags: &[String] = pal.tags.as_ref().map_or(&[], |t| &t.1);
    ui.horizontal(|ui| {
        let mut remove = None;
        for (i, c) in saved.chips.iter().enumerate() {
            if ui
                .add(egui::Button::selectable(true, format!("{c} ×")).small())
                .on_hover_text("Remove this tag filter")
                .clicked()
            {
                remove = Some(i);
            }
        }
        if let Some(i) = remove {
            saved.chips.remove(i);
        }
        let free: Vec<&String> = all_tags
            .iter()
            .filter(|t| !saved.chips.contains(t))
            .collect();
        if !free.is_empty() {
            egui::ComboBox::from_id_salt("entity tag chips")
                .selected_text("+ tag")
                .width(70.0)
                .show_ui(ui, |ui| {
                    for t in free {
                        if ui.selectable_label(false, t.as_str()).clicked() {
                            saved.chips.push(t.clone());
                        }
                    }
                });
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (v, label, tip) in [
                (PaletteView::List, "☰", "List view"),
                (PaletteView::Grid, "▦", "Icon grid"),
            ] {
                if ui
                    .add(egui::Button::selectable(saved.view == v, label).small())
                    .on_hover_text(tip)
                    .clicked()
                {
                    saved.view = v;
                }
            }
        });
    });
    // The brush.
    if let Some(b) = &saved.brush
        && let Some(def) = doc.project.entity_def(b.ty.as_str())
    {
        ui.horizontal(|ui| {
            ui.weak("Brush");
            let (r, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
            type_swatch(ui.painter(), doc, atlas, def, r);
            ui.label(&def.name);
            if !b.fields.is_empty() {
                ui.weak(format!("{} field values", b.fields.len()));
            }
        });
    }
    ui.separator();
    pal.cols = (((ui.available_width() - 16.0) / CELL_W).floor() as usize).max(1);
    ensure_rows(pal, &doc.project, doc.defs_gen, saved);
    if enter {
        // A miss is in the status bar, and the search box keeps the
        // keyboard so the query can be fixed.
        if self::enter(pal, doc, view, actions).is_err() {
            pal.focus = true;
        }
    }
    // ---- rows ----
    let level = view.active_level.filter(|&i| i < doc.levels.len());
    let brush = view.entities.brush.as_ref().map(|b| b.ty.clone());
    let mut toggle: Option<String> = None;
    let mut picked: Option<Slug> = None;
    let mut fav: Option<Slug> = None;
    let mut select: Option<Slug> = None;
    let mut hook: Option<(bool, Slug)> = None;
    let rows = &pal.rows;
    let rects = &mut pal.rects;
    let favorites = &view.entities.favorites;
    egui::ScrollArea::vertical()
        .id_salt("entity palette rows")
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, rows.len(), |ui, range| {
            for row in &rows[range] {
                let (rect, resp) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
                let p = ui.painter_at(rect.expand(1.0));
                let text = ui.visuals().text_color();
                match row {
                    Row::Part {
                        key,
                        title,
                        collapsed,
                    } => {
                        rects.insert((*key).into(), rect);
                        if resp.hovered() {
                            p.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
                        }
                        p.text(
                            rect.left_center() + Vec2::new(4.0, 0.0),
                            Align2::LEFT_CENTER,
                            format!("{} {title}", if *collapsed { "⏵" } else { "⏷" }),
                            FontId::proportional(13.0),
                            ui.visuals().strong_text_color(),
                        );
                        if resp.clicked() {
                            toggle = Some((*key).into());
                        }
                    }
                    Row::Category {
                        path,
                        depth,
                        count,
                        collapsed,
                    } => {
                        rects.insert(path.clone(), rect);
                        if resp.hovered() {
                            p.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
                        }
                        let name = path.rsplit('/').next().unwrap_or(path);
                        p.text(
                            rect.left_center() + Vec2::new(8.0 + 12.0 * *depth as f32, 0.0),
                            Align2::LEFT_CENTER,
                            format!("{} {name}", if *collapsed { "⏵" } else { "⏷" }),
                            FontId::proportional(13.0),
                            text,
                        );
                        p.text(
                            rect.right_center() - Vec2::new(6.0, 0.0),
                            Align2::RIGHT_CENTER,
                            count.to_string(),
                            FontId::proportional(11.0),
                            ui.visuals().weak_text_color(),
                        );
                        if resp.clicked() {
                            toggle = Some(path.clone());
                        }
                    }
                    Row::Note(s) => {
                        p.text(
                            rect.left_center() + Vec2::new(12.0, 0.0),
                            Align2::LEFT_CENTER,
                            s,
                            FontId::proportional(12.0),
                            ui.visuals().weak_text_color(),
                        );
                    }
                    Row::Type { slug, depth, count } => {
                        let Some(def) = doc.project.entity_def(slug.as_str()) else {
                            continue;
                        };
                        rects.entry(slug.to_string()).or_insert(rect);
                        let selected = brush.as_ref() == Some(slug);
                        if selected {
                            p.rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
                        } else if resp.hovered() {
                            p.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
                        }
                        let x0 = rect.min.x + 8.0 + 12.0 * *depth as f32;
                        let icon =
                            Rect::from_min_size(pos2(x0, rect.min.y + 3.0), Vec2::splat(20.0));
                        type_swatch(&p, doc, atlas, def, icon);
                        let mut right = rect.max.x - 4.0;
                        // ★ on every row.
                        let star = Rect::from_min_max(
                            pos2(right - 18.0, rect.min.y),
                            pos2(right, rect.max.y),
                        );
                        let sr = ui.interact(star, resp.id.with("star"), Sense::click());
                        let is_fav = favorites.contains(slug);
                        p.text(
                            star.center(),
                            Align2::CENTER_CENTER,
                            if is_fav { "★" } else { "☆" },
                            FontId::proportional(14.0),
                            if is_fav {
                                *accent
                            } else {
                                ui.visuals().weak_text_color()
                            },
                        );
                        if sr
                            .on_hover_text(if is_fav { "Unfavorite" } else { "Favorite" })
                            .clicked()
                        {
                            fav = Some(slug.clone());
                        }
                        right -= 20.0;
                        if let Some(n) = count {
                            let b = Rect::from_min_max(
                                pos2(right - 34.0, rect.min.y + 3.0),
                                pos2(right, rect.max.y - 3.0),
                            );
                            let br = ui.interact(b, resp.id.with("count"), Sense::click());
                            p.rect_filled(
                                b,
                                3.0,
                                if br.hovered() {
                                    ui.visuals().widgets.hovered.bg_fill
                                } else {
                                    ui.visuals().widgets.inactive.bg_fill
                                },
                            );
                            p.text(
                                b.center(),
                                Align2::CENTER_CENTER,
                                format!("×{n}"),
                                FontId::proportional(12.0),
                                text,
                            );
                            rects.insert(format!("count:{slug}"), b);
                            if br.on_hover_text("Select them all").clicked() {
                                select = Some(slug.clone());
                            }
                            right -= 38.0;
                        }
                        let name_rect = Rect::from_min_max(
                            pos2(icon.max.x + 6.0, rect.min.y),
                            pos2(right, rect.max.y),
                        );
                        let pc = p.with_clip_rect(name_rect);
                        let g =
                            pc.layout_no_wrap(def.name.clone(), FontId::proportional(13.0), text);
                        let w = g.size().x;
                        pc.galley(
                            pos2(name_rect.min.x, rect.center().y - g.size().y / 2.0),
                            g,
                            text,
                        );
                        pc.text(
                            pos2(name_rect.min.x + w + 6.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            slug.as_str(),
                            FontId::proportional(11.0),
                            ui.visuals().weak_text_color(),
                        );
                        if resp.clicked() {
                            picked = Some(slug.clone());
                        }
                        let resp = resp.on_hover_ui(|ui| hover_card(ui, def));
                        resp.context_menu(|ui| {
                            if ui
                                .button(if is_fav { "Unfavorite" } else { "Favorite" })
                                .clicked()
                            {
                                fav = Some(slug.clone());
                                ui.close();
                            }
                            if ui.button("Edit type…").clicked() {
                                hook = Some((true, slug.clone()));
                                ui.close();
                            }
                            if ui.button("Duplicate type").clicked() {
                                hook = Some((false, slug.clone()));
                                ui.close();
                            }
                        });
                    }
                    Row::Icons { slugs, depth } => {
                        let x0 = rect.min.x + 8.0 + 12.0 * *depth as f32;
                        for (i, slug) in slugs.iter().enumerate() {
                            let Some(def) = doc.project.entity_def(slug.as_str()) else {
                                continue;
                            };
                            let cell = Rect::from_min_size(
                                pos2(x0 + i as f32 * CELL_W, rect.min.y),
                                Vec2::new(CELL_W - 2.0, ROW_H - 2.0),
                            );
                            rects.entry(slug.to_string()).or_insert(cell);
                            let cr = ui.interact(cell, resp.id.with(i), Sense::click());
                            if brush.as_ref() == Some(slug) {
                                p.rect_filled(cell, 3.0, ui.visuals().selection.bg_fill);
                            } else if cr.hovered() {
                                p.rect_filled(cell, 3.0, ui.visuals().widgets.hovered.bg_fill);
                            }
                            type_swatch(&p, doc, atlas, def, cell.shrink(2.0));
                            if favorites.contains(slug) {
                                p.text(
                                    cell.right_top(),
                                    Align2::RIGHT_TOP,
                                    "★",
                                    FontId::proportional(9.0),
                                    *accent,
                                );
                            }
                            if cr.clicked() {
                                picked = Some(slug.clone());
                            }
                            let is_fav = favorites.contains(slug);
                            let cr = cr.on_hover_ui(|ui| hover_card(ui, def));
                            cr.context_menu(|ui| {
                                if ui
                                    .button(if is_fav { "Unfavorite" } else { "Favorite" })
                                    .clicked()
                                {
                                    fav = Some(slug.clone());
                                    ui.close();
                                }
                                if ui.button("Edit type…").clicked() {
                                    hook = Some((true, slug.clone()));
                                    ui.close();
                                }
                                if ui.button("Duplicate type").clicked() {
                                    hook = Some((false, slug.clone()));
                                    ui.close();
                                }
                            });
                        }
                    }
                }
            }
        });
    if let Some(k) = toggle
        && !view.entities.collapsed.remove(&k)
    {
        view.entities.collapsed.insert(k);
    }
    if let Some(s) = fav {
        view.entities.toggle_favorite(&s);
    }
    if let Some(s) = picked {
        pick(view, actions, &s);
    }
    if let (Some(s), Some(ix)) = (select, level) {
        select_all_of(doc, view, ix, &s);
    }
    // Right click: Edit type… opens it in the Types pane; Duplicate type
    // copies it under a new slug and opens the copy there.
    match hook {
        Some((true, s)) => actions.push(Action::OpenTypes(Some(s))),
        Some((false, s)) => actions.push(Action::DuplicateType(s)),
        None => {}
    }
}

/// A type's icon (from its atlas tile) or, without one, a square of its
/// color.
pub fn type_swatch(
    p: &egui::Painter,
    doc: &crate::doc::Document,
    atlas: &crate::viewport::Atlas,
    def: &EntityDef,
    r: Rect,
) {
    let c = parse_color(def.color.as_str());
    let col = Color32::from_rgb(c[0], c[1], c[2]);
    let drawn = def
        .icon
        .as_ref()
        .is_some_and(|icon| crate::viewport::draw_icon(p, doc, atlas, icon, r));
    if !drawn {
        let sq = r.shrink(2.0);
        p.rect_filled(sq, 3.0, col.gamma_multiply(0.85));
    }
    p.rect_stroke(
        r.shrink(1.0),
        3.0,
        Stroke::new(1.0, col),
        StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Document;

    fn slug(s: &str) -> Slug {
        Slug::new(s).unwrap()
    }

    /// A type with these keys (the rest default).
    fn def(s: &str, name: &str, category: Option<&str>, tags: &[&str]) -> EntityDef {
        serde_json::from_value(serde_json::json!({
            "slug": s, "name": name, "category": category, "tags": tags,
            "size": [1, 1], "color": "#808080" }))
        .unwrap()
    }

    fn project(defs: Vec<EntityDef>) -> Project {
        let mut p: Project = serde_json::from_value(serde_json::json!({
            "name": "t", "grid_size": 16, "seed": 1 }))
        .unwrap();
        p.entities = defs;
        p
    }

    fn sample() -> Project {
        project(vec![
            def(
                "locked_door",
                "Locked Door",
                Some("Puzzle/Doors"),
                &["object", "puzzle"],
            ),
            def(
                "floor_button",
                "Floor Button",
                Some("Puzzle/Switches"),
                &["object"],
            ),
            def("torch", "Torch", Some("Decor"), &["light"]),
            def("doormat", "Mat", None, &["decor"]),
            def("rune", "Rune", None, &["door"]),
            def("odd", "Odd", Some("Ruins/Door frames"), &[]),
        ])
    }

    fn types(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .flat_map(|r| match r {
                Row::Type { slug, .. } => vec![slug.to_string()],
                Row::Icons { slugs, .. } => slugs.iter().map(|s| s.to_string()).collect(),
                _ => vec![],
            })
            .collect()
    }

    fn list() -> EntityPalette {
        EntityPalette {
            view: PaletteView::List,
            ..Default::default()
        }
    }

    #[test]
    fn search_ranks_name_then_slug_then_category_then_tag() {
        let p = sample();
        let d = |s: &str| p.entity_def(s).unwrap();
        assert_eq!(rank(d("locked_door"), "door").unwrap().0, 3);
        assert_eq!(rank(d("doormat"), "door").unwrap().0, 2);
        assert_eq!(rank(d("odd"), "door").unwrap().0, 1);
        assert_eq!(rank(d("rune"), "door").unwrap().0, 0);
        assert_eq!(rank(d("torch"), "door"), None);
        let (rows, first) = build_rows(&p, &list(), "door", None, 4);
        assert_eq!(first, Some(slug("locked_door")));
        // The catalogue becomes the ranked matches.
        assert_eq!(types(&rows), ["locked_door", "doormat", "odd", "rune"]);
        assert!(matches!(&rows.last(), Some(Row::Type { .. })));
    }

    #[test]
    fn typing_filters_every_part() {
        let p = sample();
        let pal = EntityPalette {
            favorites: vec![slug("torch"), slug("locked_door")],
            recent: vec![slug("floor_button"), slug("torch")],
            ..list()
        };
        let counts = vec![(slug("locked_door"), 2), (slug("torch"), 5)];
        let (rows, _) = build_rows(&p, &pal, "", Some(&counts), 4);
        let parts: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Part { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert_eq!(
            parts,
            [PART_FAVORITES, PART_RECENT, PART_LEVEL, PART_CATALOGUE]
        );
        // "torch" leaves only torch in each part.
        let (rows, _) = build_rows(&p, &pal, "torch", Some(&counts), 4);
        assert_eq!(types(&rows), ["torch", "torch", "torch", "torch"]);
        assert!(rows.contains(&Row::Type {
            slug: slug("torch"),
            depth: 0,
            count: Some(5)
        }));
        // A query with no hit in a part hides that part.
        let (rows, _) = build_rows(&p, &pal, "button", Some(&counts), 4);
        assert_eq!(types(&rows), ["floor_button", "floor_button"]);
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r, Row::Part { key, .. } if *key == PART_FAVORITES))
        );
        let (rows, first) = build_rows(&p, &pal, "zzz", Some(&counts), 4);
        assert_eq!(first, None);
        assert!(rows.contains(&Row::Note("No matching types".into())));
    }

    #[test]
    fn the_tree_follows_categories_with_uncategorized_last() {
        let p = sample();
        let (rows, _) = build_rows(&p, &list(), "", None, 4);
        let cats: Vec<(String, usize, usize)> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Category {
                    path, depth, count, ..
                } => Some((path.clone(), *depth, *count)),
                _ => None,
            })
            .collect();
        assert_eq!(
            cats,
            [
                ("Decor".into(), 0, 1),
                ("Puzzle".into(), 0, 2),
                ("Puzzle/Doors".into(), 1, 1),
                ("Puzzle/Switches".into(), 1, 1),
                ("Ruins".into(), 0, 1),
                ("Ruins/Door frames".into(), 1, 1),
                (UNCATEGORIZED.into(), 0, 2),
            ]
        );
        assert_eq!(
            types(&rows),
            [
                "torch",
                "locked_door",
                "floor_button",
                "odd",
                "doormat",
                "rune"
            ]
        );
        // Collapsing a category hides what's under it; a part, everything.
        let mut pal = list();
        pal.collapsed.insert("Puzzle".into());
        let (rows, _) = build_rows(&p, &pal, "", None, 4);
        assert_eq!(types(&rows), ["torch", "odd", "doormat", "rune"]);
        assert!(rows.contains(&Row::Category {
            path: "Puzzle".into(),
            depth: 0,
            count: 2,
            collapsed: true
        }));
        pal.collapsed.insert(PART_CATALOGUE.into());
        let (rows, _) = build_rows(&p, &pal, "", None, 4);
        assert!(types(&rows).is_empty());
        // Searching ignores category collapse (the matches are flat).
        pal.collapsed.remove(PART_CATALOGUE);
        let (rows, _) = build_rows(&p, &pal, "button", None, 4);
        assert_eq!(types(&rows), ["floor_button"]);
    }

    #[test]
    fn chips_are_an_and_filter_and_grid_rows_hold_cols_icons() {
        let p = sample();
        let mut pal = list();
        pal.chips = vec!["object".into()];
        let (rows, _) = build_rows(&p, &pal, "", None, 4);
        assert_eq!(types(&rows), ["locked_door", "floor_button"]);
        pal.chips.push("puzzle".into());
        let (rows, first) = build_rows(&p, &pal, "", None, 4);
        assert_eq!(types(&rows), ["locked_door"]);
        assert_eq!(first, Some(slug("locked_door")));
        pal.chips = vec!["nothing".into()];
        let (rows, _) = build_rows(&p, &pal, "", None, 4);
        assert!(rows.contains(&Row::Note("No type has all these tags".into())));
        // Grid: types in rows of `cols`.
        let many = project(
            (0..10)
                .map(|i| def(&format!("t{i}"), &format!("T {i:02}"), Some("A"), &[]))
                .collect(),
        );
        let (rows, _) = build_rows(&many, &EntityPalette::default(), "", None, 4);
        let sizes: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Icons { slugs, .. } => Some(slugs.len()),
                _ => None,
            })
            .collect();
        assert_eq!(sizes, [4, 4, 2]);
    }

    #[test]
    fn saved_state_round_trips_and_drops_unknown_types() {
        let p = sample();
        let pal = EntityPalette {
            favorites: vec![slug("torch")],
            recent: vec![slug("rune"), slug("torch")],
            collapsed: ["Puzzle".to_string(), PART_RECENT.to_string()].into(),
            chips: vec!["object".into()],
            view: PaletteView::List,
            brush: Some(EntityBrush::of(slug("rune"))),
        };
        let ui = super::super::UiPalette::from_palette(&pal);
        let json = serde_json::to_string(&ui).unwrap();
        let back: super::super::UiPalette = serde_json::from_str(&json).unwrap();
        assert_eq!(back.to_palette(&p), pal);
        // A ui.json from before the palette: every key defaults.
        let old: super::super::UiPalette = serde_json::from_str("{}").unwrap();
        assert_eq!(old.to_palette(&p), EntityPalette::default());
        // Types that are gone (or never were) are dropped.
        let stale: super::super::UiPalette = serde_json::from_value(serde_json::json!({
            "favorites": ["torch", "gone", "torch"], "recent": ["Not A Slug", "rune"],
            "brush": "gone", "view": "grid" }))
        .unwrap();
        let p2 = stale.to_palette(&p);
        assert_eq!(p2.favorites, [slug("torch")]);
        assert_eq!(p2.recent, [slug("rune")]);
        assert_eq!(p2.brush, None);
        // Recent keeps the last eight, most recent first.
        let mut pal = EntityPalette::default();
        for i in 0..10 {
            pal.placed(&slug(&format!("t{i}")));
        }
        pal.placed(&slug("t5"));
        assert_eq!(pal.recent.len(), crate::tools::RECENT_MAX);
        assert_eq!(pal.recent[0], "t5");
        assert_eq!(pal.recent.iter().filter(|s| *s == "t5").count(), 1);
    }

    /// The mini fixture in an `EditorState`, and a frame driver drawing the
    /// palette.
    fn state() -> (tempfile::TempDir, EditorState) {
        fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
            std::fs::create_dir_all(to).unwrap();
            for e in std::fs::read_dir(from).unwrap() {
                let p = e.unwrap().path();
                let to = to.join(p.file_name().unwrap());
                if p.is_dir() {
                    copy_dir(&p, &to);
                } else {
                    std::fs::copy(&p, &to).unwrap();
                }
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mini.worlds");
        copy_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../worlds-core/tests/fixtures/mini.worlds"),
            &dir,
        );
        let doc = Document::open(&dir).unwrap();
        let mut view = crate::view::ViewState::new();
        view.sync_layers(&doc, None);
        view.active_level = doc.level_ix("crevasse");
        view.active_layer = crate::view::first_entities_layer(&doc);
        view.entities.view = PaletteView::List;
        let st = EditorState {
            doc: Some(doc),
            view,
            ..Default::default()
        };
        (tmp, st)
    }

    struct Driver {
        ctx: egui::Context,
        t: f64,
    }

    impl Driver {
        fn frame(&mut self, st: &mut EditorState, events: Vec<egui::Event>) {
            self.t += 0.1;
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(400.0, 900.0))),
                time: Some(self.t),
                focused: true,
                events,
                ..Default::default()
            };
            let mut out = self.ctx.run_ui(raw, |ui| {
                refresh(st, 1, ui.ctx());
                palette(ui, st);
            });
            out.textures_delta.clear();
        }

        fn key(&mut self, st: &mut EditorState, key: egui::Key) {
            let k = |pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Default::default(),
            };
            self.frame(st, vec![k(true), k(false)]);
        }

        fn click(&mut self, st: &mut EditorState, p: egui::Pos2) {
            for pressed in [true, false] {
                self.frame(
                    st,
                    vec![
                        egui::Event::PointerMoved(p),
                        egui::Event::PointerButton {
                            pos: p,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Default::default(),
                        },
                    ],
                );
            }
            self.frame(st, vec![]);
        }

        fn focused(&self) -> bool {
            self.ctx
                .memory(|m| m.has_focus(egui::Id::new("entity palette search")))
        }
    }

    #[test]
    fn search_focus_escape_and_enter() {
        let (_tmp, mut st) = state();
        let mut d = Driver {
            ctx: egui::Context::default(),
            t: 0.0,
        };
        d.frame(&mut st, vec![]);
        assert!(!d.focused());
        // Entering the tool (or T) gives the search box the keyboard.
        st.entities.focus = true;
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        assert!(d.focused());
        d.frame(&mut st, vec![egui::Event::Text("door".into())]);
        assert_eq!(st.entities.query, "door");
        assert_eq!(st.entities.first_hit, Some(slug("locked_door")));
        // Escape clears and keeps typing; a second Escape lets go.
        d.key(&mut st, egui::Key::Escape);
        d.frame(&mut st, vec![]);
        assert_eq!(st.entities.query, "");
        assert!(d.focused(), "first Escape keeps the keyboard");
        d.key(&mut st, egui::Key::Escape);
        d.frame(&mut st, vec![]);
        assert!(!d.focused(), "second Escape releases it");
        // Typing doesn't reach it until T again.
        d.frame(&mut st, vec![egui::Event::Text("x".into())]);
        assert_eq!(st.entities.query, "");
        // T, type, Enter: the first hit is the brush, the entity tool is
        // asked for, and the map gets the keyboard (a second Enter places).
        st.entities.focus = true;
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![egui::Event::Text("butt".into())]);
        d.key(&mut st, egui::Key::Enter);
        assert_eq!(
            st.view.entities.brush.as_ref().map(|b| b.ty.as_str()),
            Some("floor_button")
        );
        assert!(st.actions.contains(&Action::SetTool(Tool::Entity)));
        assert!(!d.focused());
    }

    /// Enter with no hit leaves the brush and the tool alone and keeps the
    /// keyboard in the search box (to fix the query); Escape then still
    /// clears it first.
    #[test]
    fn enter_with_no_hit_keeps_typing() {
        let (_tmp, mut st) = state();
        let mut d = Driver {
            ctx: egui::Context::default(),
            t: 0.0,
        };
        st.entities.focus = true;
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![]);
        d.frame(&mut st, vec![egui::Event::Text("qqzx".into())]);
        d.key(&mut st, egui::Key::Enter);
        d.frame(&mut st, vec![]);
        assert!(st.view.entities.brush.is_none());
        assert!(st.actions.is_empty(), "{:?}", st.actions);
        assert!(st.view.status.as_ref().is_some_and(|m| m.error));
        assert!(d.focused(), "still typing");
        d.frame(&mut st, vec![egui::Event::Text("y".into())]);
        assert_eq!(st.entities.query, "qqzxy");
        d.key(&mut st, egui::Key::Escape);
        d.frame(&mut st, vec![]);
        assert_eq!(st.entities.query, "");
        assert!(d.focused());
    }

    /// `enter` (what a script's `"enter": true` calls) works before the
    /// pane was ever drawn: the first hit of the query becomes the brush
    /// and the entity tool is asked for; no hit is an error and changes
    /// nothing.
    #[test]
    fn enter_picks_the_first_hit_without_a_frame() {
        let (_tmp, mut st) = state();
        assert_eq!(st.entities.cols, 0, "never drawn");
        st.entities.query = "butt".into();
        let EditorState {
            doc,
            view,
            entities,
            actions,
            ..
        } = &mut st;
        let doc = doc.as_ref().unwrap();
        assert_eq!(
            enter(entities, doc, view, actions).unwrap().as_str(),
            "floor_button"
        );
        assert_eq!(
            view.entities.brush.as_ref().map(|b| b.ty.as_str()),
            Some("floor_button")
        );
        assert!(actions.contains(&Action::SetTool(Tool::Entity)));
        entities.query = "qqzx".into();
        let e = enter(entities, doc, view, actions).unwrap_err();
        assert!(e.contains("qqzx"), "{e}");
        assert_eq!(
            view.entities.brush.as_ref().map(|b| b.ty.as_str()),
            Some("floor_button"),
            "a miss keeps the brush"
        );
    }

    #[test]
    fn rows_are_built_once_and_clicks_pick_star_select_and_collapse() {
        let (_tmp, mut st) = state();
        let mut d = Driver {
            ctx: egui::Context::default(),
            t: 0.0,
        };
        for _ in 0..3 {
            d.frame(&mut st, vec![]);
        }
        let built = st.entities.rebuilds;
        for _ in 0..5 {
            d.frame(&mut st, vec![]);
        }
        assert_eq!(st.entities.rebuilds, built, "nothing changed: no rebuild");
        st.entities.query = "lock".into();
        d.frame(&mut st, vec![]);
        assert_eq!(st.entities.rebuilds, built + 1);
        st.entities.query.clear();
        d.frame(&mut st, vec![]);
        // A click on a catalogue row sets the brush.
        let r = st.entities.rects["locked_door"];
        d.click(&mut st, r.center());
        assert_eq!(
            st.view.entities.brush.as_ref().map(|b| b.ty.as_str()),
            Some("locked_door")
        );
        // The star favorites it.
        let r = st.entities.rects["locked_door"];
        d.click(&mut st, pos2(r.max.x - 13.0, r.center().y));
        assert_eq!(st.view.entities.favorites, [slug("locked_door")]);
        // "In this level": the count button selects every instance.
        let r = st.entities.rects["count:locked_door"];
        d.click(&mut st, r.center());
        assert_eq!(
            st.view.selection.entity_ids(),
            [Id::new("doorcrev22").unwrap()]
        );
        // A part header collapses it.
        let r = st.entities.rects[PART_LEVEL];
        d.click(&mut st, r.center());
        assert!(st.view.entities.collapsed.contains(PART_LEVEL));
        assert!(!st.entities.rects.contains_key("count:locked_door"));
    }

    #[test]
    fn a_longer_query_rescores_only_the_last_hits_and_door_hits_are_real() {
        let mut p = sample();
        crate::doc::synthesize_entities(&mut p, 1000, None);
        let pal = list();
        let (_, _, h1) = build_rows_from(&p, &pal, "do", None, 4, None, None);
        assert!(h1.len() < p.entities.len());
        let order = name_order(&p);
        let narrowed = build_rows_from(&p, &pal, "door", None, 4, Some(&h1), Some(&order));
        let full = build_rows_from(&p, &pal, "door", None, 4, None, None);
        assert_eq!(narrowed, full, "the same rows from the last hits alone");
        // Every "door" hit has it as a real substring of its name, slug,
        // category or a tag (no scattered-letter matches): "Golden Statue
        // 52" is in `.../Doors/...` or tagged `door`.
        let q = fuzzy_query("door");
        for &i in &full.2 {
            let d = &p.entities[i];
            let hay = format!(
                "{} {} {} {}",
                d.name,
                d.slug,
                d.category.clone().unwrap_or_default(),
                d.tags.join(" ")
            )
            .to_lowercase();
            assert!(hay.contains("door"), "{hay}");
            assert!(rank_chars(d, &q).is_some());
        }
    }

    #[test]
    fn typing_on_narrows_the_rebuild() {
        let (_tmp, mut st) = state();
        let mut d = Driver {
            ctx: egui::Context::default(),
            t: 0.0,
        };
        d.frame(&mut st, vec![]);
        st.entities.query = "lo".into();
        d.frame(&mut st, vec![]);
        let n = st.entities.narrowed;
        st.entities.query = "loc".into();
        d.frame(&mut st, vec![]);
        assert_eq!(st.entities.narrowed, n + 1, "appended: only the last hits");
        assert_eq!(st.entities.first_hit, Some(slug("locked_door")));
        st.entities.query = "b".into();
        d.frame(&mut st, vec![]);
        assert_eq!(st.entities.narrowed, n + 1, "a new query scores everything");
        assert_eq!(st.entities.first_hit, Some(slug("floor_button")));
    }
}
