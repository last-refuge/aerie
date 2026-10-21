//! Mapping selected Drive folders to local paths. Names are made safe for the local
//! filesystem, and duplicate names in one folder get a stable numbered suffix.

use std::collections::HashMap;
use std::path::PathBuf;

use aerie_core::{DriveFileId, FileEntry};

use crate::model::{ContentId, RemoteItem};

/// Whether an item takes part in sync. Google-native documents stay browser-only, and
/// shortcuts aren't followed.
pub fn is_syncable(entry: &FileEntry) -> bool {
    entry.is_folder() || !entry.mime_type.starts_with("application/vnd.google-apps.")
}

/// Drive allows characters local filesystems don't, notably `/`.
pub fn local_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' => '∕',
            '\0' => '_',
            c => c,
        })
        .collect();
    match cleaned.as_str() {
        "" | "." | ".." => "_".to_owned(),
        _ => cleaned,
    }
}

pub(crate) fn numbered(name: &str, n: usize) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} ({n}){}", &name[..dot], &name[dot..]),
        _ => format!("{name} ({n})"),
    }
}

/// Items shared with the user that sit outside My Drive and every shared drive.
pub const SHARED_WITH_ME: &str = "Shared with me";

/// Where a synced folder goes on this computer: its whole path in Drive, under
/// "My Drive", its shared drive's name, or "Shared with me" for anything outside both.
/// `lookup` reads a cached item by ID.
pub fn root_path(
    entry: &FileEntry,
    my_drive: Option<&DriveFileId>,
    lookup: &dyn Fn(&DriveFileId) -> Option<FileEntry>,
) -> PathBuf {
    if Some(&entry.id) == my_drive {
        return PathBuf::from(aerie_store::MY_DRIVE);
    }
    let mut names = vec![local_name(&entry.name)];
    let mut top = entry.clone();
    // Bounded, so a cycle in bad metadata can't hang a pass.
    for _ in 0..256 {
        match top.parent.clone() {
            Some(parent) if Some(&parent) == my_drive => {
                names.push(aerie_store::MY_DRIVE.to_owned());
                break;
            }
            Some(parent) => match lookup(&parent) {
                Some(above) => {
                    names.push(local_name(&above.name));
                    top = above;
                }
                None => {
                    names.push(SHARED_WITH_ME.to_owned());
                    break;
                }
            },
            None => {
                if top.mime_type != aerie_drive::api::SHARED_DRIVE_MIME {
                    names.push(SHARED_WITH_ME.to_owned());
                }
                break;
            }
        }
    }
    names.into_iter().rev().collect()
}

/// Places every syncable item under the selected roots, each at its own local path (see
/// [`root_path`]). `children` lists a folder's cached children.
pub fn remote_items(
    roots: &[(FileEntry, PathBuf)],
    children: &dyn Fn(&DriveFileId) -> Vec<FileEntry>,
) -> Vec<RemoteItem> {
    let mut items = Vec::new();
    let mut pending: Vec<(FileEntry, PathBuf)> = roots.to_vec();
    while let Some((entry, path)) = pending.pop() {
        items.push(RemoteItem {
            id: entry.id.clone(),
            path: path.clone(),
            is_dir: entry.is_folder(),
            content: content_id(&entry),
            size: entry.size,
        });
        if entry.is_folder() {
            let mut kids: Vec<FileEntry> = children(&entry.id)
                .into_iter()
                .filter(is_syncable)
                .collect();
            // A stable order makes numbered duplicates stable between runs.
            kids.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
            for (kid, name) in kids.iter().zip(unique_names(&kids)) {
                pending.push((kid.clone(), path.join(name)));
            }
        }
    }
    items
}

pub fn content_id(entry: &FileEntry) -> ContentId {
    if entry.is_folder() {
        ContentId::Folder
    } else if let Some(md5) = &entry.md5 {
        ContentId::Md5(md5.clone())
    } else {
        ContentId::Revision(entry.head_revision.clone().unwrap_or_default())
    }
}

/// Local names for siblings, numbering repeats: `a.txt`, `a (2).txt`, `a (3).txt`.
fn unique_names(siblings: &[FileEntry]) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    siblings
        .iter()
        .map(|entry| {
            let name = local_name(&entry.name);
            let count = seen.entry(name.to_lowercase()).or_default();
            *count += 1;
            if *count == 1 {
                name
            } else {
                numbered(&name, *count)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use aerie_core::{Capabilities, FileKind};

    use super::*;

    fn entry(id: &str, parent: &str, name: &str, folder: bool) -> FileEntry {
        FileEntry {
            id: DriveFileId::new(id),
            parent: Some(DriveFileId::new(parent)),
            name: name.into(),
            kind: if folder {
                FileKind::Folder
            } else {
                FileKind::Other
            },
            size: (!folder).then_some(3),
            modified: 0,
            mime_type: if folder {
                "application/vnd.google-apps.folder".into()
            } else {
                "text/plain".into()
            },
            can: Capabilities::ALL,
            md5: (!folder).then(|| format!("md5-{id}")),
            head_revision: None,
            drive: None,
        }
    }

    #[test]
    fn maps_selected_folders_with_safe_unique_names() {
        let docs = entry("docs", "root", "Docs", true);
        let all = [
            entry("a", "docs", "a.txt", false),
            entry("b", "docs", "a.txt", false),
            entry("c", "docs", "Q1/Q2", false),
            entry("g", "docs", "Notes", false),
            entry("sub", "docs", "Sub", true),
            entry("d", "sub", "deep.txt", false),
        ];
        let mut native = entry("n", "docs", "Plan", false);
        native.mime_type = "application/vnd.google-apps.document".into();
        let children = |folder: &DriveFileId| -> Vec<FileEntry> {
            all.iter()
                .chain(std::iter::once(&native))
                .filter(|e| e.parent.as_ref() == Some(folder))
                .cloned()
                .collect()
        };
        let mut paths: Vec<_> = remote_items(&[(docs, PathBuf::from("Docs"))], &children)
            .into_iter()
            .map(|item| item.path.to_string_lossy().into_owned())
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                "Docs",
                "Docs/Notes",
                "Docs/Q1∕Q2",
                "Docs/Sub",
                "Docs/Sub/deep.txt",
                "Docs/a (2).txt",
                "Docs/a.txt",
            ]
        );
    }

    #[test]
    fn cleans_unrepresentable_names() {
        assert_eq!(local_name(".."), "_");
        assert_eq!(local_name("a/b"), "a∕b");
        assert_eq!(numbered("report.final.pdf", 2), "report.final (2).pdf");
        assert_eq!(numbered(".hidden", 3), ".hidden (3)");
    }
}

#[cfg(test)]
mod root_path_tests {
    use super::*;

    fn folder(id: &str, parent: Option<&str>, name: &str) -> FileEntry {
        FileEntry {
            id: DriveFileId::new(id),
            parent: parent.map(DriveFileId::new),
            name: name.to_owned(),
            kind: aerie_core::FileKind::Folder,
            size: None,
            modified: 0,
            mime_type: "application/vnd.google-apps.folder".to_owned(),
            md5: None,
            head_revision: None,
            drive: None,
            can: aerie_core::Capabilities::default(),
        }
    }

    #[test]
    fn synced_folders_keep_their_drive_path() {
        let root = DriveFileId::new("root");
        let mut drive = folder("team", None, "Team");
        drive.mime_type = aerie_drive::api::SHARED_DRIVE_MIME.to_owned();
        let all = [
            folder("clients", Some("root"), "Clients"),
            folder("acme", Some("clients"), "Acme/West"),
            drive.clone(),
            folder("plans", Some("team"), "Plans"),
        ];
        let lookup = |id: &DriveFileId| all.iter().find(|e| e.id == *id).cloned();
        let path = |entry: &FileEntry| root_path(entry, Some(&root), &lookup);

        assert_eq!(path(&all[1]), PathBuf::from("My Drive/Clients/Acme∕West"));
        assert_eq!(path(&all[0]), PathBuf::from("My Drive/Clients"));
        assert_eq!(
            path(&folder("root", None, "My Drive")),
            PathBuf::from("My Drive")
        );
        assert_eq!(path(&drive), PathBuf::from("Team"));
        assert_eq!(path(&all[3]), PathBuf::from("Team/Plans"));
        assert_eq!(
            path(&folder("theirs", Some("unknown"), "Theirs")),
            PathBuf::from("Shared with me/Theirs")
        );
        assert_eq!(
            path(&folder("loose", None, "Loose")),
            PathBuf::from("Shared with me/Loose")
        );
    }
}
