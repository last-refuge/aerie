//! Carrying out a plan against Drive and the local folder, recording the new baseline as
//! each step completes. A step that fails leaves its baseline untouched, so the next cycle
//! sees the same difference and tries again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use aerie_core::DriveFileId;
use aerie_drive::api::{DriveApi, intervening_revisions};
use aerie_drive::upload::{CHUNK_UNIT, NewFile, RetryPolicy};
use aerie_drive::{Error as DriveError, Result};
use aerie_store::{BaselineRow, Store};
use gio::prelude::*;

use crate::model::{BaselineItem, ContentId, LocalItem, Op, RemoteItem};
use crate::scan;

/// What one step did, for status and logs.
#[derive(Debug, PartialEq, Eq)]
pub enum Done {
    Applied,
    /// Both versions kept; this is the path of Drive's copy.
    ConflictCopy(PathBuf),
    /// Skipped and why; nothing changed.
    Skipped(&'static str),
}

pub struct Executor<'a> {
    pub drive: &'a DriveApi,
    pub store: &'a mut Store,
    pub account: &'a str,
    /// The local sync folder.
    pub root: &'a Path,
    /// Where items that can't use the system trash are kept.
    pub quarantine: &'a Path,
    remote: HashMap<DriveFileId, RemoteItem>,
    baseline: HashMap<DriveFileId, BaselineItem>,
    /// Drive folder ID for each local folder path, kept current as steps run.
    folders: HashMap<PathBuf, DriveFileId>,
}

impl<'a> Executor<'a> {
    pub fn new(
        drive: &'a DriveApi,
        store: &'a mut Store,
        account: &'a str,
        root: &'a Path,
        quarantine: &'a Path,
        remote: &[RemoteItem],
        baseline: &[BaselineItem],
    ) -> Self {
        let folders = baseline
            .iter()
            .filter(|item| item.is_dir)
            .map(|item| (item.path.clone(), item.remote_id.clone()))
            .chain(
                remote
                    .iter()
                    .filter(|item| item.is_dir)
                    .map(|item| (item.path.clone(), item.id.clone())),
            )
            .collect();
        Self {
            drive,
            store,
            account,
            root,
            quarantine,
            remote: remote
                .iter()
                .map(|item| (item.id.clone(), item.clone()))
                .collect(),
            baseline: baseline
                .iter()
                .map(|item| (item.remote_id.clone(), item.clone()))
                .collect(),
            folders,
        }
    }

    pub async fn run(&mut self, op: &Op) -> Result<Done> {
        // Every step re-checks that the folder is still there, so a disappeared or
        // unmounted root is never mistaken for deleted files.
        if !self.root.is_dir() {
            return Err(local("the sync folder is missing"));
        }
        match op {
            Op::Forget { remote_id } => {
                self.forget(remote_id)?;
                Ok(Done::Applied)
            }
            Op::CreateLocalDir { remote_id, path } => {
                std::fs::create_dir_all(self.root.join(path))?;
                self.folders.insert(path.clone(), remote_id.clone());
                self.record(remote_id, path, ContentId::Folder, None)?;
                Ok(Done::Applied)
            }
            Op::CreateRemoteDir { path } => self.create_remote_dir(path).await,
            Op::Download { remote_id, path } => {
                self.download(remote_id, path).await?;
                Ok(Done::Applied)
            }
            Op::UploadNew { path } => self.upload_new(path).await,
            Op::UploadReplace { remote_id, path } => self.upload_replace(remote_id, path).await,
            Op::Conflict { remote_id, path } => self.conflict(remote_id, path).await,
            Op::MoveLocal {
                remote_id,
                from,
                to,
            } => {
                let to = self.free_path(to);
                if let Some(parent) = self.root.join(&to).parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::rename(self.root.join(from), self.root.join(&to))?;
                self.moved(remote_id, from, &to)?;
                Ok(Done::Applied)
            }
            Op::MoveRemote {
                remote_id,
                from,
                to,
            } => {
                self.move_remote(remote_id, from, to).await?;
                Ok(Done::Applied)
            }
            Op::TrashLocal { remote_id, path } => {
                self.trash_local(path)?;
                self.store
                    .forget_baseline_under(self.account, path)
                    .map_err(store)?;
                let _ = remote_id;
                Ok(Done::Applied)
            }
            Op::TrashRemote { remote_id, path } => {
                self.drive.trash(remote_id).await?;
                self.store
                    .forget_baseline_under(self.account, path)
                    .map_err(store)?;
                Ok(Done::Applied)
            }
        }
    }

    fn forget(&mut self, remote_id: &DriveFileId) -> Result<()> {
        self.baseline.remove(remote_id);
        self.store
            .forget_baseline(self.account, remote_id)
            .map_err(store)
    }

    /// Records an item as in agreement. `before` is the local state read before an upload
    /// began, so an edit made during the upload still shows as a change next time.
    fn record(
        &mut self,
        remote_id: &DriveFileId,
        path: &Path,
        content: ContentId,
        before: Option<LocalItem>,
    ) -> Result<()> {
        let local = match before {
            Some(local) => local,
            None => scan::stat(self.root, path)?,
        };
        let row = BaselineRow {
            remote_id: remote_id.clone(),
            path: path.to_owned(),
            is_dir: local.is_dir,
            content: encode(&content),
            size: local.size,
            mtime: local.mtime,
            device: local.inode.0,
            inode: local.inode.1,
        };
        self.store
            .save_baseline(self.account, &row)
            .map_err(store)?;
        self.baseline.insert(remote_id.clone(), decode_row(&row));
        Ok(())
    }

    fn parent_folder(&self, path: &Path) -> Option<DriveFileId> {
        let parent = path.parent()?;
        if parent.as_os_str().is_empty() {
            return None;
        }
        self.folders.get(parent).cloned()
    }

    async fn create_remote_dir(&mut self, path: &Path) -> Result<Done> {
        // The top level of the sync folder holds the selected Drive folders only; a folder
        // made there may be one the user deselected, so it's never uploaded on its own.
        let Some(parent) = self.parent_folder(path) else {
            return Ok(Done::Skipped(
                "only folders chosen in My Drive sync; new top-level folders stay local",
            ));
        };
        let entry = self.drive.create_folder(&parent, &file_name(path)).await?;
        self.folders.insert(path.to_owned(), entry.id.clone());
        self.record(&entry.id, path, ContentId::Folder, None)?;
        Ok(Done::Applied)
    }

    /// Downloads Drive's content to `path`. A local file that changed since the baseline
    /// is kept; Drive's copy is then saved beside it instead.
    async fn download(&mut self, remote_id: &DriveFileId, path: &Path) -> Result<PathBuf> {
        let remote = self
            .remote
            .get(remote_id)
            .cloned()
            .ok_or_else(|| local("the item is no longer on Drive"))?;
        let destination = self.root.join(path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let partial = partial_path(&destination);
        self.drive
            .download_to(remote_id, &partial, |_, _| {})
            .await?;
        let written = std::fs::metadata(&partial)?.len();
        if remote.size.is_some_and(|size| size != written) {
            let _ = std::fs::remove_file(&partial);
            return Err(local("the download was incomplete"));
        }
        // Replace only what the baseline says is there; anything else is a local edit.
        let expected = self
            .baseline
            .get(remote_id)
            .filter(|item| item.path == path)
            .map(|item| item.mtime);
        let current = scan::stat(self.root, path).ok().map(|item| item.mtime);
        let target = if current.is_none() || current == expected {
            path.to_owned()
        } else {
            self.conflict_name(path)
        };
        std::fs::rename(&partial, self.root.join(&target))?;
        if target == path {
            self.record(remote_id, path, remote.content.clone(), None)?;
        }
        Ok(target)
    }

    async fn upload_new(&mut self, path: &Path) -> Result<Done> {
        let Some(parent) = self.parent_folder(path) else {
            return Ok(Done::Skipped(
                "only files inside synced folders sync; this one stays local",
            ));
        };
        let before = scan::stat(self.root, path)?;
        let mime = mime_type(path);
        let session = self
            .drive
            .start_upload(
                &NewFile {
                    name: &file_name(path),
                    parent: &parent,
                    mime_type: &mime,
                },
                before.size,
            )
            .await?;
        let mut file = tokio::fs::File::open(self.root.join(path)).await?;
        let entry = self
            .drive
            .upload(&session, &mut file, CHUNK_UNIT * 32, RetryPolicy::default())
            .await?;
        let content = crate::tree::content_id(&entry);
        self.record(&entry.id, path, content, Some(before))?;
        Ok(Done::Applied)
    }

    /// Replaces Drive's content with the local file. Drive has no conditional writes, so
    /// the remote version is checked just before writing and the revision list just
    /// after; an edit that slipped in between is saved locally as a conflict copy.
    async fn upload_replace(&mut self, remote_id: &DriveFileId, path: &Path) -> Result<Done> {
        let baseline = self.baseline.get(remote_id).cloned();
        let current = self.drive.version(remote_id).await?;
        let current_content = current
            .md5_checksum
            .clone()
            .map(ContentId::Md5)
            .unwrap_or_else(|| {
                ContentId::Revision(current.head_revision_id.clone().unwrap_or_default())
            });
        if baseline
            .as_ref()
            .is_some_and(|base| base.content != current_content)
        {
            // Drive changed since the plan was made: keep both.
            if let Some(remote) = self.remote.get_mut(remote_id) {
                remote.content = current_content;
            }
            return self.conflict(remote_id, path).await;
        }

        let before = scan::stat(self.root, path)?;
        let session = self
            .drive
            .start_replace(remote_id, &mime_type(path), before.size)
            .await?;
        let mut file = tokio::fs::File::open(self.root.join(path)).await?;
        let entry = self
            .drive
            .upload(&session, &mut file, CHUNK_UNIT * 32, RetryPolicy::default())
            .await?;

        let after = self.drive.version(remote_id).await?;
        if let Some(ours) = after.head_revision_id.as_deref() {
            let revisions = self.drive.revisions(remote_id).await?;
            if let Some(lost) = intervening_revisions(&revisions, &current, ours).last() {
                let content = self.drive.download_revision(remote_id, &lost.id).await?;
                let copy = self.conflict_name(path);
                std::fs::write(self.root.join(&copy), content)?;
                tracing::warn!("recovered an edit overwritten during upload");
                self.record(
                    remote_id,
                    path,
                    crate::tree::content_id(&entry),
                    Some(before),
                )?;
                return Ok(Done::ConflictCopy(copy));
            }
        }
        self.record(
            remote_id,
            path,
            crate::tree::content_id(&entry),
            Some(before),
        )?;
        Ok(Done::Applied)
    }

    /// Keeps both versions: Drive's goes beside the local file as a conflict copy (which
    /// the next cycle uploads as a new file), then the local version replaces Drive's.
    async fn conflict(&mut self, remote_id: &DriveFileId, path: &Path) -> Result<Done> {
        let copy = self.conflict_name(path);
        let remote = self
            .remote
            .get(remote_id)
            .cloned()
            .ok_or_else(|| local("the item is no longer on Drive"))?;
        let destination = self.root.join(&copy);
        let partial = partial_path(&destination);
        self.drive
            .download_to(remote_id, &partial, |_, _| {})
            .await?;
        std::fs::rename(&partial, &destination)?;

        // Treat Drive's current content as the baseline, then upload the local version.
        let before = scan::stat(self.root, path)?;
        self.record(remote_id, path, remote.content, Some(before))?;
        let session = self
            .drive
            .start_replace(
                remote_id,
                &mime_type(path),
                scan::stat(self.root, path)?.size,
            )
            .await?;
        let before = scan::stat(self.root, path)?;
        let mut file = tokio::fs::File::open(self.root.join(path)).await?;
        let entry = self
            .drive
            .upload(&session, &mut file, CHUNK_UNIT * 32, RetryPolicy::default())
            .await?;
        self.record(
            remote_id,
            path,
            crate::tree::content_id(&entry),
            Some(before),
        )?;
        Ok(Done::ConflictCopy(copy))
    }

    async fn move_remote(&mut self, remote_id: &DriveFileId, from: &Path, to: &Path) -> Result<()> {
        let old_parent = self.parent_folder(from);
        let new_parent = self.parent_folder(to);
        if old_parent != new_parent {
            match (old_parent, new_parent) {
                (Some(old), Some(new)) => {
                    self.drive.move_to(remote_id, &old, &new).await?;
                }
                _ => return Err(local("moving in or out of the top level isn't supported")),
            }
        }
        if file_name(from) != file_name(to) {
            self.drive.rename(remote_id, &file_name(to)).await?;
        }
        self.moved(remote_id, from, to)
    }

    fn moved(&mut self, remote_id: &DriveFileId, from: &Path, to: &Path) -> Result<()> {
        self.store
            .move_baseline(self.account, from, to)
            .map_err(store)?;
        // Refresh cached state under the new paths.
        let moved: Vec<(PathBuf, DriveFileId)> = self
            .folders
            .iter()
            .filter_map(|(path, id)| {
                path.strip_prefix(from)
                    .ok()
                    .map(|rest| (to.join(rest), id.clone()))
            })
            .collect();
        self.folders.retain(|path, _| !path.starts_with(from));
        self.folders.extend(moved);
        for item in self.baseline.values_mut() {
            if let Ok(rest) = item.path.strip_prefix(from) {
                item.path = if rest.as_os_str().is_empty() {
                    to.to_owned()
                } else {
                    to.join(rest)
                };
            }
        }
        let _ = remote_id;
        Ok(())
    }

    /// Moves a local item to the desktop trash, or to Aerie's quarantine folder when the
    /// filesystem has no trash. Either way it stays recoverable.
    fn trash_local(&self, path: &Path) -> Result<()> {
        let full = self.root.join(path);
        if !full.exists() {
            return Ok(());
        }
        let file = gio::File::for_path(&full);
        if file.trash(gio::Cancellable::NONE).is_ok() {
            return Ok(());
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let target = self.quarantine.join(stamp.to_string()).join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&full, &target)?;
        Ok(())
    }

    fn free_path(&self, path: &Path) -> PathBuf {
        if self.root.join(path).exists() {
            self.conflict_name(path)
        } else {
            path.to_owned()
        }
    }

    /// `Report.pdf` → `Report (conflicted copy from Drive 2026-10-20).pdf`, numbered if
    /// that's taken too.
    fn conflict_name(&self, path: &Path) -> PathBuf {
        let date = time::OffsetDateTime::now_utc().date();
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        (1..)
            .map(|n| {
                let suffix = if n == 1 {
                    format!(" (conflicted copy from Drive {date})")
                } else {
                    format!(" (conflicted copy from Drive {date} {n})")
                };
                path.with_file_name(format!("{stem}{suffix}{ext}"))
            })
            .find(|candidate| !self.root.join(candidate).exists())
            .expect("an unused name exists")
    }
}

/// The hidden file a download writes before it's moved into place.
pub fn partial_path(destination: &Path) -> PathBuf {
    let name = destination
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    destination.with_file_name(format!(".{name}.aerie-partial"))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn mime_type(path: &Path) -> String {
    let (content_type, _) = gio::content_type_guess(Some(path), None);
    gio::content_type_get_mime_type(&content_type)
        .map_or_else(|| "application/octet-stream".to_owned(), String::from)
}

pub fn encode(content: &ContentId) -> String {
    match content {
        ContentId::Md5(md5) => format!("md5:{md5}"),
        ContentId::Revision(rev) => format!("rev:{rev}"),
        ContentId::Folder => "folder".to_owned(),
    }
}

pub fn decode(content: &str) -> ContentId {
    if let Some(md5) = content.strip_prefix("md5:") {
        ContentId::Md5(md5.to_owned())
    } else if let Some(rev) = content.strip_prefix("rev:") {
        ContentId::Revision(rev.to_owned())
    } else {
        ContentId::Folder
    }
}

pub fn decode_row(row: &BaselineRow) -> BaselineItem {
    BaselineItem {
        remote_id: row.remote_id.clone(),
        path: row.path.clone(),
        is_dir: row.is_dir,
        content: decode(&row.content),
        size: row.size,
        mtime: row.mtime,
        inode: (row.device, row.inode),
    }
}

fn local(message: &str) -> DriveError {
    DriveError::Io(std::io::Error::other(message.to_owned()))
}

fn store(err: aerie_store::Error) -> DriveError {
    DriveError::Io(std::io::Error::other(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_content_ids() {
        for content in [
            ContentId::Md5("abc".into()),
            ContentId::Revision("r1".into()),
            ContentId::Folder,
        ] {
            assert_eq!(decode(&encode(&content)), content);
        }
    }
}
