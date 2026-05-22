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

//! In-memory Hash table implementation.
//!
//! This module provides a memory-resident hash table implementation optimized for:
//! - Fast O(1) point lookups
//! - Temporary tables and caching
//! - Fast in-memory operations without disk I/O
//! - MVCC support through version chains
//!
//! The implementation uses Rust's standard HashMap for efficient exact-match lookups,
//! storing version chains for MVCC visibility control.
//!
//! Note: Hash tables do NOT support ordered scans or range operations.
//! Use BTree or LSM engines if you need ordered iteration.

use crate::snap::Snapshot;
use crate::table::{
    BatchOps, BatchReport, Flushable, MutableTable, PointLookup, Table, TableCapabilities,
    TableEngineKind, TableError, TableResult, TableStatistics, WriteBatch,
};
use crate::txn::VersionValue;
use crate::txn::{TransactionId, VersionChain};
use crate::types::{ScanBounds, TableId, ValueBuf};
use crate::wal::LogSequenceNumber;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// In-memory Hash table.
///
/// Uses Rust's standard HashMap for O(1) point lookups.
/// Each key maps to a version chain supporting MVCC.
///
/// **Important**: Hash tables do NOT support ordered scans.
/// This implementation provides direct access methods rather than
/// the SearchableTable pattern used by ordered tables.
pub struct MemoryHashTable {
    id: TableId,
    name: String,
    /// Shared hash table data protected by RwLock for concurrent reads
    data: Arc<RwLock<HashMap<Vec<u8>, VersionChain>>>,
    /// Memory usage tracking
    memory_usage: Arc<RwLock<usize>>,
    /// Memory budget in bytes
    memory_budget: usize,
}

impl MemoryHashTable {
    /// Create a new in-memory hash table.
    pub fn new(id: TableId, name: String) -> Self {
        Self::with_budget(id, name, 64 * 1024 * 1024) // 64MB default
    }

    /// Create a new in-memory hash table with a specific memory budget.
    pub fn with_budget(id: TableId, name: String, memory_budget: usize) -> Self {
        Self::with_capacity_and_budget(id, name, 0, memory_budget)
    }

    /// Create a new in-memory hash table with initial capacity and memory budget.
    pub fn with_capacity_and_budget(
        id: TableId,
        name: String,
        capacity: usize,
        memory_budget: usize,
    ) -> Self {
        Self {
            id,
            name,
            data: Arc::new(RwLock::new(HashMap::with_capacity(capacity))),
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

    /// Get a value at a specific snapshot (direct access, no reader needed).
    pub fn get(
        &self,
        key: &[u8],
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Option<ValueBuf>> {
        let data = self.data.read().unwrap();
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
                // Empty value means tombstone (deleted)
                if value.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(ValueBuf(Vec::from(value))));
            }
        }
        Ok(None)
    }

    /// Put a value (direct access, no writer needed for simple cases).
    pub fn put(
        &self,
        key: &[u8],
        value: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<u64> {
        let mut data = self.data.write().unwrap();

        // Calculate memory delta
        let old_size = data
            .get(key)
            .map(|chain| Self::estimate_entry_size(key, chain))
            .unwrap_or(0);

        // Create new version chain
        let prev_version = data.get(key).map(|chain| Box::new(chain.clone()));
        let new_chain = VersionChain {
            value: VersionValue::inline(value.to_vec()),
            created_by: tx_id,
            commit_lsn: Some(commit_lsn),
            prev_version,
        };

        let new_size = Self::estimate_entry_size(key, &new_chain);
        data.insert(key.to_vec(), new_chain);

        // Update memory usage
        let delta = new_size as isize - old_size as isize;
        self.update_memory_usage(delta);

        Ok((key.len() + value.len() + 16) as u64)
    }

    /// Mark all versions created by the given transaction as committed.
    ///
    /// This must be called after flush() to make the changes visible to readers.
    pub fn commit_versions(
        &self,
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        let mut data = self.data.write().unwrap();

        for chain in data.values_mut() {
            // Mark versions created by this transaction as committed
            if chain.created_by == tx_id && chain.commit_lsn.is_none() {
                chain.commit(commit_lsn);
            }
        }

        Ok(())
    }

    /// Delete a value (direct access).
    pub fn delete(&self, key: &[u8]) -> TableResult<bool> {
        let mut data = self.data.write().unwrap();
        if let Some(chain) = data.remove(key) {
            let size = Self::estimate_entry_size(key, &chain);
            self.update_memory_usage(-(size as isize));
            Ok(true)
        } else {
            Ok(false)
        }
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
            // Note: freed_refs contains ValueRefs for external values that need cleanup
            // For in-memory hash tables, we don't use external values, so we can ignore this
            total_removed += removed;
        }

        Ok(total_removed)
    }

    /// Get approximate length.
    pub fn approximate_len(&self) -> TableResult<Option<u64>> {
        let data = self.data.read().unwrap();
        Ok(Some(data.len() as u64))
    }

    /// Batch get operation.
    pub fn batch_get(
        &self,
        keys: &[&[u8]],
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Vec<Option<ValueBuf>>> {
        let data = self.data.read().unwrap();
        let snapshot = Snapshot::new(
            crate::snap::SnapshotId::from(0),
            String::new(),
            snapshot_lsn,
            0,
            0,
            Vec::new(),
        );
        let mut results = Vec::with_capacity(keys.len());

        for key in keys {
            if let Some(chain) = data.get(*key) {
                if let Some(value) = chain.find_visible_inline(&snapshot) {
                    results.push(Some(ValueBuf(Vec::from(value))));
                } else {
                    results.push(None);
                }
            } else {
                results.push(None);
            }
        }

        Ok(results)
    }
}

impl Table for MemoryHashTable {
    fn table_id(&self) -> TableId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> TableEngineKind {
        TableEngineKind::Hash
    }

    fn capabilities(&self) -> TableCapabilities {
        TableCapabilities {
            ordered: false, // Hash tables are NOT ordered
            point_lookup: true,
            prefix_scan: false,  // No prefix scans
            reverse_scan: false, // No reverse scans
            range_delete: false, // No range operations
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
            page_count: None, // Memory-based implementation doesn't use pages
            total_size_bytes: Some(self.get_memory_usage() as u64),
            key_stats: None,
            value_stats: None,
            histogram: None,
            last_updated_lsn: None,
        })
    }
}

// For compatibility with tests, provide empty Reader/Writer types
pub struct MemoryHashTableReader<'a> {
    table: &'a MemoryHashTable,
    snapshot_lsn: LogSequenceNumber,
}

pub struct MemoryHashTableWriter<'a> {
    table: &'a MemoryHashTable,
    tx_id: TransactionId,
    snapshot_lsn: LogSequenceNumber,
    pending_changes: Vec<(Vec<u8>, Option<Vec<u8>>)>, // (key, Some(value) for put, None for delete)
}

impl MemoryHashTable {
    pub fn reader(
        &self,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<MemoryHashTableReader<'_>> {
        Ok(MemoryHashTableReader {
            table: self,
            snapshot_lsn,
        })
    }

    pub fn writer(
        &self,
        tx_id: TransactionId,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<MemoryHashTableWriter<'_>> {
        Ok(MemoryHashTableWriter {
            table: self,
            tx_id,
            snapshot_lsn,
            pending_changes: Vec::new(),
        })
    }
}

impl<'a> PointLookup for MemoryHashTableReader<'a> {
    fn get(&self, key: &[u8], snapshot_lsn: LogSequenceNumber) -> TableResult<Option<ValueBuf>> {
        self.table.get(key, snapshot_lsn)
    }
}

impl<'a> MemoryHashTableReader<'a> {
    pub fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }

    pub fn approximate_len(&self) -> TableResult<Option<u64>> {
        self.table.approximate_len()
    }
}

impl<'a> MutableTable for MemoryHashTableWriter<'a> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> TableResult<u64> {
        self.pending_changes
            .push((key.to_vec(), Some(value.to_vec())));
        Ok((key.len() + value.len() + 16) as u64)
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

    fn range_delete(&mut self, _bounds: ScanBounds) -> TableResult<u64> {
        // Hash tables don't support range operations
        Err(TableError::operation_not_supported(
            "Hash tables do not support range_delete operations",
        ))
    }
}

impl<'a> BatchOps for MemoryHashTableWriter<'a> {
    fn batch_get(&self, keys: &[&[u8]]) -> TableResult<Vec<Option<ValueBuf>>> {
        self.table.batch_get(keys, self.snapshot_lsn)
    }

    fn apply_batch<'b>(&mut self, batch: WriteBatch<'b>) -> TableResult<BatchReport> {
        use crate::table::Mutation;

        let mut report = BatchReport {
            attempted: batch.mutations.len() as u64,
            ..Default::default()
        };

        for mutation in batch.mutations {
            match mutation {
                Mutation::Put { key, value } => {
                    let bytes = self.put(&key, &value)?;
                    report.applied += 1;
                    report.bytes_written += bytes;
                }
                Mutation::Delete { key } => {
                    if self.delete(&key)? {
                        report.deleted += 1;
                        report.applied += 1;
                    }
                }
                Mutation::RangeDelete { .. } => {
                    // Hash tables don't support range operations - skip
                    continue;
                }
                Mutation::Merge { .. } => {
                    // Hash tables don't support merge operations - skip
                    continue;
                }
            }
        }

        Ok(report)
    }
}

impl<'a> Flushable for MemoryHashTableWriter<'a> {
    fn flush(&mut self) -> TableResult<()> {
        if self.pending_changes.is_empty() {
            return Ok(());
        }

        let mut data = self.table.data.write().unwrap();

        for (key, value_opt) in self.pending_changes.drain(..) {
            match value_opt {
                Some(value) => {
                    // Insert or update with uncommitted version
                    let old_size = data
                        .get(&key)
                        .map(|chain| MemoryHashTable::estimate_entry_size(&key, chain))
                        .unwrap_or(0);

                    let prev_version = data.get(&key).map(|chain| Box::new(chain.clone()));
                    let new_chain = VersionChain {
                        value: VersionValue::inline(value),
                        created_by: self.tx_id,
                        commit_lsn: None, // Uncommitted - will be set by commit_versions()
                        prev_version,
                    };

                    let new_size = MemoryHashTable::estimate_entry_size(&key, &new_chain);
                    data.insert(key, new_chain);

                    let delta = new_size as isize - old_size as isize;
                    self.table.update_memory_usage(delta);
                }
                None => {
                    // Delete - create tombstone
                    if let Some(chain) = data.get(&key) {
                        let old_size = MemoryHashTable::estimate_entry_size(&key, chain);

                        let prev_version = Some(Box::new(chain.clone()));
                        let tombstone = VersionChain {
                            value: VersionValue::inline(Vec::new()), // Empty value = tombstone
                            created_by: self.tx_id,
                            commit_lsn: None, // Uncommitted
                            prev_version,
                        };

                        let new_size = MemoryHashTable::estimate_entry_size(&key, &tombstone);
                        data.insert(key, tombstone);

                        let delta = new_size as isize - old_size as isize;
                        self.table.update_memory_usage(delta);
                    }
                }
            }
        }

        Ok(())
    }
}

impl<'a> MemoryHashTableWriter<'a> {
    /// Mark all versions created by this transaction as committed.
    ///
    /// This must be called after flush() to make the changes visible to readers.
    pub fn commit_versions(&self, commit_lsn: LogSequenceNumber) -> TableResult<()> {
        let mut data = self.table.data.write().unwrap();

        for chain in data.values_mut() {
            // Mark versions created by this transaction as committed
            if chain.created_by == self.tx_id && chain.commit_lsn.is_none() {
                chain.commit(commit_lsn);
            }
        }

        Ok(())
    }
}

impl<'a> MemoryHashTableWriter<'a> {
    pub fn tx_id(&self) -> TransactionId {
        self.tx_id
    }

    pub fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_table_basic_operations() {
        let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
        let mut writer = table
            .writer(TransactionId::from(1), LogSequenceNumber::from(1))
            .unwrap();

        // Test put
        let bytes = writer.put(b"key1", b"value1").unwrap();
        assert!(bytes > 0);

        // Flush and commit
        writer.flush().unwrap();
        writer.commit_versions(LogSequenceNumber::from(10)).unwrap();

        // Test get - now visible after commit
        let reader = table.reader(LogSequenceNumber::from(10)).unwrap();
        let value = reader.get(b"key1", LogSequenceNumber::from(10)).unwrap();
        assert_eq!(value, Some(ValueBuf(b"value1".to_vec())));

        // Test delete
        let mut writer = table
            .writer(TransactionId::from(2), LogSequenceNumber::from(2))
            .unwrap();
        assert!(writer.delete(b"key1").unwrap());
        writer.flush().unwrap();
        writer.commit_versions(LogSequenceNumber::from(20)).unwrap();

        let reader = table.reader(LogSequenceNumber::from(20)).unwrap();
        let value = reader.get(b"key1", LogSequenceNumber::from(20)).unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn test_hash_table_no_range_delete() {
        let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
        let mut writer = table
            .writer(TransactionId::from(1), LogSequenceNumber::from(1))
            .unwrap();

        // Range delete should fail
        let result = writer.range_delete(ScanBounds::All);
        assert!(result.is_err());
    }
}

// Made with Bob
