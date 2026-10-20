//! Phase 0 spike: live write tests in a sandbox. Signs in with `drive.file`, which only
//! reaches files Aerie creates, and works inside an "Aerie test" folder in My Drive.
//!
//! cargo run -p aerie-drive --example write_spike

use std::io::Cursor;
use std::time::Instant;

use aerie_core::DriveFileId;
use aerie_drive::api::{DriveApi, intervening_revisions};
use aerie_drive::auth::{self, DRIVE_FILE};
use aerie_drive::client::OAuthClient;
use aerie_drive::upload::{CHUNK_UNIT, NewFile, RetryPolicy, UploadProgress, UploadSession};
use aerie_drive::{credentials, http_client};

type Error = Box<dyn std::error::Error>;

const FOLDER: &str = "Aerie test";

#[tokio::main]
async fn main() -> Result<(), Error> {
    let client = OAuthClient::load(&OAuthClient::default_path())?;
    let http = http_client()?;

    let access_token = match credentials::load_refresh_token(DRIVE_FILE).await? {
        Some(refresh) => auth::refresh(&client, &http, &refresh).await?.access_token,
        None => {
            let tokens = auth::sign_in(&client, &http, DRIVE_FILE, |url| {
                println!("Opening your browser to grant access to files Aerie creates.");
                println!("If it doesn't open, visit:\n{url}\n");
                std::process::Command::new("xdg-open")
                    .arg(url.as_str())
                    .spawn()
                    .map(drop)
            })
            .await?;
            if let Some(refresh) = &tokens.refresh_token {
                credentials::save_refresh_token(DRIVE_FILE, refresh).await?;
            }
            tokens.access_token
        }
    };
    let drive = DriveApi::new(http.clone(), access_token.clone());

    // drive.file can't read the root folder itself, so use the `root` alias.
    let root = DriveFileId::new("root");
    let folder = match drive.find_folder(&root, FOLDER).await? {
        Some(folder) => folder,
        None => drive.create_folder(&root, FOLDER).await?,
    };
    println!("Working in My Drive › {FOLDER}\n");

    resumable_upload(&drive, &folder.id, &http, &access_token).await?;
    let race_file = overwrite_race(&drive, &folder.id, &http, &access_token).await?;
    lost_update(&drive, &race_file).await?;
    println!("\nDone. The test files are in My Drive › {FOLDER}; delete them whenever you like.");
    Ok(())
}

/// Uploads 1.5 MB, interrupts after the first chunk, resumes from a saved session in a
/// fresh client, then downloads the file and compares it byte for byte.
async fn resumable_upload(
    drive: &DriveApi,
    folder: &DriveFileId,
    http: &reqwest::Client,
    token: &str,
) -> Result<(), Error> {
    println!("Resumable upload");
    let data = pseudo_random(1_500_000);
    let session = drive
        .start_upload(
            &NewFile {
                name: "upload-test.bin",
                parent: folder,
                mime_type: "application/octet-stream",
            },
            data.len() as u64,
        )
        .await?;
    let first = drive
        .upload_chunk(&session, 0, data[..CHUNK_UNIT].to_vec())
        .await?;
    println!("  sent the first chunk, Drive reports {first:?}");

    // Simulate a restart: persist the session, then continue with a new client.
    let saved = serde_json::to_string(&session)?;
    let restored: UploadSession = serde_json::from_str(&saved)?;
    let fresh = DriveApi::new(http.clone(), token.to_owned());
    let status = fresh.upload_status(&restored).await?;
    if let UploadProgress::Partial { received } = status {
        println!("  after the restart, Drive holds {received} bytes; resuming");
    }
    let started = Instant::now();
    let entry = fresh
        .upload(
            &restored,
            &mut Cursor::new(&data),
            CHUNK_UNIT,
            RetryPolicy::default(),
        )
        .await?;
    println!("  finished in {:.1?}", started.elapsed());

    let downloaded = drive.download(&entry.id).await?;
    let matches = downloaded == data;
    println!(
        "  downloaded {} bytes, content {}",
        downloaded.len(),
        if matches { "matches" } else { "DIFFERS" }
    );
    if !matches {
        return Err("uploaded content doesn't match".into());
    }
    Ok(())
}

/// Checks how to detect a remote edit before overwriting, how long the check-then-write
/// window is, and whether Drive honors a conditional write.
async fn overwrite_race(
    drive: &DriveApi,
    folder: &DriveFileId,
    http: &reqwest::Client,
    token: &str,
) -> Result<DriveFileId, Error> {
    println!("\nOverwrite race");
    let file = upload_text(
        drive,
        drive
            .start_upload(
                &NewFile {
                    name: "race-test.txt",
                    parent: folder,
                    mime_type: "text/plain",
                },
                9,
            )
            .await?,
        "version A",
    )
    .await?;
    let baseline = drive.version(&file).await?;
    println!("  baseline: {baseline:?}");

    // Another device edits the file after Aerie last synced it.
    let remote_edit = "version B from another device";
    let session = drive
        .start_replace(&file, "text/plain", remote_edit.len() as u64)
        .await?;
    upload_text(drive, session, remote_edit).await?;
    let current = drive.version(&file).await?;
    println!("  after a remote edit: {current:?}");

    let conflict = current.md5_checksum != baseline.md5_checksum
        || current.head_revision_id != baseline.head_revision_id;
    println!(
        "  check before overwriting: {}",
        if conflict {
            "remote change detected, keep both versions"
        } else {
            "NOT detected"
        }
    );

    // The window between checking and finishing a replace, with no conflict present.
    let checked = Instant::now();
    let before = drive.version(&file).await?;
    let ours = "version C from Aerie";
    let session = drive
        .start_replace(&file, "text/plain", ours.len() as u64)
        .await?;
    upload_text(drive, session, ours).await?;
    println!(
        "  check-then-write window: {:.0?} (version {} → {})",
        checked.elapsed(),
        before.version,
        drive.version(&file).await?.version
    );

    // Does Drive reject a write whose If-Match doesn't match?
    let response = http
        .patch(format!(
            "https://www.googleapis.com/upload/drive/v3/files/{}",
            file.as_str()
        ))
        .query(&[("uploadType", "media")])
        .bearer_auth(token)
        .header("If-Match", "\"stale-etag\"")
        .header("Content-Type", "text/plain")
        .body("version D with a stale If-Match")
        .send()
        .await?;
    let status = response.status();
    let content = String::from_utf8(drive.download(&file).await?)?;
    println!(
        "  write with a stale If-Match: HTTP {status}, file now reads {content:?} ({})",
        if status.as_u16() == 412 {
            "conditional writes are honored"
        } else {
            "conditional writes are ignored"
        }
    );
    Ok(file)
}

/// Lets a remote edit slip in between Aerie's check and its write, then confirms the
/// revision list reveals the overwritten edit and that its content is recoverable.
async fn lost_update(drive: &DriveApi, file: &DriveFileId) -> Result<(), Error> {
    println!("\nLost-update detection");
    let baseline = drive.version(file).await?;
    let unchanged = !drive.version(file).await?.content_changed_since(&baseline);
    println!(
        "  check before writing: {}",
        if unchanged {
            "no remote change"
        } else {
            "changed"
        }
    );

    let sneaky = "version E from another device, written mid-race";
    let session = drive
        .start_replace(file, "text/plain", sneaky.len() as u64)
        .await?;
    upload_text(drive, session, sneaky).await?;

    let ours = "version F from Aerie";
    let session = drive
        .start_replace(file, "text/plain", ours.len() as u64)
        .await?;
    upload_text(drive, session, ours).await?;
    let our_head = drive
        .version(file)
        .await?
        .head_revision_id
        .ok_or("Drive returned no head revision")?;

    let revisions = drive.revisions(file).await?;
    let lost = intervening_revisions(&revisions, &baseline, &our_head);
    println!(
        "  after writing: {} revisions stored, {} written by someone else since the baseline",
        revisions.len(),
        lost.len()
    );
    let Some(last) = lost.last() else {
        return Err("the overwritten edit wasn't detected".into());
    };
    let recovered = String::from_utf8(drive.download_revision(file, &last.id).await?)?;
    println!(
        "  recovered the overwritten edit: {}",
        if recovered == sneaky {
            "content matches"
        } else {
            "content DIFFERS"
        }
    );
    if recovered != sneaky {
        return Err("recovered revision doesn't match".into());
    }
    Ok(())
}

async fn upload_text(
    drive: &DriveApi,
    session: UploadSession,
    text: &str,
) -> Result<DriveFileId, Error> {
    let entry = drive
        .upload(
            &session,
            &mut Cursor::new(text.as_bytes()),
            CHUNK_UNIT,
            RetryPolicy::default(),
        )
        .await?;
    Ok(entry.id)
}

/// Deterministic, incompressible-looking test content.
fn pseudo_random(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}
