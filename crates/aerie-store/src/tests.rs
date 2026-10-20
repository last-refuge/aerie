use super::*;

const ACCOUNT: &str = "perm-1";

fn entry(id: &str, parent: &str, name: &str, kind: FileKind, modified: i64) -> FileEntry {
    FileEntry {
        id: DriveFileId::new(id),
        parent: Some(DriveFileId::new(parent)),
        name: name.to_owned(),
        kind,
        size: (kind != FileKind::Folder).then_some(1_000),
        modified,
        mime_type: "application/pdf".into(),
        md5: None,
        head_revision: None,
        can: aerie_core::Capabilities {
            rename: true,
            ..Default::default()
        },
    }
}

fn store() -> Store {
    let store = Store::open_in_memory().unwrap();
    store
        .save_account(&Account {
            id: ACCOUNT.into(),
            email: "test@example.com".into(),
            display_name: "Test".into(),
        })
        .unwrap();
    store
}

fn inventoried(entries: &[FileEntry]) -> Store {
    let mut store = store();
    store
        .begin_inventory(ACCOUNT, &DriveFileId::new("root"), "cursor-1")
        .unwrap();
    store.save_inventory_page(ACCOUNT, entries).unwrap();
    store.finish_inventory(ACCOUNT).unwrap();
    store
}

fn names(entries: Vec<FileEntry>) -> Vec<String> {
    let mut names: Vec<_> = entries.into_iter().map(|e| e.name).collect();
    names.sort();
    names
}

#[test]
fn migrates_to_the_latest_schema_once() {
    let dir = std::env::temp_dir().join(format!("aerie-store-{}", std::process::id()));
    let path = dir.join("nested/aerie.sqlite3");
    let first = Store::open(&path).unwrap();
    assert_eq!(first.schema_version().unwrap(), 5);
    drop(first);
    assert_eq!(Store::open(&path).unwrap().schema_version().unwrap(), 5);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn refuses_a_newer_schema() {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    assert!(matches!(
        Store::prepare(conn),
        Err(Error::NewerSchema { found: 99, .. })
    ));
}

#[test]
fn stores_and_reads_an_inventory() {
    let store = inventoried(&[
        entry("work", "root", "Work", FileKind::Folder, 10),
        entry("plan", "work", "Plan.pdf", FileKind::Pdf, 20),
    ]);
    assert!(store.inventory_complete(ACCOUNT).unwrap());
    assert_eq!(
        store.root_id(ACCOUNT).unwrap(),
        Some(DriveFileId::new("root"))
    );
    assert_eq!(
        store.change_cursor(ACCOUNT).unwrap().as_deref(),
        Some("cursor-1")
    );
    assert_eq!(
        names(store.children(ACCOUNT, &DriveFileId::new("work")).unwrap()),
        ["Plan.pdf"]
    );

    let plan = store
        .file(ACCOUNT, &DriveFileId::new("plan"))
        .unwrap()
        .unwrap();
    assert_eq!(plan.kind, FileKind::Pdf);
    assert_eq!(plan.size, Some(1_000));
}

#[test]
fn an_interrupted_inventory_deletes_nothing() {
    let mut store = inventoried(&[
        entry("a", "root", "A", FileKind::Pdf, 1),
        entry("b", "root", "B", FileKind::Pdf, 1),
    ]);
    store
        .begin_inventory(ACCOUNT, &DriveFileId::new("root"), "cursor-2")
        .unwrap();
    store
        .save_inventory_page(ACCOUNT, &[entry("a", "root", "A", FileKind::Pdf, 1)])
        .unwrap();
    // No finish_inventory: the listing was cut short.
    assert!(!store.inventory_complete(ACCOUNT).unwrap());
    assert_eq!(
        names(store.children(ACCOUNT, &DriveFileId::new("root")).unwrap()),
        ["A", "B"]
    );
}

#[test]
fn a_finished_inventory_drops_what_it_did_not_see() {
    let mut store = inventoried(&[
        entry("a", "root", "A", FileKind::Pdf, 1),
        entry("b", "root", "B", FileKind::Pdf, 1),
    ]);
    store
        .begin_inventory(ACCOUNT, &DriveFileId::new("root"), "cursor-2")
        .unwrap();
    store
        .save_inventory_page(ACCOUNT, &[entry("a", "root", "A", FileKind::Pdf, 1)])
        .unwrap();
    store.finish_inventory(ACCOUNT).unwrap();
    assert_eq!(
        names(store.children(ACCOUNT, &DriveFileId::new("root")).unwrap()),
        ["A"]
    );
}

#[test]
fn applies_changes_with_their_cursor() {
    let mut store = inventoried(&[
        entry("a", "root", "A", FileKind::Pdf, 1),
        entry("b", "root", "B", FileKind::Pdf, 1),
    ]);
    store
        .apply_changes(
            ACCOUNT,
            &[
                Change::Upsert(entry("a", "root", "A renamed", FileKind::Pdf, 2)),
                Change::Removed(DriveFileId::new("b")),
                Change::Upsert(entry("c", "root", "C", FileKind::Image, 3)),
            ],
            "cursor-2",
        )
        .unwrap();
    assert_eq!(
        names(store.children(ACCOUNT, &DriveFileId::new("root")).unwrap()),
        ["A renamed", "C"]
    );
    assert_eq!(
        store.change_cursor(ACCOUNT).unwrap().as_deref(),
        Some("cursor-2")
    );
}

#[test]
fn searches_names_case_insensitively_and_literally() {
    let store = inventoried(&[
        entry("a", "root", "Budget 2026.xlsx", FileKind::Spreadsheet, 1),
        entry("b", "x", "old budget.xlsx", FileKind::Spreadsheet, 5),
        entry("c", "root", "100% done.txt", FileKind::Other, 3),
        entry("d", "root", "Notes", FileKind::Document, 4),
    ]);
    let results = store.search(ACCOUNT, "  BUDGET ", 10).unwrap();
    let ordered: Vec<_> = results.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(ordered, ["old budget.xlsx", "Budget 2026.xlsx"]);
    assert_eq!(
        names(store.search(ACCOUNT, "%", 10).unwrap()),
        ["100% done.txt"]
    );
    assert_eq!(store.search(ACCOUNT, "e", 1).unwrap().len(), 1);
}

#[test]
fn infers_the_root_from_orphaned_parents() {
    let store = inventoried(&[
        entry("a", "real-root", "A", FileKind::Folder, 1),
        entry("b", "real-root", "B", FileKind::Pdf, 1),
        entry("c", "a", "C", FileKind::Pdf, 1),
        entry("d", "shared-elsewhere", "D", FileKind::Pdf, 1),
    ]);
    assert_eq!(
        store.likely_root(ACCOUNT).unwrap(),
        Some(DriveFileId::new("real-root"))
    );
}

#[test]
fn lists_shared_drives() {
    let mut team = entry("d1", "x", "Team", FileKind::Folder, 1);
    team.parent = None;
    team.mime_type = "application/vnd.aerie.shared-drive".into();
    // Items shared with the user can lack a parent too; they aren't drives.
    let mut shared = entry("s1", "x", "Shared folder", FileKind::Folder, 1);
    shared.parent = None;
    let store = inventoried(&[
        team,
        shared,
        entry("a", "root", "Mine", FileKind::Folder, 1),
        entry("b", "d1", "Plan.pdf", FileKind::Pdf, 1),
    ]);
    let drives: Vec<_> = store
        .shared_drives(ACCOUNT)
        .unwrap()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(drives, ["Team"]);
    assert_eq!(
        names(store.children(ACCOUNT, &DriveFileId::new("d1")).unwrap()),
        ["Plan.pdf"]
    );
}

#[test]
fn forgetting_an_account_removes_its_cache() {
    let store = inventoried(&[entry("a", "root", "A", FileKind::Pdf, 1)]);
    store.forget_account(ACCOUNT).unwrap();
    assert_eq!(store.account().unwrap(), None);
    assert!(
        store
            .children(ACCOUNT, &DriveFileId::new("root"))
            .unwrap()
            .is_empty()
    );
}

/// Plan target: cached filename search p95 under 150 ms over 100,000 entries.
/// Run with `cargo test --release -p aerie-store -- --ignored`.
#[test]
#[ignore = "performance check"]
fn searches_100k_entries_quickly() {
    let entries: Vec<_> = (0..100_000)
        .map(|i| {
            entry(
                &format!("f{i}"),
                "root",
                &format!("Report {i} final.pdf"),
                FileKind::Pdf,
                i,
            )
        })
        .collect();
    let store = inventoried(&entries);
    let mut timings: Vec<_> = (0..40)
        .map(|i| {
            let started = std::time::Instant::now();
            store
                .search(ACCOUNT, &format!("{}", i * 2_311), 200)
                .unwrap();
            started.elapsed()
        })
        .collect();
    timings.sort();
    let p95 = timings[timings.len() * 95 / 100];
    println!("search p95 over 100k entries: {p95:?}");
    assert!(p95 < std::time::Duration::from_millis(150));
}
