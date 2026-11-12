use crate::config::{Mode, identifier};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub project_id: String,
    pub story_id: String,
    pub title: String,
    pub story_url: Option<String>,
    /// Complete requirements, human comments and relevant linked documents.
    pub context: String,
    pub recovery: bool,
    /// Explicit request to deliver this story via a PR, overriding the project default.
    #[serde(default)]
    pub pr: Option<PrRequest>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrRequest {
    /// None uses the stable generated story branch.
    pub branch: Option<String>,
}

impl Assignment {
    pub fn effective_mode(&self, default: Mode) -> Mode {
        if self.pr.is_some() { Mode::Pr } else { default }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoryRef {
    pub project_id: String,
    pub story_id: String,
}
impl From<&Assignment> for StoryRef {
    fn from(a: &Assignment) -> Self {
        Self {
            project_id: a.project_id.clone(),
            story_id: a.story_id.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub outcome: Outcome,
    pub summary: String,
    pub tests: Vec<String>,
    pub commit: Option<String>,
    pub pr_url: Option<String>,
}
impl Report {
    pub fn blocked(message: String) -> Self {
        Self {
            outcome: Outcome::Blocked,
            summary: message,
            tests: vec![],
            commit: None,
            pr_url: None,
        }
    }
    pub fn validate(&self, mode: Mode) -> Result<()> {
        ensure!(
            !self.summary.trim().is_empty(),
            "worker returned no summary"
        );
        if self.outcome == Outcome::Completed {
            ensure!(
                self.commit.as_ref().is_some_and(|s| !s.trim().is_empty()),
                "completed report has no pushed commit"
            );
            ensure!(
                !self.tests.is_empty(),
                "completed report has no testing evidence"
            );
            if mode == Mode::Pr {
                ensure!(
                    self.pr_url
                        .as_ref()
                        .is_some_and(|s| s.starts_with("https://")),
                    "completed PR report has no PR URL"
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Completion {
    pub id: String,
    pub story: StoryRef,
    /// Frozen at dispatch; controls Review versus Done even if requirements change.
    pub mode: Mode,
    pub report: Report,
}

/// Only the current process's work is sent to Luna; there is no board mirror.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Snapshot {
    pub active: Vec<StoryRef>,
    pub pending: Vec<Completion>,
}
impl Snapshot {
    pub fn busy(&self, project: &str) -> bool {
        self.active.iter().any(|s| s.project_id == project)
            || self.pending.iter().any(|c| c.story.project_id == project)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coordination {
    pub assignments: Vec<Assignment>,
    pub cancel: Vec<StoryRef>,
    pub acknowledged: Vec<String>,
    pub notes: Vec<String>,
    /// Projects with an incomplete coordination decision or failed remote operation.
    pub retry_projects: Vec<String>,
    /// Display observations only; never used to select or authorize work.
    #[serde(default)]
    pub observations: Vec<crate::status::BoardRead>,
}
impl Coordination {
    pub fn validate(&self, projects: &[String], snapshot: &Snapshot) -> Result<()> {
        let mut retrying = HashSet::new();
        for id in &self.retry_projects {
            ensure!(
                projects.contains(id) && retrying.insert(id),
                "unknown/duplicate project retry"
            );
        }
        let mut assigned = HashSet::new();
        for a in &self.assignments {
            ensure!(
                projects.contains(&a.project_id),
                "unknown/disabled project in assignment"
            );
            ensure!(
                !snapshot.busy(&a.project_id) && assigned.insert(&a.project_id),
                "project already has work"
            );
            ensure!(
                identifier(&a.story_id)
                    && !a.title.trim().is_empty()
                    && !a.context.trim().is_empty()
                    && a.context.len() <= 500_000,
                "invalid or incomplete assignment"
            );
        }
        for s in &self.cancel {
            ensure!(
                snapshot.active.contains(s),
                "cancellation does not refer to active work"
            );
        }
        let mut acked = HashSet::new();
        for id in &self.acknowledged {
            ensure!(
                snapshot.pending.iter().any(|c| &c.id == id) && acked.insert(id),
                "unknown/duplicate completion acknowledgement"
            );
        }
        Ok(())
    }
}

fn object(properties: Value) -> Value {
    let required: Vec<_> = properties.as_object().unwrap().keys().cloned().collect();
    json!({"type":"object", "properties":properties, "required":required, "additionalProperties":false})
}
fn strings() -> Value {
    json!({"type":"array", "items":{"type":"string"}})
}
pub fn coordination_schema() -> Value {
    object(json!({
        "assignments":{"type":"array","items":object(json!({
            "project_id":{"type":"string"}, "story_id":{"type":"string"},
            "title":{"type":"string"}, "story_url":{"type":["string","null"]}, "context":{"type":"string"}, "recovery":{"type":"boolean"},
            "pr":{"anyOf":[object(json!({"branch":{"type":["string","null"]}})), {"type":"null"}]}
        }))},
        "cancel":{"type":"array","items":object(json!({"project_id":{"type":"string"},"story_id":{"type":"string"}}))},
        "acknowledged":strings(), "notes":strings(), "retry_projects":strings(),
        "observations":{"type":"array","items":object(json!({
            "project_id":{"type":"string"},
            "counts":{"anyOf":[object(json!({
                "ready":{"type":"integer","minimum":0,"maximum":1000000000},
                "in_progress":{"type":"integer","minimum":0,"maximum":1000000000},
                "blocked":{"type":"integer","minimum":0,"maximum":1000000000},
                "review":{"type":"integer","minimum":0,"maximum":1000000000},
                "backlog":{"type":"integer","minimum":0,"maximum":1000000000},
                "done":{"type":"integer","minimum":0,"maximum":1000000000}
            })), {"type":"null"}]},
            "read":{"type":"string","enum":["complete","partial","failed","not_read"]},
            "reason":{"type":"string","enum":["no_eligible_work","dependencies","occupied","read_failed","ambiguous_board","incomplete_read","claim_failed","requirements_blocked","update_failed","work_selected"]}
        }))}
    }))
}
pub fn report_schema() -> Value {
    object(json!({
        "outcome":{"type":"string","enum":["completed","blocked"]},
        "summary":{"type":"string"}, "tests":strings(),
        "commit":{"type":["string","null"]}, "pr_url":{"type":["string","null"]}
    }))
}
