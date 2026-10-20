//! Drive API v3 requests used by the browser.

use aerie_core::{Change, DriveFileId, FileEntry, FileKind};
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{Error, Result};

const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3";
pub(crate) const FILE: &str = "id,name,mimeType,size,modifiedTime,parents,trashed";

/// The signed-in Google account.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub display_name: String,
    pub email_address: String,
}

/// One page of the change log.
#[derive(Debug)]
pub struct ChangePage {
    pub changes: Vec<Change>,
    pub next: ChangeCursor,
}

/// Where to continue reading the change log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeCursor {
    /// More pages are waiting; request this token now.
    MorePages(String),
    /// Caught up; poll this token later for new changes.
    CaughtUp(String),
}

pub struct DriveApi {
    pub(crate) http: reqwest::Client,
    pub(crate) access_token: String,
    api_base: String,
    pub(crate) upload_base: String,
}

impl DriveApi {
    pub fn new(http: reqwest::Client, access_token: String) -> Self {
        Self::with_base_urls(http, access_token, API, UPLOAD_API)
    }

    /// Points the client at a simulated Drive service, for tests.
    pub fn with_base_urls(
        http: reqwest::Client,
        access_token: String,
        api_base: &str,
        upload_base: &str,
    ) -> Self {
        Self {
            http,
            access_token,
            api_base: api_base.to_owned(),
            upload_base: upload_base.to_owned(),
        }
    }

    /// Replaces the access token after a refresh.
    pub fn set_access_token(&mut self, access_token: String) {
        self.access_token = access_token;
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

        let fields = format!("nextPageToken,files({FILE})");
        let query = format!(
            "'{}' in parents and trashed = false",
            escape(folder.as_str())
        );
        let mut entries = Vec::new();
        let mut page_token = None::<String>;
        loop {
            let mut params = vec![
                ("q", query.as_str()),
                ("fields", fields.as_str()),
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

    /// The My Drive root's real ID. Listings report it as the parent, not `"root"`.
    pub async fn root_id(&self) -> Result<DriveFileId> {
        #[derive(Deserialize)]
        struct Root {
            id: String,
        }
        let root: Root = self.get("files/root", &[("fields", "id")]).await?;
        Ok(DriveFileId::new(root.id))
    }

    /// A cursor for changes made from now on. Fetch it before an initial listing, so
    /// changes made during the listing are replayed rather than lost.
    pub async fn start_page_token(&self) -> Result<String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Start {
            start_page_token: String,
        }
        let start: Start = self.get("changes/startPageToken", &[]).await?;
        Ok(start.start_page_token)
    }

    /// Reads one page of My Drive changes. Persist the page's changes and its cursor
    /// together, then continue from the cursor.
    pub async fn changes(&self, page_token: &str) -> Result<ChangePage> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            next_page_token: Option<String>,
            new_start_page_token: Option<String>,
            #[serde(default)]
            changes: Vec<RemoteChange>,
        }

        let fields = format!(
            "nextPageToken,newStartPageToken,changes(changeType,fileId,removed,file({FILE}))"
        );
        let page: Page = self
            .get(
                "changes",
                &[
                    ("pageToken", page_token),
                    ("fields", fields.as_str()),
                    ("pageSize", "1000"),
                    ("spaces", "drive"),
                    ("includeRemoved", "true"),
                    ("restrictToMyDrive", "true"),
                ],
            )
            .await?;
        let next = match (page.next_page_token, page.new_start_page_token) {
            (Some(token), _) => ChangeCursor::MorePages(token),
            (None, Some(token)) => ChangeCursor::CaughtUp(token),
            (None, None) => {
                return Err(Error::Api {
                    status: 200,
                    message: "the change log returned no cursor".into(),
                });
            }
        };
        Ok(ChangePage {
            changes: page
                .changes
                .into_iter()
                .filter_map(RemoteChange::into_change)
                .collect(),
            next,
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        let response = self
            .http
            .get(format!("{}/{path}", self.api_base))
            .bearer_auth(&self.access_token)
            .query(params)
            .send()
            .await?;
        if response.status().is_success() {
            return Ok(response.json().await?);
        }
        Err(api_error(response).await)
    }
}

/// Turns an unsuccessful response into an error, using Drive's message when present.
pub(crate) async fn api_error(response: reqwest::Response) -> Error {
    let status = response.status();
    let message = response
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|body| body["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| status.to_string());
    Error::Api {
        status: status.as_u16(),
        message,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteFile {
    id: String,
    name: String,
    mime_type: String,
    /// Int64 values arrive as strings; absent for folders and Google-native files.
    size: Option<String>,
    modified_time: Option<String>,
    #[serde(default)]
    parents: Vec<String>,
    #[serde(default)]
    trashed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteChange {
    change_type: String,
    file_id: Option<String>,
    #[serde(default)]
    removed: bool,
    file: Option<RemoteFile>,
}

impl RemoteChange {
    /// Trashed items leave the browser, as Drive's own My Drive view does.
    fn into_change(self) -> Option<Change> {
        if self.change_type != "file" {
            return None;
        }
        match self.file {
            Some(file) if !self.removed && !file.trashed => Some(Change::Upsert(file.into_entry())),
            Some(file) => Some(Change::Removed(DriveFileId::new(file.id))),
            None => self.file_id.map(|id| Change::Removed(DriveFileId::new(id))),
        }
    }
}

impl RemoteFile {
    pub(crate) fn into_entry(self) -> FileEntry {
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

    fn change(json: &str) -> Option<Change> {
        serde_json::from_str::<RemoteChange>(json)
            .unwrap()
            .into_change()
    }

    #[test]
    fn maps_change_log_entries() {
        let edited = change(
            r#"{"changeType":"file","fileId":"1a","removed":false,
            "file":{"id":"1a","name":"Plan.pdf","mimeType":"application/pdf","parents":["r"]}}"#,
        );
        assert!(matches!(edited, Some(Change::Upsert(entry)) if entry.name == "Plan.pdf"));

        let trashed = change(
            r#"{"changeType":"file","fileId":"1a",
            "file":{"id":"1a","name":"Plan.pdf","mimeType":"application/pdf","trashed":true}}"#,
        );
        assert_eq!(trashed, Some(Change::Removed(DriveFileId::new("1a"))));

        let deleted = change(r#"{"changeType":"file","fileId":"1a","removed":true}"#);
        assert_eq!(deleted, Some(Change::Removed(DriveFileId::new("1a"))));

        assert_eq!(change(r#"{"changeType":"drive","removed":false}"#), None);
    }

    #[test]
    fn escapes_query_values() {
        assert_eq!(escape(r"it's\x"), r"it\'s\\x");
    }
}
