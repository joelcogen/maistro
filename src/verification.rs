use crate::{
    config::{Config, Project},
    git, process, sandbox,
    store::Run,
};
use anyhow::{Context, Result, ensure};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
pub async fn tests(
    config: &Config,
    p: &Project,
    r: &mut Run,
    cancel: &CancellationToken,
) -> Result<String> {
    ensure!(
        !p.test_commands.is_empty(),
        "configure deterministic test_commands before unattended execution"
    );
    let head = r.head_sha.clone().context("missing head")?;
    let dir = config
        .orchestrator
        .log_root
        .join(&r.id)
        .join(format!("tests-{}", r.attempt));
    std::fs::create_dir_all(&dir)?;
    let mut summary = String::new();
    for (i, args) in p.test_commands.iter().enumerate() {
        let mut c = sandbox::command(p, &r.worktree, true, false)?;
        c.arg("--").args(args);
        let o = process::run(
            c,
            "",
            Duration::from_secs(config.orchestrator.process_timeout_seconds),
            cancel,
            Some(&dir.join(format!("{i}.stdout"))),
        )
        .await?;
        ensure!(
            o.code == 0,
            "deterministic test {} failed (exit {}); inspect {}",
            i,
            o.code,
            dir.display()
        );
        summary.push_str(&format!("Command {args:?}: exit 0\n"));
    }
    ensure!(
        git::git(&r.worktree, &["rev-parse", "HEAD"]).await? == head,
        "test command changed HEAD"
    );
    ensure!(
        git::git(&r.worktree, &["status", "--porcelain"])
            .await?
            .is_empty(),
        "test commands modified source or created unignored artifacts; verification invalid"
    );
    r.test_sha = Some(head);
    Ok(summary)
}
