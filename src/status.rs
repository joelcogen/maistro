//! Ephemeral display observations. This module has no agent or board client.
use crate::{
    config::{Config, Mode},
    model::{Assignment, Outcome},
};
use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Method, StatusCode, Uri},
    response::Response,
};
use serde::{Deserialize, Serialize};
use std::{
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{net::TcpListener, sync::watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    pub ready: u64,
    pub in_progress: u64,
    pub blocked: u64,
    pub review: u64,
    pub backlog: u64,
    pub done: u64,
}
impl Counts {
    fn add(&mut self, other: Self) {
        self.ready = self.ready.saturating_add(other.ready);
        self.in_progress = self.in_progress.saturating_add(other.in_progress);
        self.blocked = self.blocked.saturating_add(other.blocked);
        self.review = self.review.saturating_add(other.review);
        self.backlog = self.backlog.saturating_add(other.backlog);
        self.done = self.done.saturating_add(other.done);
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadState {
    Complete,
    Partial,
    Failed,
    NotRead,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    NoEligibleWork,
    Dependencies,
    Occupied,
    ReadFailed,
    AmbiguousBoard,
    IncompleteRead,
    ClaimFailed,
    RequirementsBlocked,
    UpdateFailed,
    WorkSelected,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardRead {
    pub project_id: String,
    pub counts: Option<Counts>,
    pub read: ReadState,
    pub reason: Reason,
}
#[derive(Clone, Debug, Serialize)]
pub struct Observation {
    pub counts: Option<Counts>,
    /// Read occurred during this coordinator pass; receipt is not a board timestamp.
    pub read_started_at: Option<u64>,
    pub received_at: Option<u64>,
    pub stale: bool,
    pub last_read: ReadState,
    pub reason: Option<Reason>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Activity {
    pub state: &'static str,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub elapsed_seconds: u64,
    pub latest_event: &'static str,
    pub last_error: Option<&'static str>,
    #[serde(skip)]
    clock: Option<Instant>,
}
impl Activity {
    fn new(state: &'static str) -> Self {
        Self {
            state,
            started_at: None,
            finished_at: None,
            elapsed_seconds: 0,
            latest_event: "Not started in this process",
            last_error: None,
            clock: None,
        }
    }
    fn start(&mut self, state: &'static str) {
        let last_error = self.last_error;
        *self = Self::new(state);
        self.last_error = last_error;
        self.started_at = Some(now());
        self.clock = Some(Instant::now());
    }
    fn finish(&mut self, state: &'static str, event: &'static str) {
        self.refresh();
        self.clock = None;
        self.finished_at = Some(now());
        self.state = state;
        self.latest_event = event;
    }
    fn refresh(&mut self) {
        if let Some(start) = self.clock {
            self.elapsed_seconds = start.elapsed().as_secs();
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Story {
    pub id: String,
    pub title: String,
    pub url: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProjectStatus {
    pub id: String,
    pub enabled: bool,
    pub mode: Mode,
    pub board_url: Option<String>,
    pub story: Option<Story>,
    pub worker: Activity,
    pub last_outcome: Option<Outcome>,
    pub observation: Observation,
    pub polling: Polling,
}
#[derive(Clone, Debug, Serialize)]
pub struct Polling {
    pub method: &'static str,
    pub last_poll_at: Option<u64>,
    pub last_change_at: Option<u64>,
    pub dirty: bool,
    pub last_error: Option<&'static str>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Overall {
    /// Sum of known observations, explicitly a subtotal when coverage is incomplete.
    pub counts: Option<Counts>,
    pub observed_projects: usize,
    pub total_projects: usize,
    pub stale: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct StatusSnapshot {
    pub started_at: u64,
    pub generated_at: u64,
    pub uptime_seconds: u64,
    pub coordinator: Activity,
    pub projects: Vec<ProjectStatus>,
    pub overall: Overall,
}
pub struct Status {
    view: StatusSnapshot,
    started: Instant,
}
impl Status {
    pub fn new(config: &Config) -> Self {
        let projects = config
            .projects
            .iter()
            .map(|p| ProjectStatus {
                id: p.id.clone(),
                enabled: p.enabled,
                mode: p.mode,
                board_url: p.board_url().and_then(safe_url),
                story: None,
                worker: Activity::new(if p.enabled { "idle" } else { "disabled" }),
                last_outcome: None,
                polling: Polling {
                    method: if p.is_shortcut() {
                        "coordinator"
                    } else {
                        "rust"
                    },
                    last_poll_at: None,
                    last_change_at: None,
                    dirty: p.enabled,
                    last_error: None,
                },
                observation: Observation {
                    counts: None,
                    read_started_at: None,
                    received_at: None,
                    stale: false,
                    last_read: ReadState::NotRead,
                    reason: None,
                },
            })
            .collect();
        Self {
            started: Instant::now(),
            view: StatusSnapshot {
                started_at: now(),
                generated_at: now(),
                uptime_seconds: 0,
                coordinator: Activity::new("idle"),
                projects,
                overall: Overall {
                    counts: None,
                    observed_projects: 0,
                    total_projects: config.projects.len(),
                    stale: false,
                },
            },
        }
    }
    pub fn snapshot(&self) -> StatusSnapshot {
        let mut view = self.view.clone();
        view.generated_at = now();
        view.uptime_seconds = self.started.elapsed().as_secs();
        view.coordinator.refresh();
        for p in &mut view.projects {
            p.worker.refresh();
        }
        let mut total = Counts::default();
        for p in &view.projects {
            if let Some(counts) = p.observation.counts {
                total.add(counts);
                view.overall.observed_projects += 1;
            }
            view.overall.stale |= p.observation.stale;
        }
        if view.overall.observed_projects > 0 {
            view.overall.counts = Some(total);
        }
        view
    }
    pub fn coordinator_started(&mut self) {
        self.view.coordinator.start("reading_boards");
        self.view.coordinator.latest_event = "Coordination pass running";
    }
    #[cfg(test)]
    pub fn coordinator_finished(&mut self, reads: Option<&[BoardRead]>) {
        let selected = self
            .view
            .projects
            .iter()
            .filter(|p| p.enabled)
            .map(|p| p.id.clone())
            .collect::<Vec<_>>();
        self.coordinator_finished_for(reads, &selected);
    }
    pub fn coordinator_finished_for(&mut self, reads: Option<&[BoardRead]>, selected: &[String]) {
        self.view.coordinator.finish(
            "idle",
            if reads.is_some() {
                "Coordination pass completed"
            } else {
                "Coordinator failed; retry scheduled. See private daemon logs for details"
            },
        );
        self.view.coordinator.last_error = if reads.is_none() {
            Some("Coordinator failed; see private daemon logs")
        } else {
            None
        };
        for p in self
            .view
            .projects
            .iter_mut()
            .filter(|p| p.enabled && selected.contains(&p.id))
        {
            let matching: Vec<_> = reads
                .unwrap_or_default()
                .iter()
                .filter(|r| r.project_id == p.id)
                .collect();
            let o = &mut p.observation;
            // Missing or duplicate observations are never interpreted as zero/success.
            let read = if matching.len() == 1 {
                Some(matching[0])
            } else {
                None
            };
            o.last_read = read.map_or(ReadState::Failed, |r| r.read);
            o.reason = Some(read.map_or(Reason::ReadFailed, |r| r.reason));
            if let Some(r) = read.filter(|r| r.read == ReadState::Complete && r.counts.is_some()) {
                o.counts = r.counts;
                o.read_started_at = self.view.coordinator.started_at;
                o.received_at = Some(now());
                o.stale = false;
            } else {
                o.stale = true;
            }
        }
    }
    pub fn polled(&mut self, id: &str, changed: bool, failed: bool) {
        let p = &mut self.project(id).polling;
        p.last_poll_at = Some(now());
        if changed {
            p.last_change_at = Some(now());
        }
        p.last_error = failed
            .then_some("Board poll failed; last fingerprint retained. See private daemon logs");
    }
    pub fn poll_dirty(&mut self, id: &str, dirty: bool) {
        self.project(id).polling.dirty = dirty;
    }
    fn project(&mut self, id: &str) -> &mut ProjectStatus {
        self.view
            .projects
            .iter_mut()
            .find(|p| p.id == id)
            .expect("configured project")
    }
    pub fn worker_started(&mut self, a: &Assignment) {
        let p = self.project(&a.project_id);
        p.story = Some(Story {
            id: a.story_id.clone(),
            title: a.title.chars().take(240).collect(),
            url: a.story_url.as_deref().and_then(safe_url),
        });
        p.worker.start("active");
        p.worker.latest_event = if a.recovery {
            "Recovering interrupted work"
        } else {
            "Working on assigned story"
        };
    }
    pub fn worker_finished(&mut self, id: &str, outcome: Option<Outcome>, failed: bool) {
        if outcome.is_some() {
            self.project(id).last_outcome = outcome;
        }
        self.project(id).worker.finish(
            if outcome.is_some() {
                "pending_update"
            } else {
                "idle"
            },
            match (outcome, failed) {
                (None, _) => "Worker stopped after board cancellation",
                (_, true) => {
                    "Worker process failed; blocked result awaiting board. See private daemon logs"
                }
                (Some(Outcome::Completed), _) => "Completed; awaiting board recording",
                (Some(Outcome::Blocked), _) => "Worker reported blocked; awaiting board recording",
            },
        );
        self.project(id).worker.last_error = if failed && outcome.is_some() {
            Some("Worker process failed; see private daemon logs")
        } else {
            None
        };
        if outcome.is_none() {
            self.project(id).story = None;
        }
    }
    pub fn cancelling(&mut self, id: &str) {
        let p = self.project(id);
        p.worker.state = "stopping";
        p.worker.latest_event = "Board revoked work; stopping worker";
    }
    pub fn released(&mut self, id: &str, recorded: bool) {
        let p = self.project(id);
        p.worker.state = "idle";
        p.worker.latest_event = if recorded {
            match p.last_outcome {
                Some(Outcome::Completed) => "Completed result reconciled with board",
                Some(Outcome::Blocked) => "Blocked result reconciled with board",
                None => "Result reconciled with board",
            }
        } else {
            "Pending result discarded after board cancellation"
        };
        p.story = None;
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn safe_url(value: &str) -> Option<String> {
    let uri: Uri = value.parse().ok()?;
    (matches!(uri.scheme_str(), Some("http" | "https"))
        && uri.authority().is_some_and(|a| !a.as_str().contains('@'))
        && !value.contains(['\\', '\n', '\r'])
        && value.len() <= 2048)
        .then(|| value.to_owned())
}

pub type Published = watch::Receiver<Arc<StatusSnapshot>>;
pub async fn bind(port: u16) -> Result<TcpListener> {
    TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .with_context(|| format!("cannot bind status server to 127.0.0.1:{port}"))
}
pub async fn serve(
    listener: TcpListener,
    status: Published,
    stop: CancellationToken,
) -> Result<()> {
    // The router receives only a display snapshot, with no engine/agent handles.
    let app = Router::new().fallback(handle).with_state(status);
    axum::serve(listener, app)
        .with_graceful_shutdown(stop.cancelled_owned())
        .await?;
    Ok(())
}
async fn handle(State(status): State<Published>, method: Method, uri: Uri) -> Response {
    let (code, content_type, body) = if method != Method::GET && method != Method::HEAD {
        (
            StatusCode::METHOD_NOT_ALLOWED,
            "text/plain; charset=utf-8",
            b"Read-only: GET or HEAD only".to_vec(),
        )
    } else {
        match uri.path() {
            "/" => (
                StatusCode::OK,
                "text/html; charset=utf-8",
                include_bytes!("status.html").to_vec(),
            ),
            "/status.js" => (
                StatusCode::OK,
                "text/javascript; charset=utf-8",
                include_bytes!("status.js").to_vec(),
            ),
            "/status.css" => (
                StatusCode::OK,
                "text/css; charset=utf-8",
                include_bytes!("status.css").to_vec(),
            ),
            "/api/status" => {
                // Drop the watch borrow before serialization or network I/O.
                let snapshot = status.borrow().clone();
                (
                    StatusCode::OK,
                    "application/json",
                    serde_json::to_vec(&*snapshot).expect("status serializes"),
                )
            }
            _ => (
                StatusCode::NOT_FOUND,
                "text/plain; charset=utf-8",
                b"Not found".to_vec(),
            ),
        }
    };
    let mut response = Response::builder().status(code)
        .header("Content-Type", content_type).header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .header("Referrer-Policy", "no-referrer")
        .header("Content-Security-Policy", "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'")
        .header("Content-Length", body.len());
    if code == StatusCode::METHOD_NOT_ALLOWED {
        response = response.header("Allow", "GET, HEAD");
    }
    response
        .body(if method == Method::HEAD {
            Body::empty()
        } else {
            Body::from(body)
        })
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn config() -> Config {
        toml::from_str(
            r#"
            [[projects]]
            id = "a"
            repo = "/private/checkout"
            appflowy_page = "private board context"
            mode = "master"
            base_branch = "master"
            [[projects]]
            id = "b"
            repo = "/private/other"
            appflowy_page = "Other"
            mode = "pr"
            base_branch = "main"
            [[projects]]
            id = "disabled"
            enabled = false
            repo = "/private/disabled"
            appflowy_page = "Disabled"
            mode = "master"
            base_branch = "master"
        "#,
        )
        .unwrap()
    }
    fn read(project: &str, state: ReadState, counts: Option<Counts>) -> BoardRead {
        BoardRead {
            project_id: project.into(),
            read: state,
            counts,
            reason: Reason::NoEligibleWork,
        }
    }
    fn assignment(project: &str) -> Assignment {
        Assignment {
            project_id: project.into(),
            story_id: "story-id".into(),
            title: "<img src=x onerror=alert(1)>".into(),
            story_url: Some("javascript:alert(1)".into()),
            context: "SECRET prompt and requirements".into(),
            recovery: false,
            pr: None,
        }
    }
    #[test]
    fn counts_are_unknown_until_complete_and_partial_failures_retain_last_observation() {
        let mut status = Status::new(&config());
        let initial = status.snapshot();
        assert_eq!(initial.overall.counts, None);
        assert!(
            initial
                .projects
                .iter()
                .all(|p| p.observation.counts.is_none())
        );
        status.coordinator_started();
        let counts = Counts {
            ready: 3,
            in_progress: 2,
            review: 7,
            backlog: 11,
            done: 19,
            blocked: 5,
        };
        status.coordinator_finished(Some(&[
            read("a", ReadState::Complete, Some(counts)),
            read("b", ReadState::Partial, Some(Counts::default())),
        ]));
        let first = status.snapshot();
        assert_eq!(first.overall.counts, Some(counts));
        assert_eq!(first.overall.observed_projects, 1);
        assert_eq!(first.overall.total_projects, 3);
        assert!(first.projects[1].observation.counts.is_none());
        assert!(first.overall.stale);
        let received = first.projects[0].observation.received_at;
        assert!(received.is_some());
        for read_state in [ReadState::Partial, ReadState::Failed, ReadState::NotRead] {
            status.coordinator_started();
            status.coordinator_finished(Some(&[
                read("a", read_state, Some(Counts::default())),
                read("b", ReadState::Complete, Some(Counts::default())),
            ]));
            let view = status.snapshot();
            assert_eq!(view.projects[0].observation.counts, Some(counts));
            assert_eq!(view.projects[0].observation.received_at, received);
            assert!(view.projects[0].observation.stale);
            assert!(!view.projects[1].observation.stale);
            assert_eq!(view.overall.observed_projects, 2);
        }
        status.coordinator_finished(None);
        assert_eq!(
            status.snapshot().projects[0].observation.counts,
            Some(counts)
        );
        status.coordinator_started();
        assert!(status.snapshot().coordinator.last_error.is_some());
        status.coordinator_finished(Some(&[read(
            "a",
            ReadState::Complete,
            Some(Counts::default()),
        )]));
        let view = status.snapshot();
        assert_eq!(view.projects[0].observation.counts, Some(Counts::default()));
        assert!(!view.projects[0].observation.stale);
        assert!(view.projects[1].observation.stale); // Missing observation.
        assert!(view.projects[2].observation.counts.is_none()); // Disabled.
        assert!(view.coordinator.last_error.is_none());
    }
    #[test]
    fn invalid_or_duplicate_observations_cannot_replace_counts() {
        let mut status = Status::new(&config());
        status.coordinator_started();
        status.coordinator_finished(Some(&[read("a", ReadState::Complete, None)]));
        assert!(status.snapshot().projects[0].observation.stale);
        assert!(status.snapshot().projects[0].observation.counts.is_none());
        status.coordinator_finished(Some(&[
            read("a", ReadState::Complete, Some(Counts::default())),
            read("a", ReadState::Complete, Some(Counts::default())),
            read("unknown", ReadState::Complete, Some(Counts::default())),
        ]));
        assert_eq!(status.snapshot().overall.observed_projects, 0);
    }
    #[test]
    fn parallel_worker_pending_and_idle_snapshots_exclude_private_data() {
        let mut status = Status::new(&config());
        status.worker_started(&assignment("a"));
        status.worker_started(&assignment("b"));
        status.project("a").worker.clock = Some(Instant::now() - std::time::Duration::from_secs(6));
        let active = status.snapshot();
        assert_eq!(active.projects[0].worker.state, "active");
        assert!(active.projects[0].worker.elapsed_seconds >= 6);
        assert_eq!(active.projects[1].worker.state, "active");
        assert_eq!(active.projects[2].worker.state, "disabled");
        status.worker_finished("a", Some(Outcome::Completed), false);
        let pending = status.snapshot();
        assert_eq!(pending.projects[0].worker.state, "pending_update");
        assert!(pending.projects[0].worker.finished_at.is_some());
        assert!(pending.projects[0].story.is_some());
        assert_eq!(
            pending.projects[0].worker.elapsed_seconds,
            status.snapshot().projects[0].worker.elapsed_seconds
        );
        status.released("a", true);
        status.cancelling("b");
        assert_eq!(status.snapshot().projects[1].worker.state, "stopping");
        status.worker_finished("b", None, true);
        let idle = status.snapshot();
        assert_eq!(idle.projects[0].worker.state, "idle");
        assert!(idle.projects[0].worker.latest_event.contains("Completed"));
        assert!(idle.projects[0].story.is_none());
        assert!(idle.projects[1].story.is_none());
        status.worker_started(&assignment("b"));
        status.worker_finished("b", Some(Outcome::Blocked), true);
        assert!(status.snapshot().projects[1].worker.last_error.is_some());
        let json = serde_json::to_string(&status.snapshot()).unwrap();
        for private in [
            "SECRET",
            "/private",
            "private board context",
            "javascript:",
            "github_token",
            "context",
            "clock",
        ] {
            assert!(!json.contains(private), "exposed {private}");
        }
    }
    #[test]
    fn only_safe_links_are_exposed() {
        for url in [
            "javascript:alert(1)",
            "data:text/html,x",
            "file:///etc/passwd",
            "//host",
            "https://user:secret@host/path",
            "https://host\\@evil/path",
        ] {
            assert!(safe_url(url).is_none(), "accepted {url}");
        }
        assert_eq!(
            safe_url("https://appflowy.example/story?id=123"),
            Some("https://appflowy.example/story?id=123".into())
        );
    }
    #[tokio::test]
    async fn http_is_local_read_only_and_does_not_modify_snapshots() {
        let listener = bind(0).await.unwrap();
        let address = listener.local_addr().unwrap();
        assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
        assert!(
            bind(address.port())
                .await
                .unwrap_err()
                .to_string()
                .contains("cannot bind status server to 127.0.0.1")
        );
        let initial = Arc::new(Status::new(&config()).snapshot());
        let (sender, receiver) = watch::channel(initial.clone());
        let stop = CancellationToken::new();
        let job = tokio::spawn(serve(listener, receiver, stop.clone()));
        for (method, path, expected) in [
            ("GET", "/", 200),
            ("GET", "/status.js", 200),
            ("GET", "/status.css", 200),
            ("HEAD", "/api/status", 200),
            ("POST", "/api/status", 405),
            ("PUT", "/api/status", 405),
            ("DELETE", "/api/status", 405),
            ("PATCH", "/", 405),
            ("GET", "/retry", 404),
            ("GET", "/logs", 404),
            ("GET", "/../Cargo.toml", 404),
        ] {
            let response = request(address, method, path).await;
            assert!(
                response.starts_with(&format!("HTTP/1.1 {expected}")),
                "{response}"
            );
            assert!(response.contains("cache-control: no-store"));
            assert!(response.contains("content-security-policy:"));
            if method == "HEAD" {
                assert!(response.ends_with("\r\n\r\n"));
            }
        }
        let response = request(address, "GET", "/api/status").await;
        let value: serde_json::Value =
            serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(
            value["projects"][0]["observation"]["counts"],
            serde_json::Value::Null
        );
        assert_eq!(serde_json::to_value(&*initial).unwrap(), value);
        assert!(Arc::ptr_eq(&initial, &sender.borrow()));
        stop.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), job)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    async fn request(address: std::net::SocketAddr, method: &str, path: &str) -> String {
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        socket
            .write_all(
                format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).await.unwrap();
        response
    }
}
