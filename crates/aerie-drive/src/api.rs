//! Drive API v3 requests used by the browser.

use aerie_core::{Capabilities, Change, DriveFileId, FileEntry, FileKind};
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{Error, Result};

const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3";
pub(crate) const FILE: &str = "id,name,mimeType,size,modifiedTime,parents,trashed,md5Checksum,headRevisionId,driveId,\
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

/// How much of the account's storage is used, in bytes. `limit` is absent for accounts
/// without one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Storage {
    pub used: u64,
    pub limit: Option<u64>,
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

/// Which change feed to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Feed {
    /// My Drive and every shared drive, in one feed.
    Everything,
    /// My Drive and items shared with the user, without shared drives.
    MyDrive,
    /// One shared drive's own feed.
    SharedDrive(DriveFileId),
}

/// Which set of items an inventory lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Corpus {
    /// My Drive and items shared with the user.
    User,
    /// One shared drive, by ID.
    SharedDrive(DriveFileId),
}

/// A shared drive the user belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedDrive {
    pub id: DriveFileId,
    pub name: String,
    pub can_add_children: bool,
}

impl SharedDrive {
    /// The drive as a folder at the top of its own tree, so browsing, sync roots and the
    /// cache treat it like any other folder. Its own ID is the parent of its top items.
    pub fn as_entry(&self) -> FileEntry {
        FileEntry {
            id: self.id.clone(),
            parent: None,
            name: self.name.clone(),
            kind: FileKind::Folder,
            size: None,
            modified: 0,
            mime_type: SHARED_DRIVE_MIME.to_owned(),
            can: Capabilities {
                add_children: self.can_add_children,
                download: true,
                ..Capabilities::default()
            },
            md5: None,
            head_revision: None,
            drive: Some(self.id.clone()),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteDrive {
    id: String,
    name: String,
    #[serde(default)]
    capabilities: RemoteDriveCapabilities,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RemoteDriveCapabilities {
    can_add_children: bool,
}

impl RemoteDrive {
    fn into_drive(self) -> SharedDrive {
        SharedDrive {
            id: DriveFileId::new(self.id),
            name: self.name,
            can_add_children: self.capabilities.can_add_children,
        }
    }
}

/// Marks the folder Aerie stores for a shared drive. Items shared with the user can also
/// lack a parent, so the type, not the missing parent, identifies a drive.
pub const SHARED_DRIVE_MIME: &str = "application/vnd.aerie.shared-drive";

/// Drive's parameters for reaching items in shared drives as well as My Drive.
const ALL_DRIVES: [(&str, &str); 1] = [("supportsAllDrives", "true")];

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

#[derive(Clone)]
pub struct DriveApi {
    pub(crate) http: reqwest::Client,
    /// Shared by every clone, so the whole engine keeps one pace.
    pub(crate) pacer: std::sync::Arc<crate::pace::Pacer>,
    pub(crate) access_token: String,
    pub(crate) api_base: String,
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
            pacer: std::sync::Arc::default(),
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

    /// The account's storage use across Drive, Gmail and Photos.
    pub async fn storage(&self) -> Result<Storage> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct About {
            storage_quota: Quota,
        }
        // Int64 values arrive as strings.
        #[derive(Deserialize)]
        struct Quota {
            usage: Option<String>,
            limit: Option<String>,
        }
        let about: About = self
            .get("about", &[("fields", "storageQuota(usage,limit)")])
            .await?;
        let number = |value: Option<String>| value.and_then(|value| value.parse().ok());
        Ok(Storage {
            used: number(about.storage_quota.usage).unwrap_or(0),
            limit: number(about.storage_quota.limit),
        })
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
        corpus: &Corpus,
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
        match corpus {
            Corpus::User => params.push(("corpora", "user")),
            Corpus::SharedDrive(id) => params.extend([
                ("corpora", "drive"),
                ("driveId", id.as_str()),
                ("includeItemsFromAllDrives", "true"),
            ]),
        }
        if let Some(token) = page_token {
            params.push(("pageToken", token));
        }
        let page: Page = self.get("files", &params).await?;
        Ok((
            page.files.into_iter().map(RemoteFile::into_entry).collect(),
            page.next_page_token,
        ))
    }

    /// Lists a folder inside a shared drive, for browsing a drive that isn't indexed.
    pub async fn list_drive_folder(
        &self,
        drive: &DriveFileId,
        folder: &DriveFileId,
    ) -> Result<Vec<FileEntry>> {
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
                ("corpora", "drive"),
                ("driveId", drive.as_str()),
                ("includeItemsFromAllDrives", "true"),
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

    /// Asks Google's own search for names containing `text`, across everything the user
    /// can see. Used for drives that aren't indexed locally; results may be partial.
    pub async fn search_names(&self, text: &str, limit: usize) -> Result<Vec<FileEntry>> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            files: Vec<RemoteFile>,
        }
        let fields = format!("files({FILE})");
        let query = format!("name contains '{}' and trashed = false", escape(text));
        let limit = limit.min(1000).to_string();
        let page: Page = self
            .get(
                "files",
                &[
                    ("q", query.as_str()),
                    ("fields", fields.as_str()),
                    ("pageSize", limit.as_str()),
                    ("corpora", "allDrives"),
                    ("includeItemsFromAllDrives", "true"),
                ],
            )
            .await?;
        Ok(page.files.into_iter().map(RemoteFile::into_entry).collect())
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
            .send(
                self.http
                    .post(format!("{}/files", self.api_base))
                    .bearer_auth(&self.access_token)
                    .query(&[("fields", FILE)])
                    .query(&ALL_DRIVES)
                    .json(&serde_json::json!({
                        "name": name,
                        "mimeType": FOLDER_MIME,
                        "parents": [parent.as_str()],
                    })),
            )
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
            .send(
                self.http
                    .patch(format!("{}/files/{}", self.api_base, id.as_str()))
                    .bearer_auth(&self.access_token)
                    .query(&[("fields", FILE)])
                    .query(&ALL_DRIVES)
                    .query(params)
                    .json(&metadata),
            )
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
            .send(
                self.http
                    .get(format!("{}/files/{}", self.api_base, id.as_str()))
                    .bearer_auth(&self.access_token)
                    .query(&[("alt", "media")])
                    .query(&ALL_DRIVES),
            )
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
            .send(
                self.http
                    .get(format!(
                        "{}/files/{}/revisions/{revision}",
                        self.api_base,
                        id.as_str()
                    ))
                    .bearer_auth(&self.access_token)
                    .query(&[("alt", "media")])
                    .query(&ALL_DRIVES),
            )
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
    pub async fn start_page_token(&self, feed: &Feed) -> Result<String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Start {
            start_page_token: String,
        }
        let params: Vec<(&str, &str)> = match feed {
            Feed::SharedDrive(id) => vec![("driveId", id.as_str())],
            Feed::Everything | Feed::MyDrive => vec![],
        };
        let start: Start = self.get("changes/startPageToken", &params).await?;
        Ok(start.start_page_token)
    }

    /// The shared drives the user belongs to.
    pub async fn shared_drives(&self) -> Result<Vec<SharedDrive>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            next_page_token: Option<String>,
            #[serde(default)]
            drives: Vec<RemoteDrive>,
        }
        let mut drives = Vec::new();
        let mut page_token = None::<String>;
        loop {
            let mut params = vec![
                (
                    "fields",
                    "nextPageToken,drives(id,name,capabilities(canAddChildren))",
                ),
                ("pageSize", "100"),
            ];
            if let Some(token) = &page_token {
                params.push(("pageToken", token));
            }
            let page: Page = self.get("drives", &params).await?;
            drives.extend(page.drives.into_iter().map(RemoteDrive::into_drive));
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => return Ok(drives),
            }
        }
    }

    /// Reads one page of changes across My Drive and shared drives. Persist the page's changes and its cursor
    /// together, then continue from the cursor.
    pub async fn changes(&self, feed: &Feed, page_token: &str) -> Result<ChangePage> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            next_page_token: Option<String>,
            new_start_page_token: Option<String>,
            #[serde(default)]
            changes: Vec<RemoteChange>,
        }

        let fields = format!(
            "nextPageToken,newStartPageToken,changes(changeType,fileId,driveId,removed,\
             file({FILE}),drive(id,name,capabilities(canAddChildren)))"
        );
        let mut params = vec![
            ("pageToken", page_token),
            ("fields", fields.as_str()),
            ("pageSize", "1000"),
            ("spaces", "drive"),
            ("includeRemoved", "true"),
        ];
        match feed {
            Feed::Everything => params.push(("includeItemsFromAllDrives", "true")),
            Feed::MyDrive => params.push(("includeItemsFromAllDrives", "false")),
            Feed::SharedDrive(id) => {
                params.extend([
                    ("driveId", id.as_str()),
                    ("includeItemsFromAllDrives", "true"),
                ]);
            }
        }
        let page: Page = self.get("changes", &params).await?;
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
            .send(
                self.http
                    .get(format!("{}/{path}", self.api_base))
                    .bearer_auth(&self.access_token)
                    .query(params)
                    .query(
                        if path.starts_with("files") || path.starts_with("changes") {
                            &ALL_DRIVES[..]
                        } else {
                            &[]
                        },
                    ),
            )
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
    let body = response
        .json::<serde_json::Value>()
        .await
        .unwrap_or_default();
    let reason = body["error"]["errors"][0]["reason"].as_str().unwrap_or("");
    if status.as_u16() == 429 || crate::pace::is_rate_limit_reason(reason) {
        return Error::RateLimited;
    }
    let message = body["error"]["message"]
        .as_str()
        .map_or_else(|| status.to_string(), str::to_owned);
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
    md5_checksum: Option<String>,
    head_revision_id: Option<String>,
    drive_id: Option<String>,
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
    drive_id: Option<String>,
    #[serde(default)]
    removed: bool,
    file: Option<RemoteFile>,
    drive: Option<RemoteDrive>,
}

impl RemoteChange {
    /// Trashed items leave the browser, as Drive's own My Drive view does.
    fn into_change(self) -> Option<Change> {
        if self.change_type == "drive" {
            // A shared drive was renamed, joined, left or deleted.
            return match self.drive {
                Some(drive) if !self.removed => Some(Change::Upsert(drive.into_drive().as_entry())),
                _ => self
                    .drive_id
                    .map(|id| Change::Removed(DriveFileId::new(id))),
            };
        }
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
            md5: self.md5_checksum,
            head_revision: self.head_revision_id,
            drive: self.drive_id.map(DriveFileId::new),
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
    fn maps_shared_drive_changes_to_top_level_folders() {
        let renamed = change(
            r#"{"changeType":"drive","driveId":"d1","removed":false,
            "drive":{"id":"d1","name":"Team","capabilities":{"canAddChildren":true}}}"#,
        );
        let Some(Change::Upsert(entry)) = renamed else {
            panic!("expected an upsert")
        };
        assert_eq!(entry.name, "Team");
        assert!(entry.is_folder() && entry.parent.is_none());
        assert!(entry.can.add_children && !entry.can.trash && !entry.can.rename);

        let left = change(r#"{"changeType":"drive","driveId":"d1","removed":true}"#);
        assert_eq!(left, Some(Change::Removed(DriveFileId::new("d1"))));
    }

    #[test]
    fn escapes_query_values() {
        assert_eq!(escape(r"it's\x"), r"it\'s\\x");
    }
}
