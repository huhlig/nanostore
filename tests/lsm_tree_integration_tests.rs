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

//! Integration tests for the LSM Tree storage engine.
//!
//! These tests verify the LSM tree's configuration types and basic structures.
//! Full integration tests require a pager and VFS setup which is complex.
//!
//! Note: Many tests are marked as #[ignore] because the LSM implementation
//! is not yet complete or the API differs from expectations.

use nanostore::table::lsm::{BloomFilterBuilder, CompactionConfig, CompactionStrategy, LsmConfig};

/// Test bloom filter creation
#[test]
fn test_bloom_filter_creation() {
    let builder = BloomFilterBuilder::new(1000);
    let _filter = builder.build();

    // Filter should be created successfully (no panic)
}

/// Test LSM config creation
#[test]
fn test_lsm_config_creation() {
    let config = LsmConfig::default();

    assert!(config.memtable.max_size > 0);
    assert!(!config.compaction.levels.is_empty());
}

/// Test compaction strategies
#[test]
fn test_compaction_strategies() {
    let strategies = vec![CompactionStrategy::Leveled, CompactionStrategy::Universal];

    for strategy in strategies {
        let mut config = CompactionConfig::default();
        config.strategy = strategy;
        assert_eq!(config.strategy, strategy);
    }
}

/// Test default LSM config values
#[test]
fn test_default_lsm_config() {
    let config = LsmConfig::default();

    // Verify reasonable defaults
    assert!(config.memtable.max_size >= 1024 * 1024); // At least 1MB
    assert!(config.sstable.target_size >= 1024); // At least 1KB
    assert!(!config.compaction.levels.is_empty());
    assert!(config.compaction.max_threads > 0);
}

// The following tests are disabled because they require API changes
// or full LSM tree implementation with pager/VFS

/// Test memtable operations
#[test]
fn test_memtable_operations() {
    use nanostore::table::lsm::Memtable;
    use nanostore::txn::TransactionId;
    use nanostore::wal::LogSequenceNumber;

    let memtable = Memtable::new(1024 * 1024);

    // Test insert
    memtable
        .insert(
            b"key1".to_vec(),
            b"value1".to_vec(),
            TransactionId::from(1),
            Some(LogSequenceNumber::from(100)),
        )
        .unwrap();

    // Test get
    let value = memtable.get(b"key1", LogSequenceNumber::from(100)).unwrap();
    assert_eq!(value, Some(b"value1".to_vec()));

    // Test get with earlier LSN (should return None)
    let value = memtable.get(b"key1", LogSequenceNumber::from(99)).unwrap();
    assert_eq!(value, None);

    // Test delete
    memtable
        .delete(
            b"key1".to_vec(),
            TransactionId::from(2),
            Some(LogSequenceNumber::from(200)),
        )
        .unwrap();

    // Test get after delete (should return None)
    let value = memtable.get(b"key1", LogSequenceNumber::from(200)).unwrap();
    assert_eq!(value, None);

    // Test get before delete (should return original value)
    let value = memtable.get(b"key1", LogSequenceNumber::from(150)).unwrap();
    assert_eq!(value, Some(b"value1".to_vec()));
}

/// Test bloom filter operations
#[test]
fn test_bloom_filter_operations() {
    use nanostore::table::lsm::BloomFilterBuilder;

    // Create a bloom filter using the builder
    let mut filter = BloomFilterBuilder::new(1000).bits_per_key(10).build();

    // Add keys to the bloom filter
    filter.insert(b"key1");
    filter.insert(b"key2");
    filter.insert(b"key3");

    // Test that inserted keys are found
    assert!(filter.contains(b"key1"));
    assert!(filter.contains(b"key2"));
    assert!(filter.contains(b"key3"));

    // Test that non-inserted keys are likely not present
    // (Bloom filters can have false positives but never false negatives)
    assert!(!filter.contains(b"nonexistent_key"));

    // Test filter statistics
    assert_eq!(filter.num_items(), 3);
    assert_eq!(filter.num_bits(), 1000 * 10);

    // Test false positive rate is reasonable
    let fpr = filter.false_positive_rate();
    assert!(fpr > 0.0 && fpr < 1.0);
}

/// Test SSTable operations (requires full implementation)
#[test]
fn test_sstable_operations() {
    use nanostore::pager::{Pager, PagerConfig};
    use nanostore::table::lsm::{SStableConfig, SStableId, SStableReader, SStableWriter};
    use nanostore::txn::VersionChain;
    use nanostore::vfs::MemoryFileSystem;
    use nanostore::wal::LogSequenceNumber;
    use std::sync::Arc;

    // Create pager with MemoryFileSystem
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).expect("Failed to create pager"));

    // Create SSTable writer
    let sstable_id = SStableId::new(1);
    let sstable_config = SStableConfig::default();
    let mut writer = SStableWriter::new(
        pager.clone(),
        sstable_id,
        0, // Level 0
        sstable_config.clone(),
        3, // estimated entries
    );

    // Add sorted key-value pairs
    let keys = vec!["alpha", "beta", "gamma"];
    for key in &keys {
        let mut chain = VersionChain::new(key.as_bytes().to_vec(), 1.into());
        chain.commit(LogSequenceNumber::from(1));
        writer
            .add(key.as_bytes().to_vec(), chain)
            .expect("Failed to add entry");
    }

    // Finish writing SSTable
    let created_lsn = LogSequenceNumber::from(1);
    let metadata = writer
        .finish(created_lsn)
        .expect("Failed to finish SSTable");

    // Verify metadata
    assert_eq!(metadata.id.as_u64(), 1);
    assert_eq!(metadata.level, 0);
    assert_eq!(metadata.num_entries, 3);
    assert_eq!(metadata.min_key, b"alpha");
    assert_eq!(metadata.max_key, b"gamma");

    // Open SSTable for reading
    let reader = SStableReader::open(pager.clone(), metadata.first_page_id, sstable_config)
        .expect("Failed to open SSTable");

    // Verify bloom filter (note: bloom filters can have false positives)
    assert!(reader.may_contain(b"alpha"));
    assert!(reader.may_contain(b"beta"));
    assert!(reader.may_contain(b"gamma"));
    // "nonexistent" might be a false positive, so we don't assert it's definitely not present

    // Read back the keys
    let snapshot_lsn = LogSequenceNumber::from(1);
    for key in &keys {
        let value = reader
            .get(key.as_bytes(), snapshot_lsn)
            .expect("Failed to get key");
        assert!(value.is_some(), "Key {} should exist", key);
        assert_eq!(
            value.unwrap(),
            key.as_bytes(),
            "Value should match key for {}",
            key
        );
    }

    // Verify non-existent key
    let result = reader
        .get(b"nonexistent", snapshot_lsn)
        .expect("Failed to get key");
    assert!(result.is_none(), "Non-existent key should not be found");
}

/// Test compaction (requires full implementation)
#[test]
fn test_compaction() {
    use nanostore::table::lsm::{CompactionPicker, FileMetadata, Version, VersionEdit};
    use nanostore::wal::LogSequenceNumber;

    // Create a version with L0 files that exceed the size limit
    let mut version = Version::new(7);

    // Add L0 files (each 2MB, total exceeds 10MB limit)
    for i in 0..6u64 {
        let mut metadata = FileMetadata {
            id: nanostore::table::lsm::SStableId::new(i),
            level: 0,
            min_key: format!("key{:04}", i).into_bytes(),
            max_key: format!("key{:04}", i + 1).into_bytes(),
            num_entries: 100,
            total_size: 2 * 1024 * 1024, // 2MB each
            created_lsn: LogSequenceNumber::from(0),
            first_page_id: nanostore::pager::PageId::from(i),
            num_pages: 1,
        };
        // Make key ranges non-overlapping for cleaner test
        metadata.min_key = format!("a{:04}", i).into_bytes();
        metadata.max_key = format!("b{:04}", i).into_bytes();
        let edit = VersionEdit::add_sstable(metadata);
        version = version.apply(&edit).unwrap();
    }

    // Create compaction picker with default config
    let config = nanostore::table::lsm::CompactionConfig::default();
    let picker = CompactionPicker::new(config);

    // Should pick L0 compaction since we exceed the size limit
    let job = picker.pick_compaction(&version);
    assert!(
        job.is_some(),
        "Should pick compaction when L0 exceeds size limit"
    );

    let job = job.unwrap();
    assert_eq!(job.source_level, 0);
    assert_eq!(job.target_level, 1);
    assert!(
        !job.source_files.is_empty(),
        "Should have source files to compact"
    );
    assert!(
        job.priority > 1.0,
        "Priority should be > 1.0 when over limit"
    );

    // Verify input file IDs are unique
    let input_ids = job.input_file_ids();
    assert_eq!(input_ids.len(), job.input_file_count());

    // Verify estimated output size calculation
    let expected_size: u64 = job.source_files.iter().map(|f| f.total_size).sum::<u64>()
        + job.target_files.iter().map(|f| f.total_size).sum::<u64>();
    assert_eq!(job.estimated_output_size, expected_size);
}

/// Test LSM tree MVCC snapshot isolation
#[test]
fn test_lsm_mvcc() {
    use nanostore::pager::{PageType, Pager, PagerConfig};
    use nanostore::table::lsm::LsmTree;
    use nanostore::table::{Flushable, MutableTable, PointLookup, SearchableTable};
    use nanostore::txn::TransactionId;
    use nanostore::types::TableId;
    use nanostore::vfs::MemoryFileSystem;
    use nanostore::wal::LogSequenceNumber;
    use std::sync::Arc;

    // Create pager with MemoryFileSystem
    let fs = MemoryFileSystem::new();
    let pager_config = PagerConfig::default();
    let pager =
        Arc::new(Pager::create(&fs, "test.db", pager_config).expect("Failed to create pager"));

    // Allocate a root page for the LSM tree
    let root_page_id = pager
        .allocate_page(PageType::LsmMeta)
        .expect("Failed to allocate page");

    let lsm_config = LsmConfig::default();
    let lsm: LsmTree<MemoryFileSystem> = LsmTree::new(
        TableId::from(1),
        "test_lsm".to_string(),
        pager,
        root_page_id,
        lsm_config,
    )
    .expect("Failed to create LSM tree");

    // Transaction 1: Insert initial value at LSN 10
    let tx1 = TransactionId::from(1);
    let lsn1 = LogSequenceNumber::from(10);
    let mut writer1 = lsm.writer(tx1, lsn1).expect("Failed to create writer");
    writer1
        .put(b"mvcc_key", b"value_v1")
        .expect("Failed to put");
    writer1.flush().expect("Failed to flush");
    writer1
        .commit_versions(LogSequenceNumber::from(10))
        .expect("Failed to commit");

    // Transaction 2: Update value at LSN 20
    let tx2 = TransactionId::from(2);
    let lsn2 = LogSequenceNumber::from(20);
    let mut writer2 = lsm.writer(tx2, lsn2).expect("Failed to create writer");
    writer2
        .put(b"mvcc_key", b"value_v2")
        .expect("Failed to put");
    writer2.flush().expect("Failed to flush");
    writer2
        .commit_versions(LogSequenceNumber::from(20))
        .expect("Failed to commit");

    // Transaction 3: Delete key at LSN 30
    let tx3 = TransactionId::from(3);
    let lsn3 = LogSequenceNumber::from(30);
    let mut writer3 = lsm.writer(tx3, lsn3).expect("Failed to create writer");
    writer3.delete(b"mvcc_key").expect("Failed to delete");
    writer3.flush().expect("Failed to flush");
    writer3
        .commit_versions(LogSequenceNumber::from(30))
        .expect("Failed to commit");

    // Read at LSN 5: key should not exist (before any writes)
    let reader_before = lsm
        .reader(LogSequenceNumber::from(5))
        .expect("Failed to create reader");
    let result_before = reader_before
        .get(b"mvcc_key", LogSequenceNumber::from(5))
        .expect("Failed to get");
    assert!(
        result_before.is_none(),
        "Key should not exist before any writes"
    );

    // Read at LSN 15: should see v1 (after tx1, before tx2)
    let reader_v1 = lsm
        .reader(LogSequenceNumber::from(15))
        .expect("Failed to create reader");
    let result_v1 = reader_v1
        .get(b"mvcc_key", LogSequenceNumber::from(15))
        .expect("Failed to get");
    assert!(result_v1.is_some(), "Key should exist at LSN 15");
    assert_eq!(
        result_v1.unwrap().0.as_slice(),
        b"value_v1",
        "Should see v1 at LSN 15"
    );

    // Read at LSN 25: should see v2 (after tx2, before tx3)
    let reader_v2 = lsm
        .reader(LogSequenceNumber::from(25))
        .expect("Failed to create reader");
    let result_v2 = reader_v2
        .get(b"mvcc_key", LogSequenceNumber::from(25))
        .expect("Failed to get");
    assert!(result_v2.is_some(), "Key should exist at LSN 25");
    assert_eq!(
        result_v2.unwrap().0.as_slice(),
        b"value_v2",
        "Should see v2 at LSN 25"
    );

    // Read at LSN 35: key should be deleted (after tx3)
    let reader_after = lsm
        .reader(LogSequenceNumber::from(35))
        .expect("Failed to create reader");
    let result_after = reader_after
        .get(b"mvcc_key", LogSequenceNumber::from(35))
        .expect("Failed to get");
    assert!(result_after.is_none(), "Key should be deleted at LSN 35");

    // Test multiple keys with different versions
    let tx4 = TransactionId::from(4);
    let lsn4 = LogSequenceNumber::from(40);
    let mut writer4 = lsm.writer(tx4, lsn4).expect("Failed to create writer");
    writer4.put(b"key_a", b"a_v1").expect("Failed to put");
    writer4.put(b"key_b", b"b_v1").expect("Failed to put");
    writer4.flush().expect("Failed to flush");
    writer4
        .commit_versions(LogSequenceNumber::from(40))
        .expect("Failed to commit");

    let tx5 = TransactionId::from(5);
    let lsn5 = LogSequenceNumber::from(50);
    let mut writer5 = lsm.writer(tx5, lsn5).expect("Failed to create writer");
    writer5.put(b"key_a", b"a_v2").expect("Failed to put");
    // key_b stays at v1
    writer5.flush().expect("Failed to flush");
    writer5
        .commit_versions(LogSequenceNumber::from(50))
        .expect("Failed to commit");

    // Read at LSN 45: both keys at v1
    let reader_45 = lsm
        .reader(LogSequenceNumber::from(45))
        .expect("Failed to create reader");
    let key_a_45 = reader_45
        .get(b"key_a", LogSequenceNumber::from(45))
        .expect("Failed to get");
    let key_b_45 = reader_45
        .get(b"key_b", LogSequenceNumber::from(45))
        .expect("Failed to get");
    assert_eq!(key_a_45.unwrap().0.as_slice(), b"a_v1");
    assert_eq!(key_b_45.unwrap().0.as_slice(), b"b_v1");

    // Read at LSN 55: key_a at v2, key_b still at v1
    let reader_55 = lsm
        .reader(LogSequenceNumber::from(55))
        .expect("Failed to create reader");
    let key_a_55 = reader_55
        .get(b"key_a", LogSequenceNumber::from(55))
        .expect("Failed to get");
    let key_b_55 = reader_55
        .get(b"key_b", LogSequenceNumber::from(55))
        .expect("Failed to get");
    assert_eq!(key_a_55.unwrap().0.as_slice(), b"a_v2");
    assert_eq!(key_b_55.unwrap().0.as_slice(), b"b_v1");
}

// Made with Bob
