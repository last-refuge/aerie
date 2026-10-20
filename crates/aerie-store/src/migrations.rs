//! Schema migrations, tracked with SQLite's `user_version`. Each step runs in its own
//! transaction, so a failed migration leaves the previous schema intact.

use rusqlite::Connection;

use crate::{Error, Result};

const MIGRATIONS: &[&str] = &[
    // 1: accounts and cached metadata.
    "CREATE TABLE accounts (
        id TEXT PRIMARY KEY,
        email TEXT NOT NULL,
        display_name TEXT NOT NULL,
        root_id TEXT,
        change_cursor TEXT,
        inventory_complete INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE files (
        account_id TEXT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
        id TEXT NOT NULL,
        parent_id TEXT,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        size INTEGER,
        modified INTEGER NOT NULL,
        -- Cleared when an inventory starts and set as each entry is seen again.
        inventoried INTEGER NOT NULL DEFAULT 1,
        PRIMARY KEY (account_id, id)
    );
    CREATE INDEX files_by_parent ON files (account_id, parent_id);",
    // 2: MIME types and per-item capabilities. Cached rows lack them, so re-inventory.
    "ALTER TABLE files ADD COLUMN mime_type TEXT NOT NULL DEFAULT '';
    ALTER TABLE files ADD COLUMN capabilities INTEGER NOT NULL DEFAULT 0;
    UPDATE accounts SET inventory_complete = 0;",
    // 3: the transfer queue, which survives restarts.
    "CREATE TABLE transfers (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        account_id TEXT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
        direction TEXT NOT NULL,
        file_id TEXT,
        parent_id TEXT,
        name TEXT NOT NULL,
        local_path TEXT NOT NULL,
        mime_type TEXT NOT NULL,
        export_mime TEXT,
        total INTEGER,
        transferred INTEGER NOT NULL DEFAULT 0,
        state TEXT NOT NULL,
        upload_session TEXT,
        replaces_mtime INTEGER,
        error TEXT,
        created INTEGER NOT NULL
    );",
    // 4: content identity for sync, and sync configuration and baselines.
    "ALTER TABLE files ADD COLUMN md5 TEXT;
    ALTER TABLE files ADD COLUMN head_revision TEXT;
    UPDATE accounts SET inventory_complete = 0;
    CREATE TABLE sync_settings (
        account_id TEXT PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
        local_root TEXT NOT NULL,
        paused INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE sync_roots (
        account_id TEXT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
        remote_id TEXT NOT NULL,
        PRIMARY KEY (account_id, remote_id)
    );
    CREATE TABLE sync_baseline (
        account_id TEXT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
        remote_id TEXT NOT NULL,
        path TEXT NOT NULL,
        is_dir INTEGER NOT NULL,
        content TEXT NOT NULL,
        size INTEGER NOT NULL,
        mtime INTEGER NOT NULL,
        device INTEGER NOT NULL,
        inode INTEGER NOT NULL,
        PRIMARY KEY (account_id, remote_id)
    );",
    // 5: shared drives join the inventory, so existing caches index again.
    "UPDATE accounts SET inventory_complete = 0;",
];

pub fn run(conn: &mut Connection) -> Result<()> {
    let known = MIGRATIONS.len() as i64;
    let current: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current > known {
        return Err(Error::NewerSchema {
            found: current,
            known,
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}
