use maistro::{
    config::{Agents, Config, Mode, Orchestrator, Project, Statuses},
    engine::Engine,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command, time::Duration};
use tokio_util::sync::CancellationToken;

struct Fixture {
    temp: tempfile::TempDir,
    config: Config,
}
fn git(path: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().into()
}
impl Fixture {
    fn new(modes: &[Mode]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let executable = root.join("fake-codex");
        fs::write(
            &executable,
            include_str!("fixtures/codex.py")
                .replace("__ROOT__", &serde_json::to_string(root).unwrap()),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let home = root.join("codex-home");
        fs::create_dir(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "[mcp_servers.appflowy]\ncommand = 'fixture'\n",
        )
        .unwrap();
        let prompt = root.join("prompt.md");
        fs::write(&prompt, "Run fixture tests.").unwrap();
        let mut board = serde_json::Map::new();
        let mut projects = vec![];
        for (i, mode) in modes.iter().enumerate() {
            let id = format!("p{i}");
            let repo = root.join(&id);
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-b", "main"]);
            git(&repo, &["config", "user.email", "fixture@example.test"]);
            git(&repo, &["config", "user.name", "Fixture"]);
            git(&repo, &["config", "commit.gpgsign", "false"]);
            git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
            let remote = root.join(format!("remote{i}.git"));
            fs::create_dir(&remote).unwrap();
            git(&remote, &["init", "--bare", "-b", "main"]);
            git(
                &repo,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            );
            git(&repo, &["push", "-u", "origin", "main"]);
            board.insert(
                id.clone(),
                json!({"id":format!("story-{i}"),"status":"Ready", "mode":mode}),
            );
            projects.push(Project {
                id,
                repo,
                appflowy_page: Some(format!("board-{i}")),
                appflowy_url: None,
                mode: *mode,
                base_branch: "main".into(),
                remote: "origin".into(),
                enabled: true,
                prompt_file: Some(prompt.clone()),
                statuses: Statuses::default(),
            });
        }
        fs::write(root.join("board.json"), serde_json::to_vec(&board).unwrap()).unwrap();
        let config = Config {
            agents: Agents {
                codex: executable,
                codex_home: home,
                mcp_server: "appflowy".into(),
            },
            orchestrator: Orchestrator {
                runtime_dir: root.join("runtime"),
                poll_interval_seconds: 1,
                coordinator_timeout_seconds: 10,
                worker_timeout_seconds: 15,
            },
            projects,
        };
        Self { temp, config }
    }
    fn events(&self) -> Vec<Value> {
        fs::read_to_string(self.temp.path().join("events"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
    fn board(&self) -> Value {
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.temp.path().join("board.lock"))
            .unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        serde_json::from_slice(&fs::read(self.temp.path().join("board.json")).unwrap()).unwrap()
    }
    fn status(&self, project: &str, status: &str) {
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.temp.path().join("board.lock"))
            .unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        let path = self.temp.path().join("board.json");
        let mut board: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        board[project]["status"] = status.into();
        fs::write(path, serde_json::to_vec(&board).unwrap()).unwrap();
    }
    fn touch(&self, name: &str) {
        fs::write(self.temp.path().join(name), "").unwrap();
    }
    fn start(
        &self,
    ) -> (
        CancellationToken,
        tokio::task::JoinHandle<anyhow::Result<()>>,
    ) {
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let engine = Engine::new(self.config.clone());
        (cancel, tokio::spawn(async move { engine.run(token).await }))
    }
    async fn wait(&self, condition: impl Fn() -> bool) -> bool {
        tokio::time::timeout(Duration::from_secs(15), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .is_ok()
    }
    async fn stop(
        &self,
        token: CancellationToken,
        job: tokio::task::JoinHandle<anyhow::Result<()>>,
    ) {
        token.cancel();
        let result = tokio::time::timeout(Duration::from_secs(10), job)
            .await
            .unwrap()
            .unwrap();
        if let Err(e) = result {
            panic!("engine failed: {e:#}; events: {:?}", self.events());
        }
    }
}

#[tokio::test]
async fn projects_run_in_parallel_and_workers_commit_and_push_both_modes() {
    let f = Fixture::new(&[Mode::Master, Mode::Pr]);
    f.touch("parallel");
    let (token, job) = f.start();
    let completed = f
        .wait(|| f.board()["p0"]["status"] == "Done" && f.board()["p1"]["status"] == "Review")
        .await;
    f.stop(token, job).await;
    assert!(completed, "events: {:?}", f.events());
    let events = f.events();
    let starts: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "worker_start")
        .collect();
    assert_eq!(starts.len(), 2, "one worker per project");
    let first_finish = events
        .iter()
        .position(|e| e["kind"] == "worker_finish")
        .unwrap();
    assert_eq!(
        events[..first_finish]
            .iter()
            .filter(|e| e["kind"] == "worker_start")
            .count(),
        2
    );
    for (i, branch) in ["main", "maistro/p1/story-1"].iter().enumerate() {
        let sha = git(
            &f.temp.path().join(format!("remote{i}.git")),
            &["rev-parse", branch],
        );
        assert_eq!(f.board()[format!("p{i}")]["result"]["commit"], sha);
    }
    assert_eq!(
        git(&f.config.projects[0].repo, &["branch", "--list"]).trim(),
        "* main"
    );
    assert!(!f.config.orchestrator.runtime_dir.join("state.db").exists());
    assert!(
        fs::read_dir(&f.config.orchestrator.runtime_dir)
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("invocation-"))
    );
}

#[tokio::test]
async fn pending_completion_survives_appflowy_outage_in_memory_without_redispatch() {
    let f = Fixture::new(&[Mode::Master]);
    f.touch("outage");
    let (token, job) = f.start();
    let retried = f
        .wait(|| f.events().iter().filter(|e| e["kind"] == "outage").count() >= 2)
        .await;
    let held = f.board()["p0"]["status"] == "In Progress";
    fs::remove_file(f.temp.path().join("outage")).unwrap();
    let completed = f.wait(|| f.board()["p0"]["status"] == "Done").await;
    f.stop(token, job).await;
    assert!(retried && held && completed, "events: {:?}", f.events());
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| e["kind"] == "worker_start")
            .count(),
        1
    );
}

#[tokio::test]
async fn cancellation_stops_active_worker_and_preserves_user_status() {
    let f = Fixture::new(&[Mode::Master]);
    f.touch("hang");
    let (token, job) = f.start();
    let started = f
        .wait(|| f.events().iter().any(|e| e["kind"] == "worker_start"))
        .await;
    f.status("p0", "Cancelled");
    let stopped = f
        .wait(|| {
            f.events()
                .iter()
                .any(|e| e["kind"] == "stopped" && e["model"] == "gpt-6-astra")
        })
        .await;
    f.stop(token, job).await;
    assert!(started && stopped, "events: {:?}", f.events());
    assert_eq!(f.board()["p0"]["status"], "Cancelled");
    assert!(f.board()["p0"].get("result").is_none());
}

#[tokio::test]
async fn restart_recovers_in_progress_from_board_and_git_without_duplicate_commit() {
    let f = Fixture::new(&[Mode::Master]);
    // Crash window: a commit exists, but no push or AppFlowy result was recorded.
    let repo = &f.config.projects[0].repo;
    fs::write(repo.join("result.txt"), "implemented\n").unwrap();
    git(repo, &["add", "result.txt"]);
    git(repo, &["commit", "-m", "AppFlowy story: story-0"]);
    let previous = git(repo, &["rev-parse", "HEAD"]);
    f.status("p0", "In Progress");
    let (token, job) = f.start();
    let completed = f.wait(|| f.board()["p0"]["status"] == "Done").await;
    f.stop(token, job).await;
    assert!(completed, "events: {:?}", f.events());
    assert_eq!(git(repo, &["rev-parse", "HEAD"]), previous);
    assert_eq!(
        git(&f.temp.path().join("remote0.git"), &["rev-parse", "main"]),
        previous
    );
    assert!(
        f.events()
            .iter()
            .any(|e| e["kind"] == "worker_start" && e["recovery"] == true)
    );
}

#[tokio::test]
async fn worker_timeout_is_reported_as_blocked_and_not_retried_automatically() {
    let mut f = Fixture::new(&[Mode::Master]);
    f.config.orchestrator.worker_timeout_seconds = 1;
    f.touch("hang");
    let (token, job) = f.start();
    let blocked = f.wait(|| f.board()["p0"]["status"] == "Blocked").await;
    f.stop(token, job).await;
    assert!(blocked, "events: {:?}", f.events());
    assert!(
        f.board()["p0"]["result"]["summary"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| e["kind"] == "worker_start")
            .count(),
        1
    );
}

#[tokio::test]
async fn shutdown_leaves_in_progress_and_next_process_recovers_it() {
    let f = Fixture::new(&[Mode::Pr]);
    f.touch("hang");
    let (token, job) = f.start();
    let started = f
        .wait(|| f.events().iter().any(|e| e["kind"] == "worker_start"))
        .await;
    f.stop(token, job).await;
    assert!(started);
    assert_eq!(f.board()["p0"]["status"], "In Progress");
    fs::remove_file(f.temp.path().join("hang")).unwrap();
    let (token, job) = f.start();
    let completed = f.wait(|| f.board()["p0"]["status"] == "Review").await;
    f.stop(token, job).await;
    assert!(completed, "events: {:?}", f.events());
    let events = f.events();
    let starts: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "worker_start")
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1]["recovery"], true);
}

#[tokio::test]
async fn ignored_states_do_not_start_workers() {
    let f = Fixture::new(&[Mode::Master, Mode::Pr]);
    f.status("p0", "Backlog");
    f.status("p1", "Cancelled");
    let (token, job) = f.start();
    let polled = f
        .wait(|| {
            f.events()
                .iter()
                .filter(|e| e["kind"] == "coordinator")
                .count()
                >= 2
        })
        .await;
    f.stop(token, job).await;
    assert!(polled);
    assert!(!f.events().iter().any(|e| e["kind"] == "worker_start"));
}

#[tokio::test]
async fn repository_lock_prevents_a_second_daemon_even_with_another_runtime_directory() {
    let f = Fixture::new(&[Mode::Master]);
    let _lock =
        maistro::engine::Lock::acquire(&f.config.projects[0].repo.join(".git/maistro.lock"))
            .unwrap();
    let error = Engine::new(f.config.clone())
        .run(CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("another maistro process"));
    assert!(f.events().is_empty());
}

#[tokio::test]
async fn linked_worktrees_cannot_be_configured_as_parallel_projects() {
    let mut f = Fixture::new(&[Mode::Master, Mode::Pr]);
    let linked = f.temp.path().join("linked");
    git(
        &f.config.projects[0].repo,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    f.config.projects[1].repo = linked;
    let error = Engine::new(f.config.clone()).validate().await.unwrap_err();
    assert!(error.to_string().contains("cannot share a Git repository"));
    assert!(f.events().is_empty());
}
