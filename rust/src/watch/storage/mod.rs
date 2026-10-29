//! Agent A: transactional metadata store (SQLite WAL) implementing domain::Store.
//!
//! `open(path)` for the head, `memory()` for tests, `legacy::import` for the old JSONL journals.

use crate::watch::domain::{ApiError, Store};
use std::path::Path;
use std::sync::Arc;

pub mod legacy;
mod sqlite;
#[cfg(test)]
pub(crate) mod tests;

/// Open (creating and migrating if needed) the SQLite database at `path`.
pub fn open(path: &Path) -> Result<Arc<dyn Store>, ApiError> {
    Ok(Arc::new(sqlite::Sqlite::open(path)?))
}

/// Fresh in-memory store with the same semantics (SQLite `:memory:`).
pub fn memory() -> Arc<dyn Store> {
    Arc::new(sqlite::Sqlite::memory().expect("in-memory sqlite"))
}
