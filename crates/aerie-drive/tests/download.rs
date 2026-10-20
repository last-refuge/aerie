//! Resumable downloads and exports against a simulated Drive.

use std::sync::{Arc, Mutex};

use aerie_core::DriveFileId;
use aerie_drive::api::DriveApi;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

#[derive(Default)]
struct Fake {
    content: Vec<u8>,
    ignore_range: bool,
    ranges: Vec<Option<String>>,
}

type Shared = Arc<Mutex<Fake>>;

async fn start(content: Vec<u8>, ignore_range: bool) -> (Shared, DriveApi) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let fake = Arc::new(Mutex::new(Fake {
        content,
        ignore_range,
        ..Fake::default()
    }));
    let app = Router::new()
        .route("/api/files/{id}", get(media))
        .route("/api/files/{id}/export", get(export))
        .with_state(fake.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let api = DriveApi::with_base_urls(
        aerie_drive::http_client().unwrap(),
        "token".into(),
        &format!("{base}/api"),
        &format!("{base}/upload"),
    );
    (fake, api)
}

async fn media(State(fake): State<Shared>, headers: HeaderMap) -> Response {
    let mut fake = fake.lock().unwrap();
    let range = headers
        .get(header::RANGE)
        .map(|value| value.to_str().unwrap().to_owned());
    fake.ranges.push(range.clone());
    let total = fake.content.len();
    match range.filter(|_| !fake.ignore_range) {
        Some(range) => {
            let start: usize = range["bytes=".len()..range.len() - 1].parse().unwrap();
            if start >= total {
                return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            }
            (
                StatusCode::PARTIAL_CONTENT,
                [(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{}/{total}", total - 1),
                )],
                fake.content[start..].to_vec(),
            )
                .into_response()
        }
        None => (StatusCode::OK, fake.content.clone()).into_response(),
    }
}

async fn export(Query(query): Query<Vec<(String, String)>>) -> Response {
    let mime = query
        .iter()
        .find(|(key, _)| key == "mimeType")
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    (StatusCode::OK, format!("exported as {mime}")).into_response()
}

fn content() -> Vec<u8> {
    (0..100_000).map(|i| (i % 253) as u8).collect()
}

fn partial_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aerie-download-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("file.partial")
}

#[tokio::test]
async fn downloads_a_whole_file_with_progress() {
    let (_fake, api) = start(content(), false).await;
    let path = partial_path("whole");
    let mut last = (0, None);
    api.download_to(&DriveFileId::new("f"), &path, |done, total| {
        last = (done, total)
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), content());
    assert_eq!(last, (100_000, Some(100_000)));
}

#[tokio::test]
async fn resumes_from_a_partial_file() {
    let (fake, api) = start(content(), false).await;
    let path = partial_path("resume");
    std::fs::write(&path, &content()[..40_000]).unwrap();
    api.download_to(&DriveFileId::new("f"), &path, |_, _| {})
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), content());
    assert_eq!(
        fake.lock().unwrap().ranges,
        [Some("bytes=40000-".to_owned())]
    );
}

#[tokio::test]
async fn starts_over_when_the_server_ignores_the_range() {
    let (_fake, api) = start(content(), true).await;
    let path = partial_path("ignored");
    std::fs::write(&path, b"stale bytes that must not stay").unwrap();
    api.download_to(&DriveFileId::new("f"), &path, |_, _| {})
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), content());
}

#[tokio::test]
async fn treats_an_already_complete_partial_as_done() {
    let (_fake, api) = start(content(), false).await;
    let path = partial_path("complete");
    std::fs::write(&path, content()).unwrap();
    api.download_to(&DriveFileId::new("f"), &path, |_, _| {})
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), content());
}

#[tokio::test]
async fn exports_google_documents() {
    let (_fake, api) = start(Vec::new(), false).await;
    let path = partial_path("export");
    api.export_to(
        &DriveFileId::new("doc"),
        "application/pdf",
        &path,
        |_, _| {},
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "exported as application/pdf"
    );
}
