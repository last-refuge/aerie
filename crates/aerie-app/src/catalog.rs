//! Catalogs the browser can show: sample files, or a signed-in Drive's local cache.

use aerie_core::{Catalog, DriveFileId, FileEntry, MemoryCatalog};
use aerie_store::Store;

/// Reads a signed-in account's cached metadata. The engine writes the same database on
/// its own connection; WAL mode lets this one read without waiting.
pub struct StoreCatalog {
    store: Store,
    account: String,
}

impl StoreCatalog {
    pub fn open(account: String) -> aerie_store::Result<Self> {
        Ok(Self {
            store: Store::open(&Store::default_path())?,
            account,
        })
    }

    pub fn root(&self) -> Option<DriveFileId> {
        self.store.root_id(&self.account).ok().flatten()
    }

    pub fn indexing(&self) -> aerie_store::Indexing {
        self.store
            .indexing(&self.account)
            .unwrap_or(aerie_store::Indexing {
                all: true,
                drives: Vec::new(),
            })
    }

    pub fn shared_drives(&self) -> Vec<FileEntry> {
        logged(self.store.shared_drives(&self.account))
    }

    pub fn sync_settings(&self) -> Option<aerie_store::SyncSettings> {
        logged(self.store.sync_settings(&self.account))
    }

    pub fn sync_roots(&self) -> Vec<DriveFileId> {
        logged(self.store.sync_roots(&self.account))
    }

    pub fn transfers(&self) -> Vec<aerie_store::Transfer> {
        logged(self.store.transfers(&self.account))
    }

    pub fn inventory_complete(&self) -> bool {
        self.store
            .inventory_complete(&self.account)
            .unwrap_or(false)
    }
}

/// Read failures are logged and shown as nothing, never as deleted files.
fn logged<T: Default>(result: aerie_store::Result<T>) -> T {
    result.unwrap_or_else(|err| {
        tracing::error!(%err, "couldn't read the metadata cache");
        T::default()
    })
}

impl Catalog for StoreCatalog {
    fn children(&self, folder: &DriveFileId) -> Vec<FileEntry> {
        logged(self.store.children(&self.account, folder))
    }

    fn search(&self, query: &str) -> Vec<FileEntry> {
        logged(self.store.search(&self.account, query, 500))
    }

    fn get(&self, id: &DriveFileId) -> Option<FileEntry> {
        logged(self.store.file(&self.account, id))
    }
}

pub enum AppCatalog {
    Samples(MemoryCatalog),
    Drive(StoreCatalog),
}

impl AppCatalog {
    pub fn is_drive(&self) -> bool {
        matches!(self, Self::Drive(_))
    }
}

impl Catalog for AppCatalog {
    fn children(&self, folder: &DriveFileId) -> Vec<FileEntry> {
        match self {
            Self::Samples(catalog) => catalog.children(folder),
            Self::Drive(catalog) => catalog.children(folder),
        }
    }

    fn search(&self, query: &str) -> Vec<FileEntry> {
        match self {
            Self::Samples(catalog) => catalog.search(query),
            Self::Drive(catalog) => catalog.search(query),
        }
    }

    fn get(&self, id: &DriveFileId) -> Option<FileEntry> {
        match self {
            Self::Samples(catalog) => catalog.get(id),
            Self::Drive(catalog) => catalog.get(id),
        }
    }
}
