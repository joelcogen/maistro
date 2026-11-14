//! Staged, canonical change observations. Status filtering never authorizes work.
use crate::{config::Project, mcp::Client};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub type Fingerprint = [u8; 32];

fn fingerprint(value: &Value) -> Fingerprint {
    // serde_json's default map is a BTreeMap; objects are key ordered. Arrays
    // retain semantic order (document blocks, select options, attachments, etc.).
    Sha256::digest(serde_json::to_vec(value).expect("JSON serializes")).into()
}
fn array(value: &Value) -> Result<&Vec<Value>> {
    value
        .as_array()
        .context("expected a complete AppFlowy list, not a pagination/preview envelope")
}
fn id(value: &Value) -> Result<&str> {
    value
        .get("id")
        .or_else(|| value.get("row_id"))
        .and_then(Value::as_str)
        .filter(|s| crate::config::identifier(s))
        .context("missing/invalid AppFlowy identity")
}
fn complete(value: &Value) -> Result<()> {
    ensure!(
        ["unavailable", "content_omitted", "truncated", "partial"]
            .iter()
            .all(|key| value.get(key) != Some(&Value::Bool(true))),
        "incomplete AppFlowy response"
    );
    Ok(())
}
fn continuation(page: &Value, offset: u64) -> Result<Option<u64>> {
    if !page["has_more"].as_bool().context("paging state missing")? {
        ensure!(
            page.get("next_offset").is_none_or(Value::is_null),
            "unexpected continuation"
        );
        return Ok(None);
    }
    let next = page["next_offset"]
        .as_u64()
        .context("continuation missing")?;
    ensure!(next > offset && next <= 1_000_000, "invalid continuation");
    Ok(Some(next))
}

fn keyed(items: &Value) -> Result<BTreeMap<String, Value>> {
    let mut result = BTreeMap::new();
    for item in array(items)? {
        complete(item)?;
        ensure!(
            result.insert(id(item)?.to_owned(), item.clone()).is_none(),
            "duplicate AppFlowy identity"
        );
    }
    Ok(result)
}

// Unknown schemas/cells retain full reads instead of hiding requirement edits.
fn needs_context(row: &Value, fields: &BTreeMap<String, Value>, project: &Project) -> bool {
    let status_fields: Vec<_> = fields.values().filter(|f| f["name"] == "Status").collect();
    if status_fields.len() != 1 {
        return true;
    }
    let field = status_fields[0];
    let status = row["cells"]
        .get("Status")
        .or_else(|| field["id"].as_str().and_then(|id| row["cells"].get(id)))
        .and_then(Value::as_str);
    let Some(status) = status else {
        return true;
    };
    let s = &project.statuses;
    // Unknown labels are conservative; disabled labels are not interpreted.
    ![
        Some(s.backlog.as_str()),
        Some(s.done.as_str()),
        s.review.as_deref(),
        s.cancelled.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|label| label == status)
}

fn referenced_rows(value: &Value, ids: &BTreeSet<String>) -> BTreeSet<String> {
    let text = value.to_string();
    ids.iter()
        .filter(|id| text.contains(id.as_str()))
        .cloned()
        .collect()
}

struct Board {
    workspace: String,
    database: String,
    view: String,
}
impl Client {
    async fn pages(&self, name: &'static str, mut request: Value, key: &str) -> Result<Value> {
        let mut items = vec![];
        let mut offset = 0;
        loop {
            request["offset"] = json!(offset);
            request["limit"] = json!(100);
            let page = self.call(name, request.clone()).await?;
            complete(&page)?;
            let batch = array(page.get(key).context("missing compact list")?)?;
            for item in batch {
                complete(item)?;
            }
            items.extend(batch.iter().cloned());
            match continuation(&page, offset)? {
                Some(next) => offset = next,
                None => return Ok(Value::Array(items)),
            }
        }
    }
    async fn markdown(&self, workspace: &str, row: &str) -> Result<Value> {
        let mut offset = 0;
        let mut hash = None;
        let mut text = String::new();
        loop {
            let page = self
                .call(
                    "appflowy_fetch",
                    json!({"workspace_id":workspace,
                "kind":"page", "id":row, "response_format":"markdown", "offset":offset}),
                )
                .await?;
            for key in ["unavailable", "content_omitted", "partial"] {
                ensure!(page[key] != true, "incomplete Markdown response");
            }
            let current = page["content_hash"]
                .as_str()
                .context("Markdown hash missing")?;
            ensure!(
                hash.as_deref().is_none_or(|h| h == current),
                "Markdown changed during read"
            );
            hash = Some(current.to_owned());
            ensure!(page["offset"] == offset, "Markdown offset mismatch");
            let chunk = page["data"]["markdown"]
                .as_str()
                .context("Markdown chunk missing")?;
            text.push_str(chunk);
            match continuation(&page, offset)? {
                Some(next) => {
                    ensure!(
                        next == offset + chunk.chars().count() as u64,
                        "Markdown continuation mismatch"
                    );
                    offset = next;
                }
                None => {
                    ensure!(page["truncated"] != true, "incomplete Markdown response");
                    ensure!(
                        Sha256::digest(text.as_bytes())
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>()
                            == current,
                        "Markdown content hash mismatch"
                    );
                    return Ok(json!(text));
                }
            }
        }
    }
    async fn resolve(&self, project: &Project) -> Result<Board> {
        let url = project
            .appflowy_url
            .as_ref()
            .map(|u| reqwest::Url::parse(u))
            .transpose()
            .context("invalid AppFlowy board URL")?;
        let tokens: Vec<String> = url
            .as_ref()
            .map(|u| {
                u.path_segments()
                    .into_iter()
                    .flatten()
                    .map(str::to_owned)
                    .chain(u.query_pairs().map(|(_, v)| v.into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let workspaces = self
            .pages("appflowy_fetch", json!({"kind":"workspaces"}), "data")
            .await?;
        let workspaces = array(&workspaces)?;
        let scoped_workspace = url.is_some()
            && workspaces.iter().any(|w| {
                tokens
                    .iter()
                    .any(|t| w["workspace_id"] == *t || w["id"] == *t)
            });
        let mut matches = vec![];
        for workspace in workspaces {
            let workspace = workspace
                .get("workspace_id")
                .or_else(|| workspace.get("id"))
                .and_then(Value::as_str)
                .context("missing workspace id")?;
            // AppFlowy URLs encode the workspace in the path. If it is present,
            // avoid unrelated workspaces (including inaccessible ones).
            if scoped_workspace && !tokens.iter().any(|t| t == workspace) {
                continue;
            }
            let databases = self
                .pages(
                    "appflowy_database",
                    json!({"workspace_id":workspace,"command":"list"}),
                    "items",
                )
                .await?;
            for database in array(&databases)? {
                let db_id = id(database)?;
                for view in array(database.get("views").context("database views missing")?)? {
                    let view_id = view
                        .get("view_id")
                        .or_else(|| view.get("id"))
                        .and_then(Value::as_str)
                        .context("view id missing")?;
                    let selected = if let Some(name) = &project.appflowy_page {
                        view["name"] == *name || view_id == name
                    } else {
                        tokens.iter().any(|t| t == view_id)
                    };
                    if selected {
                        matches.push(Board {
                            workspace: workspace.into(),
                            database: db_id.into(),
                            view: view_id.into(),
                        });
                    }
                }
            }
        }
        ensure!(
            matches.len() == 1,
            "configured board is missing or ambiguous; use its AppFlowy URL or view ID"
        );
        Ok(matches.remove(0))
    }
    async fn comments(&self, workspace: &str, target: &str, kind: &str) -> Result<Value> {
        let mut comments = BTreeMap::new();
        let mut offset = 0;
        loop {
            let page = self.call("appflowy_fetch", json!({"kind":"comments","workspace_id":workspace,"id":target,"target_type":kind,"limit":100,"offset":offset})).await?;
            complete(&page)?;
            for comment in array(page.get("comments").context("comments missing")?)? {
                complete(comment)?;
                let id = comment["comment_id"]
                    .as_str()
                    .context("comment id missing")?;
                ensure!(
                    comments.insert(id.to_owned(), comment.clone()).is_none(),
                    "duplicate comments during paging"
                );
            }
            if !page["has_more"]
                .as_bool()
                .context("comment paging state missing")?
            {
                ensure!(
                    page.get("next_offset").is_none_or(Value::is_null),
                    "unexpected comment continuation"
                );
                break;
            }
            let next = page["next_offset"]
                .as_u64()
                .context("comment continuation missing")?;
            ensure!(
                next > offset && next <= 1_000_000,
                "invalid comment continuation"
            );
            offset = next;
        }
        Ok(serde_json::to_value(comments)?)
    }
    async fn scan(&self, board: &Board, project: &Project) -> Result<Fingerprint> {
        let metadata = self
            .call(
                "appflowy_fetch",
                json!({"kind":"workspace_folder","workspace_id":board.workspace,"id":board.view,"depth":1}),
            )
            .await?;
        complete(&metadata)?;
        let metadata = metadata.get("data").context("page data missing")?;
        let metadata = metadata.get("view").unwrap_or(metadata);
        complete(metadata)?;
        ensure!(
            metadata["view_id"] == board.view || metadata["id"] == board.view,
            "board metadata identity mismatch"
        );
        let fields_args =
            json!({"workspace_id":board.workspace,"database_id":board.database,"command":"fields"});
        let rows_args = json!({"workspace_id":board.workspace,"database_id":board.database,"command":"list","with_doc":false});
        let fields = keyed(
            &self
                .pages("appflowy_database", fields_args.clone(), "items")
                .await?,
        )?;
        let rows = keyed(
            &self
                .pages("appflowy_rows", rows_args.clone(), "items")
                .await?,
        )?;
        for row in rows.values() {
            ensure!(row["cells"].is_object(), "row cells missing");
            ensure!(row["has_doc"].is_boolean(), "row document presence missing");
        }
        // Complete the cheap cell scan before fetching any bodies or comments.
        let all_ids: BTreeSet<_> = rows.keys().cloned().collect();
        let mut pending: BTreeSet<_> = rows
            .iter()
            .filter(|(_, row)| needs_context(row, &fields, project))
            .map(|(id, _)| id.clone())
            .collect();
        let mut loaded = BTreeMap::new();
        while let Some(row_id) = pending.pop_first() {
            if loaded.contains_key(&row_id) {
                continue;
            }
            let row = &rows[&row_id];
            let has_doc = row["has_doc"].as_bool().unwrap();
            let body = if has_doc {
                self.markdown(&board.workspace, &row_id).await?
            } else {
                Value::Null
            };
            ensure!(!has_doc || body.is_string(), "incomplete row document");
            let comments = self.comments(&board.workspace, &row_id, "row").await?;
            let inline_comments = if has_doc {
                let doc = uuid::Uuid::new_v5(
                    &uuid::Uuid::parse_str(&row_id).context("invalid row UUID")?,
                    b"document_id",
                );
                self.comments(&board.workspace, &doc.to_string(), "document")
                    .await?
            } else {
                Value::Null
            };
            let context = json!({"cells":row["cells"],"body":body,
                "comments":comments,"inline_comments":inline_comments});
            pending.extend(referenced_rows(&context, &all_ids));
            // Free-form dependency cells without a resolvable row ID may name
            // stories rather than link them. Preserve detection conservatively.
            if row["cells"]
                .as_object()
                .unwrap()
                .iter()
                .any(|(key, value)| {
                    let name = fields
                        .get(key)
                        .and_then(|f| f["name"].as_str())
                        .unwrap_or(key);
                    name.eq_ignore_ascii_case("dependencies")
                        && value.as_str().is_some_and(|text| !text.trim().is_empty())
                        && referenced_rows(value, &all_ids).is_empty()
                })
            {
                pending.extend(all_ids.iter().cloned());
            }
            loaded.insert(row_id, context);
        }
        let cells: BTreeMap<_, _> = rows
            .iter()
            .map(|(id, row)| (id, json!({"cells":row["cells"],"has_doc":row["has_doc"]})))
            .collect();
        // Membership/schema reads bookend each scan. Two equal complete scans
        // below also protect offset paging and cross-resource races. This is
        // still eventual consistency, not a transactional snapshot.
        ensure!(
            rows.keys()
                .eq(keyed(&self.pages("appflowy_rows", rows_args, "items").await?)?.keys()),
            "board membership changed during read"
        );
        ensure!(
            fields
                == keyed(
                    &self
                        .pages("appflowy_database", fields_args, "items")
                        .await?
                )?,
            "board schema changed during read"
        );
        Ok(fingerprint(
            &json!({"workspace":board.workspace,"database":board.database,"view":board.view,
            "metadata":{"name":metadata["name"],"icon":metadata["icon"],"layout":metadata["layout"],"extra":metadata["extra"]},
            "fields":fields,"rows":cells,"context":loaded}),
        ))
    }
    pub async fn observe(&self, project: &Project) -> Result<Fingerprint> {
        let board = self.resolve(project).await?;
        let first = self.scan(&board, project).await?;
        let second = self.scan(&board, project).await?;
        ensure!(
            first == second,
            "board changed during observation; retrying next interval"
        );
        Ok(second)
    }
}

/// Resolve the configured board afresh for the coordinator, using the same identity
/// checks as polling. Only identities are supplied; story reads remain fresh.
pub(crate) async fn board_context(
    config: &toml::Value,
    project: &Project,
    timeout: std::time::Duration,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Value> {
    let mut client = None;
    let result = tokio::select! {
        _ = cancel.cancelled() => Err(anyhow::anyhow!("AppFlowy board resolution cancelled")),
        result = tokio::time::timeout(timeout, async {
            client = Some(Client::connect(config).await?);
            let board = client.as_ref().unwrap().resolve(project).await?;
            Ok::<_, anyhow::Error>(json!({"workspace_id":board.workspace,
                "database_id":board.database,"view_id":board.view}))
        }) => result.context("AppFlowy board resolution timed out").and_then(|r| r),
    };
    if let Some(client) = client {
        client.close().await;
    }
    result
}

/// Each project polls independently, including while the single coordinator runs.
/// A failed scan never emits a replacement fingerprint.
pub async fn run(
    config: toml::Value,
    project: Project,
    interval: std::time::Duration,
    timeout: std::time::Duration,
    sender: tokio::sync::mpsc::Sender<(String, Result<Fingerprint>)>,
    stop: tokio_util::sync::CancellationToken,
) {
    let mut client = None;
    loop {
        let observation = tokio::select! {
            _ = stop.cancelled() => break,
            result = tokio::time::timeout(timeout, async {
                if client.is_none() { client = Some(Client::connect(&config).await?); }
                client.as_ref().unwrap().observe(&project).await
            }) => result.context("AppFlowy board poll timed out").and_then(|r| r),
        };
        let failed = observation.is_err();
        tokio::select! {
            _ = stop.cancelled() => break,
            result = sender.send((project.id.clone(), observation)) => if result.is_err() { break; },
        }
        if failed && let Some(c) = client.take() {
            c.close().await;
        }
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(interval) => {},
        }
    }
    if let Some(c) = client {
        c.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::Schedule;
    use std::fs;

    struct Fixture {
        root: tempfile::TempDir,
        project: Project,
        data: Value,
    }
    impl Fixture {
        async fn new() -> (Self, Client) {
            let root = tempfile::tempdir().unwrap();
            let script = root.path().join("mcp.py");
            fs::write(&script, include_str!("../tests/fixtures/mcp.py")).unwrap();
            let config: toml::Value = toml::from_str(&format!(
                "command='python3'\nargs=['{}','{}']",
                script.display(),
                root.path().display()
            ))
            .unwrap();
            let project: Project = serde_json::from_value(json!({"id":"p0","repo":"/unused","appflowy_page":"board-0","mode":"master","base_branch":"master"})).unwrap();
            let data = json!({"p0":{"rows":[
                {"id":"11111111-1111-1111-1111-111111111111","title":"Ready story","status":"Ready","body":"# Requirements\n- [ ] task", "comments":[
                    {"comment_id":"c1","content":"first","author_id":"user"},
                    {"comment_id":"c2","content":"second","author_id":"luna"},
                    {"comment_id":"c3","content":"third","author_id":"astra"}
                ],"inline_comments":[]},
                {"id":"22222222-2222-2222-2222-222222222222","status":"Backlog"}
            ]}});
            let fixture = Self {
                root,
                project,
                data,
            };
            fixture.save();
            let client = Client::connect(&config).await.unwrap();
            (fixture, client)
        }
        fn save(&self) {
            fs::write(
                self.root.path().join("board.json"),
                serde_json::to_vec(&self.data).unwrap(),
            )
            .unwrap();
        }
        async fn observe(&self, client: &Client) -> Fingerprint {
            client.observe(&self.project).await.unwrap()
        }
    }
    #[tokio::test]
    async fn compact_rejects_changed_markdown_hash_and_legacy_manifest() {
        let (f, client) = Fixture::new().await;
        fs::write(f.root.path().join("poll-bad-hash"), "").unwrap();
        assert!(
            format!("{:#}", client.observe(&f.project).await.unwrap_err())
                .contains("Markdown changed")
        );
        client.close().await;
        fs::write(f.root.path().join("legacy-profile"), "").unwrap();
        let config = toml::from_str(&format!(
            "command='python3'\nargs=['{}','{}']",
            f.root.path().join("mcp.py").display(),
            f.root.path().display()
        ))
        .unwrap();
        let error = match Client::connect(&config).await {
            Ok(_) => panic!("accepted legacy server"),
            Err(e) => e,
        };
        assert!(format!("{error:#}").contains("APPFLOWY_TOOL_PROFILE=compact"));
    }

    #[tokio::test]
    async fn complete_fingerprint_covers_membership_cells_bodies_all_comments_and_schema() {
        let (mut f, client) = Fixture::new().await;
        let initial = f.observe(&client).await;
        // Transport metadata changes between reads; normalized observations remain stable.
        for _ in 0..3 {
            assert_eq!(initial, f.observe(&client).await);
        }
        let original = f.data.clone();
        let mutations = [
            ("/p0/rows/0/title", json!("Edited Done title")),
            ("/p0/rows/1/status", json!("Cancelled")),
            ("/p0/rows/0/cell", json!({"arbitrary":123})),
            ("/p0/rows/0/body", json!("# Requirements\n- [x] task")),
            ("/p0/rows/0/comments/0/content", json!("user edit")),
            ("/p0/rows/0/comments/1/content", json!("Luna edit")),
            (
                "/p0/rows/0/comments/2/content",
                json!("Astra edit on second page"),
            ),
            (
                "/p0/rows/0/inline_comments",
                json!([{"comment_id":"inline","content":"reply"}]),
            ),
        ];
        for (path, value) in mutations {
            f.data = original.clone();
            if path.ends_with("/cell") {
                f.data["p0"]["rows"][0]["cell"] = value;
            } else {
                *f.data.pointer_mut(path).unwrap() = value;
            }
            f.save();
            assert_ne!(initial, f.observe(&client).await, "missed {path}");
        }
        f.data = original.clone();
        f.data["p0"]["rows"].as_array_mut().unwrap().pop();
        f.save();
        assert_ne!(initial, f.observe(&client).await);
        f.data = original.clone();
        f.data["p0"]["rows"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"33333333-3333-3333-3333-333333333333","status":"Backlog"}));
        f.save();
        assert_ne!(initial, f.observe(&client).await);
        for fields in [
            json!([]),
            json!([{"id":"title","name":"Renamed","type_option":{"options":["new"]}}]),
        ] {
            f.data = original.clone();
            f.data["p0"]["fields"] = fields;
            f.save();
            assert_ne!(initial, f.observe(&client).await);
        }
        f.data = original.clone();
        f.data["p0"]["rows"][0]["comments"]
            .as_array_mut()
            .unwrap()
            .reverse();
        f.data["p0"]["rows"].as_array_mut().unwrap().reverse();
        f.save();
        assert_eq!(initial, f.observe(&client).await);
        // External linked documents are not read or part of the observation.
        f.data = original;
        f.data["p0"]["external_document"] = json!("reference changed");
        f.save();
        assert_eq!(initial, f.observe(&client).await);
        client.close().await;
    }
    #[tokio::test]
    async fn staged_reads_skip_inactive_context_but_track_requirements_and_dependencies() {
        let (mut f, client) = Fixture::new().await;
        let dependency = "22222222-2222-2222-2222-222222222222";
        f.data["p0"]["rows"][1]["body"] = json!("dependency requirements");
        f.data["p0"]["rows"][1]["status"] = json!("Done");
        f.save();
        let baseline = f.observe(&client).await;
        // Inactive body changes alone no longer force coordination.
        f.data["p0"]["rows"][1]["body"] = json!("unrelated edit");
        f.save();
        assert_eq!(baseline, f.observe(&client).await);
        // All cells remain watched, including dependency status transitions.
        f.data["p0"]["rows"][1]["status"] = json!("Backlog");
        f.save();
        assert_ne!(baseline, f.observe(&client).await);
        // A body link makes even a Done dependency's context relevant.
        f.data["p0"]["rows"][0]["body"] = json!(format!("Depends on {dependency}"));
        f.data["p0"]["rows"][1]["status"] = json!("Done");
        f.save();
        let linked = f.observe(&client).await;
        f.data["p0"]["rows"][1]["body"] = json!("changed linked requirement");
        f.save();
        assert_ne!(linked, f.observe(&client).await);
        // Blocked stories still detect human clarifications in comments.
        f.data["p0"]["rows"][0]["status"] = json!("Blocked");
        f.save();
        let blocked = f.observe(&client).await;
        f.data["p0"]["rows"][0]["comments"][0]["content"] = json!("clarification");
        f.save();
        assert_ne!(blocked, f.observe(&client).await);
        // An entirely inactive board performs no body/comment reads at all.
        f.data["p0"]["rows"][0]["status"] = json!("Done");
        f.save();
        fs::write(f.root.path().join("poll-events"), "").unwrap();
        f.observe(&client).await;
        let events = fs::read_to_string(f.root.path().join("poll-events")).unwrap();
        assert!(!events.contains("get_page_markdown"));
        assert!(!events.contains("list_comments"));
        // Becoming Ready makes previously skipped context visible immediately.
        f.data["p0"]["rows"][1]["status"] = json!("Ready");
        f.save();
        f.observe(&client).await;
        assert!(
            fs::read_to_string(f.root.path().join("poll-events"))
                .unwrap()
                .contains("get_page_markdown")
        );
        client.close().await;
    }

    #[test]
    fn context_selection_supports_named_cells_custom_labels_and_unknown_schemas() {
        let mut project: Project = serde_json::from_value(json!({
            "id":"p", "repo":"/unused", "appflowy_page":"Board",
            "mode":"master", "base_branch":"main"
        }))
        .unwrap();
        project.statuses.done = "Shipped".into();
        let fields = keyed(&json!([{"id":"status-id","name":"Status"}])).unwrap();
        assert!(!needs_context(
            &json!({"cells":{"Status":"Shipped"}}),
            &fields,
            &project
        ));
        assert!(!needs_context(
            &json!({"cells":{"status-id":"Shipped"}}),
            &fields,
            &project
        ));
        for status in ["Ready", "In Progress", "Blocked", "Unknown", "Done"] {
            assert!(needs_context(
                &json!({"cells":{"Status":status}}),
                &fields,
                &project
            ));
        }
        assert!(needs_context(&json!({"cells":{}}), &fields, &project));
        assert!(needs_context(
            &json!({"cells":{"Status":"Shipped"}}),
            &BTreeMap::new(),
            &project
        ));
    }

    #[tokio::test]
    async fn failures_and_partial_reads_retain_baseline_and_recovery_detects_changes() {
        let (mut f, client) = Fixture::new().await;
        let mut schedule = Schedule::new(["p0".to_owned()].into_iter());
        let baseline = f.observe(&client).await;
        schedule.observe("p0", baseline);
        let generation = schedule.ready()["p0"];
        schedule.acknowledge("p0", generation);
        for failure in [
            "fail",
            "incomplete",
            "paged",
            "churn",
            "bad-comments",
            "body-error",
        ] {
            let marker = f.root.path().join(format!("poll-{failure}-p0"));
            fs::write(&marker, "").unwrap();
            assert!(
                client.observe(&f.project).await.is_err(),
                "accepted {failure}"
            );
            assert!(schedule.ready().is_empty());
            fs::remove_file(marker).unwrap();
            assert!(!schedule.observe("p0", f.observe(&client).await));
        }
        f.data["p0"]["rows"][0]["title"] = json!("changed during outage");
        f.save();
        assert!(schedule.observe("p0", f.observe(&client).await));
        assert_eq!(schedule.ready().len(), 1);
        client.close().await;
    }
    #[tokio::test]
    async fn empty_boards_and_url_resolution_are_valid_but_ambiguity_is_not() {
        let (mut f, client) = Fixture::new().await;
        f.data["p0"]["rows"] = json!([]);
        f.save();
        let empty = f.observe(&client).await;
        f.project.appflowy_page = None;
        f.project.appflowy_url =
            Some("https://appflowy.example/workspace/workspace?v=view-p0".into());
        assert_eq!(empty, f.observe(&client).await);
        f.project.appflowy_url = None;
        f.project.appflowy_page = Some("missing board".into());
        assert!(client.observe(&f.project).await.is_err());
        client.close().await;
    }
    #[test]
    fn canonical_objects_preserve_semantic_array_order() {
        assert_eq!(
            fingerprint(&serde_json::from_str::<Value>(r#"{"b":2,"a":{"d":4,"c":3}}"#).unwrap()),
            fingerprint(&json!({"a":{"c":3,"d":4},"b":2}))
        );
        assert_ne!(
            fingerprint(&json!(["first", "second"])),
            fingerprint(&json!(["second", "first"]))
        );
        assert!(keyed(&json!([{"id":"same"},{"id":"same"}])).is_err());
    }
}
