//! Small synchronous SQLite transactions; never hold a transaction across await.
use crate::{
    config::{Config, Project},
    model::{Decision, Projection, Review, Scan, Status, Story},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Running,
    Reviewing,
    WaitingCi,
    Merging,
    Completed,
    Blocked,
    Failed,
    Cancelled,
}
impl State {
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Running | Self::Reviewing | Self::WaitingCi | Self::Merging
        )
    }
    pub fn permits(self, to: Self) -> bool {
        use State::*;
        matches!(
            (self, to),
            (Queued, Running | Blocked | Cancelled)
                | (Running, Reviewing | Blocked | Failed | Cancelled)
                | (
                    Reviewing,
                    Running | WaitingCi | Blocked | Failed | Cancelled
                )
                | (
                    WaitingCi,
                    Running | Reviewing | Merging | Completed | Blocked | Cancelled
                )
                | (Merging, Completed | Blocked)
                | (Blocked, Queued)
                | (Failed, Queued)
                | (Cancelled, Queued)
        )
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Reviewing => "reviewing",
            Self::WaitingCi => "waiting_ci",
            Self::Merging => "merging",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub project_id: String,
    pub story: Story,
    pub revision: String,
    pub state: State,
    pub attempt: u32,
    pub created_at: i64,
    pub updated_at: i64,
    pub heartbeat: i64,
    pub branch: String,
    pub worktree: PathBuf,
    pub base_sha: Option<String>,
    pub head_sha: Option<String>,
    pub pr: Option<u64>,
    pub pr_url: Option<String>,
    pub merge_sha: Option<String>,
    pub review: Option<Review>,
    pub test_sha: Option<String>,
    pub last_error: Option<String>,
    pub agent_sessions: Vec<String>,
    pub retry_after: i64,
    pub failures: u32,
    #[serde(default)]
    pub worker_inflight: bool,
    #[serde(default)]
    pub stranded_pid: Option<u32>,
}
#[derive(Clone)]
pub struct Store {
    path: PathBuf,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let s = Self { path: path.into() };
        let c = s.conn()?;
        let version: i64 = c.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        ensure!(
            version <= 1,
            "database schema newer than this maistro binary"
        );
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
CREATE TABLE IF NOT EXISTS projects(id TEXT PRIMARY KEY,config TEXT NOT NULL,board_id TEXT,last_scan INTEGER,scan TEXT,paused INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS stories(project_id TEXT NOT NULL,story_id TEXT NOT NULL,revision TEXT NOT NULL,status TEXT NOT NULL,metadata TEXT NOT NULL,PRIMARY KEY(project_id,story_id));
CREATE TABLE IF NOT EXISTS runs(id TEXT PRIMARY KEY,project_id TEXT NOT NULL,story_id TEXT NOT NULL,revision TEXT NOT NULL,state TEXT NOT NULL,priority INTEGER NOT NULL,data TEXT NOT NULL,UNIQUE(project_id,story_id,revision));
CREATE UNIQUE INDEX IF NOT EXISTS active_story ON runs(project_id,story_id) WHERE state IN ('queued','running','reviewing','waiting_ci','merging');
CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT,at INTEGER NOT NULL,kind TEXT NOT NULL,detail TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS leases(repo TEXT PRIMARY KEY,owner TEXT NOT NULL,expires INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS outbox(id INTEGER PRIMARY KEY AUTOINCREMENT,project_id TEXT NOT NULL,story_id TEXT NOT NULL,payload TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,next_at INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS controls(key TEXT PRIMARY KEY,value TEXT NOT NULL);
DELETE FROM outbox WHERE json_extract(payload, '$.status') = 'cancelled';
PRAGMA user_version=1;")?;
        Ok(s)
    }
    fn conn(&self) -> Result<Connection> {
        let c = Connection::open(&self.path)?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
        Ok(c)
    }
    pub fn sync_config(&self, c: &Config) -> Result<()> {
        self.sync_project_config(c, false)
    }
    /// Apply file configuration on startup, retaining durable execution state.
    pub fn sync_startup_config(&self, c: &Config) -> Result<()> {
        self.sync_project_config(c, true)
    }
    fn sync_project_config(&self, c: &Config, startup: bool) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for p in &c.projects {
            let old: Option<String> = tx
                .query_row("SELECT config FROM projects WHERE id=?", [&p.id], |r| {
                    r.get(0)
                })
                .optional()?;
            if let Some(old) = old {
                let old: Project = serde_json::from_str(&old)?;
                let same_repo = old.repo == p.repo
                    || old
                        .repo
                        .canonicalize()
                        .ok()
                        .zip(p.repo.canonicalize().ok())
                        .is_some_and(|(old, new)| old == new);
                let same_identity = same_repo
                    && old.github_repo == p.github_repo
                    && old.base_branch == p.base_branch
                    && old.appflowy_url == p.appflowy_url
                    && old.appflowy_page == p.appflowy_page;
                if !same_identity {
                    ensure!(
                        startup,
                        "project identity cannot change under an existing ID"
                    );
                    let has_work: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM runs WHERE project_id=?1) OR EXISTS(SELECT 1 FROM outbox WHERE project_id=?1)",
                        [&p.id],
                        |r| r.get(0),
                    )?;
                    ensure!(
                        !has_work,
                        "{}: project identity has saved work; use a new project ID",
                        p.id
                    );
                }
                if serde_json::to_value(&old)? != serde_json::to_value(p)? {
                    tx.execute(
                        "UPDATE projects SET scan=NULL,last_scan=NULL,board_id=CASE WHEN ? THEN board_id ELSE NULL END WHERE id=?",
                        params![same_identity, p.id],
                    )?;
                    tx.execute("DELETE FROM stories WHERE project_id=?", [&p.id])?;
                }
            }
            tx.execute("INSERT INTO projects(id,config) VALUES(?,?) ON CONFLICT(id) DO UPDATE SET config=excluded.config",params![p.id,serde_json::to_string(p)?])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn record_scan(&self, scan: &Scan) -> Result<()> {
        let mut c = self.conn()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT board_id FROM projects WHERE id=?",
                [&scan.project_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        ensure!(
            old.as_ref().is_none_or(|s| s == &scan.board_id),
            "resolved board changed; operator must investigate"
        );
        tx.execute(
            "UPDATE projects SET board_id=?,last_scan=?,scan=? WHERE id=?",
            params![
                scan.board_id,
                now(),
                serde_json::to_string(scan)?,
                scan.project_id
            ],
        )?;
        tx.execute("DELETE FROM stories WHERE project_id=?", [&scan.project_id])?;
        for s in &scan.stories {
            tx.execute(
                "INSERT INTO stories VALUES(?,?,?,?,?)",
                params![
                    scan.project_id,
                    s.story_id,
                    s.revision(),
                    serde_json::to_string(&s.status)?,
                    serde_json::to_string(s)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn scan(&self, id: &str) -> Result<Option<Scan>> {
        let c = self.conn()?;
        let s: Option<String> = c
            .query_row("SELECT scan FROM projects WHERE id=?", [id], |r| r.get(0))
            .optional()?
            .flatten();
        s.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }
    pub fn ingest(&self, p: &Project, scan: &Scan, root: &Path) -> Result<usize> {
        self.record_scan(scan)?;
        let mut count = 0;
        for s in &scan.stories {
            if s.status != Status::Ready {
                continue;
            }
            let mut c = self.conn()?;
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let existing:Option<String>=tx.query_row("SELECT data FROM runs WHERE project_id=? AND story_id=? AND (revision=? OR state IN ('queued','running','reviewing','waiting_ci','merging')) LIMIT 1",params![p.id,s.story_id,s.revision()],|row|row.get(0)).optional()?;
            if let Some(data) = existing {
                let mut run: Run = serde_json::from_str(&data)?;
                if run.state == State::Blocked
                    && run.revision == s.revision()
                    && run
                        .last_error
                        .as_deref()
                        .is_some_and(|s| s.starts_with("Dependency "))
                    && s.decision == Decision::Implement
                    && !s.acceptance_criteria.is_empty()
                    && s.dependencies.iter().all(|id| {
                        scan.stories
                            .iter()
                            .any(|d| &d.story_id == id && d.status == Status::Done)
                    })
                {
                    run.state = State::Queued;
                    run.last_error = None;
                    run.retry_after = 0;
                    run.updated_at = now();
                    tx.execute(
                        "UPDATE runs SET state='queued',data=? WHERE id=? AND state='blocked'",
                        params![serde_json::to_string(&run)?, run.id],
                    )?;
                    event(
                        &tx,
                        &run.id,
                        "queued",
                        "dependencies completed; Ready authorization retained",
                    )?;
                    outbox(&tx, &run, Status::Ready, "Dependencies completed; queued")?;
                    tx.commit()?;
                    count += 1;
                }
                continue;
            }
            let reason = if s.decision != Decision::Implement
                || s.acceptance_criteria.is_empty()
                || s.title.trim().is_empty()
            {
                Some(format!("Requirements need clarification: {}", s.reason))
            } else {
                s.dependencies
                    .iter()
                    .find(|d| {
                        !scan
                            .stories
                            .iter()
                            .any(|x| &x.story_id == *d && x.status == Status::Done)
                    })
                    .map(|d| {
                        format!("Dependency {d} is missing or not Done; execution waits while Ready authorization remains")
                    })
            };
            let id = uuid::Uuid::new_v4().to_string();
            let run = Run {
                id: id.clone(),
                project_id: p.id.clone(),
                story: s.clone(),
                revision: s.revision(),
                state: if reason.is_some() {
                    State::Blocked
                } else {
                    State::Queued
                },
                attempt: 0,
                created_at: now(),
                updated_at: now(),
                heartbeat: now(),
                branch: format!("maistro/{}/{id}", p.id),
                worktree: root.join(&id),
                base_sha: None,
                head_sha: None,
                pr: None,
                pr_url: None,
                merge_sha: None,
                review: None,
                test_sha: None,
                last_error: reason,
                agent_sessions: vec![],
                retry_after: 0,
                failures: 0,
                worker_inflight: false,
                stranded_pid: None,
            };
            tx.execute(
                "INSERT INTO runs VALUES(?,?,?,?,?,?,?)",
                params![
                    run.id,
                    p.id,
                    s.story_id,
                    run.revision,
                    run.state.as_str(),
                    s.priority,
                    serde_json::to_string(&run)?
                ],
            )?;
            event(&tx, &run.id, "created", run.state.as_str())?;
            if run.state == State::Blocked {
                outbox(
                    &tx,
                    &run,
                    if run
                        .last_error
                        .as_deref()
                        .is_some_and(|s| s.starts_with("Dependency "))
                    {
                        Status::Ready
                    } else {
                        Status::Blocked
                    },
                    run.last_error.as_deref().unwrap_or("Blocked"),
                )?;
            }
            tx.commit()?;
            count += 1;
        }
        Ok(count)
    }
    pub fn runs(&self) -> Result<Vec<Run>> {
        let c = self.conn()?;
        let mut st = c.prepare("SELECT data FROM runs ORDER BY priority DESC,rowid")?;
        st.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn get(&self, id: &str) -> Result<Run> {
        let c = self.conn()?;
        let s: String = c
            .query_row("SELECT data FROM runs WHERE id=?", [id], |r| r.get(0))
            .context("unknown run ID")?;
        Ok(serde_json::from_str(&s)?)
    }
    pub fn save(&self, r: &mut Run, expected: State, message: &str) -> Result<()> {
        ensure!(
            r.state == expected || expected.permits(r.state),
            "illegal state transition {:?} -> {:?}",
            expected,
            r.state
        );
        r.updated_at = now();
        r.heartbeat = now();
        let mut c = self.conn()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(
            tx.execute(
                "UPDATE runs SET state=?,data=? WHERE id=? AND state=?",
                params![
                    r.state.as_str(),
                    serde_json::to_string(r)?,
                    r.id,
                    expected.as_str()
                ]
            )? == 1,
            "state conflict"
        );
        event(&tx, &r.id, r.state.as_str(), message)?;
        if r.state == State::Cancelled {
            // Stop execution internally without changing the user's board status.
            tx.execute(
                "DELETE FROM outbox WHERE project_id=? AND story_id=?",
                params![r.project_id, r.story.story_id],
            )?;
        }
        if r.state != expected {
            let status = match r.state {
                State::Running => Some(Status::InProgress),
                State::Reviewing | State::WaitingCi => Some(Status::Review),
                State::Completed => Some(Status::Done),
                State::Blocked | State::Failed => Some(Status::Blocked),
                State::Cancelled => None,
                _ => None,
            };
            if let Some(s) = status {
                outbox(&tx, r, s, message)?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn transition(&self, r: &mut Run, to: State, message: &str) -> Result<()> {
        let from = r.state;
        r.state = to;
        self.save(r, from, message)
    }
    pub fn claim(&self, r: &mut Run, p: &Project, max: usize) -> Result<bool> {
        let mut c = self.conn()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let paused = control_tx(&tx, "paused")?.as_deref() == Some("true");
        let project_paused: bool =
            tx.query_row("SELECT paused FROM projects WHERE id=?", [&p.id], |r| {
                r.get(0)
            })?;
        if paused || project_paused {
            return Ok(false);
        }
        let existing: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE project_id=? AND id!=? AND state IN ('running','reviewing','waiting_ci','merging'))",params![p.id,r.id],|row|row.get(0))?;
        if existing {
            return Ok(false);
        }
        let busy: i64 = tx.query_row("SELECT count(*) FROM leases", [], |r| r.get(0))?;
        if busy >= max as i64 {
            return Ok(false);
        }
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO leases VALUES(?,?,?)",
            params![p.repo.canonicalize()?.to_string_lossy(), r.id, now() + 120],
        )?;
        if inserted == 0 {
            return Ok(false);
        }
        r.state = State::Running;
        r.attempt += 1;
        r.updated_at = now();
        r.heartbeat = now();
        ensure!(
            tx.execute(
                "UPDATE runs SET state='running',data=? WHERE id=? AND state='queued'",
                params![serde_json::to_string(r)?, r.id]
            )? == 1,
            "claim conflict"
        );
        event(&tx, &r.id, "running", "claimed transactionally")?;
        outbox(&tx, r, Status::InProgress, "Implementation started")?;
        tx.commit()?;
        Ok(true)
    }
    pub fn acquire_recovery(&self, r: &Run, p: &Project) -> Result<()> {
        self.conn()?.execute("INSERT INTO leases VALUES(?,?,?) ON CONFLICT(repo) DO UPDATE SET expires=excluded.expires WHERE owner=excluded.owner",params![p.repo.canonicalize()?.to_string_lossy(),r.id,now()+120])?;
        Ok(())
    }
    pub fn release(&self, id: &str) -> Result<()> {
        self.conn()?
            .execute("DELETE FROM leases WHERE owner=?", [id])?;
        Ok(())
    }
    pub fn reset_leases(&self) -> Result<()> {
        self.conn()?.execute("DELETE FROM leases", [])?;
        Ok(())
    }
    pub fn renew(&self, id: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE leases SET expires=? WHERE owner=?",
            params![now() + 120, id],
        )?;
        Ok(())
    }
    pub fn pending(&self) -> Result<Vec<(i64, Projection, u32)>> {
        let c = self.conn()?;
        let mut q =
            c.prepare("SELECT id,payload,attempts FROM outbox WHERE next_at<=? ORDER BY id")?;
        q.query_map([now()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
            ))
        })?
        .map(|r| {
            let (i, s, a) = r?;
            Ok((i, serde_json::from_str(&s)?, a))
        })
        .collect()
    }
    pub fn ack(&self, id: i64) -> Result<()> {
        self.conn()?
            .execute("DELETE FROM outbox WHERE id=?", [id])?;
        Ok(())
    }
    pub fn defer(&self, id: i64, attempt: u32) -> Result<()> {
        self.conn()?.execute(
            "UPDATE outbox SET attempts=attempts+1,next_at=? WHERE id=?",
            params![now() + backoff(attempt), id],
        )?;
        Ok(())
    }
    pub fn control(&self, key: &str) -> Result<Option<String>> {
        control_tx(&self.conn()?, key)
    }
    pub fn set_control(&self, key: &str, value: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO controls VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
    pub fn project_pause(&self, id: &str, paused: bool) -> Result<()> {
        ensure!(
            self.conn()?.execute(
                "UPDATE projects SET paused=? WHERE id=?",
                params![paused, id]
            )? == 1,
            "unknown project"
        );
        Ok(())
    }
    pub fn paused(&self, id: &str) -> Result<bool> {
        if self.control("paused")?.as_deref() == Some("true") {
            return Ok(true);
        }
        Ok(self
            .conn()?
            .query_row("SELECT paused FROM projects WHERE id=?", [id], |r| r.get(0))?)
    }
}
fn control_tx(c: &Connection, key: &str) -> Result<Option<String>> {
    Ok(
        c.query_row("SELECT value FROM controls WHERE key=?", [key], |r| {
            r.get(0)
        })
        .optional()?,
    )
}
fn event(c: &Connection, id: &str, kind: &str, detail: &str) -> Result<()> {
    c.execute(
        "INSERT INTO events(run_id,at,kind,detail) VALUES(?,?,?,?)",
        params![id, now(), kind, detail],
    )?;
    Ok(())
}
fn outbox(c: &Connection, r: &Run, status: Status, message: &str) -> Result<()> {
    let p = Projection {
        project_id: r.project_id.clone(),
        story_id: r.story.story_id.clone(),
        run_id: r.id.clone(),
        status,
        message: format!(
            "🤖 {message}\n[maistro run={} state={} attempt={}]{}",
            r.id,
            r.state.as_str(),
            r.attempt,
            r.pr_url
                .as_ref()
                .map(|url| format!("\nPR: {url}"))
                .unwrap_or_default()
        ),
        pr_url: r.pr_url.clone(),
        attempt: r.attempt,
    };
    c.execute(
        "DELETE FROM outbox WHERE project_id=? AND story_id=?",
        params![r.project_id, r.story.story_id],
    )?;
    c.execute(
        "INSERT INTO outbox(project_id,story_id,payload) VALUES(?,?,?)",
        params![r.project_id, r.story.story_id, serde_json::to_string(&p)?],
    )?;
    Ok(())
}
pub fn backoff(attempt: u32) -> i64 {
    (5_i64.saturating_mul(2_i64.saturating_pow(attempt.min(10)))).min(3600)
}
#[cfg(test)]
impl Store {
    pub(crate) fn make_outbox_due(&self) -> Result<()> {
        self.conn()?.execute("UPDATE outbox SET next_at=0", [])?;
        Ok(())
    }
}
impl Store {
    pub fn clear_control(&self, key: &str) -> Result<()> {
        self.conn()?
            .execute("DELETE FROM controls WHERE key=?", [key])?;
        Ok(())
    }
    pub fn diagnostics(&self) -> Result<serde_json::Value> {
        let c = self.conn()?;
        let events: i64 = c.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
        let pending: i64 = c.query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))?;
        let mut q = c.prepare("SELECT id,last_scan,paused FROM projects")?;
        let projects=q.query_map([],|r|Ok(serde_json::json!({"id":r.get::<_,String>(0)?,"last_scan":r.get::<_,Option<i64>>(1)?,"paused":r.get::<_,bool>(2)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
        Ok(
            serde_json::json!({"paused":self.control("paused")?.as_deref()==Some("true"),"projects":projects,"runs":self.runs()?,"event_count":events,"pending_projections":pending}),
        )
    }
}
