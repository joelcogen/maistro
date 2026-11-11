mod common;
use maistro::{git, model::Status, store::Store};
use std::path::Path;

async fn commit(repo: &Path, message: &str) {
    git::git(repo, &["add", "--all"]).await.unwrap();
    git::git(
        repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-m",
            message,
        ],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn conflicts_survive_interruption_and_commit_without_force_push() {
    let t = tempfile::tempdir().unwrap();
    let config = common::config(t.path());
    let p = &config.projects[0];
    git::git(&p.repo, &["init", "-b", "main"]).await.unwrap();
    std::fs::write(p.repo.join("guide.md"), "original\n").unwrap();
    std::fs::write(p.repo.join("obsolete.md"), "old document\n").unwrap();
    std::fs::create_dir_all(p.repo.join(".github/workflows")).unwrap();
    std::fs::write(p.repo.join(".github/workflows/ci.yml"), "original CI\n").unwrap();
    commit(&p.repo, "base").await;
    let original = git::git(&p.repo, &["rev-parse", "HEAD"]).await.unwrap();
    let remote = t.path().join("remote.git");
    git::git(
        t.path(),
        &[
            "clone",
            "--bare",
            p.repo.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    )
    .await
    .unwrap();
    git::git(
        &p.repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    )
    .await
    .unwrap();
    let store = Store::open(&config.orchestrator.state_db).unwrap();
    store.sync_config(&config).unwrap();
    store
        .ingest(
            p,
            &common::scan(p, vec![common::story(Status::Ready)]),
            &config.orchestrator.worktree_root,
        )
        .unwrap();
    let mut run = store.runs().unwrap().remove(0);
    git::prepare(p, &mut run, &config.orchestrator.worktree_root)
        .await
        .unwrap();
    std::fs::write(run.worktree.join("guide.md"), "story wording\n").unwrap();
    std::fs::remove_file(run.worktree.join("obsolete.md")).unwrap();
    git::commit(&mut run).await.unwrap();
    let story_head = run.head_sha.clone().unwrap();
    git::git(&run.worktree, &["push", "origin", &run.branch])
        .await
        .unwrap();

    // The base advances independently, including an unrelated protected file.
    std::fs::write(p.repo.join("guide.md"), "new upstream information\n").unwrap();
    std::fs::write(p.repo.join("obsolete.md"), "updated upstream document\n").unwrap();
    std::fs::write(p.repo.join(".github/workflows/ci.yml"), "new upstream CI\n").unwrap();
    commit(&p.repo, "upstream").await;
    let new_base = git::git(&p.repo, &["rev-parse", "HEAD"]).await.unwrap();
    git::git(&p.repo, &["push", "origin", "main"])
        .await
        .unwrap();
    let before_crash = run.clone();
    assert_eq!(
        git::update_base(p, &mut run).await.unwrap(),
        git::BaseUpdate::Updated {
            conflicts: vec!["guide.md".into(), "obsolete.md".into()]
        }
    );
    assert_eq!(run.base_sha.as_ref(), Some(&new_base));
    assert!(
        git::commit(&mut run)
            .await
            .unwrap_err()
            .to_string()
            .contains("Unresolved merge conflict")
    );
    assert_eq!(
        git::git(&run.worktree, &["rev-parse", "HEAD"])
            .await
            .unwrap(),
        story_head
    );

    // Recover from Git's durable merge metadata with the pre-merge SQLite data.
    run = before_crash;
    assert_eq!(run.base_sha.as_ref(), Some(&original));
    assert_eq!(
        git::update_base(p, &mut run).await.unwrap(),
        git::BaseUpdate::Updated {
            conflicts: vec!["guide.md".into(), "obsolete.md".into()]
        }
    );
    assert_eq!(run.base_sha.as_ref(), Some(&new_base));
    git::verify_conflict_paths(p, &run, &["guide.md".into()]).unwrap();
    assert!(git::verify_conflict_paths(p, &run, &[".github/workflows/ci.yml".into()]).is_err());
    std::fs::write(
        run.worktree.join("guide.md"),
        "story wording with new upstream information\n",
    )
    .unwrap();
    std::fs::remove_file(run.worktree.join("obsolete.md")).unwrap();
    git::verify_uncommitted_paths(p, &run).await.unwrap();
    std::fs::write(
        run.worktree.join(".github/workflows/ci.yml"),
        "unauthorized worker change\n",
    )
    .unwrap();
    assert!(git::verify_uncommitted_paths(p, &run).await.is_err());
    std::fs::write(
        run.worktree.join(".github/workflows/ci.yml"),
        "new upstream CI\n",
    )
    .unwrap();
    git::commit(&mut run).await.unwrap();
    git::verify_paths(p, &run).await.unwrap();
    let parents = git::git(&run.worktree, &["show", "-s", "--format=%P", "HEAD"])
        .await
        .unwrap();
    assert_eq!(
        parents.split_whitespace().collect::<Vec<_>>(),
        vec![story_head.as_str(), new_base.as_str()]
    );
    git::git(&run.worktree, &["push", "origin", &run.branch])
        .await
        .unwrap();
    assert_eq!(
        git::update_base(p, &mut run).await.unwrap(),
        git::BaseUpdate::Unchanged
    );
    assert_eq!(
        std::fs::read_to_string(p.repo.join("guide.md")).unwrap(),
        "new upstream information\n"
    );

    // A subsequent clean merge also requires a Rust-owned merge commit.
    std::fs::write(p.repo.join("extra.txt"), "upstream addition\n").unwrap();
    commit(&p.repo, "another upstream commit").await;
    git::git(&p.repo, &["push", "origin", "main"])
        .await
        .unwrap();
    assert_eq!(
        git::update_base(p, &mut run).await.unwrap(),
        git::BaseUpdate::Updated { conflicts: vec![] }
    );
    git::verify_uncommitted_paths(p, &run).await.unwrap();
    git::commit(&mut run).await.unwrap();
    git::verify_paths(p, &run).await.unwrap();
    git::git(&run.worktree, &["push", "origin", &run.branch])
        .await
        .unwrap();
}
