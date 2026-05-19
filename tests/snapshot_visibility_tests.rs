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

use nanostore::snap::{Snapshot, SnapshotId};
use nanostore::txn::TransactionId;
use nanostore::wal::LogSequenceNumber;
use std::time::Instant;

/// Test basic visibility with watermark optimization
#[test]
fn test_visibility_basic() {
    let active_txns = vec![
        TransactionId::from(5),
        TransactionId::from(10),
        TransactionId::from(15),
    ];

    let snapshot = Snapshot::new(
        SnapshotId::from(1),
        "test".to_string(),
        LogSequenceNumber::from(100),
        0,
        0,
        active_txns,
    );

    // Transactions below watermark (5) should be visible
    assert!(snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(1)));
    assert!(snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(2)));
    assert!(snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(4)));

    // Active transactions should NOT be visible
    assert!(!snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(5)));
    assert!(!snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(10)));
    assert!(!snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(15)));

    // Transactions above watermark but not in active set should be visible
    assert!(snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(6)));
    assert!(snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(11)));
    assert!(snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(20)));

    // Versions committed after snapshot should NOT be visible
    assert!(!snapshot.is_visible(LogSequenceNumber::from(150), TransactionId::from(1)));
}

/// Test visibility with empty active transaction list
#[test]
fn test_visibility_no_active_transactions() {
    let snapshot = Snapshot::new(
        SnapshotId::from(1),
        "test".to_string(),
        LogSequenceNumber::from(100),
        0,
        0,
        vec![],
    );

    // All transactions committed before snapshot should be visible
    assert!(snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(1)));
    assert!(snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(100)));
    assert!(snapshot.is_visible(LogSequenceNumber::from(99), TransactionId::from(1000)));

    // Versions committed after snapshot should NOT be visible
    assert!(!snapshot.is_visible(LogSequenceNumber::from(101), TransactionId::from(1)));
}

/// Test visibility with single active transaction
#[test]
fn test_visibility_single_active_transaction() {
    let snapshot = Snapshot::new(
        SnapshotId::from(1),
        "test".to_string(),
        LogSequenceNumber::from(100),
        0,
        0,
        vec![TransactionId::from(42)],
    );

    // Transactions below watermark should be visible
    assert!(snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(1)));
    assert!(snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(41)));

    // The active transaction should NOT be visible
    assert!(!snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(42)));

    // Transactions above watermark should be visible
    assert!(snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(43)));
    assert!(snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(100)));
}

/// Test visibility with many concurrent transactions (stress test)
#[test]
fn test_visibility_many_concurrent_transactions() {
    // Create 1000 active transactions
    let active_txns: Vec<TransactionId> = (100..1100).map(TransactionId::from).collect();

    let snapshot = Snapshot::new(
        SnapshotId::from(1),
        "test".to_string(),
        LogSequenceNumber::from(10000),
        0,
        0,
        active_txns,
    );

    // Transactions below watermark (100) should be visible - O(1) fast path
    for i in 1..100 {
        assert!(
            snapshot.is_visible(LogSequenceNumber::from(5000), TransactionId::from(i)),
            "Transaction {} below watermark should be visible",
            i
        );
    }

    // Active transactions should NOT be visible - O(1) HashSet lookup
    for i in 100..1100 {
        assert!(
            !snapshot.is_visible(LogSequenceNumber::from(5000), TransactionId::from(i)),
            "Active transaction {} should NOT be visible",
            i
        );
    }

    // Transactions above active range should be visible - O(1) HashSet lookup
    for i in 1100..1200 {
        assert!(
            snapshot.is_visible(LogSequenceNumber::from(5000), TransactionId::from(i)),
            "Transaction {} above active range should be visible",
            i
        );
    }
}

/// Benchmark visibility checking performance with watermark optimization
#[test]
fn test_visibility_performance() {
    // Create snapshot with 10,000 active transactions
    let active_txns: Vec<TransactionId> = (1000..11000).map(TransactionId::from).collect();

    let snapshot = Snapshot::new(
        SnapshotId::from(1),
        "perf-test".to_string(),
        LogSequenceNumber::from(100000),
        0,
        0,
        active_txns,
    );

    // Test fast path (below watermark) - should be O(1)
    let start = Instant::now();
    for i in 0..10000 {
        let _ = snapshot.is_visible(LogSequenceNumber::from(50000), TransactionId::from(i));
    }
    let fast_path_duration = start.elapsed();

    // Test slow path (in active set) - should be O(1) with HashSet
    let start = Instant::now();
    for i in 1000..11000 {
        let _ = snapshot.is_visible(LogSequenceNumber::from(50000), TransactionId::from(i));
    }
    let slow_path_duration = start.elapsed();

    println!("Fast path (below watermark): {:?}", fast_path_duration);
    println!("Slow path (HashSet lookup): {:?}", slow_path_duration);

    // Both should be very fast with the optimization
    // The old Vec-based approach would be much slower for the slow path
    assert!(
        fast_path_duration.as_millis() < 10,
        "Fast path should be < 10ms"
    );
    assert!(
        slow_path_duration.as_millis() < 50,
        "Slow path should be < 50ms"
    );
}

/// Test visibility with non-contiguous transaction IDs
#[test]
fn test_visibility_sparse_transaction_ids() {
    let active_txns = vec![
        TransactionId::from(10),
        TransactionId::from(100),
        TransactionId::from(1000),
        TransactionId::from(10000),
    ];

    let snapshot = Snapshot::new(
        SnapshotId::from(1),
        "sparse".to_string(),
        LogSequenceNumber::from(100000),
        0,
        0,
        active_txns,
    );

    // Watermark should be 10 (minimum)
    // Transactions below 10 should be visible
    assert!(snapshot.is_visible(LogSequenceNumber::from(50000), TransactionId::from(1)));
    assert!(snapshot.is_visible(LogSequenceNumber::from(50000), TransactionId::from(9)));

    // Active transactions should NOT be visible
    assert!(!snapshot.is_visible(LogSequenceNumber::from(50000), TransactionId::from(10)));
    assert!(!snapshot.is_visible(LogSequenceNumber::from(50000), TransactionId::from(100)));
    assert!(!snapshot.is_visible(LogSequenceNumber::from(50000), TransactionId::from(1000)));
    assert!(!snapshot.is_visible(LogSequenceNumber::from(50000), TransactionId::from(10000)));

    // Gaps between active transactions should be visible
    assert!(snapshot.is_visible(LogSequenceNumber::from(50000), TransactionId::from(50)));
    assert!(snapshot.is_visible(LogSequenceNumber::from(50000), TransactionId::from(500)));
    assert!(snapshot.is_visible(LogSequenceNumber::from(50000), TransactionId::from(5000)));
    assert!(snapshot.is_visible(LogSequenceNumber::from(50000), TransactionId::from(20000)));
}

/// Test edge case: LSN exactly at snapshot boundary
#[test]
fn test_visibility_lsn_boundary() {
    let snapshot = Snapshot::new(
        SnapshotId::from(1),
        "boundary".to_string(),
        LogSequenceNumber::from(100),
        0,
        0,
        vec![TransactionId::from(5)],
    );

    // Version at exactly snapshot LSN should be visible
    assert!(snapshot.is_visible(LogSequenceNumber::from(100), TransactionId::from(1)));

    // Version after snapshot LSN should NOT be visible
    assert!(!snapshot.is_visible(LogSequenceNumber::from(101), TransactionId::from(1)));

    // Active transaction at snapshot LSN should NOT be visible
    assert!(!snapshot.is_visible(LogSequenceNumber::from(100), TransactionId::from(5)));
}

/// Test that watermark is correctly set to minimum active transaction
#[test]
fn test_watermark_is_minimum() {
    let active_txns = vec![
        TransactionId::from(50),
        TransactionId::from(10),
        TransactionId::from(30),
        TransactionId::from(20),
    ];

    let snapshot = Snapshot::new(
        SnapshotId::from(1),
        "watermark".to_string(),
        LogSequenceNumber::from(100),
        0,
        0,
        active_txns,
    );

    // Watermark should be 10 (minimum of active transactions)
    // All transactions < 10 should use fast path
    for i in 1..10 {
        assert!(
            snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(i)),
            "Transaction {} should be visible (below watermark)",
            i
        );
    }

    // All active transactions should NOT be visible
    assert!(!snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(10)));
    assert!(!snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(20)));
    assert!(!snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(30)));
    assert!(!snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(50)));
}

// Made with Bob
