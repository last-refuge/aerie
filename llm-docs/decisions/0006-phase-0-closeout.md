# 0006: Phase 0 closeout

**Status:** Accepted, October 20, 2026. Phase 0 exit criteria are met; Phase 1 may start.

## Exit criteria

| Criterion | Evidence |
| --- | --- |
| Viable native UI | [0001](0001-gtk-shell-spike.md): brand tokens on stock libadwaita widgets, virtualized list, dark/light/high-contrast providers |
| Direct Drive connection | [0002](0002-drive-sign-in-spike.md): loopback PKCE sign-in, keyring storage, listing |
| Change tracking | [0003](0003-change-tracking-spike.md): cursor seeded before listing; live edit observed |
| Resumable transfer | [0004](0004-resumable-upload-spike.md), [0005](0005-live-write-and-overwrite-race.md): simulated faults plus a live interrupted upload resumed byte-identical |
| Overwrite races and their mitigation | [0005](0005-live-write-and-overwrite-race.md): no conditional writes in Drive v3; check before writing, then a post-write revision check that detected and recovered a deliberately overwritten edit live |
| No third-party access to credentials or data | Endpoint inventory below |

## Decisions

- **UI toolkit:** Rust with GTK4 and libadwaita, minimum GTK 4.16 and libadwaita 1.6.
- **Sync semantics:** compare against a persisted baseline (checksum or head revision), keep both versions on conflict, use remote trash and never permanently delete, and pause on unexpected bulk deletion. Overwrites check the baseline before writing and the revision list after. Overwritten remote edits are restored as conflict copies.
- **Background lifecycle:** single process with worker tasks through the browser and transfer phases. The sync engine moves into a per-user service before sync ships, and background running is opt-in.
- **Initial package format:** an Arch package (`PKGBUILD`) plus a documented source build. Flatpak comes later.
- **Scopes by milestone:** `drive.readonly` for browsing and `drive` for writes and sync. `drive.file` is for development write tests only.

## Endpoint inventory

Every request goes from the user's computer directly to Google:

| Host | Used for |
| --- | --- |
| `accounts.google.com` | Consent page, opened in the system browser |
| `oauth2.googleapis.com` | Code exchange and token refresh |
| `www.googleapis.com/drive/v3` | Metadata, listings, change log, revisions, downloads |
| `www.googleapis.com/upload/drive/v3` | Resumable uploads |
| `127.0.0.1:<random port>` | One-shot local listener for the sign-in redirect |

No Aerie-operated host receives tokens, file names, IDs or content. There is no telemetry or crash upload.
