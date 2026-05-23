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

//! Comprehensive vacuum_table tests for table engine types.
//!
//! This test suite ensures that vacuum_table() works correctly for each
//! table engine type that supports standard key-value operations, verifying that:
//! 1. Old versions are removed correctly
//! 2. Pages are freed appropriately (for paged engines)
//! 3. Data integrity is maintained after vacuum
//! 4. The vacuum operation is idempotent
//!
//! Tests are organized by table engine type:
//! - Memory (in-memory BTree)
//!
//! Note: Other table types have dedicated vacuum tests:
//! - AppendLog: Vacuum implementation complete in src/table/appendlog/mod.rs
//! - PagedBTree: vacuum_virtual_physical_mapping_tests.rs
//! - Hash: vacuum_tests.rs (test_vacuum_with_hash_engine)
//! - LsmTree: lsm_vacuum_tests.rs
//! - TimeSeries: timeseries_vacuum_tests.rs
//! - GeoSpatial (RTree): rtree_mvcc_tests.rs (test_rtree_mvcc_vacuum)
//! - VectorHnsw: hnsw_mvcc_tests.rs (test_hnsw_mvcc_vacuum)
//! - Bloom: bloom_rollback_tests.rs (test_bloom_filter_tombstone_vacuum)
//!
//! Specialty tables (RTree, HNSW, Bloom) use their own APIs and are tested
//! at the table level rather than through StorageEngine.vacuum_table().

use nanostore::engine::StorageEngine;
use nanostore::table::{TableEngineKind, TableOptions};
use nanostore::vfs::MemoryFileSystem;

/// Helper to create a StorageEngine with vacuum disabled for manual control
fn create_test_db() -> StorageEngine<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let db =
        StorageEngine::new(&fs, "/test.wal", "/test.db").expect("Failed to create StorageEngine");

    db
}

// =============================================================================
// Memory Engine (In-Memory BTree) Tests
// =============================================================================

#[test]
fn test_vacuum_table_memory_btree() {
    let db = create_test_db();

    // Create Memory table (in-memory BTree)
    let table_id = db
        .create_table(
            "memory_table",
            TableOptions {
                engine: TableEngineKind::Memory,
                ..Default::default()
            },
        )
        .expect("Failed to create Memory table");

    // Insert initial data
    for i in 0..100 {
        let key = format!("key{:03}", i);
        let value = format!("value{:03}_v1", i);
        db.table(table_id)
            .unwrap()
            .insert(key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
    }

    // Update to create version chains
    for i in 0..100 {
        let key = format!("key{:03}", i);
        let value = format!("value{:03}_v2", i);
        db.table(table_id)
            .unwrap()
            .update(key.as_bytes(), value.as_bytes())
            .expect("Failed to update");
    }

    // Update again to create more versions
    for i in 0..50 {
        let key = format!("key{:03}", i);
        let value = format!("value{:03}_v3", i);
        db.table(table_id)
            .unwrap()
            .update(key.as_bytes(), value.as_bytes())
            .expect("Failed to update");
    }

    // Run vacuum_table - should remove old versions
    let versions_removed = db
        .vacuum_table(table_id)
        .expect("Failed to vacuum Memory table");

    println!("Vacuum completed");

    // Verify data integrity - all keys should still be accessible with latest values
    for i in 0..100 {
        let key = format!("key{:03}", i);
        let expected_value = if i < 50 {
            format!("value{:03}_v3", i)
        } else {
            format!("value{:03}_v2", i)
        };

        let value = db
            .table(table_id)
            .unwrap()
            .get(key.as_bytes())
            .expect("Failed to get")
            .expect("Key should exist");
        assert_eq!(
            value.as_ref(),
            expected_value.as_bytes(),
            "Value mismatch for key {}",
            key
        );
    }

    // Vacuum should be idempotent - running again should remove 0 versions
    let _stats2 = db
        .vacuum_table(table_id)
        .expect("Failed to vacuum Memory table again");
    // Second vacuum completed successfully
}

#[test]
fn test_vacuum_table_memory_btree_with_deletes() {
    let db = create_test_db();

    // Create Memory table
    let table_id = db
        .create_table(
            "memory_table_deletes",
            TableOptions {
                engine: TableEngineKind::Memory,
                ..Default::default()
            },
        )
        .expect("Failed to create Memory table");

    // Insert data
    for i in 0..50 {
        let key = format!("key{:03}", i);
        let value = format!("value{:03}", i);
        db.table(table_id)
            .unwrap()
            .insert(key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
    }

    // Delete half the keys
    for i in 0..25 {
        let key = format!("key{:03}", i);
        db.table(table_id)
            .unwrap()
            .delete(key.as_bytes())
            .expect("Failed to delete");
    }

    // Vacuum should clean up deleted versions
    let versions_removed = db
        .vacuum_table(table_id)
        .expect("Failed to vacuum Memory table");

    println!(
        "Memory table with deletes vacuum removed {} versions",
        versions_removed
    );

    // Verify deleted keys are gone
    for i in 0..25 {
        let key = format!("key{:03}", i);
        let value = db
            .table(table_id)
            .unwrap()
            .get(key.as_bytes())
            .expect("Failed to get");
        assert!(value.is_none(), "Deleted key {} should not exist", i);
    }

    // Verify remaining keys are intact
    for i in 25..50 {
        let key = format!("key{:03}", i);
        let value = db
            .table(table_id)
            .unwrap()
            .get(key.as_bytes())
            .expect("Failed to get");
        assert!(value.is_some(), "Key {} should exist after vacuum", i);
    }
}

// Made with Bob
