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
    /// Drive's MIME type; Google-native documents use `application/vnd.google-apps.*`.
    pub mime_type: String,
    /// What the signed-in user may do with the item.
    pub can: Capabilities,
    /// Content checksum; Drive reports none for folders and Google-native documents.
    pub md5: Option<String>,
    /// Drive's current revision of the content.
    pub head_revision: Option<String>,
}

/// Per-item permissions reported by Drive. Actions are offered only when allowed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Capabilities {
    pub rename: bool,
    pub trash: bool,
    pub add_children: bool,
    pub move_item: bool,
    pub download: bool,
    pub edit: bool,
}

impl Capabilities {
    pub const ALL: Self = Self {
        rename: true,
        trash: true,
        add_children: true,
        move_item: true,
        download: true,
        edit: true,
    };

    /// Packs the flags for storage.
    pub fn bits(self) -> i64 {
        [
            self.rename,
            self.trash,
            self.add_children,
            self.move_item,
            self.download,
            self.edit,
        ]
        .iter()
        .enumerate()
        .fold(0, |bits, (index, &set)| bits | (i64::from(set) << index))
    }

    pub fn from_bits(bits: i64) -> Self {
        let set = |index: u32| bits & (1 << index) != 0;
        Self {
            rename: set(0),
            trash: set(1),
            add_children: set(2),
            move_item: set(3),
            download: set(4),
            edit: set(5),
        }
    }
}

impl FileEntry {
    pub fn is_folder(&self) -> bool {
        self.kind == FileKind::Folder
    }

    /// Docs, Sheets, Slides and similar, which download only as exports.
    pub fn is_google_native(&self) -> bool {
        self.mime_type.starts_with("application/vnd.google-apps.") && !self.is_folder()
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
    fn packs_capabilities() {
        let can = Capabilities {
            rename: true,
            move_item: true,
            ..Capabilities::default()
        };
        assert_eq!(Capabilities::from_bits(can.bits()), can);
        assert_eq!(
            Capabilities::from_bits(Capabilities::ALL.bits()),
            Capabilities::ALL
        );
    }

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
