//! Resumable uploads. A session survives dropped connections and restarts: before
//! resending, ask Drive how many bytes it holds and continue from there, so an ambiguous
//! failure never duplicates or corrupts content.

use std::time::Duration;

use aerie_core::{DriveFileId, FileEntry};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, LOCATION, RANGE};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, SeekFrom};

use crate::api::{DriveApi, FILE, RemoteFile, api_error};
use crate::{Error, Result};

/// Drive requires every chunk except the last to be a multiple of 256 KiB.
pub const CHUNK_UNIT: usize = 256 * 1024;

/// An upload in progress. Persist it to resume after a restart; the URI grants access
/// to the upload, so store it as carefully as a token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadSession {
    pub uri: String,
    pub total: u64,
}

/// What Drive holds for a session.
#[derive(Debug, PartialEq, Eq)]
pub enum UploadProgress {
    /// Bytes `0..received` are stored; send the rest from `received`.
    Partial {
        received: u64,
    },
    Complete(FileEntry),
}

/// Metadata for a new file.
pub struct NewFile<'a> {
    pub name: &'a str,
    pub parent: &'a DriveFileId,
    pub mime_type: &'a str,
}

/// How many times to recover from transient failures, and how long to wait at first.
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    pub attempts: u32,
    pub first_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 6,
            first_delay: Duration::from_secs(1),
        }
    }
}

impl DriveApi {
    /// Opens a session that creates a new file once all `total` bytes arrive.
    pub async fn start_upload(&self, file: &NewFile<'_>, total: u64) -> Result<UploadSession> {
        let metadata = serde_json::json!({
            "name": file.name,
            "parents": [file.parent.as_str()],
            "mimeType": file.mime_type,
        });
        let request = self
            .http
            .post(format!("{}/files", self.upload_base))
            .query(&[("uploadType", "resumable"), ("fields", FILE)]);
        self.open_session(request, metadata, file.mime_type, total)
            .await
    }

    /// Opens a session that replaces an existing file's content.
    pub async fn start_replace(
        &self,
        id: &DriveFileId,
        mime_type: &str,
        total: u64,
    ) -> Result<UploadSession> {
        let request = self
            .http
            .patch(format!("{}/files/{}", self.upload_base, id.as_str()))
            .query(&[("uploadType", "resumable"), ("fields", FILE)]);
        self.open_session(request, serde_json::json!({}), mime_type, total)
            .await
    }

    async fn open_session(
        &self,
        request: reqwest::RequestBuilder,
        metadata: serde_json::Value,
        mime_type: &str,
        total: u64,
    ) -> Result<UploadSession> {
        let response = request
            .bearer_auth(&self.access_token)
            .header("X-Upload-Content-Type", mime_type)
            .header("X-Upload-Content-Length", total)
            .json(&metadata)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(api_error(response).await);
        }
        let uri = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| Error::Api {
                status: response.status().as_u16(),
                message: "Drive didn't return an upload session".into(),
            })?
            .to_owned();
        Ok(UploadSession { uri, total })
    }

    /// Asks Drive how much of the upload it has stored.
    pub async fn upload_status(&self, session: &UploadSession) -> Result<UploadProgress> {
        let response = self
            .http
            .put(&session.uri)
            .bearer_auth(&self.access_token)
            .header(CONTENT_LENGTH, 0)
            .header(CONTENT_RANGE, format!("bytes */{}", session.total))
            .send()
            .await?;
        progress_from(response).await
    }

    /// Sends `data` starting at byte `offset`.
    pub async fn upload_chunk(
        &self,
        session: &UploadSession,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<UploadProgress> {
        let range = if data.is_empty() {
            format!("bytes */{}", session.total)
        } else {
            let last = offset + data.len() as u64 - 1;
            format!("bytes {offset}-{last}/{}", session.total)
        };
        let response = self
            .http
            .put(&session.uri)
            .bearer_auth(&self.access_token)
            .header(CONTENT_LENGTH, data.len())
            .header(CONTENT_RANGE, range)
            .body(data)
            .send()
            .await?;
        progress_from(response).await
    }

    /// Uploads `source` through `session`, resuming from whatever Drive already holds.
    /// Transient failures are retried with exponential backoff; an expired session
    /// returns [`Error::UploadSessionExpired`] so the caller can start a new one.
    pub async fn upload<R>(
        &self,
        session: &UploadSession,
        source: &mut R,
        chunk_size: usize,
        retry: RetryPolicy,
    ) -> Result<FileEntry>
    where
        R: AsyncRead + AsyncSeek + Unpin,
    {
        assert!(
            chunk_size > 0 && chunk_size.is_multiple_of(CHUNK_UNIT),
            "chunk size must be a multiple of 256 KiB"
        );

        let mut failures = 0;
        let mut progress = self.upload_status(session).await;
        loop {
            let received = match progress {
                Ok(UploadProgress::Complete(entry)) => return Ok(entry),
                Ok(UploadProgress::Partial { received }) => {
                    failures = 0;
                    received
                }
                Err(err) if is_transient(&err) && failures < retry.attempts => {
                    tokio::time::sleep(retry.first_delay * 2u32.pow(failures)).await;
                    failures += 1;
                    // The failed request may or may not have landed; ask before resending.
                    progress = self.upload_status(session).await;
                    continue;
                }
                Err(err) => return Err(err),
            };

            let remaining = session.total - received;
            let mut chunk = vec![0; remaining.min(chunk_size as u64) as usize];
            source.seek(SeekFrom::Start(received)).await?;
            source.read_exact(&mut chunk).await?;
            progress = self.upload_chunk(session, received, chunk).await;
        }
    }
}

async fn progress_from(response: reqwest::Response) -> Result<UploadProgress> {
    match response.status() {
        StatusCode::OK | StatusCode::CREATED => {
            let file: RemoteFile = response.json().await?;
            Ok(UploadProgress::Complete(file.into_entry()))
        }
        StatusCode::PERMANENT_REDIRECT => {
            let received = response
                .headers()
                .get(RANGE)
                .and_then(|value| value.to_str().ok())
                .map_or(Ok(0), parse_received)?;
            Ok(UploadProgress::Partial { received })
        }
        StatusCode::NOT_FOUND | StatusCode::GONE => Err(Error::UploadSessionExpired),
        _ => Err(api_error(response).await),
    }
}

/// Parses Drive's `Range: bytes=0-N` into the count of bytes stored.
fn parse_received(range: &str) -> Result<u64> {
    range
        .strip_prefix("bytes=0-")
        .and_then(|last| last.parse::<u64>().ok())
        .map(|last| last + 1)
        .ok_or_else(|| Error::Api {
            status: 308,
            message: format!("unexpected upload range {range:?}"),
        })
}

/// Network failures, rate limits and server errors are worth retrying.
fn is_transient(err: &Error) -> bool {
    match err {
        Error::Http(_) => true,
        Error::Api { status, .. } => *status == 429 || *status >= 500,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_received_ranges() {
        assert_eq!(parse_received("bytes=0-262143").unwrap(), 262_144);
        assert!(parse_received("bytes=5-9").is_err());
    }

    #[test]
    fn classifies_transient_errors() {
        let api = |status| Error::Api {
            status,
            message: String::new(),
        };
        assert!(is_transient(&api(503)));
        assert!(is_transient(&api(429)));
        assert!(!is_transient(&api(403)));
        assert!(!is_transient(&Error::UploadSessionExpired));
    }
}
