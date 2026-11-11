use crate::{config::Project, process};
use anyhow::{Context, Result, ensure};
use std::{path::Path, time::Duration};
use tokio::process::Command;
pub async fn git(repo: &Path, args: &[&str]) -> Result<String> {
    fn base(repo: &Path) -> Command {
        let mut c = Command::new("git");
        c.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", std::env::var_os("HOME").unwrap_or_default())
            .env("GIT_TERMINAL_PROMPT", "0");
        if let Some(sock) = std::env::var_os("SSH_AUTH_SOCK") {
            c.env("SSH_AUTH_SOCK", sock);
        }
        c.arg("-C").arg(repo).args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ]);
        c
    }
    let mut discover = base(repo);
    discover.args(["config", "--name-only", "--get-regexp", "^filter\\."]);
    let output = process::run(
        discover,
        "",
        Duration::from_secs(10),
        &tokio_util::sync::CancellationToken::new(),
        None,
    )
    .await?;
    ensure!(
        output.code == 0 || output.code == 1,
        "unable to inspect Git filter configuration"
    );
    let mut c = base(repo);
    let mut filters = std::collections::HashSet::new();
    for key in output.stdout.lines() {
        if let Some((prefix, _)) = key.rsplit_once('.') {
            filters.insert(prefix.to_owned());
        }
    }
    for filter in filters {
        for suffix in ["clean", "smudge", "process"] {
            c.args(["-c", &format!("{filter}.{suffix}=")]);
        }
        c.args(["-c", &format!("{filter}.required=false")]);
    }
    c.args(args);
    process::checked(c, Duration::from_secs(120)).await
}
pub async fn validate(p: &Project) -> Result<()> {
    let root = git(&p.repo, &["rev-parse", "--show-toplevel"]).await?;
    ensure!(
        Path::new(&root).canonicalize()? == p.repo.canonicalize()?,
        "repo path must be Git root"
    );
    ensure!(
        git(&p.repo, &["status", "--porcelain"]).await?.is_empty(),
        "{}: repository has local changes",
        p.id
    );
    git(&p.repo, &["check-ref-format", "--branch", &p.base_branch]).await?;
    git(
        &p.repo,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", p.base_branch),
        ],
    )
    .await?;
    let remote = git(&p.repo, &["remote", "get-url", "origin"]).await?;
    let expected = format!("https://github.com/{}", p.github_repo);
    let ssh = format!("git@github.com:{}", p.github_repo);
    ensure!(
        [
            expected.clone(),
            format!("{expected}.git"),
            ssh.clone(),
            format!("{ssh}.git")
        ]
        .contains(&remote),
        "origin does not match configured GitHub repository"
    );
    Ok(())
}

pub async fn prepare(p: &Project, r: &mut crate::store::Run, root: &Path) -> Result<()> {
    std::fs::create_dir_all(root)?;
    let root = root.canonicalize()?;
    ensure!(
        r.worktree == root.join(&r.id),
        "unexpected worktree location"
    );
    let branch_ref = format!("refs/heads/{}", r.branch);
    if r.worktree.exists() {
        let head = git(&r.worktree, &["symbolic-ref", "--short", "HEAD"]).await?;
        ensure!(head == r.branch, "worktree branch identity mismatch");
        if r.base_sha.is_none() {
            r.base_sha = Some(
                git(
                    &r.worktree,
                    &[
                        "merge-base",
                        "HEAD",
                        &format!("refs/remotes/origin/{}", p.base_branch),
                    ],
                )
                .await?,
            );
        }
        return Ok(());
    }
    git(&p.repo, &["fetch", "--no-tags", "origin", &p.base_branch]).await?;
    let base = git(
        &p.repo,
        &[
            "rev-parse",
            &format!("refs/remotes/origin/{}", p.base_branch),
        ],
    )
    .await?;
    if git(&p.repo, &["show-ref", "--verify", &branch_ref])
        .await
        .is_ok()
    {
        git(
            &p.repo,
            &[
                "worktree",
                "add",
                r.worktree.to_str().context("non UTF-8 worktree")?,
                &r.branch,
            ],
        )
        .await?;
    } else {
        git(
            &p.repo,
            &[
                "worktree",
                "add",
                "-b",
                &r.branch,
                r.worktree.to_str().context("non UTF-8 worktree")?,
                &base,
            ],
        )
        .await?;
    }
    r.base_sha = Some(base);
    Ok(())
}
pub async fn commit(r: &mut crate::store::Run) -> Result<()> {
    let pending = merge_head(&r.worktree).await?;
    if let Some(base) = &pending {
        ensure!(
            Some(base) == r.base_sha.as_ref(),
            "pending merge targets an unrecorded base"
        );
        for path in conflict_paths(&r.worktree).await? {
            let file = r.worktree.join(&path);
            if file.exists() {
                let text = std::fs::read_to_string(&file)
                    .with_context(|| format!("conflicted file {path} is not readable text"))?;
                ensure!(
                    !text
                        .lines()
                        .any(|line| line.starts_with("<<<<<<<") || line.starts_with(">>>>>>>")),
                    "Unresolved merge conflict markers in {path}; resolve the contents before verification"
                );
            }
        }
    }
    if pending.is_some()
        || !git(&r.worktree, &["status", "--porcelain"])
            .await?
            .is_empty()
    {
        git(&r.worktree, &["add", "--all", "--", "."]).await?;
        git(
            &r.worktree,
            &[
                "-c",
                "user.name=Maistro",
                "-c",
                "user.email=maistro@localhost",
                "commit",
                "-m",
                &format!("[maistro] {} ({})", r.story.title, r.id),
            ],
        )
        .await?;
    }
    let head = git(&r.worktree, &["rev-parse", "HEAD"]).await?;
    ensure!(
        Some(&head) != r.base_sha.as_ref(),
        "implementation produced no changes"
    );
    r.head_sha = Some(head);
    Ok(())
}
pub async fn diff(r: &crate::store::Run) -> Result<String> {
    git(
        &r.worktree,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            r.base_sha.as_deref().context("missing base SHA")?,
            "HEAD",
            "--",
        ],
    )
    .await
}

pub async fn verify_paths(p: &Project, r: &crate::store::Run) -> Result<()> {
    let base = r.base_sha.as_deref().context("base SHA missing")?;
    // --no-renames ensures both sides of a rename are checked, including deletion of protected files.
    let files = git(
        &r.worktree,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            base,
            "HEAD",
            "--",
        ],
    )
    .await?;
    ensure!(!files.is_empty(), "empty implementation diff");
    for path in files.split('\0').filter(|s| !s.is_empty()) {
        crate::policy::changed_path_allowed(p, path)?;
    }
    // Symlinks and submodules can redirect tools outside reviewed source; require human handling.
    let raw = git(
        &r.worktree,
        &["diff", "--raw", "--no-renames", base, "HEAD", "--"],
    )
    .await?;
    ensure!(
        !raw.lines().any(|l| l.starts_with(":120000")
            || l.starts_with(":160000")
            || l.split_whitespace()
                .nth(1)
                .is_some_and(|s| s == "120000" || s == "160000")),
        "symlink/submodule changes require human intervention"
    );
    Ok(())
}
pub async fn current_base(p: &Project) -> Result<String> {
    git(&p.repo, &["fetch", "--no-tags", "origin", &p.base_branch]).await?;
    git(
        &p.repo,
        &[
            "rev-parse",
            &format!("refs/remotes/origin/{}", p.base_branch),
        ],
    )
    .await
}
#[derive(Debug, PartialEq, Eq)]
pub enum BaseUpdate {
    Unchanged,
    Updated { conflicts: Vec<String> },
}
async fn merge_head(worktree: &Path) -> Result<Option<String>> {
    let name = git(worktree, &["rev-parse", "--git-path", "MERGE_HEAD"]).await?;
    let file = worktree.join(name);
    match std::fs::read_to_string(file) {
        Ok(text) => {
            let sha = text.trim();
            ensure!(
                sha.len() == 40 && sha.bytes().all(|c| c.is_ascii_hexdigit()),
                "invalid pending merge head"
            );
            Ok(Some(sha.to_owned()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
async fn conflict_paths(worktree: &Path) -> Result<Vec<String>> {
    let files = git(worktree, &["diff", "--name-only", "--diff-filter=U", "-z"]).await?;
    let mut paths = files
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Ok(paths)
}
pub fn verify_conflict_paths(p: &Project, r: &crate::store::Run, paths: &[String]) -> Result<()> {
    for path in paths {
        crate::policy::changed_path_allowed(p, path)?;
        let file = r.worktree.join(path);
        match std::fs::symlink_metadata(&file) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file() && metadata.len() <= 16 * 1024 * 1024,
                    "conflict in {path} requires human handling (non-regular or oversized file)"
                );
                let bytes = std::fs::read(file)?;
                ensure!(
                    !bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok(),
                    "binary conflict in {path} requires human handling"
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub async fn update_base(p: &Project, r: &mut crate::store::Run) -> Result<BaseUpdate> {
    // MERGE_HEAD survives interruption even if SQLite has not recorded the new
    // base yet. Finish that exact merge before considering a later base update.
    let pending = merge_head(&r.worktree).await?;
    let base = match &pending {
        Some(sha) => sha.clone(),
        None => current_base(p).await?,
    };
    if pending.is_none() && r.base_sha.as_ref() == Some(&base) {
        return Ok(BaseUpdate::Unchanged);
    }
    if pending.is_none() {
        ensure!(
            git(&r.worktree, &["status", "--porcelain"])
                .await?
                .is_empty(),
            "cannot update base over uncommitted work"
        );
        let result = git(
            &r.worktree,
            &[
                "-c",
                "user.name=Maistro",
                "-c",
                "user.email=maistro@localhost",
                "merge",
                "--no-commit",
                "--no-ff",
                &base,
            ],
        )
        .await;
        if let Err(error) = result {
            if error
                .downcast_ref::<process::UnconfirmedTermination>()
                .is_some()
            {
                return Err(error);
            }
            // Git exits nonzero for an ordinary content conflict. Preserve it
            // for the repair worker instead of treating it as a failed fetch.
            if merge_head(&r.worktree).await?.as_ref() != Some(&base)
                || conflict_paths(&r.worktree).await?.is_empty()
            {
                return Err(error);
            }
        }
    }
    let conflicts = conflict_paths(&r.worktree).await?;
    r.base_sha = Some(base);
    r.head_sha = Some(git(&r.worktree, &["rev-parse", "HEAD"]).await?);
    r.review = None;
    r.test_sha = None;
    Ok(BaseUpdate::Updated { conflicts })
}
pub async fn confirm_merge(p: &Project, sha: &str) -> Result<()> {
    ensure!(
        sha.len() == 40 && sha.bytes().all(|c| c.is_ascii_hexdigit()),
        "invalid merge commit SHA"
    );
    let base = current_base(p).await?;
    git(&p.repo, &["merge-base", "--is-ancestor", sha, &base]).await?;
    Ok(())
}

pub async fn verify_uncommitted_paths(p: &Project, r: &crate::store::Run) -> Result<()> {
    // During base integration, upstream edits are trusted base content. Check
    // the task's result against that base rather than misclassifying upstream
    // changes to protected files as worker edits.
    let reference = if merge_head(&r.worktree).await?.is_some() {
        r.base_sha
            .as_deref()
            .context("missing pending merge base")?
    } else {
        "HEAD"
    };
    for args in [
        vec!["diff", "--name-only", "--no-renames", "-z", reference, "--"],
        vec!["ls-files", "--others", "--exclude-standard", "-z"],
    ] {
        let paths = git(&r.worktree, &args).await?;
        for path in paths.split('\0').filter(|s| !s.is_empty()) {
            crate::policy::changed_path_allowed(p, path)?;
        }
    }
    Ok(())
}

pub async fn cleanup(p: &Project, r: &crate::store::Run, root: &Path) -> Result<()> {
    ensure!(
        r.state == crate::store::State::Completed,
        "only durable completed worktrees can be reclaimed"
    );
    if !r.worktree.exists() {
        return Ok(());
    }
    ensure!(
        r.worktree.canonicalize()? == root.canonicalize()?.join(&r.id),
        "cleanup path does not match owned run"
    );
    ensure!(
        git(&r.worktree, &["symbolic-ref", "--short", "HEAD"]).await? == r.branch,
        "cleanup branch mismatch"
    );
    ensure!(
        git(&r.worktree, &["status", "--porcelain"])
            .await?
            .is_empty(),
        "retaining dirty worktree for investigation"
    );
    git(
        &p.repo,
        &[
            "worktree",
            "remove",
            r.worktree.to_str().context("non UTF-8 path")?,
        ],
    )
    .await?;
    Ok(())
}
