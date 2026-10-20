//! File metadata as presented by the browser.

use crate::DriveFileId;

/// Broad file category, used for icons and sorting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Folder,
    Document,
    Spreadsheet,
    Presentation,
    Pdf,
    Image,
    Other,
}

/// Cached metadata for one Drive item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub id: DriveFileId,
    /// Containing folder; `None` only for the My Drive root.
    pub parent: Option<DriveFileId>,
    pub name: String,
    pub kind: FileKind,
    /// Content size in bytes; `None` for folders and Google-native files.
    pub size: Option<u64>,
    /// Last modification time in Unix seconds.
    pub modified: i64,
}

impl FileEntry {
    pub fn is_folder(&self) -> bool {
        self.kind == FileKind::Folder
    }
}

/// One remote change, as reported by Drive's change log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// The item was created, edited, renamed or moved.
    Upsert(FileEntry),
    /// The item was trashed, deleted or became inaccessible.
    Removed(DriveFileId),
}

/// Formats a byte count with decimal units, matching how Google Drive reports sizes.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];

    match bytes {
        1 => return "1 byte".to_owned(),
        0..1000 => return format!("{bytes} bytes"),
        _ => {}
    }

    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    // Promote before rounding so 999,999 bytes reads "1.0 MB", not "1000 KB".
    while value >= 999.5 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }

    if value < 9.95 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_small_sizes_in_bytes() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(1), "1 byte");
        assert_eq!(format_size(999), "999 bytes");
    }

    #[test]
    fn formats_larger_sizes_with_decimal_units() {
        assert_eq!(format_size(1_500), "1.5 KB");
        assert_eq!(format_size(12_000_000), "12 MB");
        assert_eq!(format_size(9_960_000), "10 MB");
        assert_eq!(format_size(999_999), "1.0 MB");
        assert_eq!(format_size(3_200_000_000_000), "3.2 TB");
    }
}
