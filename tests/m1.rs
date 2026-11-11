use maistro::{
    config::Config,
    model::{Decision, Scan, Status, Story},
    process,
};
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
fn config(root: &std::path::Path) -> Config {
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    let text = format!(
        r#"
[orchestrator]
[[projects]]
id="a"
name="A"
repo="{}/a"
appflowy_page="A board"
github_repo="owner/a"
[[projects]]
id="b"
name="B"
repo="{}/b"
appflowy_url="https://appflowy.com/board/b"
github_repo="owner/b"
"#,
        root.display(),
        root.display()
    );
    toml::from_str(&text).unwrap()
}
#[test]
fn omitted_runtime_paths_use_user_state_directory() {
    let t = tempfile::tempdir().unwrap();
    let c = config(t.path());
    let mut value = toml::Value::try_from(&c).unwrap();
    let runtime = value["orchestrator"].as_table_mut().unwrap();
    for key in ["state_db", "worktree_root", "log_root"] {
        runtime.remove(key);
    }
    let file = t.path().join("config.toml");
    std::fs::write(&file, toml::to_string(&value).unwrap()).unwrap();
    let loaded = Config::load(&file).unwrap();
    let state =
        std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/state/maistro");
    let mut wizard = c.clone();
    wizard.resolve_runtime_paths(t.path()).unwrap();
    assert_eq!(wizard.orchestrator.state_db, state.join("state.db"));
    assert_eq!(wizard.orchestrator.worktree_root, state.join("worktrees"));
    assert_eq!(wizard.orchestrator.log_root, state.join("logs"));
    assert_eq!(loaded.orchestrator.state_db, state.join("state.db"));
    assert_eq!(loaded.orchestrator.worktree_root, state.join("worktrees"));
    assert_eq!(loaded.orchestrator.log_root, state.join("logs"));

    value["orchestrator"]
        .as_table_mut()
        .unwrap()
        .insert("log_root".into(), "custom-logs".into());
    std::fs::write(&file, toml::to_string(&value).unwrap()).unwrap();
    let loaded = Config::load(&file).unwrap();
    assert_eq!(loaded.orchestrator.state_db, state.join("state.db"));
    assert_eq!(loaded.orchestrator.log_root, t.path().join("custom-logs"));
}

#[test]
fn two_projects_and_invalid_config() {
    let t = tempfile::tempdir().unwrap();
    let mut c = config(t.path());
    c.validate().unwrap();
    c.projects[1].id = "a".into();
    assert!(c.validate().is_err());
    c.projects[1].id = "b".into();
    c.projects[1].appflowy_page = Some("ambiguous".into());
    assert!(c.validate().is_err());
}
#[test]
fn strict_contract_and_grounded_identifiers() {
    let id = uuid::Uuid::new_v4().to_string();
    let view_id = uuid::Uuid::new_v4().to_string();
    let evidence = format!("{id} {view_id}");
    let mut scan = Scan {
        project_id: "a".into(),
        workspace_id: id.clone(),
        board_id: id.clone(),
        view_id: view_id.clone(),
        matches: 1,
        complete: true,
        fields_readable: true,
        stories: vec![],
    };
    scan.validate("a", &evidence).unwrap();
    scan.matches = 2;
    assert!(scan.validate("a", &evidence).is_err());
    scan.matches = 1;
    scan.view_id = scan.board_id.clone();
    assert!(scan.validate("a", &evidence).is_err());
    scan.view_id = view_id;
    assert!(scan.validate("a", "").is_err());
    let mut v = serde_json::to_value(scan).unwrap();
    v["unknown"] = true.into();
    assert!(serde_json::from_value::<Scan>(v).is_err());
}
#[test]
fn revision_ignores_projection_but_detects_requirements() {
    let mut s = Story {
        story_id: "s".into(),
        title: "Test".into(),
        description: "Body".into(),
        status: Status::Ready,
        priority: 1,
        dependencies: vec![],
        acceptance_criteria: vec!["works".into()],
        decision: Decision::Implement,
        reason: String::new(),
    };
    let r = s.revision();
    s.status = Status::Review;
    assert_eq!(r, s.revision());
    s.acceptance_criteria.push("more".into());
    assert_ne!(r, s.revision());
}
#[tokio::test]
async fn timeout_and_exit_are_not_success() {
    let mut c = Command::new("sleep");
    c.arg("5");
    assert!(
        process::run(
            c,
            "",
            Duration::from_millis(30),
            &CancellationToken::new(),
            None
        )
        .await
        .is_err()
    );
    let o = process::run(
        Command::new("false"),
        "",
        Duration::from_secs(2),
        &CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    assert_ne!(o.code, 0);
}
#[tokio::test]
async fn cancellation_terminates() {
    let mut c = Command::new("sleep");
    c.arg("5");
    let token = CancellationToken::new();
    token.cancel();
    assert!(
        process::run(c, "", Duration::from_secs(5), &token, None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn untrusted_text_is_passed_as_an_argument() {
    let mut cmd = Command::new("printf");
    cmd.args([
        "%s",
        "$(touch /tmp/maistro-injection-must-not-exist); literal",
    ]);
    let result = process::run(
        cmd,
        "",
        Duration::from_secs(2),
        &CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        result.stdout,
        "$(touch /tmp/maistro-injection-must-not-exist); literal"
    );
}

#[tokio::test]
async fn only_guarded_projection_tools_are_preapproved() {
    use maistro::codex::{Codex, Invocation};
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let mut c = config(t.path());
    c.agents.codex_home = t.path().join("codex-home");
    std::fs::create_dir(&c.agents.codex_home).unwrap();
    std::fs::write(
        c.agents.codex_home.join("config.toml"),
        "[mcp_servers.appflowy]\ncommand='original-server'\n",
    )
    .unwrap();
    c.agents.codex = t.path().join("fake-codex");
    std::fs::write(
        &c.agents.codex,
        r#"#!/usr/bin/env python3
import sys,json,pathlib
a=sys.argv[1:]
sys.stdin.read()
pathlib.Path(a[a.index('-o')+1]).write_text('{}')
(pathlib.Path(a[a.index('-C')+1])/'captured-args.json').write_text(json.dumps(a))
print('{"type":"turn.completed"}')
"#,
    )
    .unwrap();
    std::fs::set_permissions(&c.agents.codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let codex = Codex::new(c);
    let scope = t.path().join("committed-scope.json");
    for write in [false, true] {
        codex
            .invoke(
                Invocation {
                    model: "fixture",
                    cwd: t.path(),
                    prompt: "fixture",
                    kind: "projection",
                    mcp: Some(write),
                    scope: write.then_some(scope.as_path()),
                    log_dir: t.path(),
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let args: Vec<String> =
            serde_json::from_slice(&std::fs::read(t.path().join("captured-args.json")).unwrap())
                .unwrap();
        std::fs::remove_file(t.path().join("captured-args.json")).unwrap();
        let setting = args
            .iter()
            .find(|a| a.starts_with("mcp_servers.appflowy="))
            .unwrap();
        let parsed: toml::Value = toml::from_str(setting).unwrap();
        let server = &parsed["mcp_servers"]["appflowy"];
        let allowed = server["enabled_tools"].as_array().unwrap();
        assert!(!allowed.iter().any(|v| v.as_str() == Some("delete_row")));
        if write {
            assert_eq!(server["args"][0].as_str(), Some("internal-mcp"));
            assert_eq!(server["args"][2].as_str(), scope.to_str());
            for tool in ["update_row_cells", "append_comment"] {
                assert_eq!(
                    server["tools"][tool]["approval_mode"].as_str(),
                    Some("approve")
                );
            }
        } else {
            assert_eq!(server["command"].as_str(), Some("original-server"));
            assert!(server.get("tools").is_none());
            assert!(
                !allowed
                    .iter()
                    .any(|v| v.as_str() == Some("update_row_cells"))
            );
        }
    }
}
