mod common;
use maistro::{config::Config, model::Status, store::Store};

#[test]
fn restart_replaces_saved_settings_and_invalidates_only_changed_scans() {
    let t = tempfile::tempdir().unwrap();
    let mut config = common::config(t.path());
    let path = t.path().join("config.toml");
    let db = Store::open(&config.orchestrator.state_db).unwrap();
    db.sync_startup_config(&config).unwrap();
    let a = common::scan(&config.projects[0], vec![]);
    let b = common::scan(&config.projects[1], vec![]);
    db.record_scan(&a).unwrap();
    db.record_scan(&b).unwrap();
    db.project_pause("a", true).unwrap();
    db.sync_startup_config(&config).unwrap();
    assert!(db.scan("a").unwrap().is_some());
    drop(db);

    config.projects[0].appflowy_page = Some("Replacement board".into());
    config.projects[0].base_branch = "develop".into();
    config.projects[0].test_commands = vec![vec!["new-check".into()]];
    config.projects[0].enabled = false;
    std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
    let loaded = Config::load(&path).unwrap();
    let db = Store::open(&loaded.orchestrator.state_db).unwrap();
    db.sync_startup_config(&loaded).unwrap();
    assert!(db.scan("a").unwrap().is_none());
    assert_eq!(db.scan("b").unwrap().unwrap().board_id, b.board_id);
    assert!(db.paused("a").unwrap());
    let conn = rusqlite::Connection::open(&loaded.orchestrator.state_db).unwrap();
    let saved: String = conn
        .query_row("SELECT config FROM projects WHERE id='a'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&saved).unwrap(),
        serde_json::to_value(&loaded.projects[0]).unwrap()
    );
    db.record_scan(&common::scan(&loaded.projects[0], vec![]))
        .unwrap();
}

#[test]
fn settings_changes_preserve_runs_but_identity_changes_with_saved_work_are_atomic() {
    let t = tempfile::tempdir().unwrap();
    let mut config = common::config(t.path());
    let db = Store::open(&config.orchestrator.state_db).unwrap();
    db.sync_startup_config(&config).unwrap();
    db.ingest(
        &config.projects[1],
        &common::scan(&config.projects[1], vec![common::story(Status::Ready)]),
        &config.orchestrator.worktree_root,
    )
    .unwrap();
    let mut run = db.runs().unwrap().remove(0);
    assert!(db.claim(&mut run, &config.projects[1], 2).unwrap());
    config.projects[1].required_checks.push("lint".into());
    db.sync_startup_config(&config).unwrap();
    assert_eq!(db.runs().unwrap()[0].id, run.id);
    assert!(!db.pending().unwrap().is_empty());
    config.projects[0].name = "Must roll back".into();
    config.projects[1].github_repo = "owner/replacement".into();
    assert!(
        db.sync_startup_config(&config)
            .unwrap_err()
            .to_string()
            .contains("saved work")
    );
    let conn = rusqlite::Connection::open(&config.orchestrator.state_db).unwrap();
    let saved: String = conn
        .query_row("SELECT config FROM projects WHERE id='a'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&saved).unwrap()["name"],
        "A"
    );
    assert_eq!(db.runs().unwrap()[0].id, run.id);
}
