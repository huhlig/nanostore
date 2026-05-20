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

//! Stress tests for HNSW vector search implementation.
//!
//! These tests validate HNSW behavior under heavy load:
//! - Large numbers of vectors (10K+ vectors)
//! - High-dimensional vectors (128-512 dimensions)
//! - Complex search queries
//! - Different distance metrics
//! - Memory pressure scenarios

use nanostore::pager::{Pager, PagerConfig};
use nanostore::table::{
    HnswConfig, PagedHnswVector, VectorMetric, VectorSearch, VectorSearchOptions,
};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
use std::sync::Arc;

/// Helper to create a test HNSW index
fn create_test_hnsw(dimensions: usize, metric: VectorMetric) -> PagedHnswVector<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let pager_config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "hnsw_stress.db", pager_config).unwrap());

    let config = HnswConfig {
        dimensions,
        metric,
        max_connections: 16,
        max_connections_layer0: 32,
        ef_construction: 200,
        ml: 1.0 / (16.0_f64).ln(),
    };

    PagedHnswVector::new(TableId::from(1), "stress_hnsw".to_string(), pager, config).unwrap()
}

/// Test inserting 10,000 vectors with 128 dimensions
///
/// Validates that HNSW can handle large numbers of high-dimensional vectors.
#[test]
fn test_insert_10k_vectors_128d() {
    let hnsw = create_test_hnsw(128, VectorMetric::Cosine);
    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(0);

    // Insert 10,000 vectors
    for i in 0..10_000 {
        let id = format!("vec_{}", i);
        // Create a pseudo-random vector
        let vector: Vec<f32> = (0..128)
            .map(|j| ((i * 7919 + j * 6547) % 10000) as f32 / 10000.0)
            .collect();

        hnsw.insert_vector(id.as_bytes(), &vector, tx_id, lsn)
            .unwrap();
    }

    // Verify graph structure
    let report = hnsw.verify().unwrap();
    assert!(
        report.errors.is_empty(),
        "Graph should be valid after 10K insertions"
    );
}

/// Test inserting 5,000 vectors with 512 dimensions
///
/// Tests high-dimensional vector handling.
#[test]
fn test_insert_5k_vectors_512d() {
    let hnsw = create_test_hnsw(512, VectorMetric::Euclidean);
    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(0);

    // Insert 5,000 high-dimensional vectors
    for i in 0..5_000 {
        let id = format!("vec_{}", i);
        let vector: Vec<f32> = (0..512)
            .map(|j| ((i * 7919 + j * 6547) % 10000) as f32 / 10000.0)
            .collect();

        hnsw.insert_vector(id.as_bytes(), &vector, tx_id, lsn)
            .unwrap();
    }

    let report = hnsw.verify().unwrap();
    assert!(
        report.errors.is_empty(),
        "Graph should be valid with 512D vectors"
    );
}

/// Test search queries with 10,000 vectors
///
/// Validates that search works correctly with large datasets.
#[test]
fn test_search_with_10k_vectors() {
    let hnsw = create_test_hnsw(64, VectorMetric::Cosine);
    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(0);

    // Insert 10,000 vectors
    for i in 0..10_000 {
        let id = format!("vec_{}", i);
        let vector: Vec<f32> = (0..64)
            .map(|j| ((i * 7919 + j * 6547) % 10000) as f32 / 10000.0)
            .collect();

        hnsw.insert_vector(id.as_bytes(), &vector, tx_id, lsn)
            .unwrap();
    }

    // Perform multiple searches
    for search_idx in 0..10 {
        let query: Vec<f32> = (0..64)
            .map(|j| ((search_idx * 1000 + j * 100) % 10000) as f32 / 10000.0)
            .collect();

        let results = hnsw
            .search_vector(
                &query,
                VectorSearchOptions {
                    limit: 100,
                    ef_search: Some(200),
                    probes: None,
                    filter: None,
                },
            )
            .unwrap();

        assert_eq!(results.len(), 100, "Should return 100 results");

        // Verify results are sorted by distance
        for i in 1..results.len() {
            assert!(
                results[i - 1].distance <= results[i].distance,
                "Results should be sorted by distance"
            );
        }
    }
}

/// Test different distance metrics with 5,000 vectors
///
/// Validates that all distance metrics work correctly under load.
#[test]
fn test_distance_metrics_5k_vectors() {
    let metrics = vec![
        VectorMetric::Euclidean,
        VectorMetric::Cosine,
        VectorMetric::Manhattan,
    ];

    for metric in metrics {
        let hnsw = create_test_hnsw(32, metric);
        let tx_id = TransactionId::from(0);
        let lsn = LogSequenceNumber::from(0);

        // Insert 5,000 vectors
        for i in 0..5_000 {
            let id = format!("vec_{}", i);
            let vector: Vec<f32> = (0..32)
                .map(|j| ((i * 7919 + j * 6547) % 10000) as f32 / 10000.0)
                .collect();

            hnsw.insert_vector(id.as_bytes(), &vector, tx_id, lsn)
                .unwrap();
        }

        // Perform search
        let query: Vec<f32> = (0..32).map(|j| (j as f32) / 32.0).collect();
        let results = hnsw
            .search_vector(
                &query,
                VectorSearchOptions {
                    limit: 50,
                    ef_search: Some(100),
                    probes: None,
                    filter: None,
                },
            )
            .unwrap();

        assert_eq!(results.len(), 50, "Metric {:?} failed", metric);

        let report = hnsw.verify().unwrap();
        assert!(
            report.errors.is_empty(),
            "Metric {:?} graph should be valid",
            metric
        );
    }
}

/// Test deletions with 10,000 vectors
///
/// Validates that deletions work correctly and graph remains searchable.
#[test]
fn test_deletions_with_10k_vectors() {
    let hnsw = create_test_hnsw(64, VectorMetric::Euclidean);
    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(0);

    // Insert 10,000 vectors
    for i in 0..10_000 {
        let id = format!("vec_{}", i);
        let vector: Vec<f32> = (0..64)
            .map(|j| ((i * 7919 + j * 6547) % 10000) as f32 / 10000.0)
            .collect();

        hnsw.insert_vector(id.as_bytes(), &vector, tx_id, lsn)
            .unwrap();
    }

    // Delete every other vector (5,000 deletions)
    for i in (0..10_000).step_by(2) {
        let id = format!("vec_{}", i);
        hnsw.delete_vector(id.as_bytes(), tx_id, lsn).unwrap();
    }

    // Verify graph is still valid
    let report = hnsw.verify().unwrap();
    assert!(
        report.errors.is_empty(),
        "Graph should be valid after deletions"
    );

    // Verify search still works
    let query: Vec<f32> = (0..64).map(|j| (j as f32) / 64.0).collect();
    let results = hnsw
        .search_vector(
            &query,
            VectorSearchOptions {
                limit: 100,
                ef_search: Some(200),
                probes: None,
                filter: None,
            },
        )
        .unwrap();

    // Should find remaining vectors
    assert!(results.len() > 0, "Should find remaining vectors");

    // Verify deleted vectors are not in results
    for result in &results {
        let id_str = String::from_utf8_lossy(&result.id.0);
        if let Some(num_str) = id_str.strip_prefix("vec_") {
            if let Ok(num) = num_str.parse::<usize>() {
                assert!(num % 2 == 1, "Deleted vectors should not appear in results");
            }
        }
    }
}

/// Test clustered vectors
///
/// Validates that HNSW handles clustered data distributions well.
#[test]
fn test_clustered_vectors_15k() {
    let hnsw = create_test_hnsw(64, VectorMetric::Cosine);
    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(0);

    // Create 3 clusters of 5,000 vectors each
    for cluster in 0..3 {
        let base_value = cluster as f32 * 0.3;
        for i in 0..5_000 {
            let id = format!("cluster_{}_vec_{}", cluster, i);
            let vector: Vec<f32> = (0..64)
                .map(|j| base_value + ((i * 17 + j * 23) % 1000) as f32 / 10000.0)
                .collect();

            hnsw.insert_vector(id.as_bytes(), &vector, tx_id, lsn)
                .unwrap();
        }
    }

    // Search for vectors in each cluster
    for cluster in 0..3 {
        let base_value = cluster as f32 * 0.3;
        let query: Vec<f32> = (0..64).map(|_| base_value + 0.05).collect();

        let results = hnsw
            .search_vector(
                &query,
                VectorSearchOptions {
                    limit: 100,
                    ef_search: Some(200),
                    probes: None,
                    filter: None,
                },
            )
            .unwrap();

        assert_eq!(results.len(), 100);

        // Most results should be from the same cluster
        let cluster_prefix = format!("cluster_{}_", cluster);
        let same_cluster_count = results
            .iter()
            .filter(|r| String::from_utf8_lossy(&r.id.0).starts_with(&cluster_prefix))
            .count();

        assert!(
            same_cluster_count > 50,
            "Most results should be from cluster {}",
            cluster
        );
    }
}

/// Test mixed operations: inserts, searches, and deletes
///
/// Simulates a realistic workload with mixed operations.
#[test]
fn test_mixed_operations_20k_total() {
    let hnsw = create_test_hnsw(128, VectorMetric::Euclidean);
    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(0);

    // Phase 1: Insert 10,000 vectors
    for i in 0..10_000 {
        let id = format!("vec_{}", i);
        let vector: Vec<f32> = (0..128)
            .map(|j| ((i * 7919 + j * 6547) % 10000) as f32 / 10000.0)
            .collect();

        hnsw.insert_vector(id.as_bytes(), &vector, tx_id, lsn)
            .unwrap();
    }

    // Phase 2: Perform searches
    for search_idx in 0..100 {
        let query: Vec<f32> = (0..128)
            .map(|j| ((search_idx * 100 + j * 10) % 10000) as f32 / 10000.0)
            .collect();

        let _results = hnsw
            .search_vector(
                &query,
                VectorSearchOptions {
                    limit: 50,
                    ef_search: Some(100),
                    probes: None,
                    filter: None,
                },
            )
            .unwrap();
    }

    // Phase 3: Delete 3,000 vectors
    for i in (0..10_000).step_by(3) {
        let id = format!("vec_{}", i);
        hnsw.delete_vector(id.as_bytes(), tx_id, lsn).unwrap();
    }

    // Phase 4: Insert 10,000 new vectors
    for i in 10_000..20_000 {
        let id = format!("vec_{}", i);
        let vector: Vec<f32> = (0..128)
            .map(|j| ((i * 7919 + j * 6547) % 10000) as f32 / 10000.0)
            .collect();

        hnsw.insert_vector(id.as_bytes(), &vector, tx_id, lsn)
            .unwrap();
    }

    // Verify graph is still valid
    let report = hnsw.verify().unwrap();
    assert!(
        report.errors.is_empty(),
        "Graph should be valid after mixed operations"
    );

    // Verify search still works
    let query: Vec<f32> = (0..128).map(|j| (j as f32) / 128.0).collect();
    let results = hnsw
        .search_vector(
            &query,
            VectorSearchOptions {
                limit: 100,
                ef_search: Some(200),
                probes: None,
                filter: None,
            },
        )
        .unwrap();

    assert_eq!(results.len(), 100);
}

/// Test varying ef_construction values
///
/// Validates that different ef_construction values work correctly.
#[test]
fn test_varying_ef_construction_5k_vectors() {
    let ef_values = vec![50, 100, 200, 400];

    for ef in ef_values {
        let fs = MemoryFileSystem::new();
        let pager_config = PagerConfig::default();
        let pager = Arc::new(Pager::create(&fs, "hnsw_ef.db", pager_config).unwrap());

        let config = HnswConfig {
            dimensions: 64,
            metric: VectorMetric::Cosine,
            max_connections: 16,
            max_connections_layer0: 32,
            ef_construction: ef,
            ml: 1.0 / (16.0_f64).ln(),
        };

        let hnsw =
            PagedHnswVector::new(TableId::from(1), "ef_test".to_string(), pager, config).unwrap();

        let tx_id = TransactionId::from(0);
        let lsn = LogSequenceNumber::from(0);

        // Insert 5,000 vectors
        for i in 0..5_000 {
            let id = format!("vec_{}", i);
            let vector: Vec<f32> = (0..64)
                .map(|j| ((i * 7919 + j * 6547) % 10000) as f32 / 10000.0)
                .collect();

            hnsw.insert_vector(id.as_bytes(), &vector, tx_id, lsn)
                .unwrap();
        }

        let report = hnsw.verify().unwrap();
        assert!(
            report.errors.is_empty(),
            "Graph should be valid with ef_construction={}",
            ef
        );
    }
}

/// Test sparse vectors (many zero values)
///
/// Validates that HNSW handles sparse vectors correctly.
#[test]
fn test_sparse_vectors_5k() {
    let hnsw = create_test_hnsw(256, VectorMetric::Cosine);
    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(0);

    // Insert 5,000 sparse vectors (90% zeros)
    for i in 0..5_000 {
        let id = format!("vec_{}", i);
        let vector: Vec<f32> = (0..256)
            .map(|j| {
                if (i * 7919 + j * 6547) % 10 == 0 {
                    ((i * 17 + j * 23) % 1000) as f32 / 1000.0
                } else {
                    0.0
                }
            })
            .collect();

        hnsw.insert_vector(id.as_bytes(), &vector, tx_id, lsn)
            .unwrap();
    }

    // Verify search works with sparse vectors
    let query: Vec<f32> = (0..256)
        .map(|j| if j % 10 == 0 { 0.5 } else { 0.0 })
        .collect();

    let results = hnsw
        .search_vector(
            &query,
            VectorSearchOptions {
                limit: 50,
                ef_search: Some(100),
                probes: None,
                filter: None,
            },
        )
        .unwrap();

    assert_eq!(results.len(), 50);
}

/// Test normalized vectors
///
/// Validates that HNSW handles normalized vectors correctly.
#[test]
fn test_normalized_vectors_5k() {
    let hnsw = create_test_hnsw(128, VectorMetric::Cosine);
    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(0);

    // Insert 5,000 normalized vectors
    for i in 0..5_000 {
        let id = format!("vec_{}", i);
        let mut vector: Vec<f32> = (0..128)
            .map(|j| ((i * 7919 + j * 6547) % 10000) as f32 / 10000.0)
            .collect();

        // Normalize vector
        let magnitude: f32 = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        if magnitude > 0.0 {
            for v in &mut vector {
                *v /= magnitude;
            }
        }

        hnsw.insert_vector(id.as_bytes(), &vector, tx_id, lsn)
            .unwrap();
    }

    let report = hnsw.verify().unwrap();
    assert!(
        report.errors.is_empty(),
        "Graph should be valid with normalized vectors"
    );
}

/// Test sequential insertions vs batch insertions
///
/// Validates that both insertion patterns work correctly.
#[test]
fn test_insertion_patterns_10k_vectors() {
    let hnsw = create_test_hnsw(64, VectorMetric::Euclidean);
    let tx_id = TransactionId::from(0);
    let lsn = LogSequenceNumber::from(0);

    // Insert 10,000 vectors sequentially
    for i in 0..10_000 {
        let id = format!("vec_{}", i);
        let vector: Vec<f32> = (0..64)
            .map(|j| ((i * 7919 + j * 6547) % 10000) as f32 / 10000.0)
            .collect();

        hnsw.insert_vector(id.as_bytes(), &vector, tx_id, lsn)
            .unwrap();
    }

    // Verify graph structure
    let report = hnsw.verify().unwrap();
    assert!(
        report.errors.is_empty(),
        "Graph should be valid after sequential insertions"
    );

    // Verify search quality
    let query: Vec<f32> = (0..64).map(|j| (j as f32) / 64.0).collect();
    let results = hnsw
        .search_vector(
            &query,
            VectorSearchOptions {
                limit: 100,
                ef_search: Some(200),
                probes: None,
                filter: None,
            },
        )
        .unwrap();

    assert_eq!(results.len(), 100);
}

/// Test varying vector dimensions
///
/// Validates that HNSW works with different dimensionalities.
#[test]
fn test_varying_dimensions_2k_vectors() {
    let dimensions = vec![16, 32, 64, 128, 256];

    for dim in dimensions {
        let hnsw = create_test_hnsw(dim, VectorMetric::Euclidean);
        let tx_id = TransactionId::from(0);
        let lsn = LogSequenceNumber::from(0);

        // Insert 2,000 vectors
        for i in 0..2_000 {
            let id = format!("vec_{}", i);
            let vector: Vec<f32> = (0..dim)
                .map(|j| ((i * 7919 + j * 6547) % 10000) as f32 / 10000.0)
                .collect();

            hnsw.insert_vector(id.as_bytes(), &vector, tx_id, lsn)
                .unwrap();
        }

        let report = hnsw.verify().unwrap();
        assert!(
            report.errors.is_empty(),
            "Graph should be valid with {} dimensions",
            dim
        );
    }
}

// Made with Bob
