//! Refresh token storage in the system keyring (Secret Service, or the portal in a sandbox).
//! Tokens are never written to configuration files or the database.

use std::collections::HashMap;

use crate::Result;

const LABEL: &str = "Aether Google Drive sign-in";

fn attributes() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        ("application", "io.github.last_refuge.Aether"),
        ("kind", "google-refresh-token"),
    ])
}

pub async fn save_refresh_token(token: &str) -> Result<()> {
    let keyring = oo7::Keyring::new().await?;
    keyring
        .create_item(LABEL, &attributes(), token.as_bytes(), true)
        .await?;
    Ok(())
}

pub async fn load_refresh_token() -> Result<Option<String>> {
    let keyring = oo7::Keyring::new().await?;
    let Some(item) = keyring
        .search_items(&attributes())
        .await?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let secret = item.secret().await?;
    Ok(Some(String::from_utf8_lossy(&secret).into_owned()))
}

/// Removes the stored sign-in. Files already on disk are untouched.
pub async fn delete_refresh_token() -> Result<()> {
    oo7::Keyring::new().await?.delete(&attributes()).await?;
    Ok(())
}
