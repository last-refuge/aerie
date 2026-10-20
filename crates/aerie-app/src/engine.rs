//! The Drive engine: sign-in, inventory and change polling on a background thread with its
//! own async runtime. The UI sends [`Command`]s and receives [`Event`]s; neither side
//! blocks the other, and the GTK main thread never waits on the network or the disk.

use std::time::{Duration, Instant};

use aerie_core::redact;
use aerie_drive::api::{ChangeCursor, DriveApi};
use aerie_drive::auth::{self, DRIVE_READONLY};
use aerie_drive::client::OAuthClient;
use aerie_drive::{Error as DriveError, credentials, http_client};
use aerie_store::{Account, Store};
use async_channel::{Receiver, Sender};
use tokio::task::JoinHandle;

/// The OAuth scope the current milestone needs.
const SCOPE: &str = DRIVE_READONLY;
const FASTEST_POLL: Duration = Duration::from_secs(10);
const SLOWEST_POLL: Duration = Duration::from_secs(120);
const OFFLINE_RETRY: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum Command {
    SignIn,
    CancelSignIn,
    SignOut,
    /// Check for changes now, for example after the user asks to retry.
    Refresh,
}

#[derive(Debug)]
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
}

pub struct Engine {
    commands: Sender<Command>,
}

impl Engine {
    pub fn start(events: Sender<Event>) -> Self {
        let (commands, inbox) = async_channel::unbounded();
        std::thread::Builder::new()
            .name("aerie-engine".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("the async runtime starts");
                runtime.block_on(Worker::new(events, inbox).run());
            })
            .expect("the engine thread starts");
        Self { commands }
    }

    pub fn send(&self, command: Command) {
        // The engine lives as long as the app, so a closed channel means it's shutting down.
        let _ = self.commands.try_send(command);
    }
}

struct Session {
    account: String,
    refresh_token: String,
    drive: DriveApi,
    expires_at: Option<Instant>,
}

struct Worker {
    events: Sender<Event>,
    inbox: Receiver<Command>,
    store: Option<Store>,
    session: Option<Session>,
    sign_in: Option<JoinHandle<aerie_drive::Result<auth::Tokens>>>,
    poll_delay: Duration,
    offline: bool,
    /// Commands that arrived during a long inventory, handled once it finishes.
    pending: Vec<Command>,
}

impl Worker {
    fn new(events: Sender<Event>, inbox: Receiver<Command>) -> Self {
        Self {
            events,
            inbox,
            store: None,
            session: None,
            sign_in: None,
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
            let polling = self.session.is_some();
            let inbox = self.inbox.clone();
            let mut sign_in = self.sign_in.take();
            let waiting_for_browser = sign_in.is_some();
            tokio::select! {
                command = inbox.recv() => {
                    self.sign_in = sign_in;
                    match command {
                        Ok(command) => self.handle(command).await,
                        Err(_) => return,
                    }
                }
                result = async { sign_in.as_mut().expect("guarded").await }, if waiting_for_browser => {
                    self.finish_sign_in(result).await;
                }
                () = tokio::time::sleep(delay), if polling => {
                    self.sign_in = sign_in;
                    self.check().await;
                }
            }
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::SignIn => self.start_sign_in().await,
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
        let refresh_token = match credentials::load_refresh_token(SCOPE).await {
            Ok(Some(token)) => token,
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
            refresh_token,
            drive: DriveApi::new(http, String::new()),
            // Force a token refresh on the first request.
            expires_at: Some(Instant::now()),
        };

        let account = match self.store().account() {
            Ok(Some(account)) => account,
            // The keyring has a sign-in but the database was lost: ask Google who it is.
            _ => match self.fetch_account(&mut session).await {
                Ok(account) => account,
                Err(DriveError::SignInExpired) => {
                    let _ = credentials::delete_refresh_token(SCOPE).await;
                    return self.emit(Event::NeedsSignIn { expired: false });
                }
                Err(err) => {
                    tracing::warn!(%err, "couldn't identify the saved sign-in");
                    return self.emit(Event::NeedsSignIn { expired: false });
                }
            },
        };
        session.account = account.id.clone();
        self.session = Some(session);
        self.emit(Event::SignedIn(account));
        self.check().await;
    }

    async fn fetch_account(&mut self, session: &mut Session) -> aerie_drive::Result<Account> {
        let client = OAuthClient::load(&OAuthClient::default_path())?;
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

    async fn start_sign_in(&mut self) {
        if self.sign_in.is_some() {
            return;
        }
        let client = match OAuthClient::load(&OAuthClient::default_path()) {
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
        self.sign_in = Some(tokio::spawn(async move {
            auth::sign_in(&client, &http, SCOPE, |url| {
                let _ = events.try_send(Event::OpenUri(url.to_string()));
                Ok(())
            })
            .await
        }));
    }

    async fn finish_sign_in(
        &mut self,
        result: Result<aerie_drive::Result<auth::Tokens>, tokio::task::JoinError>,
    ) {
        let tokens = match result {
            Ok(Ok(tokens)) => tokens,
            Ok(Err(err)) => {
                tracing::warn!(%err, "sign-in failed");
                return self.emit(Event::SignInFailed(err.to_string()));
            }
            // Cancelled; CancelSignIn already reported it.
            Err(_) => return,
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
        if let Err(err) = credentials::save_refresh_token(SCOPE, &refresh_token).await {
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
        self.session = Some(Session {
            account: account.id.clone(),
            refresh_token,
            drive,
            expires_at: tokens.expires_at,
        });
        self.emit(Event::SignedIn(account));
        self.check().await;
    }

    async fn sign_out(&mut self) {
        if let Some(session) = self.session.take() {
            let _ = self.store().forget_account(&session.account);
        }
        if let Err(err) = credentials::delete_refresh_token(SCOPE).await {
            tracing::warn!(%err, "couldn't remove the sign-in from the keyring");
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
            }
            Err(err) if err.is_offline() => {
                tracing::info!("offline");
                self.offline = true;
                self.emit(Event::Offline);
            }
            Err(DriveError::SignInExpired) => {
                tracing::info!("sign-in expired");
                self.session = None;
                let _ = credentials::delete_refresh_token(SCOPE).await;
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
            .is_none_or(|at| at > Instant::now() + Duration::from_secs(60));
        if fresh {
            return Ok(());
        }
        let client = OAuthClient::load(&OAuthClient::default_path())?;
        let http = http_client()?;
        let tokens = auth::refresh(&client, &http, &session.refresh_token).await?;
        session.drive.set_access_token(tokens.access_token);
        session.expires_at = tokens.expires_at;
        Ok(())
    }

    /// Lists everything, seeding the change cursor first so edits made meanwhile replay.
    async fn inventory(&mut self, account: &str) -> aerie_drive::Result<()> {
        let session = self.session.as_ref().expect("checked by caller");
        let cursor = session.drive.start_page_token().await?;
        let root = session.drive.root_id().await?;
        self.store
            .as_mut()
            .expect("store is open")
            .begin_inventory(account, &root, &cursor)
            .map_err(store_problem)?;

        let mut files = 0u64;
        let mut page_token: Option<String> = None;
        loop {
            // Stay responsive to sign-out during a long inventory.
            if let Ok(command) = self.inbox.try_recv() {
                if matches!(command, Command::SignOut) {
                    self.sign_out().await;
                    return Ok(());
                }
                self.pending.push(command);
            }
            let session = self.session.as_ref().expect("still signed in");
            let (entries, next) = session.drive.inventory_page(page_token.as_deref()).await?;
            files += entries.len() as u64;
            self.store
                .as_mut()
                .expect("store is open")
                .save_inventory_page(account, &entries)
                .map_err(store_problem)?;
            self.emit(Event::Indexing { files });
            self.emit(Event::CatalogChanged);
            match next {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        self.store
            .as_mut()
            .expect("store is open")
            .finish_inventory(account)
            .map_err(store_problem)?;
        tracing::info!(files, "inventory complete");
        self.emit(Event::CatalogChanged);
        Ok(())
    }

    /// Applies every waiting page of changes. Returns whether anything changed.
    async fn poll_changes(
        &mut self,
        account: &str,
        mut cursor: String,
    ) -> aerie_drive::Result<bool> {
        let mut changed = false;
        loop {
            let session = self.session.as_ref().expect("checked by caller");
            let page = match session.drive.changes(&cursor).await {
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
            self.store
                .as_mut()
                .expect("store is open")
                .apply_changes(account, &page.changes, &next)
                .map_err(store_problem)?;
            cursor = next;
            if matches!(page.next, ChangeCursor::CaughtUp(_)) {
                break;
            }
        }
        if changed {
            self.emit(Event::CatalogChanged);
        }
        Ok(changed)
    }
}

fn store_problem(err: aerie_store::Error) -> DriveError {
    DriveError::Io(std::io::Error::other(err.to_string()))
}
