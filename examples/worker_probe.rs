//! Explicit live deployment probe: a real Astra call in a disposable sandbox only.
use anyhow::Result;
use maistro::{
    codex::Codex,
    config::{Config, Project},
    model::{Decision, Scan, Status, Story},
    store::Store,
};
#[tokio::main]
async fn main() -> Result<()> {
    unsafe {
        libc::umask(0o077);
    }
    let t = tempfile::tempdir()?;
    let repo = t.path().join("source");
    std::fs::create_dir(&repo)?;
    let project = Project {
        id: "probe".into(),
        name: "Disposable sandbox probe".into(),
        repo: repo.clone(),
        appflowy_url: None,
        appflowy_page: Some("unused".into()),
        base_branch: "main".into(),
        max_workers: 1,
        enabled: true,
        github_repo: "unused/probe".into(),
        required_checks: vec![],
        test_commands: vec![vec!["true".into()]],
        statuses: Default::default(),
        protected_paths: vec![],
        allowed_paths: vec![],
        toolchain_paths: vec![],
    };
    let mut config = Config {
        orchestrator: Default::default(),
        agents: Default::default(),
        github: Default::default(),
        projects: vec![project.clone()],
    };
    config.orchestrator.state_db = t.path().join("state.db");
    config.orchestrator.log_root = t.path().join("logs");
    config.orchestrator.worktree_root = t.path().join("worktrees");
    config.orchestrator.process_timeout_seconds = 180;
    let story=Story{story_id:uuid::Uuid::new_v4().to_string(),title:"Sandbox connectivity probe".into(),description:"Create only probe.txt in /work with the exact contents sandbox-ok followed by a newline. This is a disposable fixture. Do not access any external service or other path.".into(),status:Status::Ready,priority:1,dependencies:vec![],reference_pages:vec![],acceptance_criteria:vec!["probe.txt contains sandbox-ok and newline".into()],decision:Decision::Implement,reason:"Explicit deployment probe".into()};
    let scan = Scan {
        project_id: "probe".into(),
        workspace_id: uuid::Uuid::new_v4().to_string(),
        board_id: uuid::Uuid::new_v4().to_string(),
        view_id: uuid::Uuid::new_v4().to_string(),
        matches: 1,
        complete: true,
        fields_readable: true,
        stories: vec![story],
    };
    let store = Store::open(&config.orchestrator.state_db)?;
    store.sync_config(&config)?;
    store.ingest(&project, &scan, &config.orchestrator.worktree_root)?;
    let mut run = store.runs()?.remove(0);
    run.worktree = repo.clone();
    let codex = Codex::new(config);
    let result = codex
        .worker(
            &project,
            &run,
            false,
            "",
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
    if let Err(e) = result {
        eprintln!("Probe artifacts retained at {}", t.keep().display());
        return Err(e);
    }
    if std::fs::read_to_string(repo.join("probe.txt"))
        .ok()
        .as_deref()
        != Some("sandbox-ok\n")
    {
        let path = t.keep();
        anyhow::bail!(
            "deterministic verification failed; artifacts retained at {}",
            path.display()
        );
    }
    println!("Live isolated Astra invocation and deterministic file verification passed.");
    Ok(())
}
