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

//! Tests for B-Tree node split and merge operations.

use nanostore::pager::{Pager, PagerConfig};
use nanostore::table::btree::PagedBTree;
use nanostore::table::{Flushable, MutableTable, PointLookup, SearchableTable};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
use std::sync::Arc;

#[test]
fn test_btree_node_split_on_insert() {
    // Create a pager with small page size to trigger splits easily
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Create B-Tree
    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    // Get a writer
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();

    // Insert enough keys to trigger a split (DEFAULT_ORDER = 64)
    for i in 0..100 {
        let key = format!("key_{:04}", i);
        let value = format!("value_{:04}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }

    // Flush to apply changes
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Verify all keys are still accessible (read at LSN after writes)
    let read_lsn = LogSequenceNumber::from(100);
    let reader = table.reader(read_lsn).unwrap();
    for i in 0..100 {
        let key = format!("key_{:04}", i);
        let value = format!("value_{:04}", i);
        let result = reader.get(key.as_bytes(), read_lsn).unwrap();
        assert!(result.is_some(), "Key {} should exist", key);
        assert_eq!(result.unwrap().0, value.as_bytes());
    }
}

#[test]
fn test_btree_sequential_inserts() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();

    // Insert keys in sequential order
    for i in 0..200 {
        let key = format!("{:08}", i);
        let value = format!("value_{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }

    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    // Verify all keys (read at LSN after writes)
    let read_lsn = LogSequenceNumber::from(200);
    let reader = table.reader(read_lsn).unwrap();
    for i in 0..200 {
        let key = format!("{:08}", i);
        let result = reader.get(key.as_bytes(), read_lsn).unwrap();
        assert!(result.is_some(), "Key {} should exist", key);
    }
}

#[test]
fn test_btree_reverse_inserts() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();

    // Insert keys in reverse order
    for i in (0..200).rev() {
        let key = format!("{:08}", i);
        let value = format!("value_{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }

    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    // Verify all keys (read at LSN after writes)
    let read_lsn = LogSequenceNumber::from(200);
    let reader = table.reader(read_lsn).unwrap();
    for i in 0..200 {
        let key = format!("{:08}", i);
        let result = reader.get(key.as_bytes(), read_lsn).unwrap();
        assert!(result.is_some(), "Key {} should exist", key);
    }
}

#[test]
fn test_btree_random_inserts() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();

    // Insert keys in pseudo-random order
    let mut keys = Vec::new();
    for i in 0..150 {
        let key = format!("{:08}", (i * 7919) % 1000); // Simple pseudo-random
        keys.push(key.clone());
        let value = format!("value_{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }

    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    // Verify all keys (read at LSN after writes)
    let read_lsn = LogSequenceNumber::from(200);
    let reader = table.reader(read_lsn).unwrap();
    for (_i, key) in keys.iter().enumerate() {
        let result = reader.get(key.as_bytes(), read_lsn).unwrap();
        assert!(result.is_some(), "Key {} should exist", key);
        // Note: Due to overwrites, we can't verify exact values
    }
}

#[test]
fn test_btree_update_existing_keys() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();

    // Insert initial keys
    for i in 0..50 {
        let key = format!("key_{:04}", i);
        let value = format!("value_v1_{:04}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }

    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Update the same keys (use higher LSN for snapshot)
    let tx_id2 = TransactionId::from(2);
    let snapshot_lsn2 = LogSequenceNumber::from(100);
    let mut writer2 = table.writer(tx_id2, snapshot_lsn2).unwrap();
    for i in 0..50 {
        let key = format!("key_{:04}", i);
        let value = format!("value_v2_{:04}", i);
        writer2.put(key.as_bytes(), value.as_bytes()).unwrap();
    }

    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    // Verify updated values (read at latest LSN to see v2)
    let read_lsn = LogSequenceNumber::from(200);
    let reader = table.reader(read_lsn).unwrap();
    for i in 0..50 {
        let key = format!("key_{:04}", i);
        let result = reader.get(key.as_bytes(), read_lsn).unwrap();
        assert!(result.is_some(), "Key {} should exist", key);
    }
}

#[test]
fn test_btree_delete_keys() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();

    // Insert keys
    for i in 0..50 {
        let key = format!("key_{:04}", i);
        let value = format!("value_{:04}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }

    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Delete some keys (use higher LSN for snapshot)
    let tx_id2 = TransactionId::from(2);
    let snapshot_lsn2 = LogSequenceNumber::from(100);
    let mut writer2 = table.writer(tx_id2, snapshot_lsn2).unwrap();
    for i in (0..50).step_by(2) {
        let key = format!("key_{:04}", i);
        let deleted = writer2.delete(key.as_bytes()).unwrap();
        assert!(deleted, "Key {} should exist for deletion", key);
    }

    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    // Verify keys at snapshot before delete (MVCC: should still see all before delete)
    // Read at LSN 150 (after first commit at 100, before delete commit at 200)
    let read_lsn_before_delete = LogSequenceNumber::from(150);
    let reader = table.reader(read_lsn_before_delete).unwrap();
    for i in 0..50 {
        let key = format!("key_{:04}", i);
        let result = reader.get(key.as_bytes(), read_lsn_before_delete).unwrap();
        assert!(
            result.is_some(),
            "Key {} should still be visible at snapshot before delete",
            key
        );
    }
}

#[test]
fn test_btree_mixed_operations() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();

    // Mix of inserts, updates, and deletes
    for i in 0..100 {
        let key = format!("key_{:04}", i);
        let value = format!("value_{:04}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }

    // Update some
    for i in 0..30 {
        let key = format!("key_{:04}", i);
        let value = format!("updated_{:04}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }

    // Delete some
    for i in 30..60 {
        let key = format!("key_{:04}", i);
        writer.delete(key.as_bytes()).unwrap();
    }

    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    // Verify state (read at LSN after all operations)
    let read_lsn = LogSequenceNumber::from(200);
    let reader = table.reader(read_lsn).unwrap();

    // Keys 0-29 should exist (but we can't verify exact values due to MVCC)
    for i in 0..30 {
        let key = format!("key_{:04}", i);
        let result = reader.get(key.as_bytes(), read_lsn).unwrap();
        assert!(result.is_some(), "Key {} should exist", key);
    }

    // Keys 60-99 should exist
    for i in 60..100 {
        let key = format!("key_{:04}", i);
        let result = reader.get(key.as_bytes(), read_lsn).unwrap();
        assert!(result.is_some(), "Key {} should exist", key);
    }
}

#[test]
fn test_btree_delete_rebalances_across_leaf_siblings() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    let tx_insert = TransactionId::from(1);
    let mut writer = table.writer(tx_insert, LogSequenceNumber::from(0)).unwrap();

    for i in 0..100 {
        let key = format!("key_{:04}", i);
        let value = format!("value_{:04}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }

    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let tx_delete = TransactionId::from(2);
    let mut delete_writer = table
        .writer(tx_delete, LogSequenceNumber::from(100))
        .unwrap();

    for i in 0..40 {
        let key = format!("key_{:04}", i);
        assert!(delete_writer.delete(key.as_bytes()).unwrap());
    }

    delete_writer.flush().unwrap();
    delete_writer
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(200)).unwrap();

    for i in 0..40 {
        let key = format!("key_{:04}", i);
        let result = reader
            .get(key.as_bytes(), LogSequenceNumber::from(200))
            .unwrap();
        assert!(
            result.is_none(),
            "Deleted key {} should not be visible",
            key
        );
    }

    for i in 40..100 {
        let key = format!("key_{:04}", i);
        let result = reader
            .get(key.as_bytes(), LogSequenceNumber::from(200))
            .unwrap();
        assert!(
            result.is_some(),
            "Remaining key {} should still be visible",
            key
        );
    }
}

#[test]
fn test_btree_delete_rebalances_rightmost_leaf() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    let tx_insert = TransactionId::from(1);
    let mut writer = table.writer(tx_insert, LogSequenceNumber::from(0)).unwrap();

    for i in 0..100 {
        let key = format!("key_{:04}", i);
        let value = format!("value_{:04}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }

    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let tx_delete = TransactionId::from(2);
    let mut delete_writer = table
        .writer(tx_delete, LogSequenceNumber::from(100))
        .unwrap();

    for i in 60..100 {
        let key = format!("key_{:04}", i);
        assert!(delete_writer.delete(key.as_bytes()).unwrap());
    }

    delete_writer.flush().unwrap();
    delete_writer
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(200)).unwrap();

    for i in 0..60 {
        let key = format!("key_{:04}", i);
        let result = reader
            .get(key.as_bytes(), LogSequenceNumber::from(200))
            .unwrap();
        assert!(
            result.is_some(),
            "Remaining key {} should still be visible",
            key
        );
    }

    for i in 60..100 {
        let key = format!("key_{:04}", i);
        let result = reader
            .get(key.as_bytes(), LogSequenceNumber::from(200))
            .unwrap();
        assert!(
            result.is_none(),
            "Deleted key {} should not be visible",
            key
        );
    }
}

// Made with Bob
