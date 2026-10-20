//! The persistent transfer queue. Interrupted transfers resume after a restart: uploads
//! from their saved session, downloads from their partial file.

use std::path::PathBuf;

use aerie_core::DriveFileId;
use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, Store};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Upload,
    Download,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferState {
    Queued,
    Running,
    Paused,
    Failed,
    Done,
    Cancelled,
}

impl TransferState {
    fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Failed => "failed",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    fn from_name(name: &str) -> Self {
        match name {
            "running" => Self::Running,
            "paused" => Self::Paused,
            "failed" => Self::Failed,
            "done" => Self::Done,
            "cancelled" => Self::Cancelled,
            _ => Self::Queued,
        }
    }

    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub id: i64,
    pub direction: Direction,
    /// The Drive file downloaded from, or created by an upload once it completes.
    pub file_id: Option<DriveFileId>,
    /// Where an upload goes.
    pub parent_id: Option<DriveFileId>,
    pub name: String,
    pub local_path: PathBuf,
    pub mime_type: String,
    /// For Google-native documents, the format to export as.
    pub export_mime: Option<String>,
    pub total: Option<u64>,
    pub transferred: u64,
    pub state: TransferState,
    /// A serialized resumable upload session.
    pub upload_session: Option<String>,
    /// The modification time of a local file a download will replace, so a local edit
    /// made meanwhile is never overwritten.
    pub replaces_mtime: Option<i64>,
    pub error: Option<String>,
}

/// What a new transfer needs; the queue fills in the rest.
pub struct NewTransfer {
    pub direction: Direction,
    pub file_id: Option<DriveFileId>,
    pub parent_id: Option<DriveFileId>,
    pub name: String,
    pub local_path: PathBuf,
    pub mime_type: String,
    pub export_mime: Option<String>,
    pub total: Option<u64>,
    pub replaces_mtime: Option<i64>,
}

const COLUMNS: &str = "id, direction, file_id, parent_id, name, local_path, mime_type, \
                       export_mime, total, transferred, state, upload_session, replaces_mtime, error";

fn transfer_from_row(row: &Row<'_>) -> rusqlite::Result<Transfer> {
    Ok(Transfer {
        id: row.get(0)?,
        direction: if row.get::<_, String>(1)? == "upload" {
            Direction::Upload
        } else {
            Direction::Download
        },
        file_id: row.get::<_, Option<String>>(2)?.map(DriveFileId::new),
        parent_id: row.get::<_, Option<String>>(3)?.map(DriveFileId::new),
        name: row.get(4)?,
        local_path: PathBuf::from(row.get::<_, String>(5)?),
        mime_type: row.get(6)?,
        export_mime: row.get(7)?,
        total: row.get::<_, Option<i64>>(8)?.map(|total| total as u64),
        transferred: row.get::<_, i64>(9)? as u64,
        state: TransferState::from_name(&row.get::<_, String>(10)?),
        upload_session: row.get(11)?,
        replaces_mtime: row.get(12)?,
        error: row.get(13)?,
    })
}

impl Store {
    pub fn add_transfer(&self, account: &str, new: &NewTransfer, now: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO transfers (account_id, direction, file_id, parent_id, name, local_path,
                                    mime_type, export_mime, total, state, replaces_mtime, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'queued', ?10, ?11)",
            params![
                account,
                match new.direction {
                    Direction::Upload => "upload",
                    Direction::Download => "download",
                },
                new.file_id.as_ref().map(DriveFileId::as_str),
                new.parent_id.as_ref().map(DriveFileId::as_str),
                new.name,
                new.local_path.to_string_lossy(),
                new.mime_type,
                new.export_mime,
                new.total.map(|total| total as i64),
                new.replaces_mtime,
                now,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Every transfer, oldest first.
    pub fn transfers(&self, account: &str) -> Result<Vec<Transfer>> {
        let mut statement = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM transfers WHERE account_id = ?1 ORDER BY id"
        ))?;
        let rows = statement.query_map([account], transfer_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn transfer(&self, id: i64) -> Result<Option<Transfer>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM transfers WHERE id = ?1"),
                [id],
                transfer_from_row,
            )
            .optional()?)
    }

    /// The oldest transfer waiting to run.
    pub fn next_queued_transfer(&self, account: &str) -> Result<Option<Transfer>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM transfers
                     WHERE account_id = ?1 AND state = 'queued' ORDER BY id LIMIT 1"
                ),
                [account],
                transfer_from_row,
            )
            .optional()?)
    }

    /// After a crash or restart, transfers that were running are queued again.
    pub fn requeue_interrupted_transfers(&self, account: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE transfers SET state = 'queued' WHERE account_id = ?1 AND state = 'running'",
            [account],
        )?;
        Ok(())
    }

    pub fn set_transfer_state(
        &self,
        id: i64,
        state: TransferState,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE transfers SET state = ?2, error = ?3 WHERE id = ?1",
            params![id, state.name(), error],
        )?;
        Ok(())
    }

    pub fn set_transfer_progress(
        &self,
        id: i64,
        transferred: u64,
        total: Option<u64>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE transfers SET transferred = ?2, total = COALESCE(?3, total) WHERE id = ?1",
            params![id, transferred as i64, total.map(|total| total as i64)],
        )?;
        Ok(())
    }

    pub fn set_upload_session(&self, id: i64, session: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE transfers SET upload_session = ?2 WHERE id = ?1",
            params![id, session],
        )?;
        Ok(())
    }

    pub fn set_transfer_path(&self, id: i64, path: &std::path::Path) -> Result<()> {
        self.conn.execute(
            "UPDATE transfers SET local_path = ?2 WHERE id = ?1",
            params![id, path.to_string_lossy()],
        )?;
        Ok(())
    }

    pub fn set_transfer_file(&self, id: i64, file: &DriveFileId) -> Result<()> {
        self.conn.execute(
            "UPDATE transfers SET file_id = ?2 WHERE id = ?1",
            params![id, file.as_str()],
        )?;
        Ok(())
    }

    /// Removes finished and cancelled transfers from the list.
    pub fn clear_finished_transfers(&self, account: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM transfers WHERE account_id = ?1 AND state IN ('done', 'cancelled')",
            [account],
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

    fn upload(name: &str) -> NewTransfer {
        NewTransfer {
            direction: Direction::Upload,
            file_id: None,
            parent_id: Some(DriveFileId::new("folder")),
            name: name.into(),
            local_path: PathBuf::from(format!("/tmp/{name}")),
            mime_type: "text/plain".into(),
            export_mime: None,
            total: Some(10),
            replaces_mtime: None,
        }
    }

    #[test]
    fn queues_and_runs_transfers_in_order() {
        let store = store();
        let first = store.add_transfer("a", &upload("one.txt"), 1).unwrap();
        store.add_transfer("a", &upload("two.txt"), 2).unwrap();
        assert_eq!(store.next_queued_transfer("a").unwrap().unwrap().id, first);

        store
            .set_transfer_state(first, TransferState::Running, None)
            .unwrap();
        store.set_transfer_progress(first, 4, None).unwrap();
        store.set_upload_session(first, Some("{}")).unwrap();
        let running = store.transfer(first).unwrap().unwrap();
        assert_eq!(running.transferred, 4);
        assert_eq!(running.total, Some(10));
        assert_eq!(running.upload_session.as_deref(), Some("{}"));
        assert_eq!(
            store.next_queued_transfer("a").unwrap().unwrap().name,
            "two.txt"
        );
    }

    #[test]
    fn requeues_interrupted_transfers_and_clears_finished_ones() {
        let store = store();
        let running = store.add_transfer("a", &upload("one.txt"), 1).unwrap();
        let done = store.add_transfer("a", &upload("two.txt"), 2).unwrap();
        store
            .set_transfer_state(running, TransferState::Running, None)
            .unwrap();
        store
            .set_transfer_state(done, TransferState::Done, None)
            .unwrap();

        store.requeue_interrupted_transfers("a").unwrap();
        assert_eq!(
            store.transfer(running).unwrap().unwrap().state,
            TransferState::Queued
        );
        store.clear_finished_transfers("a").unwrap();
        let left: Vec<_> = store
            .transfers("a")
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(left, [running]);
    }
}
