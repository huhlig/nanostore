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

//! Comprehensive tests for DenseOrdered specialty table implementations.
//!
//! Tests both MemoryBTree and PagedBTree implementations of the DenseOrdered trait,
//! which is used for traditional B-Tree based secondary indexes.

use nanokv::pager::{Pager, PagerConfig};
use nanokv::table::{DenseOrdered, MemoryBTree, PagedBTree, SpecialtyTableCursor};
use nanokv::txn::TransactionId;
use nanokv::types::{Bound, KeyBuf, ScanBounds, TableId};
use nanokv::vfs::MemoryFileSystem;
use nanokv::wal::LogSequenceNumber;
use std::sync::Arc;

// =============================================================================
// Helper Functions
// =============================================================================

fn create_memory_index(name: &str) -> MemoryBTree {
    MemoryBTree::new(TableId::from(1), name.to_string())
}

fn create_paged_index(name: &str) -> (PagedBTree<MemoryFileSystem>, Arc<Pager<MemoryFileSystem>>) {
    let fs = Arc::new(MemoryFileSystem::new());
    let mut config = PagerConfig::default();
    // Disable cache for tests to avoid caching issues
    config.cache_capacity = 0;
    let pager = Arc::new(Pager::create(&*fs, "test.db", config).unwrap());
    let btree = PagedBTree::new(TableId::from(1), name.to_string(), pager.clone()).unwrap();
    (btree, pager)
}

// =============================================================================
// Basic Insert/Delete Tests
// =============================================================================

#[test]
fn test_memory_btree_insert_entry() {
    let mut index = create_memory_index("user_email_idx");

    // Insert index entries: email -> user_id
    index
        .insert_entry(b"alice@example.com", b"user_001", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    index.insert_entry(b"bob@example.com", b"user_002", TransactionId::from(1), LogSequenceNumber::from(2)).unwrap();
    index
        .insert_entry(b"charlie@example.com", b"user_003", TransactionId::from(1), LogSequenceNumber::from(3))
        .unwrap();

    // Verify entries exist via scan
    let mut cursor = index.scan(ScanBounds::All).unwrap();

    assert!(cursor.valid());
    assert_eq!(cursor.index_key(), Some(b"alice@example.com".as_ref()));
    assert_eq!(cursor.primary_key(), Some(b"user_001".as_ref()));

    cursor.next().unwrap();
    assert_eq!(cursor.index_key(), Some(b"bob@example.com".as_ref()));
    assert_eq!(cursor.primary_key(), Some(b"user_002".as_ref()));

    cursor.next().unwrap();
    assert_eq!(cursor.index_key(), Some(b"charlie@example.com".as_ref()));
    assert_eq!(cursor.primary_key(), Some(b"user_003".as_ref()));

    cursor.next().unwrap();
    assert!(!cursor.valid());
}

#[test]
fn test_paged_btree_insert_entry() {
    let (mut index, pager) = create_paged_index("user_email_idx");

    // Insert index entries: email -> user_id
    index
        .insert_entry(b"alice@example.com", b"user_001", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    index.insert_entry(b"bob@example.com", b"user_002", TransactionId::from(1), LogSequenceNumber::from(2)).unwrap();
    index
        .insert_entry(b"charlie@example.com", b"user_003", TransactionId::from(1), LogSequenceNumber::from(3))
        .unwrap();

    // Flush pager cache to ensure writes are persisted
    pager.flush_cache().unwrap();

    // Verify entries exist via scan
    let mut cursor = index.scan(ScanBounds::All).unwrap();

    assert!(cursor.valid());
    assert_eq!(cursor.index_key(), Some(b"alice@example.com".as_ref()));
    assert_eq!(cursor.primary_key(), Some(b"user_001".as_ref()));

    cursor.next().unwrap();
    assert_eq!(cursor.index_key(), Some(b"bob@example.com".as_ref()));
    assert_eq!(cursor.primary_key(), Some(b"user_002".as_ref()));

    cursor.next().unwrap();
    assert_eq!(cursor.index_key(), Some(b"charlie@example.com".as_ref()));
    assert_eq!(cursor.primary_key(), Some(b"user_003".as_ref()));

    cursor.next().unwrap();
    assert!(!cursor.valid());
}

#[test]
fn test_memory_btree_delete_entry() {
    let mut index = create_memory_index("user_email_idx");

    // Insert entries
    index
        .insert_entry(b"alice@example.com", b"user_001", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    index.insert_entry(b"bob@example.com", b"user_002", TransactionId::from(1), LogSequenceNumber::from(2)).unwrap();
    index
        .insert_entry(b"charlie@example.com", b"user_003", TransactionId::from(1), LogSequenceNumber::from(3))
        .unwrap();

    // Delete one entry
    index.delete_entry(b"bob@example.com", b"user_002", TransactionId::from(1), LogSequenceNumber::from(10)).unwrap();

    // Verify deletion
    let mut cursor = index.scan(ScanBounds::All).unwrap();

    assert!(cursor.valid());
    assert_eq!(cursor.index_key(), Some(b"alice@example.com".as_ref()));

    cursor.next().unwrap();
    assert_eq!(cursor.index_key(), Some(b"charlie@example.com".as_ref()));

    cursor.next().unwrap();
    assert!(!cursor.valid());
}

#[test]
fn test_paged_btree_delete_entry() {
    let (mut index, pager) = create_paged_index("user_email_idx");

    // Insert entries
    index
        .insert_entry(b"alice@example.com", b"user_001", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    index.insert_entry(b"bob@example.com", b"user_002", TransactionId::from(1), LogSequenceNumber::from(2)).unwrap();
    index
        .insert_entry(b"charlie@example.com", b"user_003", TransactionId::from(1), LogSequenceNumber::from(3))
        .unwrap();

    // Delete one entry
    index.delete_entry(b"bob@example.com", b"user_002", TransactionId::from(1), LogSequenceNumber::from(10)).unwrap();

    // Flush pager cache
    pager.flush_cache().unwrap();

    // Verify deletion
    let mut cursor = index.scan(ScanBounds::All).unwrap();

    assert!(cursor.valid());
    assert_eq!(cursor.index_key(), Some(b"alice@example.com".as_ref()));

    cursor.next().unwrap();
    assert_eq!(cursor.index_key(), Some(b"charlie@example.com".as_ref()));

    cursor.next().unwrap();
    assert!(!cursor.valid());
}

// =============================================================================
// Range Scan Tests
// =============================================================================

#[test]
fn test_memory_btree_range_scan() {
    let mut index = create_memory_index("timestamp_idx");

    // Insert timestamp -> record_id entries
    for i in 0..10 {
        let timestamp = format!("2024-01-{:02}T00:00:00Z", i + 1);
        let record_id = format!("rec_{:03}", i);
        index
            .insert_entry(timestamp.as_bytes(), record_id.as_bytes(), TransactionId::from(1), LogSequenceNumber::from(i as u64 + 1))
            .unwrap();
    }

    // Range scan: from 2024-01-03 to 2024-01-07 (inclusive)
    let bounds = ScanBounds::Range {
        start: Bound::Included(KeyBuf(b"2024-01-03T00:00:00Z".to_vec())),
        end: Bound::Included(KeyBuf(b"2024-01-07T00:00:00Z".to_vec())),
    };

    let mut cursor = index.scan(bounds).unwrap();
    let mut count = 0;

    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }

    assert_eq!(count, 5); // Days 3, 4, 5, 6, 7
}

#[test]
fn test_paged_btree_range_scan() {
    let (mut index, pager) = create_paged_index("timestamp_idx");

    // Insert timestamp -> record_id entries
    for i in 0..10 {
        let timestamp = format!("2024-01-{:02}T00:00:00Z", i + 1);
        let record_id = format!("rec_{:03}", i);
        index
            .insert_entry(timestamp.as_bytes(), record_id.as_bytes(), TransactionId::from(1), LogSequenceNumber::from(i as u64 + 1))
            .unwrap();
    }

    // Flush pager cache
    pager.flush_cache().unwrap();

    // Range scan: from 2024-01-03 to 2024-01-07 (inclusive)
    let bounds = ScanBounds::Range {
        start: Bound::Included(KeyBuf(b"2024-01-03T00:00:00Z".to_vec())),
        end: Bound::Included(KeyBuf(b"2024-01-07T00:00:00Z".to_vec())),
    };

    let mut cursor = index.scan(bounds).unwrap();
    let mut count = 0;

    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }

    assert_eq!(count, 5); // Days 3, 4, 5, 6, 7
}

#[test]
fn test_memory_btree_prefix_scan() {
    let mut index = create_memory_index("category_idx");

    // Insert category -> product_id entries
    index
        .insert_entry(b"electronics:laptop:001", b"prod_001", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    index
        .insert_entry(b"electronics:laptop:002", b"prod_002", TransactionId::from(1), LogSequenceNumber::from(2))
        .unwrap();
    index
        .insert_entry(b"electronics:phone:001", b"prod_003", TransactionId::from(1), LogSequenceNumber::from(3))
        .unwrap();
    index
        .insert_entry(b"furniture:chair:001", b"prod_004", TransactionId::from(1), LogSequenceNumber::from(4))
        .unwrap();
    index
        .insert_entry(b"furniture:desk:001", b"prod_005", TransactionId::from(1), LogSequenceNumber::from(5))
        .unwrap();

    // Prefix scan: all electronics
    let bounds = ScanBounds::Prefix(KeyBuf(b"electronics:".to_vec()));
    let mut cursor = index.scan(bounds).unwrap();
    let mut count = 0;

    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }

    assert_eq!(count, 3); // All electronics items
}

#[test]
fn test_paged_btree_prefix_scan() {
    let (mut index, pager) = create_paged_index("category_idx");

    // Insert category -> product_id entries
    index
        .insert_entry(b"electronics:laptop:001", b"prod_001", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    index
        .insert_entry(b"electronics:laptop:002", b"prod_002", TransactionId::from(1), LogSequenceNumber::from(2))
        .unwrap();
    index
        .insert_entry(b"electronics:phone:001", b"prod_003", TransactionId::from(1), LogSequenceNumber::from(3))
        .unwrap();
    index
        .insert_entry(b"furniture:chair:001", b"prod_004", TransactionId::from(1), LogSequenceNumber::from(4))
        .unwrap();
    index
        .insert_entry(b"furniture:desk:001", b"prod_005", TransactionId::from(1), LogSequenceNumber::from(5))
        .unwrap();

    // Flush pager cache
    pager.flush_cache().unwrap();

    // Prefix scan: all electronics
    let bounds = ScanBounds::Prefix(KeyBuf(b"electronics:".to_vec()));
    let mut cursor = index.scan(bounds).unwrap();
    let mut count = 0;

    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }

    assert_eq!(count, 3); // All electronics items
}

// =============================================================================
// Composite Key Tests
// =============================================================================

#[test]
fn test_memory_btree_composite_key() {
    let mut index = create_memory_index("user_timestamp_idx");

    // Composite key: user_id + timestamp -> event_id
    // Format: "user_id|timestamp"
    index
        .insert_entry(b"user_001|2024-01-01T10:00:00Z", b"event_001", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    index
        .insert_entry(b"user_001|2024-01-01T11:00:00Z", b"event_002", TransactionId::from(1), LogSequenceNumber::from(2))
        .unwrap();
    index
        .insert_entry(b"user_001|2024-01-01T12:00:00Z", b"event_003", TransactionId::from(1), LogSequenceNumber::from(3))
        .unwrap();
    index
        .insert_entry(b"user_002|2024-01-01T10:00:00Z", b"event_004", TransactionId::from(1), LogSequenceNumber::from(4))
        .unwrap();
    index
        .insert_entry(b"user_002|2024-01-01T11:00:00Z", b"event_005", TransactionId::from(1), LogSequenceNumber::from(5))
        .unwrap();

    // Query all events for user_001
    let bounds = ScanBounds::Prefix(KeyBuf(b"user_001|".to_vec()));
    let mut cursor = index.scan(bounds).unwrap();
    let mut count = 0;

    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }

    assert_eq!(count, 3); // All user_001 events
}

#[test]
fn test_paged_btree_composite_key() {
    let (mut index, pager) = create_paged_index("user_timestamp_idx");

    // Composite key: user_id + timestamp -> event_id
    // Format: "user_id|timestamp"
    index
        .insert_entry(b"user_001|2024-01-01T10:00:00Z", b"event_001", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    index
        .insert_entry(b"user_001|2024-01-01T11:00:00Z", b"event_002", TransactionId::from(1), LogSequenceNumber::from(2))
        .unwrap();
    index
        .insert_entry(b"user_001|2024-01-01T12:00:00Z", b"event_003", TransactionId::from(1), LogSequenceNumber::from(3))
        .unwrap();
    index
        .insert_entry(b"user_002|2024-01-01T10:00:00Z", b"event_004", TransactionId::from(1), LogSequenceNumber::from(4))
        .unwrap();
    index
        .insert_entry(b"user_002|2024-01-01T11:00:00Z", b"event_005", TransactionId::from(1), LogSequenceNumber::from(5))
        .unwrap();

    // Flush pager cache
    pager.flush_cache().unwrap();

    // Query all events for user_001
    let bounds = ScanBounds::Prefix(KeyBuf(b"user_001|".to_vec()));
    let mut cursor = index.scan(bounds).unwrap();
    let mut count = 0;

    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }

    assert_eq!(count, 3); // All user_001 events
}

// =============================================================================
// Non-Unique Index Tests (Multiple entries with same index key)
// =============================================================================

#[test]
fn test_memory_btree_non_unique_index() {
    let mut index = create_memory_index("status_idx");

    // Non-unique index: status -> user_id
    // Multiple users can have the same status
    index.insert_entry(b"active", b"user_001", TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
    index.insert_entry(b"active", b"user_002", TransactionId::from(1), LogSequenceNumber::from(2)).unwrap();
    index.insert_entry(b"active", b"user_003", TransactionId::from(1), LogSequenceNumber::from(3)).unwrap();
    index.insert_entry(b"inactive", b"user_004", TransactionId::from(1), LogSequenceNumber::from(4)).unwrap();
    index.insert_entry(b"pending", b"user_005", TransactionId::from(1), LogSequenceNumber::from(5)).unwrap();

    // Note: Current implementation overwrites entries with same key
    // For true non-unique indexes, we'd need to encode primary key in index key
    // e.g., "active|user_001", "active|user_002", etc.

    // Scan all entries
    let mut cursor = index.scan(ScanBounds::All).unwrap();
    let mut count = 0;

    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }

    // With current implementation, duplicate keys overwrite
    assert_eq!(count, 3); // active (last), inactive, pending
}

#[test]
fn test_memory_btree_non_unique_with_encoded_key() {
    let mut index = create_memory_index("status_idx");

    // Proper non-unique index: encode primary key in index key
    // Format: "status|primary_key"
    index.insert_entry(b"active|user_001", b"user_001", TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
    index.insert_entry(b"active|user_002", b"user_002", TransactionId::from(1), LogSequenceNumber::from(2)).unwrap();
    index.insert_entry(b"active|user_003", b"user_003", TransactionId::from(1), LogSequenceNumber::from(3)).unwrap();
    index
        .insert_entry(b"inactive|user_004", b"user_004", TransactionId::from(1), LogSequenceNumber::from(4))
        .unwrap();
    index
        .insert_entry(b"pending|user_005", b"user_005", TransactionId::from(1), LogSequenceNumber::from(5))
        .unwrap();

    // Query all active users
    let bounds = ScanBounds::Prefix(KeyBuf(b"active|".to_vec()));
    let mut cursor = index.scan(bounds).unwrap();
    let mut count = 0;

    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }

    assert_eq!(count, 3); // All active users
}

// =============================================================================
// Cursor Navigation Tests
// =============================================================================

#[test]
fn test_memory_btree_cursor_seek() {
    let mut index = create_memory_index("score_idx");

    // Insert score -> player_id entries
    for i in 0..10 {
        let score = format!("{:05}", i * 100); // 00000, 00100, 00200, ...
        let player = format!("player_{:02}", i);
        index
            .insert_entry(score.as_bytes(), player.as_bytes(), TransactionId::from(1), LogSequenceNumber::from(i as u64 + 1))
            .unwrap();
    }

    // Seek to score 00500
    let mut cursor = index.scan(ScanBounds::All).unwrap();
    cursor.seek(b"00500").unwrap();

    assert!(cursor.valid());
    assert_eq!(cursor.index_key(), Some(b"00500".as_ref()));
    assert_eq!(cursor.primary_key(), Some(b"player_05".as_ref()));

    // Move forward
    cursor.next().unwrap();
    assert_eq!(cursor.index_key(), Some(b"00600".as_ref()));

    // Move backward
    cursor.prev().unwrap();
    assert_eq!(cursor.index_key(), Some(b"00500".as_ref()));
}

#[test]
fn test_paged_btree_cursor_seek() {
    let (mut index, _pager) = create_paged_index("score_idx");

    // Insert score -> player_id entries
    for i in 0..10 {
        let score = format!("{:05}", i * 100); // 00000, 00100, 00200, ...
        let player = format!("player_{:02}", i);
        index
            .insert_entry(score.as_bytes(), player.as_bytes(), TransactionId::from(1), LogSequenceNumber::from(i as u64 + 1))
            .unwrap();
    }

    // Seek to score 00500
    let mut cursor = index.scan(ScanBounds::All).unwrap();
    cursor.seek(b"00500").unwrap();

    assert!(cursor.valid());
    assert_eq!(cursor.index_key(), Some(b"00500".as_ref()));
    assert_eq!(cursor.primary_key(), Some(b"player_05".as_ref()));

    // Move forward
    cursor.next().unwrap();
    assert_eq!(cursor.index_key(), Some(b"00600".as_ref()));

    // Move backward
    cursor.prev().unwrap();
    assert_eq!(cursor.index_key(), Some(b"00500".as_ref()));
}

// =============================================================================
// Statistics and Verification Tests
// =============================================================================

#[test]
fn test_memory_btree_stats() {
    let mut index = create_memory_index("test_idx");

    // Insert entries
    for i in 0..100 {
        let key = format!("key_{:03}", i);
        let value = format!("value_{:03}", i);
        index
            .insert_entry(key.as_bytes(), value.as_bytes(), TransactionId::from(1), LogSequenceNumber::from(i as u64 + 1))
            .unwrap();
    }

    let stats = index.stats().unwrap();
    assert_eq!(stats.entry_count, Some(100));
    assert!(stats.size_bytes.is_some());
    assert!(stats.size_bytes.unwrap() > 0);
}

#[test]
fn test_paged_btree_stats() {
    let (mut index, _pager) = create_paged_index("test_idx");

    // Insert entries
    for i in 0..100 {
        let key = format!("key_{:03}", i);
        let value = format!("value_{:03}", i);
        index
            .insert_entry(key.as_bytes(), value.as_bytes(), TransactionId::from(1), LogSequenceNumber::from(i as u64 + 1))
            .unwrap();
    }

    let stats = index.stats().unwrap();
    assert_eq!(stats.entry_count, Some(100));
    assert_eq!(stats.distinct_keys, Some(100));
}

#[test]
fn test_memory_btree_verify() {
    let mut index = create_memory_index("test_idx");

    // Insert entries
    for i in 0..50 {
        let key = format!("key_{:03}", i);
        let value = format!("value_{:03}", i);
        index
            .insert_entry(key.as_bytes(), value.as_bytes(), TransactionId::from(1), LogSequenceNumber::from(i as u64 + 1))
            .unwrap();
    }

    let report = index.verify().unwrap();
    assert_eq!(report.checked_items, 50);
    assert!(report.errors.is_empty());
    assert!(report.warnings.is_empty());
}

#[test]
fn test_paged_btree_verify() {
    let (mut index, _pager) = create_paged_index("test_idx");

    // Insert entries
    for i in 0..50 {
        let key = format!("key_{:03}", i);
        let value = format!("value_{:03}", i);
        index
            .insert_entry(key.as_bytes(), value.as_bytes(), TransactionId::from(1), LogSequenceNumber::from(i as u64 + 1))
            .unwrap();
    }

    let report = index.verify().unwrap();
    assert!(report.checked_items > 0);
    assert!(report.errors.is_empty());
}

// =============================================================================
// Capabilities Tests
// =============================================================================

#[test]
fn test_memory_btree_capabilities() {
    let index = create_memory_index("test_idx");
    let caps = index.capabilities();

    assert!(caps.exact);
    assert!(!caps.approximate);
    assert!(caps.ordered);
    assert!(!caps.sparse);
    assert!(caps.supports_delete);
    assert!(caps.supports_range_query);
    assert!(caps.supports_prefix_query);
    assert!(!caps.supports_scoring);
}

#[test]
fn test_paged_btree_capabilities() {
    let (index, _pager) = create_paged_index("test_idx");
    let caps = index.capabilities();

    assert!(caps.exact);
    assert!(!caps.approximate);
    assert!(caps.ordered);
    assert!(!caps.sparse);
    assert!(caps.supports_delete);
    assert!(caps.supports_range_query);
    assert!(caps.supports_prefix_query);
    assert!(!caps.supports_scoring);
}

// =============================================================================
// Large Dataset Tests
// =============================================================================

#[test]
fn test_memory_btree_large_dataset() {
    let mut index = create_memory_index("large_idx");

    // Insert 1000 entries
    for i in 0..1000 {
        let key = format!("key_{:06}", i);
        let value = format!("value_{:06}", i);
        index
            .insert_entry(key.as_bytes(), value.as_bytes(), TransactionId::from(1), LogSequenceNumber::from(i as u64 + 1))
            .unwrap();
    }

    // Verify count
    let stats = index.stats().unwrap();
    assert_eq!(stats.entry_count, Some(1000));

    // Verify range scan works
    let bounds = ScanBounds::Range {
        start: Bound::Included(KeyBuf(b"key_000500".to_vec())),
        end: Bound::Excluded(KeyBuf(b"key_000600".to_vec())),
    };

    let mut cursor = index.scan(bounds).unwrap();
    let mut count = 0;

    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }

    assert_eq!(count, 100); // 500-599
}

#[test]
fn test_paged_btree_large_dataset() {
    let (mut index, pager) = create_paged_index("large_idx");

    // Insert 1000 entries
    for i in 0..1000 {
        let key = format!("key_{:06}", i);
        let value = format!("value_{:06}", i);
        index
            .insert_entry(key.as_bytes(), value.as_bytes(), TransactionId::from(1), LogSequenceNumber::from(i as u64 + 1))
            .unwrap();
    }

    // Flush pager cache
    pager.flush_cache().unwrap();

    // Verify count
    let stats = index.stats().unwrap();
    assert_eq!(stats.entry_count, Some(1000));

    // Verify range scan works
    let bounds = ScanBounds::Range {
        start: Bound::Included(KeyBuf(b"key_000500".to_vec())),
        end: Bound::Excluded(KeyBuf(b"key_000600".to_vec())),
    };

    let mut cursor = index.scan(bounds).unwrap();
    let mut count = 0;

    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }

    assert_eq!(count, 100); // 500-599
}

// Made with Bob
