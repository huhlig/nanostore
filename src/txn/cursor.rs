//
// Copyright 2025-2026 Hans W. Uhlig. All Rights Reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

//! Cursor implementation for iterating over table entries.

use crate::table::TableCursor;
use crate::txn::CursorResult;
use crate::wal::LogSequenceNumber;

/// Ordered cursor over table or index entries.
///
/// Cursors provide snapshot isolation: once opened, they read from a
/// stable view at a specific LSN and are not invalidated by concurrent writers.
///
/// This is a wrapper around a table-specific cursor implementation that
/// provides a uniform interface for transaction-level operations.
pub struct Cursor {
    /// The underlying table cursor implementation.
    inner: Box<dyn TableCursor>,
}

impl Cursor {
    /// Create a new cursor wrapping a table cursor implementation.
    #[must_use]
    pub fn new(inner: Box<dyn TableCursor>) -> Self {
        Self { inner }
    }

    /// Check if the cursor is positioned at a valid entry.
    ///
    /// Returns `false` when the cursor has moved past the end of the scan
    /// bounds or before the beginning.
    #[must_use]
    pub fn valid(&self) -> bool {
        self.inner.valid()
    }

    /// Check if the cursor is still valid (not invalidated).
    ///
    /// Returns `false` if the cursor's snapshot has been released or an
    /// unrecoverable error has occurred.
    ///
    /// Note: Currently this is the same as `valid()`. In the future, this
    /// may track cursor invalidation separately from position validity.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.inner.valid()
    }

    /// Get the snapshot LSN at which this cursor reads.
    #[must_use]
    pub fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.inner.snapshot_lsn()
    }

    /// Get the key at the current cursor position.
    ///
    /// Returns `None` if the cursor is not positioned at a valid entry.
    /// The returned slice is borrowed and valid until the next cursor operation.
    #[must_use]
    pub fn key(&self) -> Option<&[u8]> {
        self.inner.key()
    }

    /// Get the value at the current cursor position.
    ///
    /// Returns `None` if the cursor is not positioned at a valid entry.
    /// The returned slice is borrowed and valid until the next cursor operation.
    #[must_use]
    pub fn value(&self) -> Option<&[u8]> {
        self.inner.value()
    }

    /// Move to the first entry in the scan bounds.
    pub fn first(&mut self) -> CursorResult<()> {
        self.inner
            .first()
            .map_err(|e| crate::txn::CursorError::Other(e.to_string()))
    }

    /// Move to the last entry in the scan bounds.
    pub fn last(&mut self) -> CursorResult<()> {
        self.inner
            .last()
            .map_err(|e| crate::txn::CursorError::Other(e.to_string()))
    }

    /// Seek to the first entry with a key greater than or equal to the supplied key.
    pub fn seek(&mut self, key: &[u8]) -> CursorResult<()> {
        self.inner
            .seek(key)
            .map_err(|e| crate::txn::CursorError::Other(e.to_string()))
    }

    /// Seek to the greatest key less than or equal to the supplied key.
    pub fn seek_prev(&mut self, key: &[u8]) -> CursorResult<()> {
        self.inner
            .seek_for_prev(key)
            .map_err(|e| crate::txn::CursorError::Other(e.to_string()))
    }

    /// Move to the next entry.
    pub fn move_next(&mut self) -> CursorResult<()> {
        self.inner
            .next()
            .map_err(|e| crate::txn::CursorError::Other(e.to_string()))
    }

    /// Move to the previous entry.
    pub fn prev(&mut self) -> CursorResult<()> {
        self.inner
            .prev()
            .map_err(|e| crate::txn::CursorError::Other(e.to_string()))
    }
}
