//! SIGINT and SIGTERM: shut down cleanly (stop the live API, which removes
//! `.worlds/server.json`) instead of dying with the discovery file left
//! behind. A kill that can't be caught (SIGKILL, a crash) still leaves it;
//! clients see the pid is gone and treat it as stale.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How long the app gets to shut down after the signal before the process
/// exits anyway.
pub const GRACE: Duration = Duration::from_secs(10);

/// Watch for SIGINT and SIGTERM on a thread of its own. The returned flag
/// goes up on the first one and `wake` is called, so the frame loop sees it
/// and shuts down. If the process is still running `grace` later, it exits
/// with 130 (SIGINT) or 143 (SIGTERM). The handlers are in place when this
/// returns (it waits up to 5 s for the thread to install them), so a
/// signal sent as soon as the caller says it is up (`--serve` prints its
/// port after this) is caught, not the default kill.
pub fn watch(wake: impl Fn() + Send + 'static, grace: Duration) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let raised = flag.clone();
    let (ready_tx, ready) = std::sync::mpsc::channel::<()>();
    let spawned = std::thread::Builder::new()
        .name("worlds-signals".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            let Some(code) = rt.block_on(wait(ready_tx)) else {
                return;
            };
            tracing::info!("signal received: shutting down");
            raised.store(true, Ordering::SeqCst);
            wake();
            std::thread::sleep(grace);
            tracing::warn!("still running {grace:?} after the signal; exiting now");
            exit_now(code);
        });
    match spawned {
        Err(e) => tracing::warn!("no signal handling: {e}"),
        // `Err`: the thread could not watch (it says nothing more).
        Ok(_) => {
            let _ = ready.recv_timeout(Duration::from_secs(5));
        }
    }
    flag
}

/// End the process with `code` at once: flush stdout and stderr, then
/// `_exit`, which runs no exit handlers and no thread-local destructors.
///
/// The window calls this once its own shutdown is done (the live API
/// stopped and `server.json` removed, `ui.json` written), right after
/// `eframe::run_native` returns. eframe keeps winit's event loop in a
/// thread-local, so a normal exit drops it in libc's exit handlers, and
/// under Xvfb that teardown segfaults in winit's X11 keymap drop
/// (`XkbKeymap::drop` -> `xkb_keymap_unref`). Nothing of ours is left to do
/// by then, so skipping the toolkit's teardown loses nothing and the exit
/// status is ours.
pub fn exit_now(code: i32) -> ! {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    #[cfg(unix)]
    // SAFETY: `_exit` only ends the process; nothing runs after it.
    unsafe {
        libc::_exit(code)
    }
    #[cfg(not(unix))]
    std::process::exit(code)
}

/// The exit code for the first signal, or `None` if none can be watched.
/// `ready` hears once the handlers are installed.
async fn wait(ready: std::sync::mpsc::Sender<()>) -> Option<i32> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).ok()?;
        let mut int = signal(SignalKind::interrupt()).ok()?;
        let _ = ready.send(());
        tokio::select! {
            _ = term.recv() => Some(143),
            _ = int.recv() => Some(130),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ready.send(());
        tokio::signal::ctrl_c().await.ok().map(|()| 130)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A SIGTERM right after `watch` returns is caught: the handler is in
    /// place by then (a signal that came first used to kill the process
    /// with the default action, as `serve_stops_cleanly_on_sigterm` saw
    /// now and then).
    #[test]
    fn a_signal_right_after_watch_is_caught() {
        let flag = watch(|| {}, Duration::from_secs(3600));
        // SAFETY: raising a signal this process now handles.
        unsafe {
            libc::raise(libc::SIGTERM);
        }
        let start = std::time::Instant::now();
        while !flag.load(Ordering::SeqCst) {
            assert!(start.elapsed() < Duration::from_secs(5), "not caught");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
