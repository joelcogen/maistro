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
fn config_resolves_paths_relative_to_config_and_rejects_legacy_prompt_file() {
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
    fs::write(&path, format!("{config}prompt_file='project.md'\n")).unwrap();
    assert!(Config::load(&path).is_err());
    fs::write(&path, config.replace("mode='master'", "mode='merge'")).unwrap();
    assert!(Config::load(&path).is_err());
}

#[test]
fn project_prompt_is_reloaded_for_each_assignment_and_git_modes_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::create_dir(dir.path().join("repo")).unwrap();
    fs::write(
        dir.path().join("repo/maistro-worker.md"),
        "First test command",
    )
    .unwrap();
    fs::write(&path, "[orchestrator]\nruntime_dir='runtime'\n[[projects]]\nid='p0'\nrepo='repo'\nappflowy_page='Stories'\nmode='pr'\nbase_branch='trunk'\n").unwrap();
    let mut project = Config::load(&path).unwrap().projects.remove(0);
    let first = maistro::agent::worker_prompt(&project, &assignment("p0")).unwrap();
    assert!(first.contains("First test command") && first.contains("maistro/p0/story-1"));
    assert!(first.contains("PR MODE:") && !first.contains("MASTER MODE:"));
    fs::write(
        dir.path().join("repo/maistro-worker.md"),
        "Revised test command",
    )
    .unwrap();
    project.mode = Mode::Master;
    let second = maistro::agent::worker_prompt(&project, &assignment("p0")).unwrap();
    assert!(second.contains("Revised test command") && second.contains("\"story_branch\":null"));
    assert!(second.contains("\"base_branch\":\"trunk\""));
    assert!(second.contains("MASTER MODE:") && !second.contains("PR MODE:"));

    let mut story = assignment("p0");
    // Product requirements about PR support must not trigger a text-based override.
    story.context = "Add support for PRs and custom branch names in master mode".into();
    assert_eq!(story.effective_mode(project.mode), Mode::Master);
    story.pr = Some(PrRequest { branch: None });
    let overridden = maistro::agent::worker_prompt(&project, &story).unwrap();
    assert!(overridden.contains("\"mode\":\"pr\""));
    assert!(overridden.contains("\"story_branch\":\"maistro/p0/story-1\""));
    assert!(overridden.contains("PR MODE:") && !overridden.contains("MASTER MODE:"));

    for mode in [Mode::Master, Mode::Pr] {
        project.mode = mode;
        story.pr.as_mut().unwrap().branch = Some("feature/requested-branch".into());
        let custom = maistro::agent::worker_prompt(&project, &story).unwrap();
        assert!(custom.contains("\"story_branch\":\"feature/requested-branch\""));
        assert!(custom.contains("\"base_branch\":\"trunk\""));
        assert!(custom.contains("\"remote\":\"origin\""));
        assert!(custom.contains("\"story_marker\":\"AppFlowy story: story-1\""));
        for invalid in [
            "", "trunk", "HEAD", "-option", "bad name", "a..b", "@{-1}", "a.lock",
        ] {
            story.pr.as_mut().unwrap().branch = Some(invalid.into());
            assert!(
                maistro::agent::worker_prompt(&project, &story).is_err(),
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

// Shortcut pagination runs in the model's MCP connection. These tests validate
// the executable request examples and required guidance, not simulated API reads.
fn coordinator_rules() -> String {
    include_str!("../prompts/coordinator.md")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn shortcut_multi_page_examples_preserve_fields_and_decode_only_cursor_once() {
    let prompt = include_str!("../prompts/coordinator.md");
    let examples = prompt
        .split("```json\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let pages: Vec<serde_json::Value> = serde_json::from_str(examples).unwrap();
    assert!(pages.len() >= 3, "exercise more than one continuation");
    assert_eq!(pages[0]["request"]["query"], "is:story");
    let fields = pages[0]["request"]["fields"].as_str().unwrap();
    for required in [
        "id",
        "workflow",
        "workflow_state",
        "archived",
        "position",
        "created_at",
    ] {
        assert!(fields.split(',').any(|field| field == required));
    }
    let mut seen = std::collections::HashSet::new();
    for pair in pages.windows(2) {
        let url = reqwest::Url::parse(pair[0]["next_page_url"].as_str().unwrap()).unwrap();
        let tokens: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key == "cursor")
            .collect();
        assert_eq!(tokens.len(), 1);
        let token = &tokens[0].1;
        assert!(!token.is_empty() && seen.insert(token.to_string()));
        let next = pair[1]["request"].as_object().unwrap();
        assert_eq!(
            next.len(),
            2,
            "no query, limit, filter or ordering on continuation"
        );
        assert_eq!(next["cursor"], token.as_ref());
        assert_eq!(next["fields"], fields);
        assert_ne!(
            next["cursor"],
            url.as_str(),
            "full URLs cause Invalid cursor"
        );
    }
    // Preserve a literal encoded substring inside the opaque token: no second decode.
    assert_eq!(pages[1]["request"]["cursor"], "page+2/=%2F");
    assert!(pages.last().unwrap()["next_page_url"].is_null());
    let rules = coordinator_rules();
    for required in [
        "URL-decode its value exactly once",
        "cursor and fields must be the ONLY query arguments on subsequent calls",
        "SAME required path identifiers",
        "Even a page containing no matching stories must be followed",
        "an empty page alone does not prove completion",
    ] {
        assert!(
            rules.contains(required),
            "missing pagination rule: {required}"
        );
    }
}

#[test]
fn shortcut_scope_contract_requires_supported_listing_or_complete_filtered_fallback() {
    let rules = coordinator_rules();
    for required in [
        "list_workflow_stories with the verified workflow_public_id",
        "Use a workflow-scoped search only if its schema explicitly supports that scope",
        "Do not invent a workflow: search operator",
        "paginate list_stories across the entire workspace",
        "search_stories with query=\"is:story\"",
        "Never send an empty or whitespace-only search",
        "Verify each story's numeric workflow ID against the resolved ID",
        "its state ID against that workflow's states",
        "Exclude archived and other-workflow stories from counts and candidates",
        "Missing membership/status fields require a direct read",
        "Count unique story IDs only after all pages are read",
        "never use workspace total_items as the configured workflow's count",
        "112 in Platform contribute only 3 to Agents' backlog",
    ] {
        assert!(
            rules.contains(required),
            "missing workflow isolation rule: {required}"
        );
    }
}

#[test]
fn shortcut_pagination_failure_contract_forbids_partial_counts_and_assignments() {
    let rules = coordinator_rules();
    for required in [
        "Missing/empty/duplicate cursor parameters",
        "malformed URLs/encoding, repeated cursors, inconsistent totals",
        "ANY page error make the read incomplete",
        "Do not combine a restarted read with pages from an earlier attempt",
        "do not claim or assign ANY story for that project, including recovery from an early page",
        "read=partial (failed if no page succeeded)",
        "counts=null and counted_assignment=null",
        "failed enumeration alone never proves deletion or movement",
        "If page 2 fails, even a Ready story on page 1 cannot authorize an assignment",
    ] {
        assert!(
            rules.contains(required),
            "missing pagination failure rule: {required}"
        );
    }
}

#[test]
fn shortcut_configuration_is_exclusive_scoped_and_backward_compatible() {
    let old = "[[projects]]\nid='p'\nrepo='/tmp'\nappflowy_page='Board'\nmode='master'\nbase_branch='master'\n";
    let old_config: Config = toml::from_str(old).unwrap();
    old_config.validate().unwrap();
    assert_eq!(
        old_config.projects[0].mcp_server(&old_config.agents),
        "appflowy"
    );
    let shortcut = old.replace(
        "appflowy_page='Board'",
        "shortcut_url='https://app.shortcut.com/team/settings/workflows/42'",
    );
    let config: Config = toml::from_str(&shortcut).unwrap();
    config.validate().unwrap();
    assert_eq!(config.projects[0].mcp_server(&config.agents), "shortcut");
    assert_eq!(config.projects[0].source_name(), "Shortcut");
    let prompt = maistro::agent::worker_prompt(&config.projects[0], &assignment("p")).unwrap();
    assert!(prompt.contains("Shortcut story: story-1"));
    assert!(prompt.contains("access AppFlowy or Shortcut"));
    for invalid in [
        shortcut.replace("shortcut_url=", "appflowy_page='Board'\nshortcut_url="),
        old.replace("appflowy_page='Board'", ""),
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
        old.replace("id='p'", "id='p2'")
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
        assert_eq!(project.is_shortcut(), shortcut);
        assert_eq!(
            project.mcp_server(&config.agents),
            if shortcut { "shortcut" } else { "appflowy" }
        );
        if index >= 3 {
            assert_eq!(project.board_url(), None);
            let prompt = maistro::agent::worker_prompt(project, &assignment("p")).unwrap();
            assert!(prompt.contains("Shortcut story: story-1"));
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
        "update_row_cells",
        "list_workspace_tokens",
    ] {
        assert!(
            !enabled.contains(&toml::Value::from(tool)),
            "unexpected {tool}"
        );
    }
    // Every selector must expose the same workflow resolution and lifecycle tools.
    for by_id in [false, true] {
        let mut selected = config.projects[0].clone();
        selected.shortcut_url = None;
        if by_id {
            selected.shortcut_workflow_id = Some(500005096);
        } else {
            selected.shortcut_workflow = Some("Agents".into());
        }
        assert_eq!(codex.mcp_config(&selected).unwrap(), shortcut);
    }
    config.projects[0].shortcut_url = None;
    config.projects[0].appflowy_page = Some("Board".into());
    config.projects[0].mcp_server = None;
    let appflowy = codex.mcp_config(&config.projects[0]).unwrap();
    assert_eq!(appflowy["command"].as_str(), Some("fixture-appflowy"));
    assert!(
        appflowy["enabled_tools"]
            .as_array()
            .unwrap()
            .contains(&toml::Value::from("update_row_cells"))
    );
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
    assert_eq!(config.agents.small_model(), "gpt-6-luna");
    assert_eq!(config.agents.big_model(), "gpt-6-astra");
    let config: Config =
        toml::from_str(&format!("[agents]\nprovider='claude'\n{project}")).unwrap();
    assert_eq!(config.agents.small_model(), "haiku");
    assert_eq!(config.agents.big_model(), "opus");
    for provider in ["codex", "claude"] {
        let config: Config = toml::from_str(&format!("[agents]\nprovider='{provider}'\nsmall_model='custom-small'\nbig_model='custom-big'\n{project}")).unwrap();
        config.validate().unwrap();
        assert_eq!(config.agents.small_model(), "custom-small");
        assert_eq!(config.agents.big_model(), "custom-big");
        for field in ["small_model", "big_model"] {
            for value in ["", "   ", "--bad-option", "bad\\nmodel"] {
                let config: Config = toml::from_str(&format!(
                    "[agents]\nprovider='{provider}'\n{field}=\"{value}\"\n{project}"
                ))
                .unwrap();
                assert!(config.validate().is_err());
            }
        }
    }
    assert!(toml::from_str::<Config>(&format!("[agents]\nprovider='other'\n{project}")).is_err());
    for field in [
        "provider='claude'",
        "small_model='other'",
        "big_model='other'",
        "small_model_effort='low'",
        "big_model_effort='high'",
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
        assert_eq!(config.agents.small_model_effort, None);
        assert_eq!(config.agents.big_model_effort, None);
        for field in ["small_model_effort", "big_model_effort"] {
            for value in &supported {
                let config: Config = toml::from_str(&format!(
                    "[agents]\nprovider='{provider}'\n{field}='{value}'\n{project}"
                ))
                .unwrap();
                config.validate().unwrap();
                let (selected, other) = if field == "small_model_effort" {
                    (
                        &config.agents.small_model_effort,
                        &config.agents.big_model_effort,
                    )
                } else {
                    (
                        &config.agents.big_model_effort,
                        &config.agents.small_model_effort,
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
    for source in [
        "appflowy_page='Board'",
        "shortcut_url='https://app.shortcut.com/team/settings/workflows/42'",
        "shortcut_workflow='Agents'",
        "shortcut_workflow_id=500005096",
    ] {
        let base = format!(
            "[[projects]]\nid='p'\nrepo='/tmp'\n{source}\nmode='master'\nbase_branch='main'\n"
        );
        let defaults: Config = toml::from_str(&base).unwrap();
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
            assert_eq!(decoded.review, statuses.review);
            assert_eq!(decoded.cancelled, statuses.cancelled);
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
            "review=false\ncancelled='Done'",
        ] {
            let parsed =
                toml::from_str::<Config>(&format!("{base}[projects.statuses]\n{settings}"));
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
    assert!(
        maistro::agent::worker_prompt(&project, &story)
            .unwrap()
            .contains("MASTER MODE:")
    );
    story.pr = Some(PrRequest { branch: None });
    for recovery in [false, true] {
        story.recovery = recovery;
        let error = maistro::agent::worker_prompt(&project, &story).unwrap_err();
        assert!(error.to_string().contains("review is disabled"));
    }
    project.statuses.review = Some("Review".into());
    assert!(
        maistro::agent::worker_prompt(&project, &story)
            .unwrap()
            .contains("PR MODE:")
    );
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
    assert!(
        !serde_json::to_value(p)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("shortcut_token_file")
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
