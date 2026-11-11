use crate::{
    config::{Config, Project},
    engine::{Engine, Singleton},
    store::{Run, State, backoff, now},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
/// Round-robin projects; durable run ordering supplies descending priority within each project.
pub fn fair_candidates(config: &Config, runs: &[Run], cursor: usize) -> Vec<Run> {
    let n = config.projects.len();
    let mut result = vec![];
    for i in 0..n {
        let p = &config.projects[(cursor + i) % n];
        if !p.enabled {
            continue;
        }
        if let Some(r) = runs
            .iter()
            .filter(|r| r.project_id == p.id && r.state.active() && r.retry_after <= now())
            .min_by_key(|r| r.state == State::Queued)
        {
            result.push(r.clone());
        }
    }
    result
}
async fn repo_lock(p: &Project) -> Result<Singleton> {
    let d = crate::git::git(&p.repo, &["rev-parse", "--git-common-dir"]).await?;
    let path = if Path::new(&d).is_absolute() {
        d.into()
    } else {
        p.repo.join(d)
    };
    Singleton::acquire(&path.join("maistro.lock"))
}
impl Engine {
    pub async fn run_daemon(mut self, path: &Path, once: bool) -> Result<()> {
        let _singleton =
            Singleton::acquire(&self.config.orchestrator.state_db.with_extension("lock"))?;
        self.store.sync_startup_config(&self.config)?;
        self.preflight().await?;
        self.store.reset_leases()?;
        let shutdown = CancellationToken::new();
        let signal = shutdown.clone();
        tokio::spawn(async move {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
            signal.cancel();
        });
        let mut jobs: JoinSet<(String, Result<()>)> = JoinSet::new();
        let mut polls: JoinSet<(String, Result<crate::model::Scan>)> = JoinSet::new();
        let mut projections: JoinSet<Result<()>> = JoinSet::new();
        let mut active: HashMap<String, CancellationToken> = HashMap::new();
        let mut polling = HashSet::new();
        let mut next_poll: HashMap<String, i64> = HashMap::new();
        let mut poll_errors: HashMap<String, u32> = HashMap::new();
        let mut locks = HashMap::new();
        let mut cursor = self
            .store
            .control("cursor")?
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let mut scanned = HashSet::new();
        let mut stopping = false;
        loop {
            if shutdown.is_cancelled() {
                stopping = true;
                for token in active.values() {
                    token.cancel();
                }
                polls.abort_all();
                projections.abort_all();
            }
            while let Some(result) = jobs.try_join_next() {
                let (id, result) = result.context("worker task panicked")?;
                active.remove(&id);
                self.store.release(&id)?;
                let mut r = self.store.get(&id)?;
                if let Err(e) = result {
                    r.last_error = Some(e.to_string());
                    r.failures += 1;
                    r.retry_after = now() + backoff(r.failures);
                    let state = r.state;
                    if let Some(unknown) =
                        e.downcast_ref::<crate::process::UnconfirmedTermination>()
                    {
                        r.stranded_pid = Some(unknown.pid);
                        self.store.save(&mut r, state, &e.to_string())?;
                        continue;
                    }
                    if !stopping
                        && r.failures >= self.config.orchestrator.max_attempts
                        && state != State::Merging
                    {
                        self.store.transition(
                            &mut r,
                            State::Blocked,
                            &format!("Operational retry budget exhausted: {e}"),
                        )?;
                    } else {
                        self.store
                            .save(&mut r, state, &format!("Deferred after error: {e}"))?;
                    }
                } else if matches!(r.state, State::WaitingCi | State::Merging) {
                    r.retry_after = now() + self.config.orchestrator.poll_interval_seconds as i64;
                    let state = r.state;
                    self.store
                        .save(&mut r, state, "waiting for external evidence")?;
                }
            }
            while let Some(result) = polls.try_join_next() {
                if stopping && result.as_ref().is_err_and(|e| e.is_cancelled()) {
                    continue;
                }
                let (id, result) = result.context("poll task panicked")?;
                polling.remove(&id);
                scanned.insert(id.clone());
                match result {
                    Ok(scan) => {
                        let p = self
                            .config
                            .projects
                            .iter()
                            .find(|p| p.id == id)
                            .context("project changed during scan")?;
                        self.store
                            .ingest(p, &scan, &self.config.orchestrator.worktree_root)?;
                        poll_errors.remove(&id);
                        next_poll.insert(
                            id,
                            now() + self.config.orchestrator.poll_interval_seconds as i64,
                        );
                    }
                    Err(e) => {
                        let errors = poll_errors.entry(id.clone()).or_default();
                        *errors += 1;
                        next_poll.insert(id.clone(), now() + backoff(*errors));
                        tracing::warn!(project=%id,error=%e,"poll failed; dispatch held until a fresh scan");
                        self.store
                            .set_control(&format!("scan_error:{id}"), &e.to_string())?;
                    }
                }
            }
            while let Some(result) = projections.try_join_next() {
                if stopping && result.as_ref().is_err_and(|e| e.is_cancelled()) {
                    continue;
                }
                if let Err(e) = result.context("projection task panicked")? {
                    tracing::warn!(error=%e,"projection reconciliation failed");
                }
            }
            if stopping {
                if active.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            let reload = self.store.control("reload")?.is_some();
            self.store
                .set_control("reloading", if reload { "true" } else { "false" })?;
            if reload && active.is_empty() && polling.is_empty() && projections.is_empty() {
                let candidate = (|| -> Result<Engine> {
                    let config = Config::load(path)?;
                    ensure!(
                        config.orchestrator.state_db == self.config.orchestrator.state_db
                            && config.orchestrator.worktree_root
                                == self.config.orchestrator.worktree_root
                            && config.orchestrator.log_root == self.config.orchestrator.log_root,
                        "runtime roots cannot change on reload"
                    );
                    Ok(Engine {
                        codex: crate::codex::Codex::new(config.clone()),
                        github: crate::github::Github::new(&config.github.token_env)?,
                        store: self.store.clone(),
                        config,
                    })
                })();
                match candidate {
                    Ok(new) => match new.preflight().await {
                        Ok(()) => {
                            if let Err(e) = self.store.sync_config(&new.config) {
                                tracing::error!(error=%e,"reload identity check failed");
                                self.store.clear_control("reload")?;
                                self.store.set_control("reloading", "false")?;
                                continue;
                            }
                            self = new;
                            next_poll.clear();
                            scanned.clear();
                            poll_errors.clear();
                        }
                        Err(e) => {
                            tracing::error!(error=%e,"reload validation failed; retained running configuration")
                        }
                    },
                    Err(e) => {
                        tracing::error!(error=%e,"reload rejected; retained running configuration")
                    }
                };
                self.store.clear_control("reload")?;
                self.store.set_control("reloading", "false")?;
            }
            for p in self.config.projects.iter().filter(|p| p.enabled) {
                if !reload
                    && !polling.contains(&p.id)
                    && now() >= *next_poll.get(&p.id).unwrap_or(&0)
                    && (!once || !scanned.contains(&p.id))
                {
                    let codex = self.codex.clone();
                    let p = p.clone();
                    let token = shutdown.child_token();
                    polling.insert(p.id.clone());
                    polls.spawn(async move {
                        let result = codex.scan(&p, &token).await;
                        (p.id, result)
                    });
                }
            }
            for id in active.keys() {
                self.store.renew(id)?;
            }
            let runs = self.store.runs()?;
            for mut r in runs.iter().filter(|r| r.state.active()).cloned() {
                let missing = self
                    .config
                    .projects
                    .iter()
                    .all(|p| p.id != r.project_id || !p.enabled);
                let manual = self.store.control(&format!("cancel:{}", r.id))?.is_some();
                let scan = self.store.scan(&r.project_id)?;
                let invalid = scan.as_ref().is_some_and(|s| {
                    crate::policy::authorized(
                        &r,
                        s.stories.iter().find(|s| s.story_id == r.story.story_id),
                    )
                    .is_err()
                });
                if missing || manual || invalid {
                    if let Some(token) = active.get(&r.id) {
                        token.cancel();
                    } else if r.state != State::Merging {
                        self.store.transition(
                            &mut r,
                            State::Cancelled,
                            "Story changed, removed, cancelled, or project disabled",
                        )?;
                    }
                }
            }
            // Retry is an explicit operator action and requires current Ready authorization.
            for mut r in self.store.runs()? {
                if self.store.control(&format!("retry:{}", r.id))?.is_some()
                    && !active.contains_key(&r.id)
                {
                    let scan = self
                        .store
                        .scan(&r.project_id)?
                        .context("retry needs a successful board scan")?;
                    let story = scan
                        .stories
                        .iter()
                        .find(|s| s.story_id == r.story.story_id)
                        .context("story missing")?;
                    if story.status == crate::model::Status::Ready
                        && story.revision() == r.revision
                        && matches!(r.state, State::Blocked | State::Failed | State::Cancelled)
                    {
                        r.attempt = 0;
                        r.failures = 0;
                        r.retry_after = 0;
                        r.worker_inflight = false;
                        self.store
                            .transition(&mut r, State::Queued, "Explicit operator retry")?;
                        self.store.clear_control(&format!("cancel:{}", r.id))?;
                    } else {
                        tracing::warn!(run=%r.id,"retry requires unchanged Ready story and terminal run");
                    }
                    self.store.clear_control(&format!("retry:{}", r.id))?;
                }
            }
            let runs = self.store.runs()?;
            if !self.config.orchestrator.retain_worktrees {
                for r in runs
                    .iter()
                    .filter(|r| r.state == State::Completed && r.worktree.exists())
                {
                    if let Some(p) = self.config.projects.iter().find(|p| p.id == r.project_id)
                        && let Err(e) =
                            crate::git::cleanup(p, r, &self.config.orchestrator.worktree_root).await
                    {
                        tracing::warn!(run=%r.id,error=%e,"completed worktree retained");
                    }
                }
            }
            locks.retain(|id, _| {
                runs.iter()
                    .any(|r| &r.project_id == id && r.state.active() && r.state != State::Queued)
            });
            if !reload {
                for mut r in fair_candidates(&self.config, &runs, cursor) {
                    if active.len() >= self.config.orchestrator.max_concurrent_workers {
                        break;
                    }
                    if active.contains_key(&r.id) || self.store.paused(&r.project_id)? {
                        continue;
                    }
                    if active.keys().any(|id| {
                        runs.iter()
                            .any(|x| &x.id == id && x.project_id == r.project_id)
                    }) {
                        continue;
                    }
                    if poll_errors.contains_key(&r.project_id) || !scanned.contains(&r.project_id) {
                        continue;
                    }
                    let p = self
                        .config
                        .projects
                        .iter()
                        .find(|p| p.id == r.project_id)
                        .unwrap();
                    if !locks.contains_key(&p.id) {
                        match repo_lock(p).await {
                            Ok(lock) => {
                                locks.insert(p.id.clone(), lock);
                            }
                            Err(e) => {
                                tracing::warn!(project=%p.id,error=%e,"repository locked by another daemon");
                                continue;
                            }
                        }
                    }
                    if r.state == State::Queued {
                        if !self.store.claim(
                            &mut r,
                            p,
                            self.config.orchestrator.max_concurrent_workers,
                        )? {
                            continue;
                        }
                    } else {
                        self.store.acquire_recovery(&r, p)?;
                    }
                    let dispatched_project = r.project_id.clone();
                    let engine = self.clone();
                    let token = shutdown.child_token();
                    active.insert(r.id.clone(), token.clone());
                    jobs.spawn(async move {
                        let id = r.id.clone();
                        let result = engine.execute(r, &token).await;
                        (id, result)
                    });
                    cursor = (self
                        .config
                        .projects
                        .iter()
                        .position(|p| p.id == dispatched_project)
                        .unwrap_or(0)
                        + 1)
                        % self.config.projects.len();
                    self.store.set_control("cursor", &cursor.to_string())?;
                }
            }
            if projections.is_empty() && !self.store.pending()?.is_empty() {
                let engine = self.clone();
                let token = shutdown.child_token();
                projections.spawn(async move { engine.flush(&token).await });
            }
            if once
                && polling.is_empty()
                && self
                    .config
                    .projects
                    .iter()
                    .filter(|p| p.enabled)
                    .all(|p| scanned.contains(&p.id))
                && active.is_empty()
                && projections.is_empty()
            {
                break;
            }
            tokio::select! {_=shutdown.cancelled()=>{},_=tokio::time::sleep(Duration::from_millis(200))=>{}}
        }
        self.store.set_control("reloading", "false")?;
        Ok(())
    }
    pub async fn preflight(&self) -> Result<()> {
        self.config.validate()?;
        self.codex.validate_cli().await?;
        for p in self.config.projects.iter().filter(|p| p.enabled) {
            crate::git::validate(p).await?;
            ensure!(
                !p.test_commands.is_empty(),
                "{}: deterministic test_commands required",
                p.id
            );
            self.github
                .validate(&p.github_repo, self.config.github.auto_merge)
                .await?;
            if self.config.github.auto_merge {
                self.github.protections(p).await?;
            }
            let mut probe = crate::sandbox::command(p, &p.repo, false, false)?;
            probe.args(["--", "/bin/true"]);
            crate::process::checked(probe, Duration::from_secs(10))
                .await
                .context("worker isolation unavailable")?;
            if p.test_commands.iter().any(|args| {
                args.first().is_some_and(|s| {
                    Path::new(s)
                        .file_name()
                        .is_some_and(|name| name == "cargo" || name == "rustc")
                })
            }) {
                let mut compiler = crate::sandbox::command(p, &p.repo, false, false)?;
                compiler.args(["--", "rustc", "-", "-o", "/tmp/maistro-toolchain-probe"]);
                let output = crate::process::run(
                    compiler,
                    "fn main() {}",
                    Duration::from_secs(30),
                    &CancellationToken::new(),
                    None,
                )
                .await
                .context("Rust compiler preflight failed")?;
                ensure!(
                    output.code == 0,
                    "{}: Rust/linker unavailable in test sandbox: {}",
                    p.id,
                    output.stderr
                );
            }
        }
        ensure!(
            self.config.agents.codex_home.join("auth.json").is_file(),
            "isolated workers require Codex file authentication"
        );
        let dir = self.config.orchestrator.log_root.join("validation");
        std::fs::create_dir_all(&dir)?;
        let mut models = vec![
            &self.config.agents.developer_model,
            &self.config.agents.reviewer_model,
        ];
        models.sort();
        models.dedup();
        for model in models {
            let(v,_)=self.codex.invoke(crate::codex::Invocation{model,cwd:&dir,prompt:"Read-only model availability probe. Do not call tools. Return summary Model available and no uncertainties.",kind:"report",mcp:None,scope:None,log_dir:&dir},&CancellationToken::new()).await?;
            let _: crate::model::AgentReport = serde_json::from_value(v)?;
            tracing::info!(model=%model,"model availability verified");
        }
        Ok(())
    }
}
