# 0004: Resumable upload spike findings

**Status:** Phase 0 evidence, October 20, 2026. Simulated only; a live run is pending.

## What the spike shows

`DriveApi` can open resumable upload sessions for new files (`start_upload`) and for replacing an existing file's content (`start_replace`), send chunks, ask Drive how much it holds (`upload_status`), and drive a whole upload with retries (`upload`).

## How it handles failure

- **Ask before resending.** After any transient failure (network error, 429 or 5xx), the client asks Drive how many bytes it stored and continues from there. A request whose response was lost is therefore never resent, and content is never duplicated.
- **Partial chunks:** if Drive stored only part of a chunk, the client resends from the exact byte Drive reports.
- **Restarts:** `UploadSession` serializes, so an interrupted upload can resume in a new process. The session URI grants access to the upload, so it must be stored like a credential.
- **Expiry:** a 404 or 410 becomes `Error::UploadSessionExpired`, so the caller opens a new session instead of retrying forever.
- **Chunking:** chunks must be multiples of 256 KiB, except the last.
- **Backoff:** retries use exponential backoff with a bounded number of attempts.

## Evidence

Seven integration tests run against a simulated Drive upload service (`crates/aerie-drive/tests/upload.rs`) with injected faults: multi-chunk upload, a stored chunk whose response is lost, a partially stored chunk, resuming a saved session after a restart, an expired session, an empty file and content replacement.

## Open items

- Run against real Drive, either in a `drive.file` sandbox folder or with a throwaway account.
- Overwrite races: before replacing content, compare the remote version with the last one seen, and measure how small the remaining race window is. Drive v3 offers no conditional write.
- Drive reports some rate limits as 403 `userRateLimitExceeded`. These aren't retried yet.
- Add jitter to the backoff, and honor `Retry-After`.
