//! `tracing` into a ring buffer the log panel reads, plus stderr.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

pub const CAPACITY: usize = 2000;

#[derive(Debug, Clone)]
pub struct LogLine {
    pub level: Level,
    /// Seconds since the Unix epoch.
    pub time: f64,
    pub target: String,
    pub message: String,
}

/// Shared ring of the most recent [`CAPACITY`] lines.
#[derive(Clone, Default)]
pub struct LogBuffer {
    inner: Arc<Mutex<LogRing>>,
}

#[derive(Default)]
struct LogRing {
    lines: VecDeque<LogLine>,
    /// Total lines ever pushed (so the panel can tell when to scroll).
    total: u64,
}

impl LogBuffer {
    pub fn push(&self, line: LogLine) {
        let mut r = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if r.lines.len() == CAPACITY {
            r.lines.pop_front();
        }
        r.lines.push_back(line);
        r.total += 1;
    }

    /// Run `f` over the buffered lines, oldest first.
    pub fn with_lines<R>(&self, f: impl FnOnce(&VecDeque<LogLine>, u64) -> R) -> R {
        let r = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&r.lines, r.total)
    }

    pub fn clear(&self) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lines
            .clear();
    }
}

struct RingLayer(LogBuffer);

#[derive(Default)]
struct Msg(String);

impl Visit for Msg {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            let _ = write!(self.0, " {}={value}", field.name());
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut m = Msg::default();
        event.record(&mut m);
        self.0.push(LogLine {
            level: *event.metadata().level(),
            time: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64()),
            target: event.metadata().target().to_string(),
            message: m.0,
        });
    }
}

/// Install the global subscriber: the ring buffer (info and up from our
/// crates, warnings from everything) and stderr (`RUST_LOG`, default info).
/// Idempotent: later calls return the same buffer.
pub fn init() -> LogBuffer {
    static GLOBAL: std::sync::OnceLock<LogBuffer> = std::sync::OnceLock::new();
    GLOBAL.get_or_init(install).clone()
}

fn install() -> LogBuffer {
    let buf = LogBuffer::default();
    let ring_filter = EnvFilter::new(
        "warn,worlds_editor=debug,worlds_core=info,worlds_rules=info,wgpu_hal::vulkan::instance=off",
    );
    let stderr_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(
            // The Vulkan loader logs unrelated broken layer manifests as errors.
            "warn,worlds_editor=info,worlds_core=info,worlds_rules=info,wgpu_hal::vulkan::instance=off",
        ));
    let _ = tracing_subscriber::registry()
        .with(RingLayer(buf.clone()).with_filter(ring_filter))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .with_filter(stderr_filter),
        )
        .try_init();
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_the_newest() {
        let b = LogBuffer::default();
        for i in 0..CAPACITY + 5 {
            b.push(LogLine {
                level: Level::INFO,
                time: 0.0,
                target: "t".into(),
                message: i.to_string(),
            });
        }
        b.with_lines(|l, total| {
            assert_eq!(l.len(), CAPACITY);
            assert_eq!(l[0].message, "5");
            assert_eq!(total, CAPACITY as u64 + 5);
        });
    }
}
