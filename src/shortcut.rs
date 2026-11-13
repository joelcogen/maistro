//! Read-only Shortcut v4 workflow snapshots. Credentials never enter agent prompts.
use crate::{config::Project, poll::Fingerprint};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client as Http, Url};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    io::Read,
    os::unix::fs::PermissionsExt,
    path::Path,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const STORY_FIELDS: &str =
    "id,name,app_url,workflow,workflow_state,archived,position,created_at,updated_at";
const MAX_BODY: usize = 4 * 1024 * 1024;

pub fn read_token(path: &Path, workspace: &str) -> Result<String> {
    let file = std::fs::File::open(path).context("cannot open Shortcut token file")?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.permissions().mode() & 0o077 == 0,
        "Shortcut token must be a private regular file (chmod 600)"
    );
    let mut token = String::new();
    file.take(4099)
        .read_to_string(&mut token)
        .context("cannot read Shortcut token file")?;
    let token = token.trim_end_matches(['\r', '\n']);
    ensure!(
        token.len() <= 4096
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "invalid Shortcut token file"
    );
    let prefix = format!("sct_r_{workspace}_");
    let legacy = format!("sct_ro_{workspace}_");
    ensure!(
        [prefix, legacy]
            .iter()
            .any(|p| token.strip_prefix(p).is_some_and(|s| !s.is_empty())),
        "Shortcut polling requires a read-only v4 token for shortcut_workspace (sct_r_ or sct_ro_)"
    );
    Ok(token.to_owned())
}

struct Client {
    http: Http,
    base: Url,
    token: String,
}
impl Client {
    fn new(project: &Project) -> Result<Self> {
        let workspace = project
            .shortcut_workspace
            .as_deref()
            .context("missing shortcut_workspace")?;
        Ok(Self {
            http: Http::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()?,
            base: Url::parse(&format!("https://api.app.shortcut.com/api/v4/{workspace}/"))?,
            token: read_token(
                project
                    .shortcut_token_file
                    .as_deref()
                    .context("missing Shortcut token file")?,
                workspace,
            )?,
        })
    }

    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        let mut url = self.base.join(path)?;
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        let mut response = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Shortcut API request failed"))?;
        ensure!(
            response.status().is_success(),
            "Shortcut API returned HTTP {}",
            response.status().as_u16()
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("Shortcut API response interrupted"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= MAX_BODY,
                "Shortcut API page exceeds size limit; read incomplete"
            );
            bytes.extend_from_slice(&chunk);
        }
        let mut value: Value =
            serde_json::from_slice(&bytes).context("Shortcut API returned invalid JSON")?;
        if value.get("entity").is_some() {
            value = value["entity"].take();
        }
        Ok(value)
    }

    async fn list(&self, path: &str, fields: &str, extra: &[(&str, &str)]) -> Result<Vec<Value>> {
        let mut items = BTreeMap::new();
        let mut cursors = HashSet::new();
        let mut cursor: Option<String> = None;
        let mut expected_total = None;
        let mut collection_bytes = 0;
        for page in 1..=10_000_u64 {
            let mut query = vec![("fields", fields)];
            if let Some(cursor) = &cursor {
                query.push(("cursor", cursor));
            } else {
                query.push(("limit", "100"));
                query.extend_from_slice(extra);
            }
            let value = self.get(path, &query).await?;
            let entities = value["entities"]
                .as_array()
                .context("Shortcut list missing entities")?;
            let total = value["total_items"]
                .as_u64()
                .context("Shortcut list missing total_items")?;
            let pages = value["total_pages"]
                .as_u64()
                .context("Shortcut list missing total_pages")?;
            ensure!(
                value["current_page"].as_u64() == Some(page)
                    && value["current_items"].as_u64() == Some(entities.len() as u64),
                "Shortcut pagination metadata inconsistent"
            );
            if let Some((n, p)) = expected_total {
                ensure!(
                    n == total && p == pages,
                    "Shortcut collection changed during pagination"
                );
            } else {
                expected_total = Some((total, pages));
            }
            collection_bytes += serde_json::to_vec(entities)?.len();
            ensure!(
                collection_bytes <= 8 * 1024 * 1024,
                "Shortcut collection exceeds size limit; read incomplete"
            );
            for entity in entities {
                let id = if entity["id"].is_null()
                    && entity["deleted"] == true
                    && path.ends_with("/comments")
                {
                    u64::MAX - items.len() as u64
                } else {
                    entity["id"]
                        .as_u64()
                        .context("Shortcut entity missing numeric ID")?
                };
                ensure!(
                    items.insert(id, entity.clone()).is_none(),
                    "Shortcut pagination returned duplicate IDs"
                );
            }
            if value["next_page_url"].is_null() {
                ensure!(
                    items.len() as u64 == total
                        && (page == pages || (page == 1 && pages == 0 && total == 0)),
                    "Shortcut pagination ended before collection was complete"
                );
                return Ok(items.into_values().collect());
            }
            let next = Url::parse(
                value["next_page_url"]
                    .as_str()
                    .context("invalid Shortcut next_page_url")?,
            )
            .context("invalid Shortcut continuation URL")?;
            let endpoint = self.base.join(path)?;
            ensure!(
                next.origin() == endpoint.origin()
                    && next.path() == endpoint.path()
                    && next.username().is_empty()
                    && next.password().is_none()
                    && next.fragment().is_none(),
                "Shortcut continuation changed endpoint"
            );
            let tokens: Vec<_> = next
                .query_pairs()
                .filter(|(k, _)| k == "cursor")
                .map(|(_, v)| v.into_owned())
                .collect();
            ensure!(
                tokens.len() == 1 && !tokens[0].is_empty() && cursors.insert(tokens[0].clone()),
                "Shortcut pagination cursor missing or repeated"
            );
            cursor = Some(tokens[0].clone());
        }
        bail!("Shortcut pagination limit exceeded")
    }

    async fn workflow(&self, project: &Project) -> Result<Value> {
        let id = if let Some(id) = project.shortcut_workflow_id {
            id
        } else if let Some(name) = &project.shortcut_workflow {
            let workflows = self.list("workflows", "id,name", &[]).await?;
            let matches: Vec<_> = workflows
                .iter()
                .filter(|w| w["name"].as_str() == Some(name))
                .collect();
            ensure!(
                matches.len() == 1,
                "Shortcut workflow name missing or ambiguous"
            );
            matches[0]["id"].as_u64().context("invalid workflow ID")?
        } else {
            let url = Url::parse(
                project
                    .shortcut_url
                    .as_deref()
                    .context("missing Shortcut selector")?,
            )?;
            let parts: Vec<_> = url
                .path_segments()
                .context("invalid Shortcut workflow URL")?
                .collect();
            let pos = parts.iter().position(|p| *p == "workflows").context("Shortcut URL must identify a workflows/<id> path; use shortcut_workflow_id or shortcut_workflow")?;
            parts
                .get(pos + 1)
                .context("Shortcut URL missing workflow ID")?
                .parse::<u64>()
                .context("invalid Shortcut workflow ID in URL")?
        };
        let workflow = self
            .get(&format!("workflows/{id}"), &[("fields", "id,name")])
            .await?;
        ensure!(
            workflow["id"].as_u64() == Some(id),
            "Shortcut workflow identity mismatch"
        );
        Ok(workflow)
    }

    async fn board(&self, project: &Project) -> Result<Value> {
        let workflow = self.workflow(project).await?;
        let id = workflow["id"].as_u64().context("missing workflow ID")?;
        let path = format!("workflows/{id}");
        let states = self
            .list(&format!("{path}/states"), "id,name,type,position", &[])
            .await?;
        let s = &project.statuses;
        for label in [&s.backlog, &s.ready, &s.in_progress, &s.done, &s.blocked]
            .into_iter()
            .chain(s.review.iter())
            .chain(s.cancelled.iter())
        {
            ensure!(
                states
                    .iter()
                    .filter(|s| s["name"].as_str() == Some(label))
                    .count()
                    == 1,
                "Shortcut configured status label missing or ambiguous"
            );
        }
        let stories = self
            .list(
                &format!("{path}/stories"),
                STORY_FIELDS,
                &[("filter", "exclude_archived")],
            )
            .await?;
        for story in &stories {
            ensure!(
                story["workflow"]["id"].as_u64() == Some(id) && story["archived"] == false,
                "Shortcut workflow listing returned an out-of-scope story"
            );
            ensure!(
                story["workflow_state"]["id"]
                    .as_u64()
                    .is_some_and(|id| states.iter().any(|s| s["id"].as_u64() == Some(id))),
                "Shortcut story has unknown workflow state"
            );
            ensure!(
                story["updated_at"].is_string()
                    && story["name"].is_string()
                    && story["position"].is_number()
                    && story["created_at"].is_string(),
                "Shortcut story listing omitted required fields"
            );
        }
        Ok(
            json!({"workspace":project.shortcut_workspace, "workflow":workflow, "states":states, "stories":stories, "read":"complete"}),
        )
    }

    async fn observe(&self, project: &Project) -> Result<Fingerprint> {
        let board = self.board(project).await?;
        let mut detail = BTreeMap::new();
        let mut dependencies = BTreeMap::new();
        // Comments and dependency statuses are read explicitly: do not assume they
        // advance a parent story's updated_at. Completed/backlog stories remain in
        // the compact membership scan and wake coordination when edited or moved.
        for story in board["stories"].as_array().context("missing stories")? {
            let state_id = &story["workflow_state"]["id"];
            let state = board["states"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| &s["id"] == state_id)
                .unwrap();
            if ![
                &project.statuses.ready,
                &project.statuses.in_progress,
                &project.statuses.blocked,
            ]
            .iter()
            .any(|name| state["name"].as_str() == Some(name.as_str()))
            {
                continue;
            }
            let id = story["id"].as_u64().unwrap();
            let comments = self
                .list(
                    &format!("stories/{id}/comments"),
                    "id,text,deleted,updated_at",
                    &[],
                )
                .await?;
            let checklist = self
                .list(
                    &format!("stories/{id}/checklist-items"),
                    "id,description,completed,position,updated_at",
                    &[],
                )
                .await?;
            let links = self
                .list(
                    &format!("stories/{id}/story-links"),
                    "id,subject,object,verb",
                    &[],
                )
                .await?;
            for link in &links {
                for key in ["subject", "object"] {
                    let other = link[key]["id"]
                        .as_u64()
                        .context("Shortcut story link missing endpoint")?;
                    if other != id && !dependencies.contains_key(&other) {
                        let related = self
                            .get(
                                &format!("stories/{other}"),
                                &[("fields", "id,workflow,workflow_state,archived,updated_at")],
                            )
                            .await?;
                        ensure!(
                            related["id"].as_u64() == Some(other)
                                && related["workflow_state"]["id"].is_number(),
                            "Shortcut dependency read incomplete"
                        );
                        dependencies.insert(other, related);
                    }
                }
            }
            detail.insert(
                id,
                json!({"comments":comments,"checklist":checklist,"links":links}),
            );
        }
        // Never publish a fingerprint spanning a concurrent status/membership edit.
        ensure!(
            board == self.board(project).await?,
            "Shortcut board changed during observation"
        );
        Ok(Sha256::digest(serde_json::to_vec(
            &json!({"board":board,"detail":detail,"dependencies":dependencies}),
        )?)
        .into())
    }
}

pub async fn board(
    project: &Project,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Value> {
    tokio::select! {
        _ = cancel.cancelled() => bail!("Shortcut read cancelled"),
        result = tokio::time::timeout(timeout, async { Client::new(project)?.board(project).await }) =>
            result.context("Shortcut workflow read timed out")?,
    }
}

pub async fn run(
    project: Project,
    interval: Duration,
    timeout: Duration,
    sender: tokio::sync::mpsc::Sender<(String, Result<Fingerprint>)>,
    stop: CancellationToken,
) {
    loop {
        // Reopen credentials each pass so token rotation needs no restart.
        let observation = tokio::select! {
            _ = stop.cancelled() => break,
            result = tokio::time::timeout(timeout, async { Client::new(&project)?.observe(&project).await }) =>
                result.context("Shortcut poll timed out").and_then(|r| r),
        };
        tokio::select! {
            _ = stop.cancelled() => break,
            result = sender.send((project.id.clone(), observation)) => if result.is_err() { break; },
        }
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(interval) => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        extract::State,
        http::{HeaderMap, StatusCode, Uri},
        response::IntoResponse,
        routing::get,
    };
    use std::sync::{Arc, Mutex};

    type Routes = Arc<Mutex<BTreeMap<String, Value>>>;
    type Calls = Arc<Mutex<Vec<(String, BTreeMap<String, String>)>>>;
    struct Fixture {
        client: Client,
        routes: Routes,
        calls: Calls,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn respond(
        State((routes, calls)): State<(Routes, Calls)>,
        uri: Uri,
        headers: HeaderMap,
    ) -> axum::response::Response {
        assert_eq!(
            headers.get("authorization").unwrap(),
            "Bearer sct_r_team_secret"
        );
        let url = Url::parse(&format!("http://localhost{uri}")).unwrap();
        let query: BTreeMap<_, _> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        calls
            .lock()
            .unwrap()
            .push((url.path().to_owned(), query.clone()));
        let key = format!(
            "{}{}",
            url.path(),
            query
                .get("cursor")
                .map(|v| format!("?cursor={v}"))
                .unwrap_or_default()
        );
        match routes.lock().unwrap().get(&key).cloned() {
            Some(value) => axum::Json(value).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }
    impl Fixture {
        async fn new() -> Self {
            let routes = Routes::default();
            let calls = Calls::default();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let router = Router::new()
                .fallback(get(respond))
                .with_state((routes.clone(), calls.clone()));
            let task = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            Self {
                client: Client {
                    http: Http::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .build()
                        .unwrap(),
                    base: Url::parse(&format!("http://{address}/api/v4/team/")).unwrap(),
                    token: "sct_r_team_secret".into(),
                },
                routes,
                calls,
                task,
            }
        }
        fn set(&self, path: &str, value: Value) {
            self.routes
                .lock()
                .unwrap()
                .insert(format!("/api/v4/team/{path}"), value);
        }
        fn project(&self) -> Project {
            serde_json::from_value(json!({
                "id":"p", "repo":"/tmp", "mode":"master", "base_branch":"main",
                "shortcut_workflow_id":42, "shortcut_workspace":"team",
                "shortcut_token_file":"/tmp/not-used", "statuses":{"review":false,"cancelled":false}
            }))
            .unwrap()
        }
        fn board(&self) {
            self.set("workflows/42", json!({"entity":{"id":42,"name":"Agents"}}));
            self.set(
                "workflows/42/states",
                page(vec![
                    json!({"id":1,"name":"Backlog"}),
                    json!({"id":2,"name":"Ready"}),
                    json!({"id":3,"name":"In Progress"}),
                    json!({"id":4,"name":"Done"}),
                    json!({"id":5,"name":"Blocked"}),
                ]),
            );
            self.set("workflows/42/stories", page(vec![story(2)]));
            self.set("stories/7/comments", page(vec![]));
            self.set("stories/7/checklist-items", page(vec![]));
            self.set("stories/7/story-links", page(vec![]));
        }
    }
    fn page(entities: Vec<Value>) -> Value {
        json!({"current_page":1,"current_items":entities.len(),"total_items":entities.len(),
            "total_pages": if entities.is_empty(){0}else{1}, "entities":entities})
    }
    fn story(state: u64) -> Value {
        json!({"id":7,"name":"Task","workflow":{"id":42},"workflow_state":{"id":state},
            "archived":false,"position":1,"created_at":"2026-11-13T00:00:00Z",
            "updated_at":"2026-11-13T00:00:00Z","app_url":"https://app.shortcut.com/team/story/7"})
    }

    #[test]
    fn tokens_are_private_read_only_workspace_bound_and_never_serialized() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        for token in ["sct_r_team_secret", "sct_ro_team_secret\n"] {
            std::fs::write(&path, token).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(read_token(&path, "team").is_ok());
            assert!(read_token(&path, "other").is_err());
        }
        for token in [
            "sct_rw_team_secret",
            "v3_secret",
            "sct_r_team_",
            "sct_r_team_secret\nsecond",
        ] {
            std::fs::write(&path, token).unwrap();
            let error = read_token(&path, "team").unwrap_err().to_string();
            assert!(!error.contains(token));
        }
        std::fs::write(&path, "sct_r_team_secret").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_token(&path, "team").is_err());
        let p:Project=serde_json::from_value(json!({"id":"p","repo":"/tmp","mode":"master","base_branch":"main",
            "shortcut_workflow_id":42,"shortcut_workspace":"team","shortcut_token_file":"/private/token"})).unwrap();
        assert!(
            !serde_json::to_string(&p)
                .unwrap()
                .contains("/private/token")
        );
        let (reads, writes) = crate::agent::source_tools(&p);
        assert!(!reads.contains(&"list_stories") && !reads.contains(&"list_workflows"));
        assert!(reads.contains(&"get_story") && writes.contains(&"update_story"));
    }

    #[tokio::test]
    async fn scoped_pagination_decodes_cursor_and_keeps_only_fields_on_continuation() {
        let f = Fixture::new().await;
        let path = "workflows/42/stories";
        let next = format!("{}{}?cursor=a%2Bb%2Fc%3D", f.client.base, path);
        f.set(
            path,
            json!({"current_page":1,"current_items":1,"total_items":2,"total_pages":2,
            "entities":[{"id":2}],"next_page_url":next}),
        );
        f.set(
            &format!("{path}?cursor=a+b/c="),
            json!({"current_page":2,"current_items":1,"total_items":2,
            "total_pages":2,"entities":[{"id":1}]}),
        );
        let rows = f
            .client
            .list(path, "id", &[("filter", "exclude_archived")])
            .await
            .unwrap();
        assert_eq!(rows, json!([{"id":1},{"id":2}]).as_array().unwrap().clone());
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls[0].1["filter"], "exclude_archived");
        assert_eq!(calls[1].1.len(), 2);
        assert_eq!(calls[1].1["cursor"], "a+b/c=");
        assert_eq!(calls[1].1["fields"], "id");
        assert!(
            calls
                .iter()
                .all(|(p, _)| p.ends_with("/workflows/42/stories"))
        );
    }

    #[tokio::test]
    async fn pagination_failures_do_not_publish_partial_reads_or_follow_foreign_urls() {
        let f = Fixture::new().await;
        for next in [
            "https://foreign.invalid/api/v4/team/stories?cursor=secret".to_owned(),
            format!("{}stories?cursor=a&cursor=b", f.client.base),
            format!("{}stories?cursor=", f.client.base),
        ] {
            f.set(
                "stories",
                json!({"current_page":1,"current_items":1,"total_items":2,"total_pages":2,
                "entities":[{"id":1}],"next_page_url":next}),
            );
            assert!(f.client.list("stories", "id", &[]).await.is_err());
        }
        let next = format!("{}stories?cursor=second", f.client.base);
        f.set(
            "stories",
            json!({"current_page":1,"current_items":1,"total_items":2,"total_pages":2,
            "entities":[{"id":1}],"next_page_url":next}),
        );
        assert!(
            f.client
                .list("stories", "id", &[])
                .await
                .unwrap_err()
                .to_string()
                .contains("404")
        );
        f.set("stories?cursor=second",json!({"current_page":2,"current_items":1,"total_items":2,"total_pages":2,"entities":[{"id":1}]}));
        assert!(
            f.client
                .list("stories", "id", &[])
                .await
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        f.set("stories?cursor=second",json!({"current_page":2,"current_items":1,"total_items":3,"total_pages":2,"entities":[{"id":2}]}));
        assert!(f.client.list("stories", "id", &[]).await.is_err());
        f.set(
            "stories?cursor=second",
            json!({"current_page":2,"current_items":0,"total_items":2,"total_pages":2,
            "entities":[],"next_page_url":next}),
        );
        assert!(
            f.client
                .list("stories", "id", &[])
                .await
                .unwrap_err()
                .to_string()
                .contains("repeated")
        );
    }

    #[tokio::test]
    async fn snapshots_validate_workflow_names_states_and_membership() {
        let f = Fixture::new().await;
        f.board();
        let mut p = f.project();
        p.shortcut_workflow_id = None;
        p.shortcut_workflow = Some("Agents".into());
        f.set("workflows", page(vec![json!({"id":42,"name":"Agents"})]));
        assert_eq!(f.client.board(&p).await.unwrap()["workflow"]["id"], 42);
        f.set(
            "workflows",
            page(vec![
                json!({"id":42,"name":"Agents"}),
                json!({"id":43,"name":"Agents"}),
            ]),
        );
        assert!(f.client.board(&p).await.is_err());
        p = f.project();
        p.statuses.ready = "Wrong".into();
        assert!(f.client.board(&p).await.is_err());
        p = f.project();
        let mut wrong = story(2);
        wrong["workflow"]["id"] = json!(99);
        f.set("workflows/42/stories", page(vec![wrong]));
        assert!(f.client.board(&p).await.is_err());
        f.set("workflows/42/stories", page(vec![]));
        assert_eq!(f.client.board(&p).await.unwrap()["stories"], json!([]));
    }

    #[tokio::test]
    async fn fingerprints_wake_only_for_changes_including_comments_dependencies_and_membership() {
        let f = Fixture::new().await;
        f.board();
        let p = f.project();
        let mut schedule = crate::schedule::Schedule::new(["p".into()].into_iter());
        let baseline = f.client.observe(&p).await.unwrap();
        schedule.observe("p", baseline);
        let generation = schedule.ready()["p"];
        schedule.acknowledge("p", generation);
        assert_eq!(baseline, f.client.observe(&p).await.unwrap());
        assert!(!schedule.observe("p", baseline));
        assert!(schedule.ready().is_empty());
        f.set(
            "stories/7/comments",
            page(vec![
                json!({"id":8,"text":"Changed requirements","deleted":false}),
                json!({"id":null,"text":null,"deleted":true}),
            ]),
        );
        let comments = f.client.observe(&p).await.unwrap();
        assert_ne!(baseline, comments);
        assert!(schedule.observe("p", comments));
        f.set(
            "stories/7/checklist-items",
            page(vec![
                json!({"id":10,"description":"New acceptance criterion","completed":false}),
            ]),
        );
        assert_ne!(comments, f.client.observe(&p).await.unwrap());
        f.set(
            "stories/7/story-links",
            page(vec![
                json!({"id":1,"subject":{"id":99},"object":{"id":7},"verb":"blocks"}),
            ]),
        );
        f.set(
            "stories/99",
            json!({"entity":{"id":99,"workflow_state":{"id":20},"updated_at":"one"}}),
        );
        let blocked = f.client.observe(&p).await.unwrap();
        f.set(
            "stories/99",
            json!({"entity":{"id":99,"workflow_state":{"id":21},"updated_at":"two"}}),
        );
        assert_ne!(blocked, f.client.observe(&p).await.unwrap());
        // Moving/archiving/removing a story removes it from the scoped list.
        f.set("workflows/42/stories", page(vec![]));
        assert_ne!(baseline, f.client.observe(&p).await.unwrap());
        f.set("workflows/42/stories", page(vec![story(3)]));
        assert_ne!(baseline, f.client.observe(&p).await.unwrap());
    }
}
