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
