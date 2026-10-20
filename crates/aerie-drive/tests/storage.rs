//! Storage use from a simulated Drive `about` endpoint.

use aerie_drive::api::{DriveApi, Storage};
use axum::Router;
use axum::routing::get;

async fn api(quota: serde_json::Value) -> DriveApi {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().route(
        "/api/about",
        get(move || async move { axum::Json(serde_json::json!({ "storageQuota": quota })) }),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    DriveApi::with_base_urls(
        aerie_drive::http_client().unwrap(),
        "test-token".into(),
        &format!("{base}/api"),
        &format!("{base}/upload"),
    )
}

#[tokio::test]
async fn reads_usage_and_limit() {
    let api = api(serde_json::json!({ "usage": "1200000000000", "limit": "2000000000000" })).await;
    assert_eq!(
        api.storage().await.unwrap(),
        Storage {
            used: 1_200_000_000_000,
            limit: Some(2_000_000_000_000),
        }
    );
}

#[tokio::test]
async fn an_unlimited_account_has_no_limit() {
    let api = api(serde_json::json!({ "usage": "5000" })).await;
    assert_eq!(
        api.storage().await.unwrap(),
        Storage {
            used: 5000,
            limit: None,
        }
    );
}
