//! One sync pass: snapshot both sides, plan, and carry out the plan.

use std::path::{Path, PathBuf};

use aerie_core::FileEntry;
use aerie_drive::Result;
use aerie_drive::api::DriveApi;
use aerie_store::Store;

use crate::execute::{Done, Executor, decode_row};
use crate::{plan, scan, tree};

/// What to do when a pass would delete an unusually large number of items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deletions {
    /// Skip them and report the count, for the user to decide.
    Hold,
    /// The user confirmed: carry them out.
    Allow,
    /// The user declined: forget the deleted side's baseline, so the surviving copies
    /// are restored to the other side on this pass and the next.
    Restore,
}

/// Where a pass is: the synced folder the current step works in, and how far along.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub root: Option<aerie_core::DriveFileId>,
    pub done: usize,
    pub total: usize,
}

#[derive(Debug, Default)]
pub struct Report {
    /// Steps planned this pass, including any that failed or were held.
    pub planned: usize,
    pub applied: usize,
    /// Paths of conflict copies made so both versions survive.
    pub conflicts: Vec<PathBuf>,
    /// Deletions held back for the user's confirmation, by count.
    pub held_deletions: Option<usize>,
    /// Items left alone, with the reason.
    pub skipped: Vec<(PathBuf, &'static str)>,
    /// Steps that failed and will be retried next pass.
    pub failed: Vec<(Option<PathBuf>, String)>,
}

impl Report {
    pub fn in_sync(&self) -> bool {
        self.failed.is_empty() && self.held_deletions.is_none() && self.applied == self.planned
    }
}

/// Runs one pass. Errors that affect the whole pass (no root folder, unreadable folder)
/// return early before anything changes.
pub async fn run(
    drive: &DriveApi,
    store: &mut Store,
    account: &str,
    root: &Path,
    quarantine: &Path,
    deletions: Deletions,
    mut progress: impl FnMut(Progress),
) -> Result<Report> {
    let roots: Vec<FileEntry> = store
        .sync_roots(account)
        .map_err(store_error)?
        .iter()
        .filter_map(|id| store.file(account, id).ok().flatten())
        .collect();
    let remote = tree::remote_items(&roots, &|folder| {
        store.children(account, folder).unwrap_or_default()
    });
    let local = scan::scan(root)?;
    let baseline: Vec<_> = store
        .baseline(account)
        .map_err(store_error)?
        .iter()
        .map(decode_row)
        .collect();

    let mut ops = plan::reconcile(&baseline, &remote, &local);
    let mut report = Report::default();
    if let Some(count) = plan::deletions_needing_confirmation(&ops, baseline.len()) {
        match deletions {
            Deletions::Allow => {}
            Deletions::Hold => {
                report.held_deletions = Some(count);
                ops.retain(|op| !op.is_deletion());
            }
            Deletions::Restore => {
                for op in ops.iter().filter(|op| op.is_deletion()) {
                    if let crate::Op::TrashLocal { path, .. }
                    | crate::Op::TrashRemote { path, .. } = op
                    {
                        store
                            .forget_baseline_under(account, path)
                            .map_err(store_error)?;
                    }
                }
                ops.retain(|op| !op.is_deletion());
            }
        }
    }
    report.planned = ops.len();

    let mut executor = Executor::new(drive, store, account, root, quarantine, &remote, &baseline);
    // Which synced folder each step works in, by its top-level local name.
    let root_ids: Vec<_> = roots.iter().map(|root| root.id.clone()).collect();
    let root_by_name: std::collections::HashMap<PathBuf, aerie_core::DriveFileId> = remote
        .iter()
        .filter(|item| root_ids.contains(&item.id))
        .map(|item| (item.path.clone(), item.id.clone()))
        .collect();
    let total = ops.len();
    for (done, op) in ops.iter().enumerate() {
        let working_in = op
            .path()
            .and_then(|path| path.components().next())
            .and_then(|first| root_by_name.get(&PathBuf::from(first.as_os_str())))
            .cloned();
        progress(Progress {
            root: working_in,
            done,
            total,
        });
        match executor.run(op).await {
            Ok(Done::Applied) => report.applied += 1,
            Ok(Done::ConflictCopy(copy)) => {
                report.applied += 1;
                report.conflicts.push(copy);
            }
            Ok(Done::Skipped(reason)) => {
                report.applied += 1;
                report
                    .skipped
                    .push((op.path().cloned().unwrap_or_default(), reason));
            }
            Err(err) => {
                // A missing root stops the pass; anything else is retried next time.
                if !root.is_dir() {
                    return Err(err);
                }
                tracing::warn!(%err, "sync step failed");
                report.failed.push((op.path().cloned(), err.to_string()));
            }
        }
    }
    Ok(report)
}

fn store_error(err: aerie_store::Error) -> aerie_drive::Error {
    aerie_drive::Error::Io(std::io::Error::other(err.to_string()))
}
