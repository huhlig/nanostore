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

//! Memtable implementation for LSM tree.
//!
//! The memtable is an in-memory write buffer that stores recent writes in sorted order.
//! It supports MVCC through version chains and provides efficient concurrent reads.
//!
//! # Architecture
//!
//! - **Skip list**: Probabilistic data structure for O(log n) operations
//! - **MVCC**: Version chains for snapshot isolation
//! - **Memory tracking**: Tracks memory usage and enforces budget
//! - **Atomic conversion**: Can be atomically converted to immutable state
//!
//! # Lifecycle
//!
//! 1. **Active**: Accepts writes, tracks memory usage
//! 2. **Immutable**: Read-only, ready for flush to SSTable
//! 3. **Flushed**: Converted to SSTable on disk

use crate::table::error::{TableError, TableResult};
use crate::txn::{TransactionId, VersionChain, VersionValue};
use crate::wal::LogSequenceNumber;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

/// Memtable for in-memory write buffer.
///
/// Uses a skip list (implemented via BTreeMap for simplicity) to maintain
/// sorted order of keys. Each key maps to a version chain for MVCC support.
#[derive(Clone)]
pub struct Memtable {
    /// Sorted map of keys to version chains
    data: Arc<RwLock<BTreeMap<Vec<u8>, VersionChain>>>,

    /// Current memory usage in bytes
    memory_usage: Arc<AtomicUsize>,

    /// Whether this memtable is immutable (read-only)
    immutable: Arc<AtomicBool>,

    /// Maximum memory size before flush
    max_size: usize,

    /// Minimum LSN of all entries (for flush ordering)
    min_lsn: Arc<RwLock<Option<LogSequenceNumber>>>,

    /// Maximum LSN of all entries (for flush ordering)
    max_lsn: Arc<RwLock<Option<LogSequenceNumber>>>,
}

impl Memtable {
    /// Create a new empty memtable with the specified size limit.
    pub fn new(max_size: usize) -> Self {
        Self {
            data: Arc::new(RwLock::new(BTreeMap::new())),
            memory_usage: Arc::new(AtomicUsize::new(0)),
            immutable: Arc::new(AtomicBool::new(false)),
            max_size,
            min_lsn: Arc::new(RwLock::new(None)),
            max_lsn: Arc::new(RwLock::new(None)),
        }
    }

    /// Insert or update a key with a new version.
    ///
    /// Creates a new version chain entry or prepends to existing chain.
    /// Returns an error if the memtable is immutable or would exceed size limit.
    pub fn insert(
        &self,
        key: Vec<u8>,
        value: Vec<u8>,
        txn_id: TransactionId,
        commit_lsn: Option<LogSequenceNumber>,
    ) -> TableResult<()> {
        // Check if immutable
        if self.immutable.load(Ordering::Acquire) {
            return Err(TableError::MemtableImmutable);
        }

        let mut data = self.data.write().unwrap();

        // Calculate memory delta
        let key_size = key.len();
        let value_size = value.len();
        let version_overhead = std::mem::size_of::<VersionChain>();

        let mut new_chain = VersionChain::new(value, txn_id);
        if let Some(lsn) = commit_lsn {
            new_chain.commit(lsn);
        }

        let memory_delta = if let Some(existing_chain) = data.get(&key) {
            // Prepend to existing chain
            let old_size = Self::estimate_chain_size(existing_chain);
            // Extract the value from VersionValue for prepending
            let value_to_prepend = match &new_chain.value {
                VersionValue::Inline(data) => data.clone(),
                VersionValue::External(_) => {
                    // For external values, we can't prepend directly
                    // This should be handled at a higher level
                    return Err(TableError::invalid_operation_state(
                        "Memtable::insert",
                        "Cannot prepend external VersionValue in memtable",
                    ));
                }
            };
            new_chain = existing_chain
                .clone()
                .prepend(value_to_prepend, new_chain.created_by);
            if let Some(lsn) = commit_lsn {
                new_chain.commit(lsn);
            }
            let new_size = Self::estimate_chain_size(&new_chain);
            new_size.saturating_sub(old_size)
        } else {
            // New key
            key_size + value_size + version_overhead
        };

        // Check size limit
        let current_usage = self.memory_usage.load(Ordering::Acquire);
        if current_usage + memory_delta > self.max_size {
            return Err(TableError::MemtableFull);
        }

        // Update LSN tracking
        if let Some(lsn) = commit_lsn {
            let mut min_lsn = self.min_lsn.write().unwrap();
            let mut max_lsn = self.max_lsn.write().unwrap();

            *min_lsn = Some(min_lsn.map_or(lsn, |current| current.min(lsn)));
            *max_lsn = Some(max_lsn.map_or(lsn, |current| current.max(lsn)));
        }

        // Insert the new chain
        data.insert(key, new_chain);

        // Update memory usage
        self.memory_usage.fetch_add(memory_delta, Ordering::Release);

        Ok(())
    }

    /// Delete a key by inserting a tombstone version.
    ///
    /// A tombstone is represented by an empty value in the version chain.
    pub fn delete(
        &self,
        key: Vec<u8>,
        txn_id: TransactionId,
        commit_lsn: Option<LogSequenceNumber>,
    ) -> TableResult<()> {
        // Tombstone is an empty value
        self.insert(key, Vec::new(), txn_id, commit_lsn)
    }

    /// Mark all versions created by a transaction as committed.
    ///
    /// This iterates through all entries in the memtable and marks the head version
    /// as committed if it was created by the specified transaction and is not yet committed.
    pub fn commit_versions(
        &self,
        txn_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        let mut data = self.data.write().unwrap();

        for chain in data.values_mut() {
            // Only mark the head version if it was created by this transaction and is uncommitted
            if chain.created_by == txn_id && chain.commit_lsn.is_none() {
                chain.commit(commit_lsn);
            }
        }

        Ok(())
    }

    /// Get the value for a key visible at the given LSN.
    ///
    /// Traverses the version chain to find the first visible version.
    /// Returns None if the key doesn't exist or no visible version is found.
    pub fn get(&self, key: &[u8], snapshot_lsn: LogSequenceNumber) -> TableResult<Option<Vec<u8>>> {
        let data = self.data.read().unwrap();

        if let Some(chain) = data.get(key) {
            // Find the first committed version visible at snapshot_lsn
            let mut current = Some(chain);

            while let Some(version) = current {
                if let Some(commit_lsn) = version.commit_lsn
                    && commit_lsn <= snapshot_lsn
                {
                    // Found visible version
                    // Empty value means tombstone (deleted)
                    if version.value.is_empty() {
                        return Ok(None);
                    }
                    // Extract inline value from VersionValue
                    match &version.value {
                        VersionValue::Inline(data) => {
                            return Ok(Some(data.clone()));
                        }
                        VersionValue::External(_) => {
                            // External values should not be in memtable
                            return Err(TableError::invalid_operation_state(
                                "Memtable::get",
                                "Found external VersionValue in memtable",
                            ));
                        }
                    }
                }

                current = version.prev_version.as_deref();
            }
        }

        Ok(None)
    }

    /// Scan a range of keys visible at the given LSN.
    ///
    /// Returns an iterator over (key, value) pairs in sorted order.
    /// Skips tombstones and invisible versions.
    pub fn scan(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let data = self.data.read().unwrap();

        let mut results = Vec::new();

        // Determine range
        let iter: Box<dyn Iterator<Item = (&Vec<u8>, &VersionChain)>> = match (start, end) {
            (Some(s), Some(e)) => Box::new(data.range(s.to_vec()..e.to_vec())),
            (Some(s), None) => Box::new(data.range(s.to_vec()..)),
            (None, Some(e)) => Box::new(data.range(..e.to_vec())),
            (None, None) => Box::new(data.iter()),
        };

        for (key, chain) in iter {
            // Find visible version
            let mut current = Some(chain);

            while let Some(version) = current {
                if let Some(commit_lsn) = version.commit_lsn
                    && commit_lsn <= snapshot_lsn
                {
                    // Found visible version
                    // Skip tombstones
                    if !version.value.is_empty() {
                        // Extract inline value from VersionValue
                        match &version.value {
                            VersionValue::Inline(data) => {
                                results.push((key.clone(), data.clone()));
                            }
                            VersionValue::External(_) => {
                                // External values should not be in memtable
                                return Err(TableError::invalid_operation_state(
                                    "Memtable::scan",
                                    "Found external VersionValue in memtable",
                                ));
                            }
                        }
                    }
                    break;
                }

                current = version.prev_version.as_deref();
            }
        }

        Ok(results)
    }

    /// Get all entries in sorted order for flushing to SSTable.
    ///
    /// Returns all version chains, not just visible versions.
    /// Only callable on immutable memtables.
    pub fn entries(&self) -> TableResult<Vec<(Vec<u8>, VersionChain)>> {
        if !self.immutable.load(Ordering::Acquire) {
            return Err(TableError::MemtableNotImmutable);
        }

        let data = self.data.read().unwrap();
        Ok(data.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }

    /// Get the current memory usage in bytes.
    pub fn memory_usage(&self) -> usize {
        self.memory_usage.load(Ordering::Acquire)
    }

    /// Check if the memtable is full and should be flushed.
    pub fn is_full(&self) -> bool {
        self.memory_usage() >= self.max_size
    }

    /// Check if the memtable is immutable.
    pub fn is_immutable(&self) -> bool {
        self.immutable.load(Ordering::Acquire)
    }

    /// Get the number of unique keys in the memtable.
    pub fn len(&self) -> usize {
        let data = self.data.read().unwrap();
        data.len()
    }

    /// Check if the memtable is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the minimum LSN of all entries.
    pub fn min_lsn(&self) -> Option<LogSequenceNumber> {
        *self.min_lsn.read().unwrap()
    }

    /// Get the maximum LSN of all entries.
    pub fn max_lsn(&self) -> Option<LogSequenceNumber> {
        *self.max_lsn.read().unwrap()
    }

    /// Convert this memtable to immutable state.
    ///
    /// After this call, no more writes are accepted.
    /// Returns true if the conversion was successful, false if already immutable.
    pub fn make_immutable(&self) -> bool {
        self.immutable
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Vacuum old versions that are no longer visible.
    ///
    /// Removes versions older than min_visible_lsn from all version chains.
    /// Returns the number of versions removed.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        if !self.immutable.load(Ordering::Acquire) {
            return Err(TableError::MemtableNotImmutable);
        }

        let mut data = self.data.write().unwrap();
        let mut total_removed = 0;
        let mut memory_freed = 0;

        for chain in data.values_mut() {
            let old_size = Self::estimate_chain_size(chain);
            // vacuum() now returns (usize, Vec<ValueRef>)
            let (removed, _freed_refs) = chain.vacuum(min_visible_lsn);
            let new_size = Self::estimate_chain_size(chain);

            total_removed += removed;
            memory_freed += old_size.saturating_sub(new_size);
        }

        // Update memory usage
        self.memory_usage.fetch_sub(memory_freed, Ordering::Release);

        Ok(total_removed)
    }

    /// Estimate the memory size of a version chain.
    fn estimate_chain_size(chain: &VersionChain) -> usize {
        let mut size = std::mem::size_of::<VersionChain>();

        // Add size of the value in the chain
        size += match &chain.value {
            VersionValue::Inline(data) => data.len(),
            VersionValue::External(value_ref) => {
                // For external values, just count the ValueRef size
                std::mem::size_of_val(value_ref)
            }
        };

        let mut current = chain.prev_version.as_deref();

        while let Some(version) = current {
            size += std::mem::size_of::<VersionChain>();
            size += match &version.value {
                VersionValue::Inline(data) => data.len(),
                VersionValue::External(value_ref) => std::mem::size_of_val(value_ref),
            };
            current = version.prev_version.as_deref();
        }

        size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_lsn(n: u64) -> LogSequenceNumber {
        LogSequenceNumber::from(n)
    }

    fn create_txn_id(n: u64) -> TransactionId {
        TransactionId::from(n)
    }

    #[test]
    fn test_memtable_new() {
        let memtable = Memtable::new(1024 * 1024);
        assert_eq!(memtable.memory_usage(), 0);
        assert!(!memtable.is_immutable());
        assert!(memtable.is_empty());
    }

    #[test]
    fn test_memtable_insert_and_get() {
        let memtable = Memtable::new(1024 * 1024);

        // Insert a key
        memtable
            .insert(
                b"key1".to_vec(),
                b"value1".to_vec(),
                create_txn_id(1),
                Some(create_lsn(100)),
            )
            .unwrap();

        // Get the key
        let value = memtable.get(b"key1", create_lsn(100)).unwrap();
        assert_eq!(value, Some(b"value1".to_vec()));

        // Get with earlier LSN should return None
        let value = memtable.get(b"key1", create_lsn(99)).unwrap();
        assert_eq!(value, None);

        // Get non-existent key
        let value = memtable.get(b"key2", create_lsn(100)).unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn test_memtable_update() {
        let memtable = Memtable::new(1024 * 1024);

        // Insert initial version
        memtable
            .insert(
                b"key1".to_vec(),
                b"value1".to_vec(),
                create_txn_id(1),
                Some(create_lsn(100)),
            )
            .unwrap();

        // Update with new version
        memtable
            .insert(
                b"key1".to_vec(),
                b"value2".to_vec(),
                create_txn_id(2),
                Some(create_lsn(200)),
            )
            .unwrap();

        // Get at LSN 100 should return old version
        let value = memtable.get(b"key1", create_lsn(150)).unwrap();
        assert_eq!(value, Some(b"value1".to_vec()));

        // Get at LSN 200 should return new version
        let value = memtable.get(b"key1", create_lsn(200)).unwrap();
        assert_eq!(value, Some(b"value2".to_vec()));
    }

    #[test]
    fn test_memtable_delete() {
        let memtable = Memtable::new(1024 * 1024);

        // Insert a key
        memtable
            .insert(
                b"key1".to_vec(),
                b"value1".to_vec(),
                create_txn_id(1),
                Some(create_lsn(100)),
            )
            .unwrap();

        // Delete the key
        memtable
            .delete(b"key1".to_vec(), create_txn_id(2), Some(create_lsn(200)))
            .unwrap();

        // Get at LSN 150 should return old version
        let value = memtable.get(b"key1", create_lsn(150)).unwrap();
        assert_eq!(value, Some(b"value1".to_vec()));

        // Get at LSN 200 should return None (deleted)
        let value = memtable.get(b"key1", create_lsn(200)).unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn test_memtable_scan() {
        let memtable = Memtable::new(1024 * 1024);

        // Insert multiple keys
        for i in 0..5 {
            let key = format!("key{}", i).into_bytes();
            let value = format!("value{}", i).into_bytes();
            memtable
                .insert(key, value, create_txn_id(1), Some(create_lsn(100)))
                .unwrap();
        }

        // Scan all
        let results = memtable.scan(None, None, create_lsn(100)).unwrap();
        assert_eq!(results.len(), 5);

        // Scan range
        let results = memtable
            .scan(Some(b"key1"), Some(b"key3"), create_lsn(100))
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, b"key1");
        assert_eq!(results[1].0, b"key2");
    }

    #[test]
    fn test_memtable_scan_with_tombstones() {
        let memtable = Memtable::new(1024 * 1024);

        // Insert keys
        memtable
            .insert(
                b"key1".to_vec(),
                b"value1".to_vec(),
                create_txn_id(1),
                Some(create_lsn(100)),
            )
            .unwrap();
        memtable
            .insert(
                b"key2".to_vec(),
                b"value2".to_vec(),
                create_txn_id(1),
                Some(create_lsn(100)),
            )
            .unwrap();
        memtable
            .insert(
                b"key3".to_vec(),
                b"value3".to_vec(),
                create_txn_id(1),
                Some(create_lsn(100)),
            )
            .unwrap();

        // Delete key2
        memtable
            .delete(b"key2".to_vec(), create_txn_id(2), Some(create_lsn(200)))
            .unwrap();

        // Scan at LSN 200 should skip key2
        let results = memtable.scan(None, None, create_lsn(200)).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, b"key1");
        assert_eq!(results[1].0, b"key3");
    }

    #[test]
    fn test_memtable_memory_tracking() {
        let memtable = Memtable::new(1024 * 1024);

        let initial_usage = memtable.memory_usage();
        assert_eq!(initial_usage, 0);

        // Insert a key
        memtable
            .insert(
                b"key1".to_vec(),
                b"value1".to_vec(),
                create_txn_id(1),
                Some(create_lsn(100)),
            )
            .unwrap();

        // Memory usage should increase
        let after_insert = memtable.memory_usage();
        assert!(after_insert > initial_usage);

        // Update the key
        memtable
            .insert(
                b"key1".to_vec(),
                b"value2".to_vec(),
                create_txn_id(2),
                Some(create_lsn(200)),
            )
            .unwrap();

        // Memory usage should increase further
        let after_update = memtable.memory_usage();
        assert!(after_update > after_insert);
    }

    #[test]
    fn test_memtable_size_limit() {
        let memtable = Memtable::new(100); // Very small limit

        // Insert should fail when exceeding limit
        let result = memtable.insert(
            vec![0u8; 50],
            vec![0u8; 100],
            create_txn_id(1),
            Some(create_lsn(100)),
        );

        assert!(matches!(result, Err(TableError::MemtableFull)));
    }

    #[test]
    fn test_memtable_immutable() {
        let memtable = Memtable::new(1024 * 1024);

        // Insert a key
        memtable
            .insert(
                b"key1".to_vec(),
                b"value1".to_vec(),
                create_txn_id(1),
                Some(create_lsn(100)),
            )
            .unwrap();

        // Make immutable
        assert!(memtable.make_immutable());
        assert!(memtable.is_immutable());

        // Second call should return false
        assert!(!memtable.make_immutable());

        // Insert should fail
        let result = memtable.insert(
            b"key2".to_vec(),
            b"value2".to_vec(),
            create_txn_id(2),
            Some(create_lsn(200)),
        );
        assert!(matches!(result, Err(TableError::MemtableImmutable)));

        // Get should still work
        let value = memtable.get(b"key1", create_lsn(100)).unwrap();
        assert_eq!(value, Some(b"value1".to_vec()));
    }

    #[test]
    fn test_memtable_entries() {
        let memtable = Memtable::new(1024 * 1024);

        // Insert keys
        memtable
            .insert(
                b"key1".to_vec(),
                b"value1".to_vec(),
                create_txn_id(1),
                Some(create_lsn(100)),
            )
            .unwrap();
        memtable
            .insert(
                b"key2".to_vec(),
                b"value2".to_vec(),
                create_txn_id(1),
                Some(create_lsn(100)),
            )
            .unwrap();

        // entries() should fail on mutable memtable
        assert!(matches!(
            memtable.entries(),
            Err(TableError::MemtableNotImmutable)
        ));

        // Make immutable
        memtable.make_immutable();

        // entries() should now work
        let entries = memtable.entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, b"key1");
        assert_eq!(entries[1].0, b"key2");
    }

    #[test]
    fn test_memtable_lsn_tracking() {
        let memtable = Memtable::new(1024 * 1024);

        assert_eq!(memtable.min_lsn(), None);
        assert_eq!(memtable.max_lsn(), None);

        // Insert with LSN 100
        memtable
            .insert(
                b"key1".to_vec(),
                b"value1".to_vec(),
                create_txn_id(1),
                Some(create_lsn(100)),
            )
            .unwrap();

        assert_eq!(memtable.min_lsn(), Some(create_lsn(100)));
        assert_eq!(memtable.max_lsn(), Some(create_lsn(100)));

        // Insert with LSN 50
        memtable
            .insert(
                b"key2".to_vec(),
                b"value2".to_vec(),
                create_txn_id(2),
                Some(create_lsn(50)),
            )
            .unwrap();

        assert_eq!(memtable.min_lsn(), Some(create_lsn(50)));
        assert_eq!(memtable.max_lsn(), Some(create_lsn(100)));

        // Insert with LSN 200
        memtable
            .insert(
                b"key3".to_vec(),
                b"value3".to_vec(),
                create_txn_id(3),
                Some(create_lsn(200)),
            )
            .unwrap();

        assert_eq!(memtable.min_lsn(), Some(create_lsn(50)));
        assert_eq!(memtable.max_lsn(), Some(create_lsn(200)));
    }

    #[test]
    fn test_memtable_vacuum() {
        let memtable = Memtable::new(1024 * 1024);

        // Insert multiple versions
        memtable
            .insert(
                b"key1".to_vec(),
                b"value1".to_vec(),
                create_txn_id(1),
                Some(create_lsn(100)),
            )
            .unwrap();
        memtable
            .insert(
                b"key1".to_vec(),
                b"value2".to_vec(),
                create_txn_id(2),
                Some(create_lsn(200)),
            )
            .unwrap();
        memtable
            .insert(
                b"key1".to_vec(),
                b"value3".to_vec(),
                create_txn_id(3),
                Some(create_lsn(300)),
            )
            .unwrap();

        let usage_before = memtable.memory_usage();

        // Vacuum should fail on mutable memtable
        assert!(matches!(
            memtable.vacuum(create_lsn(250)),
            Err(TableError::MemtableNotImmutable)
        ));

        // Make immutable
        memtable.make_immutable();

        // Vacuum versions older than LSN 250
        let removed = memtable.vacuum(create_lsn(250)).unwrap();
        assert!(removed > 0);

        // Memory usage should decrease
        let usage_after = memtable.memory_usage();
        assert!(usage_after < usage_before);

        // Should still be able to read newest version
        let value = memtable.get(b"key1", create_lsn(300)).unwrap();
        assert_eq!(value, Some(b"value3".to_vec()));
    }
}

// Made with Bob
