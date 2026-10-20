//! Folder navigation and filename search over cached metadata.

use std::cmp::Ordering;

use crate::{Change, DriveFileId, FileEntry};

/// Where the browser reads metadata from: sample data, or the local cache of a Drive.
pub trait Catalog {
    fn children(&self, folder: &DriveFileId) -> Vec<FileEntry>;
    /// Case-insensitive filename matches across the whole Drive.
    fn search(&self, query: &str) -> Vec<FileEntry>;
    fn get(&self, id: &DriveFileId) -> Option<FileEntry>;
}

/// What the browser shows: the open folder path and an optional filename query.
pub struct BrowseState<C> {
    catalog: C,
    root: DriveFileId,
    /// Folders opened below the root, outermost first.
    path: Vec<FileEntry>,
    query: String,
}

impl<C: Catalog> BrowseState<C> {
    pub fn new(root: DriveFileId, catalog: C) -> Self {
        Self {
            catalog,
            root,
            path: Vec::new(),
            query: String::new(),
        }
    }

    pub fn catalog(&self) -> &C {
        &self.catalog
    }

    pub fn catalog_mut(&mut self) -> &mut C {
        &mut self.catalog
    }

    /// Switches to another catalog, such as a newly signed-in Drive, at its root.
    pub fn replace(&mut self, root: DriveFileId, catalog: C) {
        *self = Self::new(root, catalog);
    }

    pub fn root(&self) -> &DriveFileId {
        &self.root
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
        let Some(folder) = self.catalog.get(id).filter(FileEntry::is_folder) else {
            return false;
        };
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

    /// Re-reads the open path after the catalog changed: renamed folders take their new
    /// names, and a folder that no longer exists is left for its parent.
    pub fn revalidate(&mut self) {
        let mut valid = Vec::with_capacity(self.path.len());
        for folder in &self.path {
            match self.catalog.get(&folder.id).filter(FileEntry::is_folder) {
                Some(current) => valid.push(current),
                None => break,
            }
        }
        self.path = valid;
    }

    /// Search results across the catalog, or the current folder's children.
    pub fn visible(&self) -> Vec<FileEntry> {
        if self.is_searching() {
            self.catalog.search(&self.query)
        } else {
            self.catalog.children(self.current_folder())
        }
    }

    fn ancestors(&self, entry: &FileEntry) -> Vec<FileEntry> {
        let mut chain = Vec::new();
        let mut parent = entry.parent.clone();
        while let Some(id) = parent.filter(|id| *id != self.root) {
            let Some(folder) = self.catalog.get(&id) else {
                break;
            };
            parent = folder.parent.clone();
            chain.push(folder);
        }
        chain.reverse();
        chain
    }
}

/// An in-memory catalog, for sample data and tests.
#[derive(Debug, Default)]
pub struct MemoryCatalog {
    entries: Vec<FileEntry>,
}

impl MemoryCatalog {
    pub fn new(entries: Vec<FileEntry>) -> Self {
        Self { entries }
    }

    pub fn apply(&mut self, change: Change) {
        match change {
            Change::Upsert(entry) => match self.entries.iter_mut().find(|e| e.id == entry.id) {
                Some(existing) => *existing = entry,
                None => self.entries.push(entry),
            },
            Change::Removed(id) => self.entries.retain(|e| e.id != id),
        }
    }
}

impl Catalog for MemoryCatalog {
    fn children(&self, folder: &DriveFileId) -> Vec<FileEntry> {
        self.entries
            .iter()
            .filter(|e| e.parent.as_ref() == Some(folder))
            .cloned()
            .collect()
    }

    fn search(&self, query: &str) -> Vec<FileEntry> {
        let query = query.to_lowercase();
        self.entries
            .iter()
            .filter(|e| e.name.to_lowercase().contains(&query))
            .cloned()
            .collect()
    }

    fn get(&self, id: &DriveFileId) -> Option<FileEntry> {
        self.entries.iter().find(|e| &e.id == id).cloned()
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
            mime_type: String::new(),
            md5: None,
            head_revision: None,
            can: crate::Capabilities::ALL,
        }
    }

    fn state() -> BrowseState<MemoryCatalog> {
        BrowseState::new(
            DriveFileId::new("root"),
            MemoryCatalog::new(vec![
                entry("work", "root", "Work", FileKind::Folder),
                entry("plan", "root", "Plan.pdf", FileKind::Pdf),
                entry("contracts", "work", "Contracts", FileKind::Folder),
                entry("nda", "contracts", "NDA.pdf", FileKind::Pdf),
            ]),
        )
    }

    fn names(state: &BrowseState<MemoryCatalog>) -> Vec<String> {
        state.visible().into_iter().map(|e| e.name).collect()
    }

    fn apply(state: &mut BrowseState<MemoryCatalog>, change: Change) {
        state.catalog_mut().apply(change);
        state.revalidate();
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
        apply(
            &mut state,
            Change::Upsert(entry("plan", "work", "Plan v2.pdf", FileKind::Pdf)),
        );
        apply(
            &mut state,
            Change::Upsert(entry("new", "root", "New.pdf", FileKind::Pdf)),
        );
        assert_eq!(names(&state), ["Work", "New.pdf"]);

        state.open(&DriveFileId::new("work"));
        assert_eq!(names(&state), ["Plan v2.pdf", "Contracts"]);
    }

    #[test]
    fn leaves_a_removed_folder() {
        let mut state = state();
        state.open(&DriveFileId::new("work"));
        state.open(&DriveFileId::new("contracts"));
        apply(&mut state, Change::Removed(DriveFileId::new("work")));
        assert_eq!(state.depth(), 0);
        assert_eq!(names(&state), ["Plan.pdf"]);
    }

    #[test]
    fn renames_an_open_folder() {
        let mut state = state();
        state.open(&DriveFileId::new("work"));
        apply(
            &mut state,
            Change::Upsert(entry("work", "root", "Projects", FileKind::Folder)),
        );
        assert_eq!(state.path_names().collect::<Vec<_>>(), ["Projects"]);
    }

    #[test]
    fn compares_names_case_insensitively() {
        assert_eq!(compare_names("apple", "Banana"), Ordering::Less);
        assert_eq!(compare_names("B", "b"), Ordering::Less);
    }
}
