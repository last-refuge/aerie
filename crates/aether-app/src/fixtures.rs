//! Sample Drive contents shown until the Drive adapter is connected.

use aether_core::{DriveFileId, FileEntry, FileKind};

pub const ROOT_ID: &str = "root";

const OCT_1_2026: i64 = 1_790_812_800;
const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;

/// One fixture row: id, parent, name, kind, size, day of October 2026, hour.
type Sample = (
    &'static str,
    &'static str,
    &'static str,
    FileKind,
    Option<u64>,
    i64,
    i64,
);

#[rustfmt::skip]
const SAMPLES: &[Sample] = &[
    ("work", ROOT_ID, "Work", FileKind::Folder, None, 19, 9),
    ("personal", ROOT_ID, "Personal", FileKind::Folder, None, 18, 20),
    ("design", ROOT_ID, "Design", FileKind::Folder, None, 18, 14),
    ("deck", ROOT_ID, "Aether Presentation.pptx", FileKind::Presentation, Some(12_400_000), 17, 16),
    ("plan", ROOT_ID, "Project Plan.pdf", FileKind::Pdf, Some(8_100_000), 17, 11),
    ("shot", ROOT_ID, "Screenshot.png", FileKind::Image, Some(2_050_000), 17, 10),
    ("budget", ROOT_ID, "Budget 2026.xlsx", FileKind::Spreadsheet, Some(184_000), 12, 15),
    ("notes", ROOT_ID, "notes.txt", FileKind::Other, Some(3_400), 6, 8),
    ("roadmap", "work", "Q4 Roadmap.docx", FileKind::Document, Some(96_000), 19, 9),
    ("contracts", "work", "Contracts", FileKind::Folder, None, 14, 13),
    ("nda", "contracts", "Mutual NDA.pdf", FileKind::Pdf, Some(412_000), 14, 13),
    ("taxes", "personal", "Taxes 2025.pdf", FileKind::Pdf, Some(1_250_000), 18, 20),
    ("photos", "personal", "Photos", FileKind::Folder, None, 2, 18),
    ("logo", "design", "Logo.svg", FileKind::Image, Some(28_000), 18, 14),
    ("mockups", "design", "Mockups", FileKind::Folder, None, 16, 12),
    ("browser", "mockups", "Browser.png", FileKind::Image, Some(3_800_000), 16, 12),
];

pub fn sample_entries() -> Vec<FileEntry> {
    SAMPLES
        .iter()
        .map(|&(id, parent, name, kind, size, day, hour)| FileEntry {
            id: DriveFileId::new(id),
            parent: Some(DriveFileId::new(parent)),
            name: name.to_owned(),
            kind,
            size,
            modified: OCT_1_2026 + (day - 1) * DAY + hour * HOUR,
        })
        .collect()
}
