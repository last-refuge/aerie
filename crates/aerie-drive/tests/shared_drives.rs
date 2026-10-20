//! Shared drive listing and the parameters that reach shared-drive items.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aerie_core::DriveFileId;
use aerie_drive::api::{Corpus, DriveApi};
use axum::Router;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

type Seen = Arc<Mutex<Vec<(String, HashMap<String, String>)>>>;

async fn start() -> (Seen, DriveApi) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let app = Router::new()
        .route("/api/drives", get(drives))
        .route("/api/files", get(files))
        .route("/api/about", get(about))
        .with_state(seen.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let api = DriveApi::with_base_urls(
        aerie_drive::http_client().unwrap(),
        "token".into(),
        &format!("{base}/api"),
        &format!("{base}/upload"),
    );
    (seen, api)
}

async fn drives(State(seen): State<Seen>, Query(q): Query<HashMap<String, String>>) -> Response {
    seen.lock().unwrap().push(("drives".into(), q.clone()));
    let body = if q.contains_key("pageToken") {
        serde_json::json!({"drives": [{"id": "d2", "name": "Design", "capabilities": {"canAddChildren": false}}]})
    } else {
        serde_json::json!({"nextPageToken": "p2", "drives": [{"id": "d1", "name": "Team", "capabilities": {"canAddChildren": true}}]})
    };
    axum::Json(body).into_response()
}

async fn files(State(seen): State<Seen>, Query(q): Query<HashMap<String, String>>) -> Response {
    seen.lock().unwrap().push(("files".into(), q));
    axum::Json(serde_json::json!({"files": [
        {"id": "f1", "name": "Plan.pdf", "mimeType": "application/pdf", "parents": ["d1"]}
    ]}))
    .into_response()
}

async fn about(State(seen): State<Seen>, Query(q): Query<HashMap<String, String>>) -> Response {
    seen.lock().unwrap().push(("about".into(), q));
    axum::Json(serde_json::json!({"user": {
        "permissionId": "p", "displayName": "n", "emailAddress": "e@example.com"
    }}))
    .into_response()
}

#[tokio::test]
async fn lists_every_shared_drive() {
    let (_seen, api) = start().await;
    let drives = api.shared_drives().await.unwrap();
    let names: Vec<_> = drives.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["Team", "Design"]);
    assert!(drives[0].can_add_children && !drives[1].can_add_children);
    assert_eq!(drives[0].as_entry().id, DriveFileId::new("d1"));
}

#[tokio::test]
async fn inventories_a_shared_drive_by_its_id() {
    let (seen, api) = start().await;
    let (entries, next) = api
        .inventory_page(&Corpus::SharedDrive(DriveFileId::new("d1")), None)
        .await
        .unwrap();
    assert_eq!(entries[0].parent, Some(DriveFileId::new("d1")));
    assert_eq!(next, None);
    let seen = seen.lock().unwrap();
    let (_, query) = seen.last().unwrap();
    assert_eq!(query["corpora"], "drive");
    assert_eq!(query["driveId"], "d1");
    assert_eq!(query["includeItemsFromAllDrives"], "true");
    assert_eq!(query["supportsAllDrives"], "true");
}

#[tokio::test]
async fn sends_shared_drive_support_only_where_drive_accepts_it() {
    let (seen, api) = start().await;
    api.inventory_page(&Corpus::User, None).await.unwrap();
    api.account().await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].1["corpora"], "user");
    assert_eq!(seen[0].1["supportsAllDrives"], "true");
    assert!(!seen[1].1.contains_key("supportsAllDrives"));
}
