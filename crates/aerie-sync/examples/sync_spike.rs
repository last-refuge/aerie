//! Live sync scenarios on a real account, confined by the `drive.file` scope to files
//! Aerie creates, inside My Drive › Aerie test. The local side is a scratch folder.
//!
//! cargo run -p aerie-sync --example sync_spike

use std::path::{Path, PathBuf};

use aerie_core::DriveFileId;
use aerie_drive::api::DriveApi;
use aerie_drive::auth::{self, DRIVE_FILE};
use aerie_drive::client::OAuthClient;
use aerie_drive::upload::{CHUNK_UNIT, NewFile, RetryPolicy};
use aerie_drive::{credentials, http_client};
use aerie_store::{Account, Store, SyncSettings};
use aerie_sync::cycle;

type Error = Box<dyn std::error::Error>;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let client = OAuthClient::load(&OAuthClient::default_path())?;
    let http = http_client()?;
    let refresh = credentials::load_refresh_token(DRIVE_FILE)
        .await?
        .ok_or("run the write spike once first to grant drive.file access")?;
    let token = auth::refresh(&client, &http, &refresh).await?.access_token;
    let drive = DriveApi::new(http, token);

    let scratch = std::env::temp_dir().join(format!("aerie-sync-spike-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let root = scratch.join("Aerie");
    std::fs::create_dir_all(&root)?;
    let quarantine = scratch.join("quarantine");
    let mut store = Store::open(&scratch.join("aerie.sqlite3"))?;
    let me = drive.account().await?;
    let account = me.permission_id.clone();
    store.save_account(&Account {
        id: account.clone(),
        email: me.email_address,
        display_name: me.display_name,
    })?;

    // A fresh Drive folder seeded with a file and a subfolder.
    let sandbox = drive
        .find_folder(&DriveFileId::new("root"), "Aerie test")
        .await?
        .ok_or("the Aerie test folder is missing")?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let synced = drive
        .create_folder(&sandbox.id, &format!("sync-test-{stamp}"))
        .await?;
    let sub = drive.create_folder(&synced.id, "Sub").await?;
    let seed = upload(&drive, &sub.id, "seed.txt", b"seeded on Drive").await?;
    store.set_sync_roots(&account, std::slice::from_ref(&synced.id), &[])?;
    store.save_sync_settings(
        &account,
        &SyncSettings {
            local_root: root.clone(),
            paused: false,
        },
    )?;
    let top = root.join(&synced.name);

    let pass = |label: &str| println!("\n{label}");

    pass("1. First sync downloads the folder");
    run(&drive, &mut store, &account, &root, &quarantine).await?;
    check(
        read(&top.join("Sub/seed.txt")) == "seeded on Drive",
        "seed downloaded",
    )?;

    pass("2. Local edit, new local file and local rename go up");
    std::fs::write(top.join("Sub/seed.txt"), "edited locally")?;
    std::fs::write(top.join("Sub/new-local.txt"), "made locally")?;
    std::fs::create_dir_all(top.join("Sub/Inner"))?;
    std::fs::write(top.join("Sub/Inner/deep.txt"), "deep")?;
    run(&drive, &mut store, &account, &root, &quarantine).await?;
    check(
        String::from_utf8(drive.download(&seed.id).await?)? == "edited locally",
        "edit uploaded",
    )?;
    check(
        drive.find_folder(&sub.id, "Inner").await?.is_some(),
        "new folder created on Drive",
    )?;
    std::fs::rename(
        top.join("Sub/new-local.txt"),
        top.join("Sub/renamed-local.txt"),
    )?;
    run(&drive, &mut store, &account, &root, &quarantine).await?;
    check(
        drive
            .list_children(&sub.id)
            .await?
            .iter()
            .any(|e| e.name == "renamed-local.txt"),
        "local rename followed on Drive",
    )?;

    pass("3. Drive edits and renames come down");
    replace(&drive, &seed.id, b"edited on Drive").await?;
    drive.rename(&seed.id, "seed-renamed.txt").await?;
    run(&drive, &mut store, &account, &root, &quarantine).await?;
    check(
        read(&top.join("Sub/seed-renamed.txt")) == "edited on Drive",
        "Drive edit and rename applied locally",
    )?;
    check(!top.join("Sub/seed.txt").exists(), "old local name gone")?;

    pass("4. Edits on both sides keep both versions");
    replace(&drive, &seed.id, b"Drive side of a conflict").await?;
    std::fs::write(top.join("Sub/seed-renamed.txt"), "local side of a conflict")?;
    let report = run(&drive, &mut store, &account, &root, &quarantine).await?;
    check(report.conflicts.len() == 1, "one conflict copy made")?;
    let copy = top.join(&report.conflicts[0]).canonicalize().ok();
    let copy = copy
        .or_else(|| Some(root.join(&report.conflicts[0])))
        .unwrap();
    check(
        read(&copy) == "Drive side of a conflict",
        "Drive's version kept locally",
    )?;
    check(
        String::from_utf8(drive.download(&seed.id).await?)? == "local side of a conflict",
        "local version uploaded",
    )?;
    run(&drive, &mut store, &account, &root, &quarantine).await?;
    check(
        drive
            .list_children(&sub.id)
            .await?
            .iter()
            .any(|e| e.name.contains("conflicted copy")),
        "conflict copy also on Drive",
    )?;

    pass("5. Deletions go to the trash on the other side");
    std::fs::remove_file(top.join("Sub/renamed-local.txt"))?;
    run(&drive, &mut store, &account, &root, &quarantine).await?;
    check(
        !drive
            .list_children(&sub.id)
            .await?
            .iter()
            .any(|e| e.name == "renamed-local.txt"),
        "local deletion trashed on Drive",
    )?;
    let deep = drive
        .list_children(&drive.find_folder(&sub.id, "Inner").await?.unwrap().id)
        .await?;
    drive.trash(&deep[0].id).await?;
    run(&drive, &mut store, &account, &root, &quarantine).await?;
    check(
        !top.join("Sub/Inner/deep.txt").exists(),
        "Drive deletion removed locally",
    )?;

    pass("6. With nothing changed, a pass does nothing");
    let report = run(&drive, &mut store, &account, &root, &quarantine).await?;
    check(
        report.planned == 0,
        "no feedback loop from Aerie's own writes",
    )?;

    pass("7. A vanished sync folder stops sync instead of deleting");
    std::fs::rename(&root, scratch.join("moved-away"))?;
    let result = run(&drive, &mut store, &account, &root, &quarantine).await;
    check(result.is_err(), "pass refused to run")?;
    check(
        drive
            .list_children(&sub.id)
            .await?
            .iter()
            .any(|e| e.name == "seed-renamed.txt"),
        "nothing deleted on Drive",
    )?;

    println!(
        "\nAll scenarios passed. Test files are in My Drive › Aerie test › {}",
        synced.name
    );
    std::fs::remove_dir_all(&scratch)?;
    Ok(())
}

/// Brings the metadata cache up to date, then runs one sync pass.
async fn run(
    drive: &DriveApi,
    store: &mut Store,
    account: &str,
    root: &Path,
    quarantine: &Path,
) -> Result<cycle::Report, Error> {
    let cursor = drive
        .start_page_token(&aerie_drive::api::Feed::Everything)
        .await?;
    store.begin_inventory(account, &DriveFileId::new("root"), &cursor)?;
    let mut page = None;
    loop {
        let (entries, next) = drive
            .inventory_page(&aerie_drive::api::Corpus::User, page.as_deref())
            .await?;
        store.save_inventory_page(account, &entries)?;
        match next {
            Some(token) => page = Some(token),
            None => break,
        }
    }
    store.finish_inventory(account)?;
    let report = cycle::run(
        drive,
        store,
        account,
        root,
        quarantine,
        cycle::Deletions::Hold,
        |_| {},
        |_| {},
    )
    .await?;
    println!(
        "  pass: {} steps, {} applied, {} conflicts, {} failed",
        report.planned,
        report.applied,
        report.conflicts.len(),
        report.failed.len()
    );
    for (path, err) in &report.failed {
        println!("    failed {path:?}: {err}");
    }
    Ok(report)
}

async fn upload(
    drive: &DriveApi,
    parent: &DriveFileId,
    name: &str,
    content: &[u8],
) -> Result<aerie_core::FileEntry, Error> {
    let session = drive
        .start_upload(
            &NewFile {
                name,
                parent,
                mime_type: "text/plain",
            },
            content.len() as u64,
        )
        .await?;
    Ok(drive
        .upload(
            &session,
            &mut std::io::Cursor::new(content),
            CHUNK_UNIT,
            RetryPolicy::default(),
        )
        .await?)
}

async fn replace(drive: &DriveApi, id: &DriveFileId, content: &[u8]) -> Result<(), Error> {
    let session = drive
        .start_replace(id, "text/plain", content.len() as u64)
        .await?;
    drive
        .upload(
            &session,
            &mut std::io::Cursor::new(content),
            CHUNK_UNIT,
            RetryPolicy::default(),
        )
        .await?;
    Ok(())
}

fn read(path: &PathBuf) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn check(ok: bool, what: &str) -> Result<(), Error> {
    println!("  {} {what}", if ok { "✓" } else { "✗" });
    if ok {
        Ok(())
    } else {
        Err(format!("failed: {what}").into())
    }
}
