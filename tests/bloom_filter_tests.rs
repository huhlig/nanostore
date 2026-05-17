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

//! Comprehensive integration tests for PagedBloomFilter

use nanokv::pager::{PageSize, Pager, PagerConfig};
use nanokv::table::ApproximateMembership;
use nanokv::table::bloom::PagedBloomFilter;
use nanokv::txn::TransactionId;
use nanokv::types::TableId;
use nanokv::vfs::MemoryFileSystem;
use nanokv::wal::LogSequenceNumber;
use std::sync::Arc;

fn create_test_pager() -> Arc<Pager<MemoryFileSystem>> {
    let fs = Arc::new(MemoryFileSystem::new());
    let pager = Pager::create(
        fs.as_ref(),
        "test.db",
        PagerConfig::new().with_page_size(PageSize::Size4KB),
    )
    .unwrap();
    Arc::new(pager)
}

#[test]
fn test_basic_insert_and_contains() {
    let pager = create_test_pager();
    let filter = PagedBloomFilter::new(
        TableId::from(1),
        "test_bloom".to_string(),
        pager,
        100,
        10,
        None,
    )
    .unwrap();

    // Insert keys
    filter
        .insert(b"key1", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    filter
        .insert(b"key2", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    filter
        .insert(b"key3", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    // Verify inserted keys are found
    assert!(filter.contains(b"key1").unwrap());
    assert!(filter.contains(b"key2").unwrap());
    assert!(filter.contains(b"key3").unwrap());

    // Non-inserted keys should mostly return false
    // (some false positives are expected)
    let mut false_positives = 0;
    for i in 100u32..200u32 {
        if filter.contains(&i.to_le_bytes()).unwrap() {
            false_positives += 1;
        }
    }

    // With 10 bits per key, false positive rate should be low
    assert!(
        false_positives < 20,
        "Too many false positives: {}",
        false_positives
    );
}

#[test]
fn test_persistence_and_reopen() {
    let pager = create_test_pager();
    let table_id = TableId::from(1);
    let name = "test_bloom".to_string();

    // Create and populate filter
    let root_page_id = {
        let filter =
            PagedBloomFilter::new(table_id, name.clone(), pager.clone(), 100, 10, None).unwrap();

        filter
            .insert(
                b"persistent_key1",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )
            .unwrap();
        filter
            .insert(
                b"persistent_key2",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )
            .unwrap();
        filter
            .insert(
                b"persistent_key3",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )
            .unwrap();

        filter.root_page_id()
    };

    // Reopen and verify
    let filter = PagedBloomFilter::open(table_id, name, pager, root_page_id).unwrap();

    assert!(filter.contains(b"persistent_key1").unwrap());
    assert!(filter.contains(b"persistent_key2").unwrap());
    assert!(filter.contains(b"persistent_key3").unwrap());
}

#[test]
fn test_false_positive_rate_calculation() {
    let pager = create_test_pager();
    let num_items = 1000;
    let filter = PagedBloomFilter::new(
        TableId::from(1),
        "test_bloom".to_string(),
        pager,
        num_items,
        10,
        None,
    )
    .unwrap();

    // Insert items
    let tx_id = TransactionId::from(1);
    let commit_lsn = LogSequenceNumber::from(1);
    for i in 0..num_items {
        filter.insert(&i.to_le_bytes(), tx_id, commit_lsn).unwrap();
    }

    // Check theoretical false positive rate
    let theoretical_fpr = filter.false_positive_rate();
    assert!(theoretical_fpr > 0.0 && theoretical_fpr < 0.05);

    // Measure actual false positive rate
    let mut false_positives = 0;
    let test_items = 10000;
    for i in num_items..(num_items + test_items) {
        if filter.contains(&i.to_le_bytes()).unwrap() {
            false_positives += 1;
        }
    }

    let actual_fpr = false_positives as f64 / test_items as f64;

    // Actual FPR should be close to theoretical (within 3x due to hash function variance)
    assert!(
        actual_fpr < theoretical_fpr * 3.0,
        "Actual FPR ({:.4}) too high compared to theoretical ({:.4})",
        actual_fpr,
        theoretical_fpr
    );
}

#[test]
fn test_clear_operation() {
    let pager = create_test_pager();
    let filter = PagedBloomFilter::new(
        TableId::from(1),
        "test_bloom".to_string(),
        pager,
        100,
        10,
        None,
    )
    .unwrap();

    // Insert and verify
    filter
        .insert(b"key1", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    filter
        .insert(b"key2", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    assert!(filter.contains(b"key1").unwrap());
    assert!(filter.contains(b"key2").unwrap());

    // Clear and verify
    filter.clear().unwrap();

    // After clear, false positive rate should be very low
    let mut found = 0;
    for i in 0u32..100u32 {
        if filter.contains(&i.to_le_bytes()).unwrap() {
            found += 1;
        }
    }

    // Should find very few (ideally 0, but some false positives possible)
    assert!(found < 5, "Found {} items after clear", found);
}

#[test]
fn test_large_filter() {
    let pager = create_test_pager();
    let num_items = 10000;
    let filter = PagedBloomFilter::new(
        TableId::from(1),
        "large_bloom".to_string(),
        pager,
        num_items,
        10,
        None,
    )
    .unwrap();

    // Insert many items
    let tx_id = TransactionId::from(1);
    let commit_lsn = LogSequenceNumber::from(1);
    for i in 0..num_items {
        filter.insert(&i.to_le_bytes(), tx_id, commit_lsn).unwrap();
    }

    // Verify all inserted items are found
    for i in 0..num_items {
        assert!(
            filter.contains(&i.to_le_bytes()).unwrap(),
            "Item {} not found",
            i
        );
    }

    // Check false positive rate
    let mut false_positives = 0;
    let test_range = 10000;
    for i in num_items..(num_items + test_range) {
        if filter.contains(&i.to_le_bytes()).unwrap() {
            false_positives += 1;
        }
    }

    let fpr = false_positives as f64 / test_range as f64;
    assert!(fpr < 0.05, "False positive rate too high: {:.4}", fpr);
}

#[test]
fn test_different_bits_per_key() {
    let pager = create_test_pager();

    // Test with different bits_per_key values
    for bits_per_key in [5, 10, 15, 20] {
        let filter = PagedBloomFilter::new(
            TableId::from(1),
            format!("bloom_{}", bits_per_key),
            pager.clone(),
            1000,
            bits_per_key,
            None,
        )
        .unwrap();

        // Insert items
        let tx_id = TransactionId::from(1);
        let commit_lsn = LogSequenceNumber::from(1);
        for i in 0u32..1000u32 {
            filter.insert(&i.to_le_bytes(), tx_id, commit_lsn).unwrap();
        }

        // Measure false positive rate
        let mut false_positives = 0;
        for i in 1000u32..2000u32 {
            if filter.contains(&i.to_le_bytes()).unwrap() {
                false_positives += 1;
            }
        }

        let fpr = false_positives as f64 / 1000.0;

        // Higher bits_per_key should give lower FPR
        // Note: These are generous bounds due to hash function variance
        let expected_max_fpr = match bits_per_key {
            5 => 0.25,
            10 => 0.08,
            15 => 0.03,
            20 => 0.015,
            _ => 1.0,
        };

        assert!(
            fpr < expected_max_fpr,
            "FPR {:.4} too high for {} bits/key (expected < {:.4})",
            fpr,
            bits_per_key,
            expected_max_fpr
        );
    }
}

#[test]
fn test_custom_hash_functions() {
    let pager = create_test_pager();

    // Test with different numbers of hash functions
    for num_hash in [3, 5, 7, 10] {
        let filter = PagedBloomFilter::new(
            TableId::from(1),
            format!("bloom_hash_{}", num_hash),
            pager.clone(),
            1000,
            10,
            Some(num_hash),
        )
        .unwrap();

        // Insert items
        let tx_id = TransactionId::from(1);
        let commit_lsn = LogSequenceNumber::from(1);
        for i in 0u32..1000u32 {
            filter.insert(&i.to_le_bytes(), tx_id, commit_lsn).unwrap();
        }

        // Verify all items are found
        for i in 0u32..1000u32 {
            assert!(filter.contains(&i.to_le_bytes()).unwrap());
        }
    }
}

#[test]
fn test_table_trait_implementation() {
    let pager = create_test_pager();
    let filter = PagedBloomFilter::new(
        TableId::from(42),
        "trait_test".to_string(),
        pager,
        100,
        10,
        None,
    )
    .unwrap();

    // Test Table trait methods
    use nanokv::table::Table;
    assert_eq!(Table::table_id(&filter), TableId::from(42));
    assert_eq!(Table::name(&filter), "trait_test");

    let caps = Table::capabilities(&filter);
    assert!(!caps.ordered);
    assert!(caps.point_lookup);
    assert!(!caps.prefix_scan);
    assert!(caps.disk_resident);

    let stats = Table::stats(&filter).unwrap();
    assert_eq!(stats.row_count, Some(0));
}

#[test]
fn test_approximate_membership_trait() {
    let pager = create_test_pager();
    let mut filter = PagedBloomFilter::new(
        TableId::from(1),
        "membership_test".to_string(),
        pager,
        100,
        10,
        None,
    )
    .unwrap();

    // Test ApproximateMembership trait methods
    filter
        .insert_key(
            b"test_key",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    assert!(filter.might_contain(b"test_key").unwrap());

    let fpr = filter.false_positive_rate();
    assert!(fpr > 0.0);

    use nanokv::table::ApproximateMembership;
    let caps = ApproximateMembership::capabilities(&filter);
    assert!(!caps.exact);
    assert!(caps.approximate);
    assert!(!caps.ordered);
}

#[test]
fn test_verification() {
    let pager = create_test_pager();
    let filter = PagedBloomFilter::new(
        TableId::from(1),
        "verify_test".to_string(),
        pager,
        100,
        10,
        None,
    )
    .unwrap();

    // Insert items
    let tx_id = TransactionId::from(1);
    let commit_lsn = LogSequenceNumber::from(1);
    for i in 0u32..50u32 {
        filter.insert(&i.to_le_bytes(), tx_id, commit_lsn).unwrap();
    }

    // Verify the filter
    let report = filter.verify().unwrap();
    assert_eq!(report.errors.len(), 0);

    // Should not have high FPR warning with only 50 items
    assert!(report.warnings.is_empty() || report.warnings.len() < 2);
}

#[test]
fn test_empty_filter() {
    let pager = create_test_pager();
    let filter = PagedBloomFilter::new(
        TableId::from(1),
        "empty_test".to_string(),
        pager,
        100,
        10,
        None,
    )
    .unwrap();

    // Empty filter should return false for all queries
    for i in 0u32..100u32 {
        assert!(!filter.contains(&i.to_le_bytes()).unwrap());
    }

    // False positive rate should be 0 for empty filter
    assert_eq!(filter.false_positive_rate(), 0.0);
}

#[test]
fn test_concurrent_reads() {
    use std::thread;

    let pager = create_test_pager();
    let filter = PagedBloomFilter::new(
        TableId::from(1),
        "concurrent_test".to_string(),
        pager,
        1000,
        10,
        None,
    )
    .unwrap();

    // Insert items
    let tx_id = TransactionId::from(1);
    let commit_lsn = LogSequenceNumber::from(1);
    for i in 0u32..1000u32 {
        filter.insert(&i.to_le_bytes(), tx_id, commit_lsn).unwrap();
    }

    // Share the filter across threads (wrapped in Arc)
    let filter = Arc::new(filter);
    let mut handles = vec![];

    // Spawn multiple reader threads
    for thread_id in 0..4 {
        let filter_clone = Arc::clone(&filter);
        let handle = thread::spawn(move || {
            let start = thread_id * 250u32;
            let end = start + 250u32;

            for i in start..end {
                assert!(
                    filter_clone.contains(&i.to_le_bytes()).unwrap(),
                    "Thread {} failed to find item {}",
                    thread_id,
                    i
                );
            }
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_statistics() {
    let pager = create_test_pager();
    let filter = PagedBloomFilter::new(
        TableId::from(1),
        "stats_test".to_string(),
        pager,
        100,
        10,
        None,
    )
    .unwrap();

    // Insert items and check stats
    let tx_id = TransactionId::from(1);
    let commit_lsn = LogSequenceNumber::from(1);
    for i in 0u32..50u32 {
        filter.insert(&i.to_le_bytes(), tx_id, commit_lsn).unwrap();
    }

    use nanokv::table::ApproximateMembership;
    let stats = ApproximateMembership::stats(&filter).unwrap();
    assert_eq!(stats.entry_count, Some(50));
    assert_eq!(stats.distinct_keys, Some(50));
    assert!(stats.size_bytes.is_some());
    assert!(stats.size_bytes.unwrap() > 0);
}

// Made with Bob
