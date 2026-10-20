//! Refresh token storage in the system keyring (Secret Service, or the portal in a sandbox).
//! Tokens are never written to configuration files or the database.

use std::collections::HashMap;

use crate::Result;

const LABEL: &str = "Aerie Google Drive sign-in";

/// One stored sign-in per OAuth scope, so a narrow test grant never replaces the main one.
fn attributes(scope: &str) -> HashMap<&'static str, &str> {
    HashMap::from([
        ("application", "ai.lastrefuge.Aerie"),
        ("kind", "google-refresh-token"),
        ("scope", scope),
    ])
}

pub async fn save_refresh_token(scope: &str, token: &str) -> Result<()> {
    let keyring = oo7::Keyring::new().await?;
    keyring
        .create_item(LABEL, &attributes(scope), token.as_bytes(), true)
        .await?;
    Ok(())
}

pub async fn load_refresh_token(scope: &str) -> Result<Option<String>> {
    let keyring = oo7::Keyring::new().await?;
    let Some(item) = keyring
        .search_items(&attributes(scope))
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
pub async fn delete_refresh_token(scope: &str) -> Result<()> {
    oo7::Keyring::new()
        .await?
        .delete(&attributes(scope))
        .await?;
    Ok(())
}
