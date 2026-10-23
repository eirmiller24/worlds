//! Live problems: the Problems pane follows every edit.
//!
//! Any edit, undo, redo or agent write bumps the command layer's revision
//! ([`crate::edit::Editing::rev`]). [`LiveProblems::poll`], once per frame,
//! notes the new revision and, once the project has been quiet for
//! [`QUIET`] and no gesture or agent batch is open, revalidates. A stroke
//! never pays for validation per move: the whole stroke is one revalidation
//! after it ends.
//!
//! `worlds_core::validate` costs about 0.15 ms on a 5-level project but 28 ms (p95
//! 30 ms) on a `--synthetic 500 --synthetic-entities 100` world (release),
//! over the 4 ms an inline run may take. So the window validates on a
//! worker thread, against a clone of the project (about 2.4 ms for that
//! world) tagged with the revision; a result for an older revision is
//! dropped. Headless renders validate inline, so the picture is complete.
//! The editor sends `problems` over the live API only when the list
//! changed.

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use worlds_core::Problem;

use crate::doc::Document;

/// How long the project must be quiet before it is revalidated.
pub const QUIET: Duration = Duration::from_millis(200);

/// What [`LiveProblems::poll`] found.
#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    /// Nothing to do.
    Idle,
    /// Revalidation is due in this long (or once the gesture ends): ask
    /// for a frame then.
    Wait(Duration),
    /// A fresh list (validation's, then the disk problems).
    Fresh(Vec<Problem>),
}

/// The live validation state (see the module docs).
pub struct LiveProblems {
    /// The newest revision seen.
    seen: u64,
    /// The revision the current list belongs to.
    done: u64,
    /// When `seen` last changed.
    changed_at: Option<Instant>,
    /// A worker validating revision `.0`.
    worker: Option<(u64, Receiver<Vec<Problem>>)>,
    /// Validate on this thread (headless).
    pub inline: bool,
    /// The quiet time before a revalidation ([`QUIET`]; zero headless).
    pub quiet: Duration,
    /// Problems of entity files that changed on disk and didn't load, by
    /// file: kept until that file loads.
    pub disk: BTreeMap<String, Vec<Problem>>,
    /// Revalidations started (tests).
    pub runs: usize,
    /// Worker results dropped because the project changed since (tests).
    pub stale: usize,
    /// Called from the worker when it is done (a repaint).
    pub wake: Option<crate::doc::Wake>,
}

impl LiveProblems {
    /// For a document just opened at revision `rev` (its problems are
    /// current).
    pub fn new(rev: u64, inline: bool) -> Self {
        LiveProblems {
            seen: rev,
            done: rev,
            changed_at: None,
            worker: None,
            inline,
            quiet: if inline { Duration::ZERO } else { QUIET },
            disk: BTreeMap::new(),
            runs: 0,
            stale: 0,
            wake: None,
        }
    }

    /// Whether the list is behind the project.
    pub fn dirty(&self) -> bool {
        self.done != self.seen
    }

    /// Mark revision `rev` done without validating (a save just did).
    pub fn validated(&mut self, rev: u64) {
        self.seen = rev;
        self.done = rev;
        self.changed_at = None;
    }

    /// The validation list plus the disk problems.
    fn with_disk(&self, mut list: Vec<Problem>) -> Vec<Problem> {
        for ps in self.disk.values() {
            list.extend(ps.iter().cloned());
        }
        list
    }

    /// Once per frame: `rev` is the command layer's revision, `busy` that a
    /// gesture or agent batch is open.
    pub fn poll(&mut self, doc: &Document, rev: u64, busy: bool, now: Instant) -> Poll {
        if rev != self.seen {
            self.seen = rev;
            self.changed_at = Some(now);
        }
        if let Some((r, rx)) = &self.worker {
            match rx.try_recv() {
                Ok(list) => {
                    let r = *r;
                    self.worker = None;
                    if r == self.seen {
                        self.done = r;
                        return Poll::Fresh(self.with_disk(list));
                    }
                    self.stale += 1;
                }
                Err(TryRecvError::Empty) => return Poll::Idle,
                Err(TryRecvError::Disconnected) => self.worker = None,
            }
        }
        if self.done == self.seen {
            return Poll::Idle;
        }
        let since = self
            .changed_at
            .map_or(self.quiet, |t| now.duration_since(t));
        if since < self.quiet {
            return Poll::Wait(self.quiet - since);
        }
        if busy {
            return Poll::Wait(self.quiet);
        }
        self.runs += 1;
        if self.inline {
            self.done = self.seen;
            return Poll::Fresh(self.with_disk(worlds_core::validate(&doc.project)));
        }
        let project = doc.project.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = self.wake.clone();
        let spawned = std::thread::Builder::new()
            .name("validate".into())
            .spawn(move || {
                let _ = tx.send(worlds_core::validate(&project));
                if let Some(w) = wake {
                    w();
                }
            });
        match spawned {
            Ok(_) => self.worker = Some((self.seen, rx)),
            Err(e) => {
                tracing::warn!("validation worker: {e}; validating here");
                self.done = self.seen;
                return Poll::Fresh(self.with_disk(worlds_core::validate(&doc.project)));
            }
        }
        Poll::Idle
    }
}
