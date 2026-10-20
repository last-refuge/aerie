# Changelog

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
