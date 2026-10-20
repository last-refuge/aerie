//! The three views reconciliation compares: Drive now, the local folder now, and the
//! baseline both sides last agreed on.

use std::path::PathBuf;

use aerie_core::DriveFileId;

/// Identifies a version of remote content. Checksums identify content exactly; the head
/// revision stands in when Drive reports no checksum.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ContentId {
    Md5(String),
    Revision(String),
    /// Folders have no content.
    Folder,
}

/// A synced item as it is on Drive, placed at the relative path it maps to locally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteItem {
    pub id: DriveFileId,
    /// Relative to the sync root, with names made safe for the local filesystem.
    pub path: PathBuf,
    pub is_dir: bool,
    pub content: ContentId,
    pub size: Option<u64>,
}

/// A file or folder found in the local sync root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalItem {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    /// Modification time in nanoseconds.
    pub mtime: i64,
    /// Device and inode, which follow a file across local renames.
    pub inode: (u64, u64),
}

/// The state both sides agreed on after the last successful sync of an item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineItem {
    pub remote_id: DriveFileId,
    pub path: PathBuf,
    pub is_dir: bool,
    pub content: ContentId,
    pub size: u64,
    pub mtime: i64,
    pub inode: (u64, u64),
}

impl BaselineItem {
    /// Whether the local item differs from what was last synced.
    pub fn local_changed(&self, local: &LocalItem) -> bool {
        !self.is_dir && (local.size != self.size || local.mtime != self.mtime)
    }

    pub fn remote_changed(&self, remote: &RemoteItem) -> bool {
        !self.is_dir && remote.content != self.content
    }
}

/// One step to bring the two sides back into agreement. Paths are relative to the root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// A folder exists only on Drive.
    CreateLocalDir {
        remote_id: DriveFileId,
        path: PathBuf,
    },
    /// A folder exists only locally; its parent is identified by local path.
    CreateRemoteDir { path: PathBuf },
    Download {
        remote_id: DriveFileId,
        path: PathBuf,
    },
    /// A new local file; its Drive parent is the folder at the path's parent.
    UploadNew { path: PathBuf },
    /// Local edits replace the content of an existing Drive file.
    UploadReplace {
        remote_id: DriveFileId,
        path: PathBuf,
    },
    /// Drive renamed or moved the item; follow locally.
    MoveLocal {
        remote_id: DriveFileId,
        from: PathBuf,
        to: PathBuf,
    },
    /// The item was renamed or moved locally; follow on Drive.
    MoveRemote {
        remote_id: DriveFileId,
        from: PathBuf,
        to: PathBuf,
    },
    /// Deleted on Drive and unchanged locally: move the local copy to the trash.
    TrashLocal {
        remote_id: DriveFileId,
        path: PathBuf,
    },
    /// Deleted locally and unchanged on Drive: move the Drive item to Drive's trash.
    TrashRemote {
        remote_id: DriveFileId,
        path: PathBuf,
    },
    /// Both sides changed. Drive's version is saved as a conflict copy beside the local
    /// file, and the local version is then uploaded, so neither is lost.
    Conflict {
        remote_id: DriveFileId,
        path: PathBuf,
    },
    /// Gone from both sides; drop it from the baseline.
    Forget { remote_id: DriveFileId },
}

impl Op {
    pub fn is_deletion(&self) -> bool {
        matches!(self, Self::TrashLocal { .. } | Self::TrashRemote { .. })
    }

    /// Where the op acts, for ordering parents before children.
    pub fn path(&self) -> Option<&PathBuf> {
        match self {
            Self::CreateLocalDir { path, .. }
            | Self::CreateRemoteDir { path }
            | Self::Download { path, .. }
            | Self::UploadNew { path }
            | Self::UploadReplace { path, .. }
            | Self::TrashLocal { path, .. }
            | Self::TrashRemote { path, .. }
            | Self::Conflict { path, .. } => Some(path),
            Self::MoveLocal { to, .. } | Self::MoveRemote { to, .. } => Some(to),
            Self::Forget { .. } => None,
        }
    }
}
