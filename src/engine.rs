use crate::{
    codex::Codex,
    config::Config,
    git,
    github::Github,
    store::{Run, State, Store},
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use tokio_util::sync::CancellationToken;
pub struct Singleton(File);
impl Singleton {
    pub fn acquire(path: &std::path::Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        f.try_lock_exclusive()
            .context("another maistro daemon owns this state directory")?;
        Ok(Self(f))
    }
}
impl Drop for Singleton {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
#[derive(Clone)]
pub struct Engine {
    pub config: Config,
    pub store: Store,
    pub codex: Codex,
    pub github: Github,
}
impl Engine {
    pub fn new(config: Config) -> Result<Self> {
        let store = Store::open(&config.orchestrator.state_db)?;
        Ok(Self {
            codex: Codex::new(config.clone()),
            github: Github::new(&config.github.token_env)?,
            config,
            store,
        })
    }
    pub async fn flush(&self, cancel: &CancellationToken) -> Result<()> {
        for (id, projection, attempt) in self.store.pending()? {
            let Some(p) = self
                .config
                .projects
                .iter()
                .find(|p| p.id == projection.project_id)
            else {
                continue;
            };
            let Some(scan) = self.store.scan(&p.id)? else {
                continue;
            };
            match self
                .codex
                .project_update(p, &scan, &projection, cancel)
                .await
            {
                Ok(()) => self.store.ack(id)?,
                Err(e) => {
                    tracing::warn!(project=%p.id,error=%e,"AppFlowy projection deferred");
                    self.store.defer(id, attempt)?;
                }
            }
        }
        Ok(())
    }
    pub async fn poll(&self, cancel: &CancellationToken) -> Result<()> {
        for p in self.config.projects.iter().filter(|p| p.enabled) {
            match self.codex.scan(p, cancel).await {
                Ok(scan) => {
                    self.store
                        .ingest(p, &scan, &self.config.orchestrator.worktree_root)?;
                }
                Err(e) => {
                    tracing::warn!(project=%p.id,error=%e,"scan failed; no new work authorized")
                }
            }
        }
        Ok(())
    }
    pub async fn authorize(
        &self,
        p: &crate::config::Project,
        r: &Run,
        cancel: &CancellationToken,
    ) -> Result<()> {
        ensure!(
            self.store.control(&format!("cancel:{}", r.id))?.is_none(),
            "operator cancelled run"
        );
        let scan = self.codex.scan(p, cancel).await?;
        self.store.record_scan(&scan)?;
        crate::policy::authorized(
            r,
            scan.stories.iter().find(|s| s.story_id == r.story.story_id),
        )
    }
    pub fn repair(&self, r: &mut Run, message: &str) -> Result<bool> {
        r.last_error = Some(message.into());
        r.review = None;
        r.test_sha = None;
        r.worker_inflight = false;
        r.agent_activity = None;
        if r.attempt >= self.config.orchestrator.max_attempts {
            self.store.transition(
                r,
                State::Blocked,
                &format!("Repair budget exhausted: {message}"),
            )?;
            return Ok(false);
        }
        r.attempt += 1;
        self.store.transition(r, State::Running, message)?;
        Ok(true)
    }
    pub async fn execute(&self, mut r: Run, cancel: &CancellationToken) -> Result<()> {
        let p = self
            .config
            .projects
            .iter()
            .find(|p| p.id == r.project_id)
            .context("project removed")?;
        crate::process::reconcile_groups(&self.config.orchestrator.log_root.join(&r.id))?;
        if r.stranded_pid.take().is_some() || r.agent_activity.is_some() {
            let state = r.state;
            self.store.save(&mut r, state, "recovered run state")?;
        }
        loop {
            if cancel.is_cancelled() {
                return Ok(());
            }
            self.store.renew(&r.id)?;
            // Resolve a possibly committed remote merge before any new local work.
            if let Some(number) = r.pr {
                let pr = self.github.pr(p, number).await?;
                let mut remote_identity = r.clone();
                remote_identity.head_sha = pr["head"]["sha"].as_str().map(str::to_owned);
                crate::policy::pr_identity(p, &remote_identity, &pr)?;
                if pr["merged"] == true {
                    crate::policy::pr_identity(p, &r, &pr)?;
                    let sha = pr["merge_commit_sha"]
                        .as_str()
                        .context("merged PR has no commit")?;
                    git::confirm_merge(p, sha).await?;
                    r.merge_sha = Some(sha.into());
                    if r.state != State::Completed {
                        self.store.transition(
                            &mut r,
                            State::Completed,
                            "GitHub merge commit confirmed on base",
                        )?;
                    }
                    return Ok(());
                }
                ensure!(
                    pr["state"] == "open",
                    "linked PR was closed without merging"
                );
            }
            if r.state == State::Running {
                self.authorize(p, &r, cancel).await?;
                git::prepare(p, &mut r, &self.config.orchestrator.worktree_root).await?;
                self.store
                    .save(&mut r, State::Running, "worktree prepared")?;
                if r.worker_inflight
                    && !self.repair(
                        &mut r,
                        "Interrupted implementation; process sandbox has terminated",
                    )?
                {
                    return Ok(());
                }
                let existing = if r.head_sha.is_none() {
                    self.github.find_pr(p, &r).await?
                } else {
                    None
                };
                if let Some(pr) = existing {
                    r.pr = pr["number"].as_u64();
                    r.pr_url = pr["html_url"].as_str().map(str::to_owned);
                    r.head_sha = Some(git::git(&r.worktree, &["rev-parse", "HEAD"]).await?);
                    crate::policy::pr_identity(p, &r, &pr)?;
                } else {
                    r.worker_inflight = true;
                    r.agent_activity = Some(if r.attempt > 1 {
                        "repair agent".into()
                    } else {
                        "implementation agent".into()
                    });
                    self.store
                        .save(&mut r, State::Running, "implementation agent starting")?;
                    match self
                        .codex
                        .worker(p, &r, false, r.last_error.as_deref().unwrap_or(""), cancel)
                        .await
                    {
                        Ok((v, session)) => {
                            let report: crate::model::AgentReport = serde_json::from_value(v)?;
                            r.agent_sessions.push(session);
                            r.worker_inflight = false;
                            r.agent_activity = None;
                            if !report.uncertainties.is_empty() {
                                r.last_error = Some(format!(
                                    "Implementation uncertainties: {:?}",
                                    report.uncertainties
                                ));
                            }
                        }
                        Err(e) => {
                            if e.downcast_ref::<crate::process::UnconfirmedTermination>()
                                .is_some()
                            {
                                return Err(e);
                            }
                            if cancel.is_cancelled() {
                                r.worker_inflight = false;
                                r.agent_activity = None;
                                self.store.save(&mut r, State::Running, "agent cancelled")?;
                                return Ok(());
                            }
                            if self.repair(&mut r, &e.to_string())? {
                                continue;
                            }
                            return Ok(());
                        }
                    }
                }
                self.store.transition(
                    &mut r,
                    State::Reviewing,
                    "implementation ready; verification required",
                )?;
            }
            if r.state == State::Reviewing {
                git::verify_uncommitted_paths(p, &r).await?;
                if let Err(e) = git::commit(&mut r).await {
                    if e.downcast_ref::<crate::process::UnconfirmedTermination>()
                        .is_some()
                    {
                        return Err(e);
                    }
                    if self.repair(&mut r, &e.to_string())? {
                        continue;
                    }
                    return Ok(());
                }
                self.store.save(
                    &mut r,
                    State::Reviewing,
                    "implementation committed; verification required",
                )?;
                git::verify_paths(p, &r).await?;
                let results =
                    match crate::verification::tests(&self.config, p, &mut r, cancel).await {
                        Ok(s) => s,
                        Err(e) => {
                            if e.downcast_ref::<crate::process::UnconfirmedTermination>()
                                .is_some()
                            {
                                return Err(e);
                            }
                            if cancel.is_cancelled() {
                                return Ok(());
                            }
                            if self.repair(&mut r, &e.to_string())? {
                                continue;
                            }
                            return Ok(());
                        }
                    };
                r.agent_activity = Some("review agent".into());
                self.store
                    .save(&mut r, State::Reviewing, "review agent starting")?;
                let (v, session) = match self.codex.worker(p, &r, true, &results, cancel).await {
                    Ok(result) => result,
                    Err(error) => {
                        if error
                            .downcast_ref::<crate::process::UnconfirmedTermination>()
                            .is_some()
                        {
                            return Err(error);
                        }
                        r.agent_activity = None;
                        self.store
                            .save(&mut r, State::Reviewing, "review agent finished")?;
                        return Err(error);
                    }
                };
                r.agent_activity = None;
                ensure!(
                    !r.agent_sessions.contains(&session),
                    "reviewer reused an implementation session"
                );
                r.agent_sessions.push(session);
                r.review = Some(serde_json::from_value(v)?);
                if let Err(e) = crate::policy::review_valid(&r) {
                    let findings = serde_json::to_string(&r.review)?;
                    if self.repair(&mut r, &format!("{e}: {findings}"))? {
                        continue;
                    }
                    return Ok(());
                }
                self.store.save(
                    &mut r,
                    State::Reviewing,
                    "independent review and deterministic tests passed",
                )?;
                self.authorize(p, &r, cancel).await?;
                self.github.push(p, &r).await?;
                self.github.draft_pr(p, &mut r).await?;
                self.store.transition(
                    &mut r,
                    State::WaitingCi,
                    "Reviewed PR awaiting exact-head CI",
                )?;
                self.github
                    .mark_ready(p, r.pr.context("PR missing")?)
                    .await?;
            }
            if matches!(r.state, State::WaitingCi | State::Merging) {
                let number = r.pr.context("missing PR")?;
                // PR maintenance is independent of permission to merge it. Do
                // this before waiting on CI: a conflicting PR may have no CI run.
                // Never alter a branch while reconciling an uncertain merge.
                if r.state == State::WaitingCi
                    && !self.store.paused(&p.id)?
                    && self.store.control("reloading")?.as_deref() != Some("true")
                {
                    self.authorize(p, &r, cancel).await?;
                    crate::policy::pr_identity(p, &r, &self.github.pr(p, number).await?)?;
                    if let git::BaseUpdate::Updated { conflicts } =
                        git::update_base(p, &mut r).await?
                    {
                        if let Err(e) = git::verify_conflict_paths(p, &r, &conflicts) {
                            self.store.transition(
                                &mut r,
                                State::Blocked,
                                &format!("Base merge requires human handling: {e}"),
                            )?;
                            return Ok(());
                        }
                        if conflicts.is_empty() {
                            // Persist the review obligation before committing. Recovery
                            // finishes the merge (or records its already-created head)
                            // and repeats verification without invoking a coding agent.
                            r.last_error = None;
                            self.store.transition(
                                &mut r,
                                State::Reviewing,
                                "Base integrated cleanly; fresh tests and review required",
                            )?;
                            continue;
                        }
                        let feedback = format!(
                            "Base branch {} advanced to {}. Rust prepared a merge in /work. Conflicted paths: {}. Resolve each conflict in the working files, preserving the assigned story and upstream changes. Inspect modify/delete conflicts and retain relevant upstream content rather than blindly selecting a whole side. Remove all conflict markers; delete a file only when appropriate for the intended result. Do not run Git: Rust will stage and complete the pending merge, then rerun deterministic tests and fresh independent review before updating the same PR. Automatic merging of the PR remains governed separately by configuration.",
                            p.base_branch,
                            r.base_sha.as_deref().unwrap_or(""),
                            serde_json::to_string(&conflicts)?
                        );
                        if self.repair(&mut r, &feedback)? {
                            continue;
                        }
                        return Ok(());
                    }
                }
                self.github.mark_ready(p, number).await?;
                let required = if self.config.github.auto_merge {
                    self.github.protections(p).await?
                } else {
                    p.required_checks
                        .iter()
                        .map(|name| (name.clone(), None))
                        .collect()
                };
                match self
                    .github
                    .ci(p, r.head_sha.as_deref().context("head missing")?, &required)
                    .await?
                {
                    crate::policy::Ci::Pending => {
                        if crate::store::now() - r.updated_at
                            > self.config.orchestrator.ci_timeout_seconds as i64
                        {
                            self.store
                                .transition(&mut r, State::Blocked, "CI timed out")?;
                        }
                        return Ok(());
                    }
                    crate::policy::Ci::Failed(names) => {
                        ensure!(
                            r.state != State::Merging,
                            "CI changed during merge reconciliation"
                        );
                        if self.repair(&mut r, &format!("Required CI failed: {names:?}"))? {
                            continue;
                        }
                        return Ok(());
                    }
                    crate::policy::Ci::Passed => {}
                }
                if !self.config.github.auto_merge
                    || self.store.paused(&p.id)?
                    || self.store.control("reloading")?.as_deref() == Some("true")
                {
                    return Ok(());
                }
                self.authorize(p, &r, cancel).await?;
                crate::policy::review_valid(&r)?;
                git::verify_paths(p, &r).await?;
                let pr = self.github.pr(p, number).await?;
                crate::policy::pr_identity(p, &r, &pr)?;
                crate::policy::mergeable(&pr)?;
                ensure!(
                    pr["base"]["sha"].as_str() == r.base_sha.as_deref(),
                    "GitHub base SHA differs from reviewed base"
                );
                // Persist intent before the mutation; retries first inspect the remote PR above.
                if r.state != State::Merging {
                    self.store
                        .transition(&mut r, State::Merging, "All merge gates passed")?;
                }
                if self.store.paused(&p.id)?
                    || self.store.control("reloading")?.as_deref() == Some("true")
                    || self.store.control(&format!("cancel:{}", r.id))?.is_some()
                {
                    return Ok(());
                }
                let sha = self.github.squash(p, &r).await?;
                r.merge_sha = Some(sha.clone());
                self.store
                    .save(&mut r, State::Merging, "GitHub acknowledged squash merge")?;
                git::confirm_merge(p, &sha).await?;
                self.store.transition(
                    &mut r,
                    State::Completed,
                    "Merge commit confirmed; story Done",
                )?;
                return Ok(());
            }
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repair_budget_is_bounded() {
        let t = tempfile::tempdir().unwrap();
        let config = crate::fixtures::config(t.path());
        let store = Store::open(&config.orchestrator.state_db).unwrap();
        store.sync_config(&config).unwrap();
        let p = &config.projects[0];
        store
            .ingest(
                p,
                &crate::fixtures::scan(
                    p,
                    vec![crate::fixtures::story(crate::model::Status::Ready)],
                ),
                &config.orchestrator.worktree_root,
            )
            .unwrap();
        let mut r = store.runs().unwrap().remove(0);
        store.claim(&mut r, p, 1).unwrap();
        store
            .transition(&mut r, State::Reviewing, "review")
            .unwrap();
        let engine = Engine {
            codex: Codex::new(config.clone()),
            github: Github::fixture("http://127.0.0.1:1".into()),
            store,
            config,
        };
        assert!(engine.repair(&mut r, "review rejection").unwrap());
        engine
            .store
            .transition(&mut r, State::Reviewing, "review")
            .unwrap();
        assert!(engine.repair(&mut r, "CI failure").unwrap());
        engine
            .store
            .transition(&mut r, State::Reviewing, "review")
            .unwrap();
        assert!(!engine.repair(&mut r, "review rejection").unwrap());
        assert_eq!(r.state, State::Blocked);
        assert_eq!(r.attempt, 3);
    }
}
