mod common;
use maistro::{
    model::{Review, Status},
    policy::{self, Ci},
    store::{State, Store},
};
use serde_json::json;
fn run(c: &maistro::config::Config) -> (Store, maistro::store::Run) {
    let s = Store::open(&c.orchestrator.state_db).unwrap();
    s.sync_config(c).unwrap();
    let scan = common::scan(&c.projects[0], vec![common::story(Status::Ready)]);
    s.ingest(&c.projects[0], &scan, &c.orchestrator.worktree_root)
        .unwrap();
    let r = s.runs().unwrap().remove(0);
    (s, r)
}
#[test]
fn protected_files_and_rename_sources_block() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    for name in [
        ".github/workflows/ci.yml",
        "AGENTS.md",
        "src/auth/login.rs",
        "migrations/001.sql",
        "../escape",
        "secret.key",
        ".env.production",
    ] {
        assert!(
            policy::changed_path_allowed(&c.projects[0], name).is_err(),
            "{name}"
        );
    }
    policy::changed_path_allowed(&c.projects[0], "src/component.rs").unwrap();
}
#[test]
fn ci_requires_exact_head_latest_success_and_present_checks() {
    let required = vec![("test".into(), Some(1))];
    let check = |id, status, conclusion, sha| json!({"id":id,"name":"test","status":status,"conclusion":conclusion,"head_sha":sha,"app":{"id":1}});
    assert_eq!(policy::ci("abc", &required, &[], &[]).unwrap(), Ci::Pending);
    assert_eq!(
        policy::ci(
            "abc",
            &required,
            &[check(1, "completed", "success", "abc")],
            &[]
        )
        .unwrap(),
        Ci::Passed
    );
    assert!(
        policy::ci(
            "abc",
            &required,
            &[check(1, "completed", "success", "old")],
            &[]
        )
        .is_err()
    );
    assert!(matches!(
        policy::ci(
            "abc",
            &required,
            &[
                check(1, "completed", "success", "abc"),
                check(2, "completed", "failure", "abc")
            ],
            &[]
        )
        .unwrap(),
        Ci::Failed(_)
    ));
    assert!(matches!(
        policy::ci(
            "abc",
            &required,
            &[check(1, "completed", "skipped", "abc")],
            &[]
        )
        .unwrap(),
        Ci::Failed(_)
    ));
    assert!(policy::ci("abc", &[], &[], &[]).is_err());
}
#[test]
fn changed_cancelled_removed_stories_reject() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let (_, r) = run(&c);
    policy::authorized(&r, Some(&r.story)).unwrap();
    let mut changed = r.story.clone();
    changed.description += " changed";
    assert!(policy::authorized(&r, Some(&changed)).is_err());
    changed = r.story.clone();
    changed.status = Status::Cancelled;
    assert!(policy::authorized(&r, Some(&changed)).is_err());
    assert!(policy::authorized(&r, None).is_err());
}
#[test]
fn self_report_cannot_replace_tests_or_independent_review() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let (_, mut r) = run(&c);
    r.head_sha = Some("abc".into());
    r.review = Some(Review {
        head_sha: "abc".into(),
        approved: true,
        blocking: vec![],
        nonblocking: vec![],
    });
    assert!(policy::review_valid(&r).is_err());
    r.test_sha = Some("abc".into());
    r.agent_sessions = vec!["implementer".into(), "implementer".into()];
    assert!(policy::review_valid(&r).is_err());
    r.agent_sessions[1] = "fresh reviewer".into();
    policy::review_valid(&r).unwrap();
    r.review.as_mut().unwrap().blocking.push("defect".into());
    assert!(policy::review_valid(&r).is_err());
}
#[test]
fn merge_intent_and_done_outbox_survive_crash() {
    let t = tempfile::tempdir().unwrap();
    let c = common::config(t.path());
    let (db, mut r) = run(&c);
    db.claim(&mut r, &c.projects[0], 1).unwrap();
    db.transition(&mut r, State::Reviewing, "review").unwrap();
    db.transition(&mut r, State::WaitingCi, "ci").unwrap();
    db.transition(&mut r, State::Merging, "intent").unwrap();
    drop(db);
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    let mut r = db.get(&r.id).unwrap();
    assert_eq!(r.state, State::Merging);
    r.merge_sha = Some("confirmed".into());
    db.transition(&mut r, State::Completed, "confirmed merge")
        .unwrap();
    drop(db);
    let db = Store::open(&c.orchestrator.state_db).unwrap();
    assert_eq!(db.runs().unwrap().len(), 1);
    let pending = db.pending().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].1.status, Status::Done);
    assert_eq!(db.get(&r.id).unwrap().state, State::Completed);
}
