//! Reconciliation: compare Drive, the local folder and the baseline, and decide what to do.
//! This is pure: it reads three snapshots and returns operations, so every rule is
//! testable without a network or a filesystem.
//!
//! Rules, in short:
//! - A side that changed since the baseline wins over a side that didn't.
//! - When both changed, keep both versions.
//! - An edit always beats a deletion: a deleted item that was edited elsewhere comes back.
//! - Deletion propagates only for items unchanged on the other side, and only to a trash.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use aerie_core::DriveFileId;

use crate::model::{BaselineItem, LocalItem, Op, RemoteItem};

pub fn reconcile(baseline: &[BaselineItem], remote: &[RemoteItem], local: &[LocalItem]) -> Vec<Op> {
    let all_local = local;
    let all_remote = remote;
    let remote_by_id: HashMap<&DriveFileId, &RemoteItem> =
        remote.iter().map(|item| (&item.id, item)).collect();
    let local_by_path: HashMap<&Path, &LocalItem> = local
        .iter()
        .map(|item| (item.path.as_path(), item))
        .collect();
    let local_by_inode: HashMap<(u64, u64), &LocalItem> =
        local.iter().map(|item| (item.inode, item)).collect();

    // Match each baseline item to its local counterpart: first where it was, then (for
    // renames and moves) by inode, as long as it's unmodified and not claimed already.
    let mut claimed: HashSet<&Path> = HashSet::new();
    let mut matched: HashMap<&DriveFileId, &LocalItem> = HashMap::new();
    for item in baseline {
        if let Some(found) = local_by_path
            .get(item.path.as_path())
            .filter(|found| found.is_dir == item.is_dir)
        {
            claimed.insert(found.path.as_path());
            matched.insert(&item.remote_id, found);
        }
    }
    let unmatched: Vec<&BaselineItem> = baseline
        .iter()
        .filter(|item| !matched.contains_key(&item.remote_id))
        .collect();
    for item in unmatched {
        if let Some(found) = local_by_inode.get(&item.inode).filter(|found| {
            found.is_dir == item.is_dir
                && !claimed.contains(found.path.as_path())
                && (item.is_dir || (found.size == item.size && found.mtime == item.mtime))
        }) {
            claimed.insert(found.path.as_path());
            matched.insert(&item.remote_id, found);
        }
    }

    let mut ops = Vec::new();
    let known: HashSet<&DriveFileId> = baseline.iter().map(|item| &item.remote_id).collect();

    for item in baseline {
        let remote = remote_by_id.get(&item.remote_id).copied();
        let local = matched.get(&item.remote_id).copied();
        let id = item.remote_id.clone();
        match (remote, local) {
            (None, None) => ops.push(Op::Forget { remote_id: id }),

            (None, Some(local)) if item.is_dir => {
                if changed_locally_under(&local.path, baseline, &matched, &claimed, all_local) {
                    // New or edited files inside keep the folder alive on Drive.
                    ops.push(Op::Forget { remote_id: id });
                    ops.push(Op::CreateRemoteDir {
                        path: local.path.clone(),
                    });
                } else {
                    ops.push(Op::TrashLocal {
                        remote_id: id,
                        path: local.path.clone(),
                    });
                }
            }
            (None, Some(local)) => {
                if item.local_changed(local) || local.path != item.path {
                    ops.push(Op::Forget { remote_id: id });
                    ops.push(Op::UploadNew {
                        path: local.path.clone(),
                    });
                } else {
                    ops.push(Op::TrashLocal {
                        remote_id: id,
                        path: local.path.clone(),
                    });
                }
            }

            (Some(remote_dir), None) if item.is_dir => {
                if changed_remotely_under(
                    &item.path,
                    &remote_dir.path,
                    baseline,
                    &remote_by_id,
                    all_remote,
                    &known,
                ) {
                    ops.push(Op::CreateLocalDir {
                        remote_id: id,
                        path: remote_dir.path.clone(),
                    });
                } else {
                    ops.push(Op::TrashRemote {
                        remote_id: id,
                        path: item.path.clone(),
                    });
                }
            }
            (Some(remote), None) => {
                if item.remote_changed(remote) || remote.path != item.path {
                    ops.push(Op::Download {
                        remote_id: id,
                        path: remote.path.clone(),
                    });
                } else {
                    ops.push(Op::TrashRemote {
                        remote_id: id,
                        path: item.path.clone(),
                    });
                }
            }

            (Some(remote), Some(local)) => {
                let remote_moved = remote.path != item.path;
                let local_moved = local.path != item.path;
                let path = if remote_moved && remote.path != local.path {
                    // Drive's location wins when both sides moved it.
                    ops.push(Op::MoveLocal {
                        remote_id: id.clone(),
                        from: local.path.clone(),
                        to: remote.path.clone(),
                    });
                    remote.path.clone()
                } else if local_moved && !remote_moved {
                    ops.push(Op::MoveRemote {
                        remote_id: id.clone(),
                        from: item.path.clone(),
                        to: local.path.clone(),
                    });
                    local.path.clone()
                } else {
                    local.path.clone()
                };
                if !item.is_dir {
                    match (item.remote_changed(remote), item.local_changed(local)) {
                        (true, true) => ops.push(Op::Conflict {
                            remote_id: id,
                            path,
                        }),
                        (true, false) => ops.push(Op::Download {
                            remote_id: id,
                            path,
                        }),
                        (false, true) => ops.push(Op::UploadReplace {
                            remote_id: id,
                            path,
                        }),
                        (false, false) => {}
                    }
                }
            }
        }
    }

    // New on Drive.
    for remote in remote.iter().filter(|item| !known.contains(&item.id)) {
        let existing = local_by_path
            .get(remote.path.as_path())
            .filter(|found| !claimed.contains(found.path.as_path()));
        match existing {
            Some(found) if found.is_dir && remote.is_dir => {
                // A matching folder already exists; adopt it.
                claimed.insert(found.path.as_path());
                ops.push(Op::CreateLocalDir {
                    remote_id: remote.id.clone(),
                    path: remote.path.clone(),
                });
            }
            Some(found) if !found.is_dir && !remote.is_dir => {
                // An untracked local file is in the way; keep both.
                claimed.insert(found.path.as_path());
                ops.push(Op::Conflict {
                    remote_id: remote.id.clone(),
                    path: remote.path.clone(),
                });
            }
            _ if remote.is_dir => ops.push(Op::CreateLocalDir {
                remote_id: remote.id.clone(),
                path: remote.path.clone(),
            }),
            _ => ops.push(Op::Download {
                remote_id: remote.id.clone(),
                path: remote.path.clone(),
            }),
        }
    }

    // New locally.
    for local in local
        .iter()
        .filter(|item| !claimed.contains(item.path.as_path()))
    {
        ops.push(if local.is_dir {
            Op::CreateRemoteDir {
                path: local.path.clone(),
            }
        } else {
            Op::UploadNew {
                path: local.path.clone(),
            }
        });
    }

    order(drop_nested_deletions(ops))
}

/// Whether a locally kept folder holds anything new or edited since the baseline.
fn changed_locally_under(
    dir: &Path,
    baseline: &[BaselineItem],
    matched: &HashMap<&DriveFileId, &LocalItem>,
    claimed: &HashSet<&Path>,
    local: &[LocalItem],
) -> bool {
    let edited = baseline.iter().any(|item| {
        matched
            .get(&item.remote_id)
            .is_some_and(|found| found.path.starts_with(dir) && item.local_changed(found))
    });
    let untracked = local.iter().any(|item| {
        item.path.starts_with(dir) && item.path != dir && !claimed.contains(item.path.as_path())
    });
    edited || untracked
}

/// Whether a folder still on Drive holds anything new or edited since the baseline.
fn changed_remotely_under(
    baseline_dir: &Path,
    remote_dir: &Path,
    baseline: &[BaselineItem],
    remote_by_id: &HashMap<&DriveFileId, &RemoteItem>,
    remote: &[RemoteItem],
    known: &HashSet<&DriveFileId>,
) -> bool {
    let edited = baseline.iter().any(|item| {
        item.path.starts_with(baseline_dir)
            && remote_by_id
                .get(&item.remote_id)
                .is_some_and(|now| item.remote_changed(now))
    });
    let added = remote.iter().any(|item| {
        item.path.starts_with(remote_dir) && item.path != remote_dir && !known.contains(&item.id)
    });
    edited || added
}

/// A trashed folder takes its contents with it; drop the redundant child deletions.
fn drop_nested_deletions(ops: Vec<Op>) -> Vec<Op> {
    let local_dirs: Vec<PathBuf> = ops
        .iter()
        .filter_map(|op| match op {
            Op::TrashLocal { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    let remote_dirs: Vec<PathBuf> = ops
        .iter()
        .filter_map(|op| match op {
            Op::TrashRemote { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    let inside =
        |path: &Path, dirs: &[PathBuf]| dirs.iter().any(|dir| path != dir && path.starts_with(dir));
    ops.into_iter()
        .map(|op| match &op {
            Op::TrashLocal { remote_id, path } if inside(path, &local_dirs) => Op::Forget {
                remote_id: remote_id.clone(),
            },
            Op::TrashRemote { remote_id, path } if inside(path, &remote_dirs) => Op::Forget {
                remote_id: remote_id.clone(),
            },
            _ => op,
        })
        .collect()
}

/// Parents before children for creations and moves; children before parents for
/// deletions; content transfers in between.
fn order(mut ops: Vec<Op>) -> Vec<Op> {
    let depth = |op: &Op| op.path().map_or(0, |path| path.components().count());
    let phase = |op: &Op| match op {
        Op::Forget { .. } => 0,
        Op::CreateLocalDir { .. } | Op::CreateRemoteDir { .. } => 1,
        Op::MoveLocal { .. } | Op::MoveRemote { .. } => 2,
        Op::Download { .. }
        | Op::UploadNew { .. }
        | Op::UploadReplace { .. }
        | Op::Conflict { .. } => 3,
        Op::TrashLocal { .. } | Op::TrashRemote { .. } => 4,
    };
    ops.sort_by(|a, b| {
        phase(a).cmp(&phase(b)).then_with(|| {
            if a.is_deletion() {
                depth(b).cmp(&depth(a))
            } else {
                depth(a).cmp(&depth(b))
            }
        })
    });
    ops
}

/// Plan-level safety: an unusually large deletion is held for the user to confirm. Returns
/// how many items would be deleted when the plan crosses the threshold.
pub fn deletions_needing_confirmation(ops: &[Op], tracked: usize) -> Option<usize> {
    let deletions = ops.iter().filter(|op| op.is_deletion()).count();
    let threshold = (tracked / 4).max(50);
    (deletions > threshold).then_some(deletions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ContentId;

    fn id(id: &str) -> DriveFileId {
        DriveFileId::new(id)
    }

    fn p(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    fn base_file(remote: &str, path: &str, md5: &str, inode: u64) -> BaselineItem {
        BaselineItem {
            remote_id: id(remote),
            path: p(path),
            is_dir: false,
            content: ContentId::Md5(md5.into()),
            size: 10,
            mtime: 100,
            inode: (1, inode),
        }
    }

    fn base_dir(remote: &str, path: &str, inode: u64) -> BaselineItem {
        BaselineItem {
            remote_id: id(remote),
            path: p(path),
            is_dir: true,
            content: ContentId::Folder,
            size: 0,
            mtime: 0,
            inode: (1, inode),
        }
    }

    fn remote_file(remote: &str, path: &str, md5: &str) -> RemoteItem {
        RemoteItem {
            id: id(remote),
            path: p(path),
            is_dir: false,
            content: ContentId::Md5(md5.into()),
            size: Some(10),
        }
    }

    fn remote_dir(remote: &str, path: &str) -> RemoteItem {
        RemoteItem {
            id: id(remote),
            path: p(path),
            is_dir: true,
            content: ContentId::Folder,
            size: None,
        }
    }

    fn local_file(path: &str, size: u64, mtime: i64, inode: u64) -> LocalItem {
        LocalItem {
            path: p(path),
            is_dir: false,
            size,
            mtime,
            inode: (1, inode),
        }
    }

    fn local_dir(path: &str, inode: u64) -> LocalItem {
        LocalItem {
            path: p(path),
            is_dir: true,
            size: 0,
            mtime: 0,
            inode: (1, inode),
        }
    }

    /// A synced folder `Docs` with `Docs/a.txt`, identical on both sides.
    fn synced() -> (Vec<BaselineItem>, Vec<RemoteItem>, Vec<LocalItem>) {
        (
            vec![
                base_dir("d", "Docs", 1),
                base_file("a", "Docs/a.txt", "m1", 2),
            ],
            vec![
                remote_dir("d", "Docs"),
                remote_file("a", "Docs/a.txt", "m1"),
            ],
            vec![local_dir("Docs", 1), local_file("Docs/a.txt", 10, 100, 2)],
        )
    }

    #[test]
    fn does_nothing_when_both_sides_match() {
        let (baseline, remote, local) = synced();
        assert!(reconcile(&baseline, &remote, &local).is_empty());
    }

    #[test]
    fn downloads_a_remote_edit() {
        let (baseline, mut remote, local) = synced();
        remote[1].content = ContentId::Md5("m2".into());
        assert_eq!(
            reconcile(&baseline, &remote, &local),
            [Op::Download {
                remote_id: id("a"),
                path: p("Docs/a.txt")
            }]
        );
    }

    #[test]
    fn uploads_a_local_edit() {
        let (baseline, remote, mut local) = synced();
        local[1].mtime = 200;
        assert_eq!(
            reconcile(&baseline, &remote, &local),
            [Op::UploadReplace {
                remote_id: id("a"),
                path: p("Docs/a.txt")
            }]
        );
    }

    #[test]
    fn keeps_both_versions_when_both_sides_edited() {
        let (baseline, mut remote, mut local) = synced();
        remote[1].content = ContentId::Md5("m2".into());
        local[1].size = 11;
        assert_eq!(
            reconcile(&baseline, &remote, &local),
            [Op::Conflict {
                remote_id: id("a"),
                path: p("Docs/a.txt")
            }]
        );
    }

    #[test]
    fn creates_new_remote_items_locally_parents_first() {
        let (baseline, mut remote, local) = synced();
        remote.push(remote_file("b", "Docs/New/b.txt", "m3"));
        remote.push(remote_dir("n", "Docs/New"));
        assert_eq!(
            reconcile(&baseline, &remote, &local),
            [
                Op::CreateLocalDir {
                    remote_id: id("n"),
                    path: p("Docs/New")
                },
                Op::Download {
                    remote_id: id("b"),
                    path: p("Docs/New/b.txt")
                },
            ]
        );
    }

    #[test]
    fn uploads_new_local_items_parents_first() {
        let (baseline, remote, mut local) = synced();
        local.push(local_file("Docs/Notes/c.txt", 5, 1, 9));
        local.push(local_dir("Docs/Notes", 8));
        assert_eq!(
            reconcile(&baseline, &remote, &local),
            [
                Op::CreateRemoteDir {
                    path: p("Docs/Notes")
                },
                Op::UploadNew {
                    path: p("Docs/Notes/c.txt")
                },
            ]
        );
    }

    #[test]
    fn propagates_an_unopposed_remote_deletion_to_the_local_trash() {
        let (baseline, mut remote, local) = synced();
        remote.pop();
        assert_eq!(
            reconcile(&baseline, &remote, &local),
            [Op::TrashLocal {
                remote_id: id("a"),
                path: p("Docs/a.txt")
            }]
        );
    }

    #[test]
    fn a_local_edit_beats_a_remote_deletion() {
        let (baseline, mut remote, mut local) = synced();
        remote.pop();
        local[1].mtime = 300;
        assert_eq!(
            reconcile(&baseline, &remote, &local),
            [
                Op::Forget { remote_id: id("a") },
                Op::UploadNew {
                    path: p("Docs/a.txt")
                },
            ]
        );
    }

    #[test]
    fn propagates_an_unopposed_local_deletion_to_the_drive_trash() {
        let (baseline, remote, mut local) = synced();
        local.pop();
        assert_eq!(
            reconcile(&baseline, &remote, &local),
            [Op::TrashRemote {
                remote_id: id("a"),
                path: p("Docs/a.txt")
            }]
        );
    }

    #[test]
    fn a_remote_edit_beats_a_local_deletion() {
        let (baseline, mut remote, mut local) = synced();
        local.pop();
        remote[1].content = ContentId::Md5("m2".into());
        assert_eq!(
            reconcile(&baseline, &remote, &local),
            [Op::Download {
                remote_id: id("a"),
                path: p("Docs/a.txt")
            }]
        );
    }

    #[test]
    fn follows_a_remote_rename() {
        let (baseline, mut remote, local) = synced();
        remote[1].path = p("Docs/renamed.txt");
        assert_eq!(
            reconcile(&baseline, &remote, &local),
            [Op::MoveLocal {
                remote_id: id("a"),
                from: p("Docs/a.txt"),
                to: p("Docs/renamed.txt")
            }]
        );
    }

    #[test]
    fn follows_a_local_rename_by_inode() {
        let (baseline, remote, mut local) = synced();
        local[1].path = p("Docs/renamed.txt");
        assert_eq!(
            reconcile(&baseline, &remote, &local),
            [Op::MoveRemote {
                remote_id: id("a"),
                from: p("Docs/a.txt"),
                to: p("Docs/renamed.txt")
            }]
        );
    }

    #[test]
    fn a_local_rename_with_edits_becomes_a_new_upload_and_a_trashed_original() {
        let (baseline, remote, mut local) = synced();
        local[1].path = p("Docs/renamed.txt");
        local[1].mtime = 500;
        let ops = reconcile(&baseline, &remote, &local);
        assert_eq!(
            ops,
            [
                Op::UploadNew {
                    path: p("Docs/renamed.txt")
                },
                Op::TrashRemote {
                    remote_id: id("a"),
                    path: p("Docs/a.txt")
                },
            ]
        );
    }

    #[test]
    fn drive_wins_when_both_sides_move_an_item() {
        let (baseline, mut remote, mut local) = synced();
        remote[1].path = p("Docs/drive-name.txt");
        local[1].path = p("Docs/local-name.txt");
        assert_eq!(
            reconcile(&baseline, &remote, &local),
            [Op::MoveLocal {
                remote_id: id("a"),
                from: p("Docs/local-name.txt"),
                to: p("Docs/drive-name.txt")
            }]
        );
    }

    #[test]
    fn trashes_a_remotely_deleted_folder_once() {
        let (baseline, _, local) = synced();
        assert_eq!(
            reconcile(&baseline, &[], &local),
            [
                Op::Forget { remote_id: id("a") },
                Op::TrashLocal {
                    remote_id: id("d"),
                    path: p("Docs")
                },
            ]
        );
    }

    #[test]
    fn keeps_a_remotely_deleted_folder_with_local_changes() {
        let (baseline, _, mut local) = synced();
        local[1].mtime = 999;
        let ops = reconcile(&baseline, &[], &local);
        assert!(ops.contains(&Op::CreateRemoteDir { path: p("Docs") }));
        assert!(ops.contains(&Op::UploadNew {
            path: p("Docs/a.txt")
        }));
        assert!(!ops.iter().any(Op::is_deletion));
    }

    #[test]
    fn keeps_a_locally_deleted_folder_with_remote_changes() {
        let (baseline, mut remote, _) = synced();
        remote[1].content = ContentId::Md5("m9".into());
        let ops = reconcile(&baseline, &remote, &[]);
        assert_eq!(
            ops,
            [
                Op::CreateLocalDir {
                    remote_id: id("d"),
                    path: p("Docs")
                },
                Op::Download {
                    remote_id: id("a"),
                    path: p("Docs/a.txt")
                },
            ]
        );
    }

    #[test]
    fn forgets_items_deleted_on_both_sides() {
        let (baseline, _, _) = synced();
        assert_eq!(
            reconcile(&baseline, &[], &[]),
            [
                Op::Forget { remote_id: id("d") },
                Op::Forget { remote_id: id("a") }
            ]
        );
    }

    #[test]
    fn keeps_both_when_a_new_remote_file_meets_an_untracked_local_one() {
        let remote = [remote_file("x", "Report.pdf", "m")];
        let local = [local_file("Report.pdf", 3, 3, 7)];
        assert_eq!(
            reconcile(&[], &remote, &local),
            [Op::Conflict {
                remote_id: id("x"),
                path: p("Report.pdf")
            }]
        );
    }

    #[test]
    fn holds_large_deletions_for_confirmation() {
        let ops: Vec<_> = (0..60)
            .map(|i| Op::TrashRemote {
                remote_id: id(&i.to_string()),
                path: p(&format!("f{i}")),
            })
            .collect();
        assert_eq!(deletions_needing_confirmation(&ops, 1_000), None);
        assert_eq!(deletions_needing_confirmation(&ops, 100), Some(60));
        assert_eq!(deletions_needing_confirmation(&ops[..10], 20), None);
    }
}
