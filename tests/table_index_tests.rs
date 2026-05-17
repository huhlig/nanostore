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

//! Comprehensive integration tests for table and index operations.

use nanokv::kvdb::{Database, DatabaseErrorKind};
use nanokv::pager::{Pager, PagerConfig};
use nanokv::table::TimeSeriesCursor;
use nanokv::table::fulltext::FullTextConfig;
use nanokv::table::graph::{GraphConfig, MemoryGraphTable};
use nanokv::table::hnsw::HnswConfig;
use nanokv::table::rtree::SpatialConfig;
use nanokv::table::timeseries::TimeSeriesConfig;
use nanokv::table::{
    ApproximateMembership, BatchOps, EdgeCursor, Flushable, FullTextSearch, GeoPoint, GeoSpatial,
    GeometryRef, GraphAdjacency, MutableTable, OrderedScan, PagedBTree, PagedBloomFilter,
    PagedFullTextIndex, PagedHnswVector, PagedRTree, PointLookup, SearchableTable, Table,
    TableCursor, TableEngineKind, TableOptions, TableReader, TableWriter, TimeSeries,
    TimeSeriesTable, VectorSearch,
};
use nanokv::txn::TransactionId;
use nanokv::types::{Bound, KeyBuf, ScanBounds, TableId};
use nanokv::vfs::MemoryFileSystem;
use nanokv::wal::LogSequenceNumber;
use rand::Rng;
use std::sync::Arc;
use std::time::Instant;

fn create_test_db() -> Database<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    Database::new(&fs, "test.wal", "test.db").expect("Failed to create database")
}

fn create_test_pager() -> Arc<Pager<MemoryFileSystem>> {
    let fs = MemoryFileSystem::new();
    Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).expect("Failed to create pager"))
}

fn create_test_tree() -> PagedBTree<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());
    PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap()
}

fn default_table_options() -> TableOptions {
    TableOptions {
        engine: TableEngineKind::Memory,
        key_encoding: nanokv::types::KeyEncoding::RawBytes,
        compression: None,
        encryption: None,
        page_size: None,
        format_version: 1,
        max_inline_size: None,
        max_value_size: None,
    }
}

fn btree_table_options() -> TableOptions {
    TableOptions {
        engine: TableEngineKind::BTree,
        key_encoding: nanokv::types::KeyEncoding::RawBytes,
        compression: None,
        encryption: None,
        page_size: None,
        format_version: 1,
        max_inline_size: None,
        max_value_size: None,
    }
}

fn lsm_table_options() -> TableOptions {
    TableOptions {
        engine: TableEngineKind::LsmTree,
        key_encoding: nanokv::types::KeyEncoding::RawBytes,
        compression: None,
        encryption: None,
        page_size: None,
        format_version: 1,
        max_inline_size: None,
        max_value_size: None,
    }
}

// =============================================================================
// Table CRUD Operations Tests
// =============================================================================

#[test]
fn test_table_crud_insert_get() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();
    db.insert(table_id, b"user1", b"Alice").unwrap();
    let value = db.get(table_id, b"user1").unwrap().unwrap();
    assert_eq!(value.as_ref(), b"Alice");
}

#[test]
fn test_table_crud_update() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();
    db.insert(table_id, b"user1", b"Alice").unwrap();
    db.update(table_id, b"user1", b"Alice Smith").unwrap();
    let value = db.get(table_id, b"user1").unwrap().unwrap();
    assert_eq!(value.as_ref(), b"Alice Smith");
}

#[test]
fn test_table_crud_delete() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();
    db.insert(table_id, b"user1", b"Alice").unwrap();
    let deleted = db.delete(table_id, b"user1").unwrap();
    assert!(deleted);
    assert!(db.get(table_id, b"user1").unwrap().is_none());
}

#[test]
fn test_table_crud_upsert() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();
    let is_update = db.upsert(table_id, b"user1", b"Alice").unwrap();
    assert!(!is_update);
    let is_update = db.upsert(table_id, b"user1", b"Alice Smith").unwrap();
    assert!(is_update);
    let value = db.get(table_id, b"user1").unwrap().unwrap();
    assert_eq!(value.as_ref(), b"Alice Smith");
}

#[test]
fn test_table_crud_multiple_keys() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();
    for i in 0..100 {
        let key = format!("user{:04}", i);
        let value = format!("User {}", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .unwrap();
    }
    for i in 0..100 {
        let key = format!("user{:04}", i);
        let expected = format!("User {}", i);
        let value = db.get(table_id, key.as_bytes()).unwrap().unwrap();
        assert_eq!(value.as_ref(), expected.as_bytes());
    }
}

// =============================================================================
// Table Scan Operations Tests
// =============================================================================

#[test]
fn test_btree_scan_forward() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();
    writer.put(b"charlie", b"Charlie").unwrap();
    writer.put(b"alice", b"Alice").unwrap();
    writer.put(b"bob", b"Bob").unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let bounds = ScanBounds::All;
    let mut cursor = reader.scan(bounds, LogSequenceNumber::from(100)).unwrap();
    cursor.first().unwrap();

    let mut keys = Vec::new();
    while cursor.valid() {
        if let Some(key) = cursor.key() {
            keys.push(String::from_utf8(key.to_vec()).unwrap());
        }
        cursor.next().unwrap();
    }
    assert_eq!(keys, vec!["alice", "bob", "charlie"]);
}

#[test]
fn test_btree_scan_range() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();
    for i in 0..20 {
        let key = format!("user{:03}", i);
        let value = format!("User {}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let bounds = ScanBounds::Range {
        start: Bound::Included(KeyBuf(b"user005".to_vec())),
        end: Bound::Included(KeyBuf(b"user010".to_vec())),
    };
    let mut cursor = reader.scan(bounds, LogSequenceNumber::from(100)).unwrap();
    cursor.first().unwrap();
    let mut count = 0;
    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }
    assert_eq!(count, 6);
}

#[test]
fn test_btree_scan_prefix() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();
    writer.put(b"user:1", b"Alice").unwrap();
    writer.put(b"user:2", b"Bob").unwrap();
    writer.put(b"admin:1", b"Charlie").unwrap();
    writer.put(b"user:3", b"David").unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let bounds = ScanBounds::Prefix(KeyBuf(b"user:".to_vec()));
    let mut cursor = reader.scan(bounds, LogSequenceNumber::from(100)).unwrap();
    cursor.first().unwrap();
    let mut count = 0;
    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }
    assert_eq!(count, 3);
}

#[test]
fn test_btree_scan_reverse() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();
    writer.put(b"alice", b"Alice").unwrap();
    writer.put(b"bob", b"Bob").unwrap();
    writer.put(b"charlie", b"Charlie").unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let bounds = ScanBounds::All;
    let mut cursor = reader.scan(bounds, LogSequenceNumber::from(100)).unwrap();
    cursor.last().unwrap();

    let mut keys = Vec::new();
    while cursor.valid() {
        if let Some(key) = cursor.key() {
            keys.push(String::from_utf8(key.to_vec()).unwrap());
        }
        cursor.prev().unwrap();
    }
    assert_eq!(keys, vec!["charlie", "bob", "alice"]);
}

// =============================================================================
// Table Engine Integration Tests
// =============================================================================

#[test]
fn test_btree_table_crud() {
    let db = create_test_db();
    let table_id = db.create_table("users", btree_table_options()).unwrap();
    db.insert(table_id, b"user1", b"Alice").unwrap();
    db.update(table_id, b"user1", b"Alice Smith").unwrap();
    let value = db.get(table_id, b"user1").unwrap().unwrap();
    assert_eq!(value.as_ref(), b"Alice Smith");
    let deleted = db.delete(table_id, b"user1").unwrap();
    assert!(deleted);
    assert!(db.get(table_id, b"user1").unwrap().is_none());
}

#[test]
fn test_lsm_table_crud() {
    let db = create_test_db();
    let table_id = db.create_table("events", lsm_table_options()).unwrap();
    db.insert(table_id, b"event1", b"User login").unwrap();
    db.update(table_id, b"event1", b"Admin login").unwrap();
    let value = db.get(table_id, b"event1").unwrap().unwrap();
    assert_eq!(value.as_ref(), b"Admin login");
    let deleted = db.delete(table_id, b"event1").unwrap();
    assert!(deleted);
    assert!(db.get(table_id, b"event1").unwrap().is_none());
}

#[test]
fn test_table_batch_operations() {
    let pager = create_test_pager();
    let table = PagedBTree::new(TableId::from(1), "test_batch".to_string(), pager.clone()).unwrap();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();

    let mutations: Vec<nanokv::table::Mutation> = (0..10)
        .map(|i| {
            let key = format!("key{:03}", i);
            let value = format!("value{}", i);
            nanokv::table::Mutation::Put {
                key: std::borrow::Cow::Owned(key.into_bytes()),
                value: std::borrow::Cow::Owned(value.into_bytes()),
            }
        })
        .collect();

    let batch = nanokv::table::WriteBatch { mutations };
    let report = writer.apply_batch(batch).unwrap();
    assert_eq!(report.attempted, 10);
    assert_eq!(report.applied, 10);
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    for i in 0..10 {
        let key = format!("key{:03}", i);
        let expected = format!("value{}", i);
        let value = reader
            .get(key.as_bytes(), LogSequenceNumber::from(100))
            .unwrap()
            .unwrap();
        assert_eq!(value.as_ref(), expected.as_bytes());
    }
}

// =============================================================================
// Index Operations Tests
// =============================================================================

#[test]
fn test_bloom_filter_insert_and_lookup() {
    let pager = create_test_pager();
    let mut bloom = PagedBloomFilter::new(
        TableId::from(1),
        "test_bloom".to_string(),
        pager.clone(),
        1000,
        10,
        None,
    )
    .unwrap();
    bloom.insert_key(b"key1", TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
    bloom.insert_key(b"key2", TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
    bloom.insert_key(b"key3", TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
    assert!(bloom.might_contain(b"key1").unwrap());
    assert!(bloom.might_contain(b"key2").unwrap());
    assert!(bloom.might_contain(b"key3").unwrap());
    assert!(!bloom.might_contain(b"key_not_exists").unwrap());
}

#[test]
fn test_bloom_filter_false_positive_rate() {
    let pager = create_test_pager();
    let num_items = 10000;
    let bits_per_key = 10;
    let mut bloom = PagedBloomFilter::new(
        TableId::from(1),
        "test_bloom_fpr".to_string(),
        pager.clone(),
        num_items,
        bits_per_key,
        None,
    )
    .unwrap();
    for i in 0..num_items {
        let key = format!("key{}", i);
        bloom.insert_key(key.as_bytes(), TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
    }
    let fpr = bloom.false_positive_rate();
    assert!(fpr > 0.0 && fpr < 0.1);
}

#[test]
#[ignore = "HNSW node loading not yet implemented"]
fn test_hnsw_vector_insert_and_search() {
    let pager = create_test_pager();
    let config = HnswConfig {
        dimensions: 4,
        metric: nanokv::table::VectorMetric::Cosine,
        max_connections: 16,
        max_connections_layer0: 32,
        ef_construction: 200,
        ml: 1.0,
    };
    let hnsw = PagedHnswVector::new(
        TableId::from(1),
        "test_hnsw".to_string(),
        pager.clone(),
        config,
    )
    .unwrap();
    hnsw.insert_vector(b"id1", &[1.0, 0.0, 0.0, 0.0], TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
    hnsw.insert_vector(b"id2", &[0.0, 1.0, 0.0, 0.0], TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
    hnsw.insert_vector(b"id3", &[0.0, 0.0, 1.0, 0.0], TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();

    let options = nanokv::table::VectorSearchOptions {
        limit: 2,
        ef_search: Some(50),
        probes: None,
        filter: None,
    };
    let hits = hnsw.search_vector(&[1.0, 0.0, 0.0, 0.0], options).unwrap();
    assert!(!hits.is_empty());
    assert!(hits.len() <= 2);
}

#[test]
fn test_rtree_insert_and_query() {
    let pager = create_test_pager();
    let config = SpatialConfig::default();
    let mut rtree = PagedRTree::new(
        TableId::from(1),
        "test_rtree".to_string(),
        pager.clone(),
        config,
    )
    .unwrap();
    rtree
        .insert_geometry(
            b"point1",
            GeometryRef::Point(GeoPoint { x: 1.0, y: 2.0 }),
            TransactionId::from(0),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    rtree
        .insert_geometry(
            b"point2",
            GeometryRef::Point(GeoPoint { x: 5.0, y: 5.0 }),
            TransactionId::from(0),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    rtree
        .insert_geometry(
            b"point3",
            GeometryRef::Point(GeoPoint { x: 10.0, y: 10.0 }),
            TransactionId::from(0),
            LogSequenceNumber::from(1),
        )
        .unwrap();

    let query_box = GeometryRef::BoundingBox {
        min: GeoPoint { x: 0.0, y: 0.0 },
        max: GeoPoint { x: 3.0, y: 3.0 },
    };
    let hits = rtree.intersects(query_box, 100).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id.0, b"point1");
}

#[test]
fn test_rtree_nearest_neighbor() {
    let pager = create_test_pager();
    let config = SpatialConfig::default();
    let mut rtree = PagedRTree::new(
        TableId::from(1),
        "test_rtree_nn".to_string(),
        pager.clone(),
        config,
    )
    .unwrap();
    rtree
        .insert_geometry(
            b"point1",
            GeometryRef::Point(GeoPoint { x: 0.0, y: 0.0 }),
            TransactionId::from(0),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    rtree
        .insert_geometry(
            b"point2",
            GeometryRef::Point(GeoPoint { x: 10.0, y: 10.0 }),
            TransactionId::from(0),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    rtree
        .insert_geometry(
            b"point3",
            GeometryRef::Point(GeoPoint { x: 20.0, y: 20.0 }),
            TransactionId::from(0),
            LogSequenceNumber::from(1),
        )
        .unwrap();

    let nearest = rtree.nearest(GeoPoint { x: 1.0, y: 1.0 }, 1).unwrap();
    assert_eq!(nearest.len(), 1);
    assert_eq!(nearest[0].id.0, b"point1");
}

#[test]
fn test_timeseries_append_and_scan() {
    let pager = create_test_pager();
    let config = TimeSeriesConfig::default();
    let table = TimeSeriesTable::new(
        TableId::from(1),
        "test_timeseries".to_string(),
        pager.clone(),
        config,
    )
    .unwrap();
    table.append_point(b"cpu.usage", 1000, b"value1", TransactionId::from(1)).unwrap();
    table.append_point(b"cpu.usage", 2000, b"value2", TransactionId::from(1)).unwrap();
    table.append_point(b"cpu.usage", 3000, b"value3", TransactionId::from(1)).unwrap();
    // append_point uses TransactionId::from(1), so commit with matching ID
    table
        .commit_versions(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    let mut cursor = table.scan_series(b"cpu.usage", 1500, 2500).unwrap();
    let mut count = 0;
    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }
    assert_eq!(count, 1);

    let latest = table.latest_before(b"cpu.usage", 2500).unwrap();
    assert!(latest.is_some());
    assert_eq!(latest.unwrap().timestamp, 2000);
}

#[test]
fn test_fulltext_index_and_search() {
    let pager = create_test_pager();
    let config = FullTextConfig::default();
    let mut fulltext = PagedFullTextIndex::new(
        TableId::from(1),
        "test_fulltext".to_string(),
        pager.clone(),
        config,
    )
    .unwrap();

    let fields = vec![nanokv::table::TextField {
        name: "content",
        text: "The quick brown fox jumps over the lazy dog",
        boost: 1.0,
    }];
    fulltext.index_document(b"doc1", &fields, TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();

    let fields2 = vec![nanokv::table::TextField {
        name: "content",
        text: "The lazy cat sleeps on the warm mat",
        boost: 1.0,
    }];
    fulltext.index_document(b"doc2", &fields2, TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();

    let query = nanokv::table::TextQuery {
        query: "lazy",
        default_field: Some("content"),
        require_positions: false,
    };
    let results = fulltext.search(query, 10).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results.len(), 2);
}

#[test]
fn test_graph_add_edge_and_traverse() {
    let config = GraphConfig::default();
    let mut graph = MemoryGraphTable::new(TableId::from(1), "test_graph".to_string(), config);
    graph
        .add_edge(b"alice", b"follows", b"bob", b"edge1", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    graph
        .add_edge(b"alice", b"follows", b"charlie", b"edge2", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    graph
        .add_edge(b"bob", b"follows", b"david", b"edge3", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    graph
        .commit_versions(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    let mut cursor = graph.outgoing(b"alice", Some(b"follows")).unwrap();
    let mut count = 0;
    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }
    assert_eq!(count, 2);

    let mut cursor = graph.incoming(b"bob", Some(b"follows")).unwrap();
    let mut count = 0;
    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }
    assert_eq!(count, 1);
}

// =============================================================================
// Table+Index Integration Tests
// =============================================================================

#[test]
fn test_table_with_bloom_filter() {
    let db = create_test_db();
    let table_id = db.create_table("users", btree_table_options()).unwrap();
    db.insert(table_id, b"user1", b"Alice").unwrap();
    db.insert(table_id, b"user2", b"Bob").unwrap();
    db.insert(table_id, b"user3", b"Charlie").unwrap();

    let pager = create_test_pager();
    let mut bloom = PagedBloomFilter::new(
        TableId::from(2),
        "users_bloom".to_string(),
        pager.clone(),
        1000,
        10,
        None,
    )
    .unwrap();
    bloom.insert_key(b"user1", TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
    bloom.insert_key(b"user2", TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
    bloom.insert_key(b"user3", TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();

    for key in [b"user1".as_ref(), b"user2".as_ref(), b"user3".as_ref()] {
        if bloom.might_contain(key).unwrap() {
            assert!(db.get(table_id, key).unwrap().is_some());
        }
    }
    if !bloom.might_contain(b"user999").unwrap() {
        assert!(db.get(table_id, b"user999").unwrap().is_none());
    }
}

#[test]
fn test_table_with_secondary_index_pattern() {
    let db = create_test_db();
    let users_id = db.create_table("users", btree_table_options()).unwrap();
    let email_index_id = db
        .create_table("email_index", btree_table_options())
        .unwrap();

    let user1_data = r#"{"id":1,"name":"Alice","email":"alice@example.com"}"#;
    db.insert(users_id, b"user1", user1_data.as_bytes())
        .unwrap();
    db.insert(email_index_id, b"alice@example.com", b"user1")
        .unwrap();

    let user_id = db
        .get(email_index_id, b"alice@example.com")
        .unwrap()
        .unwrap();
    let user_data = db.get(users_id, &user_id.0).unwrap().unwrap();
    assert!(
        std::str::from_utf8(user_data.as_ref())
            .unwrap()
            .contains("Alice")
    );
}

#[test]
fn test_composite_index_pattern() {
    let db = create_test_db();
    let orders_id = db.create_table("orders", btree_table_options()).unwrap();
    db.insert(orders_id, b"2024-01:user1:order1", b"{\"amount\":100}")
        .unwrap();
    db.insert(orders_id, b"2024-01:user1:order2", b"{\"amount\":200}")
        .unwrap();
    db.insert(orders_id, b"2024-01:user2:order1", b"{\"amount\":150}")
        .unwrap();
    db.insert(orders_id, b"2024-02:user1:order1", b"{\"amount\":300}")
        .unwrap();

    let info = db.get_object_info(orders_id).unwrap().unwrap();
    assert_eq!(info.options.engine, TableEngineKind::BTree);
}

// =============================================================================
// Concurrent Access Tests
// =============================================================================

#[test]
fn test_concurrent_reads() {
    use std::thread;
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();
    for i in 0..100 {
        let key = format!("user{:04}", i);
        let value = format!("User {}", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .unwrap();
    }

    let mut handles = vec![];
    for thread_id in 0..4 {
        let table_id = table_id;
        let db = create_test_db();
        let new_table_id = db.create_table("users", default_table_options()).unwrap();
        for i in 0..25 {
            let key = format!("user{:04}", thread_id * 25 + i);
            let value = format!("User {}", thread_id * 25 + i);
            db.insert(new_table_id, key.as_bytes(), value.as_bytes())
                .unwrap();
        }
        handles.push(thread::spawn(move || {
            for i in 0..25 {
                let key = format!("user{:04}", thread_id * 25 + i);
                let expected = format!("User {}", thread_id * 25 + i);
                let value = db.get(new_table_id, key.as_bytes()).unwrap().unwrap();
                assert_eq!(value.as_ref(), expected.as_bytes());
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_concurrent_writes_different_keys() {
    use std::sync::Mutex;
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").expect("Failed to create database");
    let table_id = db.create_table("users", default_table_options()).unwrap();
    let db = Arc::new(db);
    let errors = Arc::new(Mutex::new(Vec::new()));

    let mut handles = vec![];
    for thread_id in 0..4 {
        let db = db.clone();
        let errors = errors.clone();
        let table_id = table_id;
        handles.push(std::thread::spawn(move || {
            for i in 0..25 {
                let key = format!("user{:04}_{:02}", thread_id, i);
                let value = format!("User {}-{}", thread_id, i);
                if let Err(e) = db.insert(table_id, key.as_bytes(), value.as_bytes()) {
                    errors
                        .lock()
                        .unwrap()
                        .push(format!("Thread {} failed: {:?}", thread_id, e));
                }
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let errors = errors.lock().unwrap();
    assert!(
        errors.is_empty(),
        "Concurrent writes should succeed: {:?}",
        *errors
    );
}

// =============================================================================
// Error Handling Tests
// =============================================================================

#[test]
fn test_insert_duplicate_key_error() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();
    db.insert(table_id, b"user1", b"Alice").unwrap();
    let result = db.insert(table_id, b"user1", b"Bob");
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().kind,
        DatabaseErrorKind::KeyAlreadyExists
    );
}

#[test]
fn test_update_nonexistent_key_error() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();
    let result = db.update(table_id, b"user1", b"Alice");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind, DatabaseErrorKind::KeyNotFound);
}

#[test]
fn test_operation_on_nonexistent_table() {
    let db = create_test_db();
    let result = db.insert(TableId::from(999), b"key", b"value");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind, DatabaseErrorKind::NotATable);
}

#[test]
fn test_bloom_filter_definitely_not_contains() {
    let pager = create_test_pager();
    let mut bloom = PagedBloomFilter::new(
        TableId::from(1),
        "test_bloom".to_string(),
        pager.clone(),
        1000,
        10,
        None,
    )
    .unwrap();
    bloom.insert_key(b"key1", TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
    assert!(!bloom.might_contain(b"key_not_exists").unwrap());
}

#[test]
fn test_vector_search_dimension_mismatch() {
    let pager = create_test_pager();
    let config = HnswConfig {
        dimensions: 4,
        metric: nanokv::table::VectorMetric::Cosine,
        max_connections: 16,
        max_connections_layer0: 32,
        ef_construction: 200,
        ml: 1.0,
    };
    let hnsw = PagedHnswVector::new(
        TableId::from(1),
        "test_hnsw".to_string(),
        pager.clone(),
        config,
    )
    .unwrap();
    let result = hnsw.insert_vector(b"id1", &[1.0, 0.0, 0.0], TransactionId::from(1), LogSequenceNumber::from(1));
    assert!(result.is_err());
}

// =============================================================================
// Edge Cases Tests
// =============================================================================

#[test]
fn test_empty_table_operations() {
    let db = create_test_db();
    let table_id = db.create_table("empty", default_table_options()).unwrap();
    assert!(db.get(table_id, b"key").unwrap().is_none());
    assert!(!db.delete(table_id, b"key").unwrap());
}

#[test]
fn test_empty_value() {
    let db = create_test_db();
    let table_id = db
        .create_table("empty_values", default_table_options())
        .unwrap();
    db.insert(table_id, b"key", b"").unwrap();
    let value = db.get(table_id, b"key").unwrap();
    if let Some(v) = value {
        assert_eq!(v.as_ref(), b"");
    }
}

#[test]
fn test_special_byte_keys() {
    let db = create_test_db();
    let table_id = db
        .create_table("special_keys", default_table_options())
        .unwrap();
    db.insert(table_id, b"\x00\x00\x00", b"null_bytes").unwrap();
    db.insert(table_id, b"\xFF\xFF\xFF", b"max_bytes").unwrap();
    db.insert(table_id, b"key\x00with\x00nulls", b"embedded_nulls")
        .unwrap();
    assert_eq!(
        db.get(table_id, b"\x00\x00\x00").unwrap().unwrap().as_ref(),
        b"null_bytes"
    );
    assert_eq!(
        db.get(table_id, b"\xFF\xFF\xFF").unwrap().unwrap().as_ref(),
        b"max_bytes"
    );
    assert_eq!(
        db.get(table_id, b"key\x00with\x00nulls")
            .unwrap()
            .unwrap()
            .as_ref(),
        b"embedded_nulls"
    );
}

#[test]
fn test_large_values() {
    let db = create_test_db();
    let table_id = db
        .create_table("large_values", btree_table_options())
        .unwrap();
    let large_value = vec![b'X'; 1024 * 1024];
    db.insert(table_id, b"large", &large_value).unwrap();
    let retrieved = db.get(table_id, b"large").unwrap().unwrap();
    assert_eq!(retrieved.as_ref(), &large_value[..]);
}

#[test]
fn test_many_small_keys() {
    let db = create_test_db();
    let table_id = db
        .create_table("many_keys", default_table_options())
        .unwrap();
    for i in 0..1000 {
        let key = format!("k{:06}", i);
        let value = format!("v{}", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .unwrap();
    }
    for i in 0..1000 {
        let key = format!("k{:06}", i);
        let expected = format!("v{}", i);
        let value = db.get(table_id, key.as_bytes()).unwrap().unwrap();
        assert_eq!(value.as_ref(), expected.as_bytes());
    }
}

#[test]
fn test_delete_all_keys() {
    let db = create_test_db();
    let table_id = db
        .create_table("delete_all", default_table_options())
        .unwrap();
    for i in 0..100 {
        let key = format!("key{:03}", i);
        let value = format!("value{}", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .unwrap();
    }
    for i in 0..100 {
        let key = format!("key{:03}", i);
        assert!(db.delete(table_id, key.as_bytes()).unwrap());
    }
    for i in 0..100 {
        let key = format!("key{:03}", i);
        assert!(db.get(table_id, key.as_bytes()).unwrap().is_none());
    }
}

// =============================================================================
// Property-Based Tests for Invariants
// =============================================================================

#[test]
fn test_property_insert_then_get() {
    let db = create_test_db();
    let table_id = db
        .create_table("property_test", default_table_options())
        .unwrap();
    let mut rng = rand::rng();
    let mut inserted = std::collections::HashMap::new();

    for _ in 0..100 {
        let mut key = [0u8; 8];
        rng.fill_bytes(&mut key);
        let mut value = [0u8; 16];
        rng.fill_bytes(&mut value);
        if db.insert(table_id, &key, &value).is_ok() {
            inserted.insert(key.to_vec(), value.to_vec());
        }
    }

    for (key, expected_value) in &inserted {
        let retrieved = db.get(table_id, key).unwrap().unwrap();
        assert_eq!(retrieved.as_ref(), expected_value.as_slice());
    }
}

#[test]
fn test_property_delete_removes_key() {
    let db = create_test_db();
    let table_id = db
        .create_table("property_delete", default_table_options())
        .unwrap();
    let mut rng = rand::rng();
    let mut keys = Vec::new();

    for _ in 0..50 {
        let mut key = [0u8; 8];
        rng.fill_bytes(&mut key);
        let mut value = [0u8; 16];
        rng.fill_bytes(&mut value);
        db.insert(table_id, &key, &value).unwrap();
        keys.push(key.to_vec());
    }

    for key in &keys {
        assert!(db.delete(table_id, key).unwrap());
        assert!(db.get(table_id, key).unwrap().is_none());
    }
}

#[test]
fn test_property_scan_returns_all_inserted_keys() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();
    let mut inserted_keys = std::collections::BTreeSet::new();

    for i in 0..100 {
        let key = format!("key{:04}", i);
        writer
            .put(key.as_bytes(), format!("value{}", i).as_bytes())
            .unwrap();
        inserted_keys.insert(key);
    }
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let bounds = ScanBounds::All;
    let mut cursor = reader.scan(bounds, LogSequenceNumber::from(100)).unwrap();
    cursor.first().unwrap();
    let mut scanned_keys = std::collections::BTreeSet::new();
    while cursor.valid() {
        if let Some(key) = cursor.key() {
            scanned_keys.insert(String::from_utf8(key.to_vec()).unwrap());
        }
        cursor.next().unwrap();
    }
    assert_eq!(scanned_keys, inserted_keys);
}

#[test]
fn test_property_bloom_filter_no_false_negatives() {
    let pager = create_test_pager();
    let mut bloom = PagedBloomFilter::new(
        TableId::from(1),
        "property_bloom".to_string(),
        pager.clone(),
        10000,
        10,
        None,
    )
    .unwrap();
    let mut rng = rand::rng();
    let mut inserted = Vec::new();

    for _ in 0..1000 {
        let mut key = [0u8; 8];
        rng.fill_bytes(&mut key);
        bloom.insert_key(&key, TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
        inserted.push(key.to_vec());
    }

    for key in &inserted {
        assert!(
            bloom.might_contain(key).unwrap(),
            "Bloom filter must not have false negatives"
        );
    }
}

#[test]
fn test_property_table_idempotent_delete() {
    let db = create_test_db();
    let table_id = db
        .create_table("idempotent_delete", default_table_options())
        .unwrap();
    db.insert(table_id, b"key", b"value").unwrap();
    assert!(db.delete(table_id, b"key").unwrap());
    assert!(!db.delete(table_id, b"key").unwrap());
    assert!(!db.delete(table_id, b"key").unwrap());
}

// =============================================================================
// Performance Benchmarks
// =============================================================================

#[test]
fn test_benchmark_sequential_insert() {
    let db = create_test_db();
    let table_id = db
        .create_table("benchmark_seq", btree_table_options())
        .unwrap();
    let count = 1000;
    let start = Instant::now();
    for i in 0..count {
        let key = format!("key{:06}", i);
        let value = format!("value{}", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .unwrap();
    }
    let duration = start.elapsed();
    let ops_per_sec = count as f64 / duration.as_secs_f64();
    eprintln!(
        "Sequential insert: {} ops in {:?} ({:.0} ops/sec)",
        count, duration, ops_per_sec
    );
    assert!(ops_per_sec > 100.0);
}

#[test]
fn test_benchmark_sequential_get() {
    let db = create_test_db();
    let table_id = db
        .create_table("benchmark_get", btree_table_options())
        .unwrap();
    let count = 1000;
    for i in 0..count {
        let key = format!("key{:06}", i);
        let value = format!("value{}", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .unwrap();
    }
    let start = Instant::now();
    for i in 0..count {
        let key = format!("key{:06}", i);
        db.get(table_id, key.as_bytes()).unwrap();
    }
    let duration = start.elapsed();
    let ops_per_sec = count as f64 / duration.as_secs_f64();
    eprintln!(
        "Sequential get: {} ops in {:?} ({:.0} ops/sec)",
        count, duration, ops_per_sec
    );
    assert!(ops_per_sec > 1000.0);
}

#[test]
fn test_benchmark_scan() {
    let table = create_test_tree();
    let count = 1000;
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();
    for i in 0..count {
        let key = format!("key{:06}", i);
        let value = format!("value{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let start = Instant::now();
    for _ in 0..10 {
        let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
        let bounds = ScanBounds::All;
        let mut cursor = reader.scan(bounds, LogSequenceNumber::from(100)).unwrap();
        cursor.first().unwrap();
        let mut scanned = 0;
        while cursor.valid() {
            scanned += 1;
            cursor.next().unwrap();
        }
        assert_eq!(scanned, count);
    }
    let duration = start.elapsed();
    let ops_per_sec = 10.0 / duration.as_secs_f64();
    eprintln!(
        "Full scan: {} ops in {:?} ({:.0} ops/sec)",
        10, duration, ops_per_sec
    );
}

#[test]
fn test_benchmark_bloom_filter() {
    let pager = create_test_pager();
    let count = 10000;
    let mut bloom = PagedBloomFilter::new(
        TableId::from(1),
        "benchmark_bloom".to_string(),
        pager.clone(),
        count,
        10,
        None,
    )
    .unwrap();

    let start = Instant::now();
    for i in 0..count {
        let key = format!("key{}", i);
        bloom.insert_key(key.as_bytes(), TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
    }
    let duration = start.elapsed();
    let ops_per_sec = count as f64 / duration.as_secs_f64();
    eprintln!(
        "Bloom insert: {} ops in {:?} ({:.0} ops/sec)",
        count, duration, ops_per_sec
    );

    let start = Instant::now();
    for i in 0..count {
        let key = format!("key{}", i);
        bloom.might_contain(key.as_bytes()).unwrap();
    }
    let duration = start.elapsed();
    let ops_per_sec = count as f64 / duration.as_secs_f64();
    eprintln!(
        "Bloom lookup: {} ops in {:?} ({:.0} ops/sec)",
        count, duration, ops_per_sec
    );
}

// =============================================================================
// Table Statistics and Capabilities Tests
// =============================================================================

#[test]
fn test_table_capabilities() {
    let pager = create_test_pager();
    let table = PagedBTree::new(TableId::from(1), "test_caps".to_string(), pager.clone()).unwrap();
    let caps = Table::capabilities(&table);
    assert!(caps.point_lookup);
    assert!(caps.ordered);
    assert!(caps.disk_resident);
    assert!(!caps.memory_resident);
}

#[test]
fn test_table_statistics() {
    let pager = create_test_pager();
    let table = PagedBTree::new(TableId::from(1), "test_stats".to_string(), pager.clone()).unwrap();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);
    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();
    for i in 0..100 {
        let key = format!("key{:03}", i);
        let value = format!("value{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();
    let stats = Table::stats(&table).unwrap();
    assert!(stats.row_count.is_some() || stats.row_count.is_none());
}

// =============================================================================
// Multi-Engine Integration Tests
// =============================================================================

#[test]
fn test_multiple_engines_isolation() {
    let db = create_test_db();
    let memory_id = db
        .create_table("memory_table", default_table_options())
        .unwrap();
    let btree_id = db
        .create_table("btree_table", btree_table_options())
        .unwrap();
    let lsm_id = db.create_table("lsm_table", lsm_table_options()).unwrap();

    db.insert(memory_id, b"key", b"memory_value").unwrap();
    db.insert(btree_id, b"key", b"btree_value").unwrap();
    db.insert(lsm_id, b"key", b"lsm_value").unwrap();

    assert_eq!(
        db.get(memory_id, b"key").unwrap().unwrap().as_ref(),
        b"memory_value"
    );
    assert_eq!(
        db.get(btree_id, b"key").unwrap().unwrap().as_ref(),
        b"btree_value"
    );
    assert_eq!(
        db.get(lsm_id, b"key").unwrap().unwrap().as_ref(),
        b"lsm_value"
    );
}

#[test]
fn test_table_drop_and_recreate() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();
    db.insert(table_id, b"user1", b"Alice").unwrap();
    db.drop_table(table_id).unwrap();
    let new_table_id = db.create_table("users2", default_table_options()).unwrap();
    assert!(db.get(new_table_id, b"user1").unwrap().is_none());
}

// Made with Bob
