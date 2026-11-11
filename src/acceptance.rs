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
const SOURCE_FIXTURE: &str = r#"
def emit_sources(scan):
 def emit(tool,args,value):
  print(json.dumps({'type':'item.completed','item':{'type':'mcp_tool_call','status':'completed','tool':tool,'arguments':args,'result':{'content':[{'type':'text','text':json.dumps(value)}]}}}))
 for story in scan['stories']:
  args={'workspace_id':scan['workspace_id'],'database_id':scan['board_id'],'with_doc':True}
  emit('get_database_row_details',args,{'id':story['story_id'],'cells':{'Title':story['title'],'Status':{'backlog':'Backlog','ready':'Ready','in_progress':'In Progress','review':'Review','done':'Done','blocked':'Blocked','cancelled':'Cancelled'}[story['status']],'Acceptance criteria':'\n'.join(story['acceptance_criteria'])},'has_doc':True,'doc':story['description']})
  emit('list_comments',{'workspace_id':scan['workspace_id'],'target_type':'row','target_id':story['story_id']},{'comments':[],'has_more':False})
"#;
fn fake_codex(path: &Path, scan: &crate::model::Scan, fail: &Path) {
    let script = format!(
        r#"#!/usr/bin/python3
import json,sys,uuid,re,pathlib
{source_fixture}
args=sys.argv
schema=json.loads(pathlib.Path(args[args.index('--output-schema')+1]).read_text())['properties']
out=pathlib.Path(args[args.index('-o')+1]); prompt=sys.stdin.read(); evidence={{}}
if 'stories' in schema:
 result=json.loads({scan:?}); evidence=result; emit_sources(result)
elif 'applied' in schema:
 if pathlib.Path({fail:?}).exists(): sys.exit(9)
 result={{'story_id':json.loads({scan:?})['stories'][0]['story_id'],'applied':True}}; evidence={{'updated':True}}
 for scope in out.parent.glob('*.scope.json'):
  data=json.loads(scope.read_text());pathlib.Path(data['receipt']).write_text(json.dumps({{'tool':'update_row_cells','status_updated':True,'comment_present':True}})+'\n')
elif 'approved' in schema:
 result={{'head_sha':re.search(r'exactly ([0-9a-f]+)',prompt).group(1),'approved':True,'blocking':[],'nonblocking':[]}}
else:
 repair=prompt.split('REPAIR FEEDBACK=',1)[-1]
 if 'Rust prepared a merge' in repair:
  assert '<<<<<<<' in pathlib.Path('/work/feature.txt').read_text()
  pathlib.Path('/work/feature.txt').write_text('implemented\nupstream\n')
 else: pathlib.Path('/work/feature.txt').write_text('implemented\n')
 if 'fixture compiler: missing generated file' in repair: pathlib.Path('/work/repaired.txt').write_text('fixed from actual stderr\n')
 result={{'summary':'Implemented assigned fixture','uncertainties':[]}}
out.write_text(json.dumps(result))
print(json.dumps({{'type':'thread.started','thread_id':str(uuid.uuid4())}}))
if evidence: print(json.dumps({{'type':'item.completed','item':{{'type':'mcp_tool_call','status':'completed','result':evidence}}}}))
print(json.dumps({{'type':'turn.completed'}}))
"#,
        source_fixture = SOURCE_FIXTURE,
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
    config.projects[0].test_commands = vec![vec!["sh".into(), "-c".into(), "test -f repaired.txt || { printf 'fixture compiler: missing generated file\\n' >&2; exit 1; }".into()]];
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
        waiting.attempt, 3,
        "local stderr and CI failure must each trigger a successful bounded repair"
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
    assert_eq!(store.get(&run.id).unwrap().agent_sessions.len(), 5);
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
{source_fixture}
args=sys.argv; prompt=sys.stdin.read();project=json.loads(prompt.split('PROJECT CONFIG (trusted routing only): ')[1])
scans=json.loads({scans:?}); result=scans[project['id']]; emit_sources(result)
pathlib.Path(args[args.index('-o')+1]).write_text(json.dumps(result))
print(json.dumps({{'type':'item.completed','item':{{'type':'mcp_tool_call','status':'completed','result':result}}}}))
print(json.dumps({{'type':'turn.completed'}}))
"#,
        source_fixture = SOURCE_FIXTURE,
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
        github
            .validate(&p.github_repo, config.github.auto_merge)
            .await
            .unwrap();
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

#[tokio::test]
#[ignore = "requires host namespaces; real conflicting Git branches, fixture model and GitHub"]
async fn manual_merge_mode_repairs_conflicts_and_rechecks_the_same_pr() {
    base_update_pipeline(true, 0).await;
}

#[tokio::test]
#[ignore = "requires host namespaces; clean base updates at the repair limit and crash recovery"]
async fn clean_base_updates_do_not_consume_repairs() {
    for crash in 0..=2 {
        base_update_pipeline(false, crash).await;
    }
}

async fn base_update_pipeline(conflict: bool, crash: u8) {
    let t = tempfile::tempdir().unwrap();
    let mut config = fixtures::config(t.path());
    config.projects.truncate(1);
    config.github.auto_merge = false;
    config.projects[0].test_commands = vec![vec!["python3".into(), "-c".into(),
        r#"from pathlib import Path; assert Path('feature.txt').read_text() == 'implemented\nupstream\n'"#.into()]];
    if !conflict {
        config.projects[0].test_commands = vec![vec!["python3".into(), "-c".into(),
            r#"from pathlib import Path; assert Path('feature.txt').read_text() == 'implemented\n'; assert Path('upstream.txt').read_text() == 'upstream\n'"#.into()]];
    }
    let p = config.projects[0].clone();
    let commit = |repo: &Path, message: &str| {
        git(repo, &["add", "--all"]);
        git(
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
        );
    };
    git(&p.repo, &["init", "-b", "main"]);
    std::fs::write(p.repo.join("feature.txt"), "original\n").unwrap();
    commit(&p.repo, "base");
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
    let scan = fixtures::scan(&p, vec![fixtures::story(Status::Review)]);
    config.agents.codex = t.path().join("fake-codex");
    fake_codex(&config.agents.codex, &scan, &t.path().join("no-outage"));
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
    let mut ready = scan.clone();
    ready.stories[0].status = Status::Ready;
    store
        .ingest(&p, &ready, &config.orchestrator.worktree_root)
        .unwrap();
    let mut run = store.runs().unwrap().remove(0);
    store.claim(&mut run, &p, 1).unwrap();
    crate::git::prepare(&p, &mut run, &config.orchestrator.worktree_root)
        .await
        .unwrap();
    std::fs::write(run.worktree.join("feature.txt"), "implemented\n").unwrap();
    crate::git::commit(&mut run).await.unwrap();
    let old_head = run.head_sha.clone().unwrap();
    if !conflict {
        run.attempt = config.orchestrator.max_attempts;
    }
    let expected_attempt = run.attempt + u32::from(conflict);
    let expected_sessions = if conflict { 4 } else { 3 };
    git(&run.worktree, &["push", "origin", &run.branch]);
    run.pr = Some(1);
    run.pr_url = Some("https://github.com/owner/a/pull/1".into());
    run.test_sha = Some(old_head.clone());
    run.review = Some(crate::model::Review {
        head_sha: old_head.clone(),
        approved: true,
        blocking: vec![],
        nonblocking: vec![],
    });
    run.agent_sessions = vec!["previous-implementer".into(), "previous-reviewer".into()];
    store
        .transition(&mut run, State::Reviewing, "previous review")
        .unwrap();
    store
        .transition(&mut run, State::WaitingCi, "existing PR")
        .unwrap();
    std::fs::write(
        p.repo.join(if conflict {
            "feature.txt"
        } else {
            "upstream.txt"
        }),
        "upstream\n",
    )
    .unwrap();
    commit(&p.repo, "advance base");
    git(&p.repo, &["push", "origin", "main"]);
    let new_base = git(&p.repo, &["rev-parse", "HEAD"]);
    if crash != 0 {
        assert_eq!(
            crate::git::update_base(&p, &mut run).await.unwrap(),
            crate::git::BaseUpdate::Updated { conflicts: vec![] }
        );
        store
            .transition(
                &mut run,
                State::Reviewing,
                "clean update before interruption",
            )
            .unwrap();
        if crash == 2 {
            // Git commit completed, but its head was not yet saved in SQLite.
            crate::git::commit(&mut run).await.unwrap();
        }
        run = store.get(&run.id).unwrap();
    }
    let remote_api = remote.clone();
    let branch = run.branch.clone();
    let id = run.id.clone();
    let old_api_head = old_head.clone();
    let checked = Arc::new(Mutex::new(Vec::new()));
    let checked_api = checked.clone();
    let db = store.clone();
    let server = Server::start(move |line, _| {
        let head = git(&remote_api, &["rev-parse", &branch]);
        let base = git(&remote_api, &["rev-parse", "main"]);
        if line.starts_with("GET /repos/owner/a/pulls/1 ") {
            return (
                200,
                json!({"number":1,"state":"open","merged":false,"draft":false,
                "body":format!("Maistro run: {id}"),
                "head":{"sha":head,"ref":branch,"repo":{"full_name":"owner/a"}},
                "base":{"sha":base,"ref":"main","repo":{"full_name":"owner/a"}}}),
            );
        }
        if line.starts_with("GET /repos/owner/a/pulls?") {
            return (
                200,
                json!([{"number":1,"html_url":"https://github.com/owner/a/pull/1"}]),
            );
        }
        if line.contains("/check-runs?") {
            assert_ne!(
                head, old_api_head,
                "CI must be rechecked on the repaired head"
            );
            assert!(line.contains(&head));
            let persisted = db.get(&id).unwrap();
            assert_eq!(persisted.test_sha.as_ref(), Some(&head));
            assert_eq!(persisted.review.as_ref().unwrap().head_sha, head);
            assert_eq!(
                persisted.agent_sessions.len(),
                expected_sessions,
                "only conflicts require a new worker; every update needs independent review"
            );
            checked_api.lock().unwrap().push(head.clone());
            return (
                200,
                json!({"check_runs":[{"id":1,"name":"test","head_sha":head,"status":"completed","conclusion":"success","app":{"id":1}}]}),
            );
        }
        if line.contains("/statuses?") {
            return (200, json!([]));
        }
        panic!("manual mode must not create a second PR, modify protections or merge: {line}");
    });
    let mut github = Github::fixture(server.url.clone());
    github.push_remote = Some(remote.to_string_lossy().into());
    let engine = Engine {
        config: config.clone(),
        codex: Codex::new(config),
        store: store.clone(),
        github,
    };
    engine
        .execute(run.clone(), &tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let repaired = store.get(&run.id).unwrap();
    assert_eq!(
        repaired.state,
        State::WaitingCi,
        "{:?}",
        repaired.last_error
    );
    assert_eq!(repaired.attempt, expected_attempt);
    assert_eq!(repaired.pr, Some(1));
    assert_eq!(repaired.base_sha.as_ref(), Some(&new_base));
    assert_ne!(repaired.head_sha.as_ref(), Some(&old_head));
    assert_eq!(checked.lock().unwrap().len(), 1);
    assert_eq!(
        git(&remote, &["rev-parse", "main"]),
        new_base,
        "base branch must not be merged"
    );
    assert_eq!(
        git(&remote, &["rev-parse", &run.branch]),
        repaired.head_sha.clone().unwrap()
    );
    // Reconciliation is idempotent; no further repair or extra agent sessions.
    engine
        .execute(
            repaired.clone(),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(store.get(&run.id).unwrap().attempt, expected_attempt);
    assert_eq!(store.get(&run.id).unwrap().head_sha, repaired.head_sha);
}
