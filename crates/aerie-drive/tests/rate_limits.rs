//! Rate limits and server errors against a simulated Drive that answers with a scripted
//! sequence of failures before succeeding.

use std::sync::{Arc, Mutex};

use aerie_core::DriveFileId;
use aerie_drive::Error;
use aerie_drive::api::DriveApi;
use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

#[derive(Default)]
struct Fake {
    /// Answers to give, in order, before succeeding.
    failures: Vec<Failure>,
    requests: u32,
}

#[derive(Clone, Copy)]
enum Failure {
    UserRateLimit,
    TooManyRequests,
    Unavailable,
    Forbidden,
}

type Shared = Arc<Mutex<Fake>>;

async fn start(failures: Vec<Failure>) -> (Shared, DriveApi) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let fake = Arc::new(Mutex::new(Fake {
        failures,
        requests: 0,
    }));
    let app = Router::new()
        .route("/api/files/{id}", get(answer))
        .route("/api/files", axum::routing::post(answer))
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

fn drive_error(status: StatusCode, reason: &str) -> Response {
    let body = serde_json::json!({
        "error": {
            "code": status.as_u16(),
            "message": format!("simulated {reason}"),
            "errors": [{ "reason": reason }],
        }
    });
    // Retry-After: 0 keeps the test quick while still exercising the header.
    (status, [(header::RETRY_AFTER, "0")], axum::Json(body)).into_response()
}

async fn answer(State(fake): State<Shared>) -> Response {
    let mut fake = fake.lock().unwrap();
    fake.requests += 1;
    if fake.failures.is_empty() {
        let folder = serde_json::json!({
            "id": "new-folder",
            "name": "Folder",
            "mimeType": "application/vnd.google-apps.folder",
            "parents": ["root-id"],
        });
        return (StatusCode::OK, axum::Json(folder)).into_response();
    }
    match fake.failures.remove(0) {
        Failure::UserRateLimit => drive_error(StatusCode::FORBIDDEN, "userRateLimitExceeded"),
        Failure::TooManyRequests => drive_error(StatusCode::TOO_MANY_REQUESTS, "rateLimitExceeded"),
        Failure::Unavailable => drive_error(StatusCode::SERVICE_UNAVAILABLE, "backendError"),
        Failure::Forbidden => drive_error(StatusCode::FORBIDDEN, "insufficientFilePermissions"),
    }
}

#[tokio::test]
async fn retries_reads_through_rate_limits_and_server_errors() {
    use Failure::*;
    let (fake, api) = start(vec![UserRateLimit, TooManyRequests, Unavailable]).await;
    let bytes = api.download(&DriveFileId::new("abc")).await.unwrap();
    assert!(!bytes.is_empty());
    assert_eq!(fake.lock().unwrap().requests, 4);
}

#[tokio::test]
async fn a_lasting_rate_limit_becomes_rate_limited() {
    let (fake, api) = start(vec![Failure::UserRateLimit; 20]).await;
    let err = api.download(&DriveFileId::new("abc")).await.unwrap_err();
    assert!(matches!(err, Error::RateLimited), "{err}");
    assert_eq!(fake.lock().unwrap().requests, 6);
}

#[tokio::test]
async fn permission_errors_are_not_retried() {
    let (fake, api) = start(vec![Failure::Forbidden]).await;
    let err = api.download(&DriveFileId::new("abc")).await.unwrap_err();
    assert!(matches!(err, Error::Api { status: 403, .. }), "{err}");
    assert_eq!(fake.lock().unwrap().requests, 1);
}

#[tokio::test]
async fn writes_retry_rate_limits_but_not_server_errors() {
    let root = DriveFileId::new("root-id");
    let (fake, api) = start(vec![Failure::UserRateLimit]).await;
    api.create_folder(&root, "Folder").await.unwrap();
    assert_eq!(fake.lock().unwrap().requests, 2);

    // The folder may have been made before the error; creating it again could duplicate it.
    let (fake, api) = start(vec![Failure::Unavailable]).await;
    assert!(api.create_folder(&root, "Folder").await.is_err());
    assert_eq!(fake.lock().unwrap().requests, 1);
}
