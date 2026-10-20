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

fn numbered(name: &str, n: usize) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} ({n}){}", &name[..dot], &name[dot..]),
        _ => format!("{name} ({n})"),
    }
}

/// Places every syncable item under the selected `roots`. Each root becomes a top-level
/// folder named after it. `children` lists a folder's cached children.
pub fn remote_items(
    roots: &[FileEntry],
    children: &dyn Fn(&DriveFileId) -> Vec<FileEntry>,
) -> Vec<RemoteItem> {
    let mut items = Vec::new();
    let mut pending: Vec<(FileEntry, PathBuf)> = Vec::new();
    for (root, name) in roots.iter().zip(unique_names(roots)) {
        pending.push((root.clone(), PathBuf::from(name)));
    }
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
        let mut paths: Vec<_> = remote_items(&[docs], &children)
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
