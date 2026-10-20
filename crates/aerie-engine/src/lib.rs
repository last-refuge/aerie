//! The Drive engine: sign-in, inventory, change polling, transfers and sync. It runs in
//! the `aerie-service` process; the app sends [`Command`]s over a local socket and receives
//! [`Event`]s, so sync keeps running when the window is closed.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use aerie_core::{Change, DriveFileId, FileEntry, redact};
use aerie_drive::api::{ChangeCursor, Corpus, DriveApi, Feed, SharedDrive};
use aerie_drive::auth::{self, DRIVE, DRIVE_FILE, DRIVE_READONLY};
use aerie_drive::client::OAuthClient;
use aerie_drive::{Error as DriveError, credentials, http_client};
use aerie_store::{Account, Direction, Indexing, NewTransfer, Store, TransferState};
use async_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

mod protocol;
mod sync;
pub mod transfers;

pub use protocol::{Hello, PROTOCOL, socket_path};
pub use sync::SyncState;

/// The scopes Aerie asks for: read-only until the user first changes something, then full
/// Drive access. `AERIE_DEV_SCOPE=drive.file` confines a development build to files Aerie
/// creates itself, so write features can be tried on a real account safely.
fn scopes() -> (&'static str, &'static str) {
    match std::env::var("AERIE_DEV_SCOPE").as_deref() {
        Ok("drive.file") => (DRIVE_FILE, DRIVE_FILE),
        _ => (DRIVE_READONLY, DRIVE),
    }
}
const FASTEST_POLL: Duration = Duration::from_secs(10);
const SLOWEST_POLL: Duration = Duration::from_secs(120);
const OFFLINE_RETRY: Duration = Duration::from_secs(30);

#[derive(Debug, Serialize, Deserialize)]
pub enum Command {
    /// Sign in, asking for write access when `write` is set.
    SignIn {
        write: bool,
    },
    CancelSignIn,
    SignOut,
    /// Check for changes now, for example after the user asks to retry.
    Refresh,
    CreateFolder {
        parent: DriveFileId,
        name: String,
    },
    Rename {
        id: DriveFileId,
        name: String,
    },
    Move {
        id: DriveFileId,
        from: DriveFileId,
        to: DriveFileId,
    },
    Trash {
        id: DriveFileId,
    },
    Untrash {
        id: DriveFileId,
    },
    /// Upload local files into a Drive folder.
    Upload {
        files: Vec<LocalFile>,
        parent: DriveFileId,
    },
    /// Download a file, or export a Google-native document as `export_mime`.
    Download {
        entry: FileEntry,
        destination: PathBuf,
        export_mime: Option<String>,
    },
    PauseTransfer(i64),
    ResumeTransfer(i64),
    CancelTransfer(i64),
    ClearFinishedTransfers,
    /// Start syncing the chosen Drive folders into an empty local folder.
    StartSync {
        local_root: PathBuf,
        roots: Vec<DriveFileId>,
    },
    SetSyncRoots(Vec<DriveFileId>),
    PauseSync,
    ResumeSync,
    /// Turn sync off. Files on both sides stay where they are.
    StopSync,
    SyncNow,
    /// Carry out deletions that were held for confirmation.
    AllowHeldDeletions,
    /// Restore items whose deletion was held, instead of deleting them.
    KeepHeldItems,
    /// The local sync folder changed.
    LocalChanged,
    /// Stop the engine; the service exits.
    Shutdown,
    /// Choose whether every shared drive is indexed, or only some.
    SetIndexing(Indexing),
    /// Fetch a folder of a shared drive that isn't indexed, because the user opened it.
    BrowseFolder {
        drive: DriveFileId,
        folder: DriveFileId,
    },
    /// Ask Google's search, for items in drives that aren't indexed.
    SearchRemote(String),
}

/// A local file chosen for upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalFile {
    pub path: PathBuf,
    pub mime_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// Open this sign-in page in the user's browser.
    OpenUri(String),
    SignInStarted,
    SignInFailed(String),
    /// A session exists; the catalog for this account is readable from the store.
    SignedIn(Account),
    /// No usable sign-in: never signed in, signed out, or Google revoked it.
    NeedsSignIn {
        expired: bool,
    },
    SignedOut,
    Indexing {
        files: u64,
    },
    /// The cache reflects Drive as of the last successful check.
    UpToDate,
    /// New metadata is in the store; redraw.
    CatalogChanged,
    Offline,
    Problem(String),
    /// The session is read-only; changing files needs a new consent from Google.
    NeedsWriteAccess,
    /// The transfer list changed; re-read it from the store.
    TransfersChanged,
    Sync(SyncState),
    /// A sync pass is working in this synced folder (`None` when the step is at the top
    /// level), `done` of `total` steps in.
    SyncProgress {
        root: Option<DriveFileId>,
        done: usize,
        total: usize,
    },
    /// Both versions of a file were kept; this is the path of Drive's copy.
    ConflictKept(PathBuf),
    TransferProgress {
        id: i64,
        done: u64,
        total: Option<u64>,
    },
    /// Google's search found these for `query`; they're now in the cache too.
    RemoteResults {
        query: String,
        entries: Vec<FileEntry>,
    },
    /// A folder fetched on demand is in the cache.
    FolderLoaded(DriveFileId),
    /// A file action finished. `undo` restores a trashed item.
    Done {
        message: String,
        undo: Option<DriveFileId>,
    },
}

/// What a background task reports when it finishes. Aborted tasks report nothing; the
/// command that aborted them already updated the state.
enum Completion {
    SignIn(aerie_drive::Result<auth::Tokens>),
    Transfer(i64, aerie_drive::Result<transfers::Outcome>),
    Sync(aerie_drive::Result<aerie_sync::cycle::Report>),
}

/// Commands quick enough to handle between pages of a long inventory, so the app stays
/// usable while it runs.
fn runs_during_inventory(command: &Command) -> bool {
    !matches!(
        command,
        Command::SignIn { .. }
            | Command::Refresh
            | Command::StartSync { .. }
            | Command::SetSyncRoots(_)
            | Command::Shutdown
    )
}

/// Runs the engine until the command channel closes or a [`Command::Shutdown`] arrives.
/// `own` is a sender into `inbox`, for the folder watcher. `busy` reports whether the
/// engine has work that should keep the service alive without any window connected.
pub async fn run(
    events: Sender<Event>,
    inbox: Receiver<Command>,
    own: Sender<Command>,
    busy: tokio::sync::watch::Sender<bool>,
) {
    Worker::new(events, inbox, own, busy).run().await;
}

struct Session {
    account: String,
    scope: &'static str,
    refresh_token: String,
    drive: DriveApi,
    /// Wall-clock, so a token that expired while the computer slept is refreshed.
    expires_at: Option<std::time::SystemTime>,
}

struct Worker {
    events: Sender<Event>,
    inbox: Receiver<Command>,
    /// For the folder watcher, which runs on its own thread.
    own: Sender<Command>,
    busy: tokio::sync::watch::Sender<bool>,
    store: Option<Store>,
    session: Option<Session>,
    sign_in: Option<JoinHandle<()>>,
    sign_in_scope: &'static str,
    transfer: Option<(i64, JoinHandle<()>)>,
    sync: Option<JoinHandle<()>>,
    /// Background tasks report here when they finish.
    completions: (Sender<Completion>, Receiver<Completion>),
    /// Set by the local folder watcher; a sync pass is due.
    sync_due: bool,
    /// How the next pass treats an unusually large set of deletions.
    deletions: aerie_sync::cycle::Deletions,
    /// When the local folder last changed; a pass waits for it to settle.
    local_changed_at: Option<Instant>,
    watcher: Option<notify::RecommendedWatcher>,
    poll_delay: Duration,
    offline: bool,
    /// Commands that arrived during a long inventory, handled once it finishes.
    pending: Vec<Command>,
}

/// A point in an unfinished inventory, saved after every page so that a failure or a
/// restart continues from there instead of listing everything again.
#[derive(serde::Serialize, serde::Deserialize)]
struct Resume {
    /// The shared drive being listed, or `None` for My Drive and shared-with-me.
    corpus: Option<DriveFileId>,
    /// The next page in it; `None` starts the corpus from the top.
    page: Option<String>,
    files: u64,
}

impl Resume {
    const START: Self = Self {
        corpus: None,
        page: None,
        files: 0,
    };
}

impl Worker {
    fn new(
        events: Sender<Event>,
        inbox: Receiver<Command>,
        own: Sender<Command>,
        busy: tokio::sync::watch::Sender<bool>,
    ) -> Self {
        Self {
            events,
            inbox,
            own,
            busy,
            store: None,
            session: None,
            sign_in: None,
            sign_in_scope: DRIVE_READONLY,
            transfer: None,
            sync: None,
            completions: async_channel::unbounded(),
            sync_due: true,
            deletions: aerie_sync::cycle::Deletions::Hold,
            local_changed_at: None,
            watcher: None,
            poll_delay: FASTEST_POLL,
            offline: false,
            pending: Vec::new(),
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.try_send(event);
    }

    async fn run(mut self) {
        match Store::open(&Store::default_path()) {
            Ok(store) => self.store = Some(store),
            Err(err) => {
                tracing::error!(%err, "couldn't open the metadata store");
                self.emit(Event::Problem(format!(
                    "Aerie couldn't open its local database: {err}"
                )));
                return;
            }
        }
        self.restore_session().await;

        loop {
            for command in std::mem::take(&mut self.pending) {
                self.handle(command).await;
            }
            let delay = if self.offline {
                OFFLINE_RETRY
            } else {
                self.poll_delay
            };
            // Settle local changes before syncing them.
            let delay = if self.local_changed_at.is_some() {
                delay.min(sync::SETTLE)
            } else {
                delay
            };
            let polling = self.session.is_some();
            let inbox = self.inbox.clone();
            let completions = self.completions.1.clone();
            tokio::select! {
                command = inbox.recv() => match command {
                    Ok(Command::Shutdown) | Err(_) => return,
                    Ok(command) => self.handle(command).await,
                },
                Ok(done) = completions.recv() => match done {
                    Completion::SignIn(result) => {
                        self.sign_in = None;
                        self.finish_sign_in(result).await;
                    }
                    Completion::Transfer(id, result) => {
                        self.transfer = None;
                        self.finish_transfer(id, result).await;
                    }
                    Completion::Sync(result) => {
                        self.sync = None;
                        self.finish_sync(result).await;
                    }
                },
                () = tokio::time::sleep(delay), if polling => self.check().await,
            }
            self.pump_transfers().await;
            self.pump_sync().await;
            let syncing = self
                .sync_settings()
                .is_some_and(|(_, settings)| !settings.paused);
            let busy = syncing || self.transfer.is_some() || self.sign_in.is_some();
            self.busy
                .send_if_modified(|current| std::mem::replace(current, busy) != busy);
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::SignIn { write } => self.start_sign_in(write).await,
            Command::CancelSignIn => {
                if let Some(task) = self.sign_in.take() {
                    task.abort();
                    self.emit(Event::NeedsSignIn { expired: false });
                }
            }
            Command::SignOut => self.sign_out().await,
            Command::Refresh => {
                self.poll_delay = FASTEST_POLL;
                if self.session.is_some() {
                    self.check().await;
                }
            }
            Command::Upload { files, parent } => self.queue_uploads(files, parent).await,
            Command::Download {
                entry,
                destination,
                export_mime,
            } => self.queue_download(entry, destination, export_mime),
            Command::PauseTransfer(id) => self.stop_transfer(id, TransferState::Paused),
            Command::CancelTransfer(id) => self.stop_transfer(id, TransferState::Cancelled),
            Command::ResumeTransfer(id) => {
                let _ = self
                    .store()
                    .set_transfer_state(id, TransferState::Queued, None);
                self.emit(Event::TransfersChanged);
            }
            Command::StartSync { local_root, roots } => self.start_sync(local_root, roots).await,
            Command::SetSyncRoots(roots) => self.set_sync_roots(roots),
            Command::PauseSync => self.set_sync_paused(true),
            Command::ResumeSync => self.set_sync_paused(false),
            Command::StopSync => self.stop_sync(),
            Command::SyncNow => self.sync_due = true,
            Command::AllowHeldDeletions => {
                self.deletions = aerie_sync::cycle::Deletions::Allow;
                self.sync_due = true;
            }
            Command::KeepHeldItems => {
                self.deletions = aerie_sync::cycle::Deletions::Restore;
                self.sync_due = true;
            }
            Command::LocalChanged => self.local_changed_at = Some(Instant::now()),
            Command::SetIndexing(indexing) => {
                let Some(account) = self.session.as_ref().map(|s| s.account.clone()) else {
                    return;
                };
                if let Err(err) = self.store().set_indexing(&account, &indexing) {
                    return self.emit(Event::Problem(err.to_string()));
                }
                tracing::info!(
                    all = indexing.all,
                    drives = indexing.drives.len(),
                    "indexing changed"
                );
                self.poll_delay = FASTEST_POLL;
                self.check().await;
            }
            Command::BrowseFolder { drive, folder } => self.browse_folder(drive, folder).await,
            Command::SearchRemote(query) => self.search_remote(query).await,
            Command::ClearFinishedTransfers => {
                if let Some(account) = self.session.as_ref().map(|s| s.account.clone()) {
                    let _ = self.store().clear_finished_transfers(&account);
                    self.emit(Event::TransfersChanged);
                }
            }
            action => self.act(action).await,
        }
    }

    fn store(&mut self) -> &mut Store {
        self.store
            .as_mut()
            .expect("the store opens before any command")
    }

    /// Resumes a previous session from the keyring. The cache is usable at once, even
    /// offline; the network check happens in the background.
    async fn restore_session(&mut self) {
        let (read, write) = scopes();
        let stored = match credentials::load_refresh_token(write).await {
            Ok(Some(token)) => Ok(Some((write, token))),
            Ok(None) => credentials::load_refresh_token(read)
                .await
                .map(|token| token.map(|token| (read, token))),
            Err(err) => Err(err),
        };
        let (scope, refresh_token) = match stored {
            Ok(Some(stored)) => stored,
            Ok(None) => return self.emit(Event::NeedsSignIn { expired: false }),
            Err(err) => {
                tracing::error!(%err, "couldn't read the keyring");
                return self.emit(Event::Problem(
                    "Aerie couldn't read your sign-in from the system keyring. Unlock it, then try again.".into(),
                ));
            }
        };
        let Ok(http) = http_client() else { return };
        let mut session = Session {
            account: String::new(),
            scope,
            refresh_token,
            drive: DriveApi::new(http, String::new()),
            // Force a token refresh on the first request.
            expires_at: Some(std::time::SystemTime::now()),
        };

        let account = match self.store().account() {
            Ok(Some(account)) => account,
            // The keyring has a sign-in but the database was lost: ask Google who it is.
            _ => match self.fetch_account(&mut session).await {
                Ok(account) => account,
                Err(DriveError::SignInExpired) => {
                    let _ = credentials::delete_refresh_token(scope).await;
                    return self.emit(Event::NeedsSignIn { expired: false });
                }
                Err(err) => {
                    tracing::warn!(%err, "couldn't identify the saved sign-in");
                    return self.emit(Event::NeedsSignIn { expired: false });
                }
            },
        };
        session.account = account.id.clone();
        let _ = self.store().requeue_interrupted_transfers(&account.id);
        self.session = Some(session);
        self.emit(Event::SignedIn(account));
        self.resume_sync_watch();
        self.check().await;
    }

    async fn fetch_account(&mut self, session: &mut Session) -> aerie_drive::Result<Account> {
        let client = OAuthClient::configured()?;
        let tokens = auth::refresh(&client, &http_client()?, &session.refresh_token).await?;
        session.drive.set_access_token(tokens.access_token);
        session.expires_at = tokens.expires_at;
        let remote = session.drive.account().await?;
        let account = Account {
            id: remote.permission_id,
            email: remote.email_address,
            display_name: remote.display_name,
        };
        self.store().save_account(&account).map_err(store_problem)?;
        Ok(account)
    }

    async fn start_sign_in(&mut self, write: bool) {
        if self.sign_in.is_some() {
            return;
        }
        let (read, write_scope) = scopes();
        let scope = if write { write_scope } else { read };
        self.sign_in_scope = scope;
        let client = match OAuthClient::configured() {
            Ok(client) => client,
            Err(err) => {
                tracing::error!(%err, "no OAuth client file");
                return self.emit(Event::SignInFailed(
                    "Sign-in isn't set up on this computer yet: Aerie's Google client file is missing.".into(),
                ));
            }
        };
        let Ok(http) = http_client() else { return };
        let events = self.events.clone();
        self.emit(Event::SignInStarted);
        let done = self.completions.0.clone();
        self.sign_in = Some(tokio::spawn(async move {
            let result = auth::sign_in(&client, &http, scope, |url| {
                let _ = events.try_send(Event::OpenUri(url.to_string()));
                Ok(())
            })
            .await;
            let _ = done.send(Completion::SignIn(result)).await;
        }));
    }

    async fn finish_sign_in(&mut self, result: aerie_drive::Result<auth::Tokens>) {
        let tokens = match result {
            Ok(tokens) => tokens,
            Err(err) => {
                tracing::warn!(%err, "sign-in failed");
                return self.emit(Event::SignInFailed(err.to_string()));
            }
        };
        let Some(refresh_token) = tokens.refresh_token.clone() else {
            return self.emit(Event::SignInFailed(
                "Google didn't grant lasting access. Try signing in again.".into(),
            ));
        };
        let Ok(http) = http_client() else { return };
        let drive = DriveApi::new(http, tokens.access_token.clone());
        let account = match drive.account().await {
            Ok(account) => account,
            Err(err) => return self.emit(Event::SignInFailed(err.to_string())),
        };
        let scope = self.sign_in_scope;
        if let Err(err) = credentials::save_refresh_token(scope, &refresh_token).await {
            tracing::error!(%err, "couldn't save the sign-in");
            return self.emit(Event::SignInFailed(
                "Aerie couldn't save your sign-in to the system keyring.".into(),
            ));
        }
        let account = Account {
            id: account.permission_id,
            email: account.email_address,
            display_name: account.display_name,
        };
        // Switching accounts drops the previous account's cache.
        if let Ok(Some(previous)) = self.store().account()
            && previous.id != account.id
        {
            let _ = self.store().forget_account(&previous.id);
        }
        if let Err(err) = self.store().save_account(&account) {
            return self.emit(Event::Problem(err.to_string()));
        }
        tracing::info!(account = %redact::email(&account.email), "signed in");
        // A write grant supersedes the read-only one.
        let (read, write) = scopes();
        if scope == write && read != write {
            let _ = credentials::delete_refresh_token(read).await;
        }
        self.session = Some(Session {
            account: account.id.clone(),
            scope,
            refresh_token,
            drive,
            expires_at: tokens.expires_at,
        });
        self.emit(Event::SignedIn(account));
        self.resume_sync_watch();
        self.check().await;
    }

    async fn sign_out(&mut self) {
        if let Some(task) = self.sync.take() {
            task.abort();
        }
        self.watcher = None;
        if let Some((_, task)) = self.transfer.take() {
            task.abort();
        }
        if let Some(session) = self.session.take() {
            let _ = self.store().forget_account(&session.account);
        }
        let (read, write) = scopes();
        for scope in [read, write] {
            if let Err(err) = credentials::delete_refresh_token(scope).await {
                tracing::warn!(%err, "couldn't remove the sign-in from the keyring");
            }
        }
        tracing::info!("signed out");
        self.emit(Event::SignedOut);
    }

    /// Brings the cache up to date: a full inventory when there's no usable one, then
    /// change polling. Network trouble puts Aerie in an offline state and retries later.
    async fn check(&mut self) {
        let result = self.try_check().await;
        match result {
            Ok(changed) => {
                if self.offline {
                    self.offline = false;
                }
                self.poll_delay = if changed {
                    FASTEST_POLL
                } else {
                    (self.poll_delay * 2).min(SLOWEST_POLL)
                };
                self.emit(Event::UpToDate);
                self.sync_due = true;
            }
            Err(err) if err.is_offline() => {
                tracing::info!("offline");
                self.offline = true;
                self.emit(Event::Offline);
            }
            Err(DriveError::Api { status: 401, .. }) => {
                // Google no longer accepts the access token; get a new one and try again.
                tracing::info!("access token rejected; refreshing");
                if let Some(session) = self.session.as_mut() {
                    session.expires_at = Some(std::time::UNIX_EPOCH);
                }
                self.poll_delay = FASTEST_POLL;
            }
            Err(DriveError::RateLimited) => {
                // Not a problem to show: wait as long as polling ever does, then resume.
                tracing::warn!("Google asked Aerie to slow down; waiting");
                self.poll_delay = SLOWEST_POLL;
            }
            Err(DriveError::SignInExpired) => {
                tracing::info!("sign-in expired");
                if let Some(session) = self.session.take() {
                    let _ = credentials::delete_refresh_token(session.scope).await;
                }
                self.emit(Event::NeedsSignIn { expired: true });
            }
            Err(err) => {
                tracing::warn!(%err, "check failed");
                self.emit(Event::Problem(err.to_string()));
            }
        }
    }

    async fn try_check(&mut self) -> aerie_drive::Result<bool> {
        self.ensure_token().await?;
        let session = self.session.as_ref().expect("checked by caller");
        let account = session.account.clone();
        let store = self.store.as_ref().expect("store is open");

        let complete = store.inventory_complete(&account).unwrap_or(false);
        let cursor = store.change_cursor(&account).ok().flatten();
        match cursor.filter(|_| complete) {
            Some(cursor) => self.poll_changes(&account, cursor).await,
            None => self.inventory(&account).await.map(|()| true),
        }
    }

    async fn ensure_token(&mut self) -> aerie_drive::Result<()> {
        let session = self.session.as_mut().expect("checked by caller");
        let fresh = session
            .expires_at
            .is_none_or(|at| at > std::time::SystemTime::now() + Duration::from_secs(60));
        if fresh {
            return Ok(());
        }
        let client = OAuthClient::configured()?;
        let http = http_client()?;
        let tokens = auth::refresh(&client, &http, &session.refresh_token).await?;
        session.drive.set_access_token(tokens.access_token);
        session.expires_at = tokens.expires_at;
        Ok(())
    }

    fn can_write(&self) -> bool {
        let (read, write) = scopes();
        self.session
            .as_ref()
            .is_some_and(|session| read == write || session.scope == write)
    }

    async fn queue_uploads(&mut self, files: Vec<LocalFile>, parent: DriveFileId) {
        if !self.can_write() {
            return self.emit(Event::NeedsWriteAccess);
        }
        let Some(account) = self.session.as_ref().map(|s| s.account.clone()) else {
            return;
        };
        for file in files {
            let Ok(metadata) = std::fs::metadata(&file.path) else {
                continue;
            };
            if !metadata.is_file() {
                // Folders upload with sync, not as manual transfers.
                self.emit(Event::Problem(
                    "Only files can be uploaded here; folders aren't supported yet.".into(),
                ));
                continue;
            }
            let name = file
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let new = NewTransfer {
                direction: Direction::Upload,
                file_id: None,
                parent_id: Some(parent.clone()),
                name,
                local_path: file.path,
                mime_type: file.mime_type,
                export_mime: None,
                total: Some(metadata.len()),
                replaces_mtime: None,
            };
            if let Err(err) = self.store().add_transfer(&account, &new, unix_now()) {
                tracing::error!(%err, "couldn't queue an upload");
            }
        }
        self.emit(Event::TransfersChanged);
    }

    fn queue_download(
        &mut self,
        entry: FileEntry,
        destination: PathBuf,
        export_mime: Option<String>,
    ) {
        let Some(account) = self.session.as_ref().map(|s| s.account.clone()) else {
            return;
        };
        let new = NewTransfer {
            direction: Direction::Download,
            file_id: Some(entry.id),
            parent_id: None,
            name: destination
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or(entry.name),
            replaces_mtime: transfers::modified_time(&destination),
            local_path: destination,
            mime_type: entry.mime_type,
            total: if export_mime.is_some() {
                None
            } else {
                entry.size
            },
            export_mime,
        };
        if let Err(err) = self.store().add_transfer(&account, &new, unix_now()) {
            tracing::error!(%err, "couldn't queue a download");
        }
        self.emit(Event::TransfersChanged);
    }

    /// Pauses or cancels a transfer. A paused download keeps its partial file; a
    /// cancelled one removes it.
    fn stop_transfer(&mut self, id: i64, state: TransferState) {
        if self
            .transfer
            .as_ref()
            .is_some_and(|(running, _)| *running == id)
        {
            let (_, task) = self.transfer.take().expect("checked");
            task.abort();
        }
        if state == TransferState::Cancelled
            && let Ok(Some(transfer)) = self.store().transfer(id)
            && transfer.direction == Direction::Download
        {
            let _ = std::fs::remove_file(transfers::partial_path(&transfer.local_path));
        }
        let _ = self.store().set_transfer_state(id, state, None);
        self.emit(Event::TransfersChanged);
    }

    /// Starts the next queued transfer when nothing else is running.
    async fn pump_transfers(&mut self) {
        if self.transfer.is_some() || self.offline {
            return;
        }
        let Some(account) = self.session.as_ref().map(|s| s.account.clone()) else {
            return;
        };
        let Ok(Some(transfer)) = self.store().next_queued_transfer(&account) else {
            return;
        };
        if transfer.direction == Direction::Upload && !self.can_write() {
            return;
        }
        if let Err(err) = self.ensure_token().await {
            tracing::info!(%err, "transfers wait for a working sign-in");
            return;
        }
        let _ = self
            .store()
            .set_transfer_state(transfer.id, TransferState::Running, None);
        self.emit(Event::TransfersChanged);
        let drive = self.session.as_ref().expect("checked").drive.clone();
        let id = transfer.id;
        let events = self.events.clone();
        let done = self.completions.0.clone();
        let task = tokio::spawn(async move {
            let result = transfers::run(drive, transfer, events, Store::default_path()).await;
            let _ = done.send(Completion::Transfer(id, result)).await;
        });
        self.transfer = Some((id, task));
    }

    async fn finish_transfer(&mut self, id: i64, result: aerie_drive::Result<transfers::Outcome>) {
        let account = self.session.as_ref().map(|s| s.account.clone());
        match result {
            Ok(transfers::Outcome::Uploaded(entry)) => {
                let _ = self.store().set_transfer_file(id, &entry.id);
                let _ = self
                    .store()
                    .set_transfer_state(id, TransferState::Done, None);
                if let Some(account) = account {
                    let _ = self.store().apply_local(&account, &[Change::Upsert(entry)]);
                }
                self.emit(Event::CatalogChanged);
            }
            Ok(transfers::Outcome::Downloaded(path)) => {
                let renamed = self
                    .store()
                    .transfer(id)
                    .ok()
                    .flatten()
                    .is_some_and(|t| t.local_path != path);
                let _ = self.store().set_transfer_path(id, &path);
                let _ = self
                    .store()
                    .set_transfer_state(id, TransferState::Done, None);
                if renamed {
                    self.emit(Event::Problem(format!(
                        "The file on your computer changed during the download, so Aerie saved Drive's copy as “{}”.",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    )));
                }
            }
            Err(err) => {
                let offline = err.is_offline();
                tracing::warn!(transfer = id, %err, "transfer failed");
                // Offline transfers wait in the queue; anything else needs the user.
                let state = if offline {
                    self.offline = true;
                    TransferState::Queued
                } else {
                    TransferState::Failed
                };
                let message = err.to_string();
                let _ = self.store().set_transfer_state(id, state, Some(&message));
            }
        }
        self.emit(Event::TransfersChanged);
    }

    /// Runs a file action and records its result locally at once; the change log confirms
    /// it later. Read-only sessions ask the UI to obtain write access first.
    async fn act(&mut self, action: Command) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if session.scope == scopes().0 && scopes().0 != scopes().1 {
            return self.emit(Event::NeedsWriteAccess);
        }
        if let Err(err) = self.ensure_token().await {
            return self.report(err);
        }
        let session = self.session.as_ref().expect("checked above");
        let drive = &session.drive;
        let account = session.account.clone();
        let result = match &action {
            Command::CreateFolder { parent, name } => drive
                .create_folder(parent, name)
                .await
                .map(|entry| (Change::Upsert(entry), format!("Created “{name}”"), None)),
            Command::Rename { id, name } => drive
                .rename(id, name)
                .await
                .map(|entry| (Change::Upsert(entry), format!("Renamed to “{name}”"), None)),
            Command::Move { id, from, to } => drive.move_to(id, from, to).await.map(|entry| {
                let message = format!("Moved “{}”", entry.name);
                (Change::Upsert(entry), message, None)
            }),
            Command::Trash { id } => drive.trash(id).await.map(|entry| {
                (
                    Change::Removed(entry.id.clone()),
                    format!("Moved “{}” to the trash", entry.name),
                    Some(entry.id),
                )
            }),
            Command::Untrash { id } => drive.untrash(id).await.map(|entry| {
                let message = format!("Restored “{}”", entry.name);
                (Change::Upsert(entry), message, None)
            }),
            _ => return,
        };
        match result {
            Ok((change, message, undo)) => {
                if let Err(err) = self.store().apply_local(&account, &[change]) {
                    tracing::error!(%err, "couldn't record a file action");
                }
                self.poll_delay = FASTEST_POLL;
                self.emit(Event::CatalogChanged);
                self.emit(Event::Done { message, undo });
            }
            Err(err) => self.report(err),
        }
    }

    fn report(&mut self, err: DriveError) {
        tracing::warn!(%err, "file action failed");
        let message = match &err {
            _ if err.is_offline() => "You’re offline. Try again when you’re connected.".to_owned(),
            DriveError::SignInExpired => {
                "Your sign-in expired. Sign in again to continue.".to_owned()
            }
            DriveError::Api { status: 401, .. } => {
                if let Some(session) = self.session.as_mut() {
                    session.expires_at = Some(std::time::UNIX_EPOCH);
                }
                "Aerie had to renew its sign-in. Try that again.".to_owned()
            }
            DriveError::RateLimited => {
                "Google Drive is busy with Aerie’s requests. Try again in a minute.".to_owned()
            }
            DriveError::Api {
                status: 403,
                message,
            } => format!("Google Drive didn’t allow that: {message}"),
            DriveError::Api { status: 404, .. } => {
                "That item is no longer in your Drive.".to_owned()
            }
            other => format!("That didn’t work: {other}"),
        };
        self.emit(Event::Problem(message));
    }

    /// The shared drives to index: all of them, or the chosen ones plus any holding a
    /// folder that syncs.
    fn drives_to_index(
        &mut self,
        account: &str,
        drives: &[SharedDrive],
    ) -> (bool, Vec<DriveFileId>) {
        let indexing = self.store().indexing(account).unwrap_or(Indexing {
            all: true,
            drives: Vec::new(),
        });
        if indexing.all {
            return (true, drives.iter().map(|d| d.id.clone()).collect());
        }
        let synced: Vec<DriveFileId> = self
            .store()
            .sync_roots(account)
            .unwrap_or_default()
            .iter()
            .filter_map(|id| self.store().file(account, id).ok().flatten())
            .filter_map(|entry| entry.drive)
            .collect();
        let chosen = drives
            .iter()
            .map(|d| d.id.clone())
            .filter(|id| indexing.drives.contains(id) || synced.contains(id))
            .collect();
        (false, chosen)
    }

    async fn browse_folder(&mut self, drive: DriveFileId, folder: DriveFileId) {
        let Some(account) = self.session.as_ref().map(|s| s.account.clone()) else {
            return;
        };
        if let Err(err) = self.ensure_token().await {
            return self.report(err);
        }
        let session = self.session.as_ref().expect("checked");
        match session.drive.list_drive_folder(&drive, &folder).await {
            Ok(entries) => {
                let changes: Vec<_> = entries.into_iter().map(Change::Upsert).collect();
                if let Err(err) = self.store().apply_local(&account, &changes) {
                    tracing::error!(%err, "couldn't cache a folder");
                }
                self.emit(Event::FolderLoaded(folder));
                self.emit(Event::CatalogChanged);
            }
            Err(err) if err.is_offline() => self.emit(Event::Offline),
            Err(err) => self.report(err),
        }
    }

    async fn search_remote(&mut self, query: String) {
        let Some(account) = self.session.as_ref().map(|s| s.account.clone()) else {
            return;
        };
        if self.ensure_token().await.is_err() {
            return;
        }
        let session = self.session.as_ref().expect("checked");
        match session.drive.search_names(&query, 100).await {
            Ok(entries) => {
                let changes: Vec<_> = entries.iter().cloned().map(Change::Upsert).collect();
                let _ = self.store().apply_local(&account, &changes);
                self.emit(Event::RemoteResults { query, entries });
            }
            // Search beyond the index is a bonus; failures stay quiet.
            Err(err) => tracing::info!(%err, "remote search failed"),
        }
    }

    /// Lists everything, seeding the change cursor first so edits made meanwhile replay.
    async fn inventory(&mut self, account: &str) -> aerie_drive::Result<()> {
        let session = self.session.as_ref().expect("checked by caller");
        // Shared drives appear as folders at the top of their own trees. Narrow scopes
        // can't list them, which simply means none.
        let drives = match session.drive.shared_drives().await {
            Ok(drives) => drives,
            Err(DriveError::Api { status: 403, .. }) => Vec::new(),
            Err(err) => return Err(err),
        };
        let (all, indexed) = self.drives_to_index(account, &drives);
        let session = self.session.as_ref().expect("checked by caller");
        // `drive.file` can't read the root folder itself; the root is inferred below.
        let root = match session.drive.root_id().await {
            Ok(root) => Some(root),
            Err(DriveError::Api { status: 404, .. }) => None,
            Err(err) => return Err(err),
        };
        // Continue an unfinished inventory when its place is still in the list.
        let saved = self
            .store()
            .inventory_resume(account)
            .ok()
            .flatten()
            .and_then(|saved| serde_json::from_str::<Resume>(&saved).ok())
            .filter(|resume| {
                resume
                    .corpus
                    .as_ref()
                    .is_none_or(|drive| indexed.contains(drive))
            });
        let resume = match saved {
            Some(resume) => {
                tracing::info!(files = resume.files, "continuing an unfinished index");
                resume
            }
            None => {
                self.begin_inventory(account, all, &indexed, &drives, root.as_ref())
                    .await?;
                tracing::info!("indexing every file");
                Resume::START
            }
        };
        let resumed = resume.page.is_some() || resume.corpus.is_some();
        let result = match self.list_corpora(account, &indexed, resume).await {
            // Google no longer accepts the saved page: start over rather than stall.
            Err(DriveError::Api { status: 400, .. }) if resumed => {
                tracing::info!("the saved index position expired; starting over");
                self.begin_inventory(account, all, &indexed, &drives, root.as_ref())
                    .await?;
                self.list_corpora(account, &indexed, Resume::START).await
            }
            result => result,
        };
        match result {
            Ok(true) => {}
            // Sign-out or a new indexing choice ended the inventory early.
            Ok(false) => return Ok(()),
            Err(err) => return Err(err),
        }
        self.store
            .as_mut()
            .expect("store is open")
            .finish_inventory(account)
            .map_err(store_problem)?;
        if root.is_none() {
            let store = self.store.as_ref().expect("store is open");
            if let Ok(Some(inferred)) = store.likely_root(account) {
                store.set_root(account, &inferred).map_err(store_problem)?;
            }
        }
        tracing::info!("inventory complete");
        self.emit(Event::CatalogChanged);
        Ok(())
    }

    /// Starts an inventory afresh: the change cursors to follow afterwards, and the
    /// shared drives themselves.
    async fn begin_inventory(
        &mut self,
        account: &str,
        all: bool,
        indexed: &[DriveFileId],
        drives: &[SharedDrive],
        root: Option<&DriveFileId>,
    ) -> aerie_drive::Result<()> {
        let session = self.session.as_ref().expect("checked by caller");
        let feed = if all { Feed::Everything } else { Feed::MyDrive };
        let cursor = session.drive.start_page_token(&feed).await?;
        let placeholder = DriveFileId::new("root");
        self.store
            .as_mut()
            .expect("store is open")
            .begin_inventory(account, root.unwrap_or(&placeholder), &cursor)
            .map_err(store_problem)?;

        // Each indexed drive gets its own feed when not everything is indexed; seed those
        // cursors before listing too.
        if !all {
            for drive in indexed {
                let session = self.session.as_ref().expect("still signed in");
                let cursor = session
                    .drive
                    .start_page_token(&Feed::SharedDrive(drive.clone()))
                    .await?;
                self.store
                    .as_mut()
                    .expect("store is open")
                    .apply_drive_changes(account, drive, &[], &cursor)
                    .map_err(store_problem)?;
            }
        }
        let drive_entries: Vec<_> = drives.iter().map(SharedDrive::as_entry).collect();
        self.store
            .as_mut()
            .expect("store is open")
            .save_inventory_page(account, &drive_entries)
            .map_err(store_problem)
    }

    /// Lists every file from `from` on, saving its place after each page. Returns false
    /// when a command ended the listing early.
    async fn list_corpora(
        &mut self,
        account: &str,
        indexed: &[DriveFileId],
        from: Resume,
    ) -> aerie_drive::Result<bool> {
        let mut deferred = Vec::new();
        let result = self
            .list_corpora_from(account, indexed, from, &mut deferred)
            .await;
        // Sign-in and sync completions wait for the main loop.
        for done in deferred {
            let _ = self.completions.0.try_send(done);
        }
        result
    }

    async fn list_corpora_from(
        &mut self,
        account: &str,
        indexed: &[DriveFileId],
        from: Resume,
        deferred: &mut Vec<Completion>,
    ) -> aerie_drive::Result<bool> {
        let Resume {
            corpus: first,
            page,
            mut files,
        } = from;
        let mut page = Some(page);
        let corpora: Vec<Option<DriveFileId>> = std::iter::once(None)
            .chain(indexed.iter().cloned().map(Some))
            .collect();
        let start = corpora.iter().position(|c| *c == first).unwrap_or(0);
        for (index, id) in corpora.iter().enumerate().skip(start) {
            let corpus = id.clone().map_or(Corpus::User, Corpus::SharedDrive);
            let mut page_token: Option<String> = page.take().flatten();
            loop {
                // Stay responsive during a long inventory: file actions and transfers
                // go ahead, sign-out and indexing changes end it.
                while let Ok(command) = self.inbox.try_recv() {
                    match command {
                        Command::SignOut => {
                            self.sign_out().await;
                            return Ok(false);
                        }
                        // Stop here; the main loop sees the shutdown and exits. The next
                        // start resumes from the cache, not from this page.
                        Command::Shutdown => {
                            let _ = self.own.try_send(Command::Shutdown);
                            return Ok(false);
                        }
                        // A new indexing choice restarts the inventory to match it.
                        Command::SetIndexing(indexing) => {
                            self.store()
                                .set_indexing(account, &indexing)
                                .map_err(store_problem)?;
                            tracing::info!("indexing changed during inventory; restarting");
                            self.poll_delay = FASTEST_POLL;
                            return Ok(false);
                        }
                        command if runs_during_inventory(&command) => {
                            Box::pin(self.handle(command)).await;
                        }
                        command => self.pending.push(command),
                    }
                }
                while let Ok(done) = self.completions.1.try_recv() {
                    match done {
                        Completion::Transfer(id, result) => {
                            self.transfer = None;
                            Box::pin(self.finish_transfer(id, result)).await;
                        }
                        other => deferred.push(other),
                    }
                }
                Box::pin(self.pump_transfers()).await;
                let session = self.session.as_ref().expect("still signed in");
                let (entries, next) = match session
                    .drive
                    .inventory_page(&corpus, page_token.as_deref())
                    .await
                {
                    Ok(page) => page,
                    Err(err) => return Err(err),
                };
                files += entries.len() as u64;
                self.store
                    .as_mut()
                    .expect("store is open")
                    .save_inventory_page(account, &entries)
                    .map_err(store_problem)?;
                // The next page, or the start of the next corpus.
                let place = match &next {
                    Some(token) => Resume {
                        corpus: id.clone(),
                        page: Some(token.clone()),
                        files,
                    },
                    None => Resume {
                        corpus: corpora.get(index + 1).cloned().flatten(),
                        page: None,
                        files,
                    },
                };
                if next.is_some() || index + 1 < corpora.len() {
                    let place = serde_json::to_string(&place).expect("serializable");
                    if let Err(err) = self.store().set_inventory_resume(account, Some(&place)) {
                        tracing::warn!(%err, "couldn't save the index position");
                    }
                }
                self.emit(Event::Indexing { files });
                self.emit(Event::CatalogChanged);
                match next {
                    Some(token) => page_token = Some(token),
                    None => break,
                }
            }
        }
        tracing::info!(files, "listed every file");
        Ok(true)
    }

    /// Applies every waiting page of changes. Returns whether anything changed.
    async fn poll_changes(&mut self, account: &str, cursor: String) -> aerie_drive::Result<bool> {
        let all = self.store().indexing(account).map_or(true, |i| i.all);
        let feed = if all { Feed::Everything } else { Feed::MyDrive };
        let mut changed = self.poll_feed(account, &feed, cursor).await?;
        if !all {
            let (listed, drives) = self.refresh_drive_list(account).await?;
            changed |= listed;
            let (_, indexed) = self.drives_to_index(account, &drives);
            for drive in indexed {
                let Some(cursor) = self.store().drive_cursor(account, &drive).ok().flatten() else {
                    // A drive chosen since the last inventory: index everything afresh.
                    self.inventory(account).await?;
                    return Ok(true);
                };
                changed |= self
                    .poll_feed(account, &Feed::SharedDrive(drive), cursor)
                    .await?;
            }
        }
        if changed {
            self.emit(Event::CatalogChanged);
        }
        Ok(changed)
    }

    /// Keeps the list of shared drives current when the main feed leaves drives out.
    async fn refresh_drive_list(
        &mut self,
        account: &str,
    ) -> aerie_drive::Result<(bool, Vec<SharedDrive>)> {
        let session = self.session.as_ref().expect("checked by caller");
        let drives = match session.drive.shared_drives().await {
            Ok(drives) => drives,
            Err(DriveError::Api { status: 403, .. }) => return Ok((false, Vec::new())),
            Err(err) => return Err(err),
        };
        let known = self.store().shared_drives(account).map_err(store_problem)?;
        let mut changes: Vec<Change> = drives
            .iter()
            .map(SharedDrive::as_entry)
            .filter(|entry| !known.contains(entry))
            .map(Change::Upsert)
            .collect();
        changes.extend(
            known
                .iter()
                .filter(|entry| !drives.iter().any(|d| d.id == entry.id))
                .map(|entry| Change::Removed(entry.id.clone())),
        );
        if changes.is_empty() {
            return Ok((false, drives));
        }
        self.store()
            .apply_local(account, &changes)
            .map_err(store_problem)?;
        Ok((true, drives))
    }

    /// Applies every waiting page of one feed. Returns whether anything changed.
    async fn poll_feed(
        &mut self,
        account: &str,
        feed: &Feed,
        mut cursor: String,
    ) -> aerie_drive::Result<bool> {
        let mut changed = false;
        loop {
            let session = self.session.as_ref().expect("checked by caller");
            let page = match session.drive.changes(feed, &cursor).await {
                Ok(page) => page,
                // An unusable cursor means starting over with a fresh inventory, which
                // never treats missing results as deletions.
                Err(DriveError::Api {
                    status: 400 | 404 | 410,
                    ..
                }) => {
                    tracing::warn!("change cursor rejected; re-inventorying");
                    self.inventory(account).await?;
                    return Ok(true);
                }
                Err(err) => return Err(err),
            };
            changed |= !page.changes.is_empty();
            let next = match &page.next {
                ChangeCursor::MorePages(token) | ChangeCursor::CaughtUp(token) => token.clone(),
            };
            let store = self.store.as_mut().expect("store is open");
            match feed {
                Feed::SharedDrive(drive) => {
                    store.apply_drive_changes(account, drive, &page.changes, &next)
                }
                Feed::Everything | Feed::MyDrive => {
                    store.apply_changes(account, &page.changes, &next)
                }
            }
            .map_err(store_problem)?;
            cursor = next;
            if matches!(page.next, ChangeCursor::CaughtUp(_)) {
                break;
            }
        }
        Ok(changed)
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

fn store_problem(err: aerie_store::Error) -> DriveError {
    DriveError::Io(std::io::Error::other(err.to_string()))
}
