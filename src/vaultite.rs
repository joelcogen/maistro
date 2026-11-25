use crate::{
    config::{Project, identifier},
    mcp::Client,
    model::{Assignment, Completion, Outcome},
    poll::Fingerprint,
    status::{BoardRead, CountedAssignment, CountedStatus, Counts, ReadState, Reason},
};
use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, ops::Range, path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

pub fn validate_connection(config: &toml::Value) -> Result<()> {
    ensure!(
        config.get("url").is_none()
            && config
                .get("command")
                .and_then(toml::Value::as_str)
                .and_then(|cmd| Path::new(cmd).file_name())
                .is_some_and(|name| name == "vau")
            && config
                .get("args")
                .and_then(toml::Value::as_array)
                .is_some_and(|args| args.len() == 1 && args[0].as_str() == Some("mcp")),
        "Vaultite requires a local stdio MCP connection with command vau and args [\"mcp\"]"
    );
    Ok(())
}

const COMMENTS: &str = "## Comments";
const MACHINE_AUTHOR: &str = "- **Maistro** · ";
const QUERY_LIMIT: usize = 100;

#[derive(Debug, PartialEq, serde::Serialize)]
struct Story {
    path: String,
    id: String,
    status: String,
    title: String,
    text: Option<String>,
}

impl Story {
    fn context(&self) -> Result<String> {
        let text = self
            .text
            .as_deref()
            .context("Vaultite story note was not read")?;
        let (_, body) = note_parts(text)?;
        let section = comments_section(body);
        let description = section.as_ref().map_or(body, |s| &body[..s.heading]);
        let comments = section.as_ref().map_or("", |s| &body[s.body.clone()]);
        let (machine, human): (Vec<_>, Vec<_>) = comments
            .lines()
            .partition(|line| line.starts_with(MACHINE_AUTHOR));
        Ok(format!(
            "Description\n{}\n\nHuman comments\n{}\n\nPrior Maistro notes\n{}",
            if description.trim().is_empty() {
                &self.title
            } else {
                description.trim()
            },
            human.join("\n").trim(),
            machine.join("\n"),
        ))
    }
}

#[derive(Debug, PartialEq, serde::Serialize)]
pub struct Base {
    source: String,
    stories: Vec<Story>,
}

fn actionable(project: &Project, status: &str) -> bool {
    status == project.statuses.ready || status == project.statuses.in_progress
}

impl Base {
    fn find(&self, id: &str) -> Option<&Story> {
        self.stories.iter().find(|story| story.id == id)
    }

    pub fn in_progress(&self, project: &Project, id: &str) -> bool {
        self.find(id)
            .is_some_and(|story| story.status == project.statuses.in_progress)
    }

    pub fn has_work(&self, project: &Project) -> bool {
        self.stories
            .iter()
            .any(|story| actionable(project, &story.status))
    }

    pub fn coordinator_view(&self, project: &Project) -> Result<Value> {
        let stories = self.stories.iter().filter(|story| !story.id.is_empty()).map(|story| {
            let context = actionable(project, &story.status).then(|| story.context()).transpose()?;
            Ok(json!({"id": story.id, "title": story.title, "status": story.status, "context": context}))
        }).collect::<Result<Vec<_>>>()?;
        Ok(json!({"stories": stories}))
    }

    pub fn observation(
        &self,
        project: &Project,
        reason: Reason,
        assignment: Option<&Assignment>,
    ) -> BoardRead {
        let statuses = &project.statuses;
        let count = |label: &str| self.stories.iter().filter(|s| s.status == label).count() as u64;
        BoardRead {
            project_id: project.id.clone(),
            counts: Some(Counts {
                ready: count(&statuses.ready),
                in_progress: count(&statuses.in_progress),
                blocked: count(&statuses.blocked),
                review: statuses.review.as_deref().map_or(0, count),
                backlog: count(&statuses.backlog),
                done: count(&statuses.done),
            }),
            read: ReadState::Complete,
            reason,
            counted_assignment: assignment.and_then(|assignment| {
                let story = self.find(&assignment.story_id)?;
                let status = if story.status == statuses.ready {
                    CountedStatus::Ready
                } else {
                    CountedStatus::InProgress
                };
                Some(CountedAssignment {
                    story_id: assignment.story_id.clone(),
                    status,
                })
            }),
        }
    }

    fn candidate(&self, project: &Project, id: &str) -> Result<&Story> {
        let story = self.find(id).context("Vaultite story is absent")?;
        ensure!(
            actionable(project, &story.status),
            "Vaultite story is outside Ready and In Progress"
        );
        if let Some(first) = self
            .stories
            .iter()
            .find(|s| s.status == project.statuses.in_progress)
        {
            ensure!(
                first.id == id,
                "recover the first Vaultite In Progress story before claiming Ready work"
            );
        }
        Ok(story)
    }

    pub fn prepare_assignment(&self, project: &Project, assignment: &mut Assignment) -> Result<()> {
        let story = self.candidate(project, &assignment.story_id)?;
        ensure!(
            assignment.recovery == (story.status == project.statuses.in_progress),
            "Vaultite assignment does not match the current story status"
        );
        assignment.title.clone_from(&story.title);
        assignment.context = story.context()?;
        ensure!(
            assignment.context.len() <= 500_000,
            "Vaultite story context exceeds the assignment size limit"
        );
        assignment.story_url = None;
        Ok(())
    }
}

fn note_parts(text: &str) -> Result<(&str, &str)> {
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().context("Vaultite note is empty")?;
    ensure!(
        first.trim_end() == "---",
        "Vaultite story needs frontmatter"
    );
    let mut end = first.len();
    for line in lines {
        if matches!(line.trim_end(), "---" | "...") {
            return Ok((&text[first.len()..end], &text[end + line.len()..]));
        }
        end += line.len();
    }
    bail!("Vaultite story has incomplete frontmatter")
}

fn property<'a>(frontmatter: &'a str, key: &str) -> Result<&'a str> {
    let mut found = frontmatter.lines().filter_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name == key).then_some(value.trim())
    });
    let value = found
        .next()
        .context("Vaultite story lacks a required property")?;
    ensure!(
        found.next().is_none(),
        "Vaultite story has a duplicate property"
    );
    Ok(value)
}

fn property_value(value: &str) -> Result<String> {
    if value.starts_with('"') {
        let mut input = serde_json::Deserializer::from_str(value).into_iter::<String>();
        let decoded = input.next().context("empty Vaultite property")??;
        let tail = value[input.byte_offset()..].trim();
        ensure!(
            tail.is_empty() || tail.starts_with('#'),
            "invalid Vaultite property"
        );
        return Ok(decoded);
    }
    if let Some(value) = value.strip_prefix('\'') {
        let (quoted, tail) = value
            .rsplit_once('\'')
            .context("invalid quoted Vaultite property")?;
        let tail = tail.trim();
        ensure!(
            tail.is_empty() || tail.starts_with('#'),
            "invalid Vaultite property"
        );
        return Ok(quoted.replace("''", "'"));
    }
    Ok(value
        .split_once(" #")
        .map_or(value, |(value, _)| value)
        .trim()
        .to_owned())
}

struct CommentsSection {
    heading: usize,
    body: Range<usize>,
}

fn comments_section(body: &str) -> Option<CommentsSection> {
    let mut offset = 0;
    let mut section: Option<CommentsSection> = None;
    let mut fence = None;
    for line in body.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        if fenced(&mut fence, line.trim_end_matches(['\r', '\n'])) {
            continue;
        }
        let heading = line.trim_end();
        if let Some(section) = &mut section {
            if heading.starts_with("## ") || heading.starts_with("# ") {
                section.body.end = start;
                break;
            }
        } else if heading == COMMENTS {
            section = Some(CommentsSection {
                heading: start,
                body: offset..body.len(),
            });
        }
    }
    section
}

fn updated_note(story: &Story, status: &str, comment: Option<&str>) -> Result<String> {
    let text = story
        .text
        .as_deref()
        .context("Vaultite story note was not read")?;
    let (frontmatter, body) = note_parts(text)?;
    let value = property(frontmatter, "status")?;
    let start = value.as_ptr() as usize - text.as_ptr() as usize;
    let end = start + value.len();
    let encoded = serde_json::to_string(status)?;
    let tail = value
        .rfind(" #")
        .filter(|i| property_value(&value[..*i]).is_ok());
    let mut updated = text[..start].to_owned();
    updated.push_str(&encoded);
    if let Some(tail) = tail {
        updated.push_str(&value[tail..]);
    }
    let Some(comment) = comment else {
        updated.push_str(&text[end..]);
        return Ok(updated);
    };
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let body_at = text.len() - body.len();
    if let Some(section) = comments_section(body) {
        let at = body_at + section.body.end;
        let before = &text[end..at];
        updated.push_str(before.trim_end_matches(['\r', '\n']));
        updated.push_str(&format!("{eol}{eol}{comment}{eol}"));
        if at < text.len() {
            updated.push_str(eol);
        }
        updated.push_str(&text[at..]);
    } else {
        updated.push_str(text[end..].trim_end_matches(['\r', '\n']));
        updated.push_str(&format!("{eol}{eol}{COMMENTS}{eol}{eol}{comment}{eol}"));
    }
    Ok(updated)
}

fn indentation(line: &str) -> usize {
    line.bytes().take_while(|byte| *byte == b' ').count()
}

fn fence(line: &str) -> Option<(u8, usize, &str)> {
    let indent = indentation(line);
    let tail = line.get(indent..)?;
    let marker = *tail.as_bytes().first()?;
    if indent > 3 || !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = tail.bytes().take_while(|c| *c == marker).count();
    (length >= 3).then(|| (marker, length, &tail[length..]))
}

/// Returns true for the fence itself and any line inside it.
fn fenced(open: &mut Option<(u8, usize)>, line: &str) -> bool {
    let marker = fence(line);
    if let Some((kind, length)) = *open {
        if marker.is_some_and(|(c, n, rest)| c == kind && n >= length && rest.trim().is_empty()) {
            *open = None;
        }
        return true;
    }
    if let Some((kind, length, _)) = marker {
        *open = Some((kind, length));
        return true;
    }
    false
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn comment(text: &str) -> String {
    format!(
        "{MACHINE_AUTHOR}{} · {}",
        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        single_line(text)
    )
}

fn completion_comment(completion: &Completion) -> String {
    let report = &completion.report;
    let outcome = if report.outcome == Outcome::Completed {
        "Completed"
    } else {
        "Blocked"
    };
    let mut text = format!(
        "**{outcome}** · {} · Elapsed {}m {}s · Commit {} · PR {}",
        report.summary,
        completion.elapsed_seconds / 60,
        completion.elapsed_seconds % 60,
        report.commit.as_deref().unwrap_or("N/A"),
        report.pr_url.as_deref().unwrap_or("N/A")
    );
    if !report.tests.is_empty() {
        text.push_str(&format!(" · Tests: {}", report.tests.join(", ")));
    }
    text.push_str(&format!(" <!-- maistro-completion:{} -->", completion.id));
    comment(&text)
}

const PAGE_SUFFIX: &str = "; to change it: edit_file, or write_file with base)";

/// Read complete raw Markdown, retaining pagination boundaries byte-for-byte.
/// A paged final line may have a display newline added by MCP. Drop it so an
/// edit matches the same prefix in either case and preserves the file's ending.
async fn read_pages(client: &Client, path: &str) -> Result<(String, bool)> {
    let prefix = format!("{path}\n\n");
    let mut text = String::new();
    let mut offset = 1;
    let mut total = None;
    loop {
        let answer = client
            .vaultite("read", json!({"path":path, "offset":offset}))
            .await?;
        let body = answer
            .strip_prefix(&prefix)
            .context("Vaultite read resolved a different file")?;
        let Some((page, footer)) = body.rsplit_once("\n(") else {
            ensure!(offset == 1, "Vaultite read lost its pagination metadata");
            return Ok((body.to_owned(), false));
        };
        ensure!(
            !footer.ends_with("characters; vau read prints it whole)"),
            "Vaultite read truncated a line; shorten the line before retrying"
        );
        let Some(footer) = footer.strip_suffix(PAGE_SUFFIX) else {
            ensure!(offset == 1, "Vaultite read lost its pagination metadata");
            return Ok((body.to_owned(), false));
        };
        let (range, remaining) = footer
            .split_once(" of ")
            .context("invalid Vaultite page range")?;
        let (count, next) = remaining
            .split_once("; more: offset ")
            .map_or((remaining, None), |(count, next)| (count, Some(next)));
        let count: usize = count.parse().context("invalid Vaultite page total")?;
        let (from, to) = if let Some(range) = range.strip_prefix("lines ") {
            let (from, to) = range
                .split_once('-')
                .context("invalid Vaultite page span")?;
            (from.parse::<usize>()?, to.parse::<usize>()?)
        } else {
            let line: usize = range
                .strip_prefix("line ")
                .context("invalid Vaultite page line")?
                .parse()?;
            (line, line)
        };
        ensure!(
            from == offset
                && to >= from
                && to <= count
                && total.is_none_or(|total| total == count)
                && page.lines().count() == to - from + 1,
            "inconsistent Vaultite pagination"
        );
        total = Some(count);
        if let Some(next) = next {
            let next: usize = next.parse()?;
            ensure!(
                to < count && next == to + 1 && page.ends_with('\n'),
                "invalid Vaultite continuation"
            );
            text.push_str(page);
            offset = next;
        } else {
            ensure!(to == count, "Vaultite read ended before the last page");
            text.push_str(page.strip_suffix('\n').unwrap_or(page));
            return Ok((text, true));
        }
    }
}

async fn read_text(client: &Client, path: &str) -> Result<String> {
    let (text, paged) = read_pages(client, path).await?;
    if paged {
        ensure!(
            read_pages(client, path).await? == (text.clone(), true),
            "Vaultite file changed during the paged read"
        );
    }
    Ok(text)
}

fn query_page(text: &str, offset: usize) -> Result<(usize, Option<usize>, Vec<Story>)> {
    let (summary, table) = text
        .trim_end()
        .split_once("\n\n")
        .context("invalid Vaultite query response")?;
    let (total, last, next) = if let Some(summary) = summary.strip_prefix("Rows ") {
        let (range, total) = summary
            .split_once(" of ")
            .context("invalid Vaultite query range")?;
        let (first, last) = range
            .split_once('-')
            .context("invalid Vaultite query span")?;
        ensure!(
            first.parse::<usize>()? == offset + 1,
            "invalid Vaultite query offset"
        );
        let (total, next) = total
            .strip_suffix('.')
            .context("invalid Vaultite query summary")?
            .split_once("; more: offset ")
            .map_or((total.trim_end_matches('.'), None), |(total, next)| {
                (total, Some(next))
            });
        (
            total
                .strip_suffix(" files")
                .context("invalid Vaultite query count")?
                .parse::<usize>()?,
            Some(last.parse::<usize>()?),
            next.map(str::parse).transpose()?,
        )
    } else {
        (
            summary
                .strip_suffix(" files.")
                .context("invalid Vaultite query count")?
                .parse::<usize>()?,
            None,
            None,
        )
    };
    if total == 0 {
        ensure!(
            offset == 0 && next.is_none() && table == "_None._",
            "invalid empty Vaultite query"
        );
        return Ok((0, None, Vec::new()));
    }
    let mut lines = table.lines();
    ensure!(
        lines.next() == Some("| Path | Id | Status |")
            && lines.next() == Some("| --- | --- | --- |"),
        "invalid Vaultite query columns"
    );
    let mut stories = Vec::new();
    for line in lines {
        let row = line
            .strip_prefix("| ")
            .and_then(|line| line.strip_suffix(" |"))
            .context("incomplete Vaultite query row")?;
        let mut cells = row.split(" | ");
        let path = cells.next().unwrap();
        let id = cells.next().context("missing Vaultite story ID")?;
        let status = cells.next().context("missing Vaultite story status")?;
        ensure!(
            cells.next().is_none() && !row.contains("\\|"),
            "invalid or truncated Vaultite story metadata"
        );
        let title = path
            .rsplit('/')
            .next()
            .and_then(|name| name.strip_suffix(".md"))
            .context("invalid Vaultite story note path")?;
        ensure!(
            !title.is_empty() && (id.is_empty() || identifier(id)),
            "invalid Vaultite story title or ID"
        );
        stories.push(Story {
            path: path.to_owned(),
            id: id.to_owned(),
            status: status.to_owned(),
            title: title.to_owned(),
            text: None,
        });
    }
    let end = offset + stories.len();
    ensure!(
        !stories.is_empty()
            && last.is_none_or(|last| last == end)
            && next.map_or(end == total, |next| next == end && next < total),
        "inconsistent Vaultite query pagination"
    );
    Ok((total, next, stories))
}

async fn list_stories(client: &Client, folder: &str) -> Result<Vec<Story>> {
    let mut stories = Vec::new();
    let mut offset = 0;
    let mut total = None;
    loop {
        let text = client
            .vaultite(
                "query",
                json!({
                    "from": [folder], "type": ["note"], "columns": ["path", "id", "status"],
                    "sort": ["story_order", "path"], "limit": QUERY_LIMIT, "offset": offset
                }),
            )
            .await?;
        let (count, next, page) = query_page(&text, offset)?;
        ensure!(
            total.is_none_or(|total| total == count),
            "Vaultite story count changed during query"
        );
        stories.extend(page);
        if let Some(next) = next {
            total = Some(count);
            offset = next;
        } else {
            break;
        }
    }
    let mut paths = HashSet::new();
    let mut ids = HashSet::new();
    let prefix = format!("{folder}/");
    for story in &stories {
        ensure!(
            story.path.starts_with(&prefix)
                && !story.path.contains(['\\', ':'])
                && story
                    .path
                    .split('/')
                    .all(|part| !part.is_empty() && !part.starts_with('.'))
                && paths.insert(&story.path),
            "unexpected or duplicate story path in Vaultite Base"
        );
        ensure!(
            story.id.is_empty() || ids.insert(&story.id),
            "duplicate story ID in Vaultite Base"
        );
    }
    Ok(stories)
}

async fn read(client: &Client, project: &Project) -> Result<Base> {
    let path = project.vaultite_base.as_deref().unwrap();
    let folder = path
        .strip_suffix(".base")
        .context("Vaultite Base path must end in .base")?;
    let source = read_text(client, path).await?;
    let folder_filter = format!("file.inFolder({})", serde_json::to_string(folder)?);
    ensure!(
        source.lines().any(|line| {
            let line = line.trim().strip_prefix("- ").unwrap_or("").trim();
            line == folder_filter
                || line.strip_prefix('\'').and_then(|l| l.strip_suffix('\''))
                    == Some(folder_filter.as_str())
        }),
        "Vaultite Base must filter to its same-named story folder"
    );
    let mut stories = list_stories(client, folder).await?;
    for story in &mut stories {
        if !actionable(project, &story.status) {
            continue;
        }
        ensure!(
            !story.id.is_empty(),
            "Ready and In Progress Vaultite stories need a note ID"
        );
        let text = read_text(client, &story.path).await?;
        let (frontmatter, _) = note_parts(&text)?;
        ensure!(
            property_value(property(frontmatter, "id")?)? == story.id
                && property_value(property(frontmatter, "status")?)? == story.status,
            "Vaultite story changed during the Base query"
        );
        story.text = Some(text);
    }
    Ok(Base { source, stories })
}

async fn save(client: &Client, story: &Story, status: &str, result: Option<&str>) -> Result<()> {
    let old = story
        .text
        .as_deref()
        .context("Vaultite story note was not read")?;
    let new = updated_note(story, status, result)?;
    client
        .vaultite(
            "edit_file",
            json!({"path": story.path, "old": old, "new": new}),
        )
        .await?;
    let actual = read_text(client, &story.path).await?;
    // Vaultite may add or update its own note timestamps when it writes the file.
    let comparable = |text: &str| -> Result<(Vec<String>, String)> {
        let (frontmatter, body) = note_parts(text)?;
        let frontmatter = frontmatter
            .lines()
            .filter(|line| !line.starts_with("updated:") && !line.starts_with("created:"))
            .map(str::to_owned)
            .collect();
        Ok((frontmatter, body.trim_end_matches(['\r', '\n']).to_owned()))
    };
    ensure!(
        comparable(&actual)? == comparable(&new)?,
        "Vaultite story changed before the write could be verified"
    );
    Ok(())
}

pub async fn snapshot(
    config: &toml::Value,
    project: &Project,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Base> {
    with_client(config, timeout, cancel, async |client| {
        read(client, project).await
    })
    .await
}

pub async fn deliver(
    config: &toml::Value,
    project: &Project,
    completion: &Completion,
    target: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<()> {
    with_client(config, timeout, cancel, async |client| {
        let base = read(client, project).await?;
        let Some(story) = base.find(&completion.story.story_id) else {
            return Ok(());
        };
        if story.status != project.statuses.in_progress {
            return Ok(());
        }
        let marker = format!("<!-- maistro-completion:{} -->", completion.id);
        let (_, body) = note_parts(
            story
                .text
                .as_deref()
                .context("Vaultite story note was not read")?,
        )?;
        let has_result = comments_section(body).is_some_and(|section| {
            body[section.body]
                .lines()
                .any(|line| line.starts_with(MACHINE_AUTHOR) && line.trim_end().ends_with(&marker))
        });
        let result = (!has_result).then(|| completion_comment(completion));
        save(client, story, target, result.as_deref()).await
    })
    .await
}

pub async fn decide(
    config: &toml::Value,
    project: &Project,
    observed: &Base,
    id: &str,
    blocker: Option<&str>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<()> {
    with_client(config, timeout, cancel, async |client| {
        let base = read(client, project).await?;
        ensure!(
            &base == observed,
            "Vaultite Base changed during coordination"
        );
        let story = base.candidate(project, id)?;
        match blocker {
            Some(reason) => {
                let note = comment(&format!("**Blocked** · {reason}"));
                save(client, story, &project.statuses.blocked, Some(&note)).await
            }
            None if story.status == project.statuses.in_progress => Ok(()),
            None => save(client, story, &project.statuses.in_progress, None).await,
        }
    })
    .await
}

pub async fn observe(client: &Client, project: &Project) -> Result<Fingerprint> {
    Ok(Sha256::digest(serde_json::to_vec(&read(client, project).await?)?).into())
}

async fn with_client<T>(
    config: &toml::Value,
    timeout: Duration,
    cancel: &CancellationToken,
    operation: impl AsyncFnOnce(&Client) -> Result<T>,
) -> Result<T> {
    let mut client = None;
    let result = tokio::select! {
        _ = cancel.cancelled() => bail!("Vaultite Base operation cancelled"),
        result = tokio::time::timeout(timeout, async {
            client = Some(Client::connect_vaultite(config).await?);
            operation(client.as_ref().unwrap()).await
        }) => result.context("Vaultite Base operation timed out").and_then(|result| result),
    };
    if let Some(client) = client {
        client.close().await;
    }
    result
}
