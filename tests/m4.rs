mod common;
use maistro::{
    model::Status,
    scheduler::fair_candidates,
    store::{State, Store, backoff},
};
#[test]
fn round_robin_and_priority_with_active_run_precedence() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    db.sync_config(&c).unwrap();
    for p in &c.projects {
        let mut low = common::story(Status::Ready);
        low.priority = 1;
        let mut high = common::story(Status::Ready);
        high.priority = 9;
        db.ingest(
            p,
            &common::scan(p, vec![low, high]),
            &c.orchestrator.worktree_root,
        )
        .unwrap();
    }
    let runs = db.runs().unwrap();
    let a = fair_candidates(&c, &runs, 0, &Default::default());
    assert_eq!(a[0].project_id, "a");
    assert_eq!(a[0].story.priority, 9);
    assert_eq!(
        fair_candidates(&c, &runs, 1, &Default::default())[0].project_id,
        "b"
    );
    let mut low = runs
        .iter()
        .find(|r| r.project_id == "a" && r.story.priority == 1)
        .unwrap()
        .clone();
    db.claim(&mut low, &c.projects[0], 2).unwrap();
    assert_eq!(
        fair_candidates(&c, &db.runs().unwrap(), 0, &Default::default())[0].id,
        low.id
    );
}
#[test]
fn pause_persists_blocks_new_claims_and_project_resume_is_independent() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    db.sync_config(&c).unwrap();
    let p = &c.projects[0];
    db.ingest(
        p,
        &common::scan(p, vec![common::story(Status::Ready)]),
        &c.orchestrator.worktree_root,
    )
    .unwrap();
    let mut r = db.runs().unwrap().remove(0);
    db.set_control("paused", "true").unwrap();
    drop(db);
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    assert!(!db.claim(&mut r, p, 2).unwrap());
    db.set_control("paused", "false").unwrap();
    db.project_pause("a", true).unwrap();
    assert!(!db.claim(&mut r, p, 2).unwrap());
    assert!(!db.paused("b").unwrap());
    db.project_pause("a", false).unwrap();
    assert!(db.claim(&mut r, p, 2).unwrap());
}
#[test]
fn pending_pr_policy_preserves_exclusive_execution_and_allows_batch_review() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    db.sync_config(&c).unwrap();
    let p = &c.projects[0];
    db.ingest(
        p,
        &common::scan(
            p,
            vec![common::story(Status::Ready), common::story(Status::Ready)],
        ),
        &c.orchestrator.worktree_root,
    )
    .unwrap();
    let runs = db.runs().unwrap();
    let mut a = runs[0].clone();
    let mut b = runs[1].clone();
    db.claim(&mut a, p, 2).unwrap();
    db.transition(&mut a, State::Reviewing, "review").unwrap();
    db.transition(&mut a, State::WaitingCi, "CI").unwrap();
    db.release(&a.id).unwrap();
    let mut serial = p.clone();
    serial.continue_while_prs_pending = false;
    assert!(!db.claim(&mut b, &serial, 2).unwrap());
    assert!(
        p.continue_while_prs_pending,
        "batch review must be the default"
    );
    let prefer_new = std::collections::HashSet::from([p.id.clone()]);
    assert_eq!(
        fair_candidates(&c, &db.runs().unwrap(), 0, &prefer_new)[0].id,
        b.id
    );
    assert_eq!(
        fair_candidates(&c, &db.runs().unwrap(), 0, &Default::default())[0].id,
        a.id
    );
    db.acquire_recovery(&a, p).unwrap();
    assert!(
        !db.claim(&mut b, p, 2).unwrap(),
        "an active CI check or repair still owns the lease"
    );
    db.release(&a.id).unwrap();
    assert!(db.claim(&mut b, p, 2).unwrap());
    assert!(
        db.acquire_recovery(&a, p).is_err(),
        "reconciliation cannot steal the coding lease"
    );
    assert_eq!(
        fair_candidates(&c, &db.runs().unwrap(), 0, &Default::default())[0].id,
        b.id
    );
    drop(db);
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    assert_eq!(db.get(&a.id).unwrap().state, State::WaitingCi);
    assert_eq!(db.get(&b.id).unwrap().state, State::Running);
}
#[test]
fn backoff_is_bounded_and_configuration_identity_cannot_drift() {
    assert_eq!(backoff(0), 5);
    assert_eq!(backoff(1000), 3600);
    let t = tempfile::tempdir().unwrap();
    let mut c = common::config(t.path());
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    db.sync_config(&c).unwrap();
    c.projects[0].github_repo = "attacker/different".into();
    assert!(db.sync_config(&c).is_err());
}
#[tokio::test]
async fn timeout_preserves_partial_logs() {
    let t = tempfile::tempdir().unwrap();
    let log = t.path().join("run.jsonl");
    let mut cmd = tokio::process::Command::new("sh");
    cmd.args(["-c", "printf 'started\n'; sleep 10"]);
    assert!(
        maistro::process::run(
            cmd,
            "",
            std::time::Duration::from_millis(80),
            &tokio_util::sync::CancellationToken::new(),
            Some(&log)
        )
        .await
        .is_err()
    );
    assert!(std::fs::read_to_string(log).unwrap().contains("started"));
}
#[tokio::test]
#[ignore = "requires host user namespaces"]
async fn two_isolated_projects_execute_concurrently() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let token = tokio_util::sync::CancellationToken::new();
    let execute = |p: maistro::config::Project| {
        let token = token.clone();
        async move {
            let mut cmd = maistro::sandbox::command(&p, &p.repo, true, false).unwrap();
            cmd.args([
                "--",
                "/bin/sh",
                "-c",
                "printf start > started; sleep 1; printf done > done",
            ]);
            maistro::process::run(cmd, "", std::time::Duration::from_secs(10), &token, None)
                .await
                .unwrap()
        }
    };
    let a = tokio::spawn(execute(c.projects[0].clone()));
    let b = tokio::spawn(execute(c.projects[1].clone()));
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        c.projects
            .iter()
            .all(|p| p.repo.join("started").exists() && !p.repo.join("done").exists())
    );
    assert_eq!(a.await.unwrap().code, 0);
    assert_eq!(b.await.unwrap().code, 0);
    assert!(c.projects.iter().all(|p| p.repo.join("done").exists()));
}

#[test]
fn dependency_completion_reconsiders_once_without_new_authorization() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    db.sync_config(&c).unwrap();
    let dependency = common::story(Status::Backlog);
    let mut story = common::story(Status::Ready);
    story.dependencies.push(dependency.story_id.clone());
    let mut scan = common::scan(&c.projects[0], vec![story, dependency]);
    db.ingest(&c.projects[0], &scan, &c.orchestrator.worktree_root)
        .unwrap();
    assert_eq!(db.runs().unwrap()[0].state, State::Blocked);
    assert_eq!(db.pending().unwrap()[0].1.status, Status::Ready);
    scan.stories[1].status = Status::Done;
    assert_eq!(
        db.ingest(&c.projects[0], &scan, &c.orchestrator.worktree_root)
            .unwrap(),
        1
    );
    assert_eq!(db.runs().unwrap()[0].state, State::Queued);
    assert_eq!(db.runs().unwrap().len(), 1);
    assert_eq!(
        db.ingest(&c.projects[0], &scan, &c.orchestrator.worktree_root)
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn successful_supervision_records_verified_termination() {
    let t = tempfile::tempdir().unwrap();
    let log = t.path().join("process.jsonl");
    let result = maistro::process::run(
        tokio::process::Command::new("true"),
        "",
        std::time::Duration::from_secs(2),
        &tokio_util::sync::CancellationToken::new(),
        Some(&log),
    )
    .await
    .unwrap();
    assert_eq!(result.code, 0);
    assert!(log.with_extension("terminated").exists());
    maistro::process::reconcile_groups(t.path()).unwrap();
}
