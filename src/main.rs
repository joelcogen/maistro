use anyhow::Result;
use clap::{Parser, Subcommand};
use maistro::{config::Config, engine::Engine};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(version, about = "AppFlowy coordinator and per-project coding workers")]
struct Cli {
    #[arg(short, long, global=true, default_value_os_t=default_config())]
    config: PathBuf,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Run in the foreground. Use systemctl --user start/stop maistro for the service.
    Run,
    /// Check configuration, local repositories, Codex login and MCP settings; no model calls.
    Validate,
}
fn default_config() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config/maistro/config.toml")
}
#[tokio::main]
async fn main() -> Result<()> {
    unsafe {
        libc::umask(0o077);
    }
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
    let engine = Engine::new(Config::load(&cli.config)?);
    match cli.command {
        Action::Validate => {
            engine.validate().await?;
            println!(
                "Configuration, repositories, Codex login and AppFlowy MCP settings are valid. No remote access or model calls were tested."
            );
        }
        Action::Run => {
            let shutdown = CancellationToken::new();
            let token = shutdown.clone();
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            let signal = tokio::spawn(async move {
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
                token.cancel();
            });
            let result = engine.run(shutdown).await;
            signal.abort();
            result?;
        }
    }
    Ok(())
}
