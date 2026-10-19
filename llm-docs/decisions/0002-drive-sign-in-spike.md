# 0002: Drive sign-in spike findings

**Status:** Phase 0 evidence, October 19, 2026.

## What the spike shows

`cargo run -p aerie-drive --example drive_spike` signs in to Google with the read-only Drive scope, stores the refresh token in the system keyring, and lists My Drive. A second run reuses the stored sign-in without opening the browser. `--sign-out` removes it from the keyring.

Tested on Omarchy (Hyprland, gnome-keyring) against a real account: sign-in, scope check, refresh and a 183-item root listing all worked.

## How it works

- **Sign-in:** system browser via `xdg-open`, a one-shot loopback listener on `127.0.0.1` with an OS-chosen port, PKCE S256 and a checked `state`. `access_type=offline` and `prompt=consent` ensure a refresh token. Requests for other paths, such as `/favicon.ico`, are ignored. Sign-in times out after five minutes.
- **Scope check:** the granted scopes are compared with the requested one, since a user can untick permissions on the consent screen.
- **Credentials:** only the refresh token is stored, in the Secret Service through `oo7`, which also supports the portal inside a sandbox. Access tokens stay in memory. `Debug` output for client and token types omits secrets.
- **OAuth client file:** read from `$XDG_CONFIG_HOME/aerie/oauth-client.json`.
- **Network destinations:** `accounts.google.com`, `oauth2.googleapis.com` and `www.googleapis.com` only.

## Open items

- Resolved: the product was renamed from Aether to Aerie because Omarchy ships an unrelated `aether` package that owns `/usr/bin/aether` and `~/.config/aether`.
- Testing-mode refresh tokens expire after seven days; the app must handle refresh failure by asking to sign in again.
- Change tracking, resumable transfers and overwrite-race tests from Phase 0 are still to do.
- Wiring sign-in and listing into the GTK app belongs to Phase 2.
