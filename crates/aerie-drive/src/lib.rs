//! OAuth and Google Drive API v3 adapter.
//!
//! Every request goes directly from this computer to Google; nothing is routed through an
//! Aerie-hosted service.

pub mod api;
pub mod auth;
pub mod client;
pub mod credentials;

use thiserror::Error;

/// Folder name under the XDG config directory.
pub const CONFIG_DIR_NAME: &str = "aerie";

#[derive(Debug, Error)]
pub enum Error {
    #[error("couldn't read the OAuth client file at {path}: {source}")]
    ClientFile {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("the OAuth client file isn't a Desktop app client: {0}")]
    ClientFormat(#[from] serde_json::Error),
    #[error("sign-in failed: {0}")]
    Auth(String),
    #[error("sign-in was cancelled or denied: {0}")]
    Denied(String),
    #[error("sign-in timed out waiting for the browser")]
    Timeout,
    #[error("Google granted {granted:?} instead of the requested scope {requested}")]
    ScopeNotGranted {
        requested: String,
        granted: Vec<String>,
    },
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Drive API returned {status}: {message}")]
    Api { status: u16, message: String },
    #[error("system keyring error: {0}")]
    Keyring(Box<oo7::Error>),
    #[error("local I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<oo7::Error> for Error {
    fn from(err: oo7::Error) -> Self {
        Self::Keyring(Box::new(err))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// HTTP client shared by authorization and API calls. Redirects are disabled, as the
/// OAuth token endpoint must not be followed elsewhere.
pub fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("Aerie/", env!("CARGO_PKG_VERSION")))
        .build()?)
}
