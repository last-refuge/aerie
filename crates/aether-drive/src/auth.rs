//! Installed-app sign-in: system browser, loopback redirect, PKCE S256 and a checked `state`.

use std::time::{Duration, Instant};

use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, RedirectUrl,
    RefreshToken, Scope, TokenResponse, TokenUrl,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use crate::client::OAuthClient;
use crate::{Error, Result};

/// Read-only Drive access, used until write features exist.
pub const DRIVE_READONLY: &str = "https://www.googleapis.com/auth/drive.readonly";
/// Full Drive access for transfers, file actions and sync.
pub const DRIVE: &str = "https://www.googleapis.com/auth/drive";

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Aether</title>\
<body style=\"font-family:Inter,sans-serif;background:#0b1220;color:#f8fafc;\
display:grid;place-items:center;height:100vh;margin:0\">\
<div style=\"text-align:center\"><h1 style=\"font-weight:650\">You're signed in.</h1>\
<p style=\"color:#94a3b8\">You can close this tab and return to Aether.</p></div>";

const FAILED_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Aether</title>\
<body style=\"font-family:Inter,sans-serif;background:#0b1220;color:#f8fafc;\
display:grid;place-items:center;height:100vh;margin:0\">\
<div style=\"text-align:center\"><h1 style=\"font-weight:650\">Sign-in didn't finish.</h1>\
<p style=\"color:#94a3b8\">Return to Aether to try again.</p></div>";

/// Tokens from a sign-in or refresh. Never log these.
pub struct Tokens {
    pub access_token: String,
    /// Present after sign-in; Google omits it on refresh.
    pub refresh_token: Option<String>,
    pub expires_at: Option<Instant>,
    pub scopes: Vec<String>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

/// Runs the browser sign-in for one scope. `open_browser` receives Google's consent URL.
pub async fn sign_in(
    client: &OAuthClient,
    http: &reqwest::Client,
    scope: &str,
    open_browser: impl FnOnce(&Url) -> std::io::Result<()>,
) -> Result<Tokens> {
    // Bound to loopback only, on a port the OS chooses.
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let redirect = format!("http://127.0.0.1:{}", listener.local_addr()?.port());

    let oauth = BasicClient::new(ClientId::new(client.client_id.clone()))
        .set_client_secret(ClientSecret::new(client.client_secret.clone()))
        .set_auth_uri(AuthUrl::new(client.auth_uri.clone()).map_err(bad_url)?)
        .set_token_uri(TokenUrl::new(client.token_uri.clone()).map_err(bad_url)?)
        .set_redirect_uri(RedirectUrl::new(redirect).map_err(bad_url)?);

    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (consent_url, state) = oauth
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new(scope.to_owned()))
        .set_pkce_challenge(challenge)
        .add_extra_param("access_type", "offline")
        .add_extra_param("prompt", "consent")
        .url();
    open_browser(&consent_url)?;

    let code = tokio::time::timeout(SIGN_IN_TIMEOUT, receive_code(&listener, state.secret()))
        .await
        .map_err(|_| Error::Timeout)??;

    let response = oauth
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(verifier)
        .request_async(http)
        .await
        .map_err(|err| Error::Auth(err.to_string()))?;
    let tokens = tokens_from(&response);
    require_scope(&tokens, scope)?;
    Ok(tokens)
}

/// Exchanges a stored refresh token for a fresh access token.
pub async fn refresh(
    client: &OAuthClient,
    http: &reqwest::Client,
    refresh_token: &str,
) -> Result<Tokens> {
    let oauth = BasicClient::new(ClientId::new(client.client_id.clone()))
        .set_client_secret(ClientSecret::new(client.client_secret.clone()))
        .set_token_uri(TokenUrl::new(client.token_uri.clone()).map_err(bad_url)?);
    let response = oauth
        .exchange_refresh_token(&RefreshToken::new(refresh_token.to_owned()))
        .request_async(http)
        .await
        .map_err(|err| Error::Auth(err.to_string()))?;
    Ok(tokens_from(&response))
}

fn tokens_from(response: &impl TokenResponse) -> Tokens {
    Tokens {
        access_token: response.access_token().secret().clone(),
        refresh_token: response.refresh_token().map(|token| token.secret().clone()),
        expires_at: response.expires_in().map(|ttl| Instant::now() + ttl),
        scopes: response
            .scopes()
            .map(|scopes| scopes.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default(),
    }
}

/// Google may grant less than requested when the user unticks a permission.
fn require_scope(tokens: &Tokens, scope: &str) -> Result<()> {
    if tokens.scopes.iter().any(|granted| granted == scope) {
        Ok(())
    } else {
        Err(Error::ScopeNotGranted {
            requested: scope.to_owned(),
            granted: tokens.scopes.clone(),
        })
    }
}

/// Answers loopback requests until Google's redirect arrives, then returns its code.
async fn receive_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut buffer = vec![0; 8192];
        let read = stream.read(&mut buffer).await?;
        let request = String::from_utf8_lossy(&buffer[..read]);

        let outcome = match parse_redirect(&request) {
            // Browsers also ask for /favicon.ico and similar; ignore those.
            None => {
                respond(&mut stream, "404 Not Found", "").await?;
                continue;
            }
            Some(redirect) => redirect.into_code(expected_state),
        };
        let page = if outcome.is_ok() {
            DONE_PAGE
        } else {
            FAILED_PAGE
        };
        respond(&mut stream, "200 OK", page).await?;
        return outcome;
    }
}

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, body: &str) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct Redirect {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

impl Redirect {
    fn into_code(self, expected_state: &str) -> Result<String> {
        if self.state.as_deref() != Some(expected_state) {
            return Err(Error::Auth(
                "the sign-in response didn't match this request".into(),
            ));
        }
        if let Some(error) = self.error {
            return Err(Error::Denied(error));
        }
        self.code
            .ok_or_else(|| Error::Auth("Google didn't return an authorization code".into()))
    }
}

/// Parses `GET /?code=…&state=… HTTP/1.1`. Returns `None` for any other path.
fn parse_redirect(request: &str) -> Option<Redirect> {
    let target = request
        .lines()
        .next()?
        .strip_prefix("GET ")?
        .split(' ')
        .next()?;
    let url = Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    if url.path() != "/" {
        return None;
    }
    let param = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    Some(Redirect {
        code: param("code"),
        state: param("state"),
        error: param("error"),
    })
}

fn bad_url(err: url::ParseError) -> Error {
    Error::Auth(format!("invalid OAuth endpoint in the client file: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_redirect_request() {
        let redirect =
            parse_redirect("GET /?state=abc&code=4%2F0Ab&scope=x HTTP/1.1\r\nHost: 127.0.0.1\r\n")
                .unwrap();
        assert_eq!(redirect.code.as_deref(), Some("4/0Ab"));
        assert_eq!(redirect.into_code("abc").unwrap(), "4/0Ab");
    }

    #[test]
    fn ignores_other_paths() {
        assert_eq!(parse_redirect("GET /favicon.ico HTTP/1.1\r\n"), None);
        assert_eq!(parse_redirect("POST /?code=x HTTP/1.1\r\n"), None);
    }

    #[test]
    fn rejects_a_mismatched_state() {
        let redirect = parse_redirect("GET /?state=other&code=x HTTP/1.1\r\n").unwrap();
        assert!(matches!(redirect.into_code("abc"), Err(Error::Auth(_))));
    }

    #[test]
    fn reports_denied_consent() {
        let redirect = parse_redirect("GET /?state=abc&error=access_denied HTTP/1.1\r\n").unwrap();
        assert!(matches!(redirect.into_code("abc"), Err(Error::Denied(e)) if e == "access_denied"));
    }

    #[test]
    fn requires_the_requested_scope() {
        let tokens = Tokens {
            access_token: String::new(),
            refresh_token: None,
            expires_at: None,
            scopes: vec!["https://www.googleapis.com/auth/drive.file".into()],
        };
        assert!(require_scope(&tokens, DRIVE_READONLY).is_err());
    }
}
