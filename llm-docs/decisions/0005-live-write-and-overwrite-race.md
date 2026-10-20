# 0005: Live write tests and overwrite races

**Status:** Phase 0 evidence, October 20, 2026. Closes the plan's "validate available conditional-write behavior" item.

## Setup

`cargo run -p aerie-drive --example write_spike` signs in with `drive.file`, which only reaches files Aerie creates. It works in an "Aerie test" folder on a real account. The sign-in is stored separately per scope, so the narrow test grant never replaces the read-only one. Under `drive.file` the root folder itself isn't readable, so the spike addresses it by the `root` alias rather than its resolved ID.

## Results

| Test | Result |
| --- | --- |
| Resumable upload, interrupted after the first 256 KiB chunk, resumed from a serialized session in a fresh client | Drive reported 262,144 bytes held; the rest uploaded in 1.5 s; the 1,500,000-byte download matched byte for byte |
| Detecting a remote edit since the baseline | `md5Checksum` and `headRevisionId` both changed. `version` jumped from 1 to 4, because it also counts metadata changes |
| Check-then-write window (check the version, open a session, upload a small file) | About 2 seconds; `version` rose by 3 for one replace |
| Replace with a stale `If-Match` header | HTTP 200, and the content was overwritten. **Drive v3 ignores conditional writes** |

## Decisions

- **Baseline identity:** compare `md5Checksum`, or `headRevisionId` when there's no checksum, rather than `version` or `modifiedTime`. `version` changes on metadata-only edits, and timestamps are unreliable across devices.
- **No atomic overwrite protection.** Drive offers no compare-and-swap, so Aerie can't claim one. Overwrites check the remote baseline immediately before uploading, which narrows the race to the upload's duration but doesn't close it.
- **Detect a lost update after the fact.** After each overwrite, list the file's revisions. If any revision other than Aerie's appeared after the baseline, a concurrent edit was overwritten. Drive keeps that revision, so Aerie restores it as a conflict copy and asks the user. This is the mitigation the Phase 0 exit criteria require before sync work starts.
- **Prefer conflict copies:** when a remote change is detected before writing, keep both versions, as the plan specifies.

## Open items

- Implement and test the revision-list check, including revisions Drive merges or doesn't keep (`keepRevisionForever` is off by default, and binary revisions can be pruned).
- Measure the window for large files, where it lasts as long as the upload.
