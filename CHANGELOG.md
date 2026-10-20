# Changelog

## 0.1.0 beta (unreleased)

The first beta of Aerie, Google Drive for Linux.

- **Sign in** through your browser. Aerie asks for read-only access first and for permission to change your Drive only when you first do. Your sign-in lives in the system keyring.
- **Browse and search My Drive**, with breadcrumbs, sorting and instant filename search over a local cache that opens at once, even offline.
- **Manage files:** new folder, rename, move and move to trash, with undo for trash.
- **Upload and download** with a transfer queue that resumes after interruptions and restarts. Google Docs, Sheets and Slides download as exports in the format you choose.
- **Sync** chosen My Drive folders with a local folder in both directions. Conflicts keep both versions, deletions go to the trash, large deletions wait for your confirmation, and a missing sync folder stops sync instead of deleting anything.
- **Runs in the background** while syncing, can **start at login**, and follows the system's dark or light appearance.

Known limits: one Google account; My Drive only, without shared drives or "Shared with me"; no tray icon; Google-native documents open in the browser rather than syncing.
