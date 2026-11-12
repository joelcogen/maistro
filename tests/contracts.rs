use maistro::{
    config::{Config, Mode},
    model::{Assignment, Completion, Coordination, PrRequest, Report, Snapshot, StoryRef},
};
use std::fs;

fn assignment(project: &str) -> Assignment {
    Assignment {
        project_id: project.into(),
        story_id: "story-1".into(),
        title: "A story".into(),
        story_url: None,
        context: "Full requirements".into(),
        recovery: false,
        pr: None,
    }
}
fn completion() -> Completion {
    Completion {
        id: "result-1".into(),
        mode: Mode::Master,
        story: StoryRef::from(&assignment("p0")),
        report: Report::blocked("blocked".into()),
    }
}

#[test]
fn coordinator_cannot_assign_busy_unknown_or_duplicate_projects() {
    let projects = vec!["p0".into(), "p1".into()];
    let mut update = Coordination {
        assignments: vec![assignment("p0")],
        ..Default::default()
    };
    let active = Snapshot {
        active: vec![StoryRef::from(&assignment("p0"))],
        pending: vec![],
    };
    assert!(update.validate(&projects, &active).is_err());
    let pending = Snapshot {
        active: vec![],
        pending: vec![completion()],
    };
    update.acknowledged.push("result-1".into());
    // Acknowledging and assigning the same project in one pass is also forbidden.
    assert!(update.validate(&projects, &pending).is_err());
    update.acknowledged.clear();
    assert!(update.validate(&projects, &Snapshot::default()).is_ok());
    update.assignments.push(assignment("p0"));
    assert!(update.validate(&projects, &Snapshot::default()).is_err());
    update.assignments = vec![assignment("unknown")];
    assert!(update.validate(&projects, &Snapshot::default()).is_err());
}

#[test]
fn coordinator_cannot_cancel_unrelated_work_or_discard_unknown_results() {
    let projects = vec!["p0".into()];
    let snapshot = Snapshot {
        active: vec![StoryRef::from(&assignment("p0"))],
        pending: vec![completion()],
    };
    let mut update = Coordination {
        cancel: vec![StoryRef::from(&assignment("p1"))],
        ..Default::default()
    };
    assert!(update.validate(&projects, &snapshot).is_err());
    update.cancel = snapshot.active.clone();
    assert!(update.validate(&projects, &snapshot).is_ok());
    update.acknowledged.push("nonexistent".into());
    assert!(update.validate(&projects, &snapshot).is_err());
    update.acknowledged = vec!["result-1".into()];
    assert!(update.validate(&projects, &snapshot).is_ok());
}

#[test]
fn incomplete_assignment_and_incomplete_success_are_rejected() {
    let mut a = assignment("p0");
    a.context.clear();
    let update = Coordination {
        assignments: vec![a],
        ..Default::default()
    };
    assert!(
        update
            .validate(&["p0".into()], &Snapshot::default())
            .is_err()
    );
    let mut report = Report {
        outcome: maistro::model::Outcome::Completed,
        summary: "Done".into(),
        tests: vec![],
        commit: None,
        pr_url: None,
    };
    assert!(report.validate(Mode::Master).is_err());
    report.commit = Some("abc123".into());
    assert!(report.validate(Mode::Master).is_err());
    report.tests.push("cargo test passed".into());
    assert!(report.validate(Mode::Master).is_ok());
    assert!(report.validate(Mode::Pr).is_err());
    report.pr_url = Some("https://example.test/pr/1".into());
    assert!(report.validate(Mode::Pr).is_ok());
}

#[test]
fn config_resolves_project_prompt_and_runtime_paths_relative_to_config() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("repo")).unwrap();
    fs::write(dir.path().join("project.md"), "Run tests").unwrap();
    let path = dir.path().join("config.toml");
    let config = "[agents]\ngithub_token_file='credentials/github-token'\n[orchestrator]\nruntime_dir='runtime'\n[[projects]]\nid='p0'\nrepo='repo'\nappflowy_page='Stories'\nmode='master'\nbase_branch='trunk'\nprompt_file='project.md'\n";
    fs::write(&path, config).unwrap();
    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded.projects[0].repo, dir.path().join("repo"));
    assert_eq!(
        loaded.projects[0].prompt_file.as_ref().unwrap(),
        &dir.path().join("project.md")
    );
    assert_eq!(loaded.orchestrator.runtime_dir, dir.path().join("runtime"));
    assert_eq!(loaded.projects[0].base_branch, "trunk");
    assert_eq!(
        loaded.agents.github_token_file,
        dir.path().join("credentials/github-token")
    );
    fs::write(
        &path,
        config.replace("runtime_dir='runtime'", "state_db='old.db'"),
    )
    .unwrap();
    assert!(
        Config::load(&path)
            .unwrap_err()
            .to_string()
            .contains("migrated")
    );
    fs::write(&path, config.replace("project.md", "missing.md")).unwrap();
    assert!(Config::load(&path).is_err());
    fs::write(&path, config.replace("mode='master'", "mode='merge'")).unwrap();
    assert!(Config::load(&path).is_err());
}

#[test]
fn project_prompt_is_reloaded_for_each_assignment_and_git_modes_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::create_dir(dir.path().join("repo")).unwrap();
    fs::write(dir.path().join("project.md"), "First test command").unwrap();
    fs::write(&path, "[orchestrator]\nruntime_dir='runtime'\n[[projects]]\nid='p0'\nrepo='repo'\nappflowy_page='Stories'\nmode='pr'\nbase_branch='trunk'\nprompt_file='project.md'\n").unwrap();
    let mut project = Config::load(&path).unwrap().projects.remove(0);
    let first = maistro::codex::worker_prompt(&project, &assignment("p0")).unwrap();
    assert!(first.contains("First test command") && first.contains("maistro/p0/story-1"));
    assert!(first.contains("PR MODE:") && !first.contains("MASTER MODE:"));
    fs::write(dir.path().join("project.md"), "Revised test command").unwrap();
    project.mode = Mode::Master;
    let second = maistro::codex::worker_prompt(&project, &assignment("p0")).unwrap();
    assert!(second.contains("Revised test command") && second.contains("\"story_branch\":null"));
    assert!(second.contains("\"base_branch\":\"trunk\""));
    assert!(second.contains("MASTER MODE:") && !second.contains("PR MODE:"));

    let mut story = assignment("p0");
    // Product requirements about PR support must not trigger a text-based override.
    story.context = "Add support for PRs and custom branch names in master mode".into();
    assert_eq!(story.effective_mode(project.mode), Mode::Master);
    story.pr = Some(PrRequest { branch: None });
    let overridden = maistro::codex::worker_prompt(&project, &story).unwrap();
    assert!(overridden.contains("\"mode\":\"pr\""));
    assert!(overridden.contains("\"story_branch\":\"maistro/p0/story-1\""));
    assert!(overridden.contains("PR MODE:") && !overridden.contains("MASTER MODE:"));

    for mode in [Mode::Master, Mode::Pr] {
        project.mode = mode;
        story.pr.as_mut().unwrap().branch = Some("feature/requested-branch".into());
        let custom = maistro::codex::worker_prompt(&project, &story).unwrap();
        assert!(custom.contains("\"story_branch\":\"feature/requested-branch\""));
        assert!(custom.contains("\"base_branch\":\"trunk\""));
        assert!(custom.contains("\"remote\":\"origin\""));
        assert!(custom.contains("\"story_marker\":\"AppFlowy story: story-1\""));
        for invalid in [
            "", "trunk", "HEAD", "-option", "bad name", "a..b", "@{-1}", "a.lock",
        ] {
            story.pr.as_mut().unwrap().branch = Some(invalid.into());
            assert!(
                maistro::codex::worker_prompt(&project, &story).is_err(),
                "{invalid}"
            );
        }
    }
}

#[test]
fn coordinator_schema_carries_an_explicit_nullable_pr_request() {
    let schema = maistro::model::coordination_schema();
    let assignment_schema = &schema["properties"]["assignments"]["items"];
    assert!(
        assignment_schema["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("pr"))
    );
    assert_eq!(
        assignment_schema["properties"]["pr"]["anyOf"][0]["required"],
        serde_json::json!(["branch"])
    );
    for pr in [
        serde_json::Value::Null,
        serde_json::json!({"branch":null}),
        serde_json::json!({"branch":"feature/custom"}),
    ] {
        let mut value = serde_json::to_value(assignment("p0")).unwrap();
        value["pr"] = pr.clone();
        let parsed: Assignment = serde_json::from_value(value).unwrap();
        assert_eq!(
            parsed.effective_mode(Mode::Master),
            if pr.is_null() { Mode::Master } else { Mode::Pr }
        );
        assert_eq!(serde_json::to_value(parsed).unwrap()["pr"], pr);
    }
}

#[test]
fn status_server_defaults_to_optional_local_port_8044_and_rejects_invalid_ports() {
    let minimal = "[[projects]]\nid='p'\nrepo='/tmp'\nappflowy_page='Board'\nmode='master'\nbase_branch='master'\n";
    let config: Config = toml::from_str(minimal).unwrap();
    assert!(!config.status.enabled);
    assert_eq!(config.status.port, 8044);
    for port in ["0", "65536", "-1"] {
        let parsed =
            toml::from_str::<Config>(&format!("[status]\nenabled=true\nport={port}\n{minimal}"));
        assert!(parsed.is_err() || parsed.unwrap().validate().is_err());
    }
    assert!(toml::from_str::<Config>(&format!("[status]\nlisten='0.0.0.0'\n{minimal}")).is_err());
}

#[test]
fn coordinator_retry_signal_is_required_scoped_and_unique() {
    let mut update = Coordination {
        retry_projects: vec!["p0".into()],
        ..Default::default()
    };
    assert!(
        update
            .validate(&["p0".into()], &Snapshot::default())
            .is_ok()
    );
    update.retry_projects.push("p0".into());
    assert!(
        update
            .validate(&["p0".into()], &Snapshot::default())
            .is_err()
    );
    update.retry_projects = vec!["unknown".into()];
    assert!(
        update
            .validate(&["p0".into()], &Snapshot::default())
            .is_err()
    );
    assert!(serde_json::from_value::<Coordination>(serde_json::json!({"assignments":[],"cancel":[],"acknowledged":[],"notes":[],"observations":[]})).is_err());
}
