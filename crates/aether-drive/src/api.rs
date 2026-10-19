//! Drive API v3 requests used by the browser.

use aether_core::{DriveFileId, FileEntry, FileKind};
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{Error, Result};

const API: &str = "https://www.googleapis.com/drive/v3";
const FILE_FIELDS: &str = "nextPageToken,files(id,name,mimeType,size,modifiedTime,parents)";

/// The signed-in Google account.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub display_name: String,
    pub email_address: String,
}

pub struct DriveApi {
    http: reqwest::Client,
    access_token: String,
}

impl DriveApi {
    pub fn new(http: reqwest::Client, access_token: String) -> Self {
        Self { http, access_token }
    }

    pub async fn account(&self) -> Result<Account> {
        #[derive(Deserialize)]
        struct About {
            user: Account,
        }
        let about: About = self
            .get("about", &[("fields", "user(displayName,emailAddress)")])
            .await?;
        Ok(about.user)
    }

    /// Lists a folder's untrashed children, following every page. `"root"` is My Drive.
    pub async fn list_children(&self, folder: &DriveFileId) -> Result<Vec<FileEntry>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            next_page_token: Option<String>,
            #[serde(default)]
            files: Vec<RemoteFile>,
        }

        let query = format!(
            "'{}' in parents and trashed = false",
            escape(folder.as_str())
        );
        let mut entries = Vec::new();
        let mut page_token = None::<String>;
        loop {
            let mut params = vec![
                ("q", query.as_str()),
                ("fields", FILE_FIELDS),
                ("pageSize", "1000"),
                ("spaces", "drive"),
            ];
            if let Some(token) = &page_token {
                params.push(("pageToken", token));
            }
            let page: Page = self.get("files", &params).await?;
            entries.extend(page.files.into_iter().map(RemoteFile::into_entry));
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => return Ok(entries),
            }
        }
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        let response = self
            .http
            .get(format!("{API}/{path}"))
            .bearer_auth(&self.access_token)
            .query(params)
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            return Ok(response.json().await?);
        }
        let message = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|body| body["error"]["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| status.to_string());
        Err(Error::Api {
            status: status.as_u16(),
            message,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteFile {
    id: String,
    name: String,
    mime_type: String,
    /// Int64 values arrive as strings; absent for folders and Google-native files.
    size: Option<String>,
    modified_time: Option<String>,
    #[serde(default)]
    parents: Vec<String>,
}

impl RemoteFile {
    fn into_entry(self) -> FileEntry {
        FileEntry {
            kind: kind_for_mime(&self.mime_type),
            size: self.size.and_then(|size| size.parse().ok()),
            modified: self.modified_time.as_deref().map_or(0, unix_seconds),
            parent: self.parents.into_iter().next().map(DriveFileId::new),
            id: DriveFileId::new(self.id),
            name: self.name,
        }
    }
}

fn kind_for_mime(mime: &str) -> FileKind {
    match mime {
        "application/vnd.google-apps.folder" => FileKind::Folder,
        "application/pdf" => FileKind::Pdf,
        "application/vnd.google-apps.document"
        | "application/msword"
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        | "application/vnd.oasis.opendocument.text" => FileKind::Document,
        "application/vnd.google-apps.spreadsheet"
        | "application/vnd.ms-excel"
        | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.oasis.opendocument.spreadsheet"
        | "text/csv" => FileKind::Spreadsheet,
        "application/vnd.google-apps.presentation"
        | "application/vnd.ms-powerpoint"
        | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        | "application/vnd.oasis.opendocument.presentation" => FileKind::Presentation,
        _ if mime.starts_with("image/") => FileKind::Image,
        _ => FileKind::Other,
    }
}

fn unix_seconds(rfc3339: &str) -> i64 {
    OffsetDateTime::parse(rfc3339, &Rfc3339).map_or(0, OffsetDateTime::unix_timestamp)
}

/// Escapes a value for a single-quoted Drive query string.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_drive_files_to_entries() {
        let file: RemoteFile = serde_json::from_str(
            r#"{"id":"1a","name":"Plan.pdf","mimeType":"application/pdf","size":"8100000",
            "modifiedTime":"2026-10-17T11:00:00.000Z","parents":["root-id"]}"#,
        )
        .unwrap();
        let entry = file.into_entry();
        assert_eq!(entry.kind, FileKind::Pdf);
        assert_eq!(entry.size, Some(8_100_000));
        assert_eq!(entry.modified, 1_792_234_800);
        assert_eq!(entry.parent, Some(DriveFileId::new("root-id")));
    }

    #[test]
    fn google_native_files_have_no_size() {
        let file: RemoteFile = serde_json::from_str(
            r#"{"id":"1b","name":"Notes","mimeType":"application/vnd.google-apps.document"}"#,
        )
        .unwrap();
        let entry = file.into_entry();
        assert_eq!(entry.kind, FileKind::Document);
        assert_eq!(entry.size, None);
    }

    #[test]
    fn classifies_common_types() {
        assert_eq!(
            kind_for_mime("application/vnd.google-apps.folder"),
            FileKind::Folder
        );
        assert_eq!(kind_for_mime("image/png"), FileKind::Image);
        assert_eq!(kind_for_mime("application/zip"), FileKind::Other);
    }

    #[test]
    fn escapes_query_values() {
        assert_eq!(escape(r"it's\x"), r"it\'s\\x");
    }
}
