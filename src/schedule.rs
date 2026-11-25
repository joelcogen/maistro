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
    paused: bool,
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
                            paused: false,
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
    pub fn pause(&mut self, id: &str) {
        self.projects.get_mut(id).unwrap().paused = true;
    }
    pub fn is_paused(&self, id: &str) -> bool {
        self.projects[id].paused
    }
    pub fn resume(&mut self, id: &str) {
        self.projects.get_mut(id).unwrap().paused = false;
        self.wake(id);
    }
    pub fn is_current(&self, id: &str, generation: u64) -> bool {
        self.projects[id].generation == generation
    }
    pub fn ready(&self) -> BTreeMap<String, u64> {
        self.projects
            .iter()
            .filter(|(_, p)| {
                !p.paused
                    && ((p.dirty() && (p.baseline.is_some() || p.internal))
                        || p.periodic.is_some_and(|(_, due)| due <= Instant::now()))
                    && p.retry_at <= Instant::now()
            })
            .map(|(id, p)| (id.clone(), p.generation))
            .collect()
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.projects
            .values()
            .filter(|p| !p.paused)
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
    fn worker_wake_during_coordination_survives_acknowledgement() {
        let mut s = schedule();
        s.observe("a", [1; 32]);
        let pass = s.ready();
        s.wake("a");
        s.acknowledge("a", pass["a"]);
        let follow_up = s.ready();
        assert!(follow_up.contains_key("a"));
        s.acknowledge("a", follow_up["a"]);
        assert!(s.ready().is_empty());
    }

    #[tokio::test]
    async fn periodic_sources_wait_between_passes_and_back_off_after_failure() {
        let mut s = schedule();
        let interval = Duration::from_secs(1);
        s.periodically("a", interval);
        let pass = s.ready();
        s.acknowledge("a", pass["a"]);
        assert!(s.ready().is_empty());

        // Worker completion can bring a periodic read forward.
        s.wake("a");
        let pass = s.ready();
        s.acknowledge("a", pass["a"]);
        assert!(s.ready().is_empty());
        let due = s.deadline().unwrap();
        assert!(due > Instant::now());
        tokio::time::sleep_until(due).await;
        assert!(s.ready().contains_key("a"));

        // A failed pass backs off even when a new board or worker event arrives.
        s.retry("a", interval);
        s.observe("a", [2; 32]);
        s.wake("a");
        s.observe("b", [3; 32]);
        let ready = s.ready();
        assert!(ready.contains_key("b"));
        assert!(!ready.contains_key("a"));
        s.acknowledge("b", ready["b"]);
        let retry = s.deadline().unwrap();
        assert!(retry > Instant::now());
        tokio::time::sleep_until(retry).await;
        assert!(s.ready().contains_key("a"));
    }
}
