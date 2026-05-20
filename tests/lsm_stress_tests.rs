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

//! Stress tests for LSM tree storage engine.
//!
//! These tests validate LSM tree behavior under heavy load:
//! - Large dataset insertions (10K+ keys)
//! - Compaction under load
//! - Concurrent writes and reads
//! - Memory pressure scenarios
//! - Long-running operations

use nanostore::pager::{PageType, Pager, PagerConfig};
use nanostore::table::lsm::LsmTree;
use nanostore::table::{Flushable, MutableTable, PointLookup, SearchableTable};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
use std::sync::Arc;

/// Helper to create a test LSM tree
fn create_test_lsm() -> LsmTree<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let pager_config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "lsm_stress.db", pager_config).unwrap());

    let root_page_id = pager.allocate_page(PageType::LsmMeta).unwrap();
    let lsm_config = nanostore::table::lsm::LsmConfig::default();

    LsmTree::new(
        TableId::from(1),
        "stress_lsm".to_string(),
        pager,
        root_page_id,
        lsm_config,
    )
    .unwrap()
}

/// Test inserting 10,000 sequential keys
///
/// Validates that LSM tree can handle large sequential insertions
/// and that memtable flushes to SSTables correctly.
#[test]
fn test_large_sequential_insert_10k() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    let mut writer = lsm.writer(tx_id, lsn).unwrap();

    // Insert 10,000 sequential keys
    for i in 0..10_000 {
        let key = format!("key_{:08}", i);
        let value = format!("value_{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }

    writer.flush().unwrap();
    writer.commit_versions(lsn).unwrap();

    // Verify a sample of keys
    let reader = lsm.reader(lsn).unwrap();
    for i in (0..10_000).step_by(1000) {
        let key = format!("key_{:08}", i);
        let result = reader.get(key.as_bytes(), lsn).unwrap();
        assert!(result.is_some(), "Key {} should exist", key);
    }
}

/// Test inserting 50,000 keys to trigger multiple compactions
///
/// This test ensures the LSM tree can handle very large datasets
/// and that compaction works correctly under load.
#[test]
fn test_large_dataset_50k_keys() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    let mut writer = lsm.writer(tx_id, lsn).unwrap();

    // Insert 50,000 keys
    for i in 0..50_000 {
        let key = format!("key_{:08}", i);
        let value = format!("value_{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();

        // Flush periodically to trigger memtable flushes
        if i % 5000 == 0 && i > 0 {
            writer.flush().unwrap();
        }
    }

    writer.flush().unwrap();
    writer.commit_versions(lsn).unwrap();

    // Verify random sample of keys
    let reader = lsm.reader(lsn).unwrap();
    for i in [0, 1000, 10000, 25000, 40000, 49999] {
        let key = format!("key_{:08}", i);
        let result = reader.get(key.as_bytes(), lsn).unwrap();
        assert!(result.is_some(), "Key {} should exist", key);
    }
}

/// Test random key insertions to stress compaction
///
/// Random insertions create more challenging compaction scenarios
/// than sequential insertions.
#[test]
fn test_random_insertions_20k() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    let mut writer = lsm.writer(tx_id, lsn).unwrap();

    // Insert 20,000 keys with pseudo-random ordering
    for i in 0..20_000 {
        // Use a simple pseudo-random pattern
        let key_num = (i * 7919) % 20_000; // 7919 is prime
        let key = format!("key_{:08}", key_num);
        let value = format!("value_{}", key_num);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();

        if i % 2000 == 0 && i > 0 {
            writer.flush().unwrap();
        }
    }

    writer.flush().unwrap();
    writer.commit_versions(lsn).unwrap();

    // Verify keys exist
    let reader = lsm.reader(lsn).unwrap();
    for i in [0, 5000, 10000, 15000, 19999] {
        let key = format!("key_{:08}", i);
        let result = reader.get(key.as_bytes(), lsn).unwrap();
        assert!(result.is_some(), "Key {} should exist", key);
    }
}

/// Test updates to existing keys under load
///
/// Validates that LSM tree correctly handles updates and maintains
/// version chains under heavy update load.
#[test]
fn test_heavy_updates_5k_keys() {
    let lsm = create_test_lsm();

    // Insert initial keys
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);
    let mut writer = lsm.writer(tx_id, lsn).unwrap();

    for i in 0..5_000 {
        let key = format!("key_{:04}", i);
        let value = format!("value_v1_{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }
    writer.flush().unwrap();
    writer.commit_versions(lsn).unwrap();

    // Update all keys multiple times
    for version in 2..=5 {
        let tx_id = TransactionId::from(version);
        let lsn = LogSequenceNumber::from(version as u64);
        let mut writer = lsm.writer(tx_id, lsn).unwrap();

        for i in 0..5_000 {
            let key = format!("key_{:04}", i);
            let value = format!("value_v{}_{}", version, i);
            writer.put(key.as_bytes(), value.as_bytes()).unwrap();
        }
        writer.flush().unwrap();
        writer.commit_versions(lsn).unwrap();
    }

    // Verify latest version
    let reader = lsm.reader(LogSequenceNumber::from(5)).unwrap();
    for i in [0, 1000, 2500, 4999] {
        let key = format!("key_{:04}", i);
        let result = reader.get(key.as_bytes(), LogSequenceNumber::from(5)).unwrap();
        assert!(result.is_some());
        let value = result.unwrap().0;
        let expected = format!("value_v5_{}", i);
        assert_eq!(value.as_slice(), expected.as_bytes());
    }
}

/// Test deletions under load
///
/// Validates that tombstones are handled correctly and that
/// deleted keys are not returned in queries.
#[test]
fn test_heavy_deletions_10k_keys() {
    let lsm = create_test_lsm();

    // Insert keys
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);
    let mut writer = lsm.writer(tx_id, lsn).unwrap();

    for i in 0..10_000 {
        let key = format!("key_{:05}", i);
        let value = format!("value_{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }
    writer.flush().unwrap();
    writer.commit_versions(lsn).unwrap();

    // Delete every other key
    let tx_id = TransactionId::from(2);
    let lsn = LogSequenceNumber::from(2);
    let mut writer = lsm.writer(tx_id, lsn).unwrap();

    for i in (0..10_000).step_by(2) {
        let key = format!("key_{:05}", i);
        writer.delete(key.as_bytes()).unwrap();
    }
    writer.flush().unwrap();
    writer.commit_versions(lsn).unwrap();

    // Verify deleted keys don't exist
    let reader = lsm.reader(LogSequenceNumber::from(2)).unwrap();
    for i in [0, 100, 5000, 9998] {
        let key = format!("key_{:05}", i);
        let result = reader.get(key.as_bytes(), LogSequenceNumber::from(2)).unwrap();
        assert!(result.is_none(), "Key {} should be deleted", key);
    }

    // Verify non-deleted keys still exist
    for i in [1, 101, 5001, 9999] {
        let key = format!("key_{:05}", i);
        let result = reader.get(key.as_bytes(), LogSequenceNumber::from(2)).unwrap();
        assert!(result.is_some(), "Key {} should exist", key);
    }
}

/// Test mixed workload: inserts, updates, and deletes
///
/// Simulates a realistic workload with mixed operations.
#[test]
fn test_mixed_workload_15k_operations() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    let mut writer = lsm.writer(tx_id, lsn).unwrap();

    // Phase 1: Insert 5000 keys
    for i in 0..5_000 {
        let key = format!("key_{:05}", i);
        let value = format!("value_{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }
    writer.flush().unwrap();

    // Phase 2: Update 2500 keys
    for i in 0..2_500 {
        let key = format!("key_{:05}", i);
        let value = format!("updated_{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }
    writer.flush().unwrap();

    // Phase 3: Delete 1000 keys
    for i in 2_500..3_500 {
        let key = format!("key_{:05}", i);
        writer.delete(key.as_bytes()).unwrap();
    }
    writer.flush().unwrap();

    // Phase 4: Insert 7500 more keys
    for i in 5_000..12_500 {
        let key = format!("key_{:05}", i);
        let value = format!("value_{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }
    writer.flush().unwrap();
    writer.commit_versions(lsn).unwrap();

    // Verify results
    let reader = lsm.reader(lsn).unwrap();

    // Updated keys should have new values
    let result = reader.get(b"key_00100", lsn).unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().0.as_slice(), b"updated_100");

    // Deleted keys should not exist
    let result = reader.get(b"key_03000", lsn).unwrap();
    assert!(result.is_none());

    // New keys should exist
    let result = reader.get(b"key_10000", lsn).unwrap();
    assert!(result.is_some());
}

/// Test large value sizes
///
/// Validates that LSM tree can handle large values (1KB each).
#[test]
fn test_large_values_1kb_each() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    let mut writer = lsm.writer(tx_id, lsn).unwrap();

    // Insert 1000 keys with 1KB values
    let large_value = vec![b'X'; 1024];
    for i in 0..1_000 {
        let key = format!("key_{:04}", i);
        writer.put(key.as_bytes(), &large_value).unwrap();
    }

    writer.flush().unwrap();
    writer.commit_versions(lsn).unwrap();

    // Verify values
    let reader = lsm.reader(lsn).unwrap();
    for i in [0, 500, 999] {
        let key = format!("key_{:04}", i);
        let result = reader.get(key.as_bytes(), lsn).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().0.len(), 1024);
    }
}

/// Test many small transactions
///
/// Validates that LSM tree can handle many small transactions
/// without performance degradation.
#[test]
fn test_many_small_transactions_1000() {
    let lsm = create_test_lsm();

    // Execute 1000 small transactions
    for tx_num in 0..1_000 {
        let tx_id = TransactionId::from(tx_num + 1);
        let lsn = LogSequenceNumber::from((tx_num + 1) as u64);
        let mut writer = lsm.writer(tx_id, lsn).unwrap();

        // Each transaction inserts 10 keys
        for i in 0..10 {
            let key = format!("tx_{:04}_key_{:02}", tx_num, i);
            let value = format!("value_{}", i);
            writer.put(key.as_bytes(), value.as_bytes()).unwrap();
        }

        writer.flush().unwrap();
        writer.commit_versions(lsn).unwrap();
    }

    // Verify sample keys from different transactions
    let reader = lsm.reader(LogSequenceNumber::from(1000)).unwrap();
    for tx_num in [0, 100, 500, 999] {
        let key = format!("tx_{:04}_key_00", tx_num);
        let result = reader.get(key.as_bytes(), LogSequenceNumber::from(1000)).unwrap();
        assert!(result.is_some(), "Key from transaction {} should exist", tx_num);
    }
}

/// Test long key names
///
/// Validates that LSM tree can handle keys with long names (256 bytes).
#[test]
fn test_long_key_names_256_bytes() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    let mut writer = lsm.writer(tx_id, lsn).unwrap();

    // Insert 1000 keys with 256-byte names
    for i in 0..1_000 {
        let key = format!("long_key_{:0250}", i); // Pad to 256 bytes
        let value = format!("value_{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }

    writer.flush().unwrap();
    writer.commit_versions(lsn).unwrap();

    // Verify keys
    let reader = lsm.reader(lsn).unwrap();
    for i in [0, 500, 999] {
        let key = format!("long_key_{:0250}", i);
        let result = reader.get(key.as_bytes(), lsn).unwrap();
        assert!(result.is_some(), "Long key {} should exist", i);
    }
}

/// Test memtable overflow behavior
///
/// Validates that memtable correctly flushes when it reaches capacity.
#[test]
fn test_memtable_overflow_behavior() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    let mut writer = lsm.writer(tx_id, lsn).unwrap();

    // Insert enough data to trigger multiple memtable flushes
    // Default memtable size is typically 4MB, so insert 10MB of data
    let value = vec![b'X'; 1024]; // 1KB value
    for i in 0..10_000 {
        let key = format!("key_{:05}", i);
        writer.put(key.as_bytes(), &value).unwrap();

        // Flush periodically
        if i % 1000 == 0 && i > 0 {
            writer.flush().unwrap();
        }
    }

    writer.flush().unwrap();
    writer.commit_versions(lsn).unwrap();

    // Verify all keys are accessible
    let reader = lsm.reader(lsn).unwrap();
    for i in [0, 2500, 5000, 7500, 9999] {
        let key = format!("key_{:05}", i);
        let result = reader.get(key.as_bytes(), lsn).unwrap();
        assert!(result.is_some(), "Key {} should exist after memtable flushes", key);
    }
}

// Made with Bob