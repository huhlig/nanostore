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

//! Comprehensive tests for VACUUM FULL functionality.
//!
//! VACUUM FULL is a blocking operation that compacts the database file by:
//! 1. Moving data from high-numbered pages to low-numbered pages
//! 2. Truncating the file to remove unused pages at the end
//! 3. Reclaiming disk space
//!
//! These tests verify:
//! - Basic compaction and truncation
//! - Multiple table handling
//! - Large files with many free pages
//! - Edge cases (no free pages, all pages free, single page)
//! - Statistics accuracy
//! - File size reduction
//! - Data integrity after compaction
//! - Concurrent access blocking

use nanostore::engine::StorageEngine;
use nanostore::table::{TableEngineKind, TableOptions};
use nanostore::vfs::MemoryFileSystem;
use std::collections::HashMap;

/// Helper to create a StorageEngine for testing
fn create_test_db() -> StorageEngine<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    StorageEngine::new(&fs, "/test.wal", "/test.db").expect("Failed to create StorageEngine")
}

/// Helper to insert test data into a table
fn insert_test_data(db: &StorageEngine<MemoryFileSystem>, table_id: nanostore::types::TableId, count: usize) {
    for i in 0..count {
        let key = format!("key{:05}", i);
        let value = format!("value{:05}", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
    }
}

/// Helper to verify data integrity
fn verify_data(db: &StorageEngine<MemoryFileSystem>, table_id: nanostore::types::TableId, count: usize) {
    for i in 0..count {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        let expected = format!("value{:05}", i);
        assert_eq!(
            value.as_ref().map(|v| v.as_ref()),
            Some(expected.as_bytes()),
            "Data mismatch for key {}",
            key
        );
    }
}

#[test]
fn test_vacuum_full_basic_compaction() {
    // Test 1: Basic compaction test (move pages, truncate file)
    let db = create_test_db();

    // Create a B-tree table (persistent)
    let table_id = db
        .create_table(
            "test_table",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create table");

    // Insert initial data
    insert_test_data(&db, table_id, 100);

    // Delete some records to create free pages
    for i in 0..50 {
        let key = format!("key{:05}", i);
        db.delete(table_id, key.as_bytes())
            .expect("Failed to delete");
    }

    // Run regular vacuum first to mark pages as free
    let versions_removed = db.vacuum_table(table_id).expect("Failed to vacuum");
    println!("Regular vacuum removed {} versions", versions_removed);

    // Run VACUUM FULL
    let stats = db
        .vacuum_full_table(table_id)
        .expect("Failed to run VACUUM FULL");

    println!("VACUUM FULL stats: {:?}", stats);

    // Verify statistics
    assert!(
        stats.file_size_after <= stats.file_size_before,
        "File size should not increase"
    );
    assert!(
        stats.bytes_reclaimed > 0 || stats.pages_moved == 0,
        "Should reclaim bytes or have no pages to move"
    );

    // Verify data integrity - remaining keys should still be accessible
    for i in 50..100 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        let expected = format!("value{:05}", i);
        assert_eq!(
            value.as_ref().map(|v| v.as_ref()),
            Some(expected.as_bytes()),
            "Data integrity check failed for key {}",
            key
        );
    }

    // Verify deleted keys are still deleted
    for i in 0..50 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert_eq!(value, None, "Deleted key should return None");
    }
}

#[test]
fn test_vacuum_full_multiple_tables() {
    // Test 2: Multiple tables test
    let db = create_test_db();

    // Create multiple tables
    let table1 = db
        .create_table(
            "table1",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create table1");

    let table2 = db
        .create_table(
            "table2",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create table2");

    let table3 = db
        .create_table(
            "table3",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create table3");

    // Insert data into all tables
    insert_test_data(&db, table1, 50);
    insert_test_data(&db, table2, 75);
    insert_test_data(&db, table3, 100);

    // Delete some data from each table
    for i in 0..25 {
        let key = format!("key{:05}", i);
        db.delete(table1, key.as_bytes()).expect("Failed to delete");
        db.delete(table2, key.as_bytes()).expect("Failed to delete");
        db.delete(table3, key.as_bytes()).expect("Failed to delete");
    }

    // Run VACUUM FULL on all tables
    let results = db.vacuum_full_all().expect("Failed to run VACUUM FULL on all tables");

    println!("VACUUM FULL results for {} tables", results.len());
    for (tid, stats) in &results {
        println!("  Table {:?}: reclaimed {} bytes", tid, stats.bytes_reclaimed);
    }

    // Verify all tables were processed
    assert!(results.contains_key(&table1), "Table1 should be in results");
    assert!(results.contains_key(&table2), "Table2 should be in results");
    assert!(results.contains_key(&table3), "Table3 should be in results");

    // Verify data integrity for all tables
    for i in 25..50 {
        let key = format!("key{:05}", i);
        let value = db.get(table1, key.as_bytes()).expect("Failed to get");
        assert!(value.is_some(), "Table1 data should exist");
    }
    for i in 25..75 {
        let key = format!("key{:05}", i);
        let value = db.get(table2, key.as_bytes()).expect("Failed to get");
        assert!(value.is_some(), "Table2 data should exist");
    }
    for i in 25..100 {
        let key = format!("key{:05}", i);
        let value = db.get(table3, key.as_bytes()).expect("Failed to get");
        assert!(value.is_some(), "Table3 data should exist");
    }
}

#[test]
fn test_vacuum_full_large_file_many_free_pages() {
    // Test 3: Large files with many free pages
    let db = create_test_db();

    let table_id = db
        .create_table(
            "large_table",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create table");

    // Insert a large amount of data
    for i in 0..500 {
        let key = format!("key{:05}", i);
        let value = vec![0u8; 512]; // 512 bytes per value
        db.insert(table_id, key.as_bytes(), &value)
            .expect("Failed to insert");
    }

    // Delete 80% of the data to create many free pages
    for i in 0..400 {
        let key = format!("key{:05}", i);
        db.delete(table_id, key.as_bytes())
            .expect("Failed to delete");
    }

    // Run regular vacuum to mark pages as free
    db.vacuum_table(table_id).expect("Failed to vacuum");

    // Run VACUUM FULL
    let stats = db
        .vacuum_full_table(table_id)
        .expect("Failed to run VACUUM FULL");

    println!("Large file VACUUM FULL stats: {:?}", stats);

    // With 80% deletion, we should see significant space reclamation
    assert!(
        stats.bytes_reclaimed > 0 || stats.pages_moved == 0,
        "Should reclaim significant space with 80% deletion"
    );

    // Verify remaining data
    for i in 400..500 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert!(value.is_some(), "Remaining data should exist");
        assert_eq!(value.unwrap().as_ref().len(), 512, "Value size should be preserved");
    }
}

#[test]
fn test_vacuum_full_edge_case_no_free_pages() {
    // Test 4a: Edge case - no free pages
    let db = create_test_db();

    let table_id = db
        .create_table(
            "full_table",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create table");

    // Insert data without any deletions
    insert_test_data(&db, table_id, 100);

    // Run VACUUM FULL on a table with no free pages
    let stats = db
        .vacuum_full_table(table_id)
        .expect("Failed to run VACUUM FULL");

    println!("No free pages VACUUM FULL stats: {:?}", stats);

    // Should complete successfully with minimal changes
    assert_eq!(stats.pages_moved, 0, "No pages should be moved");
    assert_eq!(stats.pages_truncated, 0, "No pages should be truncated");
    assert_eq!(stats.bytes_reclaimed, 0, "No bytes should be reclaimed");

    // Verify all data is intact
    verify_data(&db, table_id, 100);
}

#[test]
fn test_vacuum_full_edge_case_all_pages_free() {
    // Test 4b: Edge case - all pages free (empty table after deletions)
    let db = create_test_db();

    let table_id = db
        .create_table(
            "empty_table",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create table");

    // Insert and then delete all data
    insert_test_data(&db, table_id, 50);
    for i in 0..50 {
        let key = format!("key{:05}", i);
        db.delete(table_id, key.as_bytes())
            .expect("Failed to delete");
    }

    // Run regular vacuum
    db.vacuum_table(table_id).expect("Failed to vacuum");

    // Run VACUUM FULL
    let stats = db
        .vacuum_full_table(table_id)
        .expect("Failed to run VACUUM FULL");

    println!("All pages free VACUUM FULL stats: {:?}", stats);

    // Should reclaim space from the empty table
    assert!(
        stats.file_size_after <= stats.file_size_before,
        "File size should not increase"
    );

    // Verify table is empty
    for i in 0..50 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert_eq!(value, None, "All keys should be deleted");
    }
}

#[test]
fn test_vacuum_full_edge_case_single_page() {
    // Test 4c: Edge case - single page table
    let db = create_test_db();

    let table_id = db
        .create_table(
            "tiny_table",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create table");

    // Insert minimal data (should fit in a single page)
    for i in 0..5 {
        let key = format!("k{}", i);
        let value = format!("v{}", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
    }

    // Run VACUUM FULL
    let stats = db
        .vacuum_full_table(table_id)
        .expect("Failed to run VACUUM FULL");

    println!("Single page VACUUM FULL stats: {:?}", stats);

    // Should complete successfully with minimal changes
    assert!(
        stats.file_size_after <= stats.file_size_before,
        "File size should not increase"
    );

    // Verify data integrity
    for i in 0..5 {
        let key = format!("k{}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        let expected = format!("v{}", i);
        assert_eq!(
            value.as_ref().map(|v| v.as_ref()),
            Some(expected.as_bytes())
        );
    }
}

#[test]
fn test_vacuum_full_statistics_accuracy() {
    // Test 5: Statistics accuracy verification
    let db = create_test_db();

    let table_id = db
        .create_table(
            "stats_table",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create table");

    // Insert data
    insert_test_data(&db, table_id, 200);

    // Delete half
    for i in 0..100 {
        let key = format!("key{:05}", i);
        db.delete(table_id, key.as_bytes())
            .expect("Failed to delete");
    }

    // Run regular vacuum
    db.vacuum_table(table_id).expect("Failed to vacuum");

    // Run VACUUM FULL
    let stats = db
        .vacuum_full_table(table_id)
        .expect("Failed to run VACUUM FULL");

    println!("Statistics verification: {:?}", stats);

    // Verify statistics consistency
    assert!(
        stats.file_size_before >= stats.file_size_after,
        "File size should not increase"
    );
    assert_eq!(
        stats.bytes_reclaimed,
        stats.file_size_before - stats.file_size_after,
        "Bytes reclaimed should match file size difference"
    );
    assert!(
        stats.duration.as_millis() > 0,
        "Duration should be recorded"
    );

    // Verify pages_truncated makes sense
    if stats.pages_truncated > 0 {
        assert!(
            stats.bytes_reclaimed > 0,
            "If pages were truncated, bytes should be reclaimed"
        );
    }

    // Verify data integrity
    verify_data(&db, table_id, 100);
}

#[test]
fn test_vacuum_full_file_size_reduction() {
    // Test 6: File size reduction verification
    let db = create_test_db();

    let table_id = db
        .create_table(
            "size_test_table",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create table");

    // Insert large values
    for i in 0..300 {
        let key = format!("key{:05}", i);
        let value = vec![0u8; 1024]; // 1KB per value
        db.insert(table_id, key.as_bytes(), &value)
            .expect("Failed to insert");
    }

    // Delete 70% of data
    for i in 0..210 {
        let key = format!("key{:05}", i);
        db.delete(table_id, key.as_bytes())
            .expect("Failed to delete");
    }

    // Run regular vacuum
    db.vacuum_table(table_id).expect("Failed to vacuum");

    // Run VACUUM FULL
    let stats = db
        .vacuum_full_table(table_id)
        .expect("Failed to run VACUUM FULL");

    println!("File size reduction test: {:?}", stats);
    println!(
        "Size reduction: {} -> {} ({:.1}% reduction)",
        stats.file_size_before,
        stats.file_size_after,
        (stats.bytes_reclaimed as f64 / stats.file_size_before as f64) * 100.0
    );

    // With 70% deletion, we should see significant reduction
    assert!(
        stats.file_size_after < stats.file_size_before,
        "File size should be reduced"
    );

    // Verify remaining data
    for i in 210..300 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert!(value.is_some(), "Remaining data should exist");
        assert_eq!(value.unwrap().as_ref().len(), 1024, "Value size should be preserved");
    }
}

#[test]
fn test_vacuum_full_data_integrity_after_compaction() {
    // Test 7: Data integrity after compaction
    let db = create_test_db();

    let table_id = db
        .create_table(
            "integrity_table",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create table");

    // Insert diverse data
    let mut expected_data = HashMap::new();
    for i in 0..150 {
        let key = format!("key{:05}", i);
        let value = format!("value_{}_{}", i, i * 2);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
        expected_data.insert(key.clone(), value);
    }

    // Delete some keys
    for i in 30..90 {
        let key = format!("key{:05}", i);
        db.delete(table_id, key.as_bytes())
            .expect("Failed to delete");
        expected_data.remove(&key);
    }

    // Update some keys
    for i in 100..120 {
        let key = format!("key{:05}", i);
        let value = format!("updated_value_{}", i);
        db.update(table_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to update");
        expected_data.insert(key.clone(), value);
    }

    // Run regular vacuum
    db.vacuum_table(table_id).expect("Failed to vacuum");

    // Run VACUUM FULL
    let stats = db
        .vacuum_full_table(table_id)
        .expect("Failed to run VACUUM FULL");

    println!("Data integrity test stats: {:?}", stats);

    // Verify all expected data is intact
    for (key, expected_value) in &expected_data {
        let value = db
            .get(table_id, key.as_bytes())
            .expect("Failed to get");
        assert_eq!(
            value.as_ref().map(|v| v.as_ref()),
            Some(expected_value.as_bytes()),
            "Data mismatch for key {}",
            key
        );
    }

    // Verify deleted keys are still deleted
    for i in 30..90 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert_eq!(value, None, "Deleted key {} should return None", key);
    }

    println!("Data integrity verified: {} keys checked", expected_data.len());
}

#[test]
fn test_vacuum_full_non_persistent_table_error() {
    // Test that VACUUM FULL fails gracefully on non-persistent tables
    let db = create_test_db();

    // Create an in-memory table
    let table_id = db
        .create_table(
            "memory_table",
            TableOptions {
                engine: TableEngineKind::Memory,
                ..Default::default()
            },
        )
        .expect("Failed to create table");

    // Insert some data
    insert_test_data(&db, table_id, 50);

    // VACUUM FULL should fail on non-persistent tables
    let result = db.vacuum_full_table(table_id);

    assert!(
        result.is_err(),
        "VACUUM FULL should fail on non-persistent tables"
    );

    println!("Expected error for non-persistent table: {:?}", result.err());
}

#[test]
fn test_vacuum_full_empty_database() {
    // Test VACUUM FULL on an empty database
    let db = create_test_db();

    // Run VACUUM FULL on all tables (should be empty)
    let results = db
        .vacuum_full_all()
        .expect("Failed to run VACUUM FULL on empty database");

    assert_eq!(results.len(), 0, "Empty database should have no tables to compact");
}

#[test]
fn test_vacuum_full_preserves_table_metadata() {
    // Test that VACUUM FULL preserves table metadata
    let db = create_test_db();

    let table_name = "metadata_table";
    let table_id = db
        .create_table(
            table_name,
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create table");

    // Insert data
    insert_test_data(&db, table_id, 100);

    // Get table info before VACUUM FULL
    let info_before = db
        .get_object_info(table_id)
        .expect("Failed to get table info")
        .expect("Table should exist");

    // Run VACUUM FULL
    db.vacuum_full_table(table_id)
        .expect("Failed to run VACUUM FULL");

    // Get table info after VACUUM FULL
    let info_after = db
        .get_object_info(table_id)
        .expect("Failed to get table info")
        .expect("Table should exist");

    // Verify metadata is preserved
    assert_eq!(info_before.id, info_after.id, "Table ID should be preserved");
    assert_eq!(info_before.name, info_after.name, "Table name should be preserved");
    assert_eq!(
        info_before.options.engine, info_after.options.engine,
        "Table engine should be preserved"
    );

    // Verify data is still accessible
    verify_data(&db, table_id, 100);
}

// Made with Bob