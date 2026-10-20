//! Shared domain models and commands, independent of GTK and HTTP.

mod browse;
mod files;
pub mod redact;

use std::fmt;

pub use browse::{BrowseState, Catalog, MemoryCatalog, compare_names};
pub use files::{Capabilities, Change, FileEntry, FileKind, format_size};

/// Stable Google Drive file identifier. Records are keyed by this, never by display name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DriveFileId(String);

impl DriveFileId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DriveFileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_file_id_round_trips() {
        let id = DriveFileId::new("1AbC");
        assert_eq!(id.as_str(), "1AbC");
        assert_eq!(id.to_string(), "1AbC");
    }
}
