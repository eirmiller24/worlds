//! `worlds-cli check-export`: checks an export directory against the export
//! format (`docs/format.md`, "export/"), without the game.
//!
//! Everything is read as plain JSON, never through `worlds_core`'s writer:
//! the point is an independent reading of the files the game gets. Problems
//! carry a kind: `format` (shape, types, geometry that doesn't add up),
//! `reference` (a file, image, atlas tile or entity type that isn't there),
//! `stale` (`--fresh`: differs from a fresh bake). Annotation links that name nothing exported are warnings
//! (kind `annotation`), never problems.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

/// What a finding is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Format,
    Reference,
    Stale,
    Annotation,
}

/// One problem or warning: `file` relative to the export directory,
/// `path` inside it (empty for the whole file).
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub file: String,
    pub path: String,
    pub message: String,
    pub kind: Kind,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}: {}", self.file, self.message)
        } else {
            write!(f, "{}: {}: {}", self.file, self.path, self.message)
        }
    }
}

/// Every finding of one check.
#[derive(Debug, Default)]
pub struct Report {
    pub problems: Vec<Finding>,
    pub warnings: Vec<Finding>,
    /// Level files read.
    pub levels: usize,
}

impl Report {
    pub fn problem(
        &mut self,
        kind: Kind,
        file: &str,
        path: impl Into<String>,
        msg: impl Into<String>,
    ) {
        self.problems.push(Finding {
            file: file.to_string(),
            path: path.into(),
            message: msg.into(),
            kind,
        });
    }

    /// An annotation warning (link or tag).
    pub fn warn(&mut self, file: &str, path: impl Into<String>, msg: impl Into<String>) {
        self.warnings.push(Finding {
            file: file.to_string(),
            path: path.into(),
            message: msg.into(),
            kind: Kind::Annotation,
        });
    }

    pub fn to_json(&self) -> Value {
        json!({
            "ok": self.problems.is_empty(),
            "levels": self.levels,
            "problems": self.problems,
            "warnings": self.warnings,
        })
    }
}

/// A note as read from the export, with where it was found.
struct NoteAt {
    file: String,
    path: String,
    note: Value,
}

/// Path segment for a list item named by a key when it has one.
fn item(list: &str, v: &Value, key: &str, i: usize) -> String {
    match v.get(key).and_then(Value::as_str) {
        Some(s) => format!("{list}[{s}]"),
        None => format!("{list}[{i}]"),
    }
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Typed reads that report a format problem when the key is missing or of
/// the wrong type.
struct Reader<'r> {
    report: &'r mut Report,
    file: String,
}

impl Reader<'_> {
    fn bad(&mut self, path: &str, msg: impl Into<String>) {
        self.report.problem(Kind::Format, &self.file, path, msg);
    }

    fn key<'v>(&mut self, obj: &'v Value, prefix: &str, key: &str) -> Option<&'v Value> {
        let v = obj.get(key);
        if v.is_none() {
            self.bad(prefix, format!("missing key {key:?}"));
        }
        v
    }

    fn str<'v>(&mut self, obj: &'v Value, prefix: &str, key: &str) -> Option<&'v str> {
        let v = self.key(obj, prefix, key)?;
        let s = v.as_str();
        if s.is_none() {
            self.bad(&join(prefix, key), "expected a string");
        }
        s
    }

    fn int(&mut self, obj: &Value, prefix: &str, key: &str) -> Option<i64> {
        let v = self.key(obj, prefix, key)?;
        let n = v.as_i64();
        if n.is_none() {
            self.bad(&join(prefix, key), "expected an integer");
        }
        n
    }

    fn arr<'v>(&mut self, obj: &'v Value, prefix: &str, key: &str) -> Option<&'v Vec<Value>> {
        let v = self.key(obj, prefix, key)?;
        let a = v.as_array();
        if a.is_none() {
            self.bad(&join(prefix, key), "expected an array");
        }
        a
    }

    fn obj<'v>(
        &mut self,
        obj: &'v Value,
        prefix: &str,
        key: &str,
    ) -> Option<&'v serde_json::Map<String, Value>> {
        let v = self.key(obj, prefix, key)?;
        let o = v.as_object();
        if o.is_none() {
            self.bad(&join(prefix, key), "expected an object");
        }
        o
    }

    /// An array of exactly `N` integers.
    fn ints<const N: usize>(&mut self, obj: &Value, prefix: &str, key: &str) -> Option<[i64; N]> {
        let v = self.key(obj, prefix, key)?;
        let out = ints_of::<N>(v);
        if out.is_none() {
            self.bad(&join(prefix, key), format!("expected {N} integers"));
        }
        out
    }

    fn format_one(&mut self, obj: &Value) {
        match obj.get("format").and_then(Value::as_i64) {
            Some(1) => {}
            Some(n) => self.bad("format", format!("format {n}; this checker reads format 1")),
            None => self.bad("format", "missing or not an integer"),
        }
    }
}

fn ints_of<const N: usize>(v: &Value) -> Option<[i64; N]> {
    let a = v.as_array()?;
    if a.len() != N {
        return None;
    }
    let mut out = [0; N];
    for (o, x) in out.iter_mut().zip(a) {
        *o = x.as_i64()?;
    }
    Some(out)
}

/// Read and parse a JSON file of the export. `Ok(None)` (with a problem
/// recorded) when it is missing or isn't JSON; `Err` when it can't be read.
fn read_json(dir: &Path, rel: &str, report: &mut Report) -> anyhow::Result<Option<Value>> {
    let path = dir.join(rel);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            report.problem(Kind::Format, rel, "", "missing");
            return Ok(None);
        }
        Err(e) => return Err(anyhow::anyhow!("reading {}: {e}", path.display())),
    };
    match serde_json::from_str(&text) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            report.problem(Kind::Format, rel, "", format!("not JSON: {e}"));
            Ok(None)
        }
    }
}

/// Check the export in `dir`. `Err` only for I/O errors (the directory or a
/// file can't be read); everything else is a finding.
pub fn check(dir: &Path) -> anyhow::Result<Report> {
    if !dir.is_dir() {
        anyhow::bail!("{}: not a directory", dir.display());
    }
    let mut report = Report::default();
    let Some(world) = read_json(dir, "world.json", &mut report)? else {
        return Ok(report);
    };
    let mut w = Reader {
        report: &mut report,
        file: "world.json".into(),
    };
    w.format_one(&world);
    w.str(&world, "", "name");
    let grid = w.int(&world, "", "grid_size").unwrap_or(0);
    if grid <= 0 && world.get("grid_size").and_then(Value::as_i64).is_some() {
        w.bad("grid_size", "must be greater than 0");
    }
    // Collision layer and blocking values: both or neither.
    let collision = match world.get("collision_layer") {
        None => None,
        Some(v) => {
            let s = v.as_str().map(str::to_string);
            if s.is_none() {
                w.bad("collision_layer", "expected a string");
            }
            s
        }
    };
    let mut blocking: HashSet<String> = HashSet::new();
    match (world.get("blocking_values"), &collision) {
        (Some(v), Some(_)) => match v.as_array() {
            Some(a) => {
                for (i, x) in a.iter().enumerate() {
                    match x.as_str() {
                        Some(s) => {
                            blocking.insert(s.to_string());
                        }
                        None => w.bad(&format!("blocking_values[{i}]"), "expected a string"),
                    }
                }
            }
            None => w.bad("blocking_values", "expected an array"),
        },
        (None, Some(_)) => w.bad("", "collision_layer is set but blocking_values is missing"),
        (Some(_), None) => w.bad("blocking_values", "present without collision_layer"),
        (None, None) => {}
    }
    // Enums.
    let enums_file = w
        .obj(&world, "", "files")
        .and_then(|f| f.get("enums"))
        .and_then(Value::as_str)
        .map(str::to_string);
    if enums_file.is_none() && world.get("files").is_some_and(Value::is_object) {
        w.bad("files.enums", "missing or not a string");
    }
    // Entity types.
    let mut entity_types = HashSet::new();
    if let Some(types) = w.arr(&world, "", "entity_types") {
        for (i, t) in types.iter().enumerate() {
            let p = format!("entity_types[{i}]");
            if let Some(s) = w.str(t, &p, "slug") {
                entity_types.insert(s.to_string());
            }
            w.str(t, &p, "name");
            w.arr(t, &p, "tags");
        }
    }
    // Worlds and their level index.
    struct Listed {
        world: String,
        slug: String,
        id: String,
        file: String,
        rect: Option<[i64; 4]>,
        path: String,
        index: Value,
    }
    let mut listed: Vec<Listed> = Vec::new();
    let mut notes: Vec<NoteAt> = Vec::new();
    let mut slugs = HashSet::new();
    if let Some(worlds) = w.arr(&world, "", "worlds") {
        for (wi, wv) in worlds.iter().enumerate() {
            let wp = item("worlds", wv, "slug", wi);
            let wslug = w.str(wv, &wp, "slug").unwrap_or("").to_string();
            if let Some(ns) = w.arr(wv, &wp, "notes") {
                for (i, n) in ns.iter().enumerate() {
                    notes.push(NoteAt {
                        file: "world.json".into(),
                        path: item(&format!("{wp}.notes"), n, "id", i),
                        note: n.clone(),
                    });
                }
            }
            let Some(levels) = w.arr(wv, &wp, "levels") else {
                continue;
            };
            for (li, lv) in levels.iter().enumerate() {
                let lp = format!("{wp}.{}", item("levels", lv, "slug", li));
                let slug = w.str(lv, &lp, "slug").unwrap_or("").to_string();
                if !slug.is_empty() && !slugs.insert(slug.clone()) {
                    w.bad(
                        &join(&lp, "slug"),
                        format!("level {slug:?} is listed twice"),
                    );
                }
                let id = w.str(lv, &lp, "id").unwrap_or("").to_string();
                w.str(lv, &lp, "name");
                let file = w.str(lv, &lp, "file").unwrap_or("").to_string();
                let rect = w.ints::<4>(lv, &lp, "rect");
                w.int(lv, &lp, "depth");
                w.arr(lv, &lp, "touching");
                listed.push(Listed {
                    world: wslug.clone(),
                    slug,
                    id,
                    file,
                    rect,
                    path: lp,
                    index: lv.clone(),
                });
            }
        }
    }
    // Touching, recomputed from the rects (same world, positive shared span).
    for a in &listed {
        let (Some(ra), Some(given)) = (a.rect, a.index.get("touching")) else {
            continue;
        };
        if !given.is_array() {
            continue;
        }
        let want: Vec<Value> = listed
            .iter()
            .filter(|b| b.world == a.world && b.slug != a.slug)
            .filter_map(|b| {
                let (edge, span) = touch(ra, b.rect?)?;
                Some(json!({ "level": b.slug, "edge": edge, "span": span }))
            })
            .collect();
        if *given != Value::Array(want.clone()) {
            report.problem(
                Kind::Format,
                "world.json",
                join(&a.path, "touching"),
                format!(
                    "doesn't match the level rects; they give {}",
                    serde_json::to_string(&want).unwrap_or_default()
                ),
            );
        }
    }
    // enums.json
    if let Some(rel) = &enums_file
        && let Some(enums) = read_json(dir, rel, &mut report)?
    {
        let mut e = Reader {
            report: &mut report,
            file: rel.clone(),
        };
        e.format_one(&enums);
        if let Some(list) = e.arr(&enums, "", "enums") {
            for (i, en) in list.iter().enumerate() {
                let p = item("enums", en, "slug", i);
                e.str(en, &p, "slug");
                e.str(en, &p, "name");
                if let Some(vals) = e.arr(en, &p, "values") {
                    for (j, v) in vals.iter().enumerate() {
                        let vp = format!("{p}.values[{j}]");
                        e.str(v, &vp, "slug");
                        e.str(v, &vp, "name");
                    }
                }
            }
        }
    }
    // Level files.
    let listed_files: HashSet<&str> = listed.iter().map(|l| l.file.as_str()).collect();
    let levels_dir = dir.join("levels");
    if levels_dir.is_dir() {
        let mut stale = Vec::new();
        for entry in std::fs::read_dir(&levels_dir)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            let rel = format!("levels/{name}");
            if name.ends_with(".json") && !listed_files.contains(rel.as_str()) {
                stale.push(rel);
            }
        }
        stale.sort();
        for rel in stale {
            report.problem(
                Kind::Format,
                &rel,
                "",
                "not listed in world.json (a stale level file)",
            );
        }
    }
    let mut entity_ids = HashSet::new();
    let mut entity_refs = Vec::new();
    for l in &listed {
        if l.file.is_empty() {
            continue;
        }
        let Some(v) = read_json(dir, &l.file, &mut report)? else {
            continue;
        };
        report.levels += 1;
        let ctx = LevelCtx {
            dir,
            file: &l.file,
            grid,
            world: &l.world,
            slug: &l.slug,
            id: &l.id,
            rect: l.rect,
            collision: collision.as_deref(),
            blocking: &blocking,
            entity_types: &entity_types,
        };
        check_level(&ctx, &l.file, &v, &mut report);
        // open_edges in the index, recomputed from the collision rows.
        check_open_edges(&ctx, &l.path, &l.index, &v, &mut report);
        if let Some(ns) = v.get("notes").and_then(Value::as_array) {
            for (i, n) in ns.iter().enumerate() {
                notes.push(NoteAt {
                    file: l.file.clone(),
                    path: item("notes", n, "id", i),
                    note: n.clone(),
                });
            }
        }
        for e in v
            .get("entities")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(id) = e.get("id").and_then(Value::as_str) {
                entity_ids.insert(id.to_string());
            }
        }
        collect_entity_refs(&l.file, &v, &mut entity_refs);
    }
    for (file, path) in entity_refs {
        report.problem(
            Kind::Reference,
            &file,
            path,
            "entity reference with \"level\": null: no entity has that id",
        );
    }
    // Annotations: shape as a problem, links as warnings.
    let level_slugs: HashSet<&str> = listed.iter().map(|l| l.slug.as_str()).collect();
    let mut ids: HashSet<String> = HashSet::new();
    for n in &notes {
        if let Err(e) = serde_json::from_value::<worlds_core::Annotation>(n.note.clone()) {
            report.problem(
                Kind::Format,
                &n.file,
                n.path.clone(),
                format!("not an annotation: {e}"),
            );
        }
        if let Some(id) = n.note.get("id").and_then(Value::as_str) {
            ids.insert(id.to_string());
        }
    }
    for n in &notes {
        let links = n.note.get("links").and_then(Value::as_array);
        for (i, link) in links.into_iter().flatten().enumerate() {
            let Some(s) = link.as_str() else { continue };
            let path = format!("{}.links[{i}]", n.path);
            let missing = match worlds_core::model::parse_link(s) {
                worlds_core::model::Link::Note(id) => (!ids.contains(id)).then_some("annotation"),
                worlds_core::model::Link::Level(slug) => {
                    (!level_slugs.contains(slug)).then_some("level")
                }
                worlds_core::model::Link::Entity(id) => {
                    (!entity_ids.contains(id)).then_some("entity")
                }
                worlds_core::model::Link::Url(_) => None,
            };
            if let Some(what) = missing {
                report.warn(&n.file, path, format!("{s:?} links to no exported {what}"));
            }
        }
    }
    Ok(report)
}

/// What a level file is checked against.
struct LevelCtx<'a> {
    dir: &'a Path,
    /// The level file, relative to the export directory.
    file: &'a str,
    grid: i64,
    world: &'a str,
    slug: &'a str,
    id: &'a str,
    rect: Option<[i64; 4]>,
    collision: Option<&'a str>,
    blocking: &'a HashSet<String>,
    entity_types: &'a HashSet<String>,
}

fn check_level(cx: &LevelCtx, file: &str, v: &Value, report: &mut Report) {
    let mut r = Reader {
        report,
        file: file.to_string(),
    };
    r.format_one(v);
    let eq = |r: &mut Reader, key: &str, want: &str| {
        if let Some(s) = r.str(v, "", key)
            && s != want
        {
            r.bad(key, format!("{s:?}, but world.json says {want:?}"));
        }
    };
    eq(&mut r, "slug", cx.slug);
    eq(&mut r, "id", cx.id);
    eq(&mut r, "world", cx.world);
    r.str(v, "", "name");
    r.int(v, "", "depth");
    let world_px = r.ints::<2>(v, "", "world_px");
    let size = r.ints::<2>(v, "", "size");
    if let Some(g) = r.int(v, "", "grid_size")
        && g != cx.grid
    {
        r.bad(
            "grid_size",
            format!("{g}, but world.json's grid_size is {}", cx.grid),
        );
    }
    let g = cx.grid.max(1);
    if let Some(px) = world_px
        && (px[0] % g != 0 || px[1] % g != 0)
    {
        r.bad("world_px", format!("{px:?} is not on the {g} px grid"));
    }
    if let Some(s) = size
        && (s[0] <= 0 || s[1] <= 0)
    {
        r.bad("size", "must be at least 1 cell each way");
    }
    if let (Some(rect), Some(px), Some(s)) = (cx.rect, world_px, size)
        && rect != [px[0], px[1], s[0] * g, s[1] * g]
    {
        r.bad(
            "",
            format!(
                "world_px {px:?} and size {s:?} (x {g} px) don't match world.json's rect {rect:?}"
            ),
        );
    }
    let [w_px, h_px] = size.map_or([0, 0], |s| [s[0] * g, s[1] * g]);
    r.obj(v, "", "fields");
    r.arr(v, "", "fields_set");
    r.arr(v, "", "notes");
    if let Some(tags) = v.get("tags")
        && !tags
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string))
    {
        r.bad("tags", "expected an array of strings");
    }
    // Int grids.
    if let Some(grids) = r.obj(v, "", "int_grids") {
        for (slug, gv) in grids {
            let p = format!("int_grids.{slug}");
            r.str(gv, &p, "name");
            let gs = r.int(gv, &p, "grid_size").unwrap_or(0);
            if gv.get("derived").is_some_and(|d| !d.is_boolean()) {
                r.bad(&join(&p, "derived"), "expected true or false");
            }
            let legend: HashSet<char> = match r.obj(gv, &p, "legend") {
                Some(l) => {
                    for (k, val) in l {
                        if k.chars().count() != 1 {
                            r.bad(
                                &join(&p, "legend"),
                                format!("key {k:?} isn't one character"),
                            );
                        }
                        if !(val.is_null() || val.is_string()) {
                            r.bad(&format!("{p}.legend.{k}"), "expected a slug or null");
                        }
                    }
                    l.keys().filter_map(|k| k.chars().next()).collect()
                }
                None => HashSet::new(),
            };
            if gs <= 0 {
                r.bad(&join(&p, "grid_size"), "must be greater than 0");
                continue;
            }
            if w_px % gs != 0 || h_px % gs != 0 {
                r.bad(
                    &join(&p, "grid_size"),
                    format!("{gs} px doesn't divide the level's {w_px}x{h_px} px"),
                );
                continue;
            }
            let (cols, rows) = (w_px / gs, h_px / gs);
            if let Some(rs) = r.arr(gv, &p, "rows") {
                if rs.len() as i64 != rows && size.is_some() {
                    r.bad(
                        &join(&p, "rows"),
                        format!("{} rows, the level has {rows}", rs.len()),
                    );
                }
                for (y, row) in rs.iter().enumerate() {
                    let rp = format!("{p}.rows[{y}]");
                    let Some(s) = row.as_str() else {
                        r.bad(&rp, "expected a string");
                        continue;
                    };
                    if s.chars().count() as i64 != cols && size.is_some() {
                        r.bad(
                            &rp,
                            format!("{} cells, the level is {cols} wide", s.chars().count()),
                        );
                    }
                    if let Some(c) = s.chars().find(|c| !legend.contains(c)) {
                        r.bad(&rp, format!("glyph {c:?} isn't in the legend"));
                    }
                }
            }
        }
    }
    // Tile layers.
    if let Some(layers) = r.arr(v, "", "tile_layers") {
        for (li, lv) in layers.iter().enumerate() {
            let p = item("tile_layers", lv, "layer", li);
            r.str(lv, &p, "layer");
            r.str(lv, &p, "name");
            let kind = r.str(lv, &p, "kind").unwrap_or("");
            if !kind.is_empty() && kind != "tiles" && kind != "auto" {
                r.bad(
                    &join(&p, "kind"),
                    format!("{kind:?}: expected \"tiles\" or \"auto\""),
                );
            }
            let grid_size = r.int(lv, &p, "grid_size").unwrap_or(0);
            if grid_size <= 0 && lv.get("grid_size").and_then(Value::as_i64).is_some() {
                r.bad(&join(&p, "grid_size"), "must be greater than 0");
            }
            for (ri, run) in r.arr(lv, &p, "tilesets").into_iter().flatten().enumerate() {
                let rp = format!("{p}.tilesets[{ri}]");
                r.str(run, &rp, "tileset");
                let ts = r.ints::<2>(run, &rp, "tile_size").unwrap_or([0, 0]);
                if ts[0] <= 0 || ts[1] <= 0 {
                    r.bad(&join(&rp, "tile_size"), "must be at least 1x1");
                }
                let num = |k: &str| run.get(k).and_then(Value::as_i64).unwrap_or(0);
                let (spacing, padding) = (num("spacing"), num("padding"));
                let dims = r
                    .str(run, &rp, "image")
                    .and_then(|img| image_dims(cx.dir, img, &mut r, &join(&rp, "image")));
                for (ti, t) in r.arr(run, &rp, "tiles").into_iter().flatten().enumerate() {
                    let tp = format!("{rp}.tiles[{ti}]");
                    let Some(t) = ints_of::<5>(t) else {
                        r.bad(&tp, "expected [x, y, col, row, flip] (integers)");
                        continue;
                    };
                    let [x, y, col, row, flip] = t;
                    if !(0..=7).contains(&flip) {
                        r.bad(&tp, format!("flip {flip} is outside 0..7"));
                    }
                    let (bw, bh) = if flip & 4 != 0 {
                        (ts[1], ts[0])
                    } else {
                        (ts[0], ts[1])
                    };
                    if size.is_some() && (x + bw <= 0 || y + bh <= 0 || x >= w_px || y >= h_px) {
                        r.bad(
                            &tp,
                            format!("tile at [{x}, {y}] lies outside the level's {w_px}x{h_px} px"),
                        );
                    }
                    if col < 0 || row < 0 {
                        r.report
                            .problem(Kind::Reference, file, tp.clone(), "negative atlas tile");
                    } else if let Some([iw, ih]) = dims
                        && ts[0] > 0
                        && ts[1] > 0
                        && (padding + col * (ts[0] + spacing) + ts[0] > iw
                            || padding + row * (ts[1] + spacing) + ts[1] > ih)
                    {
                        r.report.problem(
                            Kind::Reference,
                            file,
                            tp.clone(),
                            format!(
                                "atlas tile [{col}, {row}] lies outside the {iw}x{ih} px image"
                            ),
                        );
                    }
                }
            }
        }
    }
    // Entities.
    for (i, e) in r.arr(v, "", "entities").into_iter().flatten().enumerate() {
        let p = item("entities", e, "id", i);
        r.str(e, &p, "id");
        if let Some(t) = r.str(e, &p, "type")
            && !cx.entity_types.contains(t)
        {
            r.report.problem(
                Kind::Reference,
                file,
                join(&p, "type"),
                format!("{t:?} isn't in world.json's entity_types"),
            );
        }
        r.ints::<2>(e, &p, "px");
        r.obj(e, &p, "fields");
    }
}

/// Resolve a tileset image relative to the export directory; its size in
/// pixels when it is a readable PNG. A missing file is a reference problem.
fn image_dims(dir: &Path, image: &str, r: &mut Reader, path: &str) -> Option<[i64; 2]> {
    let full: PathBuf = dir.join(image);
    if !full.is_file() {
        let file = r.file.clone();
        r.report.problem(
            Kind::Reference,
            &file,
            path,
            format!("image {image:?} doesn't exist (resolved from the export directory)"),
        );
        return None;
    }
    let is_png = full
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"));
    if !is_png {
        return None;
    }
    match image::image_dimensions(&full) {
        Ok((w, h)) => Some([w as i64, h as i64]),
        Err(e) => {
            let file = r.file.clone();
            r.report.problem(
                Kind::Reference,
                &file,
                path,
                format!("image {image:?} can't be read: {e}"),
            );
            None
        }
    }
}

/// `{ "level": null, "id": ... }` values anywhere under `fields` of the
/// level and its entities.
fn collect_entity_refs(file: &str, v: &Value, out: &mut Vec<(String, String)>) {
    fn walk(v: &Value, path: String, file: &str, out: &mut Vec<(String, String)>) {
        match v {
            Value::Object(o) => {
                if o.len() == 2
                    && o.get("level").is_some_and(Value::is_null)
                    && o.contains_key("id")
                {
                    out.push((file.to_string(), path));
                    return;
                }
                for (k, x) in o {
                    walk(x, join(&path, k), file, out);
                }
            }
            Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    walk(x, format!("{path}[{i}]"), file, out);
                }
            }
            _ => {}
        }
    }
    if let Some(f) = v.get("fields") {
        walk(f, "fields".into(), file, out);
    }
    for (i, e) in v
        .get("entities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if let Some(f) = e.get("fields") {
            walk(
                f,
                format!("{}.fields", item("entities", e, "id", i)),
                file,
                out,
            );
        }
    }
}

/// Which edge of `a` touches `b`, and the shared span (the export's rule).
fn touch(a: [i64; 4], b: [i64; 4]) -> Option<(&'static str, [i64; 2])> {
    let [ax, ay, aw, ah] = a;
    let [bx, by, bw, bh] = b;
    let overlap = |a0: i64, a1: i64, b0: i64, b1: i64| {
        let (s, e) = (a0.max(b0), a1.min(b1));
        (s < e).then_some([s, e])
    };
    if ax == bx + bw {
        overlap(ay, ay + ah, by, by + bh).map(|s| ("left", s))
    } else if ax + aw == bx {
        overlap(ay, ay + ah, by, by + bh).map(|s| ("right", s))
    } else if ay == by + bh {
        overlap(ax, ax + aw, bx, bx + bw).map(|s| ("top", s))
    } else if ay + ah == by {
        overlap(ax, ax + aw, bx, bx + bw).map(|s| ("bottom", s))
    } else {
        None
    }
}

/// `open_edges` of a level's index entry against its collision rows: runs of
/// border cells whose value isn't blocking; `null` when the grid is absent
/// or all empty; absent when there is no collision layer.
fn check_open_edges(cx: &LevelCtx, path: &str, index: &Value, level: &Value, report: &mut Report) {
    let given = index.get("open_edges");
    let p = join(path, "open_edges");
    let Some(collision) = cx.collision else {
        if given.is_some() {
            report.problem(
                Kind::Format,
                "world.json",
                p,
                "present without collision_layer",
            );
        }
        return;
    };
    let grid = level.get("int_grids").and_then(|g| g.get(collision));
    // The open edges the rows give; `None` when they can't be computed (a
    // problem of its own, reported elsewhere): only the comparison is skipped.
    let want: Option<Value> = match grid {
        None => Some(Value::Null),
        Some(g) if g.get("derived").and_then(Value::as_bool) == Some(true) => {
            report.problem(
                Kind::Format,
                cx.file,
                format!("int_grids.{collision}.derived"),
                format!(
                    "{collision:?} is world.json's collision_layer, but derived here; collision \
                     never comes from a derived layer"
                ),
            );
            None
        }
        Some(g) => {
            let legend = g.get("legend").and_then(Value::as_object);
            let rows: Vec<Vec<char>> = g
                .get("rows")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|s| s.chars().collect())
                .collect();
            let value = |c: char| {
                legend
                    .and_then(|l| l.get(&c.to_string()))
                    .and_then(Value::as_str)
            };
            let all_empty = rows.iter().flatten().all(|&c| value(c).is_none());
            let (h, w) = (rows.len(), rows.first().map_or(0, Vec::len));
            // Rows that don't fit the level are a problem of their own.
            let gs = g.get("grid_size").and_then(Value::as_i64).unwrap_or(0);
            let size = level.get("size").and_then(ints_of::<2>);
            let fits = gs > 0
                && size.is_some_and(|[sw, sh]| {
                    (sw * cx.grid) % gs == 0
                        && (sh * cx.grid) % gs == 0
                        && w as i64 == sw * cx.grid / gs
                        && h as i64 == sh * cx.grid / gs
                });
            if !fits {
                None
            } else if all_empty || w == 0 || rows.iter().any(|r| r.len() != w) {
                Some(Value::Null)
            } else {
                let open =
                    |x: usize, y: usize| value(rows[y][x]).is_none_or(|s| !cx.blocking.contains(s));
                let runs = |n: usize, at: &dyn Fn(usize) -> bool| {
                    let mut out = Vec::new();
                    let mut start = None;
                    for i in 0..=n {
                        match (i < n && at(i), start) {
                            (true, None) => start = Some(i),
                            (false, Some(s)) => {
                                out.push(json!([s, i]));
                                start = None;
                            }
                            _ => {}
                        }
                    }
                    Value::Array(out)
                };
                Some(json!({
                    "left": runs(h, &|y| open(0, y)),
                    "right": runs(h, &|y| open(w - 1, y)),
                    "top": runs(w, &|x| open(x, 0)),
                    "bottom": runs(w, &|x| open(x, h - 1)),
                }))
            }
        }
    };
    match (given, want) {
        (None, _) => report.problem(
            Kind::Format,
            "world.json",
            path,
            "missing key \"open_edges\"",
        ),
        (Some(g), Some(want)) if *g != want => report.problem(
            Kind::Format,
            "world.json",
            p,
            format!(
                "doesn't match the collision rows of {}; they give {}",
                cx.slug,
                serde_json::to_string(&want).unwrap_or_default()
            ),
        ),
        _ => {}
    }
}

/// `--fresh`: bake `project_dir` in memory as if writing to `export_dir`
/// and compare every file byte for byte. Nothing is written.
pub fn fresh(project_dir: &Path, export_dir: &Path, report: &mut Report) -> anyhow::Result<()> {
    use anyhow::Context;
    let project = worlds_core::load(project_dir)?;
    let images = worlds_rules::TilesetImages::new(project_dir, &project.tilesets);
    let mut baked = BTreeMap::new();
    for level in project.levels() {
        let b = worlds_rules::bake_for_export(&project, level, &images)
            .with_context(|| format!("baking level {}", level.slug))?;
        baked.insert(level.slug.clone(), b);
    }
    let files = worlds_core::export::render_files(&project, project_dir, export_dir, &baked)?;
    let mut produced = BTreeSet::new();
    for (rel, text) in &files {
        let name = rel.to_string_lossy().replace('\\', "/");
        produced.insert(name.clone());
        match std::fs::read(export_dir.join(rel)) {
            Ok(bytes) if bytes == text.as_bytes() => {}
            Ok(_) => report.problem(
                Kind::Stale,
                &name,
                "",
                format!(
                    "export is stale: differs from a fresh bake of {}",
                    project_dir.display()
                ),
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => report.problem(
                Kind::Stale,
                &name,
                "",
                "export is stale: missing, a fresh bake writes it",
            ),
            Err(e) => return Err(e).with_context(|| export_dir.join(rel).display().to_string()),
        }
    }
    if let Ok(rd) = std::fs::read_dir(export_dir.join("levels")) {
        let mut extra = Vec::new();
        for entry in rd {
            let name = format!("levels/{}", entry?.file_name().to_string_lossy());
            // A file `check` already found unlisted in world.json is that one
            // problem, not a second.
            let unlisted = report
                .problems
                .iter()
                .any(|p| p.file == name && p.path.is_empty() && p.kind == Kind::Format);
            if name.ends_with(".json") && !produced.contains(&name) && !unlisted {
                extra.push(name);
            }
        }
        extra.sort();
        for name in extra {
            report.problem(
                Kind::Stale,
                &name,
                "",
                "export is stale: a fresh bake has no such level",
            );
        }
    }
    Ok(())
}
