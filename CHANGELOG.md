# Changelog

## 0.4.6 beta

- **No false sign-in prompt after updating:** while Aerie waits for its background service (which takes a few seconds after an update replaces the old one), it shows "Starting Aerie" instead of asking you to connect Google Drive.

## 0.4.5 beta

- **Sync progress:** while sync works, the sidebar and the Sync page show a progress bar, how many changes are done ("12 of 340 changes") and what's happening now ("Downloading report.pdf in Acme"). Progress updates several times a second instead of every 25 changes.

## 0.4.4 beta

- **Drive's folder structure, kept:** a synced folder lands in the same place it has on Drive. Syncing "Acme" from My Drive › Clients puts it at `~/Aerie/My Drive/Clients/Acme`; a shared drive's folders go under the drive's name, and items shared with you under "Shared with me". Folders synced by earlier versions move into place on the first pass, without downloading anything again. The folders above a synced one only hold it; they aren't synced themselves.

## 0.4.3 beta

- **Sync button:** the toolbar has a sync toggle for the folder you have open, or the whole drive when you're at its top. It's pressed while that folder syncs, and greyed out with a note when it syncs as part of a folder above it.

## 0.4.2 beta

- **Sync whole drives:** keep all of My Drive, or an entire shared drive, in sync. Right-click My Drive or a shared drive in the sidebar and choose "Keep in Sync", or switch it on in the Sync page. My Drive syncs to a "My Drive" folder; each shared drive to a folder named after it. A folder inside one that already syncs isn't synced twice.

## 0.4.1 beta

- **Keep in Sync from the folder menu:** right-click any folder, at any depth, and check "Keep in Sync" to add it to your synced folders, or uncheck it to stop. If sync isn't set up yet, it starts in ~/Aerie.

## 0.4.0 beta

A design pass to bring Aerie closer to its brand.

- **The Aerie mark:** the app icon, sidebar and welcome screen use the luminous, intertwined ribbon cloud from the brand guidelines.
- **Welcome screen:** the "Google Drive for Linux" descriptor, the "Your files, everywhere." tagline, a soft halo and horizon, and an outlined button for the sample files.
- **File list:** taller rows, a rounded selection and a hover state; each item sits on a tile tinted by its type. Dates read "2 hours ago", "Yesterday" or a weekday, with the exact time on hover; sizes line up on the right.
- **Shared drives:** a heading with a count that folds the section away, a filter past ten drives, pins to keep favorites on top, and a color for each drive.
- **Status panel:** a tinted badge says how things stand ("Synced · All files up to date"), with your storage use as a bar in the brand gradient.

## 0.3.7 beta

- **Resizable sidebar:** drag the sidebar's edge to make it wider or narrower; Aerie remembers the width. Double-click the edge to restore the default.

## 0.3.6 beta

- **Drops from file managers:** the file list accepts files whether the file manager offers to copy, move or link them, and always uploads a copy. Drops are logged so any that still fail can be diagnosed.

## 0.3.5 beta

- **Shared drives at start:** the sidebar lists your shared drives as soon as Aerie opens, instead of waiting for something on Drive to change. While the first index is still finding them, the sidebar says "Loading shared drives…" with a spinner.

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
