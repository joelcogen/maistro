//! Volatile generations. A pass can acknowledge only what it saw at its start.
use crate::poll::Fingerprint;
use std::{collections::BTreeMap, time::Duration};
use tokio::time::Instant;

pub struct Project {
    baseline: Option<Fingerprint>,
    generation: u64,
    acknowledged: u64,
    internal: bool,
    retry_at: Instant,
    periodic: Option<(Duration, Instant)>,
}
impl Project {
    pub fn dirty(&self) -> bool {
        self.generation != self.acknowledged
    }
}
pub struct Schedule {
    pub projects: BTreeMap<String, Project>,
}
impl Schedule {
    pub fn new(ids: impl Iterator<Item = String>) -> Self {
        Self {
            projects: ids
                .map(|id| {
                    (
                        id,
                        Project {
                            baseline: None,
                            generation: 1,
                            acknowledged: 0,
                            internal: false,
                            retry_at: Instant::now(),
                            periodic: None,
                        },
                    )
                })
                .collect(),
        }
    }
    /// OAuth-backed sources are read by Codex at intervals, without a Rust poller.
    pub fn periodically(&mut self, id: &str, interval: Duration) {
        self.projects.get_mut(id).unwrap().periodic = Some((interval, Instant::now()));
    }
    pub fn observe(&mut self, id: &str, fingerprint: Fingerprint) -> bool {
        let p = self.projects.get_mut(id).unwrap();
        if p.baseline == Some(fingerprint) {
            return false;
        }
        p.baseline = Some(fingerprint);
        p.generation += 1;
        true
    }
    pub fn wake(&mut self, id: &str) {
        let p = self.projects.get_mut(id).unwrap();
        p.generation += 1;
        p.internal = true;
    }
    pub fn ready(&self) -> BTreeMap<String, u64> {
        self.projects
            .iter()
            .filter(|(_, p)| {
                ((p.dirty() && (p.baseline.is_some() || p.internal))
                    || p.periodic.is_some_and(|(_, due)| due <= Instant::now()))
                    && p.retry_at <= Instant::now()
            })
            .map(|(id, p)| (id.clone(), p.generation))
            .collect()
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.projects
            .values()
            .filter_map(|p| {
                if p.dirty() && (p.baseline.is_some() || p.internal) {
                    Some(p.retry_at)
                } else {
                    p.periodic.map(|(_, due)| due.max(p.retry_at))
                }
            })
            .min()
    }
    pub fn acknowledge(&mut self, id: &str, generation: u64) {
        let p = self.projects.get_mut(id).unwrap();
        p.acknowledged = generation;
        if !p.dirty() {
            p.internal = false;
        }
        p.retry_at = Instant::now();
        if let Some((interval, due)) = &mut p.periodic {
            *due = Instant::now() + *interval;
        }
    }
    pub fn retry(&mut self, id: &str, interval: Duration) {
        self.projects.get_mut(id).unwrap().retry_at = Instant::now() + interval;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn schedule() -> Schedule {
        Schedule::new(["a".into(), "b".into()].into_iter())
    }
    #[test]
    fn unknown_requires_observation_and_unchanged_polls_do_not_wake() {
        let mut s = schedule();
        assert!(s.projects["a"].dirty());
        assert!(s.ready().is_empty());
        s.observe("a", [1; 32]);
        let pass = s.ready();
        assert_eq!(pass.len(), 1);
        s.acknowledge("a", pass["a"]);
        for _ in 0..5 {
            assert!(!s.observe("a", [1; 32]));
            assert!(s.ready().is_empty());
        }
    }
    #[test]
    fn concurrent_board_and_internal_changes_survive_acknowledgement() {
        let mut s = schedule();
        s.observe("a", [1; 32]);
        s.observe("b", [2; 32]);
        let pass = s.ready();
        s.observe("a", [3; 32]);
        s.wake("b"); // worker completion/failure/cancellation while Luna runs
        for (id, generation) in pass {
            s.acknowledge(&id, generation);
        }
        assert_eq!(s.ready().len(), 2);
        let pass = s.ready();
        for (id, generation) in pass {
            s.acknowledge(&id, generation);
        }
        assert!(s.ready().is_empty());
        s.wake("a"); // acknowledged result needs a follow-up dispatch
        assert_eq!(s.ready().len(), 1);
    }
    #[test]
    fn periodic_sources_need_no_fingerprint_and_respect_interval_and_failure_backoff() {
        let mut s = schedule();
        let interval = Duration::from_secs(30);
        s.periodically("a", interval);
        let pass = s.ready();
        assert_eq!(pass.len(), 1);
        s.acknowledge("a", pass["a"]);
        assert!(!s.projects["a"].dirty());
        assert!(s.ready().is_empty());
        assert!(s.deadline().unwrap() > Instant::now());
        // Worker completion bypasses the next periodic read, but not failure backoff.
        s.wake("a");
        let pass = s.ready();
        assert_eq!(pass.len(), 1);
        s.acknowledge("a", pass["a"]);
        s.projects
            .get_mut("a")
            .unwrap()
            .periodic
            .as_mut()
            .unwrap()
            .1 = Instant::now();
        assert_eq!(s.ready().len(), 1);
        s.retry("a", interval);
        s.wake("a");
        assert!(s.ready().is_empty());
        s.projects.get_mut("a").unwrap().retry_at = Instant::now();
        assert_eq!(s.ready().len(), 1);
        assert!(s.projects["a"].baseline.is_none());
        assert!(!s.ready().contains_key("b"));
    }
    #[test]
    fn failure_backoff_cannot_be_bypassed_by_board_or_worker_wakes() {
        let mut s = schedule();
        s.observe("a", [1; 32]);
        s.retry("a", Duration::from_secs(30));
        s.observe("a", [2; 32]);
        s.wake("a");
        assert!(s.ready().is_empty());
        s.observe("b", [3; 32]);
        assert_eq!(s.ready().len(), 1);
        s.projects.get_mut("a").unwrap().retry_at = Instant::now();
        assert_eq!(s.ready().len(), 2);
    }
}
