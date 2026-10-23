//! `worlds-server <project dir> [--save-on-write]`: serve a project
//! headlessly (no editor) until SIGINT or SIGTERM. Prints one JSON line,
//! `{"port": .., "pid": .., "project": ..}`, once it is listening; the token
//! is in `<project>/.worlds/server.json`, as with the editor.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut dir: Option<PathBuf> = None;
    let mut opts = worlds_server::headless::Options::default();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--save-on-write" => opts.save_on_write = true,
            "-h" | "--help" => {
                println!("usage: worlds-server <project dir> [--save-on-write]");
                return ExitCode::SUCCESS;
            }
            a if a.starts_with('-') => {
                eprintln!("worlds-server: unknown option {a}");
                return ExitCode::from(2);
            }
            a if dir.is_none() => dir = Some(PathBuf::from(a)),
            a => {
                eprintln!("worlds-server: one project directory only (got {a:?} too)");
                return ExitCode::from(2);
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("usage: worlds-server <project dir> [--save-on-write]");
        return ExitCode::from(2);
    };
    let server = match worlds_server::headless::run(&dir, opts) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("worlds-server: {e}");
            return ExitCode::FAILURE;
        }
    };
    let project = std::fs::canonicalize(&dir).unwrap_or(dir);
    println!(
        "{}",
        serde_json::json!({ "port": server.port(), "pid": std::process::id(), "project": project })
    );
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("worlds-server: {e}");
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(wait_for_signal());
    server.stop();
    ExitCode::SUCCESS
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
