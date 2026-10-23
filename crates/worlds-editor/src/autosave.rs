//! Autosave in the editor (see `worlds_core::autosave` for the files).
//!
//! The app asks [`Autosaver::due`] once per frame; when it is time (the
//! interval since the first unsaved change, a focus loss, a signal) it
//! renders the dirty files on the UI thread with
//! [`worlds_core::format::render_dirty`] (the bytes Ctrl+S would write) and
//! hands them to a writer thread, which writes the generation and then the
//! manifest. At most one write is in flight; a newer request replaces a
//! queued one, and a clear (after a save, a discard, a reload) is queued the
//! same way, so the files on disk always end as the newest request says.
//! A revision counts as autosaved only once its write succeeded
//! ([`Autosaver::poll`]); a failed one is reported and written again.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use worlds_core::autosave::{self, Generation};
use worlds_core::format::render_dirty;

/// The default interval of the window.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);

/// How long autosave waits for an open gesture or agent batch to end
/// before writing anyway.
pub const BUSY_CAP: Duration = Duration::from_secs(60);

/// `WORLDS_AUTOSAVE_SECS`.
pub const ENV: &str = "WORLDS_AUTOSAVE_SECS";

/// The autosave interval: `--autosave <secs>`, else `WORLDS_AUTOSAVE_SECS`,
/// else 30 s in the window and off headless. `0` (or less) is off.
pub fn interval(flag: Option<f64>, headless: bool) -> Option<Duration> {
    let secs = flag.or_else(|| {
        std::env::var(ENV)
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
    });
    match secs {
        Some(s) if s.is_finite() && s > 0.0 => Some(Duration::from_secs_f64(s)),
        Some(_) => None,
        None if headless => None,
        None => Some(DEFAULT_INTERVAL),
    }
}

/// Why this session doesn't autosave right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// A previous session's autosave waits for Recover / Discard / Later.
    Undecided,
    /// The user chose Later: the old autosave is kept, this session
    /// doesn't write over it.
    Later,
}

enum Job {
    /// A generation for revision `rev`.
    Write(PathBuf, Box<Generation>, u64),
    Clear(PathBuf),
}

/// What a finished job did, for [`Autosaver::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Done {
    /// The generation for this revision is on disk (its manifest written).
    Wrote(u64),
    /// Writing the generation for this revision failed.
    WriteFailed(u64, String),
    ClearFailed(String),
}

#[derive(Default)]
struct State {
    queued: Option<Job>,
    running: bool,
    /// Finished jobs, oldest first, until [`Autosaver::poll`] reads them.
    done: Vec<Done>,
    /// Tests: the writer takes no job while set.
    paused: bool,
}

struct Writer {
    shared: Arc<(Mutex<State>, Condvar)>,
    /// No thread could be spawned: jobs run on the caller's thread (an
    /// autosave still happens, only not in the background).
    inline: bool,
}

impl Writer {
    fn new() -> Self {
        let shared: Arc<(Mutex<State>, Condvar)> = Arc::default();
        let s = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("worlds-autosave".into())
            .spawn(move || run(&s));
        let inline = match spawned {
            Ok(_) => false,
            Err(e) => {
                tracing::warn!("no autosave writer thread ({e}); autosaving on the UI thread");
                true
            }
        };
        Writer { shared, inline }
    }

    /// A writer without a thread, as a failed spawn gives (tests).
    #[cfg(test)]
    fn inline() -> Self {
        Writer {
            shared: Arc::default(),
            inline: true,
        }
    }

    fn submit(&self, job: Job) {
        if self.inline {
            let done = perform(&job);
            let (m, _) = &*self.shared;
            m.lock()
                .unwrap_or_else(|e| e.into_inner())
                .done
                .extend(done);
            return;
        }
        let (m, cv) = &*self.shared;
        let mut st = m.lock().unwrap_or_else(|e| e.into_inner());
        st.queued = Some(job);
        cv.notify_all();
    }

    /// Wait until nothing is queued or running; false on timeout.
    fn wait_idle(&self, timeout: Duration) -> bool {
        let (m, cv) = &*self.shared;
        let st = m.lock().unwrap_or_else(|e| e.into_inner());
        let (st, res) = cv
            .wait_timeout_while(st, timeout, |s| s.queued.is_some() || s.running)
            .unwrap_or_else(|e| e.into_inner());
        drop(st);
        !res.timed_out()
    }

    fn take_done(&self) -> Vec<Done> {
        let (m, _) = &*self.shared;
        std::mem::take(&mut m.lock().unwrap_or_else(|e| e.into_inner()).done)
    }

    fn pause(&self, paused: bool) {
        let (m, cv) = &*self.shared;
        m.lock().unwrap_or_else(|e| e.into_inner()).paused = paused;
        cv.notify_all();
    }
}

/// Run one job; what it did (nothing to report for a clear that worked).
fn perform(job: &Job) -> Option<Done> {
    match job {
        Job::Write(dir, g, rev) => Some(match autosave::write(dir, g) {
            Ok(()) => Done::Wrote(*rev),
            Err(e) => {
                let e = format!(
                    "autosave to {} failed: {e}",
                    autosave::autosave_dir(dir).display()
                );
                tracing::warn!("{e}");
                Done::WriteFailed(*rev, e)
            }
        }),
        Job::Clear(dir) => autosave::clear(dir).err().map(|e| {
            let e = format!(
                "removing {} failed: {e}",
                autosave::autosave_dir(dir).display()
            );
            tracing::warn!("{e}");
            Done::ClearFailed(e)
        }),
    }
}

fn run(shared: &(Mutex<State>, Condvar)) {
    let (m, cv) = shared;
    loop {
        let job = {
            let mut st = m.lock().unwrap_or_else(|e| e.into_inner());
            while st.queued.is_none() || st.paused {
                st = cv.wait(st).unwrap_or_else(|e| e.into_inner());
            }
            st.running = true;
            st.queued.take().expect("queued")
        };
        let done = perform(&job);
        let mut st = m.lock().unwrap_or_else(|e| e.into_inner());
        st.running = false;
        st.done.extend(done);
        cv.notify_all();
    }
}

/// A frame's view of the session, for [`Autosaver::due`].
pub struct Tick {
    /// The document's revision ([`crate::edit::Editing::rev`]).
    pub rev: u64,
    pub dirty: bool,
    /// A gesture or an agent batch is open.
    pub busy: bool,
    /// The window just lost focus.
    pub focus_lost: bool,
}

/// What [`Autosaver::due`] says to do this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    Nothing,
    /// Write an autosave now.
    Write,
    /// Nothing yet; ask again after this long (repaint then).
    After(Duration),
    /// The document is clean and this session's autosave is on disk:
    /// remove it.
    Clear,
}

/// The editor's autosave state for the open project.
pub struct Autosaver {
    interval: Option<Duration>,
    /// No session lock (or a synthetic world): never autosave.
    pub enabled: bool,
    pub blocked: Option<Blocked>,
    dir: Option<PathBuf>,
    /// The revision the newest queued autosave (or the last clean state)
    /// had: nothing new to write while the document stays at it.
    last_rev: Option<u64>,
    /// The revision the newest autosave that reached the disk had (only a
    /// write that succeeded counts).
    saved_rev: Option<u64>,
    /// When the revision first differed from `last_rev`.
    changed_since: Option<Instant>,
    busy_since: Option<Instant>,
    generation: u64,
    /// An autosave of this session's may be on disk: clear it when the
    /// document is clean again.
    pub owns: bool,
    /// Whether the document was dirty last frame (a save while blocked by
    /// Later asks again).
    was_dirty: bool,
    writer: Option<Writer>,
    /// UI-thread render times of every autosave, in ms (perf reporting).
    pub render_ms: Vec<f64>,
    /// How many autosaves this session queued.
    pub written: usize,
}

impl Autosaver {
    pub fn new(interval: Option<Duration>) -> Self {
        Autosaver {
            interval,
            enabled: false,
            blocked: None,
            dir: None,
            last_rev: None,
            saved_rev: None,
            changed_since: None,
            busy_since: None,
            generation: 1,
            owns: false,
            was_dirty: false,
            writer: None,
            render_ms: Vec::new(),
            written: 0,
        }
    }

    pub fn interval(&self) -> Option<Duration> {
        self.interval
    }

    /// A (re)opened document in `dir` at revision `rev`, clean: start over
    /// (the writer keeps running). `enabled`: this session may autosave.
    pub fn reset(&mut self, dir: &Path, rev: u64, enabled: bool) {
        self.dir = Some(dir.to_path_buf());
        self.enabled = enabled;
        self.blocked = None;
        self.last_rev = Some(rev);
        self.saved_rev = None;
        self.changed_since = None;
        self.busy_since = None;
        self.owns = false;
        self.was_dirty = false;
        self.generation = autosave::next_generation(dir);
    }

    /// The document now at `rev` is what is autosaved (after a recovery
    /// that adopted the old autosave as this session's).
    pub fn adopt(&mut self, rev: u64, owns: bool) {
        self.last_rev = Some(rev);
        self.saved_rev = owns.then_some(rev);
        self.changed_since = None;
        self.owns = owns;
        self.was_dirty = true;
        if let Some(dir) = &self.dir {
            self.generation = autosave::next_generation(dir);
        }
    }

    /// Whether this session writes autosaves at all right now.
    pub fn active(&self) -> bool {
        self.enabled && self.blocked.is_none() && self.interval.is_some()
    }

    /// What to do this frame; `Some(true)` in `.1` when a save happened
    /// while blocked by Later (the caller asks about recovery again).
    pub fn due(&mut self, t: &Tick, now: Instant) -> (Due, bool) {
        let saved_while_later = self.was_dirty && !t.dirty && self.blocked == Some(Blocked::Later);
        self.was_dirty = t.dirty;
        if !t.dirty {
            self.last_rev = Some(t.rev);
            self.changed_since = None;
            self.busy_since = None;
            return (
                if self.owns { Due::Clear } else { Due::Nothing },
                saved_while_later,
            );
        }
        let Some(interval) = self.interval.filter(|_| self.active()) else {
            return (Due::Nothing, false);
        };
        if self.last_rev == Some(t.rev) {
            return (Due::Nothing, false);
        }
        let since = *self.changed_since.get_or_insert(now);
        if t.busy {
            let b = *self.busy_since.get_or_insert(now);
            if now.duration_since(b) < BUSY_CAP {
                return (Due::After(Duration::from_millis(250)), false);
            }
        } else {
            self.busy_since = None;
        }
        let waited = now.duration_since(since);
        if t.focus_lost || waited >= interval {
            (Due::Write, false)
        } else {
            (Due::After(interval - waited), false)
        }
    }

    /// Render the dirty files of `project` at `rev` now (UI thread) and
    /// queue the write. Returns the render time.
    pub fn write_now(
        &mut self,
        project: &worlds_core::Project,
        rev: u64,
        last_label: Option<String>,
    ) -> Result<Duration, String> {
        let Some(dir) = self.dir.clone() else {
            return Err("no project".into());
        };
        let t = Instant::now();
        let render = render_dirty(project).map_err(|e| format!("autosave: {e}"))?;
        let g = autosave::generation(project, render, self.generation, rev, last_label);
        let took = t.elapsed();
        self.render_ms.push(took.as_secs_f64() * 1e3);
        self.queue(dir, g, rev);
        Ok(took)
    }

    /// Queue writing generation `g` (of revision `rev`) into `dir`.
    fn queue(&mut self, dir: PathBuf, g: Generation, rev: u64) {
        self.generation = self.generation.max(g.manifest.generation) + 1;
        self.writer
            .get_or_insert_with(Writer::new)
            .submit(Job::Write(dir, Box::new(g), rev));
        self.written += 1;
        // "May be on disk": a write that fails part way can still leave
        // files of ours behind for a clear to remove.
        self.owns = true;
        self.last_rev = Some(rev);
        self.changed_since = None;
        self.busy_since = None;
    }

    /// Read what the writer finished since the last call: a write that
    /// reached the disk counts as autosaved; one that failed is retried
    /// (one interval later, or at the next focus loss) and its error
    /// returned, as is a failed clear's. Call once per frame.
    pub fn poll(&mut self, now: Instant) -> Vec<String> {
        let Some(w) = &self.writer else {
            return Vec::new();
        };
        let mut errors = Vec::new();
        for d in w.take_done() {
            match d {
                Done::Wrote(rev) => self.saved_rev = Some(rev),
                Done::WriteFailed(rev, e) => {
                    // Not autosaved after all. Unless a newer write is
                    // already queued, write this revision again later.
                    if self.last_rev == Some(rev) {
                        self.last_rev = None;
                        self.changed_since = Some(now);
                    }
                    errors.push(e);
                }
                Done::ClearFailed(e) => errors.push(e),
            }
        }
        errors
    }

    /// Queue removing this session's autosave (after a save, a discard, a
    /// reload). Does nothing unless it owns one.
    pub fn clear(&mut self) {
        if !self.owns {
            return;
        }
        if let Some(dir) = self.dir.clone() {
            self.writer
                .get_or_insert_with(Writer::new)
                .submit(Job::Clear(dir));
        }
        self.owns = false;
    }

    /// Whether an autosave of revision `rev` reached the disk (call
    /// [`Self::poll`] first; a queued or failed write doesn't count).
    pub fn current(&self, rev: u64) -> bool {
        self.owns && self.saved_rev == Some(rev)
    }

    /// Wait for queued writes; false if they didn't finish in `timeout`.
    pub fn flush(&self, timeout: Duration) -> bool {
        self.writer.as_ref().is_none_or(|w| w.wait_idle(timeout))
    }

    /// Tests: stop (or restart) the writer taking jobs, to hold a write
    /// in flight.
    #[doc(hidden)]
    pub fn pause_writer(&mut self, paused: bool) {
        self.writer.get_or_insert_with(Writer::new).pause(paused);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(rev: u64, dirty: bool) -> Tick {
        Tick {
            rev,
            dirty,
            busy: false,
            focus_lost: false,
        }
    }

    fn gen_of(n: u64, rev: u64) -> Generation {
        use std::collections::BTreeMap;
        use worlds_core::autosave::{FileEntry, FileState, Manifest};
        let bytes = b"x\n".to_vec();
        Generation {
            manifest: Manifest {
                format: autosave::FORMAT,
                generation: n,
                written: autosave::now_rfc3339(),
                pid: 1,
                rev,
                last_label: None,
                dirty: vec!["a.json".into()],
                files: BTreeMap::from([(
                    "a.json".to_string(),
                    FileEntry {
                        state: FileState::Modified,
                        base: None,
                        hash: Some(autosave::hex(autosave::hash(&bytes))),
                    },
                )]),
            },
            files: BTreeMap::from([(PathBuf::from("a.json"), bytes)]),
        }
    }

    /// A write that fails isn't counted as autosaved: `current` stays
    /// false, the error comes back once, and the same revision is due
    /// again an interval later; once the disk works it is written.
    #[test]
    fn a_failed_write_is_reported_and_retried() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // `.worlds` is a file: nothing can be written under it.
        std::fs::write(dir.join(".worlds"), "in the way").unwrap();
        let mut a = Autosaver::new(Some(Duration::from_secs(30)));
        a.reset(dir, 0, true);
        let t0 = Instant::now();
        assert_eq!(
            a.due(&tick(1, true), t0).0,
            Due::After(Duration::from_secs(30))
        );
        a.queue(dir.to_path_buf(), gen_of(1, 1), 1);
        assert!(a.flush(Duration::from_secs(5)));
        let errors = a.poll(t0);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("autosave to"), "{errors:?}");
        assert!(!a.current(1), "a failed write isn't an autosave");
        assert!(a.poll(t0).is_empty(), "reported once");
        // Due again, one interval after the failure.
        let t1 = t0 + Duration::from_secs(10);
        assert_eq!(
            a.due(&tick(1, true), t1).0,
            Due::After(Duration::from_secs(20))
        );
        assert_eq!(
            a.due(&tick(1, true), t0 + Duration::from_secs(31)).0,
            Due::Write
        );
        std::fs::remove_file(dir.join(".worlds")).unwrap();
        a.queue(dir.to_path_buf(), gen_of(2, 1), 1);
        assert!(a.flush(Duration::from_secs(5)));
        assert!(a.poll(t1).is_empty());
        assert!(a.current(1));
        assert!(autosave::autosave_dir(dir).join("manifest.json").exists());
    }

    /// Without a writer thread, jobs run in place: a flush never waits on
    /// a thread that doesn't exist, and errors still come back.
    #[test]
    fn without_a_thread_writes_run_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut a = Autosaver::new(Some(Duration::from_secs(30)));
        a.reset(dir, 0, true);
        a.writer = Some(Writer::inline());
        a.queue(dir.to_path_buf(), gen_of(1, 1), 1);
        let t = Instant::now();
        assert!(a.flush(Duration::from_secs(5)));
        assert!(t.elapsed() < Duration::from_secs(1));
        assert!(a.poll(t).is_empty());
        assert!(a.current(1));
        std::fs::write(dir.join("blocker"), "").unwrap();
        a.dir = Some(dir.join("blocker"));
        a.queue(dir.join("blocker"), gen_of(2, 2), 2);
        assert_eq!(a.poll(t).len(), 1);
        assert!(!a.current(2));
    }

    #[test]
    fn interval_flag_beats_the_default_and_zero_is_off() {
        assert_eq!(interval(Some(2.0), true), Some(Duration::from_secs(2)));
        assert_eq!(interval(Some(0.0), false), None);
        if std::env::var_os(ENV).is_none() {
            assert_eq!(interval(None, false), Some(DEFAULT_INTERVAL));
            assert_eq!(interval(None, true), None);
        }
    }

    #[test]
    fn due_waits_for_the_interval_a_gesture_and_a_change() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = Autosaver::new(Some(Duration::from_secs(30)));
        a.reset(tmp.path(), 0, true);
        let t0 = Instant::now();
        assert_eq!(a.due(&tick(0, false), t0).0, Due::Nothing);
        // Dirty: due 30 s after the first change.
        assert_eq!(
            a.due(&tick(1, true), t0).0,
            Due::After(Duration::from_secs(30))
        );
        let t1 = t0 + Duration::from_secs(31);
        // A gesture holds it (up to BUSY_CAP).
        let busy = Tick {
            busy: true,
            ..tick(2, true)
        };
        assert!(matches!(a.due(&busy, t1).0, Due::After(_)));
        assert_eq!(a.due(&tick(2, true), t1).0, Due::Write);
        // A focus loss writes at once.
        let lost = Tick {
            focus_lost: true,
            ..tick(2, true)
        };
        assert_eq!(a.due(&lost, t0).0, Due::Write);
        // Blocked: nothing.
        a.blocked = Some(Blocked::Later);
        assert_eq!(a.due(&lost, t1).0, Due::Nothing);
        // Saved while blocked by Later: ask again.
        assert!(a.due(&tick(3, false), t1).1);
    }
}
