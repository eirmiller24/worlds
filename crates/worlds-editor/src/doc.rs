//! The open project as the editor sees it: the `Project`, a flat level
//! index, cached autotile bakes, and validation problems.
//!
//! The UI thread owns the document. Bakes run on worker threads against an
//! `Arc` snapshot of the project and come back through channels:
//! [`Document::start_bakes`] bakes whole levels (at open, after a tileset
//! image changed), [`Document::start_layer_bakes`] single (level, layer)
//! pairs (after a rule edit). [`Document::poll_bakes`] and
//! [`Document::poll_layer_bakes`] store the finished ones and hand them to
//! the viewport so it can invalidate them.
//!
//! **Generations.** Every bake request (a worker batch, a layer job, or a
//! bake the command layer made on the UI thread) takes a fresh generation
//! number, recorded per (level id, layer) in `want`. A worker result is
//! stored only for the layers whose `want` is still its own generation, and
//! layer by layer, never replacing the level's other layers. So a result
//! from before a newer edit (another rule edit, a paint the command layer
//! already rebaked, a tileset reload) is dropped instead of putting an old
//! bake back on screen.
//!
//! **Layer jobs** go to one queue drained by at most
//! [`Document::max_layer_workers`] threads, started when there is work and
//! gone when the queue is empty, so a burst of rule edits (a dragged chance
//! slider) never piles up threads. A worker checks `want` before baking a
//! job: one a newer request superseded is skipped, not baked and thrown
//! away.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use worlds_core::{Id, IntGridLayer, LayerDef, Level, Problem, Project, Slug};
use worlds_rules::{BakeCache, LayerBake, LevelSeed, TilesetImages};

/// The baked `auto` layers of one level, by layer slug. Each bake is shared
/// with the command layer's `BakeCache` rather than copied.
pub type LevelBakes = BTreeMap<Slug, Arc<LayerBake>>;

/// A fresh [`Document::images_gen`].
fn next_images_gen() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A worker thread's bake of one level, layer by layer (each one baked or
/// failed on its own).
type WorkerBakes = BTreeMap<Slug, Result<Arc<LayerBake>, String>>;

/// A whole-level worker result: level id, generation, bakes, and the
/// level's bake cache entry to hand to the command layer's cache
/// ([`BakeCache::seed_level`]) when every layer of it is still current.
type LevelResult = (Id, u64, WorkerBakes, Option<LevelSeed>);

/// A layer job's result: level id, layer, generation, bake (`None`: skipped,
/// a newer bake was asked for before it started).
type LayerResult = (Id, Slug, u64, Option<Result<LayerBake, String>>);

/// Per (level id, auto layer): the generation of the newest bake asked for.
type Want = HashMap<(Id, Slug), u64>;

/// One queued layer job, with the project snapshot it bakes.
struct LayerJob {
    project: Arc<Project>,
    images: Arc<TilesetImages>,
    id: Id,
    at: LevelRef,
    layer: Slug,
    g: u64,
}

/// The layer job queue and how many workers drain it.
#[derive(Default)]
struct LayerQueue {
    jobs: VecDeque<LayerJob>,
    workers: usize,
    /// [`Document::max_layer_workers`] as of the last batch, set with its
    /// jobs: a worker over it quits before taking another job, so lowering
    /// the cap holds for workers already running, not just new ones.
    max: usize,
}

/// Every auto layer of `level`, in dependency order (writers of derived
/// grids before their readers), each succeeding or failing on its own:
/// [`BakeCache::bake_detached`], so the result also seeds the command
/// layer's cache without baking again.
fn bake_each(
    project: &Project,
    level: &Level,
    images: &TilesetImages,
) -> (WorkerBakes, Option<LevelSeed>) {
    match BakeCache::bake_detached(project, level.slug.as_str(), images) {
        Ok(mut seed) => {
            let results = seed.take_results();
            let baked: BTreeMap<&Slug, &Arc<LayerBake>> = seed.bakes().collect();
            let out: WorkerBakes = results
                .into_iter()
                .map(|(layer, r)| {
                    let r = match r {
                        Ok(_) => baked
                            .get(&layer)
                            .map(|b| Arc::clone(b))
                            .ok_or_else(|| "baked, but not cached".to_string()),
                        Err(e) => Err(e.to_string()),
                    };
                    (layer, r)
                })
                .collect();
            let all_ok = out.values().all(Result::is_ok);
            (out, all_ok.then_some(seed))
        }
        Err(e) => {
            let e = e.to_string();
            let out = project
                .layers
                .iter()
                .filter(|l| l.is_auto())
                .map(|l| (l.slug.clone(), Err(e.clone())))
                .collect();
            (out, None)
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Called by worker threads after each result (requests a repaint).
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// Index of a level in [`Document::levels`]: worlds in order, each world's
/// levels in order.
pub type LevelIx = usize;

/// Where a level lives in the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelRef {
    pub world: usize,
    pub level: usize,
}

pub struct Document {
    /// The project directory.
    pub dir: PathBuf,
    pub project: Project,
    pub levels: Vec<LevelRef>,
    /// Id of each level in [`Self::levels`], kept so indices can be mapped
    /// across a [`Self::reindex`] (ids survive renames).
    pub ids: Vec<Id>,
    /// Bake per level; `None` until its bake arrives (or if it failed).
    pub bakes: Vec<Option<Arc<LevelBakes>>>,
    pub images: Arc<TilesetImages>,
    /// Changes whenever the tileset images or their definitions do (unique
    /// across documents): what uploaded textures are keyed on.
    pub images_gen: u64,
    pub problems: Vec<Problem>,
    /// Bumped whenever a definition changes (any `def` or `def_renamed`,
    /// `level_fields`): what views derived from definitions (the entity
    /// palette's rows) are keyed on.
    pub defs_gen: u64,
    /// Levels whose bake hasn't arrived yet.
    pub bakes_pending: usize,
    /// Levels whose bake the command layer maintains (incrementally, after
    /// edits). Worker results for the layers it rebaked are dropped by
    /// generation (see the module docs); a layer job's result for such a
    /// level is also handed to its cache
    /// ([`Editing::adopt_layer_bakes`](crate::edit::Editing::adopt_layer_bakes)).
    pub bake_owned: Vec<bool>,
    /// Per level, the layers whose bake in [`Self::bakes`] is out of date
    /// (made against a tileset image or rules that have since changed).
    /// Shown until the rebake lands, but never handed to the bake cache,
    /// and not exported.
    pub stale_bakes: Vec<BTreeSet<Slug>>,
    /// Layer jobs ([`Self::start_layer_bakes`]) not yet back.
    pub layer_jobs_pending: usize,
    /// Worker results dropped because a newer bake was asked for since
    /// (see the module docs).
    pub dropped_results: usize,
    /// Run layer jobs on the calling thread instead of the workers
    /// (headless renders: the picture must be complete).
    pub inline_layer_bakes: bool,
    /// Called from the worker threads after each result.
    pub wake: Option<Wake>,
    /// Most threads draining the layer job queue at once (see the module
    /// docs). Default: the core count, 1 to 8.
    pub max_layer_workers: usize,
    /// Layer jobs a worker skipped because a newer bake of the same (level,
    /// layer) was asked for before it started (also counted in
    /// [`Self::dropped_results`]).
    pub skipped_layer_jobs: usize,
    /// Per (level id, auto layer): the generation of the newest bake asked
    /// for (see the module docs). Shared with the layer workers.
    want: Arc<Mutex<Want>>,
    layer_queue: Arc<Mutex<LayerQueue>>,
    next_gen: u64,
    /// Whole-level worker results by level id (so they land right even
    /// after a reindex), with the generation of their batch.
    bake_rx: Option<Receiver<LevelResult>>,
    /// Per level id: a whole-level worker bake every layer of which was
    /// stored as it is, for the command layer's cache
    /// ([`Self::take_seed`]).
    seeds: HashMap<Id, LevelSeed>,
    bake_started: Option<Instant>,
    layer_tx: Sender<LayerResult>,
    layer_rx: Receiver<LayerResult>,
}

impl Document {
    /// Load and validate a project directory. Bakes are not started; call
    /// [`Self::start_bakes`] or [`Self::bake_all_blocking`].
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, worlds_core::Error> {
        let dir = dir.as_ref().to_path_buf();
        let t = Instant::now();
        let project = worlds_core::load(&dir)?;
        tracing::info!(
            "loaded {} ({} levels) in {:.1} ms",
            dir.display(),
            project.levels().count(),
            t.elapsed().as_secs_f64() * 1e3
        );
        Ok(Self::from_project(dir, project))
    }

    /// Wrap an already loaded (or generated) project.
    pub fn from_project(dir: PathBuf, project: Project) -> Self {
        let levels = level_refs(&project);
        let ids = ids_of(&project, &levels);
        let images = Arc::new(TilesetImages::new(&dir, &project.tilesets));
        let problems = worlds_core::validate(&project);
        for p in problems.iter().take(20) {
            tracing::warn!("problem: {p}");
        }
        if problems.len() > 20 {
            tracing::warn!("... and {} more problems", problems.len() - 20);
        }
        let (layer_tx, layer_rx) = channel();
        Document {
            dir,
            bakes: vec![None; levels.len()],
            bake_owned: vec![false; levels.len()],
            stale_bakes: vec![BTreeSet::new(); levels.len()],
            layer_jobs_pending: 0,
            dropped_results: 0,
            inline_layer_bakes: false,
            wake: None,
            max_layer_workers: std::thread::available_parallelism()
                .map_or(4, |n| n.get())
                .clamp(1, 8),
            skipped_layer_jobs: 0,
            want: Arc::default(),
            layer_queue: Arc::default(),
            next_gen: 1,
            layer_tx,
            layer_rx,
            ids,
            levels,
            project,
            images,
            images_gen: next_images_gen(),
            problems,
            defs_gen: 0,
            bakes_pending: 0,
            bake_rx: None,
            bake_started: None,
            seeds: HashMap::new(),
        }
    }

    /// Rebuild the level index after levels were created, deleted or
    /// renamed. Bakes follow their level by id. Returns, for each old index,
    /// its new index (`None` for a deleted level).
    pub fn reindex(&mut self) -> Vec<Option<LevelIx>> {
        let levels = level_refs(&self.project);
        let ids = ids_of(&self.project, &levels);
        let map: Vec<Option<LevelIx>> = self
            .ids
            .iter()
            .map(|old| ids.iter().position(|n| n == old))
            .collect();
        let mut bakes = vec![None; levels.len()];
        let mut owned = vec![false; levels.len()];
        let mut stale = vec![BTreeSet::new(); levels.len()];
        for (old, new) in map.iter().enumerate() {
            if let Some(new) = *new {
                bakes[new] = self.bakes[old].take();
                owned[new] = self.bake_owned[old];
                stale[new] = std::mem::take(&mut self.stale_bakes[old]);
            }
        }
        self.levels = levels;
        self.ids = ids;
        self.bakes = bakes;
        self.bake_owned = owned;
        self.stale_bakes = stale;
        map
    }

    /// Where entity `id` is: its level, its entities layer, the instance.
    pub fn find_entity(&self, id: &Id) -> Option<(LevelIx, &Slug, &worlds_core::EntityInstance)> {
        (0..self.levels.len()).find_map(|ix| {
            self.level(ix).entities.iter().find_map(|el| {
                el.entities
                    .iter()
                    .find(|e| e.id == *id)
                    .map(|e| (ix, &el.layer, e))
            })
        })
    }

    /// The level with this id.
    pub fn level_ix_by_id(&self, id: &Id) -> Option<LevelIx> {
        self.ids.iter().position(|i| i == id)
    }

    pub fn level(&self, ix: LevelIx) -> &Level {
        let r = self.levels[ix];
        &self.project.worlds[r.world].levels[r.level]
    }

    pub fn level_ix(&self, slug: &str) -> Option<LevelIx> {
        (0..self.levels.len()).find(|&i| self.level(i).slug == slug)
    }

    /// World rect of a level in pixels: `[x, y, w, h]`.
    pub fn level_rect(&self, ix: LevelIx) -> [f64; 4] {
        let l = self.level(ix);
        let g = self.project.grid_size as f64;
        [
            l.world_px[0] as f64,
            l.world_px[1] as f64,
            l.size[0] as f64 * g,
            l.size[1] as f64 * g,
        ]
    }

    /// Tileset `i`'s image changed on disk. Forgets its cached opacity (the
    /// rules read it: which tiles are opaque decides cell locks) and returns
    /// the `auto` layers whose rules draw from it. If there are any, every
    /// level's bake is stale: their bakes stop being owned by the command
    /// layer, so the next worker bake replaces them. Call
    /// [`EditorCommands::tileset_changed`](crate::tools::EditorCommands::tileset_changed)
    /// and then [`Self::start_bakes`] (which also drops results still in
    /// flight from bakes started against the old image).
    pub fn tileset_changed(&mut self, i: usize) -> Vec<Slug> {
        let Some(ts) = self.project.tilesets.get(i) else {
            return Vec::new();
        };
        self.images.invalidate(ts.slug.as_str());
        self.images_gen = next_images_gen();
        let layers = self.project.auto_layers_drawing_from(ts.slug.as_str());
        if !layers.is_empty() {
            self.bake_owned.iter_mut().for_each(|o| *o = false);
            for s in &mut self.stale_bakes {
                s.extend(layers.iter().cloned());
            }
        }
        layers
    }

    /// The tileset definitions changed (one added, removed, renamed or
    /// pointed at another image): re-read the images. Bakes made before are
    /// stale; the caller rebakes.
    pub fn tileset_defs_changed(&mut self) {
        self.images = Arc::new(TilesetImages::new(&self.dir, &self.project.tilesets));
        self.images_gen = next_images_gen();
    }

    /// A fresh generation number (see the module docs).
    fn next_generation(&mut self) -> u64 {
        self.next_gen += 1;
        self.next_gen
    }

    /// Ask for a bake of every auto layer of every level: results with an
    /// older generation are dropped from now on.
    fn want_all(&mut self, g: u64) {
        let auto: Vec<Slug> = self.auto_layers();
        let mut want = lock(&self.want);
        for id in &self.ids {
            for l in &auto {
                want.insert((id.clone(), l.clone()), g);
            }
        }
    }

    fn auto_layers(&self) -> Vec<Slug> {
        self.project
            .layers
            .iter()
            .filter(|l| l.is_auto())
            .map(|l| l.slug.clone())
            .collect()
    }

    /// Bake every level on worker threads. Results arrive through
    /// [`Self::poll_bakes`]; `wake` is called after each one (to request a
    /// repaint).
    pub fn start_bakes(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        let n = self.levels.len();
        if n == 0 || !self.project.layers.iter().any(|l| l.is_auto()) {
            return;
        }
        let g = self.next_generation();
        self.want_all(g);
        let project = Arc::new(self.project.clone());
        let levels = Arc::new(self.levels.clone());
        let images = self.images.clone();
        let wake = Arc::new(wake);
        let (tx, rx) = channel();
        let workers = std::thread::available_parallelism()
            .map_or(4, |n| n.get())
            .clamp(1, 8)
            .min(n);
        for w in 0..workers {
            let (project, levels, images, wake, tx) = (
                project.clone(),
                levels.clone(),
                images.clone(),
                wake.clone(),
                tx.clone(),
            );
            std::thread::Builder::new()
                .name(format!("bake-{w}"))
                .spawn(move || {
                    for ix in (w..levels.len()).step_by(workers) {
                        let r = levels[ix];
                        let level = &project.worlds[r.world].levels[r.level];
                        let (res, seed) = bake_each(&project, level, &images);
                        if tx.send((level.id.clone(), g, res, seed)).is_err() {
                            return;
                        }
                        wake();
                    }
                })
                .expect("spawn bake thread");
        }
        self.seeds.clear();
        self.bakes_pending = n;
        self.bake_rx = Some(rx);
        self.bake_started = Some(Instant::now());
    }

    /// Bakes that finished since the last call, stored in [`Self::bakes`];
    /// returns their level indices.
    pub fn poll_bakes(&mut self) -> Vec<LevelIx> {
        let mut done = Vec::new();
        let Some(rx) = self.bake_rx.take() else {
            return done;
        };
        while let Ok((id, g, res, seed)) = rx.try_recv() {
            match self.level_ix_by_id(&id) {
                Some(ix) => {
                    self.store_bake(ix, g, res, seed);
                    done.push(ix);
                }
                // Deleted while baking.
                None => self.bakes_pending = self.bakes_pending.saturating_sub(1),
            }
        }
        self.bake_rx = Some(rx);
        if self.bakes_pending == 0 {
            self.bake_rx = None;
            if let Some(t) = self.bake_started.take() {
                tracing::info!(
                    "baked {} levels in {:.1} ms",
                    self.levels.len(),
                    t.elapsed().as_secs_f64() * 1e3
                );
            }
        }
        done
    }

    /// Bake every level on this thread (headless screenshots, tests).
    pub fn bake_all_blocking(&mut self) {
        let t = Instant::now();
        let g = self.next_generation();
        self.want_all(g);
        self.seeds.clear();
        for ix in 0..self.levels.len() {
            let (res, seed) = bake_each(&self.project, self.level(ix), &self.images);
            self.bakes_pending += 1;
            self.store_bake(ix, g, res, seed);
        }
        tracing::info!(
            "baked {} levels in {:.1} ms",
            self.levels.len(),
            t.elapsed().as_secs_f64() * 1e3
        );
    }

    /// Store a whole-level worker result of generation `g`, layer by
    /// layer: only the layers no newer bake was asked for since. A layer
    /// that failed to bake is stored empty (the error is logged), like a
    /// failed layer job: a broken rule set shows nothing rather than its old
    /// tiles, and isn't left stale (to be retried by every export).
    fn store_bake(&mut self, ix: LevelIx, g: u64, res: WorkerBakes, seed: Option<LevelSeed>) {
        self.bakes_pending = self.bakes_pending.saturating_sub(1);
        if ix >= self.bakes.len() {
            return;
        }
        let id = self.ids[ix].clone();
        let all = res.len();
        let mut fresh = Vec::new();
        {
            let want = lock(&self.want);
            for (layer, bake) in res {
                if want.get(&(id.clone(), layer.clone())) == Some(&g) {
                    fresh.push((layer, bake));
                } else {
                    self.dropped_results += 1;
                }
            }
        }
        let slug = self.level(ix).slug.to_string();
        let every = fresh.len() == all;
        let bakes = Arc::make_mut(self.bakes[ix].get_or_insert_with(Default::default));
        for (layer, bake) in fresh {
            let bake = bake.unwrap_or_else(|e| {
                tracing::error!("bake of {slug}/{layer} failed: {e}");
                Arc::new(LayerBake::default())
            });
            self.stale_bakes[ix].remove(&layer);
            bakes.insert(layer, bake);
        }
        // Only a level stored whole, every layer as baked there, may seed
        // the command layer's cache.
        match seed {
            Some(seed) if every => {
                self.seeds.insert(id, seed);
            }
            _ => {
                self.seeds.remove(&id);
            }
        }
    }

    /// The whole-level worker bake of level `ix`, if the document still
    /// shows exactly it (every layer the same bake, none stale), to seed
    /// the command layer's cache with ([`BakeCache::seed_level`]). Taken
    /// either way.
    pub fn take_seed(&mut self, ix: LevelIx) -> Option<LevelSeed> {
        let id = self.ids.get(ix)?;
        let seed = self.seeds.remove(id)?;
        let bakes = self.bakes[ix].as_ref()?;
        let same = self.stale_bakes[ix].is_empty()
            && seed
                .bakes()
                .all(|(l, b)| bakes.get(l).is_some_and(|d| Arc::ptr_eq(d, b)));
        same.then_some(seed)
    }

    /// The values the rules **computed** for derived layer `d` in level
    /// `ix`: what its writer's current bake wrote (`LayerBake::derived`).
    /// `None` when no layer writes it or the writer's bake isn't in.
    pub fn derived_computed(&self, ix: LevelIx, d: &str) -> Option<&IntGridLayer> {
        let w = self.project.derived_writer(d)?;
        self.bakes.get(ix)?.as_ref()?.get(w)?.derived.get(d)
    }

    /// The **effective** grid of derived layer `def` in level `ix`: the
    /// stored override where there is one (a suppressed cell is empty),
    /// else what the rules computed ([`Self::derived_computed`]). Sized in
    /// cells of the layer's grid; the same values `worlds_rules` exports.
    pub fn derived_effective(&self, ix: LevelIx, def: &LayerDef) -> IntGridLayer {
        let computed = self.derived_computed(ix, def.slug.as_str());
        self.project
            .derived_effective(self.level(ix), def, computed)
    }

    /// The command layer just stored these layers of level `ix` from its
    /// own (current) bakes: they are fresh, and any worker result still in
    /// flight for them is older and will be dropped.
    pub fn bakes_refreshed(&mut self, ix: LevelIx, layers: &[Slug]) {
        if ix >= self.levels.len() {
            return;
        }
        let g = self.next_generation();
        let mut want = lock(&self.want);
        for l in layers {
            self.stale_bakes[ix].remove(l);
            want.insert((self.ids[ix].clone(), l.clone()), g);
        }
    }

    /// Rebake layer `layer` of each listed level on the workers (after a
    /// rule edit: the layer's rules changed, the levels didn't). The old
    /// bakes stay on screen, marked stale, until the new ones arrive through
    /// [`Self::poll_layer_bakes`]; a job older than the newest request for
    /// its (level, layer) is skipped, or dropped if it was already baking.
    /// With [`Self::inline_layer_bakes`] the jobs run right here and are
    /// stored at once.
    pub fn start_layer_bakes(&mut self, jobs: Vec<(LevelIx, Slug)>) {
        let jobs: Vec<(LevelIx, Slug)> = jobs
            .into_iter()
            .filter(|(ix, _)| *ix < self.levels.len())
            .collect();
        if jobs.is_empty() {
            return;
        }
        let g = self.next_generation();
        {
            let mut want = lock(&self.want);
            for (ix, layer) in &jobs {
                want.insert((self.ids[*ix].clone(), layer.clone()), g);
                self.stale_bakes[*ix].insert(layer.clone());
            }
        }
        if self.inline_layer_bakes {
            for (ix, layer) in jobs {
                let res =
                    worlds_rules::bake_layer(&self.project, self.level(ix), &layer, &self.images)
                        .map_err(|e| e.to_string());
                let id = self.ids[ix].clone();
                self.store_layer(id, layer, g, Some(res));
            }
            return;
        }
        let t = Instant::now();
        let project = Arc::new(self.project.clone());
        tracing::debug!(
            "layer jobs: project snapshot in {:.2} ms",
            t.elapsed().as_secs_f64() * 1e3
        );
        self.layer_jobs_pending += jobs.len();
        let spawn = {
            let mut q = lock(&self.layer_queue);
            q.max = self.max_layer_workers;
            for (ix, layer) in jobs {
                q.jobs.push_back(LayerJob {
                    project: project.clone(),
                    images: self.images.clone(),
                    id: self.ids[ix].clone(),
                    at: self.levels[ix],
                    layer,
                    g,
                });
            }
            let n = self
                .max_layer_workers
                .saturating_sub(q.workers)
                .min(q.jobs.len());
            q.workers += n;
            n
        };
        for _ in 0..spawn {
            let (queue, want, wake, tx) = (
                self.layer_queue.clone(),
                self.want.clone(),
                self.wake.clone(),
                self.layer_tx.clone(),
            );
            std::thread::Builder::new()
                .name("layer-bake".into())
                .spawn(move || layer_worker(&queue, &want, wake.as_ref(), &tx))
                .expect("spawn layer bake thread");
        }
    }

    /// Layer jobs that came back since the last call and were stored (the
    /// ones still current); returns (level, layer) of each.
    pub fn poll_layer_bakes(&mut self) -> Vec<(LevelIx, Slug)> {
        let mut done = Vec::new();
        while let Ok((id, layer, g, res)) = self.layer_rx.try_recv() {
            self.layer_jobs_pending = self.layer_jobs_pending.saturating_sub(1);
            self.skipped_layer_jobs += usize::from(res.is_none());
            if let Some(ix) = self.store_layer(id, layer.clone(), g, res) {
                done.push((ix, layer));
            }
        }
        done
    }

    /// Wait for every layer job in flight and store the current ones
    /// (tests; a snapshot that must not show stale layers uses
    /// [`Self::refresh_stale`] instead).
    pub fn finish_layer_bakes(&mut self) -> Vec<(LevelIx, Slug)> {
        let mut done = Vec::new();
        while self.layer_jobs_pending > 0 {
            let Ok((id, layer, g, res)) = self.layer_rx.recv() else {
                break;
            };
            self.layer_jobs_pending -= 1;
            self.skipped_layer_jobs += usize::from(res.is_none());
            if let Some(ix) = self.store_layer(id, layer.clone(), g, res) {
                done.push((ix, layer));
            }
        }
        done
    }

    /// Bake every stale layer now, on this thread (before a snapshot, so an
    /// agent never sees a bake older than its last edit). Returns what was
    /// rebaked; jobs still in flight for them will be dropped.
    pub fn refresh_stale(&mut self) -> Vec<(LevelIx, Slug)> {
        let mut done = Vec::new();
        for ix in 0..self.levels.len() {
            let layers: Vec<Slug> = self.stale_bakes[ix].iter().cloned().collect();
            for layer in layers {
                let g = self.next_generation();
                lock(&self.want).insert((self.ids[ix].clone(), layer.clone()), g);
                let res =
                    worlds_rules::bake_layer(&self.project, self.level(ix), &layer, &self.images)
                        .map_err(|e| e.to_string());
                let id = self.ids[ix].clone();
                if self.store_layer(id, layer.clone(), g, Some(res)).is_some() {
                    done.push((ix, layer));
                }
            }
        }
        done
    }

    /// Store one layer job's result if it is still the newest asked for.
    /// A failed bake stores an empty layer (the error is logged), so a
    /// broken rule set shows nothing rather than its old tiles.
    fn store_layer(
        &mut self,
        id: Id,
        layer: Slug,
        g: u64,
        res: Option<Result<LayerBake, String>>,
    ) -> Option<LevelIx> {
        let current = lock(&self.want).get(&(id.clone(), layer.clone())) == Some(&g);
        // A skipped job is never current: `want` only moves forward.
        let Some(res) = res.filter(|_| current) else {
            self.dropped_results += 1;
            return None;
        };
        let ix = self.level_ix_by_id(&id)?;
        let bake = match res {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(
                    "bake of {}/{layer} failed: {e}",
                    self.level(ix).slug.as_str()
                );
                LayerBake::default()
            }
        };
        Arc::make_mut(self.bakes[ix].get_or_insert_with(Default::default))
            .insert(layer.clone(), Arc::new(bake));
        self.stale_bakes[ix].remove(&layer);
        Some(ix)
    }
}

/// A layer worker: bake queued jobs until the queue is empty (or there are
/// more workers than [`LayerQueue::max`]), skipping those a newer request
/// superseded.
fn layer_worker(
    queue: &Mutex<LayerQueue>,
    want: &Mutex<Want>,
    wake: Option<&Wake>,
    tx: &Sender<LayerResult>,
) {
    loop {
        let job = {
            let mut q = lock(queue);
            if q.workers > q.max {
                q.workers -= 1;
                return;
            }
            match q.jobs.pop_front() {
                Some(j) => j,
                None => {
                    q.workers -= 1;
                    return;
                }
            }
        };
        let current = lock(want).get(&(job.id.clone(), job.layer.clone())) == Some(&job.g);
        let res = current.then(|| {
            let level = &job.project.worlds[job.at.world].levels[job.at.level];
            worlds_rules::bake_layer(&job.project, level, &job.layer, &job.images)
                .map_err(|e| e.to_string())
        });
        if tx.send((job.id, job.layer, job.g, res)).is_err() {
            lock(queue).workers -= 1;
            return;
        }
        if let Some(w) = wake {
            w();
        }
    }
}

fn level_refs(project: &Project) -> Vec<LevelRef> {
    project
        .worlds
        .iter()
        .enumerate()
        .flat_map(|(w, world)| {
            (0..world.levels.len()).map(move |l| LevelRef { world: w, level: l })
        })
        .collect()
}

fn ids_of(project: &Project, levels: &[LevelRef]) -> Vec<Id> {
    levels
        .iter()
        .map(|r| project.worlds[r.world].levels[r.level].id.clone())
        .collect()
}

/// Grow `project` to `n` levels by copying its levels (round robin) into a
/// square grid in its first world. Slugs get a `_<i>` suffix and fresh ids.
/// For performance testing (`worlds --synthetic N`).
pub fn synthesize(project: &mut Project, n: usize) {
    let templates: Vec<Level> = project.levels().cloned().collect();
    if templates.is_empty() || project.worlds.is_empty() {
        return;
    }
    let g = project.grid_size as i64;
    let cell_w = templates
        .iter()
        .map(|l| l.size[0] as i64)
        .max()
        .unwrap_or(1)
        * g
        + 4 * g;
    let cell_h = templates
        .iter()
        .map(|l| l.size[1] as i64)
        .max()
        .unwrap_or(1)
        * g
        + 4 * g;
    let cols = (n as f64).sqrt().ceil().max(1.0) as i64;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut l = templates[i % templates.len()].clone();
        l.slug = Slug::new(format!("{}_{i}", l.slug.as_str())).expect("valid slug");
        l.id = worlds_core::Id::generate();
        // Entity ids are project-wide. References inside fields keep
        // pointing at the template's ids, which validate reports.
        for e in l.entities.iter_mut().flat_map(|e| e.entities.iter_mut()) {
            e.id = worlds_core::Id::generate();
        }
        l.world_px = [(i as i64 % cols) * cell_w, (i as i64 / cols) * cell_h];
        out.push(l);
    }
    for w in &mut project.worlds {
        w.levels.clear();
    }
    project.worlds[0].levels = out;
    project.normalize();
}

/// A small deterministic generator (SplitMix64) for synthetic content.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    /// A valid entity id (10 chars of `a-z2-7`).
    fn id(&mut self) -> worlds_core::Id {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
        let mut bits = self.next();
        let s: String = (0..10)
            .map(|_| {
                let c = ALPHABET[(bits & 31) as usize] as char;
                bits >>= 5;
                c
            })
            .collect();
        worlds_core::Id::new(s).expect("valid id")
    }
}

/// What [`synthesize_entities`] knows of the first tileset's atlas.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SynthAtlas {
    /// The image size in pixels.
    pub size: [u32; 2],
    /// The tiles (column, row; row-major) that draw something an icon
    /// shows: at least [`SynthAtlas::MIN_OPAQUE`] of their pixels opaque
    /// (alpha of half or more). Empty: not known, every tile is used.
    pub drawn: Vec<[u32; 2]>,
}

impl SynthAtlas {
    /// The share of a tile's pixels that must be opaque for it to make an
    /// icon: a tile with a stray pixel or two makes a blank-looking one.
    pub const MIN_OPAQUE: f64 = 0.10;

    /// The atlas of tileset `ts` from its image: the tiles that draw
    /// enough to make an icon ([`Self::MIN_OPAQUE`]). When none does,
    /// those that draw anything at all.
    pub fn from_image(ts: &worlds_core::TilesetDef, img: &image::RgbaImage) -> Self {
        let size = [img.width(), img.height()];
        let (cols, rows) = (ts.columns(size[0]), ts.rows(size[1]));
        let mut drawn = Vec::new();
        let mut any_pixel = Vec::new();
        for r in 0..rows {
            for c in 0..cols {
                let [x0, y0] = ts.tile_px([c, r]);
                let x1 = (x0 + ts.tile_size[0]).min(size[0]);
                let y1 = (y0 + ts.tile_size[1]).min(size[1]);
                let area = (x1.saturating_sub(x0) as u64) * (y1.saturating_sub(y0) as u64);
                let (mut opaque, mut visible) = (0u64, false);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let a = img.get_pixel(x, y)[3];
                        visible |= a > 0;
                        opaque += u64::from(a >= 128);
                    }
                }
                if area > 0 && opaque as f64 >= Self::MIN_OPAQUE * area as f64 {
                    drawn.push([c, r]);
                }
                if visible {
                    any_pixel.push([c, r]);
                }
            }
        }
        if drawn.is_empty() {
            drawn = any_pixel;
        }
        SynthAtlas { size, drawn }
    }

    /// Only the size known (every tile is used).
    pub fn of_size(size: [u32; 2]) -> Self {
        SynthAtlas {
            size,
            drawn: Vec::new(),
        }
    }
}

/// The fixed seed of [`synthesize_entities`].
pub const SYNTHETIC_SEED: u64 = 0x5EED_E471_7135;

/// Add `n` synthetic entity types `syn_<i>` to `project`, and ⌈n/4⌉
/// instances of them to every level's first entities layer (perf testing:
/// `worlds --synthetic-entities N`). Every choice and every id derives
/// from [`SYNTHETIC_SEED`], so the same project and `n` give the same
/// result:
///
/// - a three-level category tree from fixed word lists (6 x 4 x 3);
/// - 1 to 3 tags from a pool of 12;
/// - icons cycling through the first tileset's atlas: the tiles that draw
///   something when `atlas` knows them (a blank tile makes a blank icon),
///   else every tile (a 16 x 16 grid without `atlas`); hashed colors;
/// - 3 to 14 fields per type, cycling through every field type, single and
///   array: int and float with bounds, bool, string with a regex, text,
///   color, enum (a project enum `syn_kind`, added if missing, and inline
///   values), point, entity_ref (with `allowed_tags`), path, tile, json;
/// - instances at in-bounds cells, some with field values.
pub fn synthesize_entities(project: &mut Project, n: usize, atlas: Option<&SynthAtlas>) {
    use serde_json::json;
    use worlds_core::{
        Color, EntityDef, EntityInstance, EnumDef, EnumSource, EnumValue, FieldDef, FieldDisplay,
        FieldType, TileRef,
    };
    if n == 0 {
        return;
    }
    let mut rng = Rng(SYNTHETIC_SEED);
    const TOP: [&str; 6] = ["Actors", "Props", "Puzzle", "Hazards", "Triggers", "Decor"];
    const MID: [&str; 4] = ["Doors", "Switches", "Lights", "Spawns"];
    const LOW: [&str; 3] = ["Small", "Large", "Special"];
    const ADJ: [&str; 7] = [
        "Old", "Iron", "Mossy", "Golden", "Cracked", "Hidden", "Twin",
    ];
    const NOUN: [&str; 10] = [
        "Door", "Lever", "Torch", "Crate", "Spike", "Gate", "Bell", "Statue", "Chest", "Lamp",
    ];
    const TAGS: [&str; 12] = [
        "object", "puzzle", "hazard", "light", "enemy", "pickup", "door", "switch", "spawn",
        "decor", "heavy", "secret",
    ];
    let slug = |s: String| Slug::new(s).expect("valid slug");
    let kind = slug("syn_kind".into());
    if project.enum_def(kind.as_str()).is_none() {
        project.enums.push(EnumDef {
            slug: kind.clone(),
            name: Some("Synthetic kind".into()),
            values: ["small", "medium", "large"]
                .iter()
                .map(|v| EnumValue {
                    slug: slug(v.to_string()),
                    name: None,
                    color: None,
                    icon: None,
                })
                .collect(),
        });
    }
    let icon_ts = project.tilesets.first().map(|t| {
        let cols = atlas.map_or(16, |a| t.columns(a.size[0]).max(1));
        let rows = atlas.map_or(16, |a| t.rows(a.size[1]).max(1));
        (t.slug.clone(), cols, rows)
    });
    let drawn: &[[u32; 2]] = atlas.map_or(&[], |a| &a.drawn);
    for i in 0..n {
        let name = format!(
            "{} {} {i}",
            ADJ[i % ADJ.len()],
            NOUN[(i / ADJ.len()) % NOUN.len()]
        );
        let category = format!("{}/{}/{}", TOP[i % 6], MID[(i / 6) % 4], LOW[(i / 24) % 3]);
        let mut tags: Vec<String> = Vec::new();
        for _ in 0..1 + rng.below(3) {
            let t = TAGS[rng.below(12) as usize].to_string();
            if !tags.contains(&t) {
                tags.push(t);
            }
        }
        let h = rng.next();
        let color = Color::new(format!(
            "#{:02x}{:02x}{:02x}",
            64 + (h & 0x7f),
            64 + ((h >> 8) & 0x7f),
            64 + ((h >> 16) & 0x7f)
        ))
        .expect("valid color");
        let icon = icon_ts.as_ref().map(|(ts, cols, rows)| TileRef {
            tileset: ts.clone(),
            tile: if drawn.is_empty() {
                [(i as u32) % cols, ((i as u32) / cols) % rows]
            } else {
                drawn[i % drawn.len()]
            },
        });
        let nfields = 3 + i % 12;
        let mut fields = Vec::new();
        for j in 0..nfields {
            let k = (i * 5 + j) % 13;
            let array = (i + j) % 3 == 0;
            let (ty, one) = match k {
                0 => (
                    FieldType::Int {
                        min: Some(0.into()),
                        max: Some(99.into()),
                    },
                    json!(3),
                ),
                1 => (
                    FieldType::Float {
                        min: Some(serde_json::Number::from_f64(0.0).expect("finite")),
                        max: None,
                    },
                    json!(1.5),
                ),
                2 => (FieldType::Bool, json!(true)),
                3 => (
                    FieldType::String {
                        regex: Some("[a-z_]*".into()),
                    },
                    json!("key"),
                ),
                4 => (FieldType::Text { regex: None }, json!("Two\nlines")),
                5 => (FieldType::Color, json!("#c08040")),
                6 => (
                    FieldType::Enum(EnumSource::Project(kind.clone())),
                    json!("medium"),
                ),
                7 => (
                    FieldType::Enum(EnumSource::Inline(vec![
                        slug("wood".into()),
                        slug("iron".into()),
                    ])),
                    json!("iron"),
                ),
                // Instances that show an arrow get a value near them.
                8 => (FieldType::Point, serde_json::Value::Null),
                9 => (
                    FieldType::EntityRef {
                        allowed_types: Vec::new(),
                        allowed_tags: vec![TAGS[i % 12].to_string()],
                        cross_level: i % 2 == 0,
                    },
                    serde_json::Value::Null,
                ),
                10 => (
                    FieldType::Path {
                        extensions: vec!["gd".into()],
                    },
                    json!("scripts/a.gd"),
                ),
                11 => (
                    FieldType::Tile {
                        tileset: icon_ts.as_ref().map(|t| t.0.clone()),
                    },
                    match &icon_ts {
                        Some((ts, _, _)) => json!({ "tileset": ts.as_str(), "tile": [0, 0] }),
                        None => serde_json::Value::Null,
                    },
                ),
                _ => (FieldType::Json, json!({ "n": j })),
            };
            let nullable = one.is_null() || k == 9;
            let mut f = FieldDef::new(slug(format!("f{j}_{}", ty.name())), ty);
            f.array = array;
            f.nullable = nullable;
            f.default = Some(if array { json!([]) } else { one });
            f.display = FieldDisplay {
                canvas: j == 0,
                arrow: k == 8 && i % 5 == 0,
                world: j == 1,
            };
            fields.push(f);
        }
        let size = match i % 5 {
            1 => [2, 1],
            2 => [1, 2],
            _ => [1, 1],
        };
        project.entities.push(EntityDef {
            slug: slug(format!("syn_{i}")),
            name,
            category: Some(category),
            tags,
            doc: Some(format!("Synthetic type {i} for palette tests.")),
            size,
            resizable: [i % 4 == 0, i % 6 == 0],
            pivot: if i % 2 == 0 { [0.5, 1.0] } else { [0.0, 0.0] },
            icon,
            color,
            limit: None,
            fields,
        });
    }
    let Some(layer) = project.layers.iter().find(|l| l.is_entities()).cloned() else {
        project.normalize();
        return;
    };
    let per_level = n.div_ceil(4);
    let grid = project.grid_size;
    let lg = project.layer_grid(&layer).max(1);
    for w in 0..project.worlds.len() {
        for li in 0..project.worlds[w].levels.len() {
            let level = &project.worlds[w].levels[li];
            let cells = [
                (level.size[0] as u64 * grid as u64).div_ceil(lg as u64) as u32,
                (level.size[1] as u64 * grid as u64).div_ceil(lg as u64) as u32,
            ];
            let mut placed = Vec::with_capacity(per_level);
            for k in 0..per_level {
                let t = (li * 7 + k * 13) % n;
                let mut fields = std::collections::BTreeMap::new();
                if k % 3 == 0 {
                    // The first field (shown on the canvas) gets a value.
                    let def = &project.entities[project.entities.len() - n + t];
                    if let Some(f) = def.fields.first()
                        && !f.array
                        && let Some(v) = &f.default
                    {
                        fields.insert(f.slug.clone(), v.clone());
                    }
                }
                let cell = [
                    rng.below(cells[0].max(1) as u64) as i32,
                    rng.below(cells[1].max(1) as u64) as i32,
                ];
                let def = &project.entities[project.entities.len() - n + t];
                for f in def.fields.iter().filter(|f| f.display.arrow && !f.array) {
                    let x = (cell[0] + 3).min(cells[0] as i32 - 1);
                    fields.insert(f.slug.clone(), json!([x, cell[1]]));
                }
                placed.push(EntityInstance {
                    id: rng.id(),
                    ty: slug(format!("syn_{t}")),
                    cell,
                    offset_px: [0, 0],
                    size: None,
                    fields,
                });
            }
            let level = &mut project.worlds[w].levels[li];
            match level.entities.iter_mut().find(|e| e.layer == layer.slug) {
                Some(el) => el.entities.extend(placed),
                None => level.entities.push(worlds_core::EntitiesLayer {
                    layer: layer.slug.clone(),
                    entities: placed,
                }),
            }
        }
    }
    project.normalize();
}

/// A deterministic stream of numbers for the synthetic generators.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    /// A whole number in `lo..hi` (`lo` when empty).
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        if hi <= lo {
            return lo;
        }
        lo + (self.next() % (hi - lo) as u64) as i64
    }
}

/// The bounding box of every level of world `w`: `[x0, y0, x1, y1]`.
fn world_box(project: &Project, w: usize) -> [i64; 4] {
    let g = project.grid_size as i64;
    let mut b = [i64::MAX, i64::MAX, i64::MIN, i64::MIN];
    for l in &project.worlds[w].levels {
        b[0] = b[0].min(l.world_px[0]);
        b[1] = b[1].min(l.world_px[1]);
        b[2] = b[2].max(l.world_px[0] + l.size[0] as i64 * g);
        b[3] = b[3].max(l.world_px[1] + l.size[1] as i64 * g);
    }
    if b[0] > b[2] {
        [0, 0, 64 * g, 36 * g]
    } else {
        b
    }
}

/// `--synthetic-notes N` (perf testing): N world pins over the world's
/// bounding box (icons cycling through the built-in ones, every third with
/// a label), ⌈N/4⌉ notes in every level (a title, every other with a body
/// line and tags), and N/10 four-point world paths. Ids and positions come
/// from a fixed seed, so the same project and N give the same bytes.
pub fn synthesize_notes(project: &mut Project, n: usize) {
    use worlds_core::{Annotation, Shape};
    if project.worlds.is_empty() {
        return;
    }
    let mut rng = Lcg(0x5eed_0005);
    let b = world_box(project, 0);
    let tags = ["forest", "caves", "garden", "vault", "todo", "landmark"];
    let mut notes = Vec::new();
    for i in 0..n {
        let mut a = Annotation::new(Shape::Pin {
            px: [rng.range(b[0], b[2]), rng.range(b[1], b[3])],
            icon: Some(crate::annot::PIN_ICONS[i % crate::annot::PIN_ICONS.len()].to_string()),
            label: (i % 3 == 0).then(|| format!("Pin {i}")),
        });
        a.id = Id::derived(&format!("syn/pin/{i}"));
        a.title = Some(format!("Synthetic pin {i}"));
        a.tags = vec![tags[i % tags.len()].to_string()];
        notes.push(a);
    }
    for i in 0..n / 10 {
        let mut pts = Vec::new();
        for _ in 0..4 {
            pts.push([rng.range(b[0], b[2]), rng.range(b[1], b[3])]);
        }
        let mut a = Annotation::new(Shape::Path { points: pts });
        a.id = Id::derived(&format!("syn/path/{i}"));
        a.title = Some(format!("Synthetic path {i}"));
        a.tags = vec!["drawing".into(), tags[i % 4].to_string()];
        notes.push(a);
    }
    project.worlds[0].notes.extend(notes);
    let per = n.div_ceil(4);
    let g = project.grid_size as i64;
    for w in &mut project.worlds {
        for l in &mut w.levels {
            let size = [l.size[0] as i64 * g, l.size[1] as i64 * g];
            for i in 0..per {
                let mut a = Annotation::new(Shape::Note {
                    px: [rng.range(0, size[0]), rng.range(0, size[1])],
                });
                a.id = Id::derived(&format!("syn/note/{}/{i}", l.slug));
                a.title = Some(format!("Note {i} in {}", l.slug));
                if i % 2 == 0 {
                    a.body = Some("Synthetic note body".into());
                    a.tags = vec!["todo".into(), tags[i % 4].to_string()];
                }
                l.notes.push(a);
            }
        }
    }
}

/// `--synthetic-guides N` (perf testing): N four-point guide polylines
/// spread over the world's bounding box (axis-aligned steps), locked,
/// tagged `layout`; every other one in the guide group
/// `syn_shifted` (offset two project cells down). Deterministic.
pub fn synthesize_guides(project: &mut Project, n: usize) {
    use worlds_core::{Annotation, GuideGroup, Shape};
    if project.worlds.is_empty() {
        return;
    }
    let mut rng = Lcg(0x5eed_0006);
    let b = world_box(project, 0);
    let g = project.grid_size as i64;
    let group = Slug::new("syn_shifted").expect("valid slug");
    let world = &mut project.worlds[0];
    if world.guide_group(group.as_str()).is_none() {
        let mut gg = GuideGroup::new(group.clone());
        gg.offset = [0, 2 * g];
        world.guide_groups.push(gg);
    }
    for i in 0..n {
        let mut p = [rng.range(b[0], b[2]), rng.range(b[1], b[3])];
        let mut pts = vec![p];
        for k in 0..3 {
            let len = rng.range(g, 8 * g);
            if k % 2 == 0 {
                p[0] += len;
            } else {
                p[1] += len;
            }
            pts.push(p);
        }
        let mut a = Annotation::new(Shape::Guide {
            points: pts,
            closed: false,
            group: (i % 2 == 1).then(|| group.clone()),
        });
        a.id = Id::derived(&format!("syn/guide/{i}"));
        a.locked = true;
        a.tags = vec!["layout".into()];
        world.notes.push(a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewport::camera::WRect;
    use crate::viewport::scene::Scene;

    #[test]
    fn synthetic_world_scales() {
        let doc = crate::viewport::scene::tests::small_doc();
        let mut p = doc.project.clone();
        synthesize(&mut p, 500);
        let doc = Document::from_project(PathBuf::from("/nonexistent"), p);
        assert_eq!(doc.levels.len(), 500);
        let mut slugs: Vec<&str> = (0..500).map(|i| doc.level(i).slug.as_str()).collect();
        slugs.sort();
        slugs.dedup();
        assert_eq!(slugs.len(), 500);
        // No two levels overlap.
        let rects: Vec<[f64; 4]> = (0..500).map(|i| doc.level_rect(i)).collect();
        let r = |a: [f64; 4]| WRect::from_pos_size([a[0], a[1]], [a[2], a[3]]);
        for i in 0..500 {
            for j in i + 1..500 {
                assert!(!r(rects[i]).intersects(&r(rects[j])));
            }
        }
        // One level's worth of view only builds that level's chunks.
        let mut scene = Scene::new(&doc);
        let mut out = Vec::new();
        let st = scene.collect_visible(&doc, &r(rects[0]), |_| true, None, &mut out);
        assert!(out.iter().all(|c| c.level == 0), "{out:?}");
        assert!(st.rebuilt <= 8, "{}", st.rebuilt);
    }

    #[test]
    fn synthetic_icons_skip_blank_tiles() {
        let doc = crate::viewport::scene::tests::small_doc();
        let mut p = doc.project.clone();
        let Some(ts) = p.tilesets.first().cloned() else {
            panic!("small_doc has a tileset");
        };
        // A 4 x 4 atlas where only two tiles draw enough for an icon.
        let [tw, th] = ts.tile_size;
        let (w, h) = (ts.tile_px([3, 3])[0] + tw, ts.tile_px([3, 3])[1] + th);
        let mut img = image::RgbaImage::new(w, h);
        // Fill the first `n` pixels (row-major) of tile `t` with alpha `a`.
        let mut fill = |t: [u32; 2], n: u32, a: u8| {
            let [x, y] = ts.tile_px(t);
            for k in 0..n {
                img.put_pixel(x + k % tw, y + k / tw, image::Rgba([255, 255, 255, a]));
            }
        };
        let area = tw * th;
        fill([1, 0], area, 255); // all of it
        fill([2, 3], area / 4, 200); // a quarter
        fill([0, 1], 1, 255); // a stray pixel: blank as an icon
        fill([3, 3], area / 20, 255); // 5 %
        fill([2, 2], area, 40); // all of it, but faint
        let atlas = SynthAtlas::from_image(&ts, &img);
        assert_eq!(atlas.drawn, [[1, 0], [2, 3]]);
        // Deterministic, and with nothing above the bar any drawn tile goes.
        assert_eq!(SynthAtlas::from_image(&ts, &img), atlas);
        let mut faint = image::RgbaImage::new(w, h);
        let [x, y] = ts.tile_px([3, 1]);
        faint.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
        assert_eq!(SynthAtlas::from_image(&ts, &faint).drawn, [[3, 1]]);
        synthesize_entities(&mut p, 5, Some(&atlas));
        let icons: Vec<[u32; 2]> = p
            .entities
            .iter()
            .filter(|d| d.slug.as_str().starts_with("syn_"))
            .map(|d| d.icon.as_ref().unwrap().tile)
            .collect();
        assert_eq!(icons, [[1, 0], [2, 3], [1, 0], [2, 3], [1, 0]]);
    }
}
