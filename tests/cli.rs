mod common;

use maistro::{model::Status, store::Store};
use std::{path::Path, process::Command};

fn cli(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_maistro"));
    command.current_dir(root);
    command
}

fn output(root: &Path, args: &[&str]) -> String {
    let result = cli(root).args(args).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

#[test]
fn human_output_and_compatible_json_cover_projects_runs_and_controls() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let config = common::config(root);
    std::fs::write(root.join("config.toml"), toml::to_string(&config).unwrap()).unwrap();
    let projects = output(root, &["projects"]);
    assert!(projects.contains("Projects (2)"));
    assert!(projects.contains("a — A (enabled)"));
    assert!(projects.contains("Board: Board A"));
    let projects: serde_json::Value =
        serde_json::from_str(&output(root, &["projects", "--json"])).unwrap();
    assert_eq!(projects, serde_json::to_value(&config.projects).unwrap());
    assert!(output(root, &["status"]).contains("No runs recorded."));
    assert!(output(root, &["pause"]).contains("Pause requested"));
    assert!(output(root, &["status"]).contains("Dispatch: paused"));
    assert!(output(root, &["--json", "resume"]).is_empty());

    let store = Store::open(&config.orchestrator.state_db).unwrap();
    let scan = common::scan(&config.projects[0], vec![common::story(Status::Ready)]);
    store.record_scan(&scan).unwrap();
    store
        .ingest(
            &config.projects[0],
            &scan,
            &config.orchestrator.worktree_root,
        )
        .unwrap();
    let mut run = store.runs().unwrap().remove(0);
    run.last_error = Some("Example failure\nMore context".into());
    run.pr_url = Some("https://github.com/owner/a/pull/1".into());
    let state = run.state;
    store.save(&mut run, state, "CLI fixture").unwrap();
    let human = output(root, &["status"]);
    for expected in [
        &run.id,
        &run.story.story_id,
        "Example",
        "Example failure",
        "More context",
        "https://github.com/owner/a/pull/1",
    ] {
        assert!(human.contains(expected), "missing {expected}: {human}");
    }
    let json: serde_json::Value =
        serde_json::from_str(&output(root, &["--json", "status"])).unwrap();
    assert_eq!(json, store.diagnostics().unwrap());
    assert!(output(root, &["cancel", &run.id]).contains("Cancellation requested"));
    assert_eq!(
        store
            .control(&format!("cancel:{}", run.id))
            .unwrap()
            .as_deref(),
        Some("true")
    );
}

#[test]
fn config_uses_editor_arguments_and_literal_path_without_loading_toml() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let script = root.join("fake editor.sh");
    std::fs::write(&script, "test \"$1\" = '--wait' || exit 8\nprintf '%s' \"$2\" > selected-path\nprintf 'edited' > \"$2\"\n").unwrap();
    let name = "-config $(touch unexpected).toml";
    std::fs::write(root.join(name), "invalid TOML [").unwrap();
    let result = cli(root)
        .arg("config")
        .arg(format!("--config={name}"))
        .env("EDITOR", format!("sh '{}' --wait", script.display()))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(root.join("selected-path")).unwrap(),
        root.join(name).to_str().unwrap()
    );
    assert_eq!(std::fs::read_to_string(root.join(name)).unwrap(), "edited");
    assert!(!root.join("unexpected").exists());
    // The editor can also create a missing default config.toml.
    assert!(
        cli(root)
            .arg("config")
            .env("EDITOR", format!("sh '{}' --wait", script.display()))
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        std::fs::read_to_string(root.join("config.toml")).unwrap(),
        "edited"
    );
}

#[test]
fn config_reports_missing_empty_and_failing_editors() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let missing = cli(root)
        .arg("config")
        .env_remove("EDITOR")
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("Set $EDITOR"));
    for editor in [" ", "false", "maistro-nonexistent-editor"] {
        let result = cli(root)
            .arg("config")
            .env("EDITOR", editor)
            .output()
            .unwrap();
        assert!(
            !result.status.success(),
            "editor {editor} unexpectedly succeeded"
        );
        assert!(String::from_utf8_lossy(&result.stderr).contains("$EDITOR"));
    }
}

#[test]
fn create_board_uses_setup_without_a_project_and_checks_mcp_evidence() {
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let workspace = uuid::Uuid::new_v4().to_string();
    let parent = uuid::Uuid::new_v4().to_string();
    let database = uuid::Uuid::new_v4().to_string();
    let view = uuid::Uuid::new_v4().to_string();
    let board = json!({"workspace_id":workspace,"database_id":database,"view_id":view});
    let fields = json!([{"id":"status","name":"Status","type_option":{"options":[
        {"name":"Backlog","color":"iron"}, {"name":"Ready","color":"azure"},
        {"name":"In progress","color":"Mango"}, {"name":"Review","color":"mauve"},
        {"name":"Done","color":"jade"}, {"name":"Blocked","color":"camelia"},
        {"name":"Cancelled","color":"iron"}
    ]}}]);
    let calls = json!([
        {"tool":"create_database","arguments":{"workspace_id":workspace,"parent_view_id":parent,"name":"Board","layout":"Board"},"result":{"content":[{"text":board.to_string()}]}},
        {"tool":"get_database_fields","arguments":{"workspace_id":workspace,"database_id":database},"result":{"content":[{"text":fields.to_string()}]}}
    ]);
    std::fs::write(
        root.join("fixture.json"),
        json!({"board":board,"calls":calls}).to_string(),
    )
    .unwrap();
    let fake = root.join("fake-codex");
    std::fs::write(&fake, r##"#!/usr/bin/env python3
import json, pathlib, sys
root = pathlib.Path(__file__).parent
fixture = json.loads((root / 'fixture.json').read_text())
prompt = sys.stdin.read()
assert 'parent_view_id' in prompt and 'camelia' in prompt
assert '--sandbox' in sys.argv and 'read-only' in sys.argv
settings = '\n'.join(sys.argv)
assert 'create_database' in settings and 'delete_database' not in settings
assert 'internal-board-mcp' in settings and 'scope.json' in settings
pathlib.Path(sys.argv[sys.argv.index('-o') + 1]).write_text(json.dumps(fixture['board']))
for call in fixture['calls']:
    print(json.dumps({'type':'item.completed', 'item':dict(call, type='mcp_tool_call', status='completed')}))
print(json.dumps({'type':'turn.completed'}))
"##).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = root.join("codex-home");
    std::fs::create_dir(&home).unwrap();
    // This command must never be started by the fixture.
    std::fs::write(
        home.join("config.toml"),
        "[mcp_servers.appflowy]\ncommand = 'must-not-be-executed'\n",
    )
    .unwrap();
    std::fs::write(
        root.join("config.toml"),
        format!("[agents]\ncodex = {:?}\ncodex_home = {:?}\n", fake, home),
    )
    .unwrap();
    let url = format!("https://appflowy.com/app/{workspace}/{parent}");
    let result: serde_json::Value =
        serde_json::from_str(&output(root, &["--json", "create-board", &url])).unwrap();
    assert_eq!(result, board);
    assert!(output(root, &["create-board", &url]).contains(&format!(
        "Created Board: https://appflowy.com/app/{workspace}/{view}"
    )));
    assert!(!root.join("state.db").exists());
    let invalid = cli(root)
        .args(["create-board", "not-a-page-url"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
    assert!(output(root, &["create-board", "--help"]).contains("<PAGE_URL>"));
    let missing = cli(root).arg("create-board").output().unwrap();
    assert!(!missing.status.success());
    // A model's success report alone must never acknowledge board setup.
    std::fs::write(
        root.join("fixture.json"),
        json!({"board":board,"calls":[]}).to_string(),
    )
    .unwrap();
    let failed = cli(root)
        .args(["--json", "create-board", &url])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    let error = String::from_utf8(failed.stderr).unwrap();
    assert!(error.contains("missing successful board creation receipt"));
    assert!(error.contains("a partial board may exist"));
    // The retained logs are fixtures, not user diagnostics.
    let log_dir = error
        .split("private logs at ")
        .nth(1)
        .unwrap()
        .split(" and the parent page")
        .next()
        .unwrap();
    std::fs::remove_dir_all(log_dir).unwrap();
}
