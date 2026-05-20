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

//! Tests for LSM tree DenseOrdered trait implementation.
//!
//! These tests verify that the LSM tree correctly implements the DenseOrdered
//! specialty table trait for use as a secondary index.

use nanostore::pager::{Pager, PagerConfig};
use nanostore::table::lsm::{LsmConfig, LsmTree};
use nanostore::table::{DenseOrdered, SpecialtyTableCursor};
use nanostore::txn::TransactionId;
use nanostore::types::{Bound, KeyBuf, ScanBounds, TableId};
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;

fn create_test_lsm() -> LsmTree<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let pager = Pager::create(&fs, "test.db".into(), PagerConfig::default()).unwrap();
    let config = LsmConfig::default();

    LsmTree::new(
        TableId::from(1),
        "test_lsm_index".to_string(),
        std::sync::Arc::new(pager),
        nanostore::pager::PageId::from(1),
        config,
    )
    .unwrap()
}

#[test]
fn test_lsm_dense_ordered_basic_operations() {
    let mut lsm = create_test_lsm();

    // Test table_id and name
    assert_eq!(lsm.table_id(), TableId::from(1));
    assert_eq!(lsm.name(), "test_lsm_index");

    // Test capabilities
    let caps = lsm.capabilities();
    assert!(caps.exact);
    assert!(!caps.approximate);
    assert!(caps.ordered);
    assert!(!caps.sparse);
    assert!(caps.supports_delete);
    assert!(caps.supports_range_query);
    assert!(caps.supports_prefix_query);
}

#[test]
fn test_lsm_dense_ordered_insert_and_scan() {
    let mut lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Insert some index entries: index_key -> primary_key
    lsm.insert_entry(b"age:25", b"user:1", tx_id, lsn).unwrap();
    lsm.insert_entry(b"age:30", b"user:2", tx_id, lsn).unwrap();
    lsm.insert_entry(b"age:25", b"user:3", tx_id, lsn).unwrap();
    lsm.insert_entry(b"age:35", b"user:4", tx_id, lsn).unwrap();

    // Scan all entries
    let mut cursor = lsm.scan(ScanBounds::All).unwrap();

    let mut entries = Vec::new();
    while cursor.valid() {
        let index_key = cursor.index_key().unwrap().to_vec();
        let primary_key = cursor.primary_key().unwrap().to_vec();
        entries.push((index_key, primary_key));
        cursor.next().unwrap();
    }

    // Should be in sorted order by index_key
    // We inserted 4 entries but age:25 was inserted twice, so we expect 3 unique entries
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], (b"age:25".to_vec(), b"user:3".to_vec())); // Latest version
    assert_eq!(entries[1], (b"age:30".to_vec(), b"user:2".to_vec()));
    assert_eq!(entries[2], (b"age:35".to_vec(), b"user:4".to_vec()));
}

#[test]
fn test_lsm_dense_ordered_range_scan() {
    let mut lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Insert entries
    lsm.insert_entry(b"age:20", b"user:1", tx_id, lsn).unwrap();
    lsm.insert_entry(b"age:25", b"user:2", tx_id, lsn).unwrap();
    lsm.insert_entry(b"age:30", b"user:3", tx_id, lsn).unwrap();
    lsm.insert_entry(b"age:35", b"user:4", tx_id, lsn).unwrap();
    lsm.insert_entry(b"age:40", b"user:5", tx_id, lsn).unwrap();

    // Range scan: age >= 25 and age < 35
    let bounds = ScanBounds::Range {
        start: Bound::Included(KeyBuf(b"age:25".to_vec())),
        end: Bound::Excluded(KeyBuf(b"age:35".to_vec())),
    };

    let mut cursor = lsm.scan(bounds).unwrap();

    let mut entries = Vec::new();
    while cursor.valid() {
        let index_key = cursor.index_key().unwrap().to_vec();
        let primary_key = cursor.primary_key().unwrap().to_vec();
        entries.push((index_key, primary_key));
        cursor.next().unwrap();
    }

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], (b"age:25".to_vec(), b"user:2".to_vec()));
    assert_eq!(entries[1], (b"age:30".to_vec(), b"user:3".to_vec()));
}

#[test]
fn test_lsm_dense_ordered_prefix_scan() {
    let mut lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Insert entries with different prefixes
    lsm.insert_entry(b"age:25", b"user:1", tx_id, lsn).unwrap();
    lsm.insert_entry(b"age:30", b"user:2", tx_id, lsn).unwrap();
    lsm.insert_entry(b"name:alice", b"user:3", tx_id, lsn)
        .unwrap();
    lsm.insert_entry(b"name:bob", b"user:4", tx_id, lsn)
        .unwrap();

    // Prefix scan for "age:"
    let bounds = ScanBounds::Prefix(KeyBuf(b"age:".to_vec()));
    let mut cursor = lsm.scan(bounds).unwrap();

    let mut entries = Vec::new();
    while cursor.valid() {
        let index_key = cursor.index_key().unwrap().to_vec();
        let primary_key = cursor.primary_key().unwrap().to_vec();
        entries.push((index_key, primary_key));
        cursor.next().unwrap();
    }

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], (b"age:25".to_vec(), b"user:1".to_vec()));
    assert_eq!(entries[1], (b"age:30".to_vec(), b"user:2".to_vec()));
}

#[test]
fn test_lsm_dense_ordered_delete() {
    let mut lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Insert entries
    lsm.insert_entry(b"age:25", b"user:1", tx_id, lsn).unwrap();
    lsm.insert_entry(b"age:30", b"user:2", tx_id, lsn).unwrap();
    lsm.insert_entry(b"age:35", b"user:3", tx_id, lsn).unwrap();

    // Delete one entry
    let tx_id2 = TransactionId::from(2);
    let lsn2 = LogSequenceNumber::from(2);
    lsm.delete_entry(b"age:30", b"user:2", tx_id2, lsn2)
        .unwrap();

    // Scan all entries
    let mut cursor = lsm.scan(ScanBounds::All).unwrap();

    let mut entries = Vec::new();
    while cursor.valid() {
        let index_key = cursor.index_key().unwrap().to_vec();
        let primary_key = cursor.primary_key().unwrap().to_vec();
        entries.push((index_key, primary_key));
        cursor.next().unwrap();
    }

    // Should only have 2 entries now
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], (b"age:25".to_vec(), b"user:1".to_vec()));
    assert_eq!(entries[1], (b"age:35".to_vec(), b"user:3".to_vec()));
}

#[test]
fn test_lsm_dense_ordered_mvcc() {
    let mut lsm = create_test_lsm();

    // Insert entry at LSN 1
    let tx_id1 = TransactionId::from(1);
    let lsn1 = LogSequenceNumber::from(1);
    lsm.insert_entry(b"age:25", b"user:1", tx_id1, lsn1)
        .unwrap();

    // Update entry at LSN 2
    let tx_id2 = TransactionId::from(2);
    let lsn2 = LogSequenceNumber::from(2);
    lsm.insert_entry(b"age:25", b"user:2", tx_id2, lsn2)
        .unwrap();

    // Scan should see the latest version (user:2)
    let mut cursor = lsm.scan(ScanBounds::All).unwrap();

    assert!(cursor.valid());
    assert_eq!(cursor.index_key().unwrap(), b"age:25");
    assert_eq!(cursor.primary_key().unwrap(), b"user:2");

    cursor.next().unwrap();
    assert!(!cursor.valid());
}

#[test]
fn test_lsm_dense_ordered_cursor_navigation() {
    let mut lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Insert entries
    lsm.insert_entry(b"key:1", b"val:1", tx_id, lsn).unwrap();
    lsm.insert_entry(b"key:2", b"val:2", tx_id, lsn).unwrap();
    lsm.insert_entry(b"key:3", b"val:3", tx_id, lsn).unwrap();
    lsm.insert_entry(b"key:4", b"val:4", tx_id, lsn).unwrap();

    let mut cursor = lsm.scan(ScanBounds::All).unwrap();

    // Test forward navigation
    assert!(cursor.valid());
    assert_eq!(cursor.index_key().unwrap(), b"key:1");

    cursor.next().unwrap();
    assert_eq!(cursor.index_key().unwrap(), b"key:2");

    cursor.next().unwrap();
    assert_eq!(cursor.index_key().unwrap(), b"key:3");

    // Test seek
    cursor.seek(b"key:2").unwrap();
    assert_eq!(cursor.index_key().unwrap(), b"key:2");

    // Test prev
    cursor.prev().unwrap();
    assert_eq!(cursor.index_key().unwrap(), b"key:1");
}

#[test]
fn test_lsm_dense_ordered_stats() {
    let mut lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Insert entries
    for i in 0..10 {
        let key = format!("key:{:02}", i);
        let val = format!("val:{:02}", i);
        lsm.insert_entry(key.as_bytes(), val.as_bytes(), tx_id, lsn)
            .unwrap();
    }

    let stats = lsm.stats().unwrap();

    // Should have entry count
    assert!(stats.entry_count.is_some());
    assert_eq!(stats.entry_count.unwrap(), 10);

    // Should have size information
    assert!(stats.size_bytes.is_some());
}

#[test]
fn test_lsm_dense_ordered_verify() {
    let mut lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Insert some entries
    lsm.insert_entry(b"key:1", b"val:1", tx_id, lsn).unwrap();
    lsm.insert_entry(b"key:2", b"val:2", tx_id, lsn).unwrap();

    // Verify should succeed
    let report = lsm.verify().unwrap();

    // Should have checked some items
    assert!(report.checked_items > 0);

    // Should have no errors for a valid LSM tree
    assert_eq!(report.errors.len(), 0);
}

#[test]
fn test_lsm_dense_ordered_empty_scan() {
    let lsm = create_test_lsm();

    // Scan empty LSM tree
    let mut cursor = lsm.scan(ScanBounds::All).unwrap();

    // Should be invalid immediately
    assert!(!cursor.valid());
}

#[test]
fn test_lsm_dense_ordered_duplicate_keys() {
    let mut lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Insert multiple entries with the same index key but different primary keys
    // This simulates a non-unique secondary index
    lsm.insert_entry(b"age:25", b"user:1", tx_id, lsn).unwrap();
    lsm.insert_entry(b"age:25", b"user:2", tx_id, lsn).unwrap();
    lsm.insert_entry(b"age:25", b"user:3", tx_id, lsn).unwrap();

    // Scan should see only the latest version due to MVCC
    let mut cursor = lsm.scan(ScanBounds::All).unwrap();

    let mut count = 0;
    while cursor.valid() {
        assert_eq!(cursor.index_key().unwrap(), b"age:25");
        count += 1;
        cursor.next().unwrap();
    }

    // Should see only one entry (the latest version)
    assert_eq!(count, 1);
}

#[test]
fn test_lsm_dense_ordered_large_dataset() {
    let mut lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Insert a larger dataset
    for i in 0..100 {
        let key = format!("key:{:04}", i);
        let val = format!("val:{:04}", i);
        lsm.insert_entry(key.as_bytes(), val.as_bytes(), tx_id, lsn)
            .unwrap();
    }

    // Scan and verify all entries
    let mut cursor = lsm.scan(ScanBounds::All).unwrap();

    let mut count = 0;
    let mut prev_key: Option<Vec<u8>> = None;

    while cursor.valid() {
        let key = cursor.index_key().unwrap().to_vec();

        // Verify ordering
        if let Some(ref prev) = prev_key {
            assert!(key > *prev, "Keys should be in ascending order");
        }

        prev_key = Some(key);
        count += 1;
        cursor.next().unwrap();
    }

    assert_eq!(count, 100);
}

// Made with Bob
