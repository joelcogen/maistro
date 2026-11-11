//! Disposable end-to-end fixtures: real Git/SQLite/process isolation, fake model and GitHub.
use crate::{
    codex::Codex,
    engine::Engine,
    fixtures,
    github::Github,
    model::Status,
    store::{State, Store},
    test_support::Server,
};
use serde_json::json;
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};
fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().trim().into()
}
fn fake_codex(path: &Path, scan: &crate::model::Scan, fail: &Path) {
    let script = format!(
        r#"#!/usr/bin/python3
import json,sys,uuid,re,pathlib
args=sys.argv
schema=json.loads(pathlib.Path(args[args.index('--output-schema')+1]).read_text())['properties']
out=pathlib.Path(args[args.index('-o')+1]); prompt=sys.stdin.read(); evidence={{}}
if 'stories' in schema:
 result=json.loads({scan:?}); evidence=result
elif 'applied' in schema:
 if pathlib.Path({fail:?}).exists(): sys.exit(9)
 result={{'story_id':json.loads({scan:?})['stories'][0]['story_id'],'applied':True}}; evidence={{'updated':True}}
 for scope in out.parent.glob('*.scope.json'):
  data=json.loads(scope.read_text());pathlib.Path(data['receipt']).write_text(json.dumps({{'tool':'update_row_cells','status_updated':True,'comment_present':True}})+'\n')
elif 'approved' in schema:
 result={{'head_sha':re.search(r'exactly ([0-9a-f]+)',prompt).group(1),'approved':True,'blocking':[],'nonblocking':[]}}
else:
 pathlib.Path('/work/feature.txt').write_text('implemented\n')
 result={{'summary':'Implemented assigned fixture','uncertainties':[]}}
out.write_text(json.dumps(result))
print(json.dumps({{'type':'thread.started','thread_id':str(uuid.uuid4())}}))
if evidence: print(json.dumps({{'type':'item.completed','item':{{'type':'mcp_tool_call','status':'completed','result':evidence}}}}))
print(json.dumps({{'type':'turn.completed'}}))
"#,
        scan = serde_json::to_string(scan).unwrap(),
        fail = fail.to_str().unwrap()
    );
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
#[tokio::test]
#[ignore = "requires host namespaces; fake external APIs, real disposable Git repositories"]
async fn green_merge_once_recovery_and_appflowy_outage_end_to_end() {
    let t = tempfile::tempdir().unwrap();
    let mut config = fixtures::config(t.path());
    config.projects.truncate(1);
    config.github.auto_merge = true;
    let p = config.projects[0].clone();
    git(&p.repo, &["init", "-b", "main"]);
    std::fs::write(p.repo.join("README.md"), "fixture\n").unwrap();
    git(&p.repo, &["add", "."]);
    git(
        &p.repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-m",
            "base",
        ],
    );
    let remote = t.path().join("remote.git");
    git(
        t.path(),
        &[
            "clone",
            "--bare",
            p.repo.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    git(
        &p.repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    let scan = fixtures::scan(&p, vec![fixtures::story(Status::Ready)]);
    let codex_path = t.path().join("fake-codex");
    let fail = t.path().join("fail-projection");
    fake_codex(&codex_path, &scan, &fail);
    config.agents.codex = codex_path;
    config.agents.codex_home = t.path().join("codex-home");
    std::fs::create_dir(&config.agents.codex_home).unwrap();
    std::fs::write(config.agents.codex_home.join("auth.json"), "{}").unwrap();
    std::fs::write(
        config.agents.codex_home.join("config.toml"),
        "[mcp_servers.appflowy]\ncommand='/bin/true'\n",
    )
    .unwrap();
    let store = Store::open(&config.orchestrator.state_db).unwrap();
    store.sync_config(&config).unwrap();
    store
        .ingest(&p, &scan, &config.orchestrator.worktree_root)
        .unwrap();
    let mut run = store.runs().unwrap().remove(0);
    store.claim(&mut run, &p, 1).unwrap();
    let branch = run.branch.clone();
    let id = run.id.clone();
    let remote2 = remote.clone();
    let mutations = Arc::new(Mutex::new((0, 0)));
    let counts = mutations.clone();
    let mut exists = false;
    let mut merged = false;
    let mut merge_sha = String::new();
    let mut draft = true;
    let mut ci_reads = 0;
    let server = Server::start(move |line, body| {
        let sha = || git(&remote2, &["rev-parse", &branch]);
        let base = || git(&remote2, &["rev-parse", "main"]);
        if line.starts_with("POST /repos/owner/a/pulls ") {
            exists = true;
            counts.lock().unwrap().0 += 1;
            return (
                200,
                json!({"number":1,"html_url":"https://github.com/owner/a/pull/1"}),
            );
        }
        if line.starts_with("GET /repos/owner/a/pulls?") {
            return (
                200,
                if exists {
                    json!([{"number":1,"html_url":"https://github.com/owner/a/pull/1"}])
                } else {
                    json!([])
                },
            );
        }
        if line.starts_with("POST /graphql") {
            draft = false;
            return (
                200,
                json!({"data":{"markPullRequestReadyForReview":{"pullRequest":{"isDraft":false}}}}),
            );
        }
        if line.starts_with("GET /repos/owner/a/pulls/1 ") {
            return (
                200,
                json!({"number":1,"node_id":"node1","body":format!("Maistro run: {id}"),"head":{"sha":sha(),"ref":branch,"repo":{"full_name":"owner/a"}},"base":{"sha":base(),"ref":"main","repo":{"full_name":"owner/a"}},"state":if merged{"closed"}else{"open"},"draft":draft,"mergeable":true,"mergeable_state":"clean","merged":merged,"merge_commit_sha":merge_sha}),
            );
        }
        if line.starts_with("GET /repos/owner/a ") {
            return (
                200,
                json!({"permissions":{"push":true,"admin":false},"allow_squash_merge":true}),
            );
        }
        if line.contains("/protection ") {
            return (
                200,
                json!({"required_status_checks":{"strict":true,"checks":[{"context":"test","app_id":1}]},"enforce_admins":{"enabled":true},"allow_force_pushes":{"enabled":false},"allow_deletions":{"enabled":false}}),
            );
        }
        if line.contains("/rulesets?") {
            return (200, json!([]));
        }
        if line.contains("/check-runs?") {
            ci_reads += 1;
            return (
                200,
                json!({"check_runs":[{"id":1,"name":"test","head_sha":sha(),"app":{"id":1},"status":"completed","conclusion":if ci_reads==1{"failure"}else{"success"}}]}),
            );
        }
        if line.contains("/statuses?") {
            return (200, json!([]));
        }
        if line.starts_with("PUT ") && line.contains("/merge ") {
            assert_eq!(body["sha"], sha());
            assert_eq!(body["merge_method"], "squash");
            counts.lock().unwrap().1 += 1;
            let tree = git(&remote2, &["rev-parse", &format!("{}^{{tree}}", sha())]);
            merge_sha = git(
                &remote2,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit-tree",
                    &tree,
                    "-p",
                    &base(),
                    "-m",
                    "squash fixture",
                ],
            );
            git(&remote2, &["update-ref", "refs/heads/main", &merge_sha]);
            merged = true;
            return (502, json!({"message":"simulated lost merge response"}));
        }
        panic!("unexpected request {line}")
    });
    let mut github = Github::fixture(server.url.clone());
    github.push_remote = Some(remote.to_str().unwrap().into());
    let engine = Engine {
        codex: Codex::new(config.clone()),
        github,
        store: store.clone(),
        config,
    };
    let cancel = tokio_util::sync::CancellationToken::new();
    store.set_control("paused", "true").unwrap();
    engine.execute(run.clone(), &cancel).await.unwrap();
    let waiting = store.get(&run.id).unwrap();
    assert_eq!(waiting.state, State::WaitingCi);
    assert_eq!(*mutations.lock().unwrap(), (1, 0));
    assert_eq!(
        waiting.attempt, 2,
        "CI failure must trigger a bounded repair"
    );
    store.set_control("paused", "false").unwrap();
    let mut changed = scan.clone();
    changed.stories[0]
        .description
        .push_str(" changed requirements");
    fake_codex(&engine.config.agents.codex, &changed, &fail);
    assert!(engine.execute(waiting.clone(), &cancel).await.is_err());
    assert_eq!(*mutations.lock().unwrap(), (1, 0));
    changed = scan.clone();
    changed.stories[0].status = Status::Cancelled;
    fake_codex(&engine.config.agents.codex, &changed, &fail);
    assert!(engine.execute(waiting.clone(), &cancel).await.is_err());
    assert_eq!(*mutations.lock().unwrap(), (1, 0));
    fake_codex(&engine.config.agents.codex, &scan, &fail);
    let run = waiting;
    assert!(engine.execute(run.clone(), &cancel).await.is_err());
    let run = store.get(&run.id).unwrap();
    assert_eq!(run.state, State::Merging);
    engine.execute(run.clone(), &cancel).await.unwrap();
    assert_eq!(store.get(&run.id).unwrap().state, State::Completed);
    assert_eq!(*mutations.lock().unwrap(), (1, 1));
    std::fs::write(&fail, "outage").unwrap();
    engine.flush(&cancel).await.unwrap();
    assert_eq!(store.get(&run.id).unwrap().state, State::Completed);
    std::fs::remove_file(fail).unwrap();
    // Reopen and make the retry due without a wall-clock sleep.
    store.make_outbox_due().unwrap();
    engine.flush(&cancel).await.unwrap();
    assert!(store.pending().unwrap().is_empty());
    assert_eq!(store.runs().unwrap().len(), 1);
    assert_eq!(store.get(&run.id).unwrap().agent_sessions.len(), 4);
    assert_eq!(*mutations.lock().unwrap(), (1, 1));
    assert_eq!(
        std::fs::read_to_string(p.repo.join("README.md")).unwrap(),
        "fixture\n"
    );
    assert!(!p.repo.join("feature.txt").exists());
}

#[tokio::test]
async fn two_projects_validate_and_poll_independent_boards() {
    let t = tempfile::tempdir().unwrap();
    let mut config = fixtures::config(t.path());
    for p in &config.projects {
        git(&p.repo, &["init", "-b", "main"]);
        std::fs::write(p.repo.join("README.md"), "fixture").unwrap();
        git(&p.repo, &["add", "."]);
        git(
            &p.repo,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                "base",
            ],
        );
        git(
            &p.repo,
            &[
                "remote",
                "add",
                "origin",
                &format!("https://github.com/{}.git", p.github_repo),
            ],
        );
        crate::git::validate(p).await.unwrap();
    }
    let scans: std::collections::HashMap<_, _> = config
        .projects
        .iter()
        .map(|p| {
            (
                p.id.clone(),
                fixtures::scan(p, vec![fixtures::story(Status::Ready)]),
            )
        })
        .collect();
    let executable = t.path().join("codex");
    let script = format!(
        r#"#!/usr/bin/python3
import json,sys,pathlib
args=sys.argv; prompt=sys.stdin.read();project=json.loads(prompt.split('PROJECT CONFIG (trusted routing only): ')[1])
scans=json.loads({scans:?}); result=scans[project['id']]
pathlib.Path(args[args.index('-o')+1]).write_text(json.dumps(result))
print(json.dumps({{'type':'item.completed','item':{{'type':'mcp_tool_call','status':'completed','result':result}}}}))
print(json.dumps({{'type':'turn.completed'}}))
"#,
        scans = serde_json::to_string(&scans).unwrap()
    );
    std::fs::write(&executable, script).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    config.agents.codex = executable;
    config.agents.codex_home = t.path().join("codex-home");
    std::fs::create_dir(&config.agents.codex_home).unwrap();
    std::fs::write(
        config.agents.codex_home.join("config.toml"),
        "[mcp_servers.appflowy]\ncommand='/bin/true'\n",
    )
    .unwrap();
    let server = Server::start(|line, _| {
        assert!(line.starts_with("GET /repos/owner/a ") || line.starts_with("GET /repos/owner/b "));
        (
            200,
            json!({"permissions":{"push":true,"admin":false},"allow_squash_merge":true}),
        )
    });
    let github = Github::fixture(server.url.clone());
    for p in &config.projects {
        github.validate(&p.github_repo).await.unwrap();
    }
    let store = Store::open(&config.orchestrator.state_db).unwrap();
    store.sync_config(&config).unwrap();
    let engine = Engine {
        codex: Codex::new(config.clone()),
        github,
        store: store.clone(),
        config,
    };
    engine
        .poll(&tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    engine
        .poll(&tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(store.runs().unwrap().len(), 2);
    for id in ["a", "b"] {
        assert_eq!(
            store.scan(id).unwrap().unwrap().board_id,
            scans[id].board_id
        );
    }
    assert_ne!(scans["a"].board_id, scans["b"].board_id);
}

#[tokio::test]
#[ignore = "requires host namespaces; fake models reject each independent review"]
async fn reviewer_rejection_exhausts_pipeline_budget() {
    let t = tempfile::tempdir().unwrap();
    let mut config = fixtures::config(t.path());
    config.projects.truncate(1);
    let p = config.projects[0].clone();
    git(&p.repo, &["init", "-b", "main"]);
    std::fs::write(p.repo.join("README.md"), "fixture").unwrap();
    git(&p.repo, &["add", "."]);
    git(
        &p.repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-m",
            "base",
        ],
    );
    let remote = t.path().join("remote.git");
    git(
        t.path(),
        &[
            "clone",
            "--bare",
            p.repo.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    git(
        &p.repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    let scan = fixtures::scan(&p, vec![fixtures::story(Status::Ready)]);
    let executable = t.path().join("fake-codex");
    fake_codex(&executable, &scan, &t.path().join("no-failure"));
    let script = std::fs::read_to_string(&executable)
        .unwrap()
        .replace("'approved':True", "'approved':False");
    std::fs::write(&executable, script).unwrap();
    config.agents.codex = executable;
    config.agents.codex_home = t.path().join("codex-home");
    std::fs::create_dir(&config.agents.codex_home).unwrap();
    std::fs::write(config.agents.codex_home.join("auth.json"), "{}").unwrap();
    std::fs::write(
        config.agents.codex_home.join("config.toml"),
        "[mcp_servers.appflowy]\ncommand='/bin/true'\n",
    )
    .unwrap();
    let server = Server::start(|line, _| {
        assert!(line.starts_with("GET /repos/owner/a/pulls?"));
        (200, json!([]))
    });
    let store = Store::open(&config.orchestrator.state_db).unwrap();
    store.sync_config(&config).unwrap();
    store
        .ingest(&p, &scan, &config.orchestrator.worktree_root)
        .unwrap();
    let mut run = store.runs().unwrap().remove(0);
    store.claim(&mut run, &p, 1).unwrap();
    let engine = Engine {
        codex: Codex::new(config.clone()),
        github: Github::fixture(server.url.clone()),
        store: store.clone(),
        config,
    };
    engine
        .execute(run.clone(), &tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let run = store.get(&run.id).unwrap();
    assert_eq!(run.state, State::Blocked);
    assert_eq!(run.attempt, 3);
    assert_eq!(run.agent_sessions.len(), 6);
    assert!(run.pr.is_none());
}
