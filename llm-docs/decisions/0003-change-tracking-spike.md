# 0003: Change tracking spike findings

**Status:** Phase 0 evidence, October 19, 2026.

## What the spike shows

`cargo run -p aerie-drive --example drive_spike -- --watch` lists My Drive, then follows the Drive change log and applies each change to an in-memory `BrowseState`. It polls every 5 seconds while changes are arriving and backs off to once a minute when idle, so an idle session makes about one request a minute. The access token is refreshed before it expires.

## How it works

- **Bootstrap race:** the change cursor (`changes/startPageToken`) is fetched before the initial listing. Edits made while the listing runs show up as changes on the first poll rather than being lost.
- **Paging:** `DriveApi::changes` returns one page and a `ChangeCursor`, either `MorePages` to continue now or `CaughtUp` to poll later. The plan requires each page's changes and its cursor to be persisted together; the API is shaped for that, but the spike keeps them in memory until the SQLite store exists.
- **Mapping:** edits, renames and moves become `Change::Upsert`. Trashed, deleted and no-longer-accessible items become `Change::Removed`. Only `changeType: file` within My Drive is read (`restrictToMyDrive`); shared drives are out of scope for the first release.
- **Root ID:** listings report the root folder's real ID as the parent, not the alias `root`, so the browser resolves it with `files/root` first.
- **Applying changes:** `BrowseState::apply` updates or inserts entries, removes deleted ones, renames open folders in the breadcrumb path, and leaves an open folder that was removed.

Ran against a real account for 40 seconds, read-only: polling and backoff worked without errors. No changes happened during the run, so the change path is covered by unit tests rather than a live edit.

## Open items

- When a folder is trashed, Drive may report only the folder. Its descendants stay cached but unreachable; the store should prune them.
- An expired or invalid cursor should trigger a full re-inventory that never treats missing partial results as deletions, as the plan requires.
- Resumable uploads and overwrite-race tests need write access and a disposable test account.
