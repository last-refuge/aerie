//! Phase 0 spike: sign in to Google Drive (read-only), keep the sign-in in the system
//! keyring, and list My Drive.
//!
//! cargo run -p aerie-drive --example drive_spike [-- --sign-out]

use aerie_core::{DriveFileId, FileKind, format_size};
use aerie_drive::api::DriveApi;
use aerie_drive::auth::{self, DRIVE_READONLY, Tokens};
use aerie_drive::client::OAuthClient;
use aerie_drive::{credentials, http_client};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|arg| arg == "--sign-out") {
        credentials::delete_refresh_token().await?;
        println!("Signed out. The sign-in was removed from the system keyring.");
        return Ok(());
    }

    let path = OAuthClient::default_path();
    let client = OAuthClient::load(&path)?;
    let http = http_client()?;

    let tokens = match credentials::load_refresh_token().await? {
        Some(refresh_token) => match auth::refresh(&client, &http, &refresh_token).await {
            Ok(tokens) => {
                println!("Using the saved sign-in.");
                tokens
            }
            Err(err) => {
                println!("The saved sign-in no longer works ({err}). Signing in again.");
                sign_in(&client, &http).await?
            }
        },
        None => sign_in(&client, &http).await?,
    };

    let drive = DriveApi::new(http, tokens.access_token);
    let account = drive.account().await?;
    println!(
        "Signed in as {} <{}>\n",
        account.display_name, account.email_address
    );

    let mut files = drive.list_children(&DriveFileId::new("root")).await?;
    files.sort_by(|a, b| {
        b.is_folder()
            .cmp(&a.is_folder())
            .then_with(|| aerie_core::compare_names(&a.name, &b.name))
    });
    println!("My Drive: {} items", files.len());
    for file in &files {
        let size = file.size.map_or_else(|| "—".to_owned(), format_size);
        let kind = if file.kind == FileKind::Folder {
            "folder"
        } else {
            "file"
        };
        println!("  {kind:<6}  {size:>9}  {}", file.name);
    }
    Ok(())
}

async fn sign_in(
    client: &OAuthClient,
    http: &reqwest::Client,
) -> Result<Tokens, Box<dyn std::error::Error>> {
    let tokens = auth::sign_in(client, http, DRIVE_READONLY, |url| {
        println!("Opening your browser to sign in to Google Drive.");
        println!("If it doesn't open, visit:\n{url}\n");
        std::process::Command::new("xdg-open")
            .arg(url.as_str())
            .spawn()
            .map(drop)
    })
    .await?;
    match &tokens.refresh_token {
        Some(refresh_token) => credentials::save_refresh_token(refresh_token).await?,
        None => println!("Google didn't return a refresh token; you'll sign in again next run."),
    }
    Ok(tokens)
}
