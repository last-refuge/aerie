//! Drive API v3 requests used by the browser.

use aerie_core::{Capabilities, Change, DriveFileId, FileEntry, FileKind};
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{Error, Result};

const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3";
pub(crate) const FILE: &str = "id,name,mimeType,size,modifiedTime,parents,trashed,\
capabilities(canRename,canTrash,canAddChildren,canMoveItemWithinDrive,canDownload,canEdit)";

/// The signed-in Google account.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// Stable for the life of the Google account, unlike the email address.
    pub permission_id: String,
    pub display_name: String,
    pub email_address: String,
}

const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// Remote identity of a file's content. `version` increases on every change to the file,
/// including metadata; `md5_checksum` and `head_revision_id` change only with content.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteVersion {
    pub version: String,
    pub md5_checksum: Option<String>,
    pub head_revision_id: Option<String>,
    pub modified_time: Option<String>,
}

/// One stored revision of a file's content.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Revision {
    pub id: String,
    pub md5_checksum: Option<String>,
    pub modified_time: Option<String>,
}

impl RemoteVersion {
    /// Whether the content differs from `baseline`. Metadata-only edits don't count.
    pub fn content_changed_since(&self, baseline: &RemoteVersion) -> bool {
        match (&self.md5_checksum, &baseline.md5_checksum) {
            (Some(now), Some(then)) => now != then,
            _ => self.head_revision_id != baseline.head_revision_id,
        }
    }
}

/// Revisions written by someone else between `baseline` and Aerie's own `ours`. Drive has
/// no conditional writes, so a non-empty result means Aerie overwrote a concurrent edit,
/// which can still be recovered from the returned revisions.
///
/// If the baseline revision has been pruned, every revision before `ours` modified after
/// the baseline time is treated as intervening, erring towards a conflict copy.
pub fn intervening_revisions(
    revisions: &[Revision],
    baseline: &RemoteVersion,
    ours: &str,
) -> Vec<Revision> {
    let Some(end) = revisions.iter().position(|r| r.id == ours) else {
        return Vec::new();
    };
    let before_ours = &revisions[..end];
    match baseline
        .head_revision_id
        .as_deref()
        .and_then(|head| before_ours.iter().position(|r| r.id == head))
    {
        Some(start) => before_ours[start + 1..].to_vec(),
        None => before_ours
            .iter()
            .filter(|r| match (&r.modified_time, &baseline.modified_time) {
                (Some(revision), Some(base)) => unix_seconds(revision) > unix_seconds(base),
                _ => true,
            })
            .cloned()
            .collect(),
    }
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
            .get(
                "about",
                &[("fields", "user(permissionId,displayName,emailAddress)")],
            )
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

    /// One page of every untrashed item the account can see, for a full inventory.
    /// Returns the entries and the token for the next page, if any.
    pub async fn inventory_page(
        &self,
        page_token: Option<&str>,
    ) -> Result<(Vec<FileEntry>, Option<String>)> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            next_page_token: Option<String>,
            #[serde(default)]
            files: Vec<RemoteFile>,
        }
        let fields = format!("nextPageToken,files({FILE})");
        let mut params = vec![
            ("q", "trashed = false"),
            ("fields", fields.as_str()),
            ("pageSize", "1000"),
            ("spaces", "drive"),
        ];
        if let Some(token) = page_token {
            params.push(("pageToken", token));
        }
        let page: Page = self.get("files", &params).await?;
        Ok((
            page.files.into_iter().map(RemoteFile::into_entry).collect(),
            page.next_page_token,
        ))
    }

    /// Finds an untrashed folder by name directly inside `parent`.
    pub async fn find_folder(&self, parent: &DriveFileId, name: &str) -> Result<Option<FileEntry>> {
        let fields = format!("files({FILE})");
        let query = format!(
            "'{}' in parents and name = '{}' and mimeType = '{FOLDER_MIME}' and trashed = false",
            escape(parent.as_str()),
            escape(name)
        );
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            files: Vec<RemoteFile>,
        }
        let page: Page = self
            .get(
                "files",
                &[
                    ("q", query.as_str()),
                    ("fields", fields.as_str()),
                    ("spaces", "drive"),
                ],
            )
            .await?;
        Ok(page.files.into_iter().next().map(RemoteFile::into_entry))
    }

    pub async fn create_folder(&self, parent: &DriveFileId, name: &str) -> Result<FileEntry> {
        let response = self
            .http
            .post(format!("{}/files", self.api_base))
            .bearer_auth(&self.access_token)
            .query(&[("fields", FILE)])
            .json(&serde_json::json!({
                "name": name,
                "mimeType": FOLDER_MIME,
                "parents": [parent.as_str()],
            }))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(api_error(response).await);
        }
        Ok(response.json::<RemoteFile>().await?.into_entry())
    }

    pub async fn rename(&self, id: &DriveFileId, name: &str) -> Result<FileEntry> {
        self.update(id, serde_json::json!({ "name": name }), &[])
            .await
    }

    /// Moves an item from one folder to another.
    pub async fn move_to(
        &self,
        id: &DriveFileId,
        from: &DriveFileId,
        to: &DriveFileId,
    ) -> Result<FileEntry> {
        self.update(
            id,
            serde_json::json!({}),
            &[
                ("addParents", to.as_str()),
                ("removeParents", from.as_str()),
            ],
        )
        .await
    }

    /// Moves an item to Drive's trash, where it stays recoverable. Aerie never deletes
    /// permanently.
    pub async fn trash(&self, id: &DriveFileId) -> Result<FileEntry> {
        self.update(id, serde_json::json!({ "trashed": true }), &[])
            .await
    }

    pub async fn untrash(&self, id: &DriveFileId) -> Result<FileEntry> {
        self.update(id, serde_json::json!({ "trashed": false }), &[])
            .await
    }

    async fn update(
        &self,
        id: &DriveFileId,
        metadata: serde_json::Value,
        params: &[(&str, &str)],
    ) -> Result<FileEntry> {
        let response = self
            .http
            .patch(format!("{}/files/{}", self.api_base, id.as_str()))
            .bearer_auth(&self.access_token)
            .query(&[("fields", FILE)])
            .query(params)
            .json(&metadata)
            .send()
            .await?;
        tracing::debug!(status = response.status().as_u16(), "Drive update");
        if !response.status().is_success() {
            return Err(api_error(response).await);
        }
        Ok(response.json::<RemoteFile>().await?.into_entry())
    }

    /// Downloads an ordinary file's content. Google-native documents need an export instead.
    pub async fn download(&self, id: &DriveFileId) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(format!("{}/files/{}", self.api_base, id.as_str()))
            .bearer_auth(&self.access_token)
            .query(&[("alt", "media")])
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(api_error(response).await);
        }
        Ok(response.bytes().await?.to_vec())
    }

    /// What identifies the current remote content, for comparing against a sync baseline.
    pub async fn version(&self, id: &DriveFileId) -> Result<RemoteVersion> {
        self.get(
            &format!("files/{}", id.as_str()),
            &[("fields", "version,md5Checksum,headRevisionId,modifiedTime")],
        )
        .await
    }

    /// The file's stored revisions, oldest first.
    pub async fn revisions(&self, id: &DriveFileId) -> Result<Vec<Revision>> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            revisions: Vec<Revision>,
        }
        let page: Page = self
            .get(
                &format!("files/{}/revisions", id.as_str()),
                &[
                    ("fields", "revisions(id,md5Checksum,modifiedTime)"),
                    ("pageSize", "1000"),
                ],
            )
            .await?;
        Ok(page.revisions)
    }

    /// Downloads the content of one stored revision.
    pub async fn download_revision(&self, id: &DriveFileId, revision: &str) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(format!(
                "{}/files/{}/revisions/{revision}",
                self.api_base,
                id.as_str()
            ))
            .bearer_auth(&self.access_token)
            .query(&[("alt", "media")])
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(api_error(response).await);
        }
        Ok(response.bytes().await?.to_vec())
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
        tracing::debug!(
            endpoint = path_kind(path),
            status = response.status().as_u16(),
            "Drive request"
        );
        if response.status().is_success() {
            return Ok(response.json().await?);
        }
        Err(api_error(response).await)
    }
}

/// The endpoint without file IDs, for logs.
fn path_kind(path: &str) -> &str {
    path.split('/').next().unwrap_or(path)
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
    #[serde(default)]
    capabilities: RemoteCapabilities,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RemoteCapabilities {
    can_rename: bool,
    can_trash: bool,
    can_add_children: bool,
    can_move_item_within_drive: bool,
    can_download: bool,
    can_edit: bool,
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
        let can = &self.capabilities;
        FileEntry {
            can: Capabilities {
                rename: can.can_rename,
                trash: can.can_trash,
                add_children: can.can_add_children,
                move_item: can.can_move_item_within_drive,
                download: can.can_download,
                edit: can.can_edit,
            },
            kind: kind_for_mime(&self.mime_type),
            size: self.size.and_then(|size| size.parse().ok()),
            modified: self.modified_time.as_deref().map_or(0, unix_seconds),
            parent: self.parents.into_iter().next().map(DriveFileId::new),
            id: DriveFileId::new(self.id),
            name: self.name,
            mime_type: self.mime_type,
        }
    }
}

fn kind_for_mime(mime: &str) -> FileKind {
    match mime {
        FOLDER_MIME => FileKind::Folder,
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

    fn revision(id: &str, time: &str) -> Revision {
        Revision {
            id: id.into(),
            md5_checksum: None,
            modified_time: Some(time.into()),
        }
    }

    fn baseline(head: &str, time: &str) -> RemoteVersion {
        RemoteVersion {
            version: "1".into(),
            md5_checksum: None,
            head_revision_id: Some(head.into()),
            modified_time: Some(time.into()),
        }
    }

    #[test]
    fn finds_revisions_written_between_baseline_and_ours() {
        let revisions = [
            revision("a", "2026-10-20T00:00:00Z"),
            revision("b", "2026-10-20T00:01:00Z"),
            revision("c", "2026-10-20T00:02:00Z"),
        ];
        let base = baseline("a", "2026-10-20T00:00:00Z");
        assert_eq!(
            intervening_revisions(&revisions, &base, "c"),
            [revisions[1].clone()]
        );
        assert!(intervening_revisions(&revisions[..1], &base, "a").is_empty());
        assert!(
            intervening_revisions(&[revisions[0].clone(), revisions[2].clone()], &base, "c")
                .is_empty()
        );
    }

    #[test]
    fn falls_back_to_time_when_the_baseline_was_pruned() {
        let revisions = [
            revision("b", "2026-10-20T00:01:00Z"),
            revision("c", "2026-10-20T00:02:00Z"),
        ];
        let base = baseline("gone", "2026-10-20T00:00:30Z");
        assert_eq!(
            intervening_revisions(&revisions, &base, "c"),
            [revisions[0].clone()]
        );
    }

    #[test]
    fn compares_content_not_metadata() {
        let mut then = baseline("a", "2026-10-20T00:00:00Z");
        then.md5_checksum = Some("x".into());
        let mut now = then.clone();
        now.version = "5".into();
        now.head_revision_id = Some("b".into());
        assert!(!now.content_changed_since(&then));
        now.md5_checksum = Some("y".into());
        assert!(now.content_changed_since(&then));
    }

    #[test]
    fn escapes_query_values() {
        assert_eq!(escape(r"it's\x"), r"it\'s\\x");
    }
}
