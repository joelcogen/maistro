//! Revision-bearing text comes from MCP results, never a model's transcription.
use crate::{
    config::Statuses,
    model::{Decision, PageReference, Scan, Status},
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

fn objects(result: &Value) -> Result<Vec<Value>> {
    let mut values = Vec::new();
    for block in result["content"]
        .as_array()
        .context("missing MCP content")?
    {
        if let Some(text) = block["text"].as_str() {
            let value: Value = serde_json::from_str(text).context("invalid MCP source JSON")?;
            if let Value::Array(items) = value {
                values.extend(items);
            } else {
                values.push(value);
            }
        }
    }
    Ok(values)
}

fn cell<'a>(row: &'a Value, names: &[&str]) -> Result<Option<&'a str>> {
    for (name, value) in row["cells"].as_object().context("missing row cells")? {
        let name = name.to_lowercase().replace('_', " ");
        if names.contains(&name.as_str()) {
            return Ok(Some(value.as_str().context("unreadable requirement cell")?));
        }
    }
    Ok(None)
}

pub fn canonicalize(scan: &mut Scan, evidence: &str, statuses: &Statuses) -> Result<()> {
    let mut rows = BTreeMap::new();
    let mut pages = BTreeMap::new();
    let mut comments: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut complete_comments = HashSet::new();
    for line in evidence.lines() {
        let call: Value = serde_json::from_str(line)?;
        let args = &call["arguments"];
        if call["tool"] == "get_page_markdown" {
            let reference = PageReference {
                workspace_id: args["workspace_id"]
                    .as_str()
                    .context("missing page workspace")?
                    .into(),
                page_id: args["page_id"].as_str().context("missing page ID")?.into(),
            };
            let text = call["result"]["content"]
                .as_array()
                .context("missing page content")?
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            ensure!(!text.trim().is_empty(), "empty referenced page response");
            if let Some(previous) = pages.insert(reference, text.clone()) {
                ensure!(
                    previous == text,
                    "referenced page changed during source read"
                );
            }
        }
        if args["workspace_id"].as_str() != Some(&scan.workspace_id) {
            continue;
        }
        match call["tool"].as_str() {
            Some("get_database_row_details")
                if args["database_id"].as_str() == Some(&scan.board_id)
                    && args["with_doc"] == true =>
            {
                for row in objects(&call["result"])? {
                    let id = row["id"]
                        .as_str()
                        .context("missing source row ID")?
                        .to_owned();
                    let title = cell(&row, &["title", "name"])?
                        .context("missing source title")?
                        .to_owned();
                    let description = cell(&row, &["description"])?.unwrap_or("");
                    let body = match row["doc"].as_str() {
                        Some(body) => body,
                        None if row["has_doc"] == false => "",
                        _ => anyhow::bail!("missing source body for {id}"),
                    };
                    let text = [description, body]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    let criteria = cell(&row, &["acceptance criteria"])?
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or(&text)
                        .to_owned();
                    let label = cell(&row, &["status"])?.context("missing source status")?;
                    let status = [
                        (&statuses.backlog, Status::Backlog),
                        (&statuses.ready, Status::Ready),
                        (&statuses.in_progress, Status::InProgress),
                        (&statuses.review, Status::Review),
                        (&statuses.done, Status::Done),
                        (&statuses.blocked, Status::Blocked),
                        (&statuses.cancelled, Status::Cancelled),
                    ]
                    .into_iter()
                    .find(|(configured, _)| configured.as_str() == label)
                    .map(|(_, status)| status)
                    .with_context(|| format!("unknown source status {label:?} for {id}"))?;
                    let source = (title, text, criteria, status);
                    if let Some(previous) = rows.insert(id, source.clone()) {
                        ensure!(
                            previous == source,
                            "requirements changed during source read"
                        );
                    }
                }
            }
            Some("list_comments") if args["target_type"] == "row" => {
                let id = args["target_id"]
                    .as_str()
                    .context("missing comment target")?;
                let entries = comments.entry(id.to_owned()).or_default();
                for page in objects(&call["result"])? {
                    for comment in page["comments"].as_array().context("missing comments")? {
                        let text = comment["content"].as_str().context("unreadable comment")?;
                        if text.starts_with("🤖 ") && text.contains("[maistro run=") {
                            continue;
                        }
                        let comment_id = comment["comment_id"]
                            .as_str()
                            .context("missing comment ID")?;
                        if let Some(previous) =
                            entries.insert(comment_id.to_owned(), text.to_owned())
                        {
                            ensure!(previous == text, "comment changed during source read");
                        }
                    }
                    if page["has_more"] == false {
                        complete_comments.insert(id.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    for story in &mut scan.stories {
        let (title, mut body, criteria, status) =
            rows.remove(&story.story_id).with_context(|| {
                format!("missing authoritative row/body read for {}", story.story_id)
            })?;
        ensure!(
            complete_comments.contains(&story.story_id),
            "incomplete comment read for {}",
            story.story_id
        );
        for (id, text) in comments.remove(&story.story_id).unwrap_or_default() {
            body.push_str(&format!("\n\nComment {id}:\n{text}"));
        }
        // Derive linked references from the original source as well, so an
        // omitted model reference cannot silently discard a required document.
        story.reference_pages.extend(linked_pages(&body));
        story.reference_pages.sort();
        story.reference_pages.dedup();
        ensure!(
            story.reference_pages.len() <= 32,
            "too many reference pages"
        );
        for reference in &story.reference_pages {
            ensure!(
                uuid::Uuid::parse_str(&reference.workspace_id).is_ok()
                    && uuid::Uuid::parse_str(&reference.page_id).is_ok(),
                "invalid page reference"
            );
            let url = format!(
                "https://appflowy.com/app/{}/{}",
                reference.workspace_id, reference.page_id
            );
            if let Some(text) = pages.get(reference) {
                body.push_str(&format!("\n\nReferenced page {url}:\n{text}"));
            } else {
                story.decision = Decision::Blocked;
                story.reason = format!(
                    "Referenced page {url} has no successful get_page_markdown read. Read that page using the configured AppFlowy MCP; if access fails, report the actual access error."
                );
            }
        }
        story.status = status;
        ensure!(
            story.status != Status::Ready || story.decision != Decision::Ignore,
            "Ready source row {} needs a readiness assessment; coordinator ignored it",
            story.story_id
        );
        story.title = title;
        story.description = body;
        story.acceptance_criteria = if criteria.trim().is_empty() {
            vec![]
        } else {
            vec![criteria]
        };
    }
    Ok(())
}

fn linked_pages(text: &str) -> Vec<PageReference> {
    text.split(|c: char| c.is_whitespace() || "<>[]()\"'".contains(c))
        .filter_map(|token| {
            let url = reqwest::Url::parse(token.trim_end_matches(['.', ',', ';'])).ok()?;
            if url.scheme() != "https" || url.host_str() != Some("appflowy.com") {
                return None;
            }
            let parts = url.path_segments()?.collect::<Vec<_>>();
            if parts.len() != 3 || parts[0] != "app" {
                return None;
            }
            uuid::Uuid::parse_str(parts[1]).ok()?;
            uuid::Uuid::parse_str(parts[2]).ok()?;
            Some(PageReference {
                workspace_id: parts[1].into(),
                page_id: parts[2].into(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fixtures, model::Status};
    use serde_json::json;
    fn evidence(scan: &Scan, body: &str, criteria: &str, comment: &str, status: &str) -> String {
        let call = |tool: &str, arguments: Value, result: Value| {
            json!({"tool":tool,"arguments":arguments,"result":{"content":[{"text":result.to_string()}]}}).to_string()
        };
        [
            call("get_database_row_details", json!({"workspace_id":scan.workspace_id,"database_id":scan.board_id,"with_doc":true}),
                json!({"id":scan.stories[0].story_id,"cells":{"Title":"Exact title","Acceptance criteria":criteria,"Status":status,"Execution ID":"projection"},"has_doc":true,"doc":body})),
            call("list_comments", json!({"workspace_id":scan.workspace_id,"target_type":"row","target_id":scan.stories[0].story_id}),
                json!({"comments":[{"comment_id":"b","content":"🤖 Status\n[maistro run=fixture]"},{"comment_id":"a","content":comment}],"has_more":false})),
        ].join("\n")
    }
    #[test]
    fn source_text_overrides_model_formatting_and_tracks_real_edits() {
        let t = tempfile::tempdir().unwrap();
        let c = fixtures::config(t.path());
        let mut scan = fixtures::scan(&c.projects[0], vec![fixtures::story(Status::Ready)]);
        let raw = evidence(
            &scan,
            "first.second",
            "must pass",
            "human requirement",
            "Ready",
        );
        canonicalize(&mut scan, &raw, &Statuses::default()).unwrap();
        let revision = scan.stories[0].revision();
        assert_eq!(
            scan.stories[0].description,
            "first.second\n\nComment a:\nhuman requirement"
        );
        scan.stories[0].title = "Model title".into();
        scan.stories[0].description = "first.\n\nsecond".into();
        scan.stories[0].acceptance_criteria = vec!["rephrased".into()];
        let raw = evidence(
            &scan,
            "first.second",
            "must pass",
            "human requirement",
            "In Progress",
        );
        canonicalize(&mut scan, &raw, &Statuses::default()).unwrap();
        assert_eq!(revision, scan.stories[0].revision());
        for (body, criteria, comment) in [
            ("first. second", "must pass", "human requirement"),
            ("first.second", "changed criteria", "human requirement"),
            ("first.second", "must pass", "changed comment"),
        ] {
            let raw = evidence(&scan, body, criteria, comment, "In Progress");
            canonicalize(&mut scan, &raw, &Statuses::default()).unwrap();
            assert_ne!(revision, scan.stories[0].revision());
        }
    }
    #[test]
    fn status_comes_only_from_the_configured_source_cell() {
        let t = tempfile::tempdir().unwrap();
        let c = fixtures::config(t.path());
        let mut scan = fixtures::scan(&c.projects[0], vec![fixtures::story(Status::Cancelled)]);
        let statuses = Statuses {
            ready: "Approved for work".into(),
            ..Statuses::default()
        };
        let raw = evidence(
            &scan,
            "Implement the feature",
            "",
            "Previously cancelled",
            "Approved for work",
        );
        canonicalize(&mut scan, &raw, &statuses).unwrap();
        assert_eq!(scan.stories[0].status, Status::Ready);
        let revision = scan.stories[0].revision();
        let cancelled = raw.replace("Approved for work", "Cancelled");
        canonicalize(&mut scan, &cancelled, &statuses).unwrap();
        assert_eq!(scan.stories[0].status, Status::Cancelled);
        assert_eq!(scan.stories[0].revision(), revision);
        assert!(canonicalize(&mut scan, &format!("{raw}\n{cancelled}"), &statuses).is_err());
        assert!(
            canonicalize(
                &mut scan,
                &raw.replace("Approved for work", "Unknown"),
                &statuses
            )
            .is_err()
        );
        assert!(canonicalize(&mut scan, &raw.replace("Status", "Last error"), &statuses).is_err());
        scan.stories[0].decision = Decision::Ignore;
        assert!(
            canonicalize(&mut scan, &raw, &statuses)
                .unwrap_err()
                .to_string()
                .contains("readiness assessment")
        );
    }
    #[test]
    fn prose_criteria_and_incomplete_source_reads() {
        let t = tempfile::tempdir().unwrap();
        let c = fixtures::config(t.path());
        let mut scan = fixtures::scan(&c.projects[0], vec![fixtures::story(Status::Ready)]);
        let raw = evidence(&scan, "complete prose requirement", "", "comment", "Ready");
        canonicalize(&mut scan, &raw, &Statuses::default()).unwrap();
        assert_eq!(
            scan.stories[0].acceptance_criteria,
            ["complete prose requirement"]
        );
        assert!(canonicalize(&mut scan, "", &Statuses::default()).is_err());
        let incomplete = raw.lines().next().unwrap();
        assert!(canonicalize(&mut scan, incomplete, &Statuses::default()).is_err());
        let wrong_database = raw.replace(&scan.board_id, &uuid::Uuid::new_v4().to_string());
        assert!(canonicalize(&mut scan, &wrong_database, &Statuses::default()).is_err());
    }
    #[test]
    fn referenced_pages_reach_worker_and_revision_without_authorizing_other_work() {
        let t = tempfile::tempdir().unwrap();
        let c = fixtures::config(t.path());
        let mut scan = fixtures::scan(&c.projects[0], vec![fixtures::story(Status::Ready)]);
        let workspace = uuid::Uuid::new_v4().to_string();
        let page = uuid::Uuid::new_v4().to_string();
        let url = format!("https://appflowy.com/app/{workspace}/{page}");
        let body = format!("Follow the [writing guide]({url}).");
        let raw = evidence(&scan, &body, "", "human comment", "Ready");
        canonicalize(&mut scan, &raw, &Statuses::default()).unwrap();
        assert_eq!(scan.stories[0].decision, Decision::Blocked);
        assert!(scan.stories[0].reason.contains("get_page_markdown"));
        let page_call = |text: &str| {
            serde_json::json!({
                "tool":"get_page_markdown", "arguments":{"workspace_id":workspace,"page_id":page},
                "result":{"content":[{"type":"text","text":text}]}
            })
            .to_string()
        };
        let complete = format!("{raw}\n{}", page_call("Be direct. Keep necessary details."));
        scan.stories[0].decision = Decision::Implement;
        canonicalize(&mut scan, &complete, &Statuses::default()).unwrap();
        assert_eq!(scan.stories.len(), 1);
        assert_eq!(scan.stories[0].reference_pages.len(), 1);
        assert!(
            scan.stories[0]
                .description
                .contains("Be direct. Keep necessary details.")
        );
        let revision = scan.stories[0].revision();
        canonicalize(&mut scan, &complete, &Statuses::default()).unwrap();
        assert_eq!(revision, scan.stories[0].revision());
        canonicalize(
            &mut scan,
            &format!("{raw}\n{}", page_call("New guide instructions.")),
            &Statuses::default(),
        )
        .unwrap();
        assert_ne!(revision, scan.stories[0].revision());

        // A named reference also passes the exact fetched text to the worker.
        let named = evidence(&scan, "Follow the writing guide.", "", "comment", "Ready");
        canonicalize(
            &mut scan,
            &format!("{named}\n{}", page_call("Named guide contents.")),
            &Statuses::default(),
        )
        .unwrap();
        assert!(
            scan.stories[0]
                .description
                .contains("Named guide contents.")
        );
    }
}
