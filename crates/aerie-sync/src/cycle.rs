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
    /// What the step about to run does, such as "Downloading".
    pub verb: &'static str,
    /// The name of the file or folder it works on, when it has one.
    pub item: Option<String>,
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
    let ids = store.sync_roots(account).map_err(store_error)?;
    let roots = root_locations(store, account, &ids);
    relocate_old_roots(store, account, root, &roots)?;
    let remote = tree::remote_items(&roots, &|folder| {
        store.children(account, folder).unwrap_or_default()
    });
    let mut local = scan::scan(root)?;
    // The folders above a synced folder ("My Drive", "Clients") only hold it; they
    // aren't items to sync themselves.
    local.retain(|item| {
        !roots
            .iter()
            .any(|(_, path)| *path != item.path && path.starts_with(&item.path))
    });
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
    // Which synced folder each step works in, by the deepest root its path is under.
    let mut root_paths: Vec<(PathBuf, aerie_core::DriveFileId)> = roots
        .iter()
        .map(|(entry, path)| (path.clone(), entry.id.clone()))
        .collect();
    root_paths.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    let total = ops.len();
    for (done, op) in ops.iter().enumerate() {
        let working_in = op.path().and_then(|path| {
            root_paths
                .iter()
                .find(|(root, _)| path.starts_with(root))
                .map(|(_, id)| id.clone())
        });
        progress(Progress {
            root: working_in,
            done,
            total,
            verb: op.verb(),
            item: op
                .path()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned()),
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

/// Each synced folder with where it goes on this computer: its whole Drive path (see
/// [`tree::root_path`]), numbered if two would land in the same place.
pub fn root_locations(
    store: &Store,
    account: &str,
    ids: &[aerie_core::DriveFileId],
) -> Vec<(FileEntry, PathBuf)> {
    let my_drive = store.root_id(account).ok().flatten();
    let lookup = |id: &aerie_core::DriveFileId| store.file(account, id).ok().flatten();
    let mut taken = std::collections::HashSet::new();
    ids.iter()
        .filter_map(|id| store.sync_root_entry(account, id).ok().flatten())
        .map(|entry| {
            let wanted = tree::root_path(&entry, my_drive.as_ref(), &lookup);
            let mut path = wanted.clone();
            let mut n = 1;
            while !taken.insert(path.to_string_lossy().to_lowercase()) {
                n += 1;
                let name = wanted.file_name().unwrap_or_default().to_string_lossy();
                path = wanted.with_file_name(tree::numbered(&name, n));
            }
            (entry, path)
        })
        .collect()
}

/// Earlier versions kept each synced folder at the top of the sync folder under its own
/// name. Moves such a folder to its full Drive path, with its baseline, so nothing is
/// downloaded again or mistaken for a deletion.
fn relocate_old_roots(
    store: &mut Store,
    account: &str,
    root: &Path,
    roots: &[(FileEntry, PathBuf)],
) -> Result<()> {
    let baseline = store.baseline(account).map_err(store_error)?;
    for (entry, path) in roots {
        let old = PathBuf::from(tree::local_name(&entry.name));
        let ours = baseline
            .iter()
            .any(|row| row.remote_id == entry.id && row.path == old);
        if old == *path || !ours || !root.join(&old).is_dir() || root.join(path).exists() {
            continue;
        }
        if let Some(parent) = root.join(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(root.join(&old), root.join(path))?;
        store
            .move_baseline(account, &old, path)
            .map_err(store_error)?;
        tracing::info!(to = %path.display(), "moved a synced folder to its Drive path");
    }
    Ok(())
}

fn store_error(err: aerie_store::Error) -> aerie_drive::Error {
    aerie_drive::Error::Io(std::io::Error::other(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aerie_core::{Capabilities, DriveFileId, FileKind};
    use aerie_store::{Account, BaselineRow};

    const ACCOUNT: &str = "perm-1";

    fn folder(id: &str, parent: &str, name: &str) -> FileEntry {
        FileEntry {
            id: DriveFileId::new(id),
            parent: Some(DriveFileId::new(parent)),
            name: name.to_owned(),
            kind: FileKind::Folder,
            size: None,
            modified: 0,
            mime_type: "application/vnd.google-apps.folder".to_owned(),
            md5: None,
            head_revision: None,
            drive: None,
            can: Capabilities::default(),
        }
    }

    fn row(id: &str, path: &str, is_dir: bool) -> BaselineRow {
        BaselineRow {
            remote_id: DriveFileId::new(id),
            path: PathBuf::from(path),
            is_dir,
            content: String::new(),
            size: 0,
            mtime: 0,
            device: 0,
            inode: 0,
        }
    }

    #[test]
    fn moves_an_old_top_level_folder_to_its_drive_path() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .save_account(&Account {
                id: ACCOUNT.into(),
                email: "test@example.com".into(),
                display_name: "Test".into(),
            })
            .unwrap();
        store
            .begin_inventory(ACCOUNT, &DriveFileId::new("root"), "cursor")
            .unwrap();
        store
            .save_inventory_page(
                ACCOUNT,
                &[
                    folder("clients", "root", "Clients"),
                    folder("acme", "clients", "Acme"),
                ],
            )
            .unwrap();
        store.finish_inventory(ACCOUNT).unwrap();
        store
            .save_baseline(ACCOUNT, &row("acme", "Acme", true))
            .unwrap();
        store
            .save_baseline(ACCOUNT, &row("a", "Acme/a.txt", false))
            .unwrap();

        let dir = std::env::temp_dir().join(format!("aerie-relocate-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Acme")).unwrap();
        std::fs::write(dir.join("Acme/a.txt"), "a").unwrap();

        let roots = root_locations(&store, ACCOUNT, &[DriveFileId::new("acme")]);
        assert_eq!(roots[0].1, PathBuf::from("My Drive/Clients/Acme"));
        relocate_old_roots(&mut store, ACCOUNT, &dir, &roots).unwrap();

        assert!(dir.join("My Drive/Clients/Acme/a.txt").is_file());
        assert!(!dir.join("Acme").exists());
        let mut paths: Vec<_> = store
            .baseline(ACCOUNT)
            .unwrap()
            .into_iter()
            .map(|row| row.path)
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                PathBuf::from("My Drive/Clients/Acme"),
                PathBuf::from("My Drive/Clients/Acme/a.txt")
            ]
        );

        // A second pass leaves everything where it is.
        relocate_old_roots(&mut store, ACCOUNT, &dir, &roots).unwrap();
        assert!(dir.join("My Drive/Clients/Acme/a.txt").is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
