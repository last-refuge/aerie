# Changelog

## 0.3.4 beta

- **Steady file list:** the list updates only the rows that changed, so the selection, an open context menu and the scroll position stay put while Aerie indexes or syncs.

## 0.3.3 beta

- **Indexing survives restarts:** Aerie saves its place after every page of the first index, so quitting, upgrading, a restart or a rate limit continues from there instead of listing every file again. Once the index is complete, Aerie keeps it current from Google's change feed and never rebuilds it unless you change which shared drives to index.

## 0.3.2 beta

- **Uploads while indexing:** uploads, downloads and file actions no longer wait for a long index to finish; they go ahead between pages of it.
- **Upload feedback:** starting an upload says what's going where, with a button to open Transfers. Files dragged over the file list outline the drop area, and a folder that can't take files says so.
- **Sign-in after sleep:** Aerie renews its Google sign-in after the computer wakes from sleep instead of failing until it's restarted.

## 0.3.1 beta

- **Gentler on Google's limits:** every Drive request is paced to about eight a second, and the pace slows when Google pushes back. Rate-limited requests wait and retry with growing delays (honoring Google's `Retry-After`) instead of failing, so "User rate limit exceeded" no longer reaches you. If Google keeps asking, Aerie waits quietly and tries again later, and an interrupted index picks up where it stopped instead of starting over.

## 0.3.0 beta

- **Indexing choice:** Preferences › Shared drives can turn off "Index all shared drives" (on by default) and keep only chosen drives indexed. Other drives load folder by folder when opened, and search asks Google for them when online. Each indexed drive gets its own change feed, so unindexed drives cost nothing in the background. Drives holding a synced folder always stay indexed.

## 0.2.0 beta

- **The Aerie look:** the cloud mark on the welcome screen and in the sidebar, Lucide icons throughout, and Inter and JetBrains Mono bundled with the app.
- **Shared drives:** browse, search, manage and sync shared drives alongside My Drive.
- **Sync service:** sync runs in `aerie-service`, a background process that keeps working after you close the window, starts at login if you choose, and exits when there's nothing to sync.
- **Sync badges:** synced folders show a badge, which turns while sync works in them. The Sync page shows the folder and step in progress.

## 0.1.0 beta

The first beta of Aerie, Google Drive for Linux.

- **Sign in** through your browser. Aerie asks for read-only access first and for permission to change your Drive only when you first do. Your sign-in lives in the system keyring.
- **Browse and search My Drive**, with breadcrumbs, sorting and instant filename search over a local cache that opens at once, even offline.
- **Manage files:** new folder, rename, move and move to trash, with undo for trash.
- **Upload and download** with a transfer queue that resumes after interruptions and restarts. Google Docs, Sheets and Slides download as exports in the format you choose.
- **Sync** chosen My Drive folders with a local folder in both directions. Conflicts keep both versions, deletions go to the trash, large deletions wait for your confirmation, and a missing sync folder stops sync instead of deleting anything.
- **Runs in the background** while syncing, can **start at login**, and follows the system's dark or light appearance.

Known limits: one Google account; no "Shared with me" view; no tray icon; Google-native documents open in the browser rather than syncing.
