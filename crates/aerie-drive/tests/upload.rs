//! Resumable uploads against a simulated Drive upload service with injected faults.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aerie_core::DriveFileId;
use aerie_drive::Error;
use aerie_drive::api::DriveApi;
use aerie_drive::upload::{CHUNK_UNIT, NewFile, RetryPolicy, UploadSession};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{patch, post, put};

#[derive(Default)]
struct Session {
    name: String,
    total: u64,
    data: Vec<u8>,
}

#[derive(Default)]
struct Fake {
    base: String,
    sessions: HashMap<u32, Session>,
    /// Store the next chunk, then answer 503 as if the response were lost.
    lose_next_response: bool,
    /// Store only this many bytes of the next chunk.
    accept_next: Option<usize>,
    expired: bool,
    chunk_requests: u32,
}

type Shared = Arc<Mutex<Fake>>;

async fn start() -> (Shared, DriveApi) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let fake = Arc::new(Mutex::new(Fake {
        base: base.clone(),
        ..Fake::default()
    }));
    let app = Router::new()
        .route("/upload/files", post(open))
        .route("/upload/files/{id}", patch(open_replace))
        .route("/session/{id}", put(send))
        .with_state(fake.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let api = DriveApi::with_base_urls(
        aerie_drive::http_client().unwrap(),
        "test-token".into(),
        &format!("{base}/api"),
        &format!("{base}/upload"),
    );
    (fake, api)
}

async fn open(State(fake): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let metadata: serde_json::Value = serde_json::from_slice(&body).unwrap();
    new_session(
        &fake,
        &headers,
        metadata["name"].as_str().unwrap_or_default(),
    )
}

async fn open_replace(
    State(fake): State<Shared>,
    Path(_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    new_session(&fake, &headers, "replaced")
}

fn new_session(fake: &Shared, headers: &HeaderMap, name: &str) -> Response {
    let total = headers["x-upload-content-length"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let mut fake = fake.lock().unwrap();
    let id = fake.sessions.len() as u32;
    fake.sessions.insert(
        id,
        Session {
            name: name.to_owned(),
            total,
            data: Vec::new(),
        },
    );
    let location = format!("{}/session/{id}", fake.base);
    (StatusCode::OK, [(header::LOCATION, location)]).into_response()
}

async fn send(
    State(fake): State<Shared>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut fake = fake.lock().unwrap();
    if fake.expired {
        return StatusCode::NOT_FOUND.into_response();
    }
    let range = headers[header::CONTENT_RANGE].to_str().unwrap().to_owned();

    let mut lose_response = false;
    if !body.is_empty() {
        fake.chunk_requests += 1;
        lose_response = std::mem::take(&mut fake.lose_next_response);
        let accept = fake.accept_next.take().unwrap_or(body.len());
        let start: usize = range["bytes ".len()..]
            .split('-')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let session = fake.sessions.get_mut(&id).unwrap();
        assert_eq!(
            start,
            session.data.len(),
            "client resent from the wrong offset"
        );
        session.data.extend_from_slice(&body[..accept]);
    }
    if lose_response {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }

    let session = &fake.sessions[&id];
    if session.data.len() as u64 == session.total {
        let file = serde_json::json!({
            "id": format!("file-{id}"),
            "name": session.name,
            "mimeType": "application/octet-stream",
            "size": session.total.to_string(),
            "parents": ["root-id"],
        });
        return (StatusCode::OK, axum::Json(file)).into_response();
    }
    let mut response = StatusCode::PERMANENT_REDIRECT.into_response();
    if !session.data.is_empty() {
        let range = format!("bytes=0-{}", session.data.len() - 1);
        response
            .headers_mut()
            .insert(header::RANGE, range.parse().unwrap());
    }
    response
}

fn content(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

const FAST_RETRY: RetryPolicy = RetryPolicy {
    attempts: 3,
    first_delay: Duration::from_millis(1),
};

async fn upload(api: &DriveApi, data: &[u8]) -> aerie_drive::Result<aerie_core::FileEntry> {
    let parent = DriveFileId::new("root-id");
    let file = NewFile {
        name: "report.bin",
        parent: &parent,
        mime_type: "application/octet-stream",
    };
    let session = api.start_upload(&file, data.len() as u64).await?;
    api.upload(&session, &mut Cursor::new(data), CHUNK_UNIT, FAST_RETRY)
        .await
}

#[tokio::test]
async fn uploads_in_chunks() {
    let (fake, api) = start().await;
    let data = content(CHUNK_UNIT * 2 + 1000);
    let entry = upload(&api, &data).await.unwrap();

    assert_eq!(entry.name, "report.bin");
    assert_eq!(entry.size, Some(data.len() as u64));
    let fake = fake.lock().unwrap();
    assert_eq!(fake.sessions[&0].data, data);
    assert_eq!(fake.chunk_requests, 3);
}

#[tokio::test]
async fn recovers_from_a_lost_response_without_duplicating() {
    let (fake, api) = start().await;
    fake.lock().unwrap().lose_next_response = true;
    let data = content(CHUNK_UNIT * 2);
    upload(&api, &data).await.unwrap();

    // The first chunk landed although its response was lost; it wasn't sent twice.
    let fake = fake.lock().unwrap();
    assert_eq!(fake.sessions[&0].data, data);
    assert_eq!(fake.chunk_requests, 2);
}

#[tokio::test]
async fn resends_only_what_drive_did_not_store() {
    let (fake, api) = start().await;
    fake.lock().unwrap().accept_next = Some(100_000);
    let data = content(CHUNK_UNIT + 5);
    upload(&api, &data).await.unwrap();

    assert_eq!(fake.lock().unwrap().sessions[&0].data, data);
}

#[tokio::test]
async fn resumes_a_saved_session_after_a_restart() {
    let (fake, api) = start().await;
    let data = content(CHUNK_UNIT * 3);
    let parent = DriveFileId::new("root-id");
    let file = NewFile {
        name: "big.bin",
        parent: &parent,
        mime_type: "application/octet-stream",
    };
    let session = api.start_upload(&file, data.len() as u64).await.unwrap();
    api.upload_chunk(&session, 0, data[..CHUNK_UNIT].to_vec())
        .await
        .unwrap();

    // Simulate a restart: the session is persisted, reloaded and continued.
    let saved = serde_json::to_string(&session).unwrap();
    let restored: UploadSession = serde_json::from_str(&saved).unwrap();
    api.upload(&restored, &mut Cursor::new(&data), CHUNK_UNIT, FAST_RETRY)
        .await
        .unwrap();

    let fake = fake.lock().unwrap();
    assert_eq!(fake.sessions[&0].data, data);
    assert_eq!(fake.chunk_requests, 3);
}

#[tokio::test]
async fn reports_an_expired_session() {
    let (fake, api) = start().await;
    fake.lock().unwrap().expired = true;
    let result = upload(&api, &content(10)).await;
    assert!(matches!(result, Err(Error::UploadSessionExpired)));
}

#[tokio::test]
async fn uploads_an_empty_file() {
    let (_fake, api) = start().await;
    let entry = upload(&api, &[]).await.unwrap();
    assert_eq!(entry.size, Some(0));
}

#[tokio::test]
async fn replaces_existing_content() {
    let (fake, api) = start().await;
    let data = content(2000);
    let session = api
        .start_replace(
            &DriveFileId::new("existing"),
            "text/plain",
            data.len() as u64,
        )
        .await
        .unwrap();
    api.upload(&session, &mut Cursor::new(&data), CHUNK_UNIT, FAST_RETRY)
        .await
        .unwrap();
    assert_eq!(fake.lock().unwrap().sessions[&0].data, data);
}
