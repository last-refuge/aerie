//! Phase 0 spike: sign in to Google Drive (read-only), keep the sign-in in the system
//! keyring, list My Drive, and optionally follow its change log.
//!
//! cargo run -p aerie-drive --example drive_spike [-- --watch | --sign-out]

use std::time::{Duration, Instant};

use aerie_core::{Catalog, Change, FileKind, MemoryCatalog, format_size};
use aerie_drive::api::{ChangeCursor, DriveApi};
use aerie_drive::auth::{self, DRIVE_READONLY, Tokens};
use aerie_drive::client::OAuthClient;
use aerie_drive::{credentials, http_client};

type Error = Box<dyn std::error::Error>;

const FASTEST_POLL: Duration = Duration::from_secs(5);
const SLOWEST_POLL: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> Result<(), Error> {
    if std::env::args().any(|arg| arg == "--sign-out") {
        credentials::delete_refresh_token(DRIVE_READONLY).await?;
        println!("Signed out. The sign-in was removed from the system keyring.");
        return Ok(());
    }
    let watch = std::env::args().any(|arg| arg == "--watch");

    let client = OAuthClient::load(&OAuthClient::default_path())?;
    let http = http_client()?;

    let (tokens, refresh_token) = match credentials::load_refresh_token(DRIVE_READONLY).await? {
        Some(refresh_token) => match auth::refresh(&client, &http, &refresh_token).await {
            Ok(tokens) => {
                println!("Using the saved sign-in.");
                (tokens, Some(refresh_token))
            }
            Err(err) => {
                println!("The saved sign-in no longer works ({err}). Signing in again.");
                sign_in(&client, &http).await?
            }
        },
        None => sign_in(&client, &http).await?,
    };

    let mut expires_at = tokens.expires_at;
    let mut drive = DriveApi::new(http.clone(), tokens.access_token);
    let account = drive.account().await?;
    println!(
        "Signed in as {} <{}>\n",
        account.display_name, account.email_address
    );

    // Seed the change cursor before listing, so edits made mid-listing are replayed.
    let mut cursor = drive.start_page_token().await?;
    let root = drive.root_id().await?;
    let mut files = drive.list_children(&root).await?;
    files.sort_by(|a, b| {
        b.is_folder()
            .cmp(&a.is_folder())
            .then_with(|| aerie_core::compare_names(&a.name, &b.name))
    });
    println!("My Drive: {} items", files.len());
    println!("Shared drives: {}", drive.shared_drives().await?.len());
    if !watch {
        for file in &files {
            let size = file.size.map_or_else(|| "—".to_owned(), format_size);
            let kind = if file.kind == FileKind::Folder {
                "folder"
            } else {
                "file"
            };
            println!("  {kind:<6}  {size:>9}  {}", file.name);
        }
        return Ok(());
    }

    let mut catalog = MemoryCatalog::new(files);
    let mut delay = FASTEST_POLL;
    println!("Watching for changes. Press Ctrl+C to stop.");
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            () = tokio::time::sleep(delay) => {}
        }

        if expires_at.is_some_and(|at| at <= Instant::now() + Duration::from_secs(60)) {
            let Some(refresh_token) = &refresh_token else {
                return Err("the sign-in expired and can't be refreshed".into());
            };
            let tokens = auth::refresh(&client, &http, refresh_token).await?;
            expires_at = tokens.expires_at;
            drive.set_access_token(tokens.access_token);
        }

        let mut seen = 0;
        let mut page_token = cursor.clone();
        loop {
            let page = drive.changes(&page_token).await?;
            for change in page.changes {
                seen += 1;
                match &change {
                    Change::Upsert(entry) => println!("  changed  {}", entry.name),
                    Change::Removed(_) => println!("  removed  1 item"),
                }
                catalog.apply(change);
            }
            match page.next {
                ChangeCursor::MorePages(token) => page_token = token,
                ChangeCursor::CaughtUp(token) => {
                    cursor = token;
                    break;
                }
            }
        }

        // Poll quickly while things are changing, and back off when idle.
        if seen > 0 {
            println!("My Drive: {} items", catalog.children(&root).len());
            delay = FASTEST_POLL;
        } else {
            delay = (delay * 2).min(SLOWEST_POLL);
        }
    }
}

/// Signs in through the browser and saves the refresh token, which it also returns.
async fn sign_in(
    client: &OAuthClient,
    http: &reqwest::Client,
) -> Result<(Tokens, Option<String>), Error> {
    let tokens = auth::sign_in(client, http, DRIVE_READONLY, |url| {
        println!("Opening your browser to sign in to Google Drive.");
        println!("If it doesn't open, visit:\n{url}\n");
        std::process::Command::new("xdg-open")
            .arg(url.as_str())
            .spawn()
            .map(drop)
    })
    .await?;
    let refresh_token = tokens.refresh_token.clone();
    match &refresh_token {
        Some(token) => credentials::save_refresh_token(DRIVE_READONLY, token).await?,
        None => println!("Google didn't return a refresh token; you'll sign in again next run."),
    }
    Ok((tokens, refresh_token))
}
