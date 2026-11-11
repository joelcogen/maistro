use anyhow::{Context, Result, ensure};
use reqwest::{Client, Method};
use serde_json::Value;
#[derive(Clone)]
pub struct Github {
    client: Client,
    token: String,
    api_base: String,
    #[cfg(test)]
    pub(crate) push_remote: Option<String>,
}
impl Github {
    pub fn new(token_env: &str) -> Result<Self> {
        let token = std::env::var(token_env)
            .with_context(|| format!("set {token_env} in a protected environment file"))?;
        ensure!(!token.trim().is_empty(), "empty GitHub credential");
        Ok(Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            token,
            api_base: "https://api.github.com".into(),
            #[cfg(test)]
            push_remote: None,
        })
    }
    pub async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut r = self
            .client
            .request(method, format!("{}{path}", self.api_base))
            .bearer_auth(&self.token)
            .header("User-Agent", "maistro")
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(b) = body {
            r = r.json(b);
        }
        let res = r.send().await?;
        ensure!(
            res.status().is_success(),
            "GitHub request {} returned {}",
            path,
            res.status()
        );
        Ok(res.json().await?)
    }
    pub async fn validate(&self, repo: &str, auto_merge: bool) -> Result<()> {
        let v = self
            .request(Method::GET, &format!("/repos/{repo}"), None)
            .await?;
        ensure!(
            v["permissions"]["push"] == true,
            "GitHub credential needs repository push access"
        );
        // This is the authenticated account's repository role, not its token scopes.
        // Owners can run the manual-merge pilot; automatic merging keeps the stricter policy.
        ensure!(
            !auto_merge || v["permissions"]["admin"] != true,
            "automatic merging requires a non-administrator GitHub account; keep auto_merge=false to use an owner account"
        );
        ensure!(
            v["allow_squash_merge"] == true,
            "repository must allow squash merges"
        );
        Ok(())
    }
}

impl Github {
    pub async fn push(&self, p: &crate::config::Project, r: &crate::store::Run) -> Result<()> {
        #[cfg(test)]
        if let Some(remote) = &self.push_remote {
            crate::git::git(
                &r.worktree,
                &["push", remote, &format!("HEAD:refs/heads/{}", r.branch)],
            )
            .await?;
            return Ok(());
        }
        use base64::Engine;
        let basic = base64::engine::general_purpose::STANDARD
            .encode(format!("x-access-token:{}", self.token));
        let mut c = tokio::process::Command::new("git");
        c.arg("-C")
            .arg(&r.worktree)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "credential.helper=",
                "push",
                &format!("https://github.com/{}.git", p.github_repo),
                &format!("HEAD:refs/heads/{}", r.branch),
            ])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraHeader")
            .env(
                "GIT_CONFIG_VALUE_0",
                format!("Authorization: Basic {basic}"),
            );
        // Capture diagnostics, but redact credentials before they reach logs,
        // persisted run errors, or board updates.
        let o = crate::process::run(
            c,
            "",
            std::time::Duration::from_secs(120),
            &tokio_util::sync::CancellationToken::new(),
            None,
        )
        .await?;
        if o.code != 0 {
            let details = push_diagnostics(&o.stderr, &o.stdout, &self.token);
            tracing::warn!(run=%r.id, project=%p.id, exit_code=o.code, error=%details, "Git push failed");
            anyhow::bail!("Git push failed (exit {}): {}", o.code, details);
        }
        Ok(())
    }
    pub async fn find_pr(
        &self,
        p: &crate::config::Project,
        r: &crate::store::Run,
    ) -> Result<Option<Value>> {
        let owner = p.github_repo.split('/').next().context("missing owner")?;
        let v = self
            .request(
                Method::GET,
                &format!(
                    "/repos/{}/pulls?state=all&head={}:{}&base={}&per_page=100",
                    p.github_repo, owner, r.branch, p.base_branch
                ),
                None,
            )
            .await?;
        let prs = v.as_array().context("invalid PR list")?;
        ensure!(prs.len() <= 1, "multiple PRs for run branch");
        Ok(prs.first().cloned())
    }
    pub async fn draft_pr(
        &self,
        p: &crate::config::Project,
        r: &mut crate::store::Run,
    ) -> Result<()> {
        let v = if let Some(pr) = self.find_pr(p, r).await? {
            pr
        } else {
            self.request(Method::POST,&format!("/repos/{}/pulls",p.github_repo),Some(&serde_json::json!({"title":r.story.title,"head":r.branch,"base":p.base_branch,"draft":true,"body":format!("Maistro run: {}\nStory: {}\nRevision: {}\n\n{}",r.id,r.story.story_id,r.revision,r.story.description)}))).await?
        };
        r.pr = Some(v["number"].as_u64().context("PR number missing")?);
        r.pr_url = Some(v["html_url"].as_str().context("PR URL missing")?.into());
        Ok(())
    }
    pub async fn pr(&self, p: &crate::config::Project, number: u64) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/repos/{}/pulls/{number}", p.github_repo),
            None,
        )
        .await
    }
}

fn push_diagnostics(stderr: &str, stdout: &str, token: &str) -> String {
    use base64::Engine;
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    let encoded_token = base64::engine::general_purpose::STANDARD.encode(token);
    let percent_encode = |value: &str, lowercase: bool| -> String {
        value
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                    char::from(byte).to_string()
                } else if lowercase {
                    format!("%{byte:02x}")
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect()
    };
    let mut text = format!("{stderr}\n{stdout}");
    // Redact before truncation, including URL and HTTP Basic representations.
    for secret in [
        percent_encode(&basic, false),
        percent_encode(&basic, true),
        basic,
        encoded_token,
        percent_encode(token, false),
        percent_encode(token, true),
        token.to_owned(),
    ] {
        if !secret.is_empty() {
            text = text.replace(&secret, "[REDACTED]");
        }
    }
    let text = text
        .lines()
        .map(|line| {
            if line.to_ascii_lowercase().contains("authorization:") {
                "[REDACTED authorization header]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    if text.is_empty() {
        return "Git returned no diagnostic output".into();
    }
    let mut bounded: String = text.chars().take(4096).collect();
    if text.chars().count() > 4096 {
        bounded.push_str("\n[output truncated]");
    }
    bounded
}

impl Github {
    pub async fn mark_ready(&self, p: &crate::config::Project, number: u64) -> Result<()> {
        let pr = self.pr(p, number).await?;
        if pr["draft"] == false {
            return Ok(());
        }
        let node = pr["node_id"].as_str().context("PR node ID missing")?;
        let v=self.request(Method::POST,"/graphql",Some(&serde_json::json!({"query":"mutation($id: ID!) { markPullRequestReadyForReview(input:{pullRequestId:$id}) { pullRequest { isDraft } } }","variables":{"id":node}}))).await?;
        ensure!(
            v.get("errors").is_none()
                && v["data"]["markPullRequestReadyForReview"]["pullRequest"]["isDraft"] == false,
            "GitHub refused ready-for-review transition"
        );
        Ok(())
    }
    pub async fn protections(
        &self,
        p: &crate::config::Project,
    ) -> Result<Vec<(String, Option<i64>)>> {
        self.validate(&p.github_repo, true).await?;
        let branch = reqwest::Url::parse(&format!(
            "https://api.github.com/repos/{}/branches/x",
            p.github_repo
        ))?;
        let mut branch = branch;
        branch
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid URL"))?
            .pop()
            .push(&p.base_branch)
            .push("protection");
        let v = self.request(Method::GET, branch.path(), None).await?;
        ensure!(
            v["required_status_checks"]["strict"] == true,
            "strict branch protection required (up-to-date base)"
        );
        ensure!(
            v["enforce_admins"]["enabled"] == true,
            "branch protection must enforce administrators"
        );
        ensure!(
            v["allow_force_pushes"]["enabled"] != true && v["allow_deletions"]["enabled"] != true,
            "unsafe branch protection"
        );
        if let Some(b) = v["required_pull_request_reviews"].get("bypass_pull_request_allowances") {
            for k in ["users", "teams", "apps"] {
                ensure!(
                    b[k].as_array().is_none_or(Vec::is_empty),
                    "branch protection contains review bypass actors"
                );
            }
        }
        let rules = self
            .request(
                Method::GET,
                &format!(
                    "/repos/{}/rulesets?includes_parents=true&per_page=100",
                    p.github_repo
                ),
                None,
            )
            .await?;
        let rules = rules.as_array().context("invalid ruleset response")?;
        ensure!(rules.len() < 100, "ruleset list may be truncated");
        for rule in rules.iter().filter(|r| r["enforcement"] == "active") {
            let id = rule["id"].as_u64().context("ruleset ID missing")?;
            let detail = self
                .request(
                    Method::GET,
                    &format!("/repos/{}/rulesets/{id}", p.github_repo),
                    None,
                )
                .await?;
            ensure!(
                detail["bypass_actors"]
                    .as_array()
                    .is_some_and(Vec::is_empty),
                "active ruleset has bypass actors or unreadable bypass policy"
            );
        }
        let mut required: Vec<(String, Option<i64>)> = p
            .required_checks
            .iter()
            .map(|s| (s.clone(), None))
            .collect();
        if let Some(checks) = v["required_status_checks"]["checks"].as_array() {
            for c in checks {
                required.push((
                    c["context"]
                        .as_str()
                        .context("required context missing")?
                        .into(),
                    c["app_id"].as_i64().filter(|id| *id >= 0),
                ));
            }
        }
        if let Some(contexts) = v["required_status_checks"]["contexts"].as_array() {
            for c in contexts {
                required.push((c.as_str().context("invalid context")?.into(), None));
            }
        }
        required.sort();
        required.dedup();
        ensure!(
            !required.is_empty(),
            "required status checks must be enforced"
        );
        Ok(required)
    }
    pub async fn ci(
        &self,
        p: &crate::config::Project,
        head: &str,
        required: &[(String, Option<i64>)],
    ) -> Result<crate::policy::Ci> {
        let mut checks = vec![];
        let mut statuses = vec![];
        for page in 1..=100 {
            let v=self.request(Method::GET,&format!("/repos/{}/commits/{head}/check-runs?per_page=100&page={page}&filter=latest",p.github_repo),None).await?;
            let a = v["check_runs"].as_array().context("check_runs missing")?;
            checks.extend(a.iter().cloned());
            if a.len() < 100 {
                break;
            }
            ensure!(page < 100, "check list exceeds pagination bound");
        }
        for page in 1..=100 {
            let v = self
                .request(
                    Method::GET,
                    &format!(
                        "/repos/{}/commits/{head}/statuses?per_page=100&page={page}",
                        p.github_repo
                    ),
                    None,
                )
                .await?;
            let a = v.as_array().context("statuses missing")?;
            statuses.extend(a.iter().cloned());
            if a.len() < 100 {
                break;
            }
            ensure!(page < 100, "status list exceeds pagination bound");
        }
        crate::policy::ci(head, required, &checks, &statuses)
    }
    pub async fn squash(
        &self,
        p: &crate::config::Project,
        r: &crate::store::Run,
    ) -> Result<String> {
        let v=self.request(Method::PUT,&format!("/repos/{}/pulls/{}/merge",p.github_repo,r.pr.context("missing PR")?),Some(&serde_json::json!({"merge_method":"squash","sha":r.head_sha,"commit_title":r.story.title}))).await?;
        ensure!(v["merged"] == true, "GitHub did not confirm merge");
        Ok(v["sha"].as_str().context("merge SHA absent")?.into())
    }
}

#[cfg(test)]
impl Github {
    pub(crate) fn fixture(url: String) -> Self {
        Self {
            client: Client::builder()
                // Loopback fixtures must stay local even when the host sets a proxy.
                .no_proxy()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap(),
            token: "fixture-only-not-a-real-token".into(),
            api_base: url,
            push_remote: None,
        }
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn push_diagnostics_keep_rejection_reason_and_redact_credentials() {
        use base64::Engine;
        let token = "fixture+secret/token=";
        let basic =
            base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
        let stderr = format!(
            "remote: Permission denied\nfatal: https://x-access-token:{token}@github.com/repo\nAuthorization: Basic {basic}\nencoded=fixture%2Bsecret%2Ftoken%3D\nencoded=fixture%2bsecret%2ftoken%3d"
        );
        let result = super::push_diagnostics(&stderr, &format!("credential={basic}"), token);
        assert!(result.contains("remote: Permission denied"));
        assert!(result.contains("[REDACTED]"));
        for secret in [
            token,
            basic.as_str(),
            "fixture%2Bsecret%2Ftoken%3D",
            "fixture%2bsecret%2ftoken%3d",
        ] {
            assert!(!result.contains(secret));
        }
        assert!(
            super::push_diagnostics("", "! [rejected] non-fast-forward", token)
                .contains("non-fast-forward")
        );
        assert!(super::push_diagnostics("", "", token).contains("no diagnostic output"));
    }

    #[test]
    fn push_diagnostics_redact_before_truncating_and_handle_unicode() {
        let input = format!("{}secret-at-boundary", "é".repeat(4090));
        let result = super::push_diagnostics(&input, "", "secret-at-boundary");
        assert!(!result.contains("secret"));
        assert!(result.ends_with("[output truncated]"));
    }
    use super::*;
    use crate::{fixtures, model::Status, store::Store, test_support::Server};
    use serde_json::json;
    #[tokio::test]
    async fn repository_owner_can_run_only_with_automatic_merging_disabled() {
        let server = Server::start(|line, _| {
            assert!(line.starts_with("GET /repos/owner/a "));
            (
                200,
                json!({"permissions":{"push":true,"admin":true},"allow_squash_merge":true}),
            )
        });
        let gh = Github::fixture(server.url.clone());
        gh.validate("owner/a", false).await.unwrap();
        let error = gh.validate("owner/a", true).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("automatic merging requires a non-administrator")
        );
        let t = tempfile::tempdir().unwrap();
        let c = fixtures::config(t.path());
        assert!(gh.protections(&c.projects[0]).await.is_err());
    }

    #[tokio::test]
    async fn manual_merge_validation_still_requires_push_access_and_squash_support() {
        for repo in [
            json!({"permissions":{"push":false,"admin":false},"allow_squash_merge":true}),
            json!({"permissions":{"admin":true},"allow_squash_merge":true}),
            json!({"permissions":{"push":true,"admin":true},"allow_squash_merge":false}),
        ] {
            let server = Server::start(move |_, _| (200, repo.clone()));
            assert!(
                Github::fixture(server.url.clone())
                    .validate("owner/a", false)
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn non_admin_write_account_passes_repository_access_in_both_modes() {
        let server = Server::start(|_, _| {
            (
                200,
                json!({"permissions":{"push":true,"admin":false},"allow_squash_merge":true}),
            )
        });
        let gh = Github::fixture(server.url.clone());
        gh.validate("owner/a", false).await.unwrap();
        gh.validate("owner/a", true).await.unwrap();
    }

    #[tokio::test]
    async fn uncertain_create_is_reconciled_without_duplicate_pr() {
        let t = tempfile::tempdir().unwrap();
        let c = fixtures::config(t.path());
        let store = Store::open(&c.orchestrator.state_db).unwrap();
        store.sync_config(&c).unwrap();
        store
            .ingest(
                &c.projects[0],
                &fixtures::scan(&c.projects[0], vec![fixtures::story(Status::Ready)]),
                &c.orchestrator.worktree_root,
            )
            .unwrap();
        let mut run = store.runs().unwrap().remove(0);
        let mut created = false;
        let server = Server::start(move |line, _| {
            if line.starts_with("POST ") {
                created = true;
                (502, json!({"uncertain":"created but response lost"}))
            } else if created {
                (
                    200,
                    json!([{"number":7,"html_url":"https://github.com/owner/a/pull/7"}]),
                )
            } else {
                (200, json!([]))
            }
        });
        let gh = Github::fixture(server.url.clone());
        assert!(gh.draft_pr(&c.projects[0], &mut run).await.is_err());
        gh.draft_pr(&c.projects[0], &mut run).await.unwrap();
        assert_eq!(run.pr, Some(7));
        assert_eq!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(l, _)| l.starts_with("POST "))
                .count(),
            1
        );
    }
    #[tokio::test]
    async fn squash_request_is_pinned_to_head() {
        let t = tempfile::tempdir().unwrap();
        let c = fixtures::config(t.path());
        let store = Store::open(&c.orchestrator.state_db).unwrap();
        store.sync_config(&c).unwrap();
        store
            .ingest(
                &c.projects[0],
                &fixtures::scan(&c.projects[0], vec![fixtures::story(Status::Ready)]),
                &c.orchestrator.worktree_root,
            )
            .unwrap();
        let mut run = store.runs().unwrap().remove(0);
        run.pr = Some(7);
        run.head_sha = Some("a".repeat(40));
        let server = Server::start(|_, b| {
            assert_eq!(b["sha"], "a".repeat(40));
            assert_eq!(b["merge_method"], "squash");
            (200, json!({"merged":true,"sha":"b".repeat(40)}))
        });
        let gh = Github::fixture(server.url.clone());
        assert_eq!(
            gh.squash(&c.projects[0], &run).await.unwrap(),
            "b".repeat(40)
        );
    }
}
