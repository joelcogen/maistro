mod common;
use maistro::{
    engine::Singleton,
    model::Status,
    store::{State, Store},
};
#[test]
fn backlog_ready_claim_restart_deduplicate() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    db.sync_config(&c).unwrap();
    let p = &c.projects[0];
    let mut scan = common::scan(p, vec![common::story(Status::Backlog)]);
    assert_eq!(
        db.ingest(p, &scan, &c.orchestrator.worktree_root).unwrap(),
        0
    );
    scan.stories[0].status = Status::Ready;
    assert_eq!(
        db.ingest(p, &scan, &c.orchestrator.worktree_root).unwrap(),
        1
    );
    assert_eq!(
        db.ingest(p, &scan, &c.orchestrator.worktree_root).unwrap(),
        0
    );
    let mut r = db.runs().unwrap().remove(0);
    assert!(db.claim(&mut r, p, 1).unwrap());
    drop(db);
    let reopened = Store::open(&c.orchestrator.state_db).unwrap();
    assert_eq!(
        reopened
            .ingest(p, &scan, &c.orchestrator.worktree_root)
            .unwrap(),
        0
    );
    assert_eq!(reopened.runs().unwrap().len(), 1);
    assert_eq!(reopened.runs().unwrap()[0].state, State::Running);
}
#[test]
fn missing_dependencies_and_requirements_project_blocked() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    db.sync_config(&c).unwrap();
    let mut a = common::story(Status::Ready);
    a.dependencies.push(uuid::Uuid::new_v4().to_string());
    let mut b = common::story(Status::Ready);
    b.acceptance_criteria.clear();
    let scan = common::scan(&c.projects[0], vec![a, b]);
    db.ingest(&c.projects[0], &scan, &c.orchestrator.worktree_root)
        .unwrap();
    assert!(db.runs().unwrap().iter().all(|r| r.state == State::Blocked));
    assert_eq!(db.pending().unwrap().len(), 2);
    assert!(
        db.pending().unwrap().iter().all(|(_, p, _)| matches!(
            p.status,
            Status::Blocked | Status::Ready
        ) && p.message.len() > 20)
    );
}
#[test]
fn illegal_transition_and_singleton_fail_closed() {
    assert!(!State::Running.permits(State::Completed));
    assert!(!State::Completed.permits(State::Running));
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("lock");
    let lock = Singleton::acquire(&path).unwrap();
    assert!(Singleton::acquire(&path).is_err());
    drop(lock);
    assert!(Singleton::acquire(&path).is_ok());
}
#[test]
fn projection_outage_survives_restart() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    db.sync_config(&c).unwrap();
    let scan = common::scan(&c.projects[0], vec![common::story(Status::Ready)]);
    db.ingest(&c.projects[0], &scan, &c.orchestrator.worktree_root)
        .unwrap();
    let mut r = db.runs().unwrap().remove(0);
    db.claim(&mut r, &c.projects[0], 1).unwrap();
    let id = db.pending().unwrap()[0].0;
    drop(db);
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    assert_eq!(db.pending().unwrap()[0].0, id);
    db.defer(id, 0).unwrap();
    assert!(db.pending().unwrap().is_empty());
    assert_eq!(db.runs().unwrap().len(), 1);
}
#[test]
fn repository_and_global_limits() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    db.sync_config(&c).unwrap();
    for p in &c.projects {
        let scan = common::scan(
            p,
            vec![common::story(Status::Ready), common::story(Status::Ready)],
        );
        db.ingest(p, &scan, &c.orchestrator.worktree_root).unwrap();
    }
    let runs = db.runs().unwrap();
    let mut a = runs[0].clone();
    let mut a2 = runs[1].clone();
    let mut b = runs[2].clone();
    assert!(db.claim(&mut a, &c.projects[0], 2).unwrap());
    assert!(!db.claim(&mut a2, &c.projects[0], 2).unwrap());
    assert!(!db.claim(&mut b, &c.projects[1], 1).unwrap());
    assert!(db.claim(&mut b, &c.projects[1], 2).unwrap());
}
#[tokio::test]
async fn disposable_worktree_is_recovered_without_touching_main() {
    use maistro::git::git;
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let p = &c.projects[0];
    git(&p.repo, &["init", "-b", "main"]).await.unwrap();
    std::fs::write(p.repo.join("file"), "base").unwrap();
    git(&p.repo, &["add", "file"]).await.unwrap();
    git(
        &p.repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    )
    .await
    .unwrap();
    let remote = t.path().join("remote.git");
    git(
        t.path(),
        &[
            "clone",
            "--bare",
            p.repo.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    )
    .await
    .unwrap();
    git(
        &p.repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    )
    .await
    .unwrap();
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    db.sync_config(&c).unwrap();
    db.ingest(
        p,
        &common::scan(p, vec![common::story(Status::Ready)]),
        &c.orchestrator.worktree_root,
    )
    .unwrap();
    let mut r = db.runs().unwrap().remove(0);
    maistro::git::prepare(p, &mut r, &c.orchestrator.worktree_root)
        .await
        .unwrap();
    std::fs::write(r.worktree.join("file"), "change").unwrap();
    maistro::git::prepare(p, &mut r, &c.orchestrator.worktree_root)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(p.repo.join("file")).unwrap(),
        "base"
    );
    maistro::git::commit(&mut r).await.unwrap();
    assert!(r.head_sha.is_some());
    assert!(
        git(&p.repo, &["status", "--porcelain"])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
#[ignore = "requires host user namespaces; run explicitly outside nested coding sandbox"]
async fn real_sandbox_denies_other_repositories_and_credentials() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let p = &c.projects[0];
    std::fs::write(p.repo.join(".git"), "secret git metadata").unwrap();
    std::fs::write(c.projects[1].repo.join("private"), "other repo").unwrap();
    let mut cmd = maistro::sandbox::command(p, &p.repo, true, false).unwrap();
    cmd.args(["--","/bin/sh","-c","test -z \"$MAISTRO_GITHUB_TOKEN\" && test ! -s /work/.git && test ! -e /home/jole/.ssh && printf allowed > /work/result && test ! -e /home/jole/dev"]);
    let o = maistro::process::run(
        cmd,
        "",
        std::time::Duration::from_secs(10),
        &tokio_util::sync::CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(
        std::fs::read_to_string(p.repo.join("result")).unwrap(),
        "allowed"
    );
    assert_eq!(
        std::fs::read_to_string(c.projects[1].repo.join("private")).unwrap(),
        "other repo"
    );
}

#[test]
fn competing_claims_are_transactionally_exclusive() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    db.sync_config(&c).unwrap();
    db.ingest(
        &c.projects[0],
        &common::scan(&c.projects[0], vec![common::story(Status::Ready)]),
        &c.orchestrator.worktree_root,
    )
    .unwrap();
    let run = db.runs().unwrap().remove(0);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut threads = vec![];
    for _ in 0..2 {
        let db = db.clone();
        let mut run = run.clone();
        let p = c.projects[0].clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            db.claim(&mut run, &p, 2).unwrap()
        }));
    }
    let claimed = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .filter(|v| *v)
        .count();
    assert_eq!(claimed, 1);
}

#[tokio::test]
#[ignore = "requires host user namespaces"]
async fn sandbox_preflight_hides_main_checkout_git_directory() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let p = &c.projects[0];
    maistro::git::git(&p.repo, &["init", "-b", "main"])
        .await
        .unwrap();
    let original = std::fs::read(p.repo.join(".git/config")).unwrap();
    for writable in [false, true] {
        let mut cmd = maistro::sandbox::command(p, &p.repo, writable, false).unwrap();
        cmd.args([
            "--", "/bin/sh", "-c",
            "test -d /work/.git && test ! -e /work/.git/config && test ! -e /work/.git/HEAD && ! touch /work/.git/injected",
        ]);
        let result = maistro::process::run(
            cmd,
            "",
            std::time::Duration::from_secs(10),
            &tokio_util::sync::CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.code, 0, "{}", result.stderr);
    }
    assert_eq!(std::fs::read(p.repo.join(".git/config")).unwrap(), original);
    assert!(!p.repo.join(".git/injected").exists());
}

#[tokio::test]
#[ignore = "requires host user namespaces and C/Rust toolchains"]
async fn sandbox_compiles_and_runs_c_and_rust() {
    let t = tempfile::tempdir().unwrap();
    let mut c = common::config(t.path());
    let p = &mut c.projects[0];
    // Hosted CI uses rustup under the runner's home, outside the sandbox's
    // system mounts. Expose only the active toolchain, as a real project would.
    let output = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .expect("Rust toolchain must be installed for this fixture");
    assert!(
        output.status.success(),
        "cannot locate Rust toolchain: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let toolchain = std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
        .canonicalize()
        .unwrap();
    assert!(toolchain.join("bin/rustc").is_file());
    p.toolchain_paths.push(toolchain.clone());
    std::fs::write(p.repo.join("probe.c"), "int main(void) { return 0; }\n").unwrap();
    std::fs::write(p.repo.join("probe.rs"), "fn main() {}\n").unwrap();
    let mut command = maistro::sandbox::command(p, &p.repo, true, false).unwrap();
    // Also prove PATH selects the configured compiler, rather than silently
    // passing on hosts that happen to have another rustc in /usr/bin.
    command.args(["--setenv", "EXPECTED_RUSTC"]);
    command.arg(toolchain.join("bin/rustc"));
    command.args(["--", "sh", "-c", "test \"$(command -v rustc)\" = \"$EXPECTED_RUSTC\" && cc probe.c -o /tmp/c-probe && /tmp/c-probe && rustc probe.rs -o /tmp/rust-probe && /tmp/rust-probe"]);
    maistro::process::checked(command, std::time::Duration::from_secs(30))
        .await
        .unwrap();
}

#[test]
fn cancellation_stops_execution_without_projecting_a_board_status() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    db.sync_config(&c).unwrap();
    let p = &c.projects[0];
    let scan = common::scan(p, vec![common::story(Status::Ready)]);
    db.ingest(p, &scan, &c.orchestrator.worktree_root).unwrap();
    let mut run = db.runs().unwrap().remove(0);
    db.claim(&mut run, p, 1).unwrap();
    assert_eq!(db.pending().unwrap().len(), 1);
    db.transition(&mut run, State::Cancelled, "requirements changed")
        .unwrap();
    assert!(
        db.pending().unwrap().is_empty(),
        "stale In Progress update must be discarded"
    );
    assert_eq!(db.get(&run.id).unwrap().state, State::Cancelled);
    assert_eq!(
        db.scan(&p.id).unwrap().unwrap().stories[0].status,
        Status::Ready
    );

    // An upgrade must also discard cancellation writes queued by an older binary.
    let conn = rusqlite::Connection::open(&c.orchestrator.state_db).unwrap();
    for status in ["cancelled", "review"] {
        let payload = serde_json::json!({"project_id":p.id,"story_id":run.story.story_id,
            "run_id":run.id,"status":status,"message":"legacy update","pr_url":null,"attempt":1});
        conn.execute(
            "INSERT INTO outbox(project_id,story_id,payload) VALUES(?,?,?)",
            rusqlite::params![p.id, run.story.story_id, payload.to_string()],
        )
        .unwrap();
    }
    drop(conn);
    for _ in 0..2 {
        let reopened = Store::open(&c.orchestrator.state_db).unwrap();
        let pending = reopened.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1.status, Status::Review);
    }
}
