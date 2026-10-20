//! Running one transfer, and publishing downloads safely: content goes to a hidden
//! partial file on the destination filesystem, is flushed to disk, and only then is
//! renamed into place. A file the user changed locally in the meantime is never replaced.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aerie_core::FileEntry;
use aerie_drive::api::DriveApi;
use aerie_drive::upload::{CHUNK_UNIT, NewFile, RetryPolicy, UploadSession};
use aerie_drive::{Error as DriveError, Result};
use aerie_store::{Direction, Store, Transfer};
use async_channel::Sender;

use crate::engine::Event;

/// Large enough to keep requests few, small enough to resume cheaply.
const CHUNK: usize = CHUNK_UNIT * 32;
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// How a transfer ended.
pub enum Outcome {
    Uploaded(FileEntry),
    /// Saved at this path, which differs from the requested one if the original changed.
    Downloaded(PathBuf),
}

/// The hidden file a download writes before it's published.
pub fn partial_path(destination: &Path) -> PathBuf {
    let name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    destination.with_file_name(format!(".{name}.aerie-partial"))
}

pub fn modified_time(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_nanos()).ok()
}

/// Moves a finished partial file into place. If the destination changed since the
/// download was queued (`expected` is its modification time then, or `None` if it didn't
/// exist), the download is saved alongside instead, so a local edit is never lost.
pub fn publish(
    partial: &Path,
    destination: &Path,
    expected: Option<i64>,
) -> std::io::Result<PathBuf> {
    let target = if modified_time(destination) == expected {
        destination.to_owned()
    } else {
        available_name(destination)
    };
    std::fs::rename(partial, &target)?;
    // Make the rename itself durable.
    if let Some(dir) = target.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(target)
}

/// `Report.pdf` → `Report (from Drive).pdf`, then `Report (from Drive 2).pdf`, and so on.
fn available_name(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    (1..)
        .map(|n| {
            let suffix = if n == 1 {
                " (from Drive)".to_owned()
            } else {
                format!(" (from Drive {n})")
            };
            path.with_file_name(format!("{stem}{suffix}{extension}"))
        })
        .find(|candidate| !candidate.exists())
        .expect("an unused name exists")
}

/// Runs `transfer` to completion, saving progress so it can resume after a restart.
pub async fn run(
    drive: DriveApi,
    transfer: Transfer,
    events: Sender<Event>,
    database: PathBuf,
) -> Result<Outcome> {
    // Progress lives in the same database on a separate connection.
    let store = Store::open(&database).map_err(io_error)?;
    let mut last_report = Instant::now() - PROGRESS_EVERY;
    let id = transfer.id;
    let mut report = move |done: u64, total: Option<u64>| {
        if last_report.elapsed() >= PROGRESS_EVERY {
            last_report = Instant::now();
            let _ = store.set_transfer_progress(id, done, total);
            let _ = events.try_send(Event::TransferProgress { id, done, total });
        }
    };
    match transfer.direction {
        Direction::Upload => upload(&drive, &transfer, &database, &mut report)
            .await
            .map(Outcome::Uploaded),
        Direction::Download => download(&drive, &transfer, &mut report)
            .await
            .map(Outcome::Downloaded),
    }
}

async fn upload(
    drive: &DriveApi,
    transfer: &Transfer,
    database: &Path,
    report: &mut impl FnMut(u64, Option<u64>),
) -> Result<FileEntry> {
    let store = Store::open(database).map_err(io_error)?;
    let total = tokio::fs::metadata(&transfer.local_path).await?.len();
    let mut file = tokio::fs::File::open(&transfer.local_path).await?;
    let saved = transfer
        .upload_session
        .as_deref()
        .and_then(|json| serde_json::from_str::<UploadSession>(json).ok())
        // A file that changed size since it was queued needs a fresh session.
        .filter(|session| session.total == total);

    for attempt in 0..2 {
        let session = match (&saved, attempt) {
            (Some(session), 0) => session.clone(),
            _ => {
                let parent = transfer
                    .parent_id
                    .clone()
                    .expect("uploads have a destination");
                let session = drive
                    .start_upload(
                        &NewFile {
                            name: &transfer.name,
                            parent: &parent,
                            mime_type: &transfer.mime_type,
                        },
                        total,
                    )
                    .await?;
                let json = serde_json::to_string(&session).expect("sessions serialize");
                store
                    .set_upload_session(transfer.id, Some(&json))
                    .map_err(io_error)?;
                session
            }
        };
        let result = drive
            .upload_with_progress(&session, &mut file, CHUNK, RetryPolicy::default(), |done| {
                report(done, Some(total))
            })
            .await;
        match result {
            Err(DriveError::UploadSessionExpired) if attempt == 0 => {
                tracing::info!(
                    transfer = transfer.id,
                    "upload session expired; starting over"
                );
            }
            other => {
                report(total, Some(total));
                return other;
            }
        }
    }
    Err(DriveError::UploadSessionExpired)
}

async fn download(
    drive: &DriveApi,
    transfer: &Transfer,
    report: &mut impl FnMut(u64, Option<u64>),
) -> Result<PathBuf> {
    let file = transfer.file_id.clone().expect("downloads have a source");
    let partial = partial_path(&transfer.local_path);
    match &transfer.export_mime {
        Some(mime) => drive.export_to(&file, mime, &partial, report).await?,
        None => drive.download_to(&file, &partial, report).await?,
    }
    if let (None, Some(total)) = (&transfer.export_mime, transfer.total) {
        let written = tokio::fs::metadata(&partial).await?.len();
        if written != total {
            // Never publish an incomplete file; start the partial over next time.
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(DriveError::Io(std::io::Error::other(format!(
                "the download was incomplete ({written} of {total} bytes)"
            ))));
        }
    }
    let destination = transfer.local_path.clone();
    let expected = transfer.replaces_mtime;
    let saved = tokio::task::spawn_blocking(move || publish(&partial, &destination, expected))
        .await
        .map_err(|err| DriveError::Io(std::io::Error::other(err)))??;
    Ok(saved)
}

fn io_error(err: aerie_store::Error) -> DriveError {
    DriveError::Io(std::io::Error::other(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aerie-publish-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn publishes_a_new_file() {
        let dir = dir("new");
        let destination = dir.join("Report.pdf");
        let partial = partial_path(&destination);
        assert_eq!(partial.file_name().unwrap(), ".Report.pdf.aerie-partial");
        std::fs::write(&partial, b"drive").unwrap();
        assert_eq!(publish(&partial, &destination, None).unwrap(), destination);
        assert_eq!(std::fs::read(&destination).unwrap(), b"drive");
        assert!(!partial.exists());
    }

    #[test]
    fn replaces_an_unchanged_file() {
        let dir = dir("replace");
        let destination = dir.join("Report.pdf");
        std::fs::write(&destination, b"old").unwrap();
        let expected = modified_time(&destination);
        let partial = partial_path(&destination);
        std::fs::write(&partial, b"drive").unwrap();
        assert_eq!(
            publish(&partial, &destination, expected).unwrap(),
            destination
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"drive");
    }

    #[test]
    fn keeps_a_file_edited_during_the_download() {
        let dir = dir("edited");
        let destination = dir.join("Report.pdf");
        std::fs::write(&destination, b"local edit").unwrap();
        let partial = partial_path(&destination);
        std::fs::write(&partial, b"drive").unwrap();
        // Queued when the file didn't exist yet, so the local file is new.
        let saved = publish(&partial, &destination, None).unwrap();
        assert_eq!(saved, dir.join("Report (from Drive).pdf"));
        assert_eq!(std::fs::read(&destination).unwrap(), b"local edit");

        std::fs::write(&partial, b"drive again").unwrap();
        let saved = publish(&partial, &destination, None).unwrap();
        assert_eq!(saved, dir.join("Report (from Drive 2).pdf"));
    }

    mod against_a_simulated_drive {
        use std::sync::{Arc, Mutex};

        use aerie_core::DriveFileId;
        use aerie_store::{Account, NewTransfer};
        use axum::Router;
        use axum::body::Bytes;
        use axum::extract::State;
        use axum::http::{HeaderMap, StatusCode, header};
        use axum::response::{IntoResponse, Response};
        use axum::routing::{get, post, put};

        use super::*;

        #[derive(Default)]
        struct Fake {
            base: String,
            received: Vec<u8>,
            total: u64,
            content: Vec<u8>,
        }
        type Shared = Arc<Mutex<Fake>>;

        async fn start(content: Vec<u8>) -> (Shared, DriveApi) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let fake = Arc::new(Mutex::new(Fake {
                base: base.clone(),
                content,
                ..Fake::default()
            }));
            let app = Router::new()
                .route("/upload/files", post(open))
                .route("/session", put(send))
                .route("/api/files/{id}", get(media))
                .with_state(fake.clone());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let drive = DriveApi::with_base_urls(
                aerie_drive::http_client().unwrap(),
                "token".into(),
                &format!("{base}/api"),
                &format!("{base}/upload"),
            );
            (fake, drive)
        }

        async fn open(State(fake): State<Shared>, headers: HeaderMap) -> Response {
            let mut fake = fake.lock().unwrap();
            fake.total = headers["x-upload-content-length"]
                .to_str()
                .unwrap()
                .parse()
                .unwrap();
            let location = format!("{}/session", fake.base);
            (StatusCode::OK, [(header::LOCATION, location)]).into_response()
        }

        async fn send(State(fake): State<Shared>, body: Bytes) -> Response {
            let mut fake = fake.lock().unwrap();
            fake.received.extend_from_slice(&body);
            if fake.received.len() as u64 == fake.total {
                let file = serde_json::json!({
                    "id": "new-file", "name": "notes.txt", "mimeType": "text/plain",
                    "size": fake.total.to_string(), "parents": ["folder"],
                });
                return (StatusCode::OK, axum::Json(file)).into_response();
            }
            let mut response = StatusCode::PERMANENT_REDIRECT.into_response();
            if !fake.received.is_empty() {
                let range = format!("bytes=0-{}", fake.received.len() - 1);
                response
                    .headers_mut()
                    .insert(header::RANGE, range.parse().unwrap());
            }
            response
        }

        async fn media(State(fake): State<Shared>) -> Response {
            (StatusCode::OK, fake.lock().unwrap().content.clone()).into_response()
        }

        fn queue(dir: &Path, new: NewTransfer) -> (PathBuf, Transfer) {
            let database = dir.join("aerie.sqlite3");
            let store = Store::open(&database).unwrap();
            store
                .save_account(&Account {
                    id: "a".into(),
                    email: "e".into(),
                    display_name: "n".into(),
                })
                .unwrap();
            let id = store.add_transfer("a", &new, 0).unwrap();
            (database.clone(), store.transfer(id).unwrap().unwrap())
        }

        #[tokio::test]
        async fn uploads_a_file_and_saves_its_session() {
            let (fake, drive) = start(Vec::new()).await;
            let dir = dir("upload-run");
            let source = dir.join("notes.txt");
            std::fs::write(&source, b"hello from aerie").unwrap();
            let (database, transfer) = queue(
                &dir,
                NewTransfer {
                    direction: Direction::Upload,
                    file_id: None,
                    parent_id: Some(DriveFileId::new("folder")),
                    name: "notes.txt".into(),
                    local_path: source,
                    mime_type: "text/plain".into(),
                    export_mime: None,
                    total: Some(16),
                    replaces_mtime: None,
                },
            );
            let id = transfer.id;
            let (events, _inbox) = async_channel::unbounded();
            let outcome = run(drive, transfer, events, database.clone())
                .await
                .unwrap();
            let Outcome::Uploaded(entry) = outcome else {
                panic!("expected an upload")
            };
            assert_eq!(entry.id, DriveFileId::new("new-file"));
            assert_eq!(fake.lock().unwrap().received, b"hello from aerie");
            let saved = Store::open(&database)
                .unwrap()
                .transfer(id)
                .unwrap()
                .unwrap();
            assert!(saved.upload_session.is_some_and(|s| s.contains("/session")));
        }

        #[tokio::test]
        async fn downloads_and_publishes_a_file() {
            let (_fake, drive) = start(b"content from drive".to_vec()).await;
            let dir = dir("download-run");
            let destination = dir.join("Report.txt");
            let (database, transfer) = queue(
                &dir,
                NewTransfer {
                    direction: Direction::Download,
                    file_id: Some(DriveFileId::new("remote")),
                    parent_id: None,
                    name: "Report.txt".into(),
                    local_path: destination.clone(),
                    mime_type: "text/plain".into(),
                    export_mime: None,
                    total: Some(18),
                    replaces_mtime: None,
                },
            );
            let (events, _inbox) = async_channel::unbounded();
            let outcome = run(drive, transfer, events, database).await.unwrap();
            let Outcome::Downloaded(path) = outcome else {
                panic!("expected a download")
            };
            assert_eq!(path, destination);
            assert_eq!(std::fs::read(&destination).unwrap(), b"content from drive");
            assert!(!partial_path(&destination).exists());
        }

        #[tokio::test]
        async fn refuses_to_publish_an_incomplete_download() {
            let (_fake, drive) = start(b"short".to_vec()).await;
            let dir = dir("short-run");
            let destination = dir.join("Big.bin");
            let (database, transfer) = queue(
                &dir,
                NewTransfer {
                    direction: Direction::Download,
                    file_id: Some(DriveFileId::new("remote")),
                    parent_id: None,
                    name: "Big.bin".into(),
                    local_path: destination.clone(),
                    mime_type: "application/octet-stream".into(),
                    export_mime: None,
                    total: Some(1_000),
                    replaces_mtime: None,
                },
            );
            let (events, _inbox) = async_channel::unbounded();
            assert!(run(drive, transfer, events, database).await.is_err());
            assert!(!destination.exists());
            assert!(!partial_path(&destination).exists());
        }
    }
}
