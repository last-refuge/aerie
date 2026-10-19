//! The installed-app OAuth client downloaded from Google Cloud Console.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{CONFIG_DIR_NAME, Error, Result};

/// A Desktop app OAuth client. Google documents that its secret can't stay confidential in
/// a distributed app; PKCE protects the authorization code instead.
#[derive(Clone, Deserialize)]
pub struct OAuthClient {
    pub client_id: String,
    pub client_secret: String,
    pub auth_uri: String,
    pub token_uri: String,
}

impl std::fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthClient")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct ClientFile {
    installed: OAuthClient,
}

impl OAuthClient {
    pub fn load(path: &Path) -> Result<Self> {
        let json = std::fs::read_to_string(path).map_err(|source| Error::ClientFile {
            path: path.to_owned(),
            source,
        })?;
        Self::from_json(&json)
    }

    pub fn from_json(json: &str) -> Result<Self> {
        Ok(serde_json::from_str::<ClientFile>(json)?.installed)
    }

    /// `$XDG_CONFIG_HOME/aerie/oauth-client.json`, falling back to `~/.config`.
    pub fn default_path() -> PathBuf {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_default();
        config.join(CONFIG_DIR_NAME).join("oauth-client.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_desktop_client_files() {
        let client = OAuthClient::from_json(
            r#"{"installed":{"client_id":"id.apps.googleusercontent.com","project_id":"p",
            "auth_uri":"https://accounts.google.com/o/oauth2/auth",
            "token_uri":"https://oauth2.googleapis.com/token",
            "client_secret":"not-secret","redirect_uris":["http://localhost"]}}"#,
        )
        .unwrap();
        assert_eq!(client.client_id, "id.apps.googleusercontent.com");
        assert!(!format!("{client:?}").contains("not-secret"));
    }

    #[test]
    fn rejects_web_clients() {
        assert!(OAuthClient::from_json(r#"{"web":{}}"#).is_err());
    }
}
