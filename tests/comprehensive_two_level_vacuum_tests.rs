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

//! Comprehensive two-level vacuum test for BTree tables.
//!
//! This test validates the full two-level vacuum process:
//! 1. **Table-level vacuum** (`vacuum_table()`) - Condenses B-tree nodes, frees pages
//! 2. **Pager-level compaction** (`vacuum_full_table()`) - Repacks physical pages, reclaims disk space
//!
//! The test ensures both levels of compaction work correctly together.

use nanostore::engine::StorageEngine;
use nanostore::table::{TableEngineKind, TableOptions};
use nanostore::vfs::MemoryFileSystem;

/// Helper to create a StorageEngine for testing
fn create_test_db() -> StorageEngine<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    StorageEngine::new(&fs, "/test.wal", "/test.db").expect("Failed to create StorageEngine")
}

#[test]
fn test_comprehensive_two_level_vacuum_single_table() {
    println!("\n=== Comprehensive Two-Level Vacuum Test (Single Table) ===\n");

    let db = create_test_db();

    // Create BTree table
    let table_id = db
        .create_table(
            "btree_table",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create BTree table");
    println!("✓ Created BTree table: {:?}", table_id);

    // Phase 1: Insert 1200 records
    println!("\nPhase 1: Inserting 1200 records...");
    for i in 0..1200 {
        let key = format!("key{:05}", i);
        let value = format!("value{:05}_extra_data_to_make_larger", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
    }
    println!("✓ Inserted 1200 records");

    // Phase 2: Delete 80% of data
    println!("\nPhase 2: Deleting 80% of data (960 records)...");
    for i in 0..960 {
        let key = format!("key{:05}", i);
        db.delete(table_id, key.as_bytes())
            .expect("Failed to delete");
    }
    println!("✓ Deleted 960 records (80%)");

    // Phase 3: Perform table-level vacuum (condense B-tree nodes, free pages)
    println!("\nPhase 3: Performing table-level vacuum (vacuum_table)...");
    let versions_removed = db.vacuum_table(table_id).expect("Failed to vacuum");
    println!("✓ Table vacuum removed {} versions", versions_removed);

    // Phase 4: Perform pager-level compaction (repack physical pages)
    println!("\nPhase 4: Performing pager-level compaction (vacuum_full_table)...");
    let stats = db
        .vacuum_full_table(table_id)
        .expect("Failed to run vacuum_full_table");

    println!("✓ Pager compaction complete:");
    println!("  Pages moved: {}", stats.pages_moved);
    println!("  Pages truncated: {}", stats.pages_truncated);
    println!("  Bytes reclaimed: {}", stats.bytes_reclaimed);
    println!(
        "  File size: {} -> {} bytes",
        stats.file_size_before, stats.file_size_after
    );

    // Verify statistics
    assert!(
        stats.file_size_after <= stats.file_size_before,
        "File size should not increase"
    );
    assert_eq!(
        stats.bytes_reclaimed,
        stats.file_size_before - stats.file_size_after,
        "Bytes reclaimed should match file size difference"
    );

    // Phase 5: Verify data integrity for remaining 20%
    println!("\nPhase 5: Verifying data integrity...");
    let mut verified = 0;
    for i in 960..1200 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        let expected = format!("value{:05}_extra_data_to_make_larger", i);
        assert_eq!(
            value.as_ref().map(|v| v.as_ref()),
            Some(expected.as_bytes()),
            "Data mismatch for key {}",
            key
        );
        verified += 1;
    }
    println!("✓ Verified {} remaining records", verified);

    // Verify deleted data is gone
    for i in 0..960 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert_eq!(value, None, "Deleted key {} should return None", key);
    }
    println!("✓ Verified 960 deleted records are gone");

    println!("\n=== Test PASSED ===\n");
}

#[test]
fn test_comprehensive_two_level_vacuum_multiple_tables_sequential() {
    println!("\n=== Comprehensive Two-Level Vacuum Test (Multiple Tables, Sequential) ===\n");

    let db = create_test_db();

    // Phase 1: Create 3 BTree tables
    println!("Phase 1: Creating 3 BTree tables...");
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

    println!("✓ Created 3 tables");

    // Phase 2: Insert data into all tables
    println!("\nPhase 2: Inserting 500 records per table...");
    for i in 0..500 {
        let key = format!("key{:05}", i);
        let value = format!("value{:05}_data", i);
        db.insert(table1, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
        db.insert(table2, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
        db.insert(table3, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
    }
    println!("✓ Inserted 500 records into each table");

    // Phase 3: Delete 70% from each table
    println!("\nPhase 3: Deleting 70% from each table (350 records)...");
    for i in 0..350 {
        let key = format!("key{:05}", i);
        db.delete(table1, key.as_bytes()).expect("Failed to delete");
        db.delete(table2, key.as_bytes()).expect("Failed to delete");
        db.delete(table3, key.as_bytes()).expect("Failed to delete");
    }
    println!("✓ Deleted 350 records from each table");

    // Phase 4: Perform table-level vacuum on each table
    println!("\nPhase 4: Performing table-level vacuum on each table...");
    let v1 = db.vacuum_table(table1).expect("Failed to vacuum table1");
    let v2 = db.vacuum_table(table2).expect("Failed to vacuum table2");
    let v3 = db.vacuum_table(table3).expect("Failed to vacuum table3");
    println!("✓ Table1 vacuum: {} versions removed", v1);
    println!("✓ Table2 vacuum: {} versions removed", v2);
    println!("✓ Table3 vacuum: {} versions removed", v3);

    // Phase 5: Perform pager-level compaction on each table SEQUENTIALLY
    println!("\nPhase 5: Performing pager-level compaction on each table sequentially...");

    let stats1 = db
        .vacuum_full_table(table1)
        .expect("Failed to compact table1");
    println!(
        "✓ Table1 compaction: {} bytes reclaimed",
        stats1.bytes_reclaimed
    );

    let stats2 = db
        .vacuum_full_table(table2)
        .expect("Failed to compact table2");
    println!(
        "✓ Table2 compaction: {} bytes reclaimed",
        stats2.bytes_reclaimed
    );

    let stats3 = db
        .vacuum_full_table(table3)
        .expect("Failed to compact table3");
    println!(
        "✓ Table3 compaction: {} bytes reclaimed",
        stats3.bytes_reclaimed
    );

    let total_reclaimed = stats1.bytes_reclaimed + stats2.bytes_reclaimed + stats3.bytes_reclaimed;
    println!("  Total: {} bytes reclaimed", total_reclaimed);

    // Phase 6: Verify data integrity for remaining 30%
    println!("\nPhase 6: Verifying data integrity...");
    for i in 350..500 {
        let key = format!("key{:05}", i);

        let v1 = db
            .get(table1, key.as_bytes())
            .expect("Failed to get from table1");
        assert!(v1.is_some(), "Data should exist in table1 for key {}", key);

        let v2 = db
            .get(table2, key.as_bytes())
            .expect("Failed to get from table2");
        assert!(v2.is_some(), "Data should exist in table2 for key {}", key);

        let v3 = db
            .get(table3, key.as_bytes())
            .expect("Failed to get from table3");
        assert!(v3.is_some(), "Data should exist in table3 for key {}", key);
    }
    println!("✓ Verified 150 records in each table");

    // Verify deleted data is gone
    for i in 0..350 {
        let key = format!("key{:05}", i);

        let v1 = db
            .get(table1, key.as_bytes())
            .expect("Failed to get from table1");
        assert_eq!(v1, None, "Deleted key should return None in table1");

        let v2 = db
            .get(table2, key.as_bytes())
            .expect("Failed to get from table2");
        assert_eq!(v2, None, "Deleted key should return None in table2");

        let v3 = db
            .get(table3, key.as_bytes())
            .expect("Failed to get from table3");
        assert_eq!(v3, None, "Deleted key should return None in table3");
    }
    println!("✓ Verified 350 deleted records are gone from each table");

    println!("\n=== Test PASSED ===\n");
}

// Made with Bob
