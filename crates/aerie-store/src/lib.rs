//! SQLite schema and migrations for cached Drive metadata and sync state.
//!
//! The database holds no credentials. Lose it and Aerie re-inventories Drive; it never
//! treats a missing or partial inventory as deleted files.

mod migrations;
mod transfers;

use std::path::{Path, PathBuf};

use aerie_core::{Capabilities, Change, DriveFileId, FileEntry, FileKind};
use rusqlite::{Connection, OptionalExtension, Row, params};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("couldn't create the database folder: {0}")]
    Io(#[from] std::io::Error),
    #[error("the database was written by a newer Aerie (schema {found}, this build knows {known})")]
    NewerSchema { found: i64, known: i64 },
}

pub use transfers::{Direction, NewTransfer, Transfer, TransferState};

pub type Result<T> = std::result::Result<T, Error>;

/// A signed-in Google account. `id` is Drive's stable `permissionId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub email: String,
    pub display_name: String,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// `$XDG_STATE_HOME/aerie/aerie.sqlite3`. Sync state belongs in state, not the
    /// disposable cache, because losing it would force a full reconciliation.
    pub fn default_path() -> PathBuf {
        let state = std::env::var_os("XDG_STATE_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
            .unwrap_or_default();
        state.join("aerie").join("aerie.sqlite3")
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::prepare(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // The engine, transfers and the UI each hold a connection; wait out brief locks.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrations::run(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    pub fn save_account(&self, account: &Account) -> Result<()> {
        self.conn.execute(
            "INSERT INTO accounts (id, email, display_name) VALUES (?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET email = ?2, display_name = ?3",
            params![account.id, account.email, account.display_name],
        )?;
        Ok(())
    }

    pub fn account(&self) -> Result<Option<Account>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, email, display_name FROM accounts LIMIT 1",
                [],
                |row| {
                    Ok(Account {
                        id: row.get(0)?,
                        email: row.get(1)?,
                        display_name: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Removes an account and everything cached for it. Local files are untouched.
    pub fn forget_account(&self, account: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM accounts WHERE id = ?1", [account])?;
        Ok(())
    }

    /// Starts a full inventory: records the root ID and a change cursor fetched *before*
    /// listing, so changes made during the listing are replayed afterwards. Cached entries
    /// stay visible until the inventory finishes.
    pub fn begin_inventory(
        &mut self,
        account: &str,
        root: &DriveFileId,
        cursor: &str,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE accounts SET root_id = ?2, change_cursor = ?3, inventory_complete = 0
             WHERE id = ?1",
            params![account, root.as_str(), cursor],
        )?;
        tx.execute(
            "UPDATE files SET inventoried = 0 WHERE account_id = ?1",
            [account],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Stores one page of an inventory in progress.
    pub fn save_inventory_page(&mut self, account: &str, entries: &[FileEntry]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for entry in entries {
            upsert(&tx, account, entry)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Completes an inventory: entries it didn't see no longer exist remotely. Call only
    /// after every page arrived; an interrupted inventory must never delete anything.
    pub fn finish_inventory(&mut self, account: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM files WHERE account_id = ?1 AND inventoried = 0",
            [account],
        )?;
        tx.execute(
            "UPDATE accounts SET inventory_complete = 1 WHERE id = ?1",
            [account],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Whether a full inventory has completed. Until then, an empty folder may simply be
    /// unlisted, and the UI must say so rather than show an empty Drive.
    pub fn inventory_complete(&self, account: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT inventory_complete FROM accounts WHERE id = ?1",
                [account],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    pub fn root_id(&self, account: &str) -> Result<Option<DriveFileId>> {
        let root: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT root_id FROM accounts WHERE id = ?1",
                [account],
                |row| row.get(0),
            )
            .optional()?;
        Ok(root.flatten().map(DriveFileId::new))
    }

    pub fn set_root(&self, account: &str, root: &DriveFileId) -> Result<()> {
        self.conn.execute(
            "UPDATE accounts SET root_id = ?2 WHERE id = ?1",
            params![account, root.as_str()],
        )?;
        Ok(())
    }

    /// Guesses the root folder when it can't be read directly: the parent that isn't itself
    /// cached and holds the most items. Only narrow development scopes need this.
    pub fn likely_root(&self, account: &str) -> Result<Option<DriveFileId>> {
        Ok(self
            .conn
            .query_row(
                "SELECT parent_id FROM files AS f
                 WHERE account_id = ?1 AND parent_id IS NOT NULL
                   AND NOT EXISTS (SELECT 1 FROM files WHERE account_id = ?1 AND id = f.parent_id)
                 GROUP BY parent_id ORDER BY COUNT(*) DESC LIMIT 1",
                [account],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(DriveFileId::new))
    }

    pub fn change_cursor(&self, account: &str) -> Result<Option<String>> {
        let cursor: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT change_cursor FROM accounts WHERE id = ?1",
                [account],
                |row| row.get(0),
            )
            .optional()?;
        Ok(cursor.flatten())
    }

    /// Applies one page of changes and advances the cursor in a single transaction, so a
    /// crash never leaves changes applied without the cursor, or the reverse.
    pub fn apply_changes(&mut self, account: &str, changes: &[Change], cursor: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        for change in changes {
            match change {
                Change::Upsert(entry) => upsert(&tx, account, entry)?,
                Change::Removed(id) => {
                    tx.execute(
                        "DELETE FROM files WHERE account_id = ?1 AND id = ?2",
                        params![account, id.as_str()],
                    )?;
                }
            }
        }
        tx.execute(
            "UPDATE accounts SET change_cursor = ?2 WHERE id = ?1",
            params![account, cursor],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records the result of an action Aerie just performed, ahead of the change log,
    /// without moving the change cursor. The change log later confirms the same state.
    pub fn apply_local(&mut self, account: &str, changes: &[Change]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for change in changes {
            match change {
                Change::Upsert(entry) => upsert(&tx, account, entry)?,
                Change::Removed(id) => {
                    tx.execute(
                        "DELETE FROM files WHERE account_id = ?1 AND id = ?2",
                        params![account, id.as_str()],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn children(&self, account: &str, folder: &DriveFileId) -> Result<Vec<FileEntry>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT id, parent_id, name, kind, size, modified, mime_type, capabilities FROM files
             WHERE account_id = ?1 AND parent_id = ?2",
        )?;
        let rows = statement.query_map(params![account, folder.as_str()], entry_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn file(&self, account: &str, id: &DriveFileId) -> Result<Option<FileEntry>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, parent_id, name, kind, size, modified, mime_type, capabilities FROM files
                 WHERE account_id = ?1 AND id = ?2",
                params![account, id.as_str()],
                entry_from_row,
            )
            .optional()?)
    }

    /// Case-insensitive filename search over the cache, most recently modified first.
    pub fn search(&self, account: &str, query: &str, limit: usize) -> Result<Vec<FileEntry>> {
        let pattern = format!(
            "%{}%",
            query
                .trim()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut statement = self.conn.prepare_cached(
            "SELECT id, parent_id, name, kind, size, modified, mime_type, capabilities FROM files
             WHERE account_id = ?1 AND name LIKE ?2 ESCAPE '\\'
             ORDER BY modified DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(params![account, pattern, limit as i64], entry_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn upsert(conn: &Connection, account: &str, entry: &FileEntry) -> Result<()> {
    conn.execute(
        "INSERT INTO files (account_id, id, parent_id, name, kind, size, modified, mime_type,
                            capabilities, inventoried)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)
         ON CONFLICT (account_id, id) DO UPDATE SET
           parent_id = ?3, name = ?4, kind = ?5, size = ?6, modified = ?7, mime_type = ?8,
           capabilities = ?9, inventoried = 1",
        params![
            account,
            entry.id.as_str(),
            entry.parent.as_ref().map(DriveFileId::as_str),
            entry.name,
            kind_name(entry.kind),
            entry.size.map(|size| size as i64),
            entry.modified,
            entry.mime_type,
            entry.can.bits(),
        ],
    )?;
    Ok(())
}

fn entry_from_row(row: &Row<'_>) -> rusqlite::Result<FileEntry> {
    Ok(FileEntry {
        id: DriveFileId::new(row.get::<_, String>(0)?),
        parent: row.get::<_, Option<String>>(1)?.map(DriveFileId::new),
        name: row.get(2)?,
        kind: kind_from_name(&row.get::<_, String>(3)?),
        size: row.get::<_, Option<i64>>(4)?.map(|size| size as u64),
        modified: row.get(5)?,
        mime_type: row.get(6)?,
        can: Capabilities::from_bits(row.get(7)?),
    })
}

fn kind_name(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Folder => "folder",
        FileKind::Document => "document",
        FileKind::Spreadsheet => "spreadsheet",
        FileKind::Presentation => "presentation",
        FileKind::Pdf => "pdf",
        FileKind::Image => "image",
        FileKind::Other => "other",
    }
}

fn kind_from_name(name: &str) -> FileKind {
    match name {
        "folder" => FileKind::Folder,
        "document" => FileKind::Document,
        "spreadsheet" => FileKind::Spreadsheet,
        "presentation" => FileKind::Presentation,
        "pdf" => FileKind::Pdf,
        "image" => FileKind::Image,
        _ => FileKind::Other,
    }
}

#[cfg(test)]
mod tests;
