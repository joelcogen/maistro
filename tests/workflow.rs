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
        let mut board = serde_json::Map::new();
        let mut projects = vec![];
        for (i, mode) in modes.iter().enumerate() {
            let id = format!("p{i}");
            let repo = root.join(&id);
            fs::create_dir(&repo).unwrap();
            fs::write(repo.join("maistro-worker.md"), "Run fixture tests.").unwrap();
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
                worker: None,
                worker_effort: None,
                id,
                repo,
                appflowy_page: Some(format!("board-{i}")),
                appflowy_url: None,
                shortcut_url: None,
                shortcut_workflow: None,
                shortcut_workflow_id: None,
                shortcut_workspace: None,
                shortcut_token_file: None,
                mcp_server: None,
                mode: *mode,
                base_branch: "main".into(),
                remote: "origin".into(),
                enabled: true,
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
                ..Default::default()
            },
            orchestrator: Orchestrator {
                runtime_dir: root.join("runtime"),
                poll_interval_seconds: 1,
                coordinator_timeout_seconds: 10,
                worker_timeout_seconds: 15,
                ..Default::default()
            },
            projects,
        };
        Self { temp, config }
    }
    fn shortcut(&mut self, index: usize, server: &str) {
        let project = &mut self.config.projects[index];
        project.appflowy_page = None;
        project.shortcut_url =
            Some("https://app.shortcut.com/fixture/settings/workflows/42".into());
        project.mcp_server = Some(server.into());
        let path = self.config.agents.codex_home.join("config.toml");
        let mut config = fs::read_to_string(&path).unwrap();
        // Must never be contacted by Rust: only the simulated Codex uses OAuth here.
        config.push_str(&format!(
            "\n[mcp_servers.{server}]\nurl='https://mcp.shortcut.invalid/mcp'\n"
        ));
        fs::write(path, config).unwrap();
        let mut board = self.board();
        board[&format!("p{index}")]["id"] = json!((123 + index).to_string());
        fs::write(
            self.temp.path().join("board.json"),
            serde_json::to_vec(&board).unwrap(),
        )
        .unwrap();
    }
    fn claude(&mut self) {
        self.config.agents.provider = maistro::config::Provider::Claude;
        let executable = self.temp.path().join("fake-claude");
        fs::copy(&self.config.agents.codex, &executable).unwrap();
        self.config.agents.claude = executable;
        self.config.agents.claude_mcp_config = self.temp.path().join("claude.json");
        let mut connections = json!({"mcpServers": {
            "appflowy": {"command": "python3", "args": [self.temp.path().join("mcp.py"), self.temp.path()]},
            "utility": {"command":"fixture-utility"}
        }});
        for project in &self.config.projects {
            if project.is_shortcut() {
                connections["mcpServers"][project.mcp_server(&self.config.agents)] =
                    json!({"type":"http", "url":"https://mcp.shortcut.invalid/mcp"});
            }
        }
        fs::write(
            &self.config.agents.claude_mcp_config,
            serde_json::to_vec(&connections).unwrap(),
        )
        .unwrap();
        // Claude mode must not require a Codex installation or login/config.
        fs::remove_dir_all(&self.config.agents.codex_home).unwrap();
        self.config.agents.codex = "/nonexistent-codex".into();
    }
    fn workflow_selector(&mut self, index: usize, by_id: bool) {
        let project = &mut self.config.projects[index];
        project.shortcut_url = None;
        if by_id {
            project.shortcut_workflow_id = Some(500005096);
        } else {
            project.shortcut_workflow = Some("Agents".into());
        }
    }
    fn models(&mut self, coordinator: &str, worker: &str) {
        self.config.agents.coordinator = Some(coordinator.into());
        self.config.agents.worker = Some(worker.into());
        fs::write(
            self.temp.path().join("models.json"),
            serde_json::to_vec(&json!([coordinator, worker])).unwrap(),
        )
        .unwrap();
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

fn write_dream(project: &Project, name: &str, instructions: &str) {
    let dir = project.repo.join("dreams");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), instructions).unwrap();
}

#[tokio::test]
async fn daily_dreams_require_complete_empty_queues_and_push_changes_in_order() {
    for claude in [false, true] {
        let mut f = Fixture::new(&[Mode::Master, Mode::Pr]);
        if claude {
            f.claude();
        }
        f.config.orchestrator.dream_time = "00:00".into();
        let pr = &f.config.projects[1].repo;
        git(pr, &["switch", "-c", "dreams"]);
        fs::write(pr.join("old-dream.txt"), "existing dream work").unwrap();
        git(pr, &["add", "old-dream.txt"]);
        git(pr, &["commit", "-m", "An earlier dream"]);
        git(pr, &["push", "origin", "dreams"]);
        git(pr, &["switch", "main"]);
        fs::write(pr.join("updated-base.txt"), "updated base").unwrap();
        git(pr, &["add", "updated-base.txt"]);
        git(pr, &["commit", "-m", "Update base"]);
        git(pr, &["push", "origin", "main"]);
        for project in &f.config.projects {
            write_dream(project, "02-second.md", "run after first");
            write_dream(project, "01-first.md", "run first");
            write_dream(project, "03-noop.md", "no changes");
            f.touch(&format!("hold-story-{}", project.id));
        }
        f.status("p1", "In Progress");
        let (stop, job) = f.start();
        let read_count = |id: &str, state: &str| {
            f.events().iter().any(|e| {
                e["observations"].as_array().is_some_and(|observations| {
                    observations
                        .iter()
                        .any(|o| o["project_id"] == id && o["counts"][state] == 1)
                })
            })
        };
        assert!(
            f.wait(|| read_count("p0", "ready") && read_count("p1", "in_progress"))
                .await
        );
        f.touch("missing-observations");
        f.status("p0", "Backlog");
        f.status("p1", "Review");
        assert!(
            f.wait(|| f
                .events()
                .iter()
                .filter(|e| e["observations"] == json!([]))
                .count()
                >= 2)
                .await
        );
        assert!(!f.events().iter().any(|e| e["kind"] == "dream_start"));
        fs::remove_file(f.temp.path().join("missing-observations")).unwrap();

        assert!(
            f.wait(|| f
                .events()
                .iter()
                .filter(|e| e["kind"] == "dream_finish")
                .count()
                == 6)
                .await
        );
        for (i, project) in f.config.projects.iter().enumerate() {
            let names: Vec<_> = f
                .events()
                .into_iter()
                .filter(|e| e["kind"] == "dream_start" && e["project"] == project.id)
                .map(|e| e["name"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(names, ["01-first.md", "02-second.md", "03-noop.md"]);
            let remote = f.temp.path().join(format!("remote{i}.git"));
            let branch = if project.mode == Mode::Master {
                "main"
            } else {
                "dreams"
            };
            assert_eq!(
                git(&remote, &["show", &format!("{branch}:01-first.txt")]),
                "run first"
            );
            assert_eq!(
                git(&remote, &["show", &format!("{branch}:02-second.txt")]),
                "run after first"
            );
            assert_eq!(
                git(&remote, &["log", "-2", "--format=%s", branch]),
                "Dream: 02-second.md\nDream: 01-first.md"
            );
            if project.mode == Mode::Pr {
                git(&remote, &["merge-base", "--is-ancestor", "main", branch]);
                assert_eq!(
                    git(&remote, &["show", "dreams:old-dream.txt"]),
                    "existing dream work"
                );
            }
        }
        f.stop(stop, job).await;
    }
}

#[tokio::test]
async fn dream_phases_open_or_reuse_one_pr_only_when_needed() {
    let f = Fixture::new(&[Mode::Pr, Mode::Pr, Mode::Pr, Mode::Master, Mode::Pr]);
    let root = f.temp.path();
    let tools = root.join("tools");
    fs::create_dir(&tools).unwrap();
    for (name, script) in [
        ("gh", include_str!("fixtures/dream-gh.py")),
        (
            "git",
            r#"#!/bin/sh
if [ "$*" = 'remote get-url --push --all -- delivery' ]; then
    printf 'https://github.com/fixture/%s.git\n' "${PWD##*/}"
    exit
fi
PATH=${PATH#*:}
export PATH
exec git "$@"
"#,
        ),
    ] {
        let path = tools.join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut config = String::from(
        "[orchestrator]\ndream_time='00:00'\nruntime_dir='runtime'\npoll_interval_seconds=1\n\
         [agents]\ncodex='./fake-codex'\ncodex_home='codex-home'\ngithub_token_file='github-token'\n",
    );
    for (i, project) in f.config.projects.iter().enumerate() {
        git(&project.repo, &["remote", "rename", "origin", "delivery"]);
        git(&project.repo, &["branch", "-m", "release/main"]);
        git(&project.repo, &["push", "delivery", "release/main"]);
        f.status(&project.id, "Backlog");
        if i != 2 {
            write_dream(project, "01-first.md", "run first");
            write_dream(project, "02-second.md", "run after first");
        }
        write_dream(project, "03-noop.md", "no changes");
        let mode = if project.mode == Mode::Pr {
            "pr"
        } else {
            "master"
        };
        config.push_str(&format!(
            "\n[[projects]]\nid='{id}'\nrepo='{id}'\nappflowy_page='board-{i}'\n\
             mode='{mode}'\nremote='delivery'\nbase_branch='release/main'\n",
            id = project.id,
        ));
    }
    let config_path = root.join("config.toml");
    fs::write(&config_path, config).unwrap();
    let path = std::env::join_paths(
        std::iter::once(tools).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let log = root.join("daemon.log");
    let mut daemon = tokio::process::Command::new(env!("CARGO_BIN_EXE_maistro"))
        .args(["run", "--config"])
        .arg(config_path)
        .env("PATH", path)
        .env("RUST_LOG", "info")
        .stderr(fs::File::create(&log).unwrap())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let finished = f
        .wait(|| {
            fs::read_to_string(&log)
                .unwrap()
                .matches("dream phase finished")
                .count()
                == 5
        })
        .await;
    let pid = daemon.id().unwrap() as i32;
    unsafe { libc::kill(pid, libc::SIGTERM) };
    assert!(
        tokio::time::timeout(Duration::from_secs(10), daemon.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    let logs = fs::read_to_string(log).unwrap();
    assert!(finished, "{logs}");
    assert_eq!(logs.matches("completed=true").count(), 5, "{logs}");
    assert_eq!(
        logs.matches("dream pull request ready").count(),
        2,
        "{logs}"
    );
    let events = f.events();
    for (id, expected) in [
        ("p0", &["GET", "GET", "POST"][..]),
        ("p1", &["GET"][..]),
        ("p2", &[][..]),
        ("p3", &[][..]),
        ("p4", &["GET", "GET"][..]),
    ] {
        let methods: Vec<_> = events
            .iter()
            .filter(|event| event["kind"] == "dream_github" && event["project"] == id)
            .map(|event| event["method"].as_str().unwrap())
            .collect();
        assert_eq!(methods, expected, "{id}");
    }
}

#[tokio::test]
async fn dream_phase_holds_its_projects_worker_slot_until_it_finishes() {
    let mut f = Fixture::new(&[Mode::Master, Mode::Master]);
    f.config.orchestrator.dream_time = "00:00".into();
    write_dream(&f.config.projects[0], "01.md", "no changes");
    f.status("p0", "Backlog");
    f.touch("hold-dream-p0");

    let (stop, job) = f.start();
    assert!(
        f.wait(|| f.events().iter().any(|e| e["kind"] == "dream_start"))
            .await
    );
    f.status("p0", "Ready");
    assert!(f.wait(|| f.board()["p1"]["status"] == "Done").await);
    assert_eq!(f.board()["p0"]["status"], "Ready");
    assert!(
        !f.events()
            .iter()
            .any(|e| e["kind"] == "worker_start" && e["project"] == "p0")
    );

    fs::remove_file(f.temp.path().join("hold-dream-p0")).unwrap();
    assert!(f.wait(|| f.board()["p0"]["status"] == "Done").await);
    f.stop(stop, job).await;
    let events = f.events();
    let dream = events
        .iter()
        .position(|e| e["kind"] == "dream_finish")
        .unwrap();
    let story = events
        .iter()
        .position(|e| e["kind"] == "worker_start" && e["project"] == "p0")
        .unwrap();
    assert!(dream < story);
}

#[tokio::test]
async fn dashboard_dream_checks_live_board_and_uses_the_daily_checkpoint() {
    let mut f = Fixture::new(&[Mode::Master]);
    f.enable_status();
    f.config.orchestrator.dream_time = "23:59".into();
    f.config.orchestrator.poll_interval_seconds = 30;
    f.touch("hold-story-p0");
    f.touch("hold-dream-p0");
    let (stop, job) = f.start();
    assert!(
        f.wait(|| f.events().iter().any(|e| e["kind"] == "coordinator_read"))
            .await
    );

    // Add a file after the first scheduler tick, so only the dashboard can start it during this test.
    write_dream(&f.config.projects[0], "manual.md", "no changes");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let request = || async {
        let response = client
            .post(format!(
                "http://127.0.0.1:{}/api/dream",
                f.config.status.port
            ))
            .header("Content-Type", "application/json")
            .body(r#"{"project_id":"p0"}"#)
            .send()
            .await
            .unwrap();
        (response.status().as_u16(), response.text().await.unwrap())
    };
    for status in ["Ready", "In Progress"] {
        f.status("p0", status);
        let (code, message) = request().await;
        assert_eq!(code, 409);
        assert!(message.contains("Ready or In Progress"));
    }
    f.status("p0", "Backlog");
    f.touch("missing-observations");
    let (code, message) = request().await;
    assert_eq!(code, 503);
    assert!(message.contains("complete board read"));
    assert!(!f.events().iter().any(|e| e["kind"] == "dream_start"));

    fs::remove_file(f.temp.path().join("missing-observations")).unwrap();
    assert_eq!(request().await.0, 202);
    assert!(
        f.wait(|| f.events().iter().any(|e| e["kind"] == "dream_start"))
            .await
    );
    assert_eq!(
        f.status_snapshot().await["projects"][0]["worker"]["state"],
        "dreaming"
    );
    assert_eq!(request().await.0, 409);

    let previous_reads = f
        .events()
        .iter()
        .filter(|e| e["kind"] == "coordinator_read")
        .count();
    fs::remove_file(f.temp.path().join("hold-dream-p0")).unwrap();
    assert!(
        f.wait(|| f
            .events()
            .iter()
            .filter(|e| e["kind"] == "coordinator_read")
            .count()
            > previous_reads)
            .await
    );
    assert_eq!(
        f.status_snapshot().await["projects"][0]["worker"]["state"],
        "idle"
    );
    let (code, message) = request().await;
    assert_eq!(code, 409);
    assert!(message.contains("Only one phase can start per day"));
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| e["kind"] == "dream_start")
            .count(),
        1
    );
    f.stop(stop, job).await;
}

#[tokio::test]
async fn workers_reload_rotated_tokens_without_putting_them_in_logs() {
    let f = Fixture::new(&[Mode::Master, Mode::Pr]);
    let codex = maistro::agent::AgentRunner::new(f.config.clone());
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
}

#[tokio::test]
async fn completion_prompts_route_frozen_modes_to_exact_statuses_for_both_providers() {
    use maistro::model::{Completion, Outcome, Report, Snapshot, StoryRef};

    for claude in [false, true] {
        // Project defaults deliberately disagree with the dispatched modes.
        let mut f = Fixture::new(&[Mode::Pr, Mode::Master, Mode::Pr, Mode::Master]);
        f.shortcut(1, "shortcut");
        f.shortcut(3, "shortcut-other");
        if claude {
            f.claude();
        }
        let mut board = f.board();
        let mut snapshot = Snapshot::default();
        let cases = [
            (Mode::Master, Outcome::Completed, "Shipped"),
            (Mode::Pr, Outcome::Completed, "Awaiting approval"),
            (Mode::Master, Outcome::Blocked, "Needs help"),
            (Mode::Pr, Outcome::Blocked, "Needs help"),
        ];
        for (project, (mode, outcome, _)) in f.config.projects.iter_mut().zip(cases) {
            project.statuses.done = "Shipped".into();
            project.statuses.review = Some("Awaiting approval".into());
            project.statuses.blocked = "Needs help".into();
            board[&project.id]["status"] = json!("In Progress");
            snapshot.pending.push(Completion {
                id: format!("result-{}", project.id),
                story: StoryRef {
                    project_id: project.id.clone(),
                    story_id: board[&project.id]["id"].as_str().unwrap().into(),
                },
                elapsed_seconds: 0,
                mode,
                report: Report {
                    outcome,
                    // Incidental PR/review references must not control routing.
                    summary: "Implemented review prompts; historical PR linked".into(),
                    tests: vec!["fixture checks passed".into()],
                    commit: Some("abc123".into()),
                    pr_url: Some("https://example.test/pull/1".into()),
                },
            });
        }
        fs::write(
            f.temp.path().join("board.json"),
            serde_json::to_vec(&board).unwrap(),
        )
        .unwrap();
        let runner = maistro::agent::AgentRunner::new(f.config.clone());
        let result = runner
            .coordinate(
                &snapshot,
                &f.config
                    .projects
                    .iter()
                    .map(|p| p.id.clone())
                    .collect::<Vec<_>>(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.acknowledged.len(), cases.len());
        assert!(result.assignments.is_empty());
        let events = f.events();
        let delivered = events.iter().find(|e| e["kind"] == "coordinator").unwrap();
        for (i, (_, _, expected)) in cases.iter().enumerate() {
            assert_eq!(
                delivered["snapshot"]["pending"][i]["target_status"],
                *expected
            );
            assert_eq!(f.board()[format!("p{i}")]["status"], *expected);
        }
    }
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
    assert_eq!(view["projects"][0]["observation"]["counts"]["ready"], 0);
    assert_eq!(
        view["projects"][0]["observation"]["counts"]["in_progress"],
        1
    );
    assert_eq!(view["overall"]["counts"]["ready"], 0);
    assert_eq!(view["overall"]["counts"]["in_progress"], 1);
    assert_eq!(view["overall"]["stale"], true);
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
async fn repository_lock_prevents_a_second_daemon_even_with_another_runtime_directory() {
    let f = Fixture::new(&[Mode::Master]);
    f.touch("luna-hold");
    let (token, job) = f.start();
    assert!(
        f.wait(|| f.events().iter().any(|e| e["kind"] == "coordinator_read"))
            .await
    );

    let mut other = f.config.clone();
    other.orchestrator.runtime_dir = f.temp.path().join("other-runtime");
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        Engine::new(other).run(CancellationToken::new()),
    )
    .await;
    f.stop(token, job).await;
    let error = result
        .expect("second daemon did not refuse the repository")
        .unwrap_err();
    assert!(error.to_string().contains("another maistro process"));
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
    f.shortcut(1, "shortcut");
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
        assert_eq!(view["overall"]["counts"]["ready"], 0);
        assert_eq!(view["overall"]["counts"]["in_progress"], 2);
        assert_eq!(view["overall"]["counts"]["backlog"], 1);
        for index in [0, 1] {
            assert_eq!(view["projects"][index]["observation"]["counts"]["ready"], 0);
            assert_eq!(
                view["projects"][index]["observation"]["counts"]["in_progress"],
                1
            );
            assert_eq!(view["projects"][index]["observation"]["stale"], false);
        }
        assert_eq!(
            view["projects"][0]["story"]["url"],
            "https://appflowy.example/story"
        );
        let json = view.to_string();
        for secret in [
            "github_pat_fixture_secret",
            "Complete fixture requirements",
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
    let mut f = Fixture::new(&[Mode::Master]);
    f.enable_status();
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
    let first_poll = f.status_snapshot().await["projects"][0]["polling"]["last_poll_at"]
        .as_u64()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(3200)).await;
    let latest_poll = f.status_snapshot().await["projects"][0]["polling"]["last_poll_at"]
        .as_u64()
        .unwrap();
    assert!(latest_poll > first_poll);
    f.stop(token, job).await;
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| e["kind"] == "coordinator")
            .count(),
        1
    );
}

#[tokio::test]
async fn mixed_sources_complete_with_per_project_servers_and_stable_markers() {
    let mut f = Fixture::new(&[Mode::Master, Mode::Pr]);
    f.shortcut(1, "shortcut_team");
    f.enable_status();
    f.touch("parallel");
    let (token, job) = f.start();
    let complete = f
        .wait(|| f.board()["p0"]["status"] == "Done" && f.board()["p1"]["status"] == "Review")
        .await;
    let view = f.status_snapshot().await;
    f.stop(token, job).await;
    assert!(complete, "events: {:?}", f.events());
    assert_eq!(
        view["projects"][1]["board_url"],
        "https://app.shortcut.com/fixture/settings/workflows/42"
    );
    assert_eq!(view["projects"][1]["polling"]["method"], "coordinator");
    assert!(view["projects"][1]["polling"]["last_poll_at"].is_null());
    let events = f.events();
    let starts: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "worker_start")
        .collect();
    assert_eq!(starts.len(), 2);
    let shortcut = starts.iter().find(|e| e["project"] == "p1").unwrap();
    assert_eq!(shortcut["git"]["story_marker"], "Shortcut story: 124");
    assert_eq!(shortcut["git"]["story_branch"], "maistro/p1/124");
    let remote = f.temp.path().join("remote1.git");
    assert_eq!(
        git(&remote, &["rev-parse", "maistro/p1/124"]),
        f.board()["p1"]["result"]["commit"]
    );
}

#[tokio::test]
async fn workflow_selectors_reach_both_coordinators() {
    for claude in [false, true] {
        for by_id in [false, true] {
            let mut f = Fixture::new(&[Mode::Master]);
            f.shortcut(0, "shortcut_team");
            f.workflow_selector(0, by_id);
            f.config.projects[0].statuses.ready = "Available".into();
            if claude {
                f.claude();
            }
            f.config.validate().unwrap();
            let runner = maistro::agent::AgentRunner::new(f.config.clone());
            runner
                .coordinate(
                    &Default::default(),
                    &["p0".into()],
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            let prompt = fs::read_to_string(f.temp.path().join("coordinator-prompt.txt")).unwrap();
            let projects: Value = serde_json::from_str(
                prompt
                    .split("\nPROJECTS=")
                    .nth(1)
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(projects[0]["source"], "Shortcut");
            assert_eq!(projects[0]["mcp_server"], "shortcut_team");
            assert!(projects[0]["shortcut_url"].is_null());
            assert_eq!(
                projects[0]["shortcut_workflow"],
                if by_id { Value::Null } else { json!("Agents") }
            );
            assert_eq!(
                projects[0]["shortcut_workflow_id"],
                if by_id { json!(500005096) } else { Value::Null }
            );
            assert_eq!(projects[0]["statuses"]["ready"], "Available");
        }
    }
}

#[tokio::test]
async fn shortcut_incomplete_read_cannot_dispatch_and_retries_until_recovery() {
    for claude in [false, true] {
        let mut f = Fixture::new(&[Mode::Master]);
        f.shortcut(0, "shortcut");
        f.workflow_selector(0, true);
        f.enable_status();
        if claude {
            f.claude();
        }
        // The simulated coordinator incorrectly returns an assignment together
        // with retry_projects, as if selecting from an early successful page.
        f.touch("luna-incomplete");
        let (token, job) = f.start();
        assert!(
            f.wait(|| f
                .events()
                .iter()
                .filter(|e| e["kind"] == "coordinator_read")
                .count()
                >= 2)
                .await
        );
        assert!(!f.events().iter().any(|e| e["kind"] == "worker_start"));
        let view = f.status_snapshot().await;
        assert!(view["projects"][0]["observation"]["counts"].is_null());
        assert_eq!(view["projects"][0]["observation"]["stale"], true);
        assert!(view["overall"]["counts"].is_null());
        fs::remove_file(f.temp.path().join("luna-incomplete")).unwrap();
        let completed = f.wait(|| f.board()["p0"]["status"] == "Done").await;
        f.stop(token, job).await;
        assert!(completed, "events: {:?}", f.events());
        let starts: Vec<_> = f
            .events()
            .into_iter()
            .filter(|e| e["kind"] == "worker_start")
            .collect();
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0]["recovery"], true);
    }
}

#[tokio::test]
async fn workflow_selectors_preserve_archive_cancellation_with_cancelled_disabled() {
    for claude in [false, true] {
        for by_id in [false, true] {
            let mut f = Fixture::new(&[Mode::Master]);
            f.shortcut(0, "shortcut");
            f.workflow_selector(0, by_id);
            f.config.projects[0].statuses.cancelled = None;
            if claude {
                f.claude();
            }
            f.config.validate().unwrap();
            f.touch("hang");
            let (token, job) = f.start();
            let started = f
                .wait(|| f.events().iter().any(|e| e["kind"] == "worker_start"))
                .await;
            // The simulated coordinator reports archive cancellation independently
            // of the story's workflow status, as the real coordinator must do.
            f.touch("archived");
            let stopped = f
                .wait(|| {
                    f.events()
                        .iter()
                        .any(|e| e["kind"] == "stopped" && e["model"] == f.config.agents.worker())
                })
                .await;
            f.stop(token, job).await;
            assert!(started && stopped, "events: {:?}", f.events());
            assert_eq!(f.board()["p0"]["status"], "In Progress");
            assert!(f.board()["p0"].get("result").is_none());
            assert_eq!(
                f.events()
                    .iter()
                    .filter(|e| e["kind"] == "worker_start")
                    .count(),
                1
            );
        }
    }
}

#[tokio::test]
async fn shortcut_only_periodic_reads_find_new_work_and_cancel_without_rust_mcp() {
    let mut f = Fixture::new(&[Mode::Master]);
    f.shortcut(0, "shortcut");
    f.workflow_selector(0, false);
    // Shortcut-only installations do not need an AppFlowy MCP entry.
    fs::write(
        f.config.agents.codex_home.join("config.toml"),
        "[mcp_servers.shortcut]\nurl='https://mcp.shortcut.invalid/mcp'\n",
    )
    .unwrap();
    f.status("p0", "Backlog");
    f.touch("hang");
    let (token, job) = f.start();
    let idle = f
        .wait(|| {
            f.events()
                .iter()
                .filter(|e| e["kind"] == "coordinator")
                .count()
                >= 2
        })
        .await;
    assert!(f.events().iter().all(|e| e["kind"] != "worker_start"));
    f.status("p0", "Ready");
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
    assert!(idle && started && stopped, "events: {:?}", f.events());
    assert_eq!(f.board()["p0"]["status"], "Cancelled");
    assert!(f.board()["p0"].get("result").is_none());
}

#[tokio::test]
async fn shortcut_recovers_pushed_commit_after_result_delivery_failure() {
    let mut f = Fixture::new(&[Mode::Master]);
    f.shortcut(0, "shortcut");
    f.workflow_selector(0, true);
    f.touch("outage");
    let (token, job) = f.start();
    let pending = f
        .wait(|| f.events().iter().filter(|e| e["kind"] == "outage").count() >= 2)
        .await;
    f.stop(token, job).await;
    assert!(pending, "events: {:?}", f.events());
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| e["kind"] == "worker_start")
            .count(),
        1
    );
    let remote = f.temp.path().join("remote0.git");
    let sha = git(&remote, &["rev-parse", "main"]);
    fs::remove_file(f.temp.path().join("outage")).unwrap();
    let (token, job) = f.start();
    let complete = f.wait(|| f.board()["p0"]["status"] == "Done").await;
    f.stop(token, job).await;
    assert!(complete, "events: {:?}", f.events());
    assert_eq!(git(&remote, &["rev-parse", "main"]), sha);
    assert_eq!(f.board()["p0"]["result"]["commit"], sha);
    assert!(
        f.events()
            .iter()
            .any(|e| e["kind"] == "worker_start" && e["recovery"] == true)
    );
}

#[tokio::test]
async fn effort_overrides_reach_each_role_and_logs_for_both_providers() {
    use tracing::instrument::WithSubscriber;
    // Keep a second dispatcher registered so tracing checks the test subscriber
    // even when a parallel test is first to reach an agent log callsite.
    let _fallback_dispatch = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    for claude in [false, true] {
        for (coordinator_effort, worker_effort) in [
            (None, None),
            (Some("low"), Some("high")),
            (Some("medium"), None),
            (None, Some(if claude { "max" } else { "ultra" })),
        ] {
            let mut f = Fixture::new(&[Mode::Master; 2]);
            if claude {
                f.claude();
            }
            f.config.agents.coordinator_effort = coordinator_effort.map(str::to_owned);
            f.config.agents.worker_effort = worker_effort.map(str::to_owned);
            fs::write(
                f.temp.path().join("inherited-effort.json"),
                serde_json::to_vec(&std::env::var("CLAUDE_CODE_EFFORT_LEVEL").ok()).unwrap(),
            )
            .unwrap();
            fs::write(
                f.temp.path().join("efforts.json"),
                serde_json::to_vec(&[coordinator_effort, worker_effort]).unwrap(),
            )
            .unwrap();
            let runner = maistro::agent::AgentRunner::new(f.config.clone());
            runner.validate_cli().await.unwrap();
            let log_path = f.temp.path().join("start-logs.jsonl");
            let subscriber = tracing_subscriber::fmt()
                .json()
                .with_writer(fs::File::create(&log_path).unwrap())
                .finish();
            async {
                let cancel = CancellationToken::new();
                let result = runner
                    .coordinate(&Default::default(), &["p0".into(), "p1".into()], &cancel)
                    .await
                    .unwrap();
                assert_eq!(result.assignments.len(), 2);
                for assignment in result.assignments {
                    let project = f
                        .config
                        .projects
                        .iter()
                        .find(|p| p.id == assignment.project_id)
                        .unwrap();
                    runner.work(project, &assignment, &cancel).await.unwrap();
                }
            }
            .with_subscriber(subscriber)
            .await;
            let logs: Vec<Value> = fs::read_to_string(log_path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .filter(|event| event["fields"]["message"] == "agent starting")
                .collect();
            assert_eq!(logs.len(), 3);
            for (log, (role, effort)) in logs.iter().zip([
                ("coordinator", coordinator_effort),
                ("p0", worker_effort),
                ("p1", worker_effort),
            ]) {
                assert_eq!(log["fields"]["role"], role);
                assert_eq!(
                    log["fields"]["configured_effort"],
                    effort.unwrap_or("inherited/default")
                );
            }
            assert_eq!(
                f.events()
                    .iter()
                    .filter(|e| e["kind"] == "worker_start" && e["effort"] == json!(worker_effort))
                    .count(),
                2
            );
        }
    }
}

#[tokio::test]
async fn claude_requires_effort_flag_only_when_configured() {
    let mut f = Fixture::new(&[Mode::Master]);
    f.claude();
    f.touch("no-effort-flag");
    maistro::agent::AgentRunner::new(f.config.clone())
        .validate_cli()
        .await
        .unwrap();
    for (coordinator_effort, worker_effort) in [(Some("low"), None), (None, Some("high"))] {
        f.config.agents.coordinator_effort = coordinator_effort.map(str::to_owned);
        f.config.agents.worker_effort = worker_effort.map(str::to_owned);
        let error = maistro::agent::AgentRunner::new(f.config.clone())
            .validate_cli()
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("installed Claude Code lacks --effort")
        );
    }
}

#[tokio::test]
async fn claude_mixed_sources_models_git_modes_and_recovery() {
    let mut f = Fixture::new(&[Mode::Master, Mode::Pr]);
    f.shortcut(1, "shortcut_team");
    f.workflow_selector(1, false);
    f.claude();
    f.models("custom-haiku", "custom-opus");
    f.touch("parallel");
    f.touch("outage");
    let (token, job) = f.start();
    let pending = f
        .wait(|| f.events().iter().filter(|e| e["kind"] == "outage").count() >= 2)
        .await;
    f.stop(token, job).await;
    assert!(pending, "events: {:?}", f.events());
    let sha = git(&f.temp.path().join("remote0.git"), &["rev-parse", "main"]);
    fs::remove_file(f.temp.path().join("outage")).unwrap();
    let (token, job) = f.start();
    let complete = f
        .wait(|| f.board()["p0"]["status"] == "Done" && f.board()["p1"]["status"] == "Review")
        .await;
    f.stop(token, job).await;
    assert!(complete, "events: {:?}", f.events());
    assert_eq!(
        git(&f.temp.path().join("remote0.git"), &["rev-parse", "main"]),
        sha
    );
    assert_eq!(f.board()["p0"]["result"]["commit"], sha);
    assert!(f.events().iter().any(|e| e["kind"] == "worker_start"
        && e["recovery"] == true
        && e["provider"] == "claude"));
    assert_eq!(
        f.board()["p1"]["result"]["pr_url"],
        "https://example.test/pull/p1"
    );
}

#[tokio::test]
async fn claude_cancellation_and_timeout_stop_workers() {
    for timeout in [false, true] {
        let mut f = Fixture::new(&[Mode::Master]);
        f.claude();
        if timeout {
            f.config.orchestrator.worker_timeout_seconds = 2;
        }
        f.touch("hang");
        let (token, job) = f.start();
        let started = f
            .wait(|| f.events().iter().any(|e| e["kind"] == "worker_start"))
            .await;
        if !timeout {
            f.status("p0", "Cancelled");
        }
        let stopped = f
            .wait(|| {
                f.events()
                    .iter()
                    .any(|e| e["kind"] == "stopped" && e["model"] == "opus")
            })
            .await;
        let delivered = if timeout {
            f.wait(|| f.board()["p0"]["status"] == "Blocked").await
        } else {
            true
        };
        f.stop(token, job).await;
        assert!(started && stopped && delivered, "events: {:?}", f.events());
        assert_eq!(
            f.board()["p0"]["status"],
            if timeout { "Blocked" } else { "Cancelled" }
        );
    }
}

#[tokio::test]
async fn claude_validation_rejects_logged_out_cli_and_failed_results() {
    let mut f = Fixture::new(&[Mode::Master]);
    f.claude();
    f.touch("logged-out");
    assert!(
        Engine::new(f.config.clone())
            .validate()
            .await
            .unwrap_err()
            .to_string()
            .contains("not logged in")
    );
    fs::remove_file(f.temp.path().join("logged-out")).unwrap();
    Engine::new(f.config.clone()).validate().await.unwrap();
    f.touch("bad-result");
    let runner = maistro::agent::AgentRunner::new(f.config.clone());
    assert!(
        runner
            .coordinate(
                &Default::default(),
                &["p0".into()],
                &CancellationToken::new()
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn repository_prompts_are_optional_role_specific_scoped_and_reloaded_for_both_providers() {
    for claude in [false, true] {
        let mut f = Fixture::new(&[Mode::Master; 4]);
        if claude {
            f.claude();
        }
        f.config.projects[2].enabled = false;
        for project in &f.config.projects {
            fs::remove_file(project.repo.join("maistro-worker.md")).unwrap();
        }
        fs::write(
            f.config.projects[1].repo.join("maistro-coordinator.md"),
            "Only coordinate p1.",
        )
        .unwrap();
        fs::write(
            f.config.projects[1].repo.join("maistro-worker.md"),
            "Only implement p1.",
        )
        .unwrap();
        // Disabled and unselected projects must not even have their files read.
        for index in [2, 3] {
            fs::create_dir(f.config.projects[index].repo.join("maistro-coordinator.md")).unwrap();
        }
        let runner = maistro::agent::AgentRunner::new(f.config.clone());
        let cancel = CancellationToken::new();
        let project = &f.config.projects[0];
        for (coordinator, worker) in [
            (None, None),
            (Some("Coordinate p0 only.\nGather context."), None),
            (
                Some("Revised p0 coordination."),
                Some("Implement p0 only.\nRun tests."),
            ),
            (None, Some("Revised p0 implementation.")),
            (Some(""), Some("")),
        ] {
            for (filename, contents) in [
                ("maistro-coordinator.md", coordinator),
                ("maistro-worker.md", worker),
            ] {
                let path = project.repo.join(filename);
                if let Some(contents) = contents {
                    fs::write(path, contents).unwrap();
                } else if path.exists() {
                    fs::remove_file(path).unwrap();
                }
            }
            let result = runner
                .coordinate(
                    &Default::default(),
                    &["p0".into(), "p1".into(), "p2".into()],
                    &cancel,
                )
                .await
                .unwrap();
            let assignment = result
                .assignments
                .iter()
                .find(|a| a.project_id == "p0")
                .unwrap();
            runner.work(project, assignment, &cancel).await.unwrap();
            let coordination =
                fs::read_to_string(f.temp.path().join("coordinator-prompt.txt")).unwrap();
            let implementation =
                fs::read_to_string(f.temp.path().join("p0-worker-prompt.txt")).unwrap();
            assert!(coordination.contains("Only coordinate p1."));
            assert!(coordination.contains("apply only to project \"p1\""));
            assert!(!coordination.contains("Only implement p1."));
            assert!(!implementation.contains("Only coordinate p1."));
            assert!(!implementation.contains("Only implement p1."));
            assert!(!implementation.contains("PROJECT COORDINATOR INSTRUCTIONS"));
            let custom = coordinator.unwrap_or_default();
            assert_eq!(
                coordination.contains("apply only to project \"p0\""),
                !custom.is_empty()
            );
            if !custom.is_empty() {
                assert!(coordination.contains(custom));
                assert!(!implementation.contains(custom));
            }
            let custom = worker.unwrap_or_default();
            assert_eq!(
                implementation.contains("PROJECT INSTRUCTIONS:"),
                !custom.is_empty()
            );
            if !custom.is_empty() {
                assert!(implementation.contains(custom));
                assert!(!coordination.contains(custom));
            }
        }
        // Unreadable or invalid files fail before starting either agent.
        for filename in ["maistro-coordinator.md", "maistro-worker.md"] {
            let path = project.repo.join(filename);
            fs::remove_file(&path).unwrap();
            for directory in [false, true] {
                if directory {
                    fs::create_dir(&path).unwrap();
                } else {
                    fs::write(&path, [0xff]).unwrap();
                }
                let before = f.events();
                let error = if filename == "maistro-coordinator.md" {
                    runner
                        .coordinate(&Default::default(), &["p0".into()], &cancel)
                        .await
                        .unwrap_err()
                } else {
                    let assignment = maistro::model::Assignment {
                        project_id: "p0".into(),
                        story_id: "story-0".into(),
                        title: "Fixture".into(),
                        story_url: None,
                        context: "Requirements".into(),
                        recovery: false,
                        pr: None,
                    };
                    runner
                        .work(project, &assignment, &cancel)
                        .await
                        .unwrap_err()
                };
                assert!(error.to_string().contains("p0: read prompt file"));
                assert!(error.to_string().contains(path.to_str().unwrap()));
                assert_eq!(f.events(), before);
                if directory {
                    fs::remove_dir(&path).unwrap();
                } else {
                    fs::remove_file(&path).unwrap();
                }
            }
        }
    }
}

#[tokio::test]
async fn shortcut_master_delivers_done_with_optional_states_disabled() {
    let mut f = Fixture::new(&[Mode::Master]);
    f.shortcut(0, "shortcut");
    f.config.projects[0].statuses.review = None;
    f.config.projects[0].statuses.cancelled = None;
    f.config.validate().unwrap();
    let (token, job) = f.start();
    let completed = f.wait(|| f.board()["p0"]["status"] == "Done").await;
    f.stop(token, job).await;
    assert!(completed, "events: {:?}", f.events());
    assert_eq!(
        f.board()["p0"]["result"]["commit"],
        git(&f.temp.path().join("remote0.git"), &["rev-parse", "main"])
    );
}

#[tokio::test]
async fn failed_shortcut_api_read_isolated_from_other_projects_without_cli_fallback() {
    use maistro::{
        agent::AgentRunner,
        model::{Completion, Report, Snapshot, StoryRef},
    };
    let mut f = Fixture::new(&[Mode::Master, Mode::Master]);
    f.shortcut(1, "shortcut");
    f.config.projects[1].shortcut_workspace = Some("team".into());
    f.config.projects[1].shortcut_token_file = Some(f.temp.path().join("missing-token"));
    let pending = Completion {
        id: "pending-api".into(),
        story: StoryRef {
            project_id: "p1".into(),
            story_id: "124".into(),
        },
        elapsed_seconds: 0,
        mode: Mode::Master,
        report: Report::blocked("Still needs delivery".into()),
    };
    let snapshot = Snapshot {
        active: vec![],
        pending: vec![pending],
    };
    let result = AgentRunner::new(f.config.clone())
        .coordinate(
            &snapshot,
            &["p0".into(), "p1".into()],
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.retry_projects, vec!["p1"]);
    assert!(result.acknowledged.is_empty());
    assert!(result.cancel.is_empty());
    assert_eq!(result.assignments.len(), 1);
    assert_eq!(result.assignments[0].project_id, "p0");
    assert_eq!(f.board()["p1"]["status"], "Ready");
    let events_before = f.events().len();
    let result = AgentRunner::new(f.config.clone())
        .coordinate(&snapshot, &["p1".into()], &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.retry_projects, vec!["p1"]);
    assert!(result.assignments.is_empty());
    assert_eq!(
        f.events().len(),
        events_before,
        "No CLI/model call for failed API-only pass"
    );
}

#[tokio::test]
async fn project_worker_settings_reach_both_providers() {
    for claude in [false, true] {
        let mut f = Fixture::new(&[Mode::Master; 3]);
        if claude {
            f.claude();
        }
        f.models("custom-coordinator", "custom-worker");
        f.config.agents.worker_effort = Some("high".into());
        f.config.projects[0].worker = Some("custom-project".into());
        f.config.projects[0].worker_effort = Some("low".into());
        f.config.projects[1].worker = Some("custom-other".into());
        f.config.projects[2].worker_effort = Some("medium".into());
        let expected = json!({"p0": ["custom-project", "low"], "p1": ["custom-other", "high"], "p2": ["custom-worker", "medium"]});
        fs::write(
            f.temp.path().join("project-settings.json"),
            serde_json::to_vec(&expected).unwrap(),
        )
        .unwrap();
        let runner = maistro::agent::AgentRunner::new(f.config.clone());
        runner.validate_cli().await.unwrap();
        let cancel = CancellationToken::new();
        let result = runner
            .coordinate(
                &Default::default(),
                &["p0".into(), "p1".into(), "p2".into()],
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(result.assignments.len(), 3);
        for assignment in result.assignments {
            let project = f
                .config
                .projects
                .iter()
                .find(|p| p.id == assignment.project_id)
                .unwrap();
            runner.work(project, &assignment, &cancel).await.unwrap();
        }
        let events = f.events();
        let workers: Vec<_> = events
            .iter()
            .filter(|e| e["kind"] == "worker_start")
            .collect();
        assert_eq!(workers.len(), 3);
        for event in workers {
            let settings = &expected[event["project"].as_str().unwrap()];
            assert_eq!(event["model"], settings[0]);
            assert_eq!(event["effort"], settings[1]);
        }
        if claude {
            f.config.agents.worker_effort = None;
            f.touch("no-effort-flag");
            let error = maistro::agent::AgentRunner::new(f.config.clone())
                .validate_cli()
                .await
                .unwrap_err();
            assert!(error.to_string().contains("projects[].worker_effort"));
        }
    }
}

#[tokio::test]
async fn unresolved_appflowy_board_is_excluded_from_coordination() {
    let mut f = Fixture::new(&[Mode::Master, Mode::Master]);
    f.config.projects[1].appflowy_page = Some("missing-board".into());
    let runner = maistro::agent::AgentRunner::new(f.config.clone());
    let result = runner
        .coordinate(
            &Default::default(),
            &["p0".into(), "p1".into()],
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.retry_projects, vec!["p1"]);
    assert_eq!(result.assignments.len(), 1);
    assert_eq!(result.assignments[0].project_id, "p0");
    assert_eq!(f.board()["p1"]["status"], "Ready");
    let events = f.events().len();
    let result = runner
        .coordinate(
            &Default::default(),
            &["p1".into()],
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.retry_projects, vec!["p1"]);
    assert!(result.assignments.is_empty());
    assert_eq!(
        events,
        f.events().len(),
        "No model call for unresolved boards"
    );
}
