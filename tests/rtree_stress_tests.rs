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

//! Stress tests for R-Tree geospatial indexing.
//!
//! These tests validate R-Tree behavior under heavy load:
//! - Large numbers of geometries (10K+ points)
//! - Complex spatial queries
//! - Dense spatial distributions
//! - Overlapping bounding boxes
//! - High-dimensional data

use nanostore::pager::{PageSize, Pager, PagerConfig};
use nanostore::table::{
    GeoPoint, GeoSpatial, GeometryRef, PagedRTree, SpatialConfig, SplitStrategy,
};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
use std::sync::Arc;

/// Helper to create a test R-Tree
fn create_test_rtree(config: SpatialConfig) -> PagedRTree<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let pager_config = PagerConfig::new()
        .with_page_size(PageSize::Size4KB)
        .with_cache_capacity(0);
    let pager = Arc::new(Pager::create(&fs, "rtree_stress.db", pager_config).unwrap());

    PagedRTree::new(TableId::from(1), "stress_rtree".to_string(), pager, config).unwrap()
}

/// Test inserting 10,000 points in a grid pattern
///
/// Validates that R-Tree can handle large numbers of points
/// and that spatial queries work correctly.
#[test]
fn test_insert_10k_grid_points() {
    let config = SpatialConfig::default().with_max_entries(64);
    let mut rtree = create_test_rtree(config);

    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(1);

    // Insert 10,000 points in a 100x100 grid
    for x in 0..100 {
        for y in 0..100 {
            let id = format!("point_{}_{}", x, y);
            let point = GeoPoint {
                x: x as f64,
                y: y as f64,
            };
            rtree
                .insert_geometry(id.as_bytes(), GeometryRef::Point(point), tx_id, lsn)
                .unwrap();
        }
    }

    // Verify stats
    let stats = rtree.stats().unwrap();
    assert_eq!(stats.entry_count, Some(10_000));

    // Query a region
    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 25.0, y: 25.0 },
        max: GeoPoint { x: 75.0, y: 75.0 },
    };
    let results = rtree.intersects(query, 5000).unwrap();
    assert!(results.len() >= 2500, "Should find at least 2500 points in 50x50 region");
}

/// Test inserting 25,000 random points
///
/// Random distribution creates more challenging tree structure
/// than grid patterns.
#[test]
fn test_insert_25k_random_points() {
    let config = SpatialConfig::default().with_max_entries(50);
    let mut rtree = create_test_rtree(config);

    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(1);

    // Insert 25,000 pseudo-random points
    for i in 0..25_000 {
        let id = format!("point_{}", i);
        // Use pseudo-random distribution
        let x = ((i * 7919) % 10000) as f64 / 100.0;
        let y = ((i * 6547) % 10000) as f64 / 100.0;
        let point = GeoPoint { x, y };
        rtree
            .insert_geometry(id.as_bytes(), GeometryRef::Point(point), tx_id, lsn)
            .unwrap();
    }

    let stats = rtree.stats().unwrap();
    assert_eq!(stats.entry_count, Some(25_000));

    // Query entire space
    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 0.0, y: 0.0 },
        max: GeoPoint { x: 100.0, y: 100.0 },
    };
    let results = rtree.intersects(query, 30_000).unwrap();
    assert_eq!(results.len(), 25_000);
}

/// Test inserting 5,000 overlapping bounding boxes
///
/// Overlapping boxes create complex spatial relationships
/// and stress the tree's ability to handle overlaps.
#[test]
fn test_insert_5k_overlapping_boxes() {
    let config = SpatialConfig::default().with_max_entries(64);
    let mut rtree = create_test_rtree(config);

    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(1);

    // Insert 5,000 overlapping bounding boxes
    for i in 0..5_000 {
        let id = format!("box_{}", i);
        let x = (i % 100) as f64;
        let y = (i / 100) as f64;
        let min = GeoPoint { x, y };
        let max = GeoPoint {
            x: x + 10.0,
            y: y + 10.0,
        };
        rtree
            .insert_geometry(id.as_bytes(), GeometryRef::BoundingBox { min, max }, tx_id, lsn)
            .unwrap();
    }

    let stats = rtree.stats().unwrap();
    assert_eq!(stats.entry_count, Some(5_000));

    // Query a region that should intersect many boxes
    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 45.0, y: 20.0 },
        max: GeoPoint { x: 55.0, y: 30.0 },
    };
    let results = rtree.intersects(query, 10_000).unwrap();
    assert!(results.len() > 100, "Should find many overlapping boxes");
}

/// Test nearest neighbor queries with 10,000 points
///
/// Validates that nearest neighbor search works correctly
/// with large datasets.
#[test]
fn test_nearest_neighbor_10k_points() {
    let config = SpatialConfig::default().with_max_entries(64);
    let mut rtree = create_test_rtree(config);

    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(1);

    // Insert 10,000 points in a grid
    for x in 0..100 {
        for y in 0..100 {
            let id = format!("point_{}_{}", x, y);
            let point = GeoPoint {
                x: x as f64,
                y: y as f64,
            };
            rtree
                .insert_geometry(id.as_bytes(), GeometryRef::Point(point), tx_id, lsn)
                .unwrap();
        }
    }

    // Find 100 nearest neighbors to center point
    let query_point = GeoPoint { x: 50.0, y: 50.0 };
    let results = rtree.nearest(query_point, 100).unwrap();

    assert_eq!(results.len(), 100);

    // Verify results are sorted by distance
    for i in 1..results.len() {
        assert!(
            results[i - 1].distance.unwrap() <= results[i].distance.unwrap(),
            "Results should be sorted by distance"
        );
    }

    // Verify closest point is at (50, 50)
    assert!(results[0].distance.unwrap() < 1.0);
}

/// Test dense clustering of points
///
/// Validates that R-Tree handles dense clusters efficiently.
#[test]
fn test_dense_clustering_20k_points() {
    let config = SpatialConfig::default().with_max_entries(64);
    let mut rtree = create_test_rtree(config);

    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(1);

    // Create 4 dense clusters of 5,000 points each
    let clusters = [(10.0, 10.0), (90.0, 10.0), (10.0, 90.0), (90.0, 90.0)];

    for (cluster_idx, (cx, cy)) in clusters.iter().enumerate() {
        for i in 0..5_000 {
            let id = format!("cluster_{}_point_{}", cluster_idx, i);
            // Points within 5 units of cluster center
            let x = cx + ((i * 17) % 1000) as f64 / 200.0 - 2.5;
            let y = cy + ((i * 23) % 1000) as f64 / 200.0 - 2.5;
            let point = GeoPoint { x, y };
            rtree
                .insert_geometry(id.as_bytes(), GeometryRef::Point(point), tx_id, lsn)
                .unwrap();
        }
    }

    let stats = rtree.stats().unwrap();
    assert_eq!(stats.entry_count, Some(20_000));

    // Query one cluster
    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 5.0, y: 5.0 },
        max: GeoPoint { x: 15.0, y: 15.0 },
    };
    let results = rtree.intersects(query, 10_000).unwrap();
    assert!(results.len() >= 4500, "Should find most points in cluster");
}

/// Test different split strategies under load
///
/// Validates that all split strategies work correctly with large datasets.
#[test]
fn test_split_strategies_with_5k_points() {
    let strategies = vec![
        SplitStrategy::Linear,
        SplitStrategy::Quadratic,
        SplitStrategy::RStar,
    ];

    for strategy in strategies {
        let config = SpatialConfig::default()
            .with_max_entries(64)
            .with_split_strategy(strategy);
        let mut rtree = create_test_rtree(config);

        let tx_id = TransactionId::from(0);
        let lsn = LogSequenceNumber::from(1);

        // Insert 5,000 points
        for i in 0..5_000 {
            let id = format!("point_{}", i);
            let x = ((i * 7919) % 1000) as f64 / 10.0;
            let y = ((i * 6547) % 1000) as f64 / 10.0;
            let point = GeoPoint { x, y };
            rtree
                .insert_geometry(id.as_bytes(), GeometryRef::Point(point), tx_id, lsn)
                .unwrap();
        }

        let stats = rtree.stats().unwrap();
        assert_eq!(
            stats.entry_count,
            Some(5_000),
            "Strategy {:?} failed",
            strategy
        );

        // Verify queries work
        let query = GeometryRef::BoundingBox {
            min: GeoPoint { x: 0.0, y: 0.0 },
            max: GeoPoint { x: 100.0, y: 100.0 },
        };
        let results = rtree.intersects(query, 10_000).unwrap();
        assert_eq!(results.len(), 5_000, "Strategy {:?} failed", strategy);
    }
}

/// Test deletions with large dataset
///
/// Validates that deletions work correctly and tree remains balanced.
#[test]
fn test_deletions_with_10k_points() {
    let config = SpatialConfig::default().with_max_entries(64);
    let mut rtree = create_test_rtree(config);

    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(1);

    // Insert 10,000 points
    for i in 0..10_000 {
        let id = format!("point_{}", i);
        let x = (i % 100) as f64;
        let y = (i / 100) as f64;
        let point = GeoPoint { x, y };
        rtree
            .insert_geometry(id.as_bytes(), GeometryRef::Point(point), tx_id, lsn)
            .unwrap();
    }

    // Delete every other point (5,000 deletions)
    for i in (0..10_000).step_by(2) {
        let id = format!("point_{}", i);
        rtree.delete_geometry(id.as_bytes(), tx_id, lsn).unwrap();
    }

    let stats = rtree.stats().unwrap();
    assert_eq!(stats.entry_count, Some(5_000));

    // Verify remaining points are accessible
    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 0.0, y: 0.0 },
        max: GeoPoint { x: 100.0, y: 100.0 },
    };
    let results = rtree.intersects(query, 10_000).unwrap();
    assert_eq!(results.len(), 5_000);
}

/// Test mixed operations: inserts, queries, and deletes
///
/// Simulates a realistic workload with mixed operations.
#[test]
fn test_mixed_operations_15k_total() {
    let config = SpatialConfig::default().with_max_entries(64);
    let mut rtree = create_test_rtree(config);

    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(1);

    // Phase 1: Insert 10,000 points
    for i in 0..10_000 {
        let id = format!("point_{}", i);
        let x = (i % 100) as f64;
        let y = (i / 100) as f64;
        let point = GeoPoint { x, y };
        rtree
            .insert_geometry(id.as_bytes(), GeometryRef::Point(point), tx_id, lsn)
            .unwrap();
    }

    // Phase 2: Query multiple regions
    for x in (0..100).step_by(20) {
        for y in (0..100).step_by(20) {
            let query = GeometryRef::BoundingBox {
                min: GeoPoint {
                    x: x as f64,
                    y: y as f64,
                },
                max: GeoPoint {
                    x: (x + 10) as f64,
                    y: (y + 10) as f64,
                },
            };
            let _results = rtree.intersects(query, 1000).unwrap();
        }
    }

    // Phase 3: Delete 3,000 points
    for i in (0..10_000).step_by(3) {
        let id = format!("point_{}", i);
        rtree.delete_geometry(id.as_bytes(), tx_id, lsn).unwrap();
    }

    // Phase 4: Insert 5,000 new points
    for i in 10_000..15_000 {
        let id = format!("point_{}", i);
        let x = ((i * 7919) % 10000) as f64 / 100.0;
        let y = ((i * 6547) % 10000) as f64 / 100.0;
        let point = GeoPoint { x, y };
        rtree
            .insert_geometry(id.as_bytes(), GeometryRef::Point(point), tx_id, lsn)
            .unwrap();
    }

    let stats = rtree.stats().unwrap();
    // 10,000 - 3,333 (deleted) + 5,000 (new) ≈ 11,667
    assert!(stats.entry_count.unwrap() > 11_000);
}

/// Test large bounding boxes
///
/// Validates that R-Tree handles large bounding boxes correctly.
#[test]
fn test_large_bounding_boxes_1k() {
    let config = SpatialConfig::default().with_max_entries(64);
    let mut rtree = create_test_rtree(config);

    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(1);

    // Insert 1,000 large bounding boxes
    for i in 0..1_000 {
        let id = format!("box_{}", i);
        let x = (i % 50) as f64 * 2.0;
        let y = (i / 50) as f64 * 2.0;
        let min = GeoPoint { x, y };
        let max = GeoPoint {
            x: x + 50.0,
            y: y + 50.0,
        };
        rtree
            .insert_geometry(id.as_bytes(), GeometryRef::BoundingBox { min, max }, tx_id, lsn)
            .unwrap();
    }

    let stats = rtree.stats().unwrap();
    assert_eq!(stats.entry_count, Some(1_000));

    // Query should find many overlapping boxes
    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 25.0, y: 25.0 },
        max: GeoPoint { x: 75.0, y: 75.0 },
    };
    let results = rtree.intersects(query, 2000).unwrap();
    assert!(results.len() > 100, "Should find many overlapping large boxes");
}

/// Test spatial queries at boundaries
///
/// Validates that boundary conditions are handled correctly.
#[test]
fn test_boundary_queries_5k_points() {
    let config = SpatialConfig::default().with_max_entries(64);
    let mut rtree = create_test_rtree(config);

    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(1);

    // Insert points across the entire space
    for i in 0..5_000 {
        let id = format!("point_{}", i);
        let x = ((i * 7919) % 10000) as f64 / 100.0;
        let y = ((i * 6547) % 10000) as f64 / 100.0;
        let point = GeoPoint { x, y };
        rtree
            .insert_geometry(id.as_bytes(), GeometryRef::Point(point), tx_id, lsn)
            .unwrap();
    }

    // Query at boundaries
    let queries = vec![
        // Left edge
        GeometryRef::BoundingBox {
            min: GeoPoint { x: 0.0, y: 0.0 },
            max: GeoPoint { x: 10.0, y: 100.0 },
        },
        // Right edge
        GeometryRef::BoundingBox {
            min: GeoPoint { x: 90.0, y: 0.0 },
            max: GeoPoint { x: 100.0, y: 100.0 },
        },
        // Top edge
        GeometryRef::BoundingBox {
            min: GeoPoint { x: 0.0, y: 90.0 },
            max: GeoPoint { x: 100.0, y: 100.0 },
        },
        // Bottom edge
        GeometryRef::BoundingBox {
            min: GeoPoint { x: 0.0, y: 0.0 },
            max: GeoPoint { x: 100.0, y: 10.0 },
        },
    ];

    for query in queries {
        let results = rtree.intersects(query, 1000).unwrap();
        assert!(results.len() > 0, "Should find points at boundaries");
    }
}

/// Test persistence with large dataset
///
/// Validates that large R-Trees can be persisted and reopened.
#[test]
fn test_persistence_with_5k_points() {
    let fs = MemoryFileSystem::new();
    let pager_config = PagerConfig::new()
        .with_page_size(PageSize::Size4KB)
        .with_cache_capacity(0);
    let pager = Arc::new(Pager::create(&fs, "rtree_persist.db", pager_config).unwrap());

    let config = SpatialConfig::default().with_max_entries(64);
    let root_page_id;

    // Create and populate tree
    {
        let mut rtree =
            PagedRTree::new(TableId::from(1), "persist_rtree".to_string(), pager.clone(), config.clone())
                .unwrap();

        root_page_id = rtree.root_page_id();

        let tx_id = TransactionId::from(0);
        let lsn = LogSequenceNumber::from(1);

        for i in 0..5_000 {
            let id = format!("point_{}", i);
            let x = (i % 100) as f64;
            let y = (i / 100) as f64;
            let point = GeoPoint { x, y };
            rtree
                .insert_geometry(id.as_bytes(), GeometryRef::Point(point), tx_id, lsn)
                .unwrap();
        }
    }

    // Reopen tree
    let pager2 = Arc::new(Pager::open(&fs, "rtree_persist.db").unwrap());
    let rtree2 = PagedRTree::open(
        TableId::from(1),
        "persist_rtree".to_string(),
        pager2,
        root_page_id,
        config,
    )
    .unwrap();

    // Verify data is still there
    let stats = rtree2.stats().unwrap();
    assert_eq!(stats.entry_count, Some(5_000));

    // Verify queries work
    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 0.0, y: 0.0 },
        max: GeoPoint { x: 100.0, y: 100.0 },
    };
    let results = rtree2.intersects(query, 10_000).unwrap();
    assert_eq!(results.len(), 5_000);
}

// Made with Bob