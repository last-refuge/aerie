//! Scanning the local sync root. Any read error aborts the whole scan: a folder that
//! couldn't be listed must never look like a folder whose files were deleted.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::model::LocalItem;

/// Names Aerie never syncs, in either direction: its own partial downloads, common
/// editor temporaries, and the folder metadata macOS and Windows scatter about. Copies
/// of `.DS_Store` ("…(conflicted copy …)", "… (2)") are junk too.
pub fn is_ignored(name: &str) -> bool {
    name.ends_with(".aerie-partial")
        || name.starts_with(".~lock.")
        || name.starts_with("~$")
        || name.starts_with(".goutputstream-")
        || name.ends_with(".swp")
        || name.ends_with('~')
        || name.starts_with(".DS_Store")
        || name.starts_with("._")
        || name.eq_ignore_ascii_case("Thumbs.db")
        || name.eq_ignore_ascii_case("desktop.ini")
}

/// Lists everything under `root` relative to it, skipping symbolic links (which aren't
/// followed in this release) and ignored names.
pub fn scan(root: &Path) -> io::Result<Vec<LocalItem>> {
    let meta = std::fs::metadata(root)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "the sync folder isn't a folder",
        ));
    }
    let mut items = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for entry in std::fs::read_dir(root.join(&relative))? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                // Names that aren't valid UTF-8 can't be represented on Drive.
                continue;
            };
            if is_ignored(name_str) {
                continue;
            }
            let meta = entry.metadata()?;
            if meta.file_type().is_symlink() || !(meta.is_dir() || meta.is_file()) {
                continue;
            }
            let path = relative.join(&name);
            items.push(LocalItem {
                path: path.clone(),
                is_dir: meta.is_dir(),
                size: if meta.is_dir() { 0 } else { meta.len() },
                mtime: meta.mtime() * 1_000_000_000 + meta.mtime_nsec(),
                inode: (meta.dev(), meta.ino()),
            });
            if meta.is_dir() {
                pending.push(path);
            }
        }
    }
    items.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(items)
}

/// Reads one item, for recording the baseline right after Aerie wrote it.
pub fn stat(root: &Path, relative: &Path) -> io::Result<LocalItem> {
    let meta = std::fs::symlink_metadata(root.join(relative))?;
    Ok(LocalItem {
        path: relative.to_owned(),
        is_dir: meta.is_dir(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        mtime: meta.mtime() * 1_000_000_000 + meta.mtime_nsec(),
        inode: (meta.dev(), meta.ino()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_files_and_folders_but_not_links_or_temporaries() {
        let root = std::env::temp_dir().join(format!("aerie-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Docs/Inner")).unwrap();
        std::fs::write(root.join("Docs/a.txt"), b"hello").unwrap();
        std::fs::write(root.join("Docs/.a.txt.aerie-partial"), b"x").unwrap();
        std::fs::write(root.join("Docs/a.txt~"), b"x").unwrap();
        std::os::unix::fs::symlink("/etc", root.join("Docs/link")).unwrap();

        let items = scan(&root).unwrap();
        let paths: Vec<_> = items
            .iter()
            .map(|i| i.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, ["Docs", "Docs/Inner", "Docs/a.txt"]);
        let file = &items[2];
        assert_eq!(file.size, 5);
        assert_eq!(stat(&root, Path::new("Docs/a.txt")).unwrap(), *file);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_root_is_an_error_not_an_empty_folder() {
        assert!(scan(Path::new("/nonexistent/aerie-root")).is_err());
    }
}
