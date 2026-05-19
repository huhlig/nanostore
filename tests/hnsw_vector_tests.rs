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

//! Tests for HNSW vector search implementation.
//!
//! Note: These tests verify the basic structure and configuration of the HNSW
//! implementation. Full functionality tests (insert, search, delete) require
//! completing the node storage implementation (load_node/store_node methods).

use nanostore::pager::{Pager, PagerConfig};
use nanostore::table::{
    HnswConfig, PagedHnswVector, VectorMetric, VectorSearch, VectorSearchOptions,
};
use nanostore::txn::TransactionId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
use std::sync::Arc;

#[test]
fn test_hnsw_creation() {
    let fs = MemoryFileSystem::new();
    let config_pager = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config_pager).unwrap());

    let config = HnswConfig {
        dimensions: 128,
        metric: VectorMetric::Cosine,
        max_connections: 16,
        max_connections_layer0: 32,
        ef_construction: 200,
        ml: 1.0 / (16.0_f64).ln(),
    };

    let hnsw = PagedHnswVector::new(1.into(), "test_hnsw".to_string(), pager, config);

    assert!(hnsw.is_ok(), "HNSW creation should succeed");
    let hnsw = hnsw.unwrap();

    // Verify basic properties through VectorSearch trait
    assert_eq!(hnsw.dimensions(), 128);
    assert_eq!(hnsw.metric(), VectorMetric::Cosine);
}

#[test]
fn test_hnsw_euclidean_metric() {
    let fs = MemoryFileSystem::new();
    let config_pager = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config_pager).unwrap());

    let config = HnswConfig {
        dimensions: 3,
        metric: VectorMetric::Euclidean,
        max_connections: 16,
        max_connections_layer0: 32,
        ef_construction: 200,
        ml: 1.0 / (16.0_f64).ln(),
    };

    let hnsw = PagedHnswVector::new(1.into(), "test_euclidean".to_string(), pager, config).unwrap();

    assert_eq!(hnsw.metric(), VectorMetric::Euclidean);
    assert_eq!(hnsw.dimensions(), 3);
}

#[test]
fn test_hnsw_cosine_metric() {
    let fs = MemoryFileSystem::new();
    let config_pager = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config_pager).unwrap());

    let config = HnswConfig {
        dimensions: 64,
        metric: VectorMetric::Cosine,
        max_connections: 32,
        max_connections_layer0: 64,
        ef_construction: 400,
        ml: 1.0 / (32.0_f64).ln(),
    };

    let hnsw = PagedHnswVector::new(2.into(), "test_cosine".to_string(), pager, config).unwrap();

    assert_eq!(hnsw.metric(), VectorMetric::Cosine);
    assert_eq!(hnsw.dimensions(), 64);
}

#[test]
fn test_hnsw_manhattan_metric() {
    let fs = MemoryFileSystem::new();
    let config_pager = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config_pager).unwrap());

    let config = HnswConfig {
        dimensions: 32,
        metric: VectorMetric::Manhattan,
        max_connections: 16,
        max_connections_layer0: 32,
        ef_construction: 200,
        ml: 1.0 / (16.0_f64).ln(),
    };

    let hnsw = PagedHnswVector::new(3.into(), "test_manhattan".to_string(), pager, config).unwrap();

    assert_eq!(hnsw.metric(), VectorMetric::Manhattan);
    assert_eq!(hnsw.dimensions(), 32);
}

#[test]
fn test_hnsw_configuration_parameters() {
    let fs = MemoryFileSystem::new();
    let config_pager = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config_pager).unwrap());

    // Test with custom configuration parameters
    let config = HnswConfig {
        dimensions: 256,
        metric: VectorMetric::Euclidean,
        max_connections: 48,
        max_connections_layer0: 96,
        ef_construction: 500,
        ml: 1.0 / (48.0_f64).ln(),
    };

    let hnsw = PagedHnswVector::new(4.into(), "test_config".to_string(), pager, config);

    assert!(hnsw.is_ok(), "HNSW with custom config should succeed");
    let hnsw = hnsw.unwrap();

    assert_eq!(hnsw.dimensions(), 256);
    assert_eq!(hnsw.metric(), VectorMetric::Euclidean);
}

#[test]
fn test_hnsw_delete_vector_removes_deleted_id_from_search_results() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = HnswConfig {
        dimensions: 2,
        metric: VectorMetric::Euclidean,
        max_connections: 4,
        max_connections_layer0: 8,
        ef_construction: 16,
        ml: 10.0,
    };

    let hnsw =
        PagedHnswVector::new(10.into(), "test_delete_search".to_string(), pager, config).unwrap();

    hnsw.insert_vector(
        b"a",
        &[0.0, 0.0],
        TransactionId::from(0),
        LogSequenceNumber::from(0),
    )
    .unwrap();
    hnsw.insert_vector(
        b"b",
        &[1.0, 0.0],
        TransactionId::from(0),
        LogSequenceNumber::from(0),
    )
    .unwrap();
    hnsw.insert_vector(
        b"c",
        &[2.0, 0.0],
        TransactionId::from(0),
        LogSequenceNumber::from(0),
    )
    .unwrap();

    hnsw.delete_vector(b"b", TransactionId::from(0), LogSequenceNumber::from(0))
        .unwrap();

    let results = hnsw
        .search_vector(
            &[1.0, 0.0],
            VectorSearchOptions {
                limit: 10,
                ef_search: Some(16),
                probes: None,
                filter: None,
            },
        )
        .unwrap();

    assert!(
        !results.iter().any(|hit| hit.id.0 == b"b"),
        "deleted vector should not appear in search results"
    );

    let report = hnsw.verify().unwrap();
    assert!(
        report.errors.is_empty(),
        "graph verification should succeed after delete repair: {:?}",
        report.errors
    );
}

#[test]
fn test_hnsw_delete_vector_updates_entry_point_and_keeps_graph_searchable() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = HnswConfig {
        dimensions: 2,
        metric: VectorMetric::Euclidean,
        max_connections: 4,
        max_connections_layer0: 8,
        ef_construction: 16,
        ml: 10.0,
    };

    let hnsw =
        PagedHnswVector::new(11.into(), "test_delete_entry".to_string(), pager, config).unwrap();

    hnsw.insert_vector(
        b"root",
        &[0.0, 0.0],
        TransactionId::from(0),
        LogSequenceNumber::from(0),
    )
    .unwrap();
    hnsw.insert_vector(
        b"left",
        &[1.0, 0.0],
        TransactionId::from(0),
        LogSequenceNumber::from(0),
    )
    .unwrap();
    hnsw.insert_vector(
        b"right",
        &[2.0, 0.0],
        TransactionId::from(0),
        LogSequenceNumber::from(0),
    )
    .unwrap();

    hnsw.delete_vector(b"root", TransactionId::from(0), LogSequenceNumber::from(0))
        .unwrap();

    let results = hnsw
        .search_vector(
            &[1.5, 0.0],
            VectorSearchOptions {
                limit: 10,
                ef_search: Some(16),
                probes: None,
                filter: None,
            },
        )
        .unwrap();

    assert_eq!(
        results.len(),
        2,
        "remaining vectors should still be searchable"
    );
    assert!(results.iter().any(|hit| hit.id.0 == b"left"));
    assert!(results.iter().any(|hit| hit.id.0 == b"right"));
    assert!(!results.iter().any(|hit| hit.id.0 == b"root"));

    let report = hnsw.verify().unwrap();
    assert!(
        report.errors.is_empty(),
        "graph verification should succeed after entry point replacement: {:?}",
        report.errors
    );
}

// Made with Bob
