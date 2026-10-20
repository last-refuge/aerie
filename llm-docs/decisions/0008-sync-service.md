# 0008: Separate sync service

**Status:** Accepted, October 20, 2026. Resolves the beta deviation in [0007](0007-sync-engine.md).

## Decision

The Drive engine (sign-in, inventory, change polling, transfers and sync) runs in `aerie-service`, a per-user process that's separate from the window. The app is a client of the service.

- **IPC:** newline-delimited JSON over a Unix socket at `$XDG_RUNTIME_DIR/aerie/service.sock`. The folder is mode 0700 and the socket 0600, and the service also rejects peers whose user ID differs from the folder's owner.
- **Protocol:** both sides start by exchanging `Hello { protocol, version }`. On a mismatch the app sends `Shutdown` and starts its own service. `PROTOCOL` must be bumped when `Command` or `Event` change incompatibly.
- **Single owner:** a second service that finds a live socket exits. A stale socket is replaced.
- **State replay:** the service keeps the latest session, connectivity and sync events, and sends them to each app that connects.
- **Lifecycle:** the app starts the service when none answers and reconnects if the service restarts. The service exits after 60 seconds with no app connected and no work, meaning sync is off or paused and no transfer or sign-in is running. Closing the window quits the app; sync continues.
- **Start at login:** the autostart entry runs `aerie-service` directly. `aerie --background` now just starts the service.
- **Notifications:** with no window connected, the service reports kept conflicts through `notify-send`.
- **Data:** the service owns all writes to the database. The app reads the metadata cache directly through SQLite WAL for fast browsing.
- **Development:** debug builds use `service-devel.sock` and the app ID `ai.lastrefuge.Aerie.Devel`, so they never connect to an installed Aerie.

## Evidence

In the `drive.file` sandbox:
- The app started the service and received a replay of the current state.
- With the app closed, the service uploaded a newly created local file.
- With sync paused and no app connected, the service exited after the idle period.
- A restarted service resumed syncing.
