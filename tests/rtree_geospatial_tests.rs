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

//! Integration tests for R-Tree geospatial indexing.

use nanostore::pager::{PageSize, Pager, PagerConfig};
use nanostore::table::{
    GeoPoint, GeoSpatial, GeometryRef, PagedRTree, SpatialConfig, SplitStrategy,
};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
use std::sync::Arc;

fn create_test_pager(fs: &MemoryFileSystem, path: &str) -> Arc<Pager<MemoryFileSystem>> {
    let config = PagerConfig::new()
        .with_page_size(PageSize::Size4KB)
        .with_cache_capacity(0);
    Arc::new(Pager::create(fs, path, config).unwrap())
}

#[test]
fn test_rtree_create_and_insert_points() {
    let fs = MemoryFileSystem::new();
    let pager = create_test_pager(&fs, "/test_rtree_create_and_insert_points.db");

    let config = SpatialConfig::default();
    let mut rtree =
        PagedRTree::new(TableId::from(1), "test_rtree".to_string(), pager, config).unwrap();

    // Insert some points
    let points = vec![
        (b"point1".as_slice(), GeoPoint { x: 0.0, y: 0.0 }),
        (b"point2".as_slice(), GeoPoint { x: 1.0, y: 1.0 }),
        (b"point3".as_slice(), GeoPoint { x: 2.0, y: 2.0 }),
        (b"point4".as_slice(), GeoPoint { x: 3.0, y: 3.0 }),
    ];

    for (id, point) in &points {
        rtree
            .insert_geometry(
                id,
                GeometryRef::Point(*point),
                TransactionId::from(0),
                LogSequenceNumber::from(1),
            )
            .unwrap();
    }

    // Verify stats
    let stats = rtree.stats().unwrap();
    assert_eq!(stats.entry_count, Some(4));
}

#[test]
fn test_rtree_intersects_query() {
    let fs = MemoryFileSystem::new();
    let pager = create_test_pager(&fs, "/test_rtree_intersects_query.db");

    let config = SpatialConfig::default().with_max_entries(64);
    let mut rtree =
        PagedRTree::new(TableId::from(1), "test_rtree".to_string(), pager, config).unwrap();

    // Insert points in a grid
    for x in 0..10 {
        for y in 0..10 {
            let id = format!("point_{}_{}", x, y);
            let point = GeoPoint {
                x: x as f64,
                y: y as f64,
            };
            rtree
                .insert_geometry(
                    id.as_bytes(),
                    GeometryRef::Point(point),
                    TransactionId::from(0),
                    LogSequenceNumber::from(1),
                )
                .unwrap();
        }
    }

    // Query for points in a bounding box
    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 2.0, y: 2.0 },
        max: GeoPoint { x: 5.0, y: 5.0 },
    };

    let results = rtree.intersects(query, 100).unwrap();

    // Should find points in the range [2,5] x [2,5]
    // That's 4x4 = 16 points
    assert!(
        results.len() >= 16,
        "Expected at least 16 results, got {}",
        results.len()
    );
}

#[test]
fn test_rtree_nearest_query() {
    let fs = MemoryFileSystem::new();
    let pager = create_test_pager(&fs, "/test_rtree_nearest_query.db");

    let config = SpatialConfig::default();
    let mut rtree =
        PagedRTree::new(TableId::from(1), "test_rtree".to_string(), pager, config).unwrap();

    // Insert some scattered points
    let points = vec![
        (b"far1".as_slice(), GeoPoint { x: 100.0, y: 100.0 }),
        (b"near1".as_slice(), GeoPoint { x: 1.0, y: 1.0 }),
        (b"near2".as_slice(), GeoPoint { x: 2.0, y: 2.0 }),
        (b"far2".as_slice(), GeoPoint { x: 200.0, y: 200.0 }),
        (b"near3".as_slice(), GeoPoint { x: 1.5, y: 1.5 }),
    ];

    for (id, point) in &points {
        rtree
            .insert_geometry(
                id,
                GeometryRef::Point(*point),
                TransactionId::from(0),
                LogSequenceNumber::from(1),
            )
            .unwrap();
    }

    // Find 3 nearest points to origin
    let query_point = GeoPoint { x: 0.0, y: 0.0 };
    let results = rtree.nearest(query_point, 3).unwrap();

    assert_eq!(results.len(), 3);

    // Verify they are sorted by distance
    for i in 1..results.len() {
        assert!(
            results[i - 1].distance.unwrap() <= results[i].distance.unwrap(),
            "Results should be sorted by distance"
        );
    }

    // The nearest should be "near1" at (1, 1)
    assert!(results[0].distance.unwrap() < 2.0);
}

#[test]
fn test_rtree_split_strategies() {
    let strategies = vec![
        SplitStrategy::Linear,
        SplitStrategy::Quadratic,
        SplitStrategy::RStar,
    ];

    for strategy in strategies {
        let fs = MemoryFileSystem::new();
        let pager = create_test_pager(&fs, &format!("/test_rtree_split_{:?}.db", strategy));

        let config = SpatialConfig::default()
            .with_max_entries(64)
            .with_split_strategy(strategy);
        let mut rtree = PagedRTree::new(
            TableId::from(1),
            format!("test_rtree_{:?}", strategy),
            pager,
            config,
        )
        .unwrap();

        // Insert enough points to trigger splits
        for i in 0..200 {
            let id = format!("point_{}", i);
            let point = GeoPoint {
                x: (i % 20) as f64,
                y: (i / 20) as f64,
            };
            rtree
                .insert_geometry(
                    id.as_bytes(),
                    GeometryRef::Point(point),
                    TransactionId::from(0),
                    LogSequenceNumber::from(1),
                )
                .unwrap();
        }

        // Verify all points are still accessible
        let query = GeometryRef::BoundingBox {
            min: GeoPoint { x: 0.0, y: 0.0 },
            max: GeoPoint { x: 20.0, y: 20.0 },
        };

        let results = rtree.intersects(query, 300).unwrap();
        assert_eq!(results.len(), 200, "Strategy {:?} failed", strategy);
    }
}

#[test]
fn test_rtree_bounding_box_insert() {
    let fs = MemoryFileSystem::new();
    let pager = create_test_pager(&fs, "/test_rtree_bounding_box_insert.db");

    let config = SpatialConfig::default().with_max_entries(64);
    let mut rtree =
        PagedRTree::new(TableId::from(1), "test_rtree".to_string(), pager, config).unwrap();

    // Insert bounding boxes
    let boxes = vec![
        (
            b"box1".as_slice(),
            GeoPoint { x: 0.0, y: 0.0 },
            GeoPoint { x: 2.0, y: 2.0 },
        ),
        (
            b"box2".as_slice(),
            GeoPoint { x: 3.0, y: 3.0 },
            GeoPoint { x: 5.0, y: 5.0 },
        ),
        (
            b"box3".as_slice(),
            GeoPoint { x: 1.0, y: 1.0 },
            GeoPoint { x: 4.0, y: 4.0 },
        ),
    ];

    for (id, min, max) in &boxes {
        rtree
            .insert_geometry(
                id,
                GeometryRef::BoundingBox {
                    min: *min,
                    max: *max,
                },
                TransactionId::from(0),
                LogSequenceNumber::from(1),
            )
            .unwrap();
    }

    // Query for intersecting boxes
    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 2.5, y: 2.5 },
        max: GeoPoint { x: 3.5, y: 3.5 },
    };

    let results = rtree.intersects(query, 10).unwrap();

    // Should find box2 and box3
    assert!(results.len() >= 2, "Expected at least 2 intersecting boxes");
}

#[test]
fn test_rtree_3d_support() {
    let fs = MemoryFileSystem::new();
    let pager = create_test_pager(&fs, "/test_rtree_3d_support.db");

    let config = SpatialConfig::new(3); // 3D
    let mut rtree =
        PagedRTree::new(TableId::from(1), "test_rtree_3d".to_string(), pager, config).unwrap();

    // Insert 3D points (using 2D interface for now, z=0)
    for i in 0..10 {
        let id = format!("point_{}", i);
        let point = GeoPoint {
            x: i as f64,
            y: i as f64,
        };
        rtree
            .insert_geometry(
                id.as_bytes(),
                GeometryRef::Point(point),
                TransactionId::from(0),
                LogSequenceNumber::from(1),
            )
            .unwrap();
    }

    let stats = rtree.stats().unwrap();
    assert_eq!(stats.entry_count, Some(10));
}

#[test]
fn test_rtree_large_dataset() {
    let fs = MemoryFileSystem::new();
    let pager = create_test_pager(&fs, "/test_rtree_large_dataset.db");

    let config = SpatialConfig::default().with_max_entries(50);
    let mut rtree = PagedRTree::new(
        TableId::from(1),
        "test_rtree_large".to_string(),
        pager,
        config,
    )
    .unwrap();

    // Insert 1000 random-ish points
    for i in 0..1000 {
        let id = format!("point_{}", i);
        let point = GeoPoint {
            x: ((i * 17) % 100) as f64,
            y: ((i * 23) % 100) as f64,
        };
        rtree
            .insert_geometry(
                id.as_bytes(),
                GeometryRef::Point(point),
                TransactionId::from(0),
                LogSequenceNumber::from(1),
            )
            .unwrap();
    }

    // Verify we can query them
    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 0.0, y: 0.0 },
        max: GeoPoint { x: 100.0, y: 100.0 },
    };

    let results = rtree.intersects(query, 2000).unwrap();
    assert_eq!(results.len(), 1000);
}

#[test]
fn test_rtree_empty_queries() {
    let fs = MemoryFileSystem::new();
    let pager = create_test_pager(&fs, "/test_rtree_empty_queries.db");

    let config = SpatialConfig::default();
    let mut rtree =
        PagedRTree::new(TableId::from(1), "test_rtree".to_string(), pager, config).unwrap();

    // Insert points in one area
    for i in 0..10 {
        let id = format!("point_{}", i);
        let point = GeoPoint {
            x: i as f64,
            y: i as f64,
        };
        rtree
            .insert_geometry(
                id.as_bytes(),
                GeometryRef::Point(point),
                TransactionId::from(0),
                LogSequenceNumber::from(1),
            )
            .unwrap();
    }

    // Query in a different area
    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 100.0, y: 100.0 },
        max: GeoPoint { x: 200.0, y: 200.0 },
    };

    let results = rtree.intersects(query, 100).unwrap();
    assert_eq!(results.len(), 0, "Should find no results in empty area");
}

#[test]
fn test_rtree_config_validation() {
    let mut config = SpatialConfig::default();

    // Valid config
    assert!(config.validate().is_ok());

    // Invalid dimensions
    config.dimensions = 1;
    assert!(config.validate().is_err());

    config.dimensions = 4;
    assert!(config.validate().is_err());

    // Invalid entries
    config.dimensions = 2;
    config.max_entries_per_node = 2;
    assert!(config.validate().is_err());

    config.max_entries_per_node = 10;
    config.min_entries_per_node = 10;
    assert!(config.validate().is_err());
}

#[test]
fn test_rtree_persistence() {
    let fs = MemoryFileSystem::new();
    let pager = create_test_pager(&fs, "/test_rtree_persistence.db");

    let config = SpatialConfig::default();
    let root_page_id;

    // Create and populate tree
    {
        let mut rtree = PagedRTree::new(
            TableId::from(1),
            "test_rtree".to_string(),
            pager.clone(),
            config.clone(),
        )
        .unwrap();

        root_page_id = rtree.root_page_id();

        for i in 0..50 {
            let id = format!("point_{}", i);
            let point = GeoPoint {
                x: i as f64,
                y: i as f64,
            };
            rtree
                .insert_geometry(
                    id.as_bytes(),
                    GeometryRef::Point(point),
                    TransactionId::from(0),
                    LogSequenceNumber::from(1),
                )
                .unwrap();
        }
    }

    // Reopen tree
    let pager2 = Arc::new(Pager::open(&fs, "/test_rtree_persistence.db").unwrap());
    let rtree2 = PagedRTree::open(
        TableId::from(1),
        "test_rtree".to_string(),
        pager2,
        root_page_id,
        config,
    )
    .unwrap();

    // Verify data is still there
    let stats = rtree2.stats().unwrap();
    assert_eq!(stats.entry_count, Some(50));
}

#[test]
fn test_rtree_delete_geometry_removes_entry() {
    let fs = MemoryFileSystem::new();
    let pager = create_test_pager(&fs, "/test_rtree_delete_geometry_removes_entry.db");

    let config = SpatialConfig::default();
    let mut rtree = PagedRTree::new(
        TableId::from(1),
        "test_rtree_delete".to_string(),
        pager,
        config,
    )
    .unwrap();

    rtree
        .insert_geometry(
            b"point1",
            GeometryRef::Point(GeoPoint { x: 1.0, y: 1.0 }),
            TransactionId::from(0),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    rtree
        .insert_geometry(
            b"point2",
            GeometryRef::Point(GeoPoint { x: 2.0, y: 2.0 }),
            TransactionId::from(0),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    rtree
        .delete_geometry(
            b"point1",
            TransactionId::from(0),
            LogSequenceNumber::from(1),
        )
        .unwrap();

    let stats = rtree.stats().unwrap();
    assert_eq!(stats.entry_count, Some(1));

    let results = rtree
        .intersects(
            GeometryRef::BoundingBox {
                min: GeoPoint { x: 0.0, y: 0.0 },
                max: GeoPoint { x: 3.0, y: 3.0 },
            },
            10,
        )
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id.as_ref(), b"point2");
}

#[test]
fn test_rtree_delete_geometry_missing_id_is_noop() {
    let fs = MemoryFileSystem::new();
    let pager = create_test_pager(&fs, "/test_rtree_delete_geometry_missing_id_is_noop.db");

    let config = SpatialConfig::default();
    let mut rtree = PagedRTree::new(
        TableId::from(1),
        "test_rtree_delete_missing".to_string(),
        pager,
        config,
    )
    .unwrap();

    rtree
        .insert_geometry(
            b"point1",
            GeometryRef::Point(GeoPoint { x: 1.0, y: 1.0 }),
            TransactionId::from(0),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    rtree
        .delete_geometry(
            b"missing",
            TransactionId::from(0),
            LogSequenceNumber::from(1),
        )
        .unwrap();

    let stats = rtree.stats().unwrap();
    assert_eq!(stats.entry_count, Some(1));
}

#[test]
fn test_rtree_delete_triggers_underflow_reinsertion() {
    let fs = MemoryFileSystem::new();
    let pager = create_test_pager(&fs, "/test_rtree_delete_triggers_underflow_reinsertion.db");

    let config = SpatialConfig::new(2).with_max_entries(5);
    let mut rtree = PagedRTree::new(
        TableId::from(1),
        "test_rtree_delete_underflow".to_string(),
        pager,
        config,
    )
    .unwrap();

    for i in 0..12 {
        let id = format!("point_{}", i);
        let point = GeoPoint {
            x: i as f64,
            y: (i % 3) as f64,
        };
        rtree
            .insert_geometry(
                id.as_bytes(),
                GeometryRef::Point(point),
                TransactionId::from(0),
                LogSequenceNumber::from(1),
            )
            .unwrap();
    }

    for id in [
        b"point_0".as_slice(),
        b"point_1".as_slice(),
        b"point_2".as_slice(),
    ] {
        rtree
            .delete_geometry(id, TransactionId::from(0), LogSequenceNumber::from(1))
            .unwrap();
    }

    let stats = rtree.stats().unwrap();
    assert_eq!(stats.entry_count, Some(9));

    let results = rtree
        .intersects(
            GeometryRef::BoundingBox {
                min: GeoPoint { x: -1.0, y: -1.0 },
                max: GeoPoint { x: 20.0, y: 20.0 },
            },
            20,
        )
        .unwrap();

    assert_eq!(results.len(), 9);
    assert!(!results.iter().any(|hit| hit.id.as_ref() == b"point_0"));
    assert!(!results.iter().any(|hit| hit.id.as_ref() == b"point_1"));
    assert!(!results.iter().any(|hit| hit.id.as_ref() == b"point_2"));
}

// Made with Bob
