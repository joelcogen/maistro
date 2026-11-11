//! Deterministic merge policy. Missing evidence is a rejection, never an approval.
use crate::{config::Project, model::Story, store::Run};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::collections::HashMap;
pub fn changed_path_allowed(p: &Project, path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.starts_with('/')
            && !path.contains('\\')
            && !path.split('/').any(|c| c == ".." || c.is_empty()),
        "unsafe changed path"
    );
    let path = path.to_ascii_lowercase();
    let protected = [
        ".github/",
        ".gitlab",
        ".circleci/",
        ".buildkite/",
        ".codex/",
        ".agents/",
        ".gitmodules",
        ".gitattributes",
        ".env",
        "docker-compose",
        "compose.",
        "terraform/",
        "infra/",
        "deploy/",
        "deployment/",
        "k8s/",
        "kubernetes/",
        "migrations/",
        "migration/",
        "policy/",
        "policies/",
        "security/",
        "auth/",
        "authorization/",
        "credentials/",
        "secrets/",
    ];
    let basename = path.rsplit('/').next().unwrap_or("");
    ensure!(
        !matches!(
            basename,
            "agents.md"
                | "codeowners"
                | "security.md"
                | "jenkinsfile"
                | "dockerfile"
                | "procfile"
                | "fly.toml"
                | "render.yaml"
                | "railway.json"
                | "serverless.yml"
                | "permissions.rs"
                | "permissions.ts"
                | "rbac.rs"
                | "jwt.rs"
        ) && !protected.iter().any(|x| path.starts_with(x))
            && !path.split('/').any(|s| matches!(
                s,
                "auth"
                    | "authorization"
                    | "authentication"
                    | "migrations"
                    | "secrets"
                    | "credentials"
                    | "policy"
                    | "policies"
            ))
            && ![".pem", ".key", ".p12", ".pfx"]
                .iter()
                .any(|s| path.ends_with(s)),
        "protected file requires human intervention: {path}"
    );
    ensure!(
        !p.protected_paths
            .iter()
            .any(|s| path.starts_with(&s.to_ascii_lowercase())),
        "configured protected file: {path}"
    );
    ensure!(
        p.allowed_paths.is_empty()
            || p.allowed_paths
                .iter()
                .any(|s| path.starts_with(&s.to_ascii_lowercase())),
        "file outside allowed prefixes: {path}"
    );
    Ok(())
}
pub fn authorized(r: &Run, story: Option<&Story>) -> Result<()> {
    let s = story.ok_or_else(|| anyhow::anyhow!("story disappeared"))?;
    ensure!(s.authorized(), "story authorization revoked");
    ensure!(
        s.revision() == r.revision,
        "requirements changed since claim/review"
    );
    Ok(())
}
pub fn review_valid(r: &Run) -> Result<()> {
    let sha = r
        .head_sha
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("head missing"))?;
    let review = r
        .review
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("independent review missing"))?;
    ensure!(
        review.head_sha == sha && review.approved && review.blocking.is_empty(),
        "review is stale or has blocking findings"
    );
    ensure!(
        r.test_sha.as_deref() == Some(sha),
        "deterministic tests missing for exact head"
    );
    ensure!(
        r.agent_sessions.len() >= 2
            && r.agent_sessions[r.agent_sessions.len() - 1]
                != r.agent_sessions[r.agent_sessions.len() - 2],
        "review must be a fresh independent session"
    );
    Ok(())
}
pub fn pr_identity(p: &Project, r: &Run, pr: &Value) -> Result<()> {
    ensure!(pr["number"].as_u64() == r.pr, "PR number mismatch");
    ensure!(
        pr["base"]["repo"]["full_name"].as_str() == Some(&p.github_repo)
            && pr["head"]["repo"]["full_name"].as_str() == Some(&p.github_repo),
        "PR repository identity mismatch"
    );
    ensure!(
        pr["base"]["ref"].as_str() == Some(&p.base_branch)
            && pr["head"]["ref"].as_str() == Some(&r.branch),
        "PR branch identity mismatch"
    );
    ensure!(
        pr["head"]["sha"].as_str() == r.head_sha.as_deref(),
        "PR head changed"
    );
    ensure!(
        pr["body"]
            .as_str()
            .is_some_and(|s| s.contains(&format!("Maistro run: {}", r.id))),
        "PR is not linked to recorded run"
    );
    Ok(())
}
pub fn mergeable(pr: &Value) -> Result<()> {
    ensure!(
        pr["state"] == "open"
            && pr["draft"] == false
            && pr["mergeable"] == true
            && pr["mergeable_state"] == "clean",
        "GitHub does not report an open, ready, clean mergeable PR"
    );
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ci {
    Pending,
    Failed(Vec<String>),
    Passed,
}
/// Latest check-run per (name, app), latest commit-status per context. No skipped/neutral acceptance.
pub fn ci(
    head: &str,
    required: &[(String, Option<i64>)],
    checks: &[Value],
    statuses: &[Value],
) -> Result<Ci> {
    ensure!(
        !required.is_empty(),
        "no required checks configured or enforced"
    );
    let mut latest: HashMap<(String, Option<i64>), &Value> = HashMap::new();
    for c in checks {
        ensure!(
            c["head_sha"].as_str() == Some(head),
            "check belongs to different SHA"
        );
        let key = (
            c["name"].as_str().unwrap_or("").to_string(),
            c["app"]["id"].as_i64(),
        );
        if latest
            .get(&key)
            .is_none_or(|old| c["id"].as_u64() > old["id"].as_u64())
        {
            latest.insert(key, c);
        }
    }
    let mut pending = false;
    let mut failures = vec![];
    for (name, app) in required {
        let matches: Vec<_> = latest
            .iter()
            .filter(|((n, a), _)| n == name && (app.is_none() || app == a))
            .map(|(_, v)| *v)
            .collect();
        if matches.is_empty() {
            if let Some(s) = statuses
                .iter()
                .find(|s| s["context"].as_str() == Some(name) && app.is_none())
            {
                match s["state"].as_str() {
                    Some("success") => {}
                    Some("failure" | "error") => failures.push(name.clone()),
                    _ => pending = true,
                }
            } else {
                pending = true;
            }
        } else {
            for check in matches {
                if check["status"] != "completed" {
                    pending = true;
                } else if check["conclusion"] != "success" {
                    failures.push(name.clone());
                }
            }
        }
    }
    if !failures.is_empty() {
        Ok(Ci::Failed(failures))
    } else if pending {
        Ok(Ci::Pending)
    } else {
        Ok(Ci::Passed)
    }
}
