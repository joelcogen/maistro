#![allow(dead_code)]
use maistro::{
    config::{Config, Project},
    model::{Decision, Scan, Status, Story},
};
use std::path::Path;
pub fn config(root: &Path) -> Config {
    std::fs::create_dir_all(root.join("repo-a")).unwrap();
    std::fs::create_dir_all(root.join("repo-b")).unwrap();
    let text = format!(
        r#"
[orchestrator]
state_db="{0}/state.db"
worktree_root="{0}/worktrees"
log_root="{0}/logs"
[[projects]]
id="a"
name="A"
repo="{0}/repo-a"
appflowy_page="Board A"
github_repo="owner/a"
required_checks=["test"]
test_commands=[["true"]]
[[projects]]
id="b"
name="B"
repo="{0}/repo-b"
appflowy_page="Board B"
github_repo="owner/b"
required_checks=["test"]
test_commands=[["true"]]
"#,
        root.display()
    );
    toml::from_str(&text).unwrap()
}
pub fn story(status: Status) -> Story {
    Story {
        story_id: uuid::Uuid::new_v4().to_string(),
        title: "Example".into(),
        description: "Do the thing".into(),
        status,
        priority: 1,
        dependencies: vec![],
        reference_pages: vec![],
        acceptance_criteria: vec!["It works".into()],
        decision: Decision::Implement,
        reason: "Scoped".into(),
    }
}
pub fn scan(p: &Project, stories: Vec<Story>) -> Scan {
    Scan {
        project_id: p.id.clone(),
        workspace_id: uuid::Uuid::new_v4().to_string(),
        board_id: uuid::Uuid::new_v4().to_string(),
        view_id: uuid::Uuid::new_v4().to_string(),
        matches: 1,
        complete: true,
        fields_readable: true,
        stories,
    }
}
