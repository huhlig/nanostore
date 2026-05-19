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

//! MVCC tests for PagedRTree geospatial indexing.

use nanostore::pager::{Pager, PagerConfig};
use nanostore::snap::Snapshot;
use nanostore::table::rtree::{PagedRTree, SpatialConfig};
use nanostore::table::{GeoPoint, GeometryRef};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
use std::sync::Arc;

#[test]
fn test_rtree_mvcc_snapshot_isolation() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = SpatialConfig::default();
    let rtree = PagedRTree::new(TableId::from(1), "test_rtree".to_string(), pager, config).unwrap();

    let tx_id1 = TransactionId::from(1);
    let tx_id2 = TransactionId::from(2);

    // Transaction 1: Insert a point at (10.0, 20.0)
    let point1 = GeometryRef::Point(GeoPoint { x: 10.0, y: 20.0 });
    rtree.insert_geometry_tx(b"point1", point1, tx_id1).unwrap();

    // Commit transaction 1 at LSN 10
    rtree
        .commit_versions(tx_id1, LogSequenceNumber::from(10))
        .unwrap();

    // Create snapshot at LSN 10 (should see point1)
    let snapshot1 = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap1".to_string(),
        LogSequenceNumber::from(10),
        0,
        0,
        Vec::new(),
    );

    // Transaction 2: Insert a point at (15.0, 25.0)
    let point2 = GeometryRef::Point(GeoPoint { x: 15.0, y: 25.0 });
    rtree.insert_geometry_tx(b"point2", point2, tx_id2).unwrap();

    // Commit transaction 2 at LSN 20
    rtree
        .commit_versions(tx_id2, LogSequenceNumber::from(20))
        .unwrap();

    // Snapshot 1 should only see point1 (snapshot isolation)
    // Use a bounding box that covers both points
    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 0.0, y: 0.0 },
        max: GeoPoint { x: 100.0, y: 100.0 },
    };
    let results1 = rtree
        .search_intersects_snapshot(query.clone(), 10, &snapshot1)
        .unwrap();
    assert_eq!(results1.len(), 1, "Snapshot 1 should see only point1");
    assert_eq!(results1[0].id.0, b"point1");

    // Create new snapshot at LSN 20 (should see both points)
    let snapshot2 = Snapshot::new(
        nanostore::snap::SnapshotId::from(2),
        "snap2".to_string(),
        LogSequenceNumber::from(20),
        0,
        0,
        Vec::new(),
    );

    let results2 = rtree
        .search_intersects_snapshot(query.clone(), 10, &snapshot2)
        .unwrap();
    assert_eq!(results2.len(), 2, "Snapshot 2 should see both points");
}

#[test]
fn test_rtree_mvcc_multiple_versions() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = SpatialConfig::default();
    let rtree = PagedRTree::new(TableId::from(1), "test_rtree".to_string(), pager, config).unwrap();

    // Create multiple versions of geometries at different locations
    for i in 1..=5 {
        let tx_id = TransactionId::from(i);
        let x = 10.0 + (i as f64) * 5.0;
        let y = 20.0 + (i as f64) * 5.0;
        let point = GeometryRef::Point(GeoPoint { x, y });
        let id = format!("point{}", i);
        rtree
            .insert_geometry_tx(id.as_bytes(), point, tx_id)
            .unwrap();
        rtree
            .commit_versions(tx_id, LogSequenceNumber::from(i * 10))
            .unwrap();
    }

    // Each snapshot should see its corresponding set of points
    for i in 1..=5 {
        let snapshot = Snapshot::new(
            nanostore::snap::SnapshotId::from(i),
            format!("snap{}", i),
            LogSequenceNumber::from(i * 10),
            0,
            0,
            Vec::new(),
        );

        // Query a large area that should contain all points up to this version
        let query = GeometryRef::BoundingBox {
            min: GeoPoint { x: 0.0, y: 0.0 },
            max: GeoPoint { x: 50.0, y: 50.0 },
        };
        let results = rtree
            .search_intersects_snapshot(query, 10, &snapshot)
            .unwrap();
        assert_eq!(
            results.len(),
            i as usize,
            "Snapshot {} should see {} points",
            i,
            i
        );
    }
}

#[test]
fn test_rtree_mvcc_delete_creates_tombstone() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = SpatialConfig::default();
    let rtree = PagedRTree::new(TableId::from(1), "test_rtree".to_string(), pager, config).unwrap();

    let tx_id1 = TransactionId::from(1);
    let tx_id2 = TransactionId::from(2);

    // Transaction 1: Insert a point
    let point = GeometryRef::Point(GeoPoint { x: 10.0, y: 20.0 });
    rtree.insert_geometry_tx(b"point1", point, tx_id1).unwrap();
    rtree
        .commit_versions(tx_id1, LogSequenceNumber::from(10))
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

    // Verify point is visible
    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 5.0, y: 15.0 },
        max: GeoPoint { x: 15.0, y: 25.0 },
    };
    let results = rtree
        .search_intersects_snapshot(query.clone(), 10, &snapshot1)
        .unwrap();
    assert_eq!(results.len(), 1);

    // Transaction 2: Delete the point
    rtree.delete_geometry_tx(b"point1", tx_id2).unwrap();
    rtree
        .commit_versions(tx_id2, LogSequenceNumber::from(20))
        .unwrap();

    // Old snapshot should still see the point
    let results = rtree
        .search_intersects_snapshot(query.clone(), 10, &snapshot1)
        .unwrap();
    assert_eq!(
        results.len(),
        1,
        "Old snapshot should still see deleted point"
    );

    // New snapshot should not see the point
    let snapshot2 = Snapshot::new(
        nanostore::snap::SnapshotId::from(2),
        "snap2".to_string(),
        LogSequenceNumber::from(20),
        0,
        0,
        Vec::new(),
    );

    let results = rtree
        .search_intersects_snapshot(query.clone(), 10, &snapshot2)
        .unwrap();
    assert_eq!(
        results.len(),
        0,
        "New snapshot should not see deleted point"
    );
}

#[test]
fn test_rtree_mvcc_nearest_neighbor_snapshot() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = SpatialConfig::default();
    let rtree = PagedRTree::new(TableId::from(1), "test_rtree".to_string(), pager, config).unwrap();

    let tx_id1 = TransactionId::from(1);
    let tx_id2 = TransactionId::from(2);

    // Transaction 1: Insert points at (10, 10) and (20, 20)
    let point1 = GeometryRef::Point(GeoPoint { x: 10.0, y: 10.0 });
    let point2 = GeometryRef::Point(GeoPoint { x: 20.0, y: 20.0 });
    rtree.insert_geometry_tx(b"point1", point1, tx_id1).unwrap();
    rtree.insert_geometry_tx(b"point2", point2, tx_id1).unwrap();
    rtree
        .commit_versions(tx_id1, LogSequenceNumber::from(10))
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

    // Transaction 2: Insert a closer point at (5, 5)
    let point3 = GeometryRef::Point(GeoPoint { x: 5.0, y: 5.0 });
    rtree.insert_geometry_tx(b"point3", point3, tx_id2).unwrap();
    rtree
        .commit_versions(tx_id2, LogSequenceNumber::from(20))
        .unwrap();

    // Snapshot 1 should find point1 as nearest to origin
    let query_point = GeoPoint { x: 0.0, y: 0.0 };
    let results1 = rtree
        .search_nearest_snapshot(query_point, 1, &snapshot1)
        .unwrap();
    assert_eq!(results1.len(), 1);
    assert_eq!(results1[0].id.0, b"point1");

    // New snapshot should find point3 as nearest
    let snapshot2 = Snapshot::new(
        nanostore::snap::SnapshotId::from(2),
        "snap2".to_string(),
        LogSequenceNumber::from(20),
        0,
        0,
        Vec::new(),
    );

    let results2 = rtree
        .search_nearest_snapshot(query_point, 1, &snapshot2)
        .unwrap();
    assert_eq!(results2.len(), 1);
    assert_eq!(results2[0].id.0, b"point3");
}

#[test]
fn test_rtree_mvcc_vacuum() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = SpatialConfig::default();
    let rtree = PagedRTree::new(TableId::from(1), "test_rtree".to_string(), pager, config).unwrap();

    // Create multiple versions of the same geometry
    for i in 1..=5 {
        let tx_id = TransactionId::from(i);
        let x = 10.0 + (i as f64);
        let y = 20.0 + (i as f64);
        let point = GeometryRef::Point(GeoPoint { x, y });
        rtree.insert_geometry_tx(b"point1", point, tx_id).unwrap();
        rtree
            .commit_versions(tx_id, LogSequenceNumber::from(i * 10))
            .unwrap();
    }

    // Vacuum old versions (keep only versions >= LSN 40)
    let removed = rtree.vacuum(LogSequenceNumber::from(40)).unwrap();

    // Should have removed some old versions
    assert!(removed > 0, "Vacuum should remove old versions");

    // Latest version should still be accessible
    let snapshot = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap".to_string(),
        LogSequenceNumber::from(50),
        0,
        0,
        Vec::new(),
    );

    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 10.0, y: 20.0 },
        max: GeoPoint { x: 20.0, y: 30.0 },
    };
    let results = rtree
        .search_intersects_snapshot(query, 10, &snapshot)
        .unwrap();
    assert_eq!(results.len(), 1, "Latest version should still be visible");
    assert_eq!(results[0].id.0, b"point1");
}

#[test]
fn test_rtree_mvcc_uncommitted_not_visible() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = SpatialConfig::default();
    let rtree = PagedRTree::new(TableId::from(1), "test_rtree".to_string(), pager, config).unwrap();

    let tx_id = TransactionId::from(1);

    // Insert a point but don't commit it
    let point = GeometryRef::Point(GeoPoint { x: 10.0, y: 20.0 });
    rtree.insert_geometry_tx(b"point1", point, tx_id).unwrap();

    // Create a snapshot - uncommitted data should not be visible
    let snapshot = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap".to_string(),
        LogSequenceNumber::from(10),
        0,
        0,
        Vec::new(),
    );

    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 5.0, y: 15.0 },
        max: GeoPoint { x: 15.0, y: 25.0 },
    };
    let results = rtree
        .search_intersects_snapshot(query, 10, &snapshot)
        .unwrap();
    assert_eq!(results.len(), 0, "Uncommitted data should not be visible");
}

#[test]
fn test_rtree_mvcc_concurrent_transactions() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = SpatialConfig::default();
    let rtree = PagedRTree::new(TableId::from(1), "test_rtree".to_string(), pager, config).unwrap();

    let tx_id1 = TransactionId::from(1);
    let tx_id2 = TransactionId::from(2);
    let tx_id3 = TransactionId::from(3);

    // Transaction 1: Insert point1
    let point1 = GeometryRef::Point(GeoPoint { x: 10.0, y: 10.0 });
    rtree.insert_geometry_tx(b"point1", point1, tx_id1).unwrap();

    // Transaction 2: Insert point2 (before tx1 commits)
    let point2 = GeometryRef::Point(GeoPoint { x: 20.0, y: 20.0 });
    rtree.insert_geometry_tx(b"point2", point2, tx_id2).unwrap();

    // Commit tx1 at LSN 10
    rtree
        .commit_versions(tx_id1, LogSequenceNumber::from(10))
        .unwrap();

    // Snapshot at LSN 10 should only see point1
    let snapshot1 = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap1".to_string(),
        LogSequenceNumber::from(10),
        0,
        0,
        Vec::new(),
    );

    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 0.0, y: 0.0 },
        max: GeoPoint { x: 35.0, y: 35.0 },
    };
    let results1 = rtree
        .search_intersects_snapshot(query.clone(), 10, &snapshot1)
        .unwrap();
    assert_eq!(results1.len(), 1);
    assert_eq!(results1[0].id.0, b"point1");

    // Commit tx2 at LSN 20
    rtree
        .commit_versions(tx_id2, LogSequenceNumber::from(20))
        .unwrap();

    // Transaction 3: Insert point3
    let point3 = GeometryRef::Point(GeoPoint { x: 30.0, y: 30.0 });
    rtree.insert_geometry_tx(b"point3", point3, tx_id3).unwrap();
    rtree
        .commit_versions(tx_id3, LogSequenceNumber::from(30))
        .unwrap();

    // Snapshot at LSN 20 should see point1 and point2
    let snapshot2 = Snapshot::new(
        nanostore::snap::SnapshotId::from(2),
        "snap2".to_string(),
        LogSequenceNumber::from(20),
        0,
        0,
        Vec::new(),
    );

    let results2 = rtree
        .search_intersects_snapshot(query.clone(), 10, &snapshot2)
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

    let results3 = rtree
        .search_intersects_snapshot(query.clone(), 10, &snapshot3)
        .unwrap();
    assert_eq!(results3.len(), 3);
}

// Made with Bob
