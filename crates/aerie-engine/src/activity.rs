//! The sync activity log: every change a pass plans, and how each one went.

use std::time::{Duration, Instant};

use aerie_sync::cycle::{Outcome, Step};
use aerie_sync::model::Direction;
use serde::{Deserialize, Serialize};

/// The most items a log carries; a larger pass lists these and counts the rest.
const MOST_ITEMS: usize = 1000;
/// How often a running pass's log is sent.
const EVERY: Duration = Duration::from_millis(300);

/// One sync pass with changes in it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    /// When the pass started, in Unix seconds.
    pub started: i64,
    /// Set once the pass has ended.
    pub finished: bool,
    /// Changes in the pass, including any not listed.
    pub total: usize,
    pub items: Vec<ActivityItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityItem {
    /// "Downloading", "Uploading", …
    pub verb: String,
    pub way: Way,
    /// Relative to the sync folder.
    pub path: String,
    pub state: ItemState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Way {
    Down,
    Up,
    Trash,
    Both,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemState {
    Waiting,
    Running,
    Done,
    /// Both versions kept; the other copy's path.
    KeptBoth(String),
    Skipped(String),
    Failed(String),
}

impl Activity {
    pub fn count(&self, matches: impl Fn(&ItemState) -> bool) -> usize {
        self.items
            .iter()
            .filter(|item| matches(&item.state))
            .count()
    }
}

/// Builds the log from a pass's steps and says when it's worth sending.
pub struct Recorder {
    activity: Activity,
    /// Each planned step's place in `activity.items`, if listed.
    index: Vec<Option<usize>>,
    last_sent: Option<Instant>,
}

impl Recorder {
    pub fn new(started: i64) -> Self {
        Self {
            activity: Activity {
                started,
                ..Activity::default()
            },
            index: Vec::new(),
            last_sent: None,
        }
    }

    /// Records a step; returns the log when it should be sent now.
    pub fn record(&mut self, step: Step) -> Option<Activity> {
        let urgent = match step {
            Step::Planned(steps) => {
                for planned in steps {
                    let way = match planned.direction {
                        Direction::Down => Way::Down,
                        Direction::Up => Way::Up,
                        Direction::Trash => Way::Trash,
                        Direction::Both => Way::Both,
                        Direction::Bookkeeping => {
                            self.index.push(None);
                            continue;
                        }
                    };
                    self.activity.total += 1;
                    if self.activity.items.len() < MOST_ITEMS {
                        self.index.push(Some(self.activity.items.len()));
                        self.activity.items.push(ActivityItem {
                            verb: planned.verb.to_owned(),
                            way,
                            path: planned.path.to_string_lossy().into_owned(),
                            state: ItemState::Waiting,
                        });
                    } else {
                        self.index.push(None);
                    }
                }
                true
            }
            Step::Started(at) => {
                self.set(at, ItemState::Running);
                false
            }
            Step::Finished(at, outcome) => {
                let state = match outcome {
                    Outcome::Done => ItemState::Done,
                    Outcome::KeptBoth(copy) => {
                        ItemState::KeptBoth(copy.to_string_lossy().into_owned())
                    }
                    Outcome::Skipped(reason) => ItemState::Skipped(reason),
                    Outcome::Failed(message) => ItemState::Failed(message),
                };
                let failed = matches!(state, ItemState::Failed(_));
                self.set(at, state);
                failed
            }
        };
        // A pass with nothing to do leaves the last log in place.
        if self.activity.total == 0 {
            return None;
        }
        let due = self.last_sent.is_none_or(|at| at.elapsed() >= EVERY);
        (urgent || due).then(|| {
            self.last_sent = Some(Instant::now());
            self.activity.clone()
        })
    }

    /// The final log, once the pass ends; `None` when it had nothing to do.
    pub fn finish(mut self) -> Option<Activity> {
        if self.activity.total == 0 {
            return None;
        }
        self.activity.finished = true;
        // Steps a failed pass never reached stay waiting for the next one.
        Some(self.activity)
    }

    fn set(&mut self, at: usize, state: ItemState) {
        if let Some(item) = self
            .index
            .get(at)
            .copied()
            .flatten()
            .and_then(|i| self.activity.items.get_mut(i))
        {
            item.state = state;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aerie_sync::cycle::PlannedStep;
    use std::path::PathBuf;

    fn planned(verb: &'static str, direction: Direction, path: &str) -> PlannedStep {
        PlannedStep {
            verb,
            direction,
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn follows_a_pass() {
        let mut recorder = Recorder::new(100);
        let sent = recorder
            .record(Step::Planned(vec![
                planned("Tidying up", Direction::Bookkeeping, ""),
                planned("Downloading", Direction::Down, "My Drive/a.pdf"),
                planned("Uploading", Direction::Up, "My Drive/b.txt"),
            ]))
            .expect("the plan is sent at once");
        assert_eq!(sent.total, 2);
        assert_eq!(sent.items[0].state, ItemState::Waiting);

        recorder.record(Step::Started(1));
        recorder.record(Step::Finished(1, Outcome::Done));
        recorder.record(Step::Started(2));
        let sent = recorder
            .record(Step::Finished(2, Outcome::Failed("offline".into())))
            .expect("a failure is sent at once");
        assert_eq!(sent.items[0].state, ItemState::Done);
        assert_eq!(sent.items[1].state, ItemState::Failed("offline".into()));

        let last = recorder.finish().unwrap();
        assert!(last.finished);
    }

    #[test]
    fn an_empty_pass_sends_nothing() {
        let mut recorder = Recorder::new(100);
        assert!(recorder.record(Step::Planned(Vec::new())).is_none());
        assert!(recorder.finish().is_none());
    }
}
