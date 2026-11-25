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
        elapsed_seconds: 0,
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
    report.pr_url = Some("https://example.test/pr/1".into());
    assert!(report.validate(Mode::Pr).is_ok());
}

#[test]
fn config_resolves_paths_relative_to_config() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("repo")).unwrap();
    let path = dir.path().join("config.toml");
    let config = "[agents]\ngithub_token_file='credentials/github-token'\n[orchestrator]\nruntime_dir='runtime'\n[[projects]]\nid='p0'\nrepo='repo'\nappflowy_page='Stories'\nmode='master'\nbase_branch='trunk'\n";
    fs::write(&path, config).unwrap();
    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded.projects[0].repo, dir.path().join("repo"));
    assert_eq!(loaded.orchestrator.runtime_dir, dir.path().join("runtime"));
    assert_eq!(loaded.projects[0].base_branch, "trunk");
    assert_eq!(
        loaded.agents.github_token_file,
        dir.path().join("credentials/github-token")
    );
}

#[test]
fn worker_git_delivery_uses_explicit_requests_and_rejects_invalid_branch_names() {
    let dir = tempfile::tempdir().unwrap();
    let project = serde_json::from_value(serde_json::json!({
        "id": "p0", "repo": dir.path(), "appflowy_page": "Stories",
        "mode": "master", "base_branch": "trunk", "remote": "upstream"
    }))
    .unwrap();
    let mut story = assignment("p0");
    story.context = "Add support for PRs and custom branch names in master mode".into();
    let prompt = maistro::agent::worker_prompt(&project, &story).unwrap();
    let git: serde_json::Value = serde_json::from_str(
        prompt
            .lines()
            .find_map(|line| line.strip_prefix("GIT="))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        git,
        serde_json::json!({
            "mode": "master", "base_branch": "trunk", "remote": "upstream",
            "story_branch": null, "story_marker": "AppFlowy story: story-1"
        })
    );

    for branch in [
        "", "trunk", "HEAD", "-option", "bad name", "a..b", "@{-1}", "a.lock",
    ] {
        story.pr = Some(PrRequest {
            branch: Some(branch.into()),
        });
        assert!(
            maistro::agent::worker_prompt(&project, &story).is_err(),
            "accepted {branch:?}"
        );
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

#[test]
fn incomplete_coordination_cannot_authorize_new_or_recovered_work() {
    let projects = vec!["p0".into(), "p1".into()];
    for recovery in [false, true] {
        let mut story = assignment("p0");
        story.recovery = recovery;
        let mut update = Coordination {
            assignments: vec![story],
            retry_projects: vec!["p0".into()],
            ..Default::default()
        };
        let error = update
            .validate(&projects, &Snapshot::default())
            .unwrap_err();
        assert!(error.to_string().contains("incomplete coordination"));
        // A failed project does not prohibit work in another, fully read project.
        update.assignments[0].project_id = "p1".into();
        update.validate(&projects, &Snapshot::default()).unwrap();
    }
    // Enumeration failure must not suppress independently confirmed lifecycle reads.
    let story = StoryRef::from(&assignment("p0"));
    let update = Coordination {
        retry_projects: vec!["p0".into()],
        cancel: vec![story.clone()],
        acknowledged: vec!["result-1".into()],
        ..Default::default()
    };
    update
        .validate(
            &projects,
            &Snapshot {
                active: vec![story],
                pending: vec![completion()],
            },
        )
        .unwrap();
}

#[test]
fn shortcut_configuration_rejects_invalid_urls_and_source_connections() {
    let appflowy = "[[projects]]\nid='p'\nrepo='/tmp'\nappflowy_page='Board'\nmode='master'\nbase_branch='master'\n";
    let shortcut = appflowy.replace(
        "appflowy_page='Board'",
        "shortcut_url='https://app.shortcut.com/team/settings/workflows/42'",
    );
    for invalid in [
        appflowy.replace("appflowy_page='Board'", ""),
        shortcut.replace("https://app.shortcut.com/team/settings/workflows/42", ""),
        shortcut.replace("https://", "http://"),
        shortcut.replace("app.shortcut.com", "app.shortcut.com.evil.test"),
        shortcut.replace("app.shortcut.com", "user:secret@app.shortcut.com"),
        shortcut.replace("/team/settings/workflows/42", "/team"),
        shortcut.replace("mode='master'", "mode='master'\nmcp_server='bad.name'"),
    ] {
        assert!(
            toml::from_str::<Config>(&invalid)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let conflicting = format!(
        "{shortcut}\n{}",
        appflowy
            .replace("id='p'", "id='p2'")
            .replace("repo='/tmp'", "repo='/var'")
            .replace(
                "appflowy_page='Board'",
                "appflowy_page='Board'\nmcp_server='shortcut'"
            )
    );
    assert!(
        toml::from_str::<Config>(&conflicting)
            .unwrap()
            .validate()
            .is_err()
    );
}

#[test]
fn workflow_selectors_validate_and_preserve_source_identity() {
    let base = "[[projects]]\nid='p'\nrepo='/tmp'\nmode='master'\nbase_branch='master'\n";
    let selectors = [
        "appflowy_page='Board'",
        "appflowy_url='https://appflowy.example/board'",
        "shortcut_url='https://app.shortcut.com/team/settings/workflows/42'",
        "shortcut_workflow='Agents'",
        "shortcut_workflow_id=500005096",
    ];
    for (index, selector) in selectors.iter().enumerate() {
        let mut config: Config = toml::from_str(&format!("{base}{selector}")).unwrap();
        config.validate().unwrap();
        let shortcut = index >= 2;
        let project = &config.projects[0];
        assert_eq!(
            project.mcp_server(&config.agents),
            if shortcut { "shortcut" } else { "appflowy" }
        );
        if index >= 3 {
            assert_eq!(project.board_url(), None);
        }
        config.projects[0].mcp_server = Some("my_workspace".into());
        assert_eq!(
            config.projects[0].mcp_server(&config.agents),
            "my_workspace"
        );
        for other in &selectors[index + 1..] {
            let conflicting: Config =
                toml::from_str(&format!("{base}{selector}\n{other}")).unwrap();
            assert!(
                conflicting
                    .validate()
                    .unwrap_err()
                    .to_string()
                    .contains("exactly one")
            );
        }
    }
    for invalid in [
        "shortcut_workflow=''",
        "shortcut_workflow='   '",
        "shortcut_workflow_id=0",
    ] {
        let config: Config = toml::from_str(&format!("{base}{invalid}")).unwrap();
        assert!(config.validate().is_err(), "accepted {invalid}");
    }
    for invalid in [
        "shortcut_workflow_id=-1",
        "shortcut_workflow_id='500005096'",
        "shortcut_workflow_id=1.5",
        "shortcut_workflow_id=true",
        "shortcut_workflow=500005096",
    ] {
        assert!(
            toml::from_str::<Config>(&format!("{base}{invalid}")).is_err(),
            "accepted {invalid}"
        );
    }
    let named: Config = toml::from_str(&format!("{base}shortcut_workflow='500005096'")).unwrap();
    named.validate().unwrap();
    assert_eq!(
        named.projects[0].shortcut_workflow.as_deref(),
        Some("500005096")
    );
    assert_eq!(named.projects[0].shortcut_workflow_id, None);
}

#[test]
fn source_mcp_policies_reuse_connections_and_expose_only_coordination_tools() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("config.toml"),
        r#"
[mcp_servers.team_shortcut]
url = "https://mcp.shortcut.com/mcp"
enabled = false
disabled_tools = ["update_story"]
[mcp_servers.team_shortcut.http_headers]
x-fixture = "preserved"
[mcp_servers.team_shortcut.tools.update_story]
approval_mode = "prompt"
[mcp_servers.appflowy]
command = "fixture-appflowy"
args = ["--fixture"]
"#,
    )
    .unwrap();
    let mut config: Config = toml::from_str("[[projects]]\nid='p'\nrepo='/tmp'\nshortcut_url='https://app.shortcut.com/team/settings/workflows/42'\nmcp_server='team_shortcut'\nmode='master'\nbase_branch='master'\n").unwrap();
    config.agents.codex_home = dir.path().into();
    let codex = maistro::agent::AgentRunner::new(config.clone());
    let shortcut = codex.mcp_config(&config.projects[0]).unwrap();
    assert_eq!(
        shortcut["url"].as_str(),
        Some("https://mcp.shortcut.com/mcp")
    );
    assert_eq!(
        shortcut["http_headers"]["x-fixture"].as_str(),
        Some("preserved")
    );
    assert_eq!(shortcut["enabled"].as_bool(), Some(true));
    assert!(shortcut.get("disabled_tools").is_none());
    assert_eq!(
        shortcut["tools"]["update_story"]["approval_mode"].as_str(),
        Some("approve")
    );
    let enabled = shortcut["enabled_tools"].as_array().unwrap();
    for tool in [
        "list_workflows",
        "get_workflow",
        "list_workflow_states",
        "list_workflow_stories",
        "list_stories",
        "search_stories",
        "get_story",
        "list_story_comments",
        "create_story_comment",
        "update_story",
        "get_doc",
        "read_file",
    ] {
        assert!(enabled.contains(&toml::Value::from(tool)), "missing {tool}");
    }
    for tool in [
        "create_story",
        "delete_story",
        "create_doc",
        "appflowy_rows",
        "list_workspace_tokens",
    ] {
        assert!(
            !enabled.contains(&toml::Value::from(tool)),
            "unexpected {tool}"
        );
    }
    config.projects[0].shortcut_url = None;
    config.projects[0].appflowy_page = Some("Board".into());
    config.projects[0].mcp_server = None;
    let appflowy = codex.mcp_config(&config.projects[0]).unwrap();
    assert_eq!(appflowy["command"].as_str(), Some("fixture-appflowy"));
    for tool in ["appflowy_database", "appflowy_rows", "appflowy_update"] {
        assert_eq!(
            appflowy["tools"][tool]["approval_mode"].as_str(),
            Some("approve"),
            "compact tool {tool} must work under approval policy never"
        );
    }

    config.projects[0].mcp_server = Some("missing".into());
    assert!(
        codex
            .mcp_config(&config.projects[0])
            .unwrap_err()
            .to_string()
            .contains("missing")
    );
}

#[test]
fn provider_and_models_are_global_with_compatible_defaults() {
    use maistro::config::Provider;
    let project = "[[projects]]\nid='p'\nrepo='/tmp'\nappflowy_page='Stories'\nmode='master'\nbase_branch='main'\n";
    let config: Config = toml::from_str(project).unwrap();
    assert_eq!(config.agents.provider, Provider::Codex);
    assert_eq!(config.agents.coordinator(), "gpt-6-luna");
    assert_eq!(config.agents.worker(), "gpt-6-astra");
    let config: Config =
        toml::from_str(&format!("[agents]\nprovider='claude'\n{project}")).unwrap();
    assert_eq!(config.agents.coordinator(), "haiku");
    assert_eq!(config.agents.worker(), "opus");
    for provider in ["codex", "claude"] {
        let config: Config = toml::from_str(&format!("[agents]\nprovider='{provider}'\ncoordinator='custom-coordinator'\nworker='custom-worker'\n{project}")).unwrap();
        config.validate().unwrap();
        assert_eq!(config.agents.coordinator(), "custom-coordinator");
        assert_eq!(config.agents.worker(), "custom-worker");
        for field in ["coordinator", "worker"] {
            for value in ["", "   ", "--bad-option", "bad\\nmodel"] {
                let config: Config = toml::from_str(&format!(
                    "[agents]\nprovider='{provider}'\n{field}=\"{value}\"\n{project}"
                ))
                .unwrap();
                let error = config.validate().unwrap_err().to_string();
                assert!(error.contains(&format!("agents.{field}")), "{error}");
            }
        }
    }
    assert!(toml::from_str::<Config>(&format!("[agents]\nprovider='other'\n{project}")).is_err());
    for field in [
        "provider='claude'",
        "coordinator='other'",
        "coordinator_effort='low'",
    ] {
        assert!(toml::from_str::<Config>(&format!("{project}{field}\n")).is_err());
    }
}

#[test]
fn effort_validation_is_optional_role_specific_and_provider_specific() {
    let project = "[[projects]]\nid='p'\nrepo='/tmp'\nappflowy_page='Stories'\nmode='master'\nbase_branch='main'\n";
    for (provider, supported) in [
        (
            "codex",
            vec![
                "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
            ],
        ),
        ("claude", vec!["low", "medium", "high", "xhigh", "max"]),
    ] {
        let config: Config =
            toml::from_str(&format!("[agents]\nprovider='{provider}'\n{project}")).unwrap();
        config.validate().unwrap();
        assert_eq!(config.agents.coordinator_effort, None);
        assert_eq!(config.agents.worker_effort, None);
        for field in ["coordinator_effort", "worker_effort"] {
            for value in &supported {
                let config: Config = toml::from_str(&format!(
                    "[agents]\nprovider='{provider}'\n{field}='{value}'\n{project}"
                ))
                .unwrap();
                config.validate().unwrap();
                let (selected, other) = if field == "coordinator_effort" {
                    (
                        &config.agents.coordinator_effort,
                        &config.agents.worker_effort,
                    )
                } else {
                    (
                        &config.agents.worker_effort,
                        &config.agents.coordinator_effort,
                    )
                };
                assert_eq!(selected.as_deref(), Some(*value));
                assert_eq!(other, &None);
            }
            for value in [
                "",
                " ",
                "HIGH",
                " high",
                "high ",
                "default",
                "auto",
                "ultracode",
                "--high",
                "unknown",
                "none",
                "minimal",
                "ultra",
            ] {
                if supported.contains(&value) {
                    continue;
                }
                let config: Config = toml::from_str(&format!(
                    "[agents]\nprovider='{provider}'\n{field}='{value}'\n{project}"
                ))
                .unwrap();
                let error = config.validate().unwrap_err().to_string();
                assert!(
                    error.contains(field)
                        && error.contains(provider)
                        && error.contains("supported values"),
                    "{error}"
                );
            }
            for value in ["1", "true", "[]"] {
                assert!(
                    toml::from_str::<Config>(&format!(
                        "[agents]\nprovider='{provider}'\n{field}={value}\n{project}"
                    ))
                    .is_err()
                );
            }
        }
    }
}

#[test]
fn claude_paths_resolve_and_mcp_transport_preserves_headers_and_source_selection() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("repo")).unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "[agents]\nprovider='claude'\nclaude='./bin/claude'\nclaude_mcp_config='connections.json'\n[orchestrator]\nruntime_dir='runtime'\n[[projects]]\nid='p'\nrepo='repo'\nappflowy_page='Stories'\nmcp_server='team'\nmode='master'\nbase_branch='main'\n").unwrap();
    fs::write(dir.path().join("connections.json"), r#"{"mcpServers":{"team":{"type":"http","url":"https://example.test/mcp","headers":{"Authorization":"Bearer ${MAISTRO_TEST_UNSET_819:-fixture-only}"}}}}"#).unwrap();
    let config = Config::load(&path).unwrap();
    assert_eq!(config.agents.claude, dir.path().join("bin/claude"));
    assert_eq!(
        config.agents.claude_mcp_config,
        dir.path().join("connections.json")
    );
    let runner = maistro::agent::AgentRunner::new(config.clone());
    let transport = runner.mcp_config(&config.projects[0]).unwrap();
    assert_eq!(transport["url"].as_str(), Some("https://example.test/mcp"));
    assert_eq!(
        transport["http_headers"]["Authorization"].as_str(),
        Some("Bearer fixture-only")
    );
    fs::write(
        dir.path().join("connections.json"),
        r#"{"mcpServers":{"team":{"type":"sse","url":"https://example.test/sse"}}}"#,
    )
    .unwrap();
    assert!(runner.mcp_config(&config.projects[0]).is_err());
    fs::write(dir.path().join("connections.json"), r#"{"mcpServers":{}}"#).unwrap();
    assert!(
        runner
            .mcp_config(&config.projects[0])
            .unwrap_err()
            .to_string()
            .contains("team missing")
    );
}

#[test]
fn optional_states_preserve_defaults_and_validate_enabled_labels() {
    let base = "[[projects]]\nid='p'\nrepo='/tmp'\nappflowy_page='Board'\nmode='master'\nbase_branch='main'\n";
    let defaults: Config = toml::from_str(base).unwrap();
    assert_eq!(
        defaults.projects[0].statuses.review.as_deref(),
        Some("Review")
    );
    assert_eq!(
        defaults.projects[0].statuses.cancelled.as_deref(),
        Some("Cancelled")
    );
    for settings in [
        "review=false",
        "cancelled=false",
        "review=false\ncancelled=false",
    ] {
        let config: Config =
            toml::from_str(&format!("{base}[projects.statuses]\n{settings}")).unwrap();
        config.validate().unwrap();
        let statuses = &config.projects[0].statuses;
        let json = serde_json::to_value(statuses).unwrap();
        for name in ["review", "cancelled"] {
            if settings.contains(&format!("{name}=false")) {
                assert_eq!(json[name], false);
            }
        }
        let encoded = toml::to_string(statuses).unwrap();
        let decoded: maistro::config::Statuses = toml::from_str(&encoded).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), json);
    }
    for settings in [
        "review=true",
        "cancelled=true",
        "ready=false",
        "backlog=false",
        "in_progress=false",
        "done=false",
        "blocked=false",
        "review=''",
        "cancelled='  '",
        "review='Done'",
        "cancelled='Ready'",
    ] {
        let parsed = toml::from_str::<Config>(&format!("{base}[projects.statuses]\n{settings}"));
        assert!(
            parsed.is_err() || parsed.unwrap().validate().is_err(),
            "{settings}"
        );
    }
    let pr = base.replace("mode='master'", "mode='pr'");
    let disabled: Config =
        toml::from_str(&format!("{pr}[projects.statuses]\nreview=false")).unwrap();
    assert!(
        disabled
            .validate()
            .unwrap_err()
            .to_string()
            .contains("review")
    );
    let cancelled: Config =
        toml::from_str(&format!("{pr}[projects.statuses]\ncancelled=false")).unwrap();
    cancelled.validate().unwrap();
}

#[test]
fn disabled_review_prevents_pr_workers_including_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let mut project: maistro::config::Project = serde_json::from_value(serde_json::json!({
        "id":"p", "repo":dir.path(), "appflowy_page":"Board",
        "mode":"master", "base_branch":"main", "statuses":{"review":false,"cancelled":false}
    }))
    .unwrap();
    let mut story = assignment("p");
    maistro::agent::worker_prompt(&project, &story).unwrap();
    story.pr = Some(PrRequest { branch: None });
    for recovery in [false, true] {
        story.recovery = recovery;
        let error = maistro::agent::worker_prompt(&project, &story).unwrap_err();
        assert!(error.to_string().contains("review is disabled"));
    }
    project.statuses.review = Some("Review".into());
    maistro::agent::worker_prompt(&project, &story).unwrap();
}

#[test]
fn shortcut_api_config_requires_workspace_and_private_file_reference_together() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::create_dir(dir.path().join("repo")).unwrap();
    let base = "[orchestrator]\nruntime_dir='runtime'\n[[projects]]\nid='p'\nrepo='repo'\nshortcut_workflow_id=42\nmode='master'\nbase_branch='main'\n";
    fs::write(
        &path,
        format!("{base}shortcut_workspace='team'\nshortcut_token_file='token'\n"),
    )
    .unwrap();
    let config = Config::load(&path).unwrap();
    let p = &config.projects[0];
    assert!(p.shortcut_api());
    assert_eq!(
        p.shortcut_token_file.as_ref().unwrap(),
        &dir.path().join("token")
    );
    for settings in [
        "shortcut_workspace='team'",
        "shortcut_token_file='token'",
        "shortcut_workspace='../other'\nshortcut_token_file='token'",
        "shortcut_workspace='team'\nshortcut_token_file=''",
    ] {
        fs::write(&path, format!("{base}{settings}\n")).unwrap();
        assert!(Config::load(&path).is_err(), "{settings}");
    }
    fs::write(
        &path,
        format!(
            "{}shortcut_workspace='team'\nshortcut_token_file='token'\n",
            base.replace("shortcut_workflow_id=42", "appflowy_page='Board'")
        ),
    )
    .unwrap();
    assert!(Config::load(&path).is_err());
    fs::write(
        &path,
        format!(
            "{}shortcut_workspace='team'\nshortcut_token_file='token'\n",
            base.replace(
                "shortcut_workflow_id=42",
                "shortcut_url='https://app.shortcut.com/other/settings/workflows/42'"
            )
        ),
    )
    .unwrap();
    assert!(Config::load(&path).is_err());
}

#[test]
fn project_worker_settings_inherit_independently_and_validate() {
    let project = "[[projects]]\nid='p'\nrepo='/tmp'\nappflowy_page='Stories'\nmode='master'\nbase_branch='main'\n";
    for (provider, default, wrong_model) in [
        ("codex", "gpt-6-astra", "claude-sonnet-4"),
        ("claude", "opus", "gpt-6-astra"),
    ] {
        for global in [false, true] {
            for model in [None, Some("custom-worker")] {
                for effort in [None, Some("low")] {
                    let mut text = format!("[agents]\nprovider='{provider}'\n");
                    if global {
                        text.push_str("worker='custom-global'\nworker_effort='high'\n");
                    }
                    text.push_str(project);
                    if let Some(model) = model {
                        text.push_str(&format!("worker='{model}'\n"));
                    }
                    if let Some(effort) = effort {
                        text.push_str(&format!("worker_effort='{effort}'\n"));
                    }
                    let config: Config = toml::from_str(&text).unwrap();
                    config.validate().unwrap();
                    let p = &config.projects[0];
                    let serialized = serde_json::to_value(p).unwrap();
                    assert_eq!(serialized["worker"], serde_json::json!(model));
                    assert_eq!(serialized["worker_effort"], serde_json::json!(effort));
                    assert_eq!(
                        p.worker(&config.agents),
                        model.unwrap_or(if global { "custom-global" } else { default })
                    );
                    assert_eq!(
                        p.worker_effort(&config.agents),
                        effort.or(global.then_some("high"))
                    );
                }
            }
        }
        for (field, value) in [
            ("worker", ""),
            ("worker", "--option"),
            ("worker", wrong_model),
            ("worker_effort", "HIGH"),
            ("worker_effort", "unknown"),
        ] {
            let config: Config = toml::from_str(&format!(
                "[agents]\nprovider='{provider}'\n{project}{field}='{value}'\n"
            ))
            .unwrap();
            let error = config.validate().unwrap_err().to_string();
            assert!(error.contains(&format!("projects.p.{field}")), "{error}");
            assert!(error.contains(provider), "{error}");
            if field.ends_with("effort") {
                assert!(error.contains("supported values"), "{error}");
            }
        }
    }
}
