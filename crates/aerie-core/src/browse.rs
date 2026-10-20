//! Folder navigation and filename search over cached metadata.

use std::cmp::Ordering;

use crate::{Change, DriveFileId, FileEntry};

/// What the browser shows: the open folder path and an optional filename query.
#[derive(Debug)]
pub struct BrowseState {
    root: DriveFileId,
    entries: Vec<FileEntry>,
    /// Folders opened below the root, outermost first.
    path: Vec<FileEntry>,
    query: String,
}

impl BrowseState {
    pub fn new(root: DriveFileId, entries: Vec<FileEntry>) -> Self {
        Self {
            root,
            entries,
            path: Vec::new(),
            query: String::new(),
        }
    }

    pub fn current_folder(&self) -> &DriveFileId {
        self.path.last().map_or(&self.root, |folder| &folder.id)
    }

    /// Names of the open folders below the root, outermost first.
    pub fn path_names(&self) -> impl Iterator<Item = &str> {
        self.path.iter().map(|folder| folder.name.as_str())
    }

    pub fn depth(&self) -> usize {
        self.path.len()
    }

    pub fn is_searching(&self) -> bool {
        !self.query.is_empty()
    }

    /// Opens a folder in the current listing or search results. Returns false for files.
    pub fn open(&mut self, id: &DriveFileId) -> bool {
        let Some(folder) = self.entries.iter().find(|e| &e.id == id && e.is_folder()) else {
            return false;
        };
        let folder = folder.clone();
        if self.is_searching() {
            // A result can live anywhere, so rebuild the path from its ancestors.
            self.path = self.ancestors(&folder);
        }
        self.path.push(folder);
        self.query.clear();
        true
    }

    /// Returns to the folder at `depth`, where 0 is the root.
    pub fn go_to_depth(&mut self, depth: usize) {
        self.path.truncate(depth);
        self.query.clear();
    }

    /// Sets the filename query; surrounding whitespace is ignored.
    pub fn set_query(&mut self, query: &str) {
        self.query = query.trim().to_lowercase();
    }

    pub fn add(&mut self, entry: FileEntry) {
        self.entries.push(entry);
    }

    /// Applies a remote change to the cached entries and the open path.
    pub fn apply(&mut self, change: Change) {
        match change {
            Change::Upsert(entry) => {
                for folder in self.path.iter_mut().filter(|f| f.id == entry.id) {
                    *folder = entry.clone();
                }
                match self.entries.iter_mut().find(|e| e.id == entry.id) {
                    Some(existing) => *existing = entry,
                    None => self.entries.push(entry),
                }
            }
            Change::Removed(id) => {
                self.entries.retain(|e| e.id != id);
                // Leave a folder that no longer exists, returning to its parent.
                if let Some(depth) = self.path.iter().position(|f| f.id == id) {
                    self.path.truncate(depth);
                }
            }
        }
    }

    /// Search results across all cached entries, or the current folder's children.
    pub fn visible(&self) -> Vec<&FileEntry> {
        let folder = self.current_folder();
        self.entries
            .iter()
            .filter(|entry| {
                if self.is_searching() {
                    entry.name.to_lowercase().contains(&self.query)
                } else {
                    entry.parent.as_ref() == Some(folder)
                }
            })
            .collect()
    }

    fn ancestors(&self, entry: &FileEntry) -> Vec<FileEntry> {
        let mut chain = Vec::new();
        let mut parent = entry.parent.as_ref();
        while let Some(id) = parent.filter(|id| **id != self.root) {
            let Some(folder) = self.entries.iter().find(|e| &e.id == id) else {
                break;
            };
            chain.push(folder.clone());
            parent = folder.parent.as_ref();
        }
        chain.reverse();
        chain
    }
}

/// Orders names case-insensitively, falling back to exact order for stability.
pub fn compare_names(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileKind;

    fn entry(id: &str, parent: &str, name: &str, kind: FileKind) -> FileEntry {
        FileEntry {
            id: DriveFileId::new(id),
            parent: Some(DriveFileId::new(parent)),
            name: name.to_owned(),
            kind,
            size: None,
            modified: 0,
        }
    }

    fn state() -> BrowseState {
        BrowseState::new(
            DriveFileId::new("root"),
            vec![
                entry("work", "root", "Work", FileKind::Folder),
                entry("plan", "root", "Plan.pdf", FileKind::Pdf),
                entry("contracts", "work", "Contracts", FileKind::Folder),
                entry("nda", "contracts", "NDA.pdf", FileKind::Pdf),
            ],
        )
    }

    fn names(state: &BrowseState) -> Vec<&str> {
        state.visible().iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn lists_children_of_the_open_folder() {
        let mut state = state();
        assert_eq!(names(&state), ["Work", "Plan.pdf"]);

        assert!(state.open(&DriveFileId::new("work")));
        assert!(state.open(&DriveFileId::new("contracts")));
        assert_eq!(names(&state), ["NDA.pdf"]);
        assert_eq!(
            state.path_names().collect::<Vec<_>>(),
            ["Work", "Contracts"]
        );

        state.go_to_depth(1);
        assert_eq!(names(&state), ["Contracts"]);
    }

    #[test]
    fn opening_a_file_does_nothing() {
        let mut state = state();
        assert!(!state.open(&DriveFileId::new("plan")));
        assert_eq!(state.depth(), 0);
    }

    #[test]
    fn searches_all_folders_case_insensitively() {
        let mut state = state();
        state.open(&DriveFileId::new("work"));
        state.set_query("  pDf ");
        assert_eq!(names(&state), ["Plan.pdf", "NDA.pdf"]);

        state.set_query("");
        assert_eq!(names(&state), ["Contracts"]);
    }

    #[test]
    fn opening_a_search_result_restores_its_path() {
        let mut state = state();
        state.set_query("contracts");
        assert!(state.open(&DriveFileId::new("contracts")));
        assert!(!state.is_searching());
        assert_eq!(
            state.path_names().collect::<Vec<_>>(),
            ["Work", "Contracts"]
        );
    }

    #[test]
    fn applies_remote_changes() {
        let mut state = state();
        state.apply(Change::Upsert(entry(
            "plan",
            "work",
            "Plan v2.pdf",
            FileKind::Pdf,
        )));
        state.apply(Change::Upsert(entry(
            "new",
            "root",
            "New.pdf",
            FileKind::Pdf,
        )));
        assert_eq!(names(&state), ["Work", "New.pdf"]);

        state.open(&DriveFileId::new("work"));
        assert_eq!(names(&state), ["Plan v2.pdf", "Contracts"]);
    }

    #[test]
    fn leaves_a_removed_folder() {
        let mut state = state();
        state.open(&DriveFileId::new("work"));
        state.open(&DriveFileId::new("contracts"));
        state.apply(Change::Removed(DriveFileId::new("work")));
        assert_eq!(state.depth(), 0);
        assert_eq!(names(&state), ["Plan.pdf"]);
    }

    #[test]
    fn renames_an_open_folder() {
        let mut state = state();
        state.open(&DriveFileId::new("work"));
        state.apply(Change::Upsert(entry(
            "work",
            "root",
            "Projects",
            FileKind::Folder,
        )));
        assert_eq!(state.path_names().collect::<Vec<_>>(), ["Projects"]);
    }

    #[test]
    fn compares_names_case_insensitively() {
        assert_eq!(compare_names("apple", "Banana"), Ordering::Less);
        assert_eq!(compare_names("B", "b"), Ordering::Less);
    }
}
