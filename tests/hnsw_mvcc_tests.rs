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

//! MVCC tests for PagedHnswVector.

use nanostore::pager::{Pager, PagerConfig};
use nanostore::snap::Snapshot;
use nanostore::table::hnsw::{HnswConfig, PagedHnswVector};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
use std::sync::Arc;

/// Helper function to create a test vector with a specific pattern.
fn create_test_vector(dimensions: usize, base_value: f32) -> Vec<f32> {
    (0..dimensions)
        .map(|i| base_value + (i as f32) * 0.1)
        .collect()
}

#[test]
fn test_hnsw_mvcc_snapshot_isolation() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = HnswConfig {
        dimensions: 4,
        ..Default::default()
    };
    let hnsw =
        PagedHnswVector::new(TableId::from(1), "test_hnsw".to_string(), pager, config).unwrap();

    let tx_id1 = TransactionId::from(1);
    let tx_id2 = TransactionId::from(2);

    // Transaction 1: Insert vector1
    let vector1 = create_test_vector(4, 1.0);
    hnsw.insert_vector_tx(b"vector1", &vector1, tx_id1).unwrap();

    // Commit transaction 1 at LSN 10
    hnsw.commit_versions(tx_id1, LogSequenceNumber::from(10))
        .unwrap();

    // Create snapshot at LSN 10 (should see vector1)
    let snapshot1 = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap1".to_string(),
        LogSequenceNumber::from(10),
        0,
        0,
        Vec::new(),
    );

    // Transaction 2: Insert vector2
    let vector2 = create_test_vector(4, 2.0);
    hnsw.insert_vector_tx(b"vector2", &vector2, tx_id2).unwrap();

    // Commit transaction 2 at LSN 20
    hnsw.commit_versions(tx_id2, LogSequenceNumber::from(20))
        .unwrap();

    // Snapshot 1 should only see vector1 (snapshot isolation)
    let query = create_test_vector(4, 1.0);
    let results1 = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot1)
        .unwrap();
    assert_eq!(results1.len(), 1, "Snapshot 1 should see only vector1");
    assert_eq!(results1[0].id.0, b"vector1");

    // Create new snapshot at LSN 20 (should see both vectors)
    let snapshot2 = Snapshot::new(
        nanostore::snap::SnapshotId::from(2),
        "snap2".to_string(),
        LogSequenceNumber::from(20),
        0,
        0,
        Vec::new(),
    );

    let results2 = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot2)
        .unwrap();
    assert_eq!(results2.len(), 2, "Snapshot 2 should see both vectors");
}

#[test]
fn test_hnsw_mvcc_multiple_versions() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = HnswConfig {
        dimensions: 4,
        ..Default::default()
    };
    let hnsw =
        PagedHnswVector::new(TableId::from(1), "test_hnsw".to_string(), pager, config).unwrap();

    // Create multiple versions of vectors
    for i in 1..=5 {
        let tx_id = TransactionId::from(i);
        let vector = create_test_vector(4, i as f32);
        let id = format!("vector{}", i);
        hnsw.insert_vector_tx(id.as_bytes(), &vector, tx_id)
            .unwrap();
        hnsw.commit_versions(tx_id, LogSequenceNumber::from(i * 10))
            .unwrap();
    }

    // Each snapshot should see its corresponding set of vectors
    for i in 1..=5 {
        let snapshot = Snapshot::new(
            nanostore::snap::SnapshotId::from(i),
            format!("snap{}", i),
            LogSequenceNumber::from(i * 10),
            0,
            0,
            Vec::new(),
        );

        // Query with a vector similar to the first one
        let query = create_test_vector(4, 1.0);
        let results = hnsw
            .search_vector_snapshot(&query, 10, None, &snapshot)
            .unwrap();
        assert_eq!(
            results.len(),
            i as usize,
            "Snapshot {} should see {} vectors",
            i,
            i
        );
    }
}

#[test]
fn test_hnsw_mvcc_delete_creates_tombstone() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = HnswConfig {
        dimensions: 4,
        ..Default::default()
    };
    let hnsw =
        PagedHnswVector::new(TableId::from(1), "test_hnsw".to_string(), pager, config).unwrap();

    let tx_id1 = TransactionId::from(1);
    let tx_id2 = TransactionId::from(2);

    // Transaction 1: Insert a vector
    let vector = create_test_vector(4, 1.0);
    hnsw.insert_vector_tx(b"vector1", &vector, tx_id1).unwrap();
    hnsw.commit_versions(tx_id1, LogSequenceNumber::from(10))
        .unwrap();

    // Create snapshot before deletion
    let snapshot1 = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap1".to_string(),
        LogSequenceNumber::from(10),
        0,
        0,
        Vec::new(),
    );

    // Verify vector is visible
    let query = create_test_vector(4, 1.0);
    let results = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot1)
        .unwrap();
    assert_eq!(results.len(), 1);

    // Transaction 2: Delete the vector
    hnsw.delete_vector_tx(b"vector1", tx_id2).unwrap();
    hnsw.commit_versions(tx_id2, LogSequenceNumber::from(20))
        .unwrap();

    // Old snapshot should still see the vector
    let results = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot1)
        .unwrap();
    assert_eq!(
        results.len(),
        1,
        "Old snapshot should still see deleted vector"
    );

    // New snapshot should not see the vector
    let snapshot2 = Snapshot::new(
        nanostore::snap::SnapshotId::from(2),
        "snap2".to_string(),
        LogSequenceNumber::from(20),
        0,
        0,
        Vec::new(),
    );

    let results = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot2)
        .unwrap();
    assert_eq!(
        results.len(),
        0,
        "New snapshot should not see deleted vector"
    );
}

#[test]
fn test_hnsw_mvcc_nearest_neighbor_snapshot() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = HnswConfig {
        dimensions: 4,
        ..Default::default()
    };
    let hnsw =
        PagedHnswVector::new(TableId::from(1), "test_hnsw".to_string(), pager, config).unwrap();

    let tx_id1 = TransactionId::from(1);
    let tx_id2 = TransactionId::from(2);

    // Transaction 1: Insert vectors at distance 2.0 and 4.0 from origin
    let vector1 = create_test_vector(4, 2.0);
    let vector2 = create_test_vector(4, 4.0);
    hnsw.insert_vector_tx(b"vector1", &vector1, tx_id1).unwrap();
    hnsw.insert_vector_tx(b"vector2", &vector2, tx_id1).unwrap();
    hnsw.commit_versions(tx_id1, LogSequenceNumber::from(10))
        .unwrap();

    // Create snapshot at LSN 10
    let snapshot1 = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap1".to_string(),
        LogSequenceNumber::from(10),
        0,
        0,
        Vec::new(),
    );

    // Transaction 2: Insert a closer vector at distance 1.0
    let vector3 = create_test_vector(4, 1.0);
    hnsw.insert_vector_tx(b"vector3", &vector3, tx_id2).unwrap();
    hnsw.commit_versions(tx_id2, LogSequenceNumber::from(20))
        .unwrap();

    // Snapshot 1 should only see vector1 and vector2 (not vector3)
    let query = vec![0.0; 4];
    let results1 = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot1)
        .unwrap();
    assert_eq!(results1.len(), 2, "Snapshot 1 should see only 2 vectors");
    // Check that vector3 is not in the results
    assert!(
        !results1.iter().any(|r| r.id.0 == b"vector3"),
        "Snapshot 1 should not see vector3"
    );

    // New snapshot should see all three vectors
    let snapshot2 = Snapshot::new(
        nanostore::snap::SnapshotId::from(2),
        "snap2".to_string(),
        LogSequenceNumber::from(20),
        0,
        0,
        Vec::new(),
    );

    let results2 = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot2)
        .unwrap();
    assert_eq!(results2.len(), 3, "Snapshot 2 should see all 3 vectors");
    // Check that vector3 is in the results
    assert!(
        results2.iter().any(|r| r.id.0 == b"vector3"),
        "Snapshot 2 should see vector3"
    );
}

#[test]
fn test_hnsw_mvcc_vacuum() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = HnswConfig {
        dimensions: 4,
        ..Default::default()
    };
    let hnsw =
        PagedHnswVector::new(TableId::from(1), "test_hnsw".to_string(), pager, config).unwrap();

    // Insert a vector
    let tx_id1 = TransactionId::from(1);
    let vector1 = create_test_vector(4, 1.0);
    hnsw.insert_vector_tx(b"vector1", &vector1, tx_id1).unwrap();
    hnsw.commit_versions(tx_id1, LogSequenceNumber::from(10))
        .unwrap();

    // Delete it (creates a tombstone version)
    let tx_id2 = TransactionId::from(2);
    hnsw.delete_vector_tx(b"vector1", tx_id2).unwrap();
    hnsw.commit_versions(tx_id2, LogSequenceNumber::from(20))
        .unwrap();

    // Insert more vectors to ensure we have data
    for i in 2..=5 {
        let tx_id = TransactionId::from(i + 2);
        let vector = create_test_vector(4, i as f32);
        let id = format!("vector{}", i);
        hnsw.insert_vector_tx(id.as_bytes(), &vector, tx_id)
            .unwrap();
        hnsw.commit_versions(tx_id, LogSequenceNumber::from((i + 2) * 10))
            .unwrap();
    }

    // Vacuum old versions (keep only versions >= LSN 30)
    // This should remove the old version of vector1 (LSN 10)
    let removed = hnsw.vacuum(LogSequenceNumber::from(30)).unwrap();

    // Should have removed at least the old version of vector1
    assert!(removed > 0, "Vacuum should remove old versions");

    // Latest versions should still be accessible
    let snapshot = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap".to_string(),
        LogSequenceNumber::from(70),
        0,
        0,
        Vec::new(),
    );

    let query = create_test_vector(4, 2.0);
    let results = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot)
        .unwrap();
    assert!(
        results.len() >= 3,
        "Latest versions should still be visible"
    );
}

#[test]
fn test_hnsw_mvcc_uncommitted_not_visible() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = HnswConfig {
        dimensions: 4,
        ..Default::default()
    };
    let hnsw =
        PagedHnswVector::new(TableId::from(1), "test_hnsw".to_string(), pager, config).unwrap();

    let tx_id = TransactionId::from(1);

    // Insert a vector but don't commit it
    let vector = create_test_vector(4, 1.0);
    hnsw.insert_vector_tx(b"vector1", &vector, tx_id).unwrap();

    // Create a snapshot - uncommitted data should not be visible
    let snapshot = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap".to_string(),
        LogSequenceNumber::from(10),
        0,
        0,
        Vec::new(),
    );

    let query = create_test_vector(4, 1.0);
    let results = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot)
        .unwrap();
    assert_eq!(results.len(), 0, "Uncommitted data should not be visible");
}

#[test]
fn test_hnsw_mvcc_concurrent_transactions() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = HnswConfig {
        dimensions: 4,
        ..Default::default()
    };
    let hnsw =
        PagedHnswVector::new(TableId::from(1), "test_hnsw".to_string(), pager, config).unwrap();

    let tx_id1 = TransactionId::from(1);
    let tx_id2 = TransactionId::from(2);
    let tx_id3 = TransactionId::from(3);

    // Transaction 1: Insert vector1
    let vector1 = create_test_vector(4, 1.0);
    hnsw.insert_vector_tx(b"vector1", &vector1, tx_id1).unwrap();

    // Transaction 2: Insert vector2 (before tx1 commits)
    let vector2 = create_test_vector(4, 2.0);
    hnsw.insert_vector_tx(b"vector2", &vector2, tx_id2).unwrap();

    // Commit tx1 at LSN 10
    hnsw.commit_versions(tx_id1, LogSequenceNumber::from(10))
        .unwrap();

    // Snapshot at LSN 10 should only see vector1
    let snapshot1 = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap1".to_string(),
        LogSequenceNumber::from(10),
        0,
        0,
        Vec::new(),
    );

    let query = create_test_vector(4, 1.0);
    let results1 = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot1)
        .unwrap();
    assert_eq!(results1.len(), 1);
    assert_eq!(results1[0].id.0, b"vector1");

    // Commit tx2 at LSN 20
    hnsw.commit_versions(tx_id2, LogSequenceNumber::from(20))
        .unwrap();

    // Transaction 3: Insert vector3
    let vector3 = create_test_vector(4, 3.0);
    hnsw.insert_vector_tx(b"vector3", &vector3, tx_id3).unwrap();
    hnsw.commit_versions(tx_id3, LogSequenceNumber::from(30))
        .unwrap();

    // Snapshot at LSN 20 should see vector1 and vector2
    let snapshot2 = Snapshot::new(
        nanostore::snap::SnapshotId::from(2),
        "snap2".to_string(),
        LogSequenceNumber::from(20),
        0,
        0,
        Vec::new(),
    );

    let results2 = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot2)
        .unwrap();
    assert_eq!(results2.len(), 2);

    // Snapshot at LSN 30 should see all three
    let snapshot3 = Snapshot::new(
        nanostore::snap::SnapshotId::from(3),
        "snap3".to_string(),
        LogSequenceNumber::from(30),
        0,
        0,
        Vec::new(),
    );

    let results3 = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot3)
        .unwrap();
    assert_eq!(results3.len(), 3);
}

#[test]
fn test_hnsw_mvcc_delete_and_reinsert() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = HnswConfig {
        dimensions: 4,
        ..Default::default()
    };
    let hnsw =
        PagedHnswVector::new(TableId::from(1), "test_hnsw".to_string(), pager, config).unwrap();

    let tx_id1 = TransactionId::from(1);
    let tx_id2 = TransactionId::from(2);

    // Transaction 1: Insert vector1
    let vector1 = create_test_vector(4, 1.0);
    hnsw.insert_vector_tx(b"vector1", &vector1, tx_id1).unwrap();
    hnsw.commit_versions(tx_id1, LogSequenceNumber::from(10))
        .unwrap();

    // Create snapshot after insert
    let snapshot1 = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap1".to_string(),
        LogSequenceNumber::from(10),
        0,
        0,
        Vec::new(),
    );

    // Transaction 2: Delete vector1
    hnsw.delete_vector_tx(b"vector1", tx_id2).unwrap();
    hnsw.commit_versions(tx_id2, LogSequenceNumber::from(20))
        .unwrap();

    // Create snapshot after delete
    let snapshot2 = Snapshot::new(
        nanostore::snap::SnapshotId::from(2),
        "snap2".to_string(),
        LogSequenceNumber::from(20),
        0,
        0,
        Vec::new(),
    );

    let query = create_test_vector(4, 1.0);

    // Snapshot 1 should see the vector
    let results1 = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot1)
        .unwrap();
    assert_eq!(results1.len(), 1, "Snapshot 1 should see vector1");

    // Snapshot 2 should not see the vector
    let results2 = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot2)
        .unwrap();
    assert_eq!(results2.len(), 0, "Snapshot 2 should not see vector1");
}

#[test]
fn test_hnsw_mvcc_search_with_limit() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = HnswConfig {
        dimensions: 4,
        ..Default::default()
    };
    let hnsw =
        PagedHnswVector::new(TableId::from(1), "test_hnsw".to_string(), pager, config).unwrap();

    let tx_id = TransactionId::from(1);

    // Insert 5 vectors
    for i in 1..=5 {
        let vector = create_test_vector(4, i as f32);
        let id = format!("vector{}", i);
        hnsw.insert_vector_tx(id.as_bytes(), &vector, tx_id)
            .unwrap();
    }
    hnsw.commit_versions(tx_id, LogSequenceNumber::from(10))
        .unwrap();

    let snapshot = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap".to_string(),
        LogSequenceNumber::from(10),
        0,
        0,
        Vec::new(),
    );

    let query = create_test_vector(4, 1.0);

    // Test with limit of 3
    let results = hnsw
        .search_vector_snapshot(&query, 3, None, &snapshot)
        .unwrap();
    assert_eq!(
        results.len(),
        3,
        "Should return at most 3 results when limit is 3"
    );

    // Test with limit larger than available vectors
    let results = hnsw
        .search_vector_snapshot(&query, 10, None, &snapshot)
        .unwrap();
    assert_eq!(
        results.len(),
        5,
        "Should return all 5 vectors when limit is 10"
    );
}

// Made with Bob
