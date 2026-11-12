use crate::{
    codex::Codex,
    config::Config,
    model::{Assignment, Completion, Coordination, Report, Snapshot, StoryRef},
    poll, process,
    schedule::Schedule,
    status::{self, Status},
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use std::{
    collections::{BTreeMap, HashSet},
    fs::{File, OpenOptions},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::{process::Command, task::JoinSet, time::Instant};
use tokio_util::sync::CancellationToken;

/// OS locks prevent overlapping daemons. Their contents carry no workflow state.
pub struct Lock(File);
impl Lock {
    pub fn acquire(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.try_lock_exclusive()
            .with_context(|| format!("another maistro process holds {}", path.display()))?;
        Ok(Self(file))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

struct Active {
    story: StoryRef,
    cancel: CancellationToken,
}
#[derive(Default)]
struct State {
    active: BTreeMap<String, Active>,
    pending: BTreeMap<String, Completion>,
}
impl State {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            active: self.active.values().map(|a| a.story.clone()).collect(),
            pending: self.pending.values().cloned().collect(),
        }
    }
    fn cancel(&mut self, story: &StoryRef) {
        if let Some(a) = self
            .active
            .get(&story.project_id)
            .filter(|a| &a.story == story)
        {
            tracing::info!(project=%story.project_id, story=%story.story_id, "board revoked work; stopping worker");
            a.cancel.cancel();
        }
        // The worker may have finished while Luna was checking cancellation.
        if self
            .pending
            .get(&story.project_id)
            .is_some_and(|c| &c.story == story)
        {
            self.pending.remove(&story.project_id);
        }
    }
    fn finish(&mut self, assignment: Assignment, result: Result<Report>) -> Result<()> {
        let active = self
            .active
            .remove(&assignment.project_id)
            .context("worker has no active assignment")?;
        if let Err(e) = &result {
            // Never reuse a repository if we cannot confirm its previous process stopped.
            if e.downcast_ref::<process::UnconfirmedTermination>()
                .is_some()
            {
                return Err(result.unwrap_err());
            }
        }
        if active.cancel.is_cancelled() {
            return Ok(());
        }
        let report = result.unwrap_or_else(|e| {
            Report::blocked(format!(
                "Worker failed: {e:#}. Inspect existing work before retrying."
            ))
        });
        tracing::info!(project=%assignment.project_id, story=%assignment.story_id, outcome=?report.outcome, "worker finished; awaiting AppFlowy update");
        self.pending.insert(
            assignment.project_id,
            Completion {
                id: uuid::Uuid::new_v4().to_string(),
                story: active.story,
                report,
            },
        );
        Ok(())
    }
}

pub struct Engine {
    pub config: Config,
    pub codex: Codex,
}
impl Engine {
    pub fn new(config: Config) -> Self {
        Self {
            codex: Codex::new(config.clone()),
            config,
        }
    }
    pub async fn validate(&self) -> Result<()> {
        self.config.validate()?;
        self.codex.validate_cli().await?;
        let mut seen = HashSet::new();
        for p in self.config.projects.iter().filter(|p| p.enabled) {
            let root = git_output(&p.repo, &["rev-parse", "--show-toplevel"]).await?;
            ensure!(
                Path::new(&root).canonicalize()? == p.repo.canonicalize()?,
                "{}: repo must be the checkout root",
                p.id
            );
            let common = git_output(
                &p.repo,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .await?;
            ensure!(
                seen.insert(std::fs::canonicalize(common)?),
                "projects cannot share a Git repository, including linked worktrees"
            );
            git_output(&p.repo, &["check-ref-format", "--branch", &p.base_branch]).await?;
            git_output(&p.repo, &["remote", "get-url", &p.remote]).await?;
        }
        Ok(())
    }
    pub async fn run(&self, shutdown: CancellationToken) -> Result<()> {
        std::fs::create_dir_all(&self.config.orchestrator.runtime_dir)?;
        let _daemon = Lock::acquire(&self.config.orchestrator.runtime_dir.join("daemon.lock"))?;
        self.validate().await?;
        let mut repo_locks = vec![];
        for p in self.config.projects.iter().filter(|p| p.enabled) {
            let common = git_output(
                &p.repo,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .await?;
            repo_locks.push(Lock::acquire(&Path::new(&common).join("maistro.lock"))?);
        }
        let stop = shutdown.child_token();
        let mut diagnostics = Status::new(&self.config);
        let (published, receiver) = tokio::sync::watch::channel(Arc::new(diagnostics.snapshot()));
        let mut server = JoinSet::new();
        if self.config.status.enabled {
            let listener = status::bind(self.config.status.port).await?;
            tracing::info!(address=%listener.local_addr()?, "read-only status server listening");
            server.spawn(status::serve(listener, receiver, stop.child_token()));
        }
        let mut status_tick = tokio::time::interval(Duration::from_secs(1));
        status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut workers = JoinSet::new();
        let mut coordinator = JoinSet::new();
        let mut state = State::default();
        let interval = Duration::from_secs(self.config.orchestrator.poll_interval_seconds);
        let mut schedule = Schedule::new(
            self.config
                .projects
                .iter()
                .filter(|p| p.enabled)
                .map(|p| p.id.clone()),
        );
        let mut polls = JoinSet::new();
        let (poll_sender, mut poll_results) =
            tokio::sync::mpsc::channel(self.config.projects.len());
        let connection = self.codex.mcp_config()?;
        for project in self.config.projects.iter().filter(|p| p.enabled) {
            polls.spawn(poll::run(
                connection.clone(),
                project.clone(),
                interval,
                Duration::from_secs(self.config.orchestrator.coordinator_timeout_seconds),
                poll_sender.clone(),
                stop.child_token(),
            ));
        }
        drop(poll_sender);
        let mut pass = BTreeMap::new();
        // Briefly coalesce independent startup observations and simultaneous events.
        let mut coalesce_until = Instant::now() + Duration::from_millis(100);
        tracing::info!(projects = repo_locks.len(), "maistro started");
        let mut result: Result<()> = async {
            loop {
                if stop.is_cancelled() { break; }
                if coordinator.is_empty() && Instant::now() >= coalesce_until {
                    pass = schedule.ready();
                    if !pass.is_empty() {
                        diagnostics.coordinator_started();
                        let mut snapshot = state.snapshot();
                        snapshot.active.retain(|s| pass.contains_key(&s.project_id));
                        snapshot.pending.retain(|c| pass.contains_key(&c.story.project_id));
                        let selected: Vec<_> = pass.keys().cloned().collect();
                        let codex = self.codex.clone();
                        let cancel = stop.child_token();
                        coordinator.spawn(async move { codex.coordinate(&snapshot, &selected, &cancel).await });
                    }
                }
                for (id, project) in &schedule.projects { diagnostics.poll_dirty(id, project.dirty()); }
                let deadline = schedule.deadline().map(|d| d.max(coalesce_until));
                published.send_replace(Arc::new(diagnostics.snapshot()));
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => break,
                    outcome = server.join_next(), if !server.is_empty() => {
                        outcome.context("status server disappeared")???;
                        anyhow::bail!("status server stopped unexpectedly");
                    }
                    _ = status_tick.tick() => {},
                    outcome = coordinator.join_next(), if !coordinator.is_empty() => {
                        match outcome.context("coordinator disappeared")?? {
                            Ok(update) => {
                                diagnostics.coordinator_finished_for(Some(&update.observations), &pass.keys().cloned().collect::<Vec<_>>());
                                let released: Vec<_> = state.pending.values()
                                    .filter(|c| update.acknowledged.contains(&c.id) || update.cancel.contains(&c.story))
                                    .map(|c| c.story.project_id.clone()).collect();
                                for (id, generation) in &pass {
                                    if update.retry_projects.contains(id) { schedule.retry(id, interval); }
                                    else { schedule.acknowledge(id, *generation); }
                                }
                                self.apply(update, &mut state, &mut workers, &stop, &mut diagnostics)?;
                                for id in released { schedule.wake(&id); }
                                for id in state.pending.keys().filter(|id| pass.contains_key(*id)) {
                                    schedule.wake(id);
                                    schedule.retry(id, interval);
                                }
                                coalesce_until = Instant::now() + Duration::from_millis(100);
                            }
                            Err(e) => {
                                diagnostics.coordinator_finished_for(None, &pass.keys().cloned().collect::<Vec<_>>());
                                if e.downcast_ref::<process::UnconfirmedTermination>().is_some() { return Err(e); }
                                tracing::warn!(error=%format!("{e:#}"), "coordinator failed; retrying without releasing pending work");
                                for id in pass.keys() { schedule.retry(id, interval); }
                            }
                        }
                    }
                    outcome = workers.join_next(), if !workers.is_empty() => {
                        let (assignment, result) = outcome.context("worker disappeared")??;
                        let id = assignment.project_id.clone();
                        let failed = result.is_err();
                        state.finish(assignment, result)?;
                        diagnostics.worker_finished(&id, state.pending.get(&id).map(|c| c.report.outcome), failed);
                        schedule.wake(&id);
                        coalesce_until = Instant::now() + Duration::from_millis(100);
                    }
                    observation = poll_results.recv(), if !polls.is_empty() => {
                        if let Some((id, observation)) = observation {
                            match observation {
                                Ok(fingerprint) => {
                                    let changed = schedule.observe(&id, fingerprint);
                                    diagnostics.polled(&id, changed, false);
                                }
                                Err(e) => {
                                    diagnostics.polled(&id, false, true);
                                    tracing::warn!(project=%id, error=%format!("{e:#}"), "board poll failed; retaining last observation and retrying");
                                }
                            }
                        } else {
                            anyhow::bail!("board poll result channel closed unexpectedly");
                        }
                    }
                    outcome = polls.join_next(), if !polls.is_empty() => {
                        outcome.context("board poller disappeared")??;
                        anyhow::bail!("board poller stopped unexpectedly");
                    }
                    _ = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)), if coordinator.is_empty() && deadline.is_some() => {}
                }
            }
            Ok(())
        }.await;
        // Always stop and reap agents before dropping repository locks, including errors.
        stop.cancel();
        while let Some(outcome) = workers.join_next().await {
            record_shutdown(outcome.map(|(_, report)| report.map(|_| ())), &mut result);
        }
        while let Some(outcome) = coordinator.join_next().await {
            record_shutdown(outcome.map(|update| update.map(|_| ())), &mut result);
        }
        while let Some(outcome) = polls.join_next().await {
            if let Err(e) = outcome
                && result.is_ok()
            {
                result = Err(e.into());
            }
        }
        // Idle/slow HTTP clients must not delay daemon shutdown.
        server.shutdown().await;
        tracing::info!("maistro stopped; AppFlowy will drive recovery on next start");
        result
    }
    fn apply(
        &self,
        update: Coordination,
        state: &mut State,
        workers: &mut JoinSet<(Assignment, Result<Report>)>,
        stop: &CancellationToken,
        diagnostics: &mut Status,
    ) -> Result<()> {
        for note in update.notes {
            tracing::info!(note, "coordinator");
        }
        for story in update.cancel {
            state.cancel(&story);
            if state.active.contains_key(&story.project_id) {
                diagnostics.cancelling(&story.project_id);
            } else {
                diagnostics.released(&story.project_id, false);
            }
        }
        for c in state
            .pending
            .values()
            .filter(|c| update.acknowledged.contains(&c.id))
        {
            diagnostics.released(&c.story.project_id, true);
        }
        state
            .pending
            .retain(|_, c| !update.acknowledged.contains(&c.id));
        for assignment in update.assignments {
            ensure!(
                !state.snapshot().busy(&assignment.project_id),
                "attempted overlapping project work"
            );
            let project = self
                .config
                .projects
                .iter()
                .find(|p| p.id == assignment.project_id && p.enabled)
                .context("assignment refers to unknown project")?
                .clone();
            let cancel = stop.child_token();
            state.active.insert(
                project.id.clone(),
                Active {
                    story: (&assignment).into(),
                    cancel: cancel.clone(),
                },
            );
            diagnostics.worker_started(&assignment);
            tracing::info!(project=%project.id, story=%assignment.story_id, recovery=assignment.recovery, "dispatching story");
            let codex = self.codex.clone();
            workers.spawn(async move {
                let result = codex.work(&project, &assignment, &cancel).await;
                (assignment, result)
            });
        }
        Ok(())
    }
}

fn record_shutdown(outcome: Result<Result<()>, tokio::task::JoinError>, result: &mut Result<()>) {
    let error = match outcome {
        Err(e) => Some(anyhow::Error::from(e)),
        Ok(Err(e))
            if e.downcast_ref::<process::UnconfirmedTermination>()
                .is_some() =>
        {
            Some(e)
        }
        _ => None, // Ordinary cancellation is expected during shutdown.
    };
    if let Some(e) = error {
        tracing::error!(error=%e, "agent shutdown failed");
        if result.is_ok() {
            *result = Err(e);
        }
    }
}

async fn git_output(repo: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo).args(args);
    process::checked(cmd, Duration::from_secs(15)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active() -> (State, Assignment) {
        let a = Assignment {
            project_id: "p".into(),
            story_id: "s".into(),
            title: "Story".into(),
            story_url: None,
            context: "Requirements".into(),
            recovery: false,
        };
        let mut state = State::default();
        state.active.insert(
            a.project_id.clone(),
            Active {
                story: (&a).into(),
                cancel: CancellationToken::new(),
            },
        );
        (state, a)
    }

    #[test]
    fn cancellation_arriving_after_worker_completion_discards_the_pending_result() {
        let (mut state, a) = active();
        state
            .finish(a.clone(), Ok(Report::blocked("An outcome".into())))
            .unwrap();
        assert!(state.snapshot().busy("p"));
        state.cancel(&StoryRef::from(&a));
        assert!(!state.snapshot().busy("p"));
    }

    #[test]
    fn cancellation_keeps_slot_until_process_stops_and_does_not_report_blocked() {
        let (mut state, a) = active();
        state.cancel(&StoryRef::from(&a));
        assert!(state.snapshot().busy("p"));
        assert!(state.active["p"].cancel.is_cancelled());
        state
            .finish(a, Err(anyhow::anyhow!("subprocess cancelled")))
            .unwrap();
        assert!(!state.snapshot().busy("p"));
    }
}
