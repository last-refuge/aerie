# 0007: Sync engine

**Status:** Accepted for the beta, October 20, 2026.

## Model

Sync compares three snapshots: Drive now (from the metadata cache), the local folder now (a fresh scan), and the **baseline**, the state both sides last agreed on, stored per item in SQLite. `aerie-sync::plan::reconcile` is a pure function from those snapshots to a list of operations, so every rule is unit-tested without a network or a filesystem.

| Situation | Result |
| --- | --- |
| Changed on one side only | That side's version wins |
| Changed on both sides | Keep both: Drive's version is saved locally as "name (conflicted copy from Drive DATE)", which later uploads as its own file, and the local version replaces Drive's |
| Deleted on one side, unchanged on the other | The deletion propagates, to the desktop trash locally (falling back to a quarantine folder) or to Drive's trash |
| Deleted on one side, edited on the other | The edit wins and the item comes back |
| Renamed or moved | Followed on the other side. Local moves are found by inode, and only when the file is otherwise unchanged. When both sides moved an item, Drive's location wins |
| New on Drive where an untracked local file has the same name | Keep both |
| Deleted on both sides | Dropped from the baseline |

## Safeguards

- A scan that can't read a folder aborts the pass, and a missing or unmounted sync folder stops sync. Neither is ever read as deletions.
- Deletions above max(50, 25% of tracked items) are held for the user to confirm.
- Content is replaced only after checking Drive's version against the baseline. After uploading, the revision list is checked, and an edit that slipped in is saved as a conflict copy, as decision [0005](0005-live-write-and-overwrite-race.md) requires.
- Downloads go to a hidden partial file, are size-checked, and are renamed into place only if the local file still matches the baseline; otherwise they're saved beside it.
- Each step records its baseline only after it succeeds. A failed step leaves the difference in place to retry.
- Google-native documents and shortcuts aren't synced. Symbolic links aren't followed. Names Drive allows but Linux doesn't are mapped (`/` becomes `∕`), and duplicate names get stable numbered suffixes.
- The sync folder's top level holds only the chosen Drive folders. New top-level items stay local, because a deselected folder's leftover copy must never be re-uploaded as new.
- Deselecting a folder keeps its local files and forgets its baseline, so nothing is deleted on either side.

## Evidence

- 24 unit tests across reconciliation, scanning and path mapping.
- `cargo run -p aerie-sync --example sync_spike` runs live scenarios on a real account, confined by `drive.file` to "Aerie test", and all pass:
  - first download;
  - a local edit, new file, new folder and rename going up;
  - a Drive edit and rename coming down;
  - a conflict keeping both versions on both sides;
  - deletions in each direction;
  - a quiet pass doing nothing, with no feedback loop from Aerie's own writes;
  - a vanished sync folder stopping the pass with nothing deleted on Drive.

## Deviations for the beta

- The engine runs inside the app process rather than in a separate `aerie-service`. The app keeps running in the background while sync is on. Splitting out a service remains a release requirement.
- Uploads and downloads within a pass run one at a time.
