use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Backlog,
    Ready,
    InProgress,
    Review,
    Done,
    Blocked,
    Cancelled,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Implement,
    Blocked,
    Ignore,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Story {
    pub story_id: String,
    pub title: String,
    pub description: String,
    pub status: Status,
    pub priority: i64,
    pub dependencies: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub decision: Decision,
    pub reason: String,
}
impl Story {
    /// Status/projection fields are deliberately excluded. Normalization changes fail closed.
    pub fn revision(&self) -> String {
        let mut dependencies = self.dependencies.clone();
        dependencies.sort();
        dependencies.dedup();
        let data = serde_json::json!([
            self.title,
            self.description,
            dependencies,
            self.acceptance_criteria
        ]);
        format!("{:x}", Sha256::digest(data.to_string()))
    }
    pub fn authorized(&self) -> bool {
        matches!(
            self.status,
            Status::Ready | Status::InProgress | Status::Review
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scan {
    pub project_id: String,
    pub workspace_id: String,
    pub board_id: String,
    pub view_id: String,
    pub matches: u32,
    pub complete: bool,
    pub fields_readable: bool,
    pub stories: Vec<Story>,
}
impl Scan {
    pub fn validate(&self, project: &str, evidence: &str) -> Result<()> {
        ensure!(
            self.project_id == project,
            "coordinator returned wrong project"
        );
        ensure!(
            self.matches == 1,
            "board must resolve uniquely; found {} matches",
            self.matches
        );
        ensure!(
            self.complete && self.fields_readable,
            "incomplete board read or unreadable fields"
        );
        ensure!(
            self.board_id != self.view_id,
            "board_id must be the database ID, distinct from view_id"
        );
        for id in [&self.workspace_id, &self.board_id, &self.view_id] {
            ensure!(
                uuid::Uuid::parse_str(id).is_ok() && evidence.contains(id),
                "ungrounded board identifier"
            );
        }
        let mut ids = HashSet::new();
        for s in &self.stories {
            ensure!(
                uuid::Uuid::parse_str(&s.story_id).is_ok()
                    && evidence.contains(&s.story_id)
                    && ids.insert(&s.story_id),
                "invalid/duplicate/ungrounded story identifier"
            );
            ensure!(
                s.title.len() <= 1000
                    && s.description.len() <= 200_000
                    && s.acceptance_criteria.len() <= 200,
                "story exceeds limits"
            );
            ensure!(
                s.dependencies
                    .iter()
                    .all(|x| uuid::Uuid::parse_str(x).is_ok() && evidence.contains(x)),
                "ungrounded dependency"
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentReport {
    pub summary: String,
    pub uncertainties: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub head_sha: String,
    pub approved: bool,
    pub blocking: Vec<String>,
    pub nonblocking: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    pub project_id: String,
    pub story_id: String,
    pub run_id: String,
    pub status: Status,
    pub message: String,
    pub pr_url: Option<String>,
    pub attempt: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionResult {
    pub story_id: String,
    pub applied: bool,
}
pub fn schema(name: &str) -> serde_json::Value {
    serde_json::from_str(match name {
        "scan" => include_str!("../schemas/scan.json"),
        "report" => include_str!("../schemas/report.json"),
        "review" => include_str!("../schemas/review.json"),
        "projection" => include_str!("../schemas/projection.json"),
        _ => unreachable!(),
    })
    .expect("embedded schema")
}
