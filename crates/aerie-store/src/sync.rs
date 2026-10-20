//! Sync configuration and the baseline: the state both sides last agreed on.

use std::path::PathBuf;

use aerie_core::DriveFileId;
use rusqlite::{OptionalExtension, params};

use crate::{Result, Store};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncSettings {
    pub local_root: PathBuf,
    pub paused: bool,
}

/// One baseline row. `content` is an opaque content identity, such as `md5:…`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineRow {
    pub remote_id: DriveFileId,
    pub path: PathBuf,
    pub is_dir: bool,
    pub content: String,
    pub size: u64,
    pub mtime: i64,
    pub device: u64,
    pub inode: u64,
}

impl Store {
    pub fn sync_settings(&self, account: &str) -> Result<Option<SyncSettings>> {
        Ok(self
            .conn
            .query_row(
                "SELECT local_root, paused FROM sync_settings WHERE account_id = ?1",
                [account],
                |row| {
                    Ok(SyncSettings {
                        local_root: PathBuf::from(row.get::<_, String>(0)?),
                        paused: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn save_sync_settings(&self, account: &str, settings: &SyncSettings) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_settings (account_id, local_root, paused) VALUES (?1, ?2, ?3)
             ON CONFLICT (account_id) DO UPDATE SET local_root = ?2, paused = ?3",
            params![
                account,
                settings.local_root.to_string_lossy(),
                settings.paused
            ],
        )?;
        Ok(())
    }

    /// Turns sync off: forgets the settings, roots and baseline. Local files stay put.
    pub fn stop_sync(&mut self, account: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        for table in ["sync_settings", "sync_roots", "sync_baseline"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE account_id = ?1"),
                [account],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn sync_roots(&self, account: &str) -> Result<Vec<DriveFileId>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT remote_id FROM sync_roots WHERE account_id = ?1 ORDER BY remote_id",
        )?;
        let rows = statement.query_map([account], |row| {
            Ok(DriveFileId::new(row.get::<_, String>(0)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replaces the selected folders. Deselected folders stop syncing; their local files
    /// stay, and their baseline is dropped so nothing is deleted on either side.
    pub fn set_sync_roots(
        &mut self,
        account: &str,
        roots: &[DriveFileId],
        kept_paths: &[PathBuf],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM sync_roots WHERE account_id = ?1", [account])?;
        for root in roots {
            tx.execute(
                "INSERT INTO sync_roots (account_id, remote_id) VALUES (?1, ?2)",
                params![account, root.as_str()],
            )?;
        }
        // Drop baseline rows outside the folders that remain selected.
        let rows: Vec<(String, String)> = {
            let mut statement =
                tx.prepare("SELECT remote_id, path FROM sync_baseline WHERE account_id = ?1")?;
            statement
                .query_map([account], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (remote_id, path) in rows {
            let path = PathBuf::from(path);
            if !kept_paths.iter().any(|kept| path.starts_with(kept)) {
                tx.execute(
                    "DELETE FROM sync_baseline WHERE account_id = ?1 AND remote_id = ?2",
                    params![account, remote_id],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn baseline(&self, account: &str) -> Result<Vec<BaselineRow>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT remote_id, path, is_dir, content, size, mtime, device, inode
             FROM sync_baseline WHERE account_id = ?1",
        )?;
        let rows = statement.query_map([account], |row| {
            Ok(BaselineRow {
                remote_id: DriveFileId::new(row.get::<_, String>(0)?),
                path: PathBuf::from(row.get::<_, String>(1)?),
                is_dir: row.get(2)?,
                content: row.get(3)?,
                size: row.get::<_, i64>(4)? as u64,
                mtime: row.get(5)?,
                device: row.get::<_, i64>(6)? as u64,
                inode: row.get::<_, i64>(7)? as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records an item as in agreement on both sides.
    pub fn save_baseline(&self, account: &str, row: &BaselineRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_baseline (account_id, remote_id, path, is_dir, content, size, mtime,
                                        device, inode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (account_id, remote_id) DO UPDATE SET
               path = ?3, is_dir = ?4, content = ?5, size = ?6, mtime = ?7, device = ?8, inode = ?9",
            params![
                account,
                row.remote_id.as_str(),
                row.path.to_string_lossy(),
                row.is_dir,
                row.content,
                row.size as i64,
                row.mtime,
                row.device as i64,
                row.inode as i64,
            ],
        )?;
        Ok(())
    }

    /// Moves baseline paths after a folder or file moved: `from` and everything under it.
    pub fn move_baseline(
        &mut self,
        account: &str,
        from: &std::path::Path,
        to: &std::path::Path,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let rows: Vec<(String, String)> = {
            let mut statement =
                tx.prepare("SELECT remote_id, path FROM sync_baseline WHERE account_id = ?1")?;
            statement
                .query_map([account], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (remote_id, path) in rows {
            let path = PathBuf::from(path);
            if let Ok(rest) = path.strip_prefix(from) {
                let moved = if rest.as_os_str().is_empty() {
                    to.to_owned()
                } else {
                    to.join(rest)
                };
                tx.execute(
                    "UPDATE sync_baseline SET path = ?3 WHERE account_id = ?1 AND remote_id = ?2",
                    params![account, remote_id, moved.to_string_lossy()],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Forgets an item and, for a folder, everything recorded under it.
    pub fn forget_baseline_under(&self, account: &str, path: &std::path::Path) -> Result<()> {
        let rows = self.baseline(account)?;
        for row in rows.iter().filter(|row| row.path.starts_with(path)) {
            self.forget_baseline(account, &row.remote_id)?;
        }
        Ok(())
    }

    pub fn forget_baseline(&self, account: &str, remote_id: &DriveFileId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM sync_baseline WHERE account_id = ?1 AND remote_id = ?2",
            params![account, remote_id.as_str()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Account;

    fn store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store
            .save_account(&Account {
                id: "a".into(),
                email: "e".into(),
                display_name: "n".into(),
            })
            .unwrap();
        store
    }

    fn row(id: &str, path: &str) -> BaselineRow {
        BaselineRow {
            remote_id: DriveFileId::new(id),
            path: PathBuf::from(path),
            is_dir: false,
            content: "md5:x".into(),
            size: 1,
            mtime: 2,
            device: 3,
            inode: 4,
        }
    }

    #[test]
    fn stores_settings_roots_and_baseline() {
        let mut store = store();
        let settings = SyncSettings {
            local_root: PathBuf::from("/home/me/Aerie"),
            paused: false,
        };
        store.save_sync_settings("a", &settings).unwrap();
        assert_eq!(store.sync_settings("a").unwrap(), Some(settings));

        store
            .set_sync_roots("a", &[DriveFileId::new("docs")], &[PathBuf::from("Docs")])
            .unwrap();
        assert_eq!(store.sync_roots("a").unwrap(), [DriveFileId::new("docs")]);

        store.save_baseline("a", &row("f", "Docs/f.txt")).unwrap();
        store.save_baseline("a", &row("f", "Docs/g.txt")).unwrap();
        assert_eq!(store.baseline("a").unwrap(), [row("f", "Docs/g.txt")]);
        store.forget_baseline("a", &DriveFileId::new("f")).unwrap();
        assert!(store.baseline("a").unwrap().is_empty());
    }

    #[test]
    fn deselecting_a_folder_drops_its_baseline_only() {
        let mut store = store();
        store.save_baseline("a", &row("1", "Docs/a.txt")).unwrap();
        store.save_baseline("a", &row("2", "Photos/b.jpg")).unwrap();
        store
            .set_sync_roots("a", &[DriveFileId::new("docs")], &[PathBuf::from("Docs")])
            .unwrap();
        let left: Vec<_> = store
            .baseline("a")
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect();
        assert_eq!(left, [PathBuf::from("Docs/a.txt")]);
    }

    #[test]
    fn moves_and_forgets_subtrees() {
        let mut store = store();
        store.save_baseline("a", &row("d", "Docs")).unwrap();
        store.save_baseline("a", &row("f", "Docs/f.txt")).unwrap();
        store.save_baseline("a", &row("x", "Docsx/f.txt")).unwrap();
        store
            .move_baseline(
                "a",
                std::path::Path::new("Docs"),
                std::path::Path::new("Work/Docs"),
            )
            .unwrap();
        let mut paths: Vec<_> = store
            .baseline("a")
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                PathBuf::from("Docsx/f.txt"),
                PathBuf::from("Work/Docs"),
                PathBuf::from("Work/Docs/f.txt")
            ]
        );
        store
            .forget_baseline_under("a", std::path::Path::new("Work"))
            .unwrap();
        assert_eq!(store.baseline("a").unwrap().len(), 1);
    }

    #[test]
    fn stopping_sync_forgets_everything() {
        let mut store = store();
        store
            .save_sync_settings(
                "a",
                &SyncSettings {
                    local_root: PathBuf::from("/x"),
                    paused: true,
                },
            )
            .unwrap();
        store.save_baseline("a", &row("1", "a")).unwrap();
        store.stop_sync("a").unwrap();
        assert_eq!(store.sync_settings("a").unwrap(), None);
        assert!(store.baseline("a").unwrap().is_empty());
    }
}
