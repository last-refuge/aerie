//! Streaming downloads and exports. Downloads resume from a partial file with a `Range`
//! request; exports of Google-native documents can't resume and start over.

use std::path::Path;

use aerie_core::DriveFileId;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

use crate::Result;
use crate::api::{DriveApi, api_error};

impl DriveApi {
    /// Downloads a file's content into `partial`, continuing after any bytes already there.
    /// `progress` receives the bytes written so far and the total when known. The caller
    /// publishes the finished file; nothing here touches the final destination.
    pub async fn download_to(
        &self,
        id: &DriveFileId,
        partial: &Path,
        progress: impl FnMut(u64, Option<u64>),
    ) -> Result<()> {
        let offset = tokio::fs::metadata(partial).await.map_or(0, |m| m.len());
        let mut request = self
            .http
            .get(format!("{}/files/{}", self.api_base, id.as_str()))
            .bearer_auth(&self.access_token)
            .query(&[("alt", "media"), ("supportsAllDrives", "true")]);
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let response = request.send().await?;
        match response.status() {
            StatusCode::RANGE_NOT_SATISFIABLE => Ok(()),
            // The server sent everything: start the partial file over.
            StatusCode::OK => write_body(response, partial, 0, progress).await,
            StatusCode::PARTIAL_CONTENT => write_body(response, partial, offset, progress).await,
            _ => Err(api_error(response).await),
        }
    }

    /// Exports a Google-native document as `mime_type` into `partial`, from the start.
    pub async fn export_to(
        &self,
        id: &DriveFileId,
        mime_type: &str,
        partial: &Path,
        progress: impl FnMut(u64, Option<u64>),
    ) -> Result<()> {
        let response = self
            .http
            .get(format!("{}/files/{}/export", self.api_base, id.as_str()))
            .bearer_auth(&self.access_token)
            .query(&[("mimeType", mime_type), ("supportsAllDrives", "true")])
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(api_error(response).await);
        }
        write_body(response, partial, 0, progress).await
    }
}

async fn write_body(
    mut response: reqwest::Response,
    partial: &Path,
    offset: u64,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<()> {
    let total = total_size(&response, offset);
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(offset > 0)
        .truncate(offset == 0)
        .open(partial)
        .await?;
    let mut written = offset;
    progress(written, total);
    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
        progress(written, total);
    }
    file.flush().await?;
    file.sync_all().await?;
    Ok(())
}

/// The full size: from `Content-Range` on a partial response, else `Content-Length`.
fn total_size(response: &reqwest::Response, offset: u64) -> Option<u64> {
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    if let Some(range) = header(CONTENT_RANGE) {
        return range
            .rsplit('/')
            .next()
            .and_then(|total| total.parse().ok());
    }
    header(CONTENT_LENGTH)
        .and_then(|length| length.parse::<u64>().ok())
        .map(|length| length + offset)
}
