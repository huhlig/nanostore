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

//! Tests for bloom filter tombstone-based rollback.

use nanokv::pager::Pager;
use nanokv::table::bloom::PagedBloomFilter;
use nanokv::txn::TransactionId;
use nanokv::types::TableId;
use nanokv::vfs::MemoryFileSystem;
use nanokv::wal::LogSequenceNumber;
use std::sync::Arc;

#[test]
fn test_bloom_filter_rollback_basic() {
    // Create test infrastructure
    let fs = Arc::new(MemoryFileSystem::new());
    let pager = Arc::new(
        Pager::create(
            &*fs,
            "test.db",
            nanokv::pager::PagerConfig::default(),
        )
        .unwrap(),
    );

    // Create bloom filter
    let table_id = TableId::from(1);
    let mut bloom = PagedBloomFilter::new(
        table_id,
        "test_bloom".to_string(),
        pager.clone(),
        1000,  // num_items
        10,    // bits_per_key
        None,  // auto hash functions
    )
    .unwrap();

    // Insert a key in a transaction
    let tx_id = TransactionId::from(1);
    let commit_lsn = LogSequenceNumber::from(100);
    
    bloom.insert(b"key1", tx_id, commit_lsn).unwrap();
    
    // Key should be present
    assert!(bloom.contains(b"key1").unwrap());
    
    // Add a tombstone (simulating rollback)
    bloom.add_tombstone(b"key1".to_vec(), tx_id).unwrap();
    
    // Commit the tombstone
    bloom.commit_tombstones(tx_id, commit_lsn);
    
    // Verify tombstone was added
    assert_eq!(bloom.tombstone_count(), 1);
    
    // The bits are still set in the filter
    assert!(bloom.contains(b"key1").unwrap());
}

#[test]
fn test_bloom_filter_rollback_visibility() {
    let fs = Arc::new(MemoryFileSystem::new());
    let pager = Arc::new(
        Pager::create(
            &*fs,
            "test.db",
            nanokv::pager::PagerConfig::default(),
        )
        .unwrap(),
    );

    let table_id = TableId::from(1);
    let mut bloom = PagedBloomFilter::new(
        table_id,
        "test_bloom".to_string(),
        pager.clone(),
        1000,
        10,
        None,
    )
    .unwrap();

    // Insert keys at different LSNs
    bloom.insert(b"key1", TransactionId::from(1), LogSequenceNumber::from(100)).unwrap();
    bloom.insert(b"key2", TransactionId::from(2), LogSequenceNumber::from(200)).unwrap();
    
    // Add tombstone for key1 at LSN 150
    bloom.add_tombstone(b"key1".to_vec(), TransactionId::from(1)).unwrap();
    bloom.commit_tombstones(TransactionId::from(1), LogSequenceNumber::from(150));
    
    // Verify tombstone was added
    assert_eq!(bloom.tombstone_count(), 1);
    
    // Both keys should still be in the filter
    assert!(bloom.contains(b"key1").unwrap());
    assert!(bloom.contains(b"key2").unwrap());
}

#[test]
fn test_bloom_filter_tombstone_vacuum() {
    let fs = Arc::new(MemoryFileSystem::new());
    let pager = Arc::new(
        Pager::create(
            &*fs,
            "test.db",
            nanokv::pager::PagerConfig::default(),
        )
        .unwrap(),
    );

    let table_id = TableId::from(1);
    let mut bloom = PagedBloomFilter::new(
        table_id,
        "test_bloom".to_string(),
        pager.clone(),
        1000,
        10,
        None,
    )
    .unwrap();

    // Add tombstones at different LSNs
    bloom.add_tombstone(b"key1".to_vec(), TransactionId::from(1)).unwrap();
    bloom.commit_tombstones(TransactionId::from(1), LogSequenceNumber::from(100));
    
    bloom.add_tombstone(b"key2".to_vec(), TransactionId::from(2)).unwrap();
    bloom.commit_tombstones(TransactionId::from(2), LogSequenceNumber::from(200));
    
    bloom.add_tombstone(b"key3".to_vec(), TransactionId::from(3)).unwrap();
    bloom.commit_tombstones(TransactionId::from(3), LogSequenceNumber::from(300));
    
    assert_eq!(bloom.tombstone_count(), 3);
    
    // Vacuum with min_visible_lsn = 150 should remove key1 (LSN 100)
    let removed = bloom.vacuum_tombstones(LogSequenceNumber::from(150));
    assert_eq!(removed, 1);
    assert_eq!(bloom.tombstone_count(), 2);
    
    // Vacuum with min_visible_lsn = 250 should remove key2 (LSN 200)
    let removed = bloom.vacuum_tombstones(LogSequenceNumber::from(250));
    assert_eq!(removed, 1);
    assert_eq!(bloom.tombstone_count(), 1);
    
    // Vacuum with min_visible_lsn = 350 should remove key3 (LSN 300)
    let removed = bloom.vacuum_tombstones(LogSequenceNumber::from(350));
    assert_eq!(removed, 1);
    assert_eq!(bloom.tombstone_count(), 0);
}

#[test]
fn test_bloom_filter_multiple_tombstones() {
    let fs = Arc::new(MemoryFileSystem::new());
    let pager = Arc::new(
        Pager::create(
            &*fs,
            "test.db",
            nanokv::pager::PagerConfig::default(),
        )
        .unwrap(),
    );

    let table_id = TableId::from(1);
    let mut bloom = PagedBloomFilter::new(
        table_id,
        "test_bloom".to_string(),
        pager.clone(),
        1000,
        10,
        None,
    )
    .unwrap();

    // Insert multiple keys
    for i in 0..10 {
        let key = format!("key{}", i);
        bloom.insert(key.as_bytes(), TransactionId::from(i), LogSequenceNumber::from(100 + i as u64)).unwrap();
    }
    
    // Tombstone half of them
    for i in 0..5 {
        let key = format!("key{}", i);
        bloom.add_tombstone(key.as_bytes().to_vec(), TransactionId::from(i)).unwrap();
        bloom.commit_tombstones(TransactionId::from(i), LogSequenceNumber::from(200 + i as u64));
    }
    
    assert_eq!(bloom.tombstone_count(), 5);
    
    // All keys should still be in the filter
    for i in 0..10 {
        let key = format!("key{}", i);
        assert!(bloom.contains(key.as_bytes()).unwrap());
    }
}

#[test]
fn test_bloom_filter_uncommitted_tombstone() {
    let fs = Arc::new(MemoryFileSystem::new());
    let pager = Arc::new(
        Pager::create(
            &*fs,
            "test.db",
            nanokv::pager::PagerConfig::default(),
        )
        .unwrap(),
    );

    let table_id = TableId::from(1);
    let mut bloom = PagedBloomFilter::new(
        table_id,
        "test_bloom".to_string(),
        pager.clone(),
        1000,
        10,
        None,
    )
    .unwrap();

    // Insert a key
    bloom.insert(b"key1", TransactionId::from(1), LogSequenceNumber::from(100)).unwrap();
    
    // Add tombstone but don't commit it
    bloom.add_tombstone(b"key1".to_vec(), TransactionId::from(1)).unwrap();
    
    // Tombstone should be present but uncommitted
    assert_eq!(bloom.tombstone_count(), 1);
    
    // Now commit the tombstone
    bloom.commit_tombstones(TransactionId::from(1), LogSequenceNumber::from(150));
    
    // Tombstone should still be present
    assert_eq!(bloom.tombstone_count(), 1);
    
    // Key should still be in the filter
    assert!(bloom.contains(b"key1").unwrap());
}

// Made with Bob
