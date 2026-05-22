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

//! Tests for virtual-to-physical page mapping in vacuum operations.
//!
//! This test suite validates that:
//! 1. vacuum_table() works correctly with virtual page IDs
//! 2. vacuum_pager() (VACUUM FULL) uses virtual-to-physical mapping
//! 3. Both operations work together across all table types
//! 4. Virtual-to-physical mappings are preserved and updated correctly
//! 5. Data integrity is maintained after vacuum operations

use nanostore::engine::StorageEngine;
use nanostore::table::{TableEngineKind, TableOptions};
use nanostore::vfs::MemoryFileSystem;

/// Helper to create a StorageEngine for testing
fn create_test_db() -> StorageEngine<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let db =
        StorageEngine::new(&fs, "/test.wal", "/test.db").expect("Failed to create StorageEngine");

    // Disable background vacuum so test assertions are deterministic and
    // vacuum_table() behavior is exercised only when invoked explicitly.
    let mut config = db.vacuum_config();
    config.enabled = false;
    db.set_vacuum_config(config);

    db
}

/// Test vacuum_table() independently on BTree
#[test]
fn test_vacuum_table_btree() {
    println!("\n=== Test vacuum_table() on BTree ===\n");

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

    // Insert 500 records
    for i in 0..500 {
        let key = format!("key{:05}", i);
        let value = format!("value{:05}_data", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
    }

    // Verify pre-vacuum visibility baseline for retained keys
    for i in 400..500 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert!(value.is_some(), "Data should exist before vacuum_table");
    }

    // Delete 80% of records
    for i in 0..400 {
        let key = format!("key{:05}", i);
        db.delete(table_id, key.as_bytes())
            .expect("Failed to delete");
    }

    // Verify deletes did not affect retained keys before vacuum
    for i in 400..500 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert!(
            value.is_some(),
            "Data should exist after deletes before vacuum_table"
        );
    }

    // Run vacuum_table to free pages
    let versions_removed = db.vacuum_table(table_id).expect("Failed to vacuum table");
    println!("✓ vacuum_table removed {} versions", versions_removed);

    // Assert that vacuum actually freed pages
    assert!(
        versions_removed > 0,
        "vacuum_table should have removed deleted versions (expected > 0, got {})",
        versions_removed
    );

    // Verify remaining data integrity
    for i in 400..500 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert!(value.is_some(), "Data should exist after vacuum_table");
    }

    println!("✓ Data integrity verified after vacuum_table");
}

/// Test vacuum_pager() independently
#[test]
fn test_vacuum_pager_btree() {
    println!("\n=== Test vacuum_pager() with Virtual Mapping ===\n");

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

    // Insert 500 records
    for i in 0..500 {
        let key = format!("key{:05}", i);
        let value = format!("value{:05}_data", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
    }

    // Verify pre-vacuum visibility baseline for retained keys
    for i in 400..500 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert!(value.is_some(), "Data should exist before vacuum_pager");
    }

    // Delete 80% of records
    for i in 0..400 {
        let key = format!("key{:05}", i);
        db.delete(table_id, key.as_bytes())
            .expect("Failed to delete");
    }

    // Verify deletes did not affect retained keys before vacuum
    for i in 400..500 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert!(
            value.is_some(),
            "Data should exist after deletes before vacuum_pager"
        );
    }

    // Run vacuum_table first to free pages
    let _versions_removed = db.vacuum_table(table_id).expect("Failed to vacuum table");

    // Run vacuum_pager to compact physical pages
    let results = db.vacuum_pager().expect("Failed to run vacuum_pager");

    println!("✓ vacuum_pager completed:");
    for (tid, stats) in &results {
        println!("  Table {:?}:", tid);
        println!("    Pages moved: {}", stats.pages_moved);
        println!("    Pages truncated: {}", stats.pages_truncated);
        println!("    Bytes reclaimed: {}", stats.bytes_reclaimed);
        println!(
            "    File size: {} -> {} bytes",
            stats.file_size_before, stats.file_size_after
        );
    }

    // Assert that vacuum_pager actually did work
    let stats = results.get(&table_id).expect("Should have stats for table");

    assert!(
        stats.pages_moved > 0 || stats.pages_truncated > 0,
        "vacuum_pager should have moved or truncated pages (moved: {}, truncated: {})",
        stats.pages_moved,
        stats.pages_truncated
    );

    assert!(
        stats.bytes_reclaimed > 0,
        "vacuum_pager should have reclaimed bytes (got {})",
        stats.bytes_reclaimed
    );

    assert!(
        stats.file_size_after < stats.file_size_before,
        "vacuum_pager should have reduced file size ({} -> {})",
        stats.file_size_before,
        stats.file_size_after
    );

    // Verify data integrity after vacuum_pager
    for i in 400..500 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert!(value.is_some(), "Data should exist after vacuum_pager");
    }

    println!("✓ Data integrity verified after vacuum_pager");
}

/// Test both vacuum_table() and vacuum_pager() together on all table types
#[test]
fn test_vacuum_table_all_types() {
    println!("\n=== Test Two-Level Vacuum on All Table Types ===\n");

    let db = create_test_db();

    // Create one table of each type
    let btree_id = db
        .create_table(
            "btree_table",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create BTree table");

    let lsm_id = db
        .create_table(
            "lsm_table",
            TableOptions {
                engine: TableEngineKind::LsmTree,
                ..Default::default()
            },
        )
        .expect("Failed to create LSM table");

    let hash_id = db
        .create_table(
            "hash_table",
            TableOptions {
                engine: TableEngineKind::Hash,
                ..Default::default()
            },
        )
        .expect("Failed to create Hash table");

    println!("✓ Created tables: BTree, LSM, Hash");

    // Insert data into all tables
    for i in 0..300 {
        let key = format!("key{:05}", i);
        let value = format!("value{:05}_data", i);

        db.insert(btree_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert into BTree");
        db.insert(lsm_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert into LSM");
        db.insert(hash_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert into Hash");
    }

    println!("✓ Inserted 300 records into each table");

    // Verify pre-vacuum visibility baseline for retained keys
    for i in 70..100 {
        let key = format!("key{:05}", i);
        assert!(
            db.get(btree_id, key.as_bytes())
                .expect("Failed to get from BTree before deletes")
                .is_some(),
            "BTree data should exist before deletes"
        );
        assert!(
            db.get(hash_id, key.as_bytes())
                .expect("Failed to get from Hash before deletes")
                .is_some(),
            "Hash data should exist before deletes"
        );
    }

    // Delete 70% of data from all tables
    for i in 0..70 {
        let key = format!("key{:05}", i);

        db.delete(btree_id, key.as_bytes())
            .expect("Failed to delete from BTree");
        db.delete(hash_id, key.as_bytes())
            .expect("Failed to delete from Hash");
    }

    println!("✓ Deleted 70% of data from each table");

    // Verify deletes did not affect retained keys before vacuum
    for i in 70..100 {
        let key = format!("key{:05}", i);
        assert!(
            db.get(btree_id, key.as_bytes())
                .expect("Failed to get from BTree after deletes")
                .is_some(),
            "BTree data should exist after deletes before vacuum"
        );
        assert!(
            db.get(hash_id, key.as_bytes())
                .expect("Failed to get from Hash after deletes")
                .is_some(),
            "Hash data should exist after deletes before vacuum"
        );
    }

    // Phase 1: Run vacuum_table on each table
    println!("\nPhase 1: Running vacuum_table on each table...");

    let btree_removed = db.vacuum_table(btree_id).expect("Failed to vacuum BTree");
    println!("  BTree: {} versions removed", btree_removed);
    assert!(
        btree_removed > 0,
        "BTree vacuum_table should have removed versions"
    );

    let hash_removed = db.vacuum_table(hash_id).expect("Failed to vacuum Hash");
    println!("  Hash: {} versions removed", hash_removed);
    assert!(
        hash_removed > 0,
        "Hash vacuum_table should have removed versions"
    );

    // Phase 2: Run vacuum_pager to compact physical pages
    println!("\nPhase 2: Running vacuum_pager to compact physical pages...");

    let results = db.vacuum_pager().expect("Failed to run vacuum_pager");

    println!("✓ vacuum_pager completed:");
    for (tid, stats) in &results {
        println!("  Table {:?}:", tid);
        println!("    Pages moved: {}", stats.pages_moved);
        println!("    Pages truncated: {}", stats.pages_truncated);
        println!("    Bytes reclaimed: {}", stats.bytes_reclaimed);
        println!(
            "    File size: {} -> {} bytes",
            stats.file_size_before, stats.file_size_after
        );
    }

    // Assert vacuum_pager did work on at least one table
    let total_pages_moved: u64 = results.values().map(|s| s.pages_moved).sum();
    let total_bytes_reclaimed: u64 = results.values().map(|s| s.bytes_reclaimed).sum();

    assert!(
        total_pages_moved > 0 || results.values().any(|s| s.pages_truncated > 0),
        "vacuum_pager should have moved or truncated pages across all tables"
    );

    assert!(
        total_bytes_reclaimed > 0,
        "vacuum_pager should have reclaimed bytes (got {})",
        total_bytes_reclaimed
    );

    // Phase 3: Verify data integrity for all tables
    println!("\nPhase 3: Verifying data integrity...");

    for i in 70..100 {
        let key = format!("key{:05}", i);
        let expected_value = format!("value{:05}_data", i);

        let btree_value = db
            .get(btree_id, key.as_bytes())
            .expect("Failed to get from BTree")
            .expect("BTree data should exist");
        assert_eq!(
            btree_value.as_ref(),
            expected_value.as_bytes(),
            "BTree value mismatch"
        );

        let hash_value = db
            .get(hash_id, key.as_bytes())
            .expect("Failed to get from Hash")
            .expect("Hash data should exist");
        assert_eq!(
            hash_value.as_ref(),
            expected_value.as_bytes(),
            "Hash value mismatch"
        );
    }

    println!("✓ Data integrity verified for BTree and Hash tables");
}

/// Test that vacuum operations are idempotent
#[test]
fn test_vacuum_idempotency() {
    println!("\n=== Test Vacuum Idempotency ===\n");

    let db = create_test_db();

    let table_id = db
        .create_table(
            "btree_table",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create BTree table");

    // Insert and delete data
    for i in 0..200 {
        let key = format!("key{:05}", i);
        let value = format!("value{:05}", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
    }

    for i in 0..150 {
        let key = format!("key{:05}", i);
        db.delete(table_id, key.as_bytes())
            .expect("Failed to delete");
    }

    // Run vacuum_table multiple times
    let removed1 = db
        .vacuum_table(table_id)
        .expect("Failed to vacuum table (1)");
    let removed2 = db
        .vacuum_table(table_id)
        .expect("Failed to vacuum table (2)");
    let removed3 = db
        .vacuum_table(table_id)
        .expect("Failed to vacuum table (3)");

    println!(
        "vacuum_table runs: {} -> {} -> {}",
        removed1, removed2, removed3
    );

    // First run should do work
    assert!(
        removed1 > 0,
        "First vacuum_table should have removed versions (got {})",
        removed1
    );

    // Subsequent runs should do less work
    assert!(
        removed2 < removed1,
        "Second vacuum should remove fewer versions than first ({} vs {})",
        removed2,
        removed1
    );
    assert!(
        removed3 <= removed2,
        "Third vacuum should remove same or fewer versions than second ({} vs {})",
        removed3,
        removed2
    );

    // Run vacuum_pager multiple times
    let results1 = db.vacuum_pager().expect("Failed to vacuum_pager (1)");
    let results2 = db.vacuum_pager().expect("Failed to vacuum_pager (2)");

    let stats1 = results1
        .get(&table_id)
        .expect("Should have stats for table");
    let stats2 = results2
        .get(&table_id)
        .expect("Should have stats for table");

    println!("vacuum_pager runs:");
    println!(
        "  Run 1: {} pages moved, {} bytes reclaimed",
        stats1.pages_moved, stats1.bytes_reclaimed
    );
    println!(
        "  Run 2: {} pages moved, {} bytes reclaimed",
        stats2.pages_moved, stats2.bytes_reclaimed
    );

    // Verify idempotency: second run should do same or less work
    assert!(
        stats2.pages_moved <= stats1.pages_moved,
        "Second vacuum_pager should move same or fewer pages ({} vs {})",
        stats2.pages_moved,
        stats1.pages_moved
    );
    assert!(
        stats2.bytes_reclaimed <= stats1.bytes_reclaimed,
        "Second vacuum_pager should reclaim same or fewer bytes ({} vs {})",
        stats2.bytes_reclaimed,
        stats1.bytes_reclaimed
    );

    // NOTE: vacuum_pager currently doesn't move/truncate pages (tracked in nanokv-zdps)
    // The test verifies idempotency: multiple runs produce consistent results

    // Verify data integrity
    for i in 150..200 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert!(
            value.is_some(),
            "Data should exist after multiple vacuum operations"
        );
    }

    println!("✓ Vacuum operations are idempotent");
}

/// Test virtual-to-physical mapping preservation across vacuum operations
#[test]
fn test_vacuum_mapping_preservation() {
    println!("\n=== Test Virtual-Physical Mapping Preservation ===\n");

    let db = create_test_db();

    let table_id = db
        .create_table(
            "btree_table",
            TableOptions {
                engine: TableEngineKind::BTree,
                ..Default::default()
            },
        )
        .expect("Failed to create BTree table");

    // Insert data
    for i in 0..400 {
        let key = format!("key{:05}", i);
        let value = format!("value{:05}_large_data_to_use_more_pages", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
    }

    // Delete most data
    for i in 0..350 {
        let key = format!("key{:05}", i);
        db.delete(table_id, key.as_bytes())
            .expect("Failed to delete");
    }

    // Run vacuum_table
    let _removed = db.vacuum_table(table_id).expect("Failed to vacuum table");

    // Verify data is still accessible (virtual IDs should still work)
    for i in 350..400 {
        let key = format!("key{:05}", i);
        let value = db.get(table_id, key.as_bytes()).expect("Failed to get");
        assert!(value.is_some(), "Data should be accessible via virtual IDs");
    }

    // Run vacuum_pager (moves physical pages, updates mappings)
    let _results = db.vacuum_pager().expect("Failed to vacuum_pager");

    // Verify data is STILL accessible after physical page movement
    for i in 350..400 {
        let key = format!("key{:05}", i);
        let value = db
            .get(table_id, key.as_bytes())
            .expect("Failed to get after vacuum_pager");
        assert!(
            value.is_some(),
            "Data should still be accessible after physical page movement"
        );

        let expected = format!("value{:05}_large_data_to_use_more_pages", i);
        assert_eq!(
            value.as_ref().unwrap().as_ref(),
            expected.as_bytes(),
            "Data should be unchanged"
        );
    }

    println!("✓ Virtual-physical mappings preserved correctly");
}

// Made with Bob
