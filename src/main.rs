use anyhow::Result;
use clap::{Parser, Subcommand};
use maistro::{config::Config, engine::Engine};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    version,
    about = "Coding orchestration for AppFlowy, Shortcut, and Vaultite"
)]
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
    /// Check configuration, token file, repositories, selected agent login and MCP; no model calls.
    Validate,
    #[command(hide = true)]
    NeedsGithubToken,
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
    let load_engine = || Config::load(&cli.config).map(Engine::new);
    match cli.command {
        Action::Validate => {
            let engine = load_engine()?;
            engine.validate().await?;
            engine.validate_source_tools().await?;
            println!(
                "Configuration, private token files, repositories, agent login, source MCP settings, and local board tool availability are valid. Board access, token permissions, and model calls were not tested."
            );
        }
        Action::Run => {
            let engine = load_engine()?;
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
        Action::NeedsGithubToken => {
            // The installer only needs delivery settings, before paths and tools are ready.
            let config: Config = toml::from_str(&std::fs::read_to_string(&cli.config)?)?;
            println!("{}", config.requires_github_token());
        }
    }
    Ok(())
}
