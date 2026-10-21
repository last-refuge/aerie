//! Running sync passes: when to start one, watching the local folder, and reporting.

use std::path::PathBuf;
use std::time::Duration;

use aerie_core::DriveFileId;
use aerie_store::{Store, SyncSettings};
use aerie_sync::cycle::{self, Deletions, Report};
use notify::Watcher;

use crate::{Command, Completion, Event, Worker};

/// How long the local folder must be quiet before a pass syncs its changes.
pub const SETTLE: Duration = Duration::from_secs(2);

/// What the sync status shows.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SyncState {
    Off,
    Paused,
    Syncing,
    InSync,
    /// Something needs the user, with an explanation.
    Attention(String),
    /// A pass held this many deletions for confirmation.
    HeldDeletions(usize),
}

impl Worker {
    pub(crate) fn sync_settings(&self) -> Option<(String, SyncSettings)> {
        let account = self.session.as_ref()?.account.clone();
        let settings = self
            .store
            .as_ref()?
            .sync_settings(&account)
            .ok()
            .flatten()?;
        Some((account, settings))
    }

    /// Drops chosen folders that sit inside another chosen folder: they already sync as
    /// part of it, and syncing them twice would copy their files twice.
    fn outermost(&mut self, account: &str, roots: Vec<DriveFileId>) -> Vec<DriveFileId> {
        let chosen: std::collections::HashSet<_> = roots.iter().cloned().collect();
        let store = self.store();
        roots
            .into_iter()
            .filter(|root| {
                let mut current = store
                    .file(account, root)
                    .ok()
                    .flatten()
                    .and_then(|e| e.parent);
                // Bounded, so a cycle in bad metadata can't hang the engine.
                for _ in 0..256 {
                    match current {
                        Some(parent) if chosen.contains(&parent) => return false,
                        Some(parent) => {
                            current = store
                                .file(account, &parent)
                                .ok()
                                .flatten()
                                .and_then(|e| e.parent);
                        }
                        None => return true,
                    }
                }
                true
            })
            .collect()
    }

    /// The local folder name each chosen Drive folder syncs to.
    fn root_paths(&mut self, account: &str, roots: &[DriveFileId]) -> Vec<PathBuf> {
        roots
            .iter()
            .filter_map(|id| self.store().sync_root_entry(account, id).ok().flatten())
            .map(|entry| PathBuf::from(aerie_sync::tree::local_name(&entry.name)))
            .collect()
    }

    pub(super) async fn start_sync(&mut self, local_root: PathBuf, roots: Vec<DriveFileId>) {
        if !self.can_write() {
            return self.emit(Event::NeedsWriteAccess);
        }
        let Some(account) = self.session.as_ref().map(|s| s.account.clone()) else {
            return;
        };
        if !local_root.is_dir() {
            return self.emit(Event::Sync(SyncState::Attention(
                "The folder you chose isn't available.".into(),
            )));
        }
        let roots = self.outermost(&account, roots);
        let paths = self.root_paths(&account, &roots);
        let settings = SyncSettings {
            local_root,
            paused: false,
        };
        let store = self.store();
        if let Err(err) = store
            .save_sync_settings(&account, &settings)
            .and_then(|()| store.set_sync_roots(&account, &roots, &paths))
        {
            return self.emit(Event::Problem(err.to_string()));
        }
        tracing::info!(folders = roots.len(), "sync started");
        self.resume_sync_watch();
        self.sync_due = true;
        self.emit(Event::Sync(SyncState::Syncing));
    }

    pub(super) fn set_sync_roots(&mut self, roots: Vec<DriveFileId>) {
        let Some((account, _)) = self.sync_settings() else {
            return;
        };
        let roots = self.outermost(&account, roots);
        let paths = self.root_paths(&account, &roots);
        if let Err(err) = self.store().set_sync_roots(&account, &roots, &paths) {
            return self.emit(Event::Problem(err.to_string()));
        }
        self.sync_due = true;
    }

    pub(super) fn set_sync_paused(&mut self, paused: bool) {
        let Some((account, mut settings)) = self.sync_settings() else {
            return;
        };
        settings.paused = paused;
        let _ = self.store().save_sync_settings(&account, &settings);
        if paused {
            // A running pass finishes its current step's network call, then stops.
            if let Some(task) = self.sync.take() {
                task.abort();
            }
            self.watcher = None;
            self.emit(Event::Sync(SyncState::Paused));
        } else {
            self.resume_sync_watch();
            self.sync_due = true;
            self.emit(Event::Sync(SyncState::Syncing));
        }
    }

    pub(super) fn stop_sync(&mut self) {
        if let Some(task) = self.sync.take() {
            task.abort();
        }
        self.watcher = None;
        if let Some(account) = self.session.as_ref().map(|s| s.account.clone()) {
            let _ = self.store().stop_sync(&account);
        }
        tracing::info!("sync stopped");
        self.emit(Event::Sync(SyncState::Off));
    }

    /// Watches the local folder for changes, when sync is on and not paused.
    pub(super) fn resume_sync_watch(&mut self) {
        let Some((_, settings)) = self.sync_settings() else {
            self.emit(Event::Sync(SyncState::Off));
            return;
        };
        if settings.paused {
            self.emit(Event::Sync(SyncState::Paused));
            return;
        }
        let own = self.own.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            // Reads, including Aerie's own scans, aren't changes.
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            // Aerie's own partial downloads aren't changes worth a pass.
            let relevant = event.paths.iter().any(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_none_or(|name| !aerie_sync::scan::is_ignored(name))
            });
            if relevant {
                let _ = own.try_send(Command::LocalChanged);
            }
        });
        match watcher {
            Ok(mut watcher) => {
                if let Err(err) =
                    watcher.watch(&settings.local_root, notify::RecursiveMode::Recursive)
                {
                    // Without a watcher, periodic passes still pick up changes.
                    tracing::warn!(%err, "couldn't watch the sync folder");
                }
                self.watcher = Some(watcher);
            }
            Err(err) => tracing::warn!(%err, "couldn't start the folder watcher"),
        }
    }

    /// Starts a pass when one is due and nothing prevents it.
    pub(super) async fn pump_sync(&mut self) {
        if self
            .local_changed_at
            .is_some_and(|at| at.elapsed() >= SETTLE)
        {
            self.local_changed_at = None;
            self.sync_due = true;
        }
        if self.sync.is_some() || !self.sync_due || self.offline || !self.can_write() {
            return;
        }
        let Some((account, settings)) = self.sync_settings() else {
            return;
        };
        if settings.paused || !self.store().inventory_complete(&account).unwrap_or(false) {
            return;
        }
        if let Err(err) = self.ensure_token().await {
            tracing::info!(%err, "sync waits for a working sign-in");
            return;
        }
        self.sync_due = false;
        let drive = self.session.as_ref().expect("checked").drive.clone();
        let deletions = std::mem::replace(&mut self.deletions, Deletions::Hold);
        let done = self.completions.0.clone();
        let events = self.events.clone();
        self.emit(Event::Sync(SyncState::Syncing));
        self.sync = Some(tokio::spawn(async move {
            let result = async {
                let mut store = Store::open(&Store::default_path()).map_err(io)?;
                let quarantine = quarantine_dir();
                // Report each change of folder, not every step.
                let mut last_root = None;
                cycle::run(
                    &drive,
                    &mut store,
                    &account,
                    &settings.local_root,
                    &quarantine,
                    deletions,
                    |progress| {
                        if progress.root != last_root || progress.done % 25 == 0 {
                            last_root.clone_from(&progress.root);
                            let _ = events.try_send(Event::SyncProgress {
                                root: progress.root,
                                done: progress.done,
                                total: progress.total,
                            });
                        }
                    },
                )
                .await
            }
            .await;
            let _ = done.send(Completion::Sync(result)).await;
        }));
    }

    pub(super) async fn finish_sync(&mut self, result: aerie_drive::Result<Report>) {
        match result {
            Ok(report) => {
                tracing::info!(
                    planned = report.planned,
                    applied = report.applied,
                    failed = report.failed.len(),
                    "sync pass"
                );
                for copy in &report.conflicts {
                    self.emit(Event::ConflictKept(copy.clone()));
                }
                if report.applied > 0 {
                    // Confirm the result with another pass once Drive reports it back.
                    self.emit(Event::CatalogChanged);
                    self.poll_delay = crate::FASTEST_POLL;
                }
                let state = if let Some(count) = report.held_deletions {
                    SyncState::HeldDeletions(count)
                } else if !report.failed.is_empty() {
                    SyncState::Attention(format!(
                        "{} item{} couldn’t sync yet. Aerie will keep trying.",
                        report.failed.len(),
                        if report.failed.len() == 1 { "" } else { "s" }
                    ))
                } else {
                    SyncState::InSync
                };
                self.emit(Event::Sync(state));
            }
            Err(err) if err.is_offline() => {
                self.offline = true;
                self.emit(Event::Offline);
            }
            Err(err) => {
                tracing::warn!(%err, "sync pass stopped");
                let missing = self
                    .sync_settings()
                    .is_some_and(|(_, settings)| !settings.local_root.is_dir());
                let message = if missing {
                    "The sync folder is missing. Reconnect the drive it’s on, or stop syncing."
                        .to_owned()
                } else {
                    format!("Sync stopped: {err}")
                };
                self.emit(Event::Sync(SyncState::Attention(message)));
            }
        }
    }
}

/// Where items go when the system trash can't take them: `$XDG_DATA_HOME/aerie/quarantine`.
fn quarantine_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_default()
        .join("aerie")
        .join("quarantine")
}

fn io(err: aerie_store::Error) -> aerie_drive::Error {
    aerie_drive::Error::Io(std::io::Error::other(err.to_string()))
}
