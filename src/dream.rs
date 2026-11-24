use crate::{
    config::{Config, Project},
    model::{Coordination, Outcome},
    status::ReadState,
};
use anyhow::{Context, Result, ensure};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub(crate) const BRANCH: &str = "dreams";

pub(crate) fn parse_time(value: &str) -> Result<NaiveTime> {
    let bytes = value.as_bytes();
    ensure!(
        bytes.len() == 5
            && bytes[2] == b':'
            && bytes
                .iter()
                .enumerate()
                .all(|(i, b)| i == 2 || b.is_ascii_digit()),
        "orchestrator.dream_time must be a local time in HH:MM format (00:00–23:59)"
    );
    NaiveTime::parse_from_str(value, "%H:%M")
        .context("orchestrator.dream_time must be between 00:00 and 23:59")
}

pub(crate) struct Dream {
    pub name: String,
    pub instructions: String,
}

struct Manifest {
    files: Vec<Dream>,
    fingerprints: BTreeMap<String, [u8; 32]>,
}

impl Manifest {
    fn read(repo: &Path) -> Result<Self> {
        let dir = repo.join("dreams");
        let mut files = Vec::new();
        match fs::symlink_metadata(&dir) {
            Ok(metadata) => ensure!(metadata.is_dir(), "{} must be a directory", dir.display()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    files,
                    fingerprints: BTreeMap::new(),
                });
            }
            Err(e) => return Err(e.into()),
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_file() || (file_type.is_symlink() && path.is_file()) {
                let name = entry.file_name().into_string().map_err(|_| {
                    anyhow::anyhow!("dream filename in {} must be UTF-8", dir.display())
                })?;
                let instructions = fs::read_to_string(&path)
                    .with_context(|| format!("read {}", path.display()))?;
                files.push(Dream { name, instructions });
            }
        }
        files.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        let fingerprints = files
            .iter()
            .map(|file| {
                (
                    file.name.clone(),
                    Sha256::digest(file.instructions.as_bytes()).into(),
                )
            })
            .collect();
        Ok(Self {
            files,
            fingerprints,
        })
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Checkpoint {
    last_phase: Option<NaiveDate>,
    manifest: BTreeMap<String, [u8; 32]>,
    story_work: bool,
}

struct ProjectDreams {
    path: PathBuf,
    checkpoint: Checkpoint,
    candidate: Option<(NaiveDate, Manifest)>,
}

impl ProjectDreams {
    fn save(&mut self, checkpoint: Checkpoint) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())?;
        serde_json::to_writer(&mut file, &checkpoint)?;
        file.flush()?;
        file.as_file().sync_all()?;
        file.persist(&self.path)?;
        self.checkpoint = checkpoint;
        Ok(())
    }
}

pub(crate) struct Dreams {
    time: NaiveTime,
    projects: BTreeMap<String, ProjectDreams>,
}

impl Dreams {
    pub fn new(config: &Config) -> Result<Self> {
        let time = parse_time(&config.orchestrator.dream_time)?;
        let root = config.orchestrator.runtime_dir.join("dreams");
        fs::create_dir_all(&root)?;
        let mut projects = BTreeMap::new();
        for project in config.projects.iter().filter(|p| p.enabled) {
            let path = root.join(format!("{}.json", project.id));
            let checkpoint = match fs::read(&path) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .with_context(|| format!("read dream checkpoint {}", path.display()))?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => Checkpoint::default(),
                Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
            };
            projects.insert(
                project.id.clone(),
                ProjectDreams {
                    path,
                    checkpoint,
                    candidate: None,
                },
            );
        }
        Ok(Self { time, projects })
    }

    pub fn day(&self, local: NaiveDateTime) -> Option<NaiveDate> {
        (local.time() >= self.time).then_some(local.date())
    }

    pub fn record_story_work(&mut self, id: &str) -> Result<()> {
        let state = self.projects.get_mut(id).unwrap();
        if !state.checkpoint.story_work {
            state.save(Checkpoint {
                story_work: true,
                ..state.checkpoint.clone()
            })?;
        }
        Ok(())
    }

    /// Wake coordination once per candidate/day. A busy board can wake it again
    /// through the usual polling path when its state changes.
    pub fn refresh(&mut self, project: &Project, day: Option<NaiveDate>) -> Result<bool> {
        let state = self.projects.get_mut(&project.id).unwrap();
        let Some(day) =
            day.filter(|day| state.checkpoint.last_phase.is_none_or(|last| last < *day))
        else {
            state.candidate = None;
            return Ok(false);
        };
        let manifest = match Manifest::read(&project.repo) {
            Ok(manifest) => manifest,
            Err(e) => {
                state.candidate = None;
                return Err(e).context("discover Markdown files in dreams/");
            }
        };
        let files_changed = manifest
            .fingerprints
            .iter()
            .any(|(name, digest)| state.checkpoint.manifest.get(name) != Some(digest));
        if manifest.files.is_empty() || (!state.checkpoint.story_work && !files_changed) {
            state.candidate = None;
            return Ok(false);
        }
        let wake = state
            .candidate
            .as_ref()
            .is_none_or(|(date, _)| *date != day);
        state.candidate = Some((day, manifest));
        Ok(wake)
    }

    pub fn candidate(&self, id: &str) -> Option<NaiveDate> {
        self.projects[id].candidate.as_ref().map(|(date, _)| *date)
    }

    pub fn start(&mut self, id: &str) -> Result<Vec<Dream>> {
        let state = self.projects.get_mut(id).unwrap();
        let (day, manifest) = state
            .candidate
            .take()
            .context("dream phase has no candidate")?;
        // Checkpoint before dispatch so a restart cannot replay a release or another side effect.
        state.save(Checkpoint {
            last_phase: Some(day),
            manifest: manifest.fingerprints,
            story_work: false,
        })?;
        Ok(manifest.files)
    }
}

/// A complete read from this pass is required; missing, duplicate and partial
/// results cannot establish an empty queue.
pub(crate) fn empty_queue(update: &Coordination, id: &str) -> Option<bool> {
    let mut matches = update.observations.iter().filter(|o| o.project_id == id);
    let observation = matches.next()?;
    if observation.read != ReadState::Complete || matches.next().is_some() {
        return None;
    }
    let counts = observation.counts?;
    Some(counts.ready == 0 && counts.in_progress == 0)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DreamReport {
    pub outcome: Outcome,
    pub summary: String,
    pub tests: Vec<String>,
    pub commit: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_time_uses_a_daily_window_and_validates_the_config_value() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            orchestrator: crate::config::Orchestrator {
                runtime_dir: dir.path().into(),
                ..Default::default()
            },
            status: Default::default(),
            agents: Default::default(),
            projects: vec![],
        };
        let dreams = Dreams::new(&config).unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 3, 29).unwrap();
        assert_eq!(dreams.day(day.and_hms_opt(2, 59, 59).unwrap()), None);
        assert_eq!(dreams.day(day.and_hms_opt(3, 0, 0).unwrap()), Some(day));
        assert_eq!(dreams.day(day.and_hms_opt(23, 59, 0).unwrap()), Some(day));
        assert_eq!(
            dreams.day(day.succ_opt().unwrap().and_hms_opt(0, 0, 0).unwrap()),
            None
        );
        for invalid in ["3:00", "24:00", "03:60", "03:00:00", "03:00Z", ""] {
            assert!(parse_time(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn checkpoint_survives_restart_and_only_new_input_or_story_work_opens_another_phase() {
        let dir = tempfile::tempdir().unwrap();
        let mut config: Config = toml::from_str(
            r#"
            [[projects]]
            id = "project"
            repo = "."
            appflowy_page = "Stories"
            mode = "master"
            base_branch = "main"
            "#,
        )
        .unwrap();
        config.orchestrator.runtime_dir = dir.path().join("runtime");
        config.projects[0].repo = dir.path().join("repo");
        let project = &config.projects[0];
        let files = project.repo.join("dreams");
        let examples = project.repo.join("examples");
        fs::create_dir_all(&files).unwrap();
        fs::create_dir_all(&examples).unwrap();
        let example = examples.join("first.md");
        fs::write(&example, "first").unwrap();
        fs::write(files.join("02.md"), "second").unwrap();
        std::os::unix::fs::symlink("../examples/first.md", files.join("01.md")).unwrap();
        std::os::unix::fs::symlink("../examples/missing.md", files.join("missing.md")).unwrap();
        std::os::unix::fs::symlink("../examples", files.join("directory.md")).unwrap();
        fs::write(files.join("ignored.txt"), "not Markdown").unwrap();

        let day = NaiveDate::from_ymd_opt(2026, 11, 23).unwrap();
        let tomorrow = day.succ_opt().unwrap();
        let later = tomorrow.succ_opt().unwrap();
        let mut dreams = Dreams::new(&config).unwrap();
        assert!(!dreams.refresh(project, None).unwrap());
        assert!(dreams.refresh(project, Some(day)).unwrap());
        let tasks = dreams.start(&project.id).unwrap();
        assert_eq!(
            tasks
                .iter()
                .map(|task| task.name.as_str())
                .collect::<Vec<_>>(),
            ["01.md", "02.md"]
        );
        assert_eq!(tasks[0].instructions, "first");

        let mut dreams = Dreams::new(&config).unwrap();
        assert!(!dreams.refresh(project, Some(day)).unwrap());
        assert!(!dreams.refresh(project, Some(tomorrow)).unwrap());
        dreams.record_story_work(&project.id).unwrap();

        let mut dreams = Dreams::new(&config).unwrap();
        assert!(dreams.refresh(project, Some(tomorrow)).unwrap());
        dreams.start(&project.id).unwrap();
        assert!(!dreams.refresh(project, Some(later)).unwrap());
        fs::remove_file(files.join("02.md")).unwrap();
        assert!(!dreams.refresh(project, Some(later)).unwrap());
        fs::write(example, "edited first").unwrap();
        assert!(!dreams.refresh(project, Some(tomorrow)).unwrap());
        assert!(dreams.refresh(project, Some(later)).unwrap());
        assert_eq!(
            dreams.start(&project.id).unwrap()[0].instructions,
            "edited first"
        );
    }
}
