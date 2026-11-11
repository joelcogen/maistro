use crate::{
    config::{Config, Project},
    git, process, sandbox,
    store::Run,
};
use anyhow::{Context, Result, ensure};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn excerpt(text: &str) -> String {
    const LIMIT: usize = 12_000;
    if text.len() <= LIMIT {
        return text.to_owned();
    }
    let mut start = text.len() - LIMIT;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[earlier output omitted]\n{}", &text[start..])
}
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
        let outcome = process::run(
            c,
            "",
            Duration::from_secs(config.orchestrator.process_timeout_seconds),
            cancel,
            Some(&dir.join(format!("{i}.stdout"))),
        )
        .await;
        let o = match outcome {
            Ok(output) => output,
            Err(error) => {
                if cancel.is_cancelled()
                    || error
                        .downcast_ref::<process::UnconfirmedTermination>()
                        .is_some()
                {
                    return Err(error);
                }
                let stdout =
                    std::fs::read_to_string(dir.join(format!("{i}.stdout"))).unwrap_or_default();
                let stderr =
                    std::fs::read_to_string(dir.join(format!("{i}.stderr"))).unwrap_or_default();
                anyhow::bail!(
                    "Test command {args:?} could not complete: {error}\nSTDOUT:\n{}\nSTDERR:\n{}",
                    excerpt(&stdout),
                    excerpt(&stderr)
                );
            }
        };
        ensure!(
            o.code == 0,
            "Deterministic test {} failed (exit {}). Command: {:?}. Workdir: /work. Network: isolated.\nSTDOUT:\n{}\nSTDERR:\n{}\nFull logs retained at {}",
            i,
            o.code,
            args,
            excerpt(&o.stdout),
            excerpt(&o.stderr),
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
