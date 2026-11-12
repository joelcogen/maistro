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
        let mcp = root.join("mcp.py");
        fs::write(&mcp, include_str!("fixtures/mcp.py")).unwrap();
        fs::write(
            home.join("config.toml"),
            format!(
                "[mcp_servers.appflowy]\ncommand = 'python3'\nargs = ['{}', '{}']\n",
                mcp.display(),
                root.display()
            ),
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
        let github_token_file = root.join("github-token");
        fs::write(&github_token_file, "github_pat_fixture_secret\n").unwrap();
        fs::set_permissions(&github_token_file, fs::Permissions::from_mode(0o600)).unwrap();
        let config = Config {
            status: Default::default(),
            agents: Agents {
                codex: executable,
                codex_home: home,
                mcp_server: "appflowy".into(),
                github_token_file,
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
    fn enable_status(&mut self) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        self.config.status.enabled = true;
        self.config.status.port = listener.local_addr().unwrap().port();
    }
    async fn status_snapshot(&self) -> Value {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", self.config.status.port))
            .await
            .unwrap();
        socket
            .write_all(b"GET /api/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));
        serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
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
async fn workers_reload_rotated_tokens_without_putting_them_in_logs() {
    let f = Fixture::new(&[Mode::Master, Mode::Pr]);
    let codex = maistro::codex::Codex::new(f.config.clone());
    let tokens = ["github_pat_first", "github_pat_rotated"];
    for token in tokens {
        fs::write(&f.config.agents.github_token_file, token).unwrap();
        for (i, project) in f.config.projects.iter().enumerate() {
            let assignment = maistro::model::Assignment {
                project_id: project.id.clone(),
                story_id: format!("story-{i}"),
                title: "Token test".into(),
                story_url: None,
                context: "Verify authentication".into(),
                recovery: false,
                pr: None,
            };
            codex
                .work(project, &assignment, &CancellationToken::new())
                .await
                .unwrap();
        }
    }
    for entry in fs::read_dir(f.config.orchestrator.runtime_dir.join("logs")).unwrap() {
        let content = fs::read_to_string(entry.unwrap().path()).unwrap();
        for token in tokens {
            assert!(!content.contains(token));
        }
    }
}

#[tokio::test]
async fn missing_token_fails_validation_before_starting_agents() {
    let f = Fixture::new(&[Mode::Master]);
    fs::remove_file(&f.config.agents.github_token_file).unwrap();
    let error = Engine::new(f.config.clone()).validate().await.unwrap_err();
    assert!(error.to_string().contains("run install.sh"));
    assert!(f.events().is_empty());
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
async fn master_story_pr_overrides_push_generated_or_requested_branch_and_finish_in_review() {
    for requested in [None, Some("feature/custom-delivery")] {
        let f = Fixture::new(&[Mode::Master]);
        let mut board = f.board();
        board["p0"]["pr"] = json!({"branch": requested});
        fs::write(
            f.temp.path().join("board.json"),
            serde_json::to_vec(&board).unwrap(),
        )
        .unwrap();
        let remote = f.temp.path().join("remote0.git");
        let base = git(&remote, &["rev-parse", "main"]);
        let (token, job) = f.start();
        let completed = f.wait(|| f.board()["p0"]["status"] == "Review").await;
        f.stop(token, job).await;
        assert!(completed, "events: {:?}", f.events());
        let branch = requested.unwrap_or("maistro/p0/story-0");
        let sha = git(&remote, &["rev-parse", branch]);
        assert_ne!(sha, base);
        assert_eq!(git(&remote, &["rev-parse", "main"]), base);
        assert_eq!(f.board()["p0"]["result"]["commit"], sha);
        assert!(f.board()["p0"]["result"]["pr_url"].is_string());
        let events = f.events();
        let pending = events
            .iter()
            .find_map(|e| e["snapshot"]["pending"].as_array().and_then(|p| p.first()))
            .unwrap();
        assert_eq!(pending["mode"], "pr");
    }
}

#[tokio::test]
async fn master_story_pr_override_recovers_pushed_work_without_duplicate_commit() {
    let f = Fixture::new(&[Mode::Master]);
    let mut board = f.board();
    board["p0"]["pr"] = json!({"branch":"feature/recover"});
    fs::write(
        f.temp.path().join("board.json"),
        serde_json::to_vec(&board).unwrap(),
    )
    .unwrap();
    f.touch("outage");
    let (token, job) = f.start();
    let pending = f
        .wait(|| f.events().iter().any(|e| e["kind"] == "outage"))
        .await;
    f.stop(token, job).await;
    assert!(pending, "events: {:?}", f.events());
    let remote = f.temp.path().join("remote0.git");
    let sha = git(&remote, &["rev-parse", "feature/recover"]);
    assert_eq!(f.board()["p0"]["status"], "In Progress");
    fs::remove_file(f.temp.path().join("outage")).unwrap();
    let (token, job) = f.start();
    let completed = f.wait(|| f.board()["p0"]["status"] == "Review").await;
    f.stop(token, job).await;
    assert!(completed, "events: {:?}", f.events());
    assert_eq!(git(&remote, &["rev-parse", "feature/recover"]), sha);
    assert_eq!(f.board()["p0"]["result"]["commit"], sha);
    let events = f.events();
    let starts: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "worker_start")
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1]["recovery"], true);
    assert_eq!(starts[1]["git"]["story_branch"], "feature/recover");
}

#[tokio::test]
async fn master_story_pr_override_cannot_complete_without_pr_url() {
    let f = Fixture::new(&[Mode::Master]);
    let mut board = f.board();
    board["p0"]["pr"] = json!({"branch":null});
    fs::write(
        f.temp.path().join("board.json"),
        serde_json::to_vec(&board).unwrap(),
    )
    .unwrap();
    f.touch("missing-pr-url");
    let (token, job) = f.start();
    let blocked = f.wait(|| f.board()["p0"]["status"] == "Blocked").await;
    f.stop(token, job).await;
    assert!(blocked, "events: {:?}", f.events());
    assert!(
        f.board()["p0"]["result"]["summary"]
            .as_str()
            .unwrap()
            .contains("completed PR report has no PR URL")
    );
}

#[tokio::test]
async fn pending_completion_survives_appflowy_outage_in_memory_without_redispatch() {
    let mut f = Fixture::new(&[Mode::Master]);
    f.enable_status();
    f.touch("outage");
    let (token, job) = f.start();
    let retried = f
        .wait(|| f.events().iter().filter(|e| e["kind"] == "outage").count() >= 2)
        .await;
    let view = f.status_snapshot().await;
    assert_eq!(view["projects"][0]["worker"]["state"], "pending_update");
    assert_eq!(view["projects"][0]["story"]["id"], "story-0");
    assert_eq!(view["projects"][0]["observation"]["stale"], true);
    assert_eq!(view["projects"][0]["observation"]["counts"]["ready"], 1);
    assert!(view["coordinator"]["last_error"].is_string());
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
            ["p0", "p1"].iter().all(|id| {
                f.events().iter().any(|e| {
                    e["kind"] == "coordinator"
                        && e["projects"]
                            .as_array()
                            .is_some_and(|p| p.contains(&json!(id)))
                })
            })
        })
        .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    f.stop(token, job).await;
    assert!(polled);
    // Independent startup observations may arrive in different passes; each
    // project must be coordinated once, with no repeated idle invocations.
    for id in ["p0", "p1"] {
        assert_eq!(
            f.events()
                .iter()
                .filter(|e| e["kind"] == "coordinator"
                    && e["projects"]
                        .as_array()
                        .is_some_and(|p| p.contains(&json!(id))))
                .count(),
            1
        );
    }
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

#[tokio::test]
async fn status_traffic_does_not_coordinate_or_dispatch_and_shows_parallel_idle_disabled_projects()
{
    let mut f = Fixture::new(&[Mode::Master, Mode::Pr, Mode::Master, Mode::Pr]);
    f.enable_status();
    f.config.orchestrator.poll_interval_seconds = 60;
    f.config.projects[2].enabled = false;
    f.status("p3", "Backlog");
    f.touch("hang");
    let (token, job) = f.start();
    let started = f
        .wait(|| {
            f.events()
                .iter()
                .filter(|e| e["kind"] == "worker_start")
                .count()
                == 2
        })
        .await;
    assert!(started, "events: {:?}", f.events());
    let events_before = f.events();
    let board_before = f.board();
    for _ in 0..20 {
        let view = f.status_snapshot().await;
        assert_eq!(view["projects"][0]["worker"]["state"], "active");
        assert_eq!(view["projects"][1]["worker"]["state"], "active");
        assert_eq!(view["projects"][2]["worker"]["state"], "disabled");
        assert_eq!(view["projects"][3]["worker"]["state"], "idle");
        assert_eq!(view["projects"][0]["mode"], "master");
        assert_eq!(view["projects"][1]["mode"], "pr");
        assert_eq!(view["overall"]["observed_projects"], 3);
        assert_eq!(view["overall"]["counts"]["ready"], 2);
        assert_eq!(view["overall"]["counts"]["backlog"], 1);
        assert_eq!(
            view["projects"][0]["story"]["url"],
            "https://appflowy.example/story"
        );
        let json = view.to_string();
        for secret in [
            "github_pat_fixture_secret",
            "Complete fixture requirements",
            "prompt_file",
            "codex_home",
        ] {
            assert!(!json.contains(secret));
        }
    }
    assert_eq!(events_before, f.events());
    assert_eq!(board_before, f.board());
    f.stop(token, job).await;
}

#[tokio::test]
async fn changes_during_luna_remain_dirty_and_only_one_pass_runs() {
    let mut f = Fixture::new(&[Mode::Master]);
    f.enable_status();
    f.status("p0", "Done");
    f.touch("luna-hold");
    let (token, job) = f.start();
    assert!(
        f.wait(|| f.events().iter().any(|e| e["kind"] == "coordinator_read"))
            .await
    );
    f.status("p0", "Backlog");
    tokio::time::sleep(Duration::from_millis(1400)).await;
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| e["kind"] == "coordinator")
            .count(),
        1
    );
    assert_eq!(
        f.status_snapshot().await["projects"][0]["polling"]["dirty"],
        true
    );
    fs::remove_file(f.temp.path().join("luna-hold")).unwrap();
    assert!(
        f.wait(|| f
            .events()
            .iter()
            .filter(|e| e["kind"] == "coordinator")
            .count()
            == 2)
            .await
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| e["kind"] == "coordinator")
            .count(),
        2
    );
    assert_eq!(
        f.status_snapshot().await["projects"][0]["polling"]["dirty"],
        false
    );
    f.stop(token, job).await;
}

#[tokio::test]
async fn failed_and_incomplete_luna_calls_retry_unchanged_boards_at_bounded_intervals() {
    for failure in ["luna-fail", "luna-incomplete"] {
        let f = Fixture::new(&[Mode::Master]);
        f.status("p0", "Backlog");
        f.touch(failure);
        let (token, job) = f.start();
        assert!(
            f.wait(|| f
                .events()
                .iter()
                .filter(|e| e["kind"] == "coordinator")
                .count()
                >= 2)
                .await
        );
        let before = f
            .events()
            .iter()
            .filter(|e| e["kind"] == "coordinator")
            .count();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            before,
            f.events()
                .iter()
                .filter(|e| e["kind"] == "coordinator")
                .count()
        );
        fs::remove_file(f.temp.path().join(failure)).unwrap();
        assert!(
            f.wait(|| f
                .events()
                .iter()
                .filter(|e| e["kind"] == "coordinator")
                .count()
                > before)
                .await
        );
        tokio::time::sleep(Duration::from_millis(1400)).await;
        let count = f
            .events()
            .iter()
            .filter(|e| e["kind"] == "coordinator")
            .count();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(
            count,
            f.events()
                .iter()
                .filter(|e| e["kind"] == "coordinator")
                .count()
        );
        f.stop(token, job).await;
    }
}

#[tokio::test]
async fn failing_poll_is_isolated_and_does_not_start_luna_until_valid_observation() {
    let mut f = Fixture::new(&[Mode::Master, Mode::Master]);
    f.enable_status();
    f.status("p0", "Backlog");
    f.status("p1", "Done");
    f.touch("poll-fail-p0");
    let (token, job) = f.start();
    assert!(
        f.wait(|| f.events().iter().any(|e| e["kind"] == "coordinator"))
            .await
    );
    tokio::time::sleep(Duration::from_millis(1400)).await;
    let events = f.events();
    let passes: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "coordinator")
        .collect();
    assert_eq!(passes.len(), 1);
    assert_eq!(passes[0]["projects"], json!(["p1"]));
    let view = f.status_snapshot().await;
    assert_eq!(view["projects"][0]["polling"]["dirty"], true);
    assert!(view["projects"][0]["polling"]["last_error"].is_string());
    assert!(view["projects"][0]["polling"]["last_change_at"].is_null());
    fs::remove_file(f.temp.path().join("poll-fail-p0")).unwrap();
    assert!(
        f.wait(|| f
            .events()
            .iter()
            .filter(|e| e["kind"] == "coordinator")
            .count()
            == 2)
            .await
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    f.touch("poll-incomplete-p0");
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| e["kind"] == "coordinator")
            .count(),
        2
    );
    let view = f.status_snapshot().await;
    assert_eq!(view["projects"][0]["polling"]["dirty"], false);
    assert!(view["projects"][0]["polling"]["last_error"].is_string());
    assert!(view["projects"][0]["polling"]["last_change_at"].is_number());
    f.stop(token, job).await;
}

#[tokio::test]
async fn result_acknowledgement_dispatches_queued_work_without_waiting_for_poll() {
    let mut f = Fixture::new(&[Mode::Master]);
    f.config.orchestrator.poll_interval_seconds = 60;
    f.touch("queued-ready");
    let (token, job) = f.start();
    assert!(
        f.wait(|| f
            .events()
            .iter()
            .filter(|e| e["kind"] == "worker_start")
            .count()
            == 2)
            .await
    );
    assert!(f.wait(|| f.board()["p0"]["status"] == "Done").await);
    f.stop(token, job).await;
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| e["kind"] == "worker_start")
            .count(),
        2
    );
}

#[tokio::test]
async fn pending_delivery_retries_even_when_luna_returns_without_acknowledgement() {
    let f = Fixture::new(&[Mode::Master]);
    f.touch("delivery-incomplete");
    let (token, job) = f.start();
    assert!(
        f.wait(|| f
            .events()
            .iter()
            .filter(|e| e["kind"] == "coordinator"
                && e["snapshot"]["pending"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty()))
            .count()
            >= 2)
            .await
    );
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| e["kind"] == "worker_start")
            .count(),
        1
    );
    fs::remove_file(f.temp.path().join("delivery-incomplete")).unwrap();
    assert!(
        f.wait(|| f
            .events()
            .iter()
            .filter(|e| e["kind"] == "coordinator" && e["snapshot"]["pending"] == json!([]))
            .count()
            >= 2)
            .await
    );
    f.stop(token, job).await;
}

#[tokio::test]
async fn slow_poll_does_not_block_other_projects_or_shutdown() {
    let mut f = Fixture::new(&[Mode::Master, Mode::Master]);
    f.config.orchestrator.coordinator_timeout_seconds = 2;
    f.status("p0", "Backlog");
    f.status("p1", "Backlog");
    f.touch("poll-slow-p0");
    let (token, job) = f.start();
    assert!(
        f.wait(|| f.events().iter().any(|e| e["kind"] == "coordinator"))
            .await
    );
    let passes: Vec<_> = f
        .events()
        .into_iter()
        .filter(|e| e["kind"] == "coordinator")
        .collect();
    assert_eq!(passes[0]["projects"], json!(["p1"]));
    f.stop(token, job).await;
}

#[tokio::test]
async fn empty_board_has_one_initial_coordination_and_no_repeated_idle_calls() {
    let f = Fixture::new(&[Mode::Master]);
    let mut board = f.board();
    board["p0"]["status"] = json!("Backlog");
    board["p0"]["rows"] = json!([]);
    fs::write(
        f.temp.path().join("board.json"),
        serde_json::to_vec(&board).unwrap(),
    )
    .unwrap();
    let (token, job) = f.start();
    assert!(
        f.wait(|| f.events().iter().any(|e| e["kind"] == "coordinator_read"))
            .await
    );
    tokio::time::sleep(Duration::from_millis(3200)).await;
    f.stop(token, job).await;
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| e["kind"] == "coordinator")
            .count(),
        1
    );
    let polls = fs::read_to_string(f.temp.path().join("poll-events")).unwrap();
    assert!(polls.lines().filter(|l| l.contains("get_page")).count() >= 6);
}
