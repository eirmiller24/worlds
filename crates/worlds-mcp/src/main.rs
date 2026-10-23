//! `worlds-mcp [--project <dir>] [--headless] [--name <client>]`: the
//! Worlds MCP server on stdio. Logs go to stderr; stdout is the MCP channel.

use std::path::PathBuf;

use clap::Parser;
use rmcp::ServiceExt;
use worlds_mcp::backend::live_server;
use worlds_mcp::{Config, WorldsMcp};

#[derive(Parser, Debug)]
#[command(
    name = "worlds-mcp",
    version,
    about = "MCP server for a Worlds project (stdio)"
)]
struct Cli {
    /// The project directory (holding project.json). Default: walk up from
    /// the working directory.
    #[arg(long, value_name = "DIR")]
    project: Option<PathBuf>,
    /// Never talk to the editor; refuse to start while it has the project open.
    #[arg(long)]
    headless: bool,
    /// Client name for the undo history when the MCP client sends none.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("WORLDS_MCP_LOG")
                .unwrap_or_else(|_| "worlds_mcp=info".into()),
        )
        .init();
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;
    let dir = worlds_mcp::project::find_project(cli.project.as_deref(), &cwd)
        .map_err(anyhow::Error::msg)?;
    match live_server(&dir) {
        Some(info) if cli.headless => anyhow::bail!(
            "the Worlds editor (pid {}) has {} open; --headless would make two writers. Close the editor or drop --headless",
            info.pid,
            dir.display()
        ),
        Some(info) => tracing::info!(
            "worlds-mcp: project {}, mode editor (pid {}, port {})",
            dir.display(),
            info.pid,
            info.port
        ),
        None => tracing::info!(
            "worlds-mcp: project {}, mode headless (no live editor; edits are saved to disk after every call)",
            dir.display()
        ),
    }
    let name = cli
        .name
        .as_deref()
        .and_then(worlds_mcp::engine::sanitize_name)
        .unwrap_or_else(|| "mcp".to_string());
    let server = WorldsMcp::start(Config {
        dir,
        force_headless: cli.headless,
        name,
    });
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let service = server.serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        anyhow::Ok(())
    })
}
