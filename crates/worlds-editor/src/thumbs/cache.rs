//! The thumbnail cache: one egui texture per [`ThumbId`], a per-frame
//! budget shared by everyone who asks, and the disk policy (see the module
//! docs of [`crate::thumbs`]).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use image::RgbaImage;

use super::{Images, Source, ThumbId};
use crate::doc::Document;

/// How much picture-making one frame (one egui pass) may do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    /// Pictures made (drawn or read from disk) per frame; `None`: no limit.
    pub pictures: Option<usize>,
    /// Time spent making them per frame (at least one is always made);
    /// `None`: no limit.
    pub time: Option<Duration>,
    /// An out-of-date picture younger than this is kept a little longer
    /// (another frame is asked for when it is due): a brush stroke moves
    /// its level's key every frame, and redrawing the thumbnail every frame
    /// of a stroke would only show the same thing sooner. `None`: redrawn
    /// at once.
    pub refresh: Option<Duration>,
}

impl Budget {
    /// The interactive editor's: 8 pictures or 4 ms a frame, a stale one
    /// redrawn at most 4 times a second.
    pub const FRAME: Budget = Budget {
        pictures: Some(8),
        time: Some(Duration::from_millis(4)),
        refresh: Some(Duration::from_millis(250)),
    };
    /// Headless renders: every picture asked for, current, at once.
    pub const UNLIMITED: Budget = Budget {
        pictures: None,
        time: None,
        refresh: None,
    };
}

impl Default for Budget {
    fn default() -> Self {
        Budget::FRAME
    }
}

struct Entry {
    key: u64,
    tex: egui::TextureHandle,
    /// When the picture was made.
    made: Instant,
}

/// Every thumbnail texture the editor shows.
pub struct Thumbs {
    entries: HashMap<ThumbId, Entry>,
    images: Images,
    pub budget: Budget,
    /// Headless (`--screenshot`): read cache files, never write one
    /// (nothing is written into the project unless `--save`).
    pub headless: bool,
    /// The egui pass the budget below counts in.
    pass: Option<u64>,
    made_this_pass: usize,
    spent_this_pass: Duration,
    /// Pictures drawn so far (not read from a file; tests).
    pub rendered: usize,
    /// Pictures read from a cache file so far (tests).
    pub loaded: usize,
    /// Requests the budget turned away so far (tests).
    pub deferred: usize,
    /// `(levels, images_gen)` when deleted levels were last dropped.
    pruned: (usize, u64),
}

impl Default for Thumbs {
    fn default() -> Self {
        Thumbs {
            entries: HashMap::new(),
            images: Images::default(),
            budget: Budget::FRAME,
            headless: false,
            pass: None,
            made_this_pass: 0,
            spent_this_pass: Duration::ZERO,
            rendered: 0,
            loaded: 0,
            deferred: 0,
            pruned: (0, 0),
        }
    }
}

impl Thumbs {
    /// Forget every picture (a project was opened or reloaded).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.images = Images::default();
    }

    /// How many pictures are held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The texture of `src`'s thumbnail. Current: returned at once. Missing
    /// or out of date: made now if this frame's budget allows (read from
    /// its cache file, else drawn, and the file written), else the old
    /// texture, if any, is returned as it is and another frame is asked
    /// for (so is one made less than [`Budget::refresh`] ago). `None`:
    /// nothing to show yet.
    pub fn get(&mut self, ctx: &egui::Context, src: &dyn Source) -> Option<egui::TextureHandle> {
        let id = src.id();
        let key = src.key();
        if let Some(e) = self.entries.get(&id) {
            if e.key == key {
                return Some(e.tex.clone());
            }
            let age = e.made.elapsed();
            if let Some(r) = self.budget.refresh
                && age < r
            {
                ctx.request_repaint_after(r - age);
                return Some(e.tex.clone());
            }
        }
        if !self.spend(ctx) {
            self.deferred += 1;
            ctx.request_repaint();
            return self.entries.get(&id).map(|e| e.tex.clone());
        }
        let t = Instant::now();
        let img = self.picture(src);
        let ci = egui::ColorImage::from_rgba_unmultiplied(
            [img.width() as usize, img.height() as usize],
            img.as_raw(),
        );
        // Nearest when the map magnifies a level (pixel art stays crisp),
        // linear when a row shrinks a big one.
        let opts = egui::TextureOptions {
            magnification: egui::TextureFilter::Nearest,
            minification: egui::TextureFilter::Linear,
            ..egui::TextureOptions::NEAREST
        };
        let tex = match self.entries.get_mut(&id) {
            // In place: the texture id stays, so nothing holding it breaks.
            Some(e) => {
                e.tex.set(ci, opts);
                e.key = key;
                e.made = Instant::now();
                e.tex.clone()
            }
            None => {
                let tex = ctx.load_texture(format!("thumb {id:?}"), ci, opts);
                self.entries.insert(
                    id,
                    Entry {
                        key,
                        tex: tex.clone(),
                        made: Instant::now(),
                    },
                );
                tex
            }
        };
        self.spent_this_pass += t.elapsed();
        Some(tex)
    }

    /// Take one picture from this frame's budget; false when it is spent.
    fn spend(&mut self, ctx: &egui::Context) -> bool {
        let pass = ctx.cumulative_pass_nr();
        if self.pass != Some(pass) {
            self.pass = Some(pass);
            self.made_this_pass = 0;
            self.spent_this_pass = Duration::ZERO;
        }
        let b = self.budget;
        if b.pictures.is_some_and(|n| self.made_this_pass >= n)
            || (self.made_this_pass > 0 && b.time.is_some_and(|t| self.spent_this_pass >= t))
        {
            return false;
        }
        self.made_this_pass += 1;
        true
    }

    /// `src`'s picture: its cache file if there is one, else drawn (and
    /// the file written, older ones of the same thing removed, unless
    /// headless; a write failure is logged, the picture is still used).
    fn picture(&mut self, src: &dyn Source) -> RgbaImage {
        let file = src.file();
        if let Some(f) = &file
            && let Ok(img) = image::open(f.path())
        {
            self.loaded += 1;
            return img.to_rgba8();
        }
        let img = src.render(&mut self.images);
        self.rendered += 1;
        if let Some(f) = file.filter(|_| !self.headless) {
            let path = f.path();
            if let Err(e) = std::fs::create_dir_all(&f.dir).and_then(|_| {
                img.save(&path)
                    .map_err(|e| std::io::Error::other(e.to_string()))
            }) {
                tracing::warn!("cannot write {}: {e}", path.display());
            }
            for e in std::fs::read_dir(&f.dir).into_iter().flatten().flatten() {
                if f.is_older(&e.file_name().to_string_lossy()) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        img
    }

    /// Drop the pictures of levels `doc` no longer has. Cheap to call every
    /// frame: it only looks when the level count or the images changed.
    pub fn prune(&mut self, doc: &Document) {
        let sig = (doc.levels.len(), doc.images_gen);
        if self.pruned == sig {
            return;
        }
        self.pruned = sig;
        let ids: std::collections::HashSet<&worlds_core::Id> = doc.ids.iter().collect();
        self.entries.retain(|id, _| match id {
            ThumbId::Level(l) => ids.contains(l),
            ThumbId::Assembly(_) => true,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::Path;

    use super::super::CacheFile;
    use super::*;

    /// A source that counts its renders.
    struct Fake<'a> {
        id: &'a str,
        key: u64,
        file: Option<CacheFile>,
        drawn: &'a Cell<usize>,
    }

    impl Source for Fake<'_> {
        fn id(&self) -> ThumbId {
            ThumbId::Assembly(worlds_core::Slug::new(self.id).unwrap())
        }
        fn key(&self) -> u64 {
            self.key
        }
        fn file(&self) -> Option<CacheFile> {
            self.file.clone()
        }
        fn render(&self, _: &mut Images) -> RgbaImage {
            self.drawn.set(self.drawn.get() + 1);
            RgbaImage::from_pixel(4, 3, image::Rgba([self.key as u8, 0, 0, 255]))
        }
    }

    /// Run `f` inside one egui pass (one frame of budget).
    fn pass(ctx: &egui::Context, mut f: impl FnMut(&egui::Context)) {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| f(ui.ctx()));
        // No renderer here to take the uploads.
        out.textures_delta.clear();
    }

    /// The interactive budget, but a stale picture is redrawn at once.
    fn now() -> Thumbs {
        Thumbs {
            budget: Budget {
                refresh: None,
                ..Budget::FRAME
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_stale_picture_is_redrawn_at_most_every_refresh() {
        let ctx = egui::Context::default();
        let drawn = Cell::new(0);
        let mut t = Thumbs {
            budget: Budget {
                refresh: Some(Duration::from_millis(80)),
                ..Budget::FRAME
            },
            ..Default::default()
        };
        let mut src = Fake {
            id: "door",
            key: 1,
            file: None,
            drawn: &drawn,
        };
        pass(&ctx, |ctx| assert!(t.get(ctx, &src).is_some()));
        // A stroke: the key moves every frame, the picture stays a while.
        for key in 2..5 {
            src.key = key;
            pass(&ctx, |ctx| assert!(t.get(ctx, &src).is_some()));
        }
        assert_eq!(drawn.get(), 1);
        std::thread::sleep(Duration::from_millis(100));
        pass(&ctx, |ctx| assert!(t.get(ctx, &src).is_some()));
        assert_eq!(drawn.get(), 2);
        // Current now: nothing more to draw.
        pass(&ctx, |ctx| assert!(t.get(ctx, &src).is_some()));
        assert_eq!(drawn.get(), 2);
    }

    fn file(dir: &Path, hash: &str) -> CacheFile {
        CacheFile {
            dir: dir.to_path_buf(),
            prefix: "assembly-door-".into(),
            hash: hash.into(),
        }
    }

    #[test]
    fn a_picture_is_made_once_per_key_and_replaced_in_place() {
        let ctx = egui::Context::default();
        let drawn = Cell::new(0);
        let mut t = now();
        let mut src = Fake {
            id: "door",
            key: 1,
            file: None,
            drawn: &drawn,
        };
        let mut first = None;
        pass(&ctx, |ctx| first = t.get(ctx, &src).map(|t| t.id()));
        pass(&ctx, |ctx| {
            assert_eq!(t.get(ctx, &src).map(|t| t.id()), first)
        });
        assert_eq!((drawn.get(), t.rendered), (1, 1));
        // A new key: drawn again, into the same texture.
        src.key = 2;
        pass(&ctx, |ctx| {
            let again = t.get(ctx, &src).unwrap();
            assert_eq!(Some(again.id()), first);
        });
        assert_eq!((drawn.get(), t.rendered, t.len()), (2, 2, 1));
    }

    #[test]
    fn the_budget_spreads_pictures_over_frames_and_keeps_the_old_one_meanwhile() {
        let ctx = egui::Context::default();
        let drawn = Cell::new(0);
        let mut t = Thumbs {
            budget: Budget {
                pictures: Some(2),
                time: None,
                refresh: None,
            },
            ..Default::default()
        };
        let names = ["a", "b", "c", "d", "e"];
        let srcs = |key: u64| -> Vec<Fake<'_>> {
            names
                .iter()
                .map(|id| Fake {
                    id,
                    key,
                    file: None,
                    drawn: &drawn,
                })
                .collect()
        };
        let got = |t: &mut Thumbs, key: u64| {
            let mut n = 0;
            pass(&ctx, |ctx| {
                n = srcs(key)
                    .iter()
                    .filter(|s| t.get(ctx, *s).is_some())
                    .count()
            });
            n
        };
        // Five asked for, two a frame: 2, 4, then all 5.
        assert_eq!(got(&mut t, 1), 2);
        assert_eq!(got(&mut t, 1), 4);
        assert_eq!(got(&mut t, 1), 5);
        assert_eq!(drawn.get(), 5);
        assert_eq!(t.deferred, 3 + 1);
        // Every key moves: the old pictures stay on screen while two a
        // frame are redrawn.
        assert_eq!(got(&mut t, 2), 5);
        assert_eq!(drawn.get(), 7);
        // Unlimited (headless): everything in one frame.
        t.budget = Budget::UNLIMITED;
        assert_eq!(got(&mut t, 3), 5);
        assert_eq!(drawn.get(), 12);
    }

    #[test]
    fn a_new_picture_replaces_the_older_files_and_headless_writes_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("thumbs");
        let ctx = egui::Context::default();
        let drawn = Cell::new(0);
        // Headless: drawn, nothing written.
        let mut t = Thumbs {
            headless: true,
            ..Default::default()
        };
        let src = Fake {
            id: "door",
            key: 1,
            file: Some(file(&dir, "0000000000000001")),
            drawn: &drawn,
        };
        pass(&ctx, |ctx| assert!(t.get(ctx, &src).is_some()));
        assert_eq!(t.rendered, 1);
        assert!(!dir.exists(), "headless wrote into the project");
        // Interactive: written; a second cache (the next session) reads it.
        let mut t = now();
        pass(&ctx, |ctx| assert!(t.get(ctx, &src).is_some()));
        let old = file(&dir, "0000000000000001").path();
        assert!(old.exists());
        // Neighbours that only look alike stay.
        for other in [
            "assembly-door_b-0000000000000009.png",
            "assembly-door-b-0000000000000009.png",
        ] {
            std::fs::write(dir.join(other), b"x").unwrap();
        }
        let mut next = now();
        pass(&ctx, |ctx| assert!(next.get(ctx, &src).is_some()));
        assert_eq!((next.loaded, next.rendered, drawn.get()), (1, 0, 2));
        // A new key: a new file, the old one gone, the look-alikes kept.
        let src2 = Fake {
            key: 2,
            file: Some(file(&dir, "0000000000000002")),
            ..src
        };
        pass(&ctx, |ctx| assert!(next.get(ctx, &src2).is_some()));
        assert!(!old.exists());
        assert!(file(&dir, "0000000000000002").path().exists());
        assert!(dir.join("assembly-door_b-0000000000000009.png").exists());
        assert!(dir.join("assembly-door-b-0000000000000009.png").exists());
        // Headless with the file there reads it and removes nothing.
        let mut hl = Thumbs {
            headless: true,
            ..Default::default()
        };
        pass(&ctx, |ctx| assert!(hl.get(ctx, &src).is_some()));
        assert_eq!((hl.loaded, hl.rendered), (0, 1));
        assert!(file(&dir, "0000000000000002").path().exists());
    }
}
