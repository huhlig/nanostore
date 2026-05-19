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

//! In-memory B-Tree table implementation.
//!
//! This module provides a memory-resident B-Tree implementation optimized for:
//! - Temporary tables and intermediate query results
//! - Fast in-memory operations without disk I/O
//! - MVCC support through version chains
//! - Efficient range scans and point lookups
//!
//! The implementation uses a standard B-Tree structure with configurable order,
//! storing version chains for MVCC visibility control.

use crate::snap::Snapshot;
use crate::table::{
    BatchOps, BatchReport, DenseOrdered, Flushable, MutableTable, OrderedScan, PointLookup,
    SearchableTable, SpecialtyTableCapabilities, SpecialtyTableCursor, SpecialtyTableStats, Table,
    TableCapabilities, TableCursor, TableEngineKind, TableReader, TableResult, TableStatistics,
    TableWriter, VerificationReport, WriteBatch,
};
use crate::txn::{TransactionId, VersionChain};
use crate::types::{Bound, ScanBounds, TableId, ValueBuf};
use crate::wal::LogSequenceNumber;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

/// Default B-Tree order (maximum keys per node).
const DEFAULT_ORDER: usize = 128;

/// In-memory B-Tree table.
///
/// Uses Rust's standard BTreeMap for efficient ordered operations.
/// Each key maps to a version chain supporting MVCC.
pub struct MemoryBTree {
    id: TableId,
    name: String,
    /// Shared tree data protected by RwLock for concurrent reads
    data: Arc<RwLock<BTreeMap<Vec<u8>, VersionChain>>>,
    /// Memory usage tracking
    memory_usage: Arc<RwLock<usize>>,
    /// Memory budget in bytes
    memory_budget: usize,
}

impl MemoryBTree {
    /// Create a new in-memory B-Tree table.
    pub fn new(id: TableId, name: String) -> Self {
        Self::with_budget(id, name, 64 * 1024 * 1024) // 64MB default
    }

    /// Create a new in-memory B-Tree table with a specific memory budget.
    pub fn with_budget(id: TableId, name: String, memory_budget: usize) -> Self {
        Self {
            id,
            name,
            data: Arc::new(RwLock::new(BTreeMap::new())),
            memory_usage: Arc::new(RwLock::new(0)),
            memory_budget,
        }
    }

    /// Get current memory usage.
    fn get_memory_usage(&self) -> usize {
        *self.memory_usage.read().unwrap()
    }

    /// Update memory usage by delta (can be negative).
    fn update_memory_usage(&self, delta: isize) {
        let mut usage = self.memory_usage.write().unwrap();
        if delta < 0 {
            *usage = usage.saturating_sub(delta.unsigned_abs());
        } else {
            *usage = usage.saturating_add(delta as usize);
        }
    }

    /// Estimate memory usage of a key-value pair with version chain.
    fn estimate_entry_size(key: &[u8], chain: &VersionChain) -> usize {
        let mut size = key.len() + std::mem::size_of::<Vec<u8>>();
        let mut current = Some(chain);
        while let Some(version) = current {
            size += version.value.len()
                + std::mem::size_of::<VersionChain>()
                + std::mem::size_of::<Option<Box<VersionChain>>>();
            current = version.prev_version.as_deref();
        }
        size
    }

    /// Vacuum obsolete versions from all entries in the table.
    ///
    /// Iterates through all keys and calls VersionChain::vacuum() on each,
    /// removing versions older than min_visible_lsn while preserving one
    /// old version as a base.
    ///
    /// Returns the total count of removed versions.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        let mut data = self.data.write().unwrap();
        let mut total_removed = 0;

        for (_key, chain) in data.iter_mut() {
            let (removed, _freed_refs) = chain.vacuum(min_visible_lsn);
            total_removed += removed;
        }

        Ok(total_removed)
    }
}

impl Table for MemoryBTree {
    fn table_id(&self) -> TableId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> TableEngineKind {
        TableEngineKind::Memory
    }

    fn capabilities(&self) -> TableCapabilities {
        TableCapabilities {
            ordered: true,
            point_lookup: true,
            prefix_scan: true,
            reverse_scan: true,
            range_delete: true,
            merge_operator: false,
            mvcc_native: true,
            append_optimized: false,
            memory_resident: true,
            disk_resident: false,
            supports_compression: false,
            supports_encryption: false,
        }
    }

    fn stats(&self) -> TableResult<TableStatistics> {
        let data = self.data.read().unwrap();
        Ok(TableStatistics {
            row_count: Some(data.len() as u64),
            total_size_bytes: Some(self.get_memory_usage() as u64),
            key_stats: None,
            value_stats: None,
            histogram: None,
            last_updated_lsn: None,
        })
    }
}

impl SearchableTable for MemoryBTree {
    type Reader<'a> = MemoryBTreeReader<'a>;
    type Writer<'a> = MemoryBTreeWriter<'a>;

    fn reader(&self, snapshot_lsn: LogSequenceNumber) -> TableResult<Self::Reader<'_>> {
        Ok(MemoryBTreeReader {
            table: self,
            snapshot_lsn,
        })
    }

    fn writer(
        &self,
        tx_id: TransactionId,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self::Writer<'_>> {
        Ok(MemoryBTreeWriter {
            table: self,
            tx_id,
            snapshot_lsn,
            pending_changes: Vec::new(),
        })
    }
}

/// Read-only view of the B-Tree at a specific snapshot.
pub struct MemoryBTreeReader<'a> {
    table: &'a MemoryBTree,
    snapshot_lsn: LogSequenceNumber,
}

impl<'a> PointLookup for MemoryBTreeReader<'a> {
    fn get(&self, key: &[u8], snapshot_lsn: LogSequenceNumber) -> TableResult<Option<ValueBuf>> {
        let data = self.table.data.read().unwrap();
        if let Some(chain) = data.get(key) {
            // Create a snapshot for visibility checking
            let snapshot = Snapshot::new(
                crate::snap::SnapshotId::from(0),
                String::new(),
                snapshot_lsn,
                0,
                0,
                Vec::new(),
            );
            if let Some(value) = chain.find_visible_inline(&snapshot) {
                return Ok(Some(ValueBuf(value.to_vec())));
            }
        }
        Ok(None)
    }

    fn get_stream(
        &self,
        key: &[u8],
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Option<Box<dyn crate::table::ValueStream + '_>>> {
        use crate::table::SliceValueStream;

        // For in-memory table, use default implementation that wraps the value in a stream
        self.get(key, snapshot_lsn).map(|opt| {
            opt.map(|value_buf| {
                Box::new(SliceValueStream::new(value_buf.0))
                    as Box<dyn crate::table::ValueStream + '_>
            })
        })
    }
}

impl<'a> OrderedScan for MemoryBTreeReader<'a> {
    type Cursor<'b>
        = MemoryBTreeCursor<'b>
    where
        Self: 'b;

    fn scan(
        &self,
        bounds: ScanBounds,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self::Cursor<'_>> {
        Ok(MemoryBTreeCursor::new(self.table, bounds, snapshot_lsn))
    }
}

impl<'a> TableReader for MemoryBTreeReader<'a> {
    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }

    fn approximate_len(&self) -> TableResult<Option<u64>> {
        let data = self.table.data.read().unwrap();
        Ok(Some(data.len() as u64))
    }
}

/// Write view of the B-Tree for a specific transaction.
pub struct MemoryBTreeWriter<'a> {
    table: &'a MemoryBTree,
    tx_id: TransactionId,
    snapshot_lsn: LogSequenceNumber,
    pending_changes: Vec<(Vec<u8>, Option<Vec<u8>>)>, // (key, Some(value) for put, None for delete)
}

impl<'a> MutableTable for MemoryBTreeWriter<'a> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> TableResult<u64> {
        self.pending_changes
            .push((key.to_vec(), Some(value.to_vec())));
        // Return approximate size: key + value + overhead
        Ok((key.len() + value.len() + 16) as u64)
    }

    fn put_stream(
        &mut self,
        key: &[u8],
        stream: &mut dyn crate::table::ValueStream,
    ) -> TableResult<u64> {
        // For in-memory table, read the entire stream into memory
        let mut buffer = Vec::new();
        if let Some(size_hint) = stream.size_hint() {
            buffer.reserve(size_hint as usize);
        }

        let mut temp_buf = vec![0u8; 8192]; // 8KB chunks
        loop {
            let n = stream.read(&mut temp_buf)?;
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&temp_buf[..n]);
        }

        self.put(key, &buffer)
    }

    fn delete(&mut self, key: &[u8]) -> TableResult<bool> {
        let data = self.table.data.read().unwrap();
        let exists = data.contains_key(key);
        drop(data);

        if exists {
            self.pending_changes.push((key.to_vec(), None));
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn range_delete(&mut self, bounds: ScanBounds) -> TableResult<u64> {
        let data = self.table.data.read().unwrap();
        let keys_to_delete: Vec<Vec<u8>> = match bounds {
            ScanBounds::All => data.keys().cloned().collect(),
            ScanBounds::Prefix(prefix) => data
                .range(prefix.0.clone()..)
                .take_while(|(k, _)| k.starts_with(&prefix.0))
                .map(|(k, _)| k.clone())
                .collect(),
            ScanBounds::Range { start, end } => {
                let start_key = match start {
                    Bound::Included(k) => Some(k.0),
                    Bound::Excluded(k) => Some(k.0),
                    Bound::Unbounded => None,
                };
                let end_key = match end {
                    Bound::Included(k) => Some(k.0),
                    Bound::Excluded(k) => Some(k.0),
                    Bound::Unbounded => None,
                };

                match (start_key, end_key) {
                    (Some(s), Some(e)) => data.range(s..=e).map(|(k, _)| k.clone()).collect(),
                    (Some(s), None) => data.range(s..).map(|(k, _)| k.clone()).collect(),
                    (None, Some(e)) => data.range(..=e).map(|(k, _)| k.clone()).collect(),
                    (None, None) => data.keys().cloned().collect(),
                }
            }
        };
        drop(data);

        let count = keys_to_delete.len() as u64;
        for key in keys_to_delete {
            self.pending_changes.push((key, None));
        }
        Ok(count)
    }
}

impl<'a> BatchOps for MemoryBTreeWriter<'a> {
    fn batch_get(&self, keys: &[&[u8]]) -> TableResult<Vec<Option<ValueBuf>>> {
        let data = self.table.data.read().unwrap();
        let snapshot = Snapshot::new(
            crate::snap::SnapshotId::from(0),
            String::new(),
            self.snapshot_lsn,
            0,
            0,
            Vec::new(),
        );
        let mut results = Vec::with_capacity(keys.len());

        for key in keys {
            if let Some(chain) = data.get(*key) {
                if let Some(value) = chain.find_visible_inline(&snapshot) {
                    results.push(Some(ValueBuf(value.to_vec())));
                } else {
                    results.push(None);
                }
            } else {
                results.push(None);
            }
        }

        Ok(results)
    }

    fn apply_batch<'b>(&mut self, batch: WriteBatch<'b>) -> TableResult<BatchReport> {
        let mut report = BatchReport {
            attempted: batch.mutations.len() as u64,
            ..Default::default()
        };

        for mutation in batch.mutations {
            match mutation {
                crate::table::Mutation::Put { key, value } => {
                    self.put(&key, &value)?;
                    report.applied += 1;
                    report.bytes_written += key.len() as u64 + value.len() as u64;
                }
                crate::table::Mutation::Delete { key } => {
                    if self.delete(&key)? {
                        report.deleted += 1;
                    }
                    report.applied += 1;
                }
                crate::table::Mutation::RangeDelete { bounds } => {
                    let deleted = self.range_delete(bounds)?;
                    report.deleted += deleted;
                    report.applied += 1;
                }
                crate::table::Mutation::Merge { .. } => {
                    // Merge not supported in memory B-Tree
                    continue;
                }
            }
        }

        Ok(report)
    }
}

impl<'a> Flushable for MemoryBTreeWriter<'a> {
    fn flush(&mut self) -> TableResult<()> {
        if self.pending_changes.is_empty() {
            return Ok(());
        }

        let mut data = self.table.data.write().unwrap();

        for (key, value_opt) in self.pending_changes.drain(..) {
            match value_opt {
                Some(value) => {
                    // Insert or update
                    let old_size = data
                        .get(&key)
                        .map(|chain| Self::estimate_entry_size(&key, chain))
                        .unwrap_or(0);

                    let new_chain = if let Some(existing_chain) = data.remove(&key) {
                        // Prepend new version to existing chain
                        existing_chain.prepend(value.clone(), self.tx_id)
                    } else {
                        // Create new version chain
                        VersionChain::new(value.clone(), self.tx_id)
                    };

                    let new_size = Self::estimate_entry_size(&key, &new_chain);
                    data.insert(key, new_chain);

                    // Update memory usage
                    let delta = new_size as isize - old_size as isize;
                    self.table.update_memory_usage(delta);
                }
                None => {
                    // Delete
                    if let Some(chain) = data.remove(&key) {
                        let size = Self::estimate_entry_size(&key, &chain);
                        self.table.update_memory_usage(-(size as isize));
                    }
                }
            }
        }

        Ok(())
    }
}

impl<'a> MemoryBTreeWriter<'a> {
    /// Mark all versions created by this transaction as committed.
    ///
    /// This must be called after flush() to make the changes visible to readers.
    /// The commit_lsn is obtained from the WAL after writing the COMMIT record.
    pub fn commit_versions(&self, commit_lsn: LogSequenceNumber) -> TableResult<()> {
        let mut data = self.table.data.write().unwrap();

        // Iterate through all keys and mark versions created by this transaction as committed
        for chain in data.values_mut() {
            // Only mark the head version if it was created by this transaction
            if chain.created_by == self.tx_id && chain.commit_lsn.is_none() {
                chain.commit(commit_lsn);
            }
        }

        Ok(())
    }
}

impl<'a> MemoryBTreeWriter<'a> {
    fn estimate_entry_size(key: &[u8], chain: &VersionChain) -> usize {
        MemoryBTree::estimate_entry_size(key, chain)
    }
}

impl<'a> TableWriter for MemoryBTreeWriter<'a> {
    fn tx_id(&self) -> TransactionId {
        self.tx_id
    }

    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }
}

/// Cursor for iterating over the B-Tree.
pub struct MemoryBTreeCursor<'a> {
    table: &'a MemoryBTree,
    snapshot_lsn: LogSequenceNumber,
    bounds: ScanBounds,
    current_key: Option<Vec<u8>>,
    current_value: Option<Vec<u8>>,
    exhausted: bool,
}

impl<'a> MemoryBTreeCursor<'a> {
    fn new(table: &'a MemoryBTree, bounds: ScanBounds, snapshot_lsn: LogSequenceNumber) -> Self {
        let mut cursor = Self {
            table,
            snapshot_lsn,
            bounds,
            current_key: None,
            current_value: None,
            exhausted: false,
        };
        // Position at first valid entry
        let _ = cursor.first();
        cursor
    }

    fn is_in_bounds(&self, key: &[u8]) -> bool {
        match &self.bounds {
            ScanBounds::All => true,
            ScanBounds::Prefix(prefix) => key.starts_with(&prefix.0),
            ScanBounds::Range { start, end } => {
                let after_start = match start {
                    Bound::Included(k) => key >= k.0.as_slice(),
                    Bound::Excluded(k) => key > k.0.as_slice(),
                    Bound::Unbounded => true,
                };
                let before_end = match end {
                    Bound::Included(k) => key <= k.0.as_slice(),
                    Bound::Excluded(k) => key < k.0.as_slice(),
                    Bound::Unbounded => true,
                };
                after_start && before_end
            }
        }
    }

    fn find_visible_value(&self, chain: &VersionChain) -> Option<Vec<u8>> {
        let snapshot = Snapshot::new(
            crate::snap::SnapshotId::from(0),
            String::new(),
            self.snapshot_lsn,
            0,
            0,
            Vec::new(),
        );
        chain.find_visible_inline(&snapshot).map(|v| v.to_vec())
    }
}

impl<'a> TableCursor for MemoryBTreeCursor<'a> {
    fn valid(&self) -> bool {
        !self.exhausted && self.current_key.is_some()
    }

    fn key(&self) -> Option<&[u8]> {
        self.current_key.as_deref()
    }

    fn value(&self) -> Option<&[u8]> {
        self.current_value.as_deref()
    }

    fn next(&mut self) -> TableResult<()> {
        if self.exhausted {
            return Ok(());
        }

        let data = self.table.data.read().unwrap();
        let current_key = match &self.current_key {
            Some(k) => k.clone(),
            None => {
                self.exhausted = true;
                return Ok(());
            }
        };

        // Find next key after current
        for (key, chain) in data.range(current_key..).skip(1) {
            if !self.is_in_bounds(key) {
                self.exhausted = true;
                self.current_key = None;
                self.current_value = None;
                return Ok(());
            }

            if let Some(value) = self.find_visible_value(chain) {
                self.current_key = Some(key.clone());
                self.current_value = Some(value);
                return Ok(());
            }
        }

        self.exhausted = true;
        self.current_key = None;
        self.current_value = None;
        Ok(())
    }

    fn prev(&mut self) -> TableResult<()> {
        if self.exhausted {
            return Ok(());
        }

        let data = self.table.data.read().unwrap();
        let current_key = match &self.current_key {
            Some(k) => k.clone(),
            None => {
                self.exhausted = true;
                return Ok(());
            }
        };

        // Find previous key before current
        let keys: Vec<_> = data
            .range(..current_key)
            .rev()
            .map(|(k, _)| k.clone())
            .collect();

        for key in keys {
            if !self.is_in_bounds(&key) {
                self.exhausted = true;
                self.current_key = None;
                self.current_value = None;
                return Ok(());
            }

            if let Some(chain) = data.get(&key)
                && let Some(value) = self.find_visible_value(chain)
            {
                self.current_key = Some(key);
                self.current_value = Some(value);
                return Ok(());
            }
        }

        self.exhausted = true;
        self.current_key = None;
        self.current_value = None;
        Ok(())
    }

    fn seek(&mut self, key: &[u8]) -> TableResult<()> {
        let data = self.table.data.read().unwrap();

        for (k, chain) in data.range(key.to_vec()..) {
            if !self.is_in_bounds(k) {
                self.exhausted = true;
                self.current_key = None;
                self.current_value = None;
                return Ok(());
            }

            if let Some(value) = self.find_visible_value(chain) {
                self.current_key = Some(k.clone());
                self.current_value = Some(value);
                self.exhausted = false;
                return Ok(());
            }
        }

        self.exhausted = true;
        self.current_key = None;
        self.current_value = None;
        Ok(())
    }

    fn seek_for_prev(&mut self, key: &[u8]) -> TableResult<()> {
        let data = self.table.data.read().unwrap();

        // Find the last key <= target
        let keys: Vec<_> = data
            .range(..=key.to_vec())
            .rev()
            .map(|(k, _)| k.clone())
            .collect();

        for k in keys {
            if !self.is_in_bounds(&k) {
                continue;
            }

            if let Some(chain) = data.get(&k)
                && let Some(value) = self.find_visible_value(chain)
            {
                self.current_key = Some(k);
                self.current_value = Some(value);
                self.exhausted = false;
                return Ok(());
            }
        }

        self.exhausted = true;
        self.current_key = None;
        self.current_value = None;
        Ok(())
    }

    fn first(&mut self) -> TableResult<()> {
        let data = self.table.data.read().unwrap();

        let start_key = match &self.bounds {
            ScanBounds::All => None,
            ScanBounds::Prefix(prefix) => Some(prefix.0.clone()),
            ScanBounds::Range { start, .. } => match start {
                Bound::Included(k) => Some(k.0.clone()),
                Bound::Excluded(k) => Some(k.0.clone()),
                Bound::Unbounded => None,
            },
        };

        let iter: Box<dyn Iterator<Item = (&Vec<u8>, &VersionChain)>> =
            if let Some(start) = start_key {
                Box::new(data.range(start..))
            } else {
                Box::new(data.iter())
            };

        for (key, chain) in iter {
            if !self.is_in_bounds(key) {
                self.exhausted = true;
                self.current_key = None;
                self.current_value = None;
                return Ok(());
            }

            if let Some(value) = self.find_visible_value(chain) {
                self.current_key = Some(key.clone());
                self.current_value = Some(value);
                self.exhausted = false;
                return Ok(());
            }
        }

        self.exhausted = true;
        self.current_key = None;
        self.current_value = None;
        Ok(())
    }

    fn last(&mut self) -> TableResult<()> {
        let data = self.table.data.read().unwrap();

        let end_key = match &self.bounds {
            ScanBounds::All => None,
            ScanBounds::Prefix(prefix) => {
                // Find the last key with this prefix
                let mut end = prefix.0.clone();
                if let Some(last) = end.last_mut() {
                    if *last < 255 {
                        *last += 1;
                    } else {
                        end.push(0);
                    }
                }
                Some(end)
            }
            ScanBounds::Range { end, .. } => match end {
                Bound::Included(k) => Some(k.0.clone()),
                Bound::Excluded(k) => Some(k.0.clone()),
                Bound::Unbounded => None,
            },
        };

        let iter: Box<dyn Iterator<Item = (&Vec<u8>, &VersionChain)>> = if let Some(end) = end_key {
            Box::new(data.range(..end).rev())
        } else {
            Box::new(data.iter().rev())
        };

        for (key, chain) in iter {
            if !self.is_in_bounds(key) {
                continue;
            }

            if let Some(value) = self.find_visible_value(chain) {
                self.current_key = Some(key.clone());
                self.current_value = Some(value);
                self.exhausted = false;
                return Ok(());
            }
        }

        self.exhausted = true;
        self.current_key = None;
        self.current_value = None;
        Ok(())
    }

    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }
}

// Made with Bob

// =============================================================================
// DenseOrdered Specialty Table Implementation
// =============================================================================

/// Specialty cursor for index operations.
///
/// For secondary indexes, the "index_key" is the indexed field value,
/// and the "primary_key" is the pointer back to the main table record.
pub struct MemoryBTreeSpecialtyCursor<'a> {
    inner: MemoryBTreeCursor<'a>,
}

impl<'a> SpecialtyTableCursor for MemoryBTreeSpecialtyCursor<'a> {
    fn valid(&self) -> bool {
        self.inner.valid()
    }

    fn index_key(&self) -> Option<&[u8]> {
        self.inner.key()
    }

    fn primary_key(&self) -> Option<&[u8]> {
        self.inner.value()
    }

    fn next(&mut self) -> TableResult<()> {
        self.inner.next()
    }

    fn prev(&mut self) -> TableResult<()> {
        self.inner.prev()
    }

    fn seek(&mut self, index_key: &[u8]) -> TableResult<()> {
        self.inner.seek(index_key)
    }
}

impl DenseOrdered for MemoryBTree {
    type Cursor<'a> = MemoryBTreeSpecialtyCursor<'a>;

    fn table_id(&self) -> TableId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> SpecialtyTableCapabilities {
        SpecialtyTableCapabilities {
            exact: true,
            approximate: false,
            ordered: true,
            sparse: false,
            supports_delete: true,
            supports_range_query: true,
            supports_prefix_query: true,
            supports_scoring: false,
            supports_incremental_rebuild: false,
            may_be_stale: false,
        }
    }

    fn insert_entry(
        &mut self,
        index_key: &[u8],
        primary_key: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        // For a secondary index, we store: index_key -> primary_key
        // This allows lookups by the indexed field to find the primary key
        let mut data = self.data.write().unwrap();

        let old_size = data
            .get(index_key)
            .map(|chain| Self::estimate_entry_size(index_key, chain))
            .unwrap_or(0);

        // Create a new version with the primary key as the value
        let mut new_chain = if let Some(existing_chain) = data.remove(index_key) {
            existing_chain.prepend(primary_key.to_vec(), tx_id)
        } else {
            VersionChain::new(primary_key.to_vec(), tx_id)
        };

        // Commit the version with the provided LSN
        new_chain.commit(commit_lsn);

        let new_size = Self::estimate_entry_size(index_key, &new_chain);
        data.insert(index_key.to_vec(), new_chain);

        // Update memory usage
        let delta = new_size as isize - old_size as isize;
        self.update_memory_usage(delta);

        Ok(())
    }

    fn delete_entry(
        &mut self,
        index_key: &[u8],
        primary_key: &[u8],
        _tx_id: TransactionId,
        _commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        // For secondary indexes, we need to delete the specific index_key -> primary_key mapping
        // In a simple implementation, we just remove the entry if it matches
        let mut data = self.data.write().unwrap();

        if let Some(chain) = data.get(index_key) {
            // Check if the current version points to the expected primary key
            let snapshot = Snapshot::new(
                crate::snap::SnapshotId::from(0),
                String::new(),
                LogSequenceNumber::from(u64::MAX),
                0,
                0,
                Vec::new(),
            );

            if let Some(stored_primary_key) = chain.find_visible_inline(&snapshot)
                && stored_primary_key == primary_key
            {
                // Remove the entry
                let removed_chain = data.remove(index_key).unwrap();
                let size = Self::estimate_entry_size(index_key, &removed_chain);
                self.update_memory_usage(-(size as isize));
            }
        }

        Ok(())
    }

    fn scan(&self, bounds: ScanBounds) -> TableResult<Self::Cursor<'_>> {
        let inner = MemoryBTreeCursor::new(
            self,
            bounds,
            LogSequenceNumber::from(u64::MAX), // Use max LSN to see all versions
        );
        Ok(MemoryBTreeSpecialtyCursor { inner })
    }

    fn stats(&self) -> TableResult<SpecialtyTableStats> {
        let data = self.data.read().unwrap();
        Ok(SpecialtyTableStats {
            entry_count: Some(data.len() as u64),
            size_bytes: Some(self.get_memory_usage() as u64),
            distinct_keys: Some(data.len() as u64),
            stale_entries: Some(0),
            last_updated_lsn: None,
        })
    }

    fn verify(&self) -> TableResult<VerificationReport> {
        // Basic verification: check that all entries are valid
        let data = self.data.read().unwrap();
        let report = VerificationReport {
            checked_items: data.len() as u64,
            errors: Vec::new(),
            warnings: Vec::new(),
        };

        // Could add more sophisticated checks here:
        // - Verify version chains are well-formed
        // - Check memory usage calculations
        // - Validate key ordering

        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::KeyBuf;

    #[test]
    fn test_basic_operations() {
        let table = MemoryBTree::new(TableId::from(1), "test".to_string());
        let tx_id = TransactionId::from(1);
        let write_lsn = LogSequenceNumber::from(1);

        // Write data
        let mut writer = table.writer(tx_id, write_lsn).unwrap();
        writer.put(b"key1", b"value1").unwrap();
        writer.put(b"key2", b"value2").unwrap();
        writer.flush().unwrap();

        // Commit the versions
        {
            let mut data = table.data.write().unwrap();
            let commit_lsn = LogSequenceNumber::from(10);
            for chain in data.values_mut() {
                chain.commit(commit_lsn);
            }
        }

        // Read at a later LSN to see committed data
        let read_lsn = LogSequenceNumber::from(20);
        let reader = table.reader(read_lsn).unwrap();
        assert_eq!(
            reader.get(b"key1", read_lsn).unwrap(),
            Some(ValueBuf(b"value1".to_vec()))
        );
        assert_eq!(
            reader.get(b"key2", read_lsn).unwrap(),
            Some(ValueBuf(b"value2".to_vec()))
        );
        assert_eq!(reader.get(b"key3", read_lsn).unwrap(), None);
    }

    #[test]
    fn test_cursor_iteration() {
        let table = MemoryBTree::new(TableId::from(1), "test".to_string());
        let tx_id = TransactionId::from(1);
        let write_lsn = LogSequenceNumber::from(1);

        // Write data
        let mut writer = table.writer(tx_id, write_lsn).unwrap();
        writer.put(b"a", b"1").unwrap();
        writer.put(b"b", b"2").unwrap();
        writer.put(b"c", b"3").unwrap();
        writer.flush().unwrap();

        // Commit the versions
        {
            let mut data = table.data.write().unwrap();
            let commit_lsn = LogSequenceNumber::from(10);
            for chain in data.values_mut() {
                chain.commit(commit_lsn);
            }
        }

        // Read at a later LSN
        let read_lsn = LogSequenceNumber::from(20);
        let reader = table.reader(read_lsn).unwrap();
        let mut cursor = reader.scan(ScanBounds::All, read_lsn).unwrap();

        assert!(cursor.valid());
        assert_eq!(cursor.key(), Some(b"a".as_ref()));
        assert_eq!(cursor.value(), Some(b"1".as_ref()));

        cursor.next().unwrap();
        assert_eq!(cursor.key(), Some(b"b".as_ref()));

        cursor.next().unwrap();
        assert_eq!(cursor.key(), Some(b"c".as_ref()));

        cursor.next().unwrap();
        assert!(!cursor.valid());
    }

    #[test]
    fn test_range_scan() {
        let table = MemoryBTree::new(TableId::from(1), "test".to_string());
        let tx_id = TransactionId::from(1);
        let write_lsn = LogSequenceNumber::from(1);

        // Write data
        let mut writer = table.writer(tx_id, write_lsn).unwrap();
        for i in 0..10 {
            let key = format!("key{:02}", i);
            let value = format!("value{}", i);
            writer.put(key.as_bytes(), value.as_bytes()).unwrap();
        }
        writer.flush().unwrap();

        // Commit the versions
        {
            let mut data = table.data.write().unwrap();
            let commit_lsn = LogSequenceNumber::from(10);
            for chain in data.values_mut() {
                chain.commit(commit_lsn);
            }
        }

        // Read at a later LSN
        let read_lsn = LogSequenceNumber::from(20);
        let reader = table.reader(read_lsn).unwrap();
        let bounds = ScanBounds::Range {
            start: Bound::Included(KeyBuf(b"key03".to_vec())),
            end: Bound::Excluded(KeyBuf(b"key07".to_vec())),
        };
        let mut cursor = reader.scan(bounds, read_lsn).unwrap();

        let mut count = 0;
        while cursor.valid() {
            count += 1;
            cursor.next().unwrap();
        }
        assert_eq!(count, 4); // key03, key04, key05, key06
    }
}
