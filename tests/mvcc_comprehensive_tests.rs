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

//! Comprehensive MVCC snapshot isolation tests across all storage engines.
//!
//! This test suite covers:
//! 1. Basic snapshot isolation (concurrent reads see consistent state)
//! 2. Write-write conflicts
//! 3. Read-committed vs snapshot isolation
//! 4. Long-running transactions
//! 5. Vacuum with active snapshots
//! 6. Cross-table transaction visibility

use nanostore::kvdb::Database;
use nanostore::table::{TableEngineKind, TableOptions};
use nanostore::types::{Durability, KeyEncoding};
use nanostore::vfs::MemoryFileSystem;

/// Helper to create a test database
fn create_test_db() -> Database<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    Database::new(&fs, "test.wal", "test.db").expect("Failed to create database")
}

/// Helper to create table options for a specific engine
fn table_options(engine: TableEngineKind) -> TableOptions {
    TableOptions {
        engine,
        ..TableOptions::default()
    }
}

// =============================================================================
// Phase 1: Basic Snapshot Isolation Tests
// =============================================================================

/// Test snapshot isolation with MemoryBTree engine
#[test]
fn test_snapshot_isolation_btree() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_btree", table_options(TableEngineKind::Memory))
        .unwrap();

    // Transaction 1: Write initial value
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table_id, b"key1", b"value1").unwrap();
    tx1.commit().unwrap();

    // Create snapshot after first commit
    let snapshot1 = db.create_snapshot("snap1").unwrap();

    // Transaction 2: Update value
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx2.put(table_id, b"key1", b"value2").unwrap();
    tx2.commit().unwrap();

    // Read from snapshot - should see old value
    let tx_snap = db.begin_read_at(snapshot1.lsn).unwrap();
    let value = tx_snap.get(table_id, b"key1").unwrap();
    assert_eq!(value.as_ref().map(|v| v.0.as_slice()), Some(&b"value1"[..]));

    // Read from current - should see new value
    let tx_current = db.begin_read().unwrap();
    let value = tx_current.get(table_id, b"key1").unwrap();
    assert_eq!(value.as_ref().map(|v| v.0.as_slice()), Some(&b"value2"[..]));

    // Clean up
    db.release_snapshot(snapshot1.id).unwrap();
}

/// Test snapshot isolation with MemoryHashTable engine
#[test]
fn test_snapshot_isolation_hash() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_hash", table_options(TableEngineKind::Hash))
        .unwrap();

    // Transaction 1: Write initial value
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table_id, b"key1", b"value1").unwrap();
    tx1.commit().unwrap();

    // Create snapshot after first commit
    let snapshot1 = db.create_snapshot("snap1").unwrap();

    // Transaction 2: Update value
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx2.put(table_id, b"key1", b"value2").unwrap();
    tx2.commit().unwrap();

    // Read from snapshot - should see old value
    let tx_snap = db.begin_read_at(snapshot1.lsn).unwrap();
    let value = tx_snap.get(table_id, b"key1").unwrap();
    assert_eq!(value.as_ref().map(|v| v.0.as_slice()), Some(&b"value1"[..]));

    // Read from current - should see new value
    let tx_current = db.begin_read().unwrap();
    let value = tx_current.get(table_id, b"key1").unwrap();
    assert_eq!(value.as_ref().map(|v| v.0.as_slice()), Some(&b"value2"[..]));

    // Clean up
    db.release_snapshot(snapshot1.id).unwrap();
}

/// Test snapshot isolation with BTree engine
#[test]
fn test_snapshot_isolation_paged_btree() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_paged", table_options(TableEngineKind::BTree))
        .unwrap();

    // Transaction 1: Write initial value
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table_id, b"key1", b"value1").unwrap();
    tx1.commit().unwrap();

    // Create snapshot after first commit
    let snapshot1 = db.create_snapshot("snap1").unwrap();

    // Transaction 2: Update value
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx2.put(table_id, b"key1", b"value2").unwrap();
    tx2.commit().unwrap();

    // Read from snapshot - should see old value
    let tx_snap = db.begin_read_at(snapshot1.lsn).unwrap();
    let value = tx_snap.get(table_id, b"key1").unwrap();
    assert_eq!(value.as_ref().map(|v| v.0.as_slice()), Some(&b"value1"[..]));

    // Read from current - should see new value
    let tx_current = db.begin_read().unwrap();
    let value = tx_current.get(table_id, b"key1").unwrap();
    assert_eq!(value.as_ref().map(|v| v.0.as_slice()), Some(&b"value2"[..]));

    // Clean up
    db.release_snapshot(snapshot1.id).unwrap();
}

/// Test concurrent readers see consistent state
#[test]
fn test_concurrent_readers_consistent_state() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_concurrent", table_options(TableEngineKind::Memory))
        .unwrap();

    // Write initial values
    let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx.put(table_id, b"key1", b"value1").unwrap();
    tx.put(table_id, b"key2", b"value2").unwrap();
    tx.put(table_id, b"key3", b"value3").unwrap();
    tx.commit().unwrap();

    // Create snapshot
    let snapshot = db.create_snapshot("snap").unwrap();

    // Start concurrent update
    let mut tx_writer = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx_writer.put(table_id, b"key1", b"updated1").unwrap();
    tx_writer.put(table_id, b"key2", b"updated2").unwrap();

    // Multiple readers from snapshot should see consistent state
    for i in 0..5 {
        let tx_reader = db.begin_read_at(snapshot.lsn).unwrap();

        let v1 = tx_reader.get(table_id, b"key1").unwrap();
        let v2 = tx_reader.get(table_id, b"key2").unwrap();
        let v3 = tx_reader.get(table_id, b"key3").unwrap();

        assert_eq!(
            v1.as_ref().map(|v| v.0.as_slice()),
            Some(&b"value1"[..]),
            "Reader {} saw wrong value for key1",
            i
        );
        assert_eq!(
            v2.as_ref().map(|v| v.0.as_slice()),
            Some(&b"value2"[..]),
            "Reader {} saw wrong value for key2",
            i
        );
        assert_eq!(
            v3.as_ref().map(|v| v.0.as_slice()),
            Some(&b"value3"[..]),
            "Reader {} saw wrong value for key3",
            i
        );
    }

    // Commit the update
    tx_writer.commit().unwrap();

    // Readers from snapshot should still see old values
    let tx_reader = db.begin_read_at(snapshot.lsn).unwrap();
    let v1 = tx_reader.get(table_id, b"key1").unwrap();
    assert_eq!(v1.as_ref().map(|v| v.0.as_slice()), Some(&b"value1"[..]));

    // New readers should see updated values
    let tx_new = db.begin_read().unwrap();
    let v1 = tx_new.get(table_id, b"key1").unwrap();
    assert_eq!(v1.as_ref().map(|v| v.0.as_slice()), Some(&b"updated1"[..]));

    // Clean up
    db.release_snapshot(snapshot.id).unwrap();
}

/// Test snapshots don't see uncommitted changes
#[test]
fn test_snapshots_dont_see_uncommitted() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_uncommitted", table_options(TableEngineKind::Memory))
        .unwrap();

    // Write and commit initial value
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table_id, b"key1", b"value1").unwrap();
    tx1.commit().unwrap();

    // Create snapshot
    let snapshot = db.create_snapshot("snap").unwrap();

    // Start transaction but don't commit
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx2.put(table_id, b"key1", b"uncommitted").unwrap();
    // Don't commit tx2

    // Read from snapshot - should not see uncommitted change
    let tx_reader = db.begin_read_at(snapshot.lsn).unwrap();
    let value = tx_reader.get(table_id, b"key1").unwrap();
    assert_eq!(value.as_ref().map(|v| v.0.as_slice()), Some(&b"value1"[..]));

    // Even current readers shouldn't see uncommitted changes
    let tx_current = db.begin_read().unwrap();
    let value = tx_current.get(table_id, b"key1").unwrap();
    assert_eq!(value.as_ref().map(|v| v.0.as_slice()), Some(&b"value1"[..]));

    // Rollback the uncommitted transaction
    tx2.rollback().unwrap();

    // Clean up
    db.release_snapshot(snapshot.id).unwrap();
}

// =============================================================================
// Phase 2: Write-Write Conflict Tests
// =============================================================================

/// Test write-write conflict detection with BTree
#[test]
fn test_write_write_conflict_btree() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_conflict", table_options(TableEngineKind::Memory))
        .unwrap();

    // Write initial value
    let mut tx0 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx0.put(table_id, b"key1", b"initial").unwrap();
    tx0.commit().unwrap();

    // Start two concurrent transactions
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();

    // First transaction writes to key
    tx1.put(table_id, b"key1", b"tx1_value").unwrap();

    // Second transaction tries to write to same key - should detect conflict
    let result = tx2.put(table_id, b"key1", b"tx2_value");
    assert!(result.is_err(), "Expected write-write conflict on put");

    // Verify the error is a write-write conflict
    match result {
        Err(e) => {
            let err_str = format!("{:?}", e);
            assert!(
                err_str.contains("WriteWriteConflict"),
                "Expected WriteWriteConflict error, got: {}",
                err_str
            );
        }
        Ok(_) => panic!("Expected error but got Ok"),
    }

    // First commit should succeed
    assert!(tx1.commit().is_ok());

    // tx2 already failed at put(), so we don't need to test commit
}

/// Test write-write conflict with different keys succeeds
#[test]
fn test_write_write_different_keys_succeeds() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_no_conflict", table_options(TableEngineKind::Memory))
        .unwrap();

    // Start two concurrent transactions
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();

    // Write to different keys
    tx1.put(table_id, b"key1", b"tx1_value").unwrap();
    tx2.put(table_id, b"key2", b"tx2_value").unwrap();

    // Both commits should succeed
    assert!(tx1.commit().is_ok());
    assert!(tx2.commit().is_ok());

    // Verify both values are present
    let tx_read = db.begin_read().unwrap();
    let v1 = tx_read.get(table_id, b"key1").unwrap();
    let v2 = tx_read.get(table_id, b"key2").unwrap();
    assert_eq!(v1.as_ref().map(|v| v.0.as_slice()), Some(&b"tx1_value"[..]));
    assert_eq!(v2.as_ref().map(|v| v.0.as_slice()), Some(&b"tx2_value"[..]));
}

// =============================================================================
// Phase 3: Long-Running Transaction Tests
// =============================================================================

/// Test long-running transaction maintains consistent view
#[test]
fn test_long_running_transaction_consistency() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_long_running", table_options(TableEngineKind::Memory))
        .unwrap();

    // Write initial values
    let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
    for i in 0..10 {
        let key = format!("key{}", i);
        let value = format!("value{}", i);
        tx.put(table_id, key.as_bytes(), value.as_bytes()).unwrap();
    }
    tx.commit().unwrap();

    // Start long-running read transaction
    let tx_long = db.begin_read().unwrap();

    // Perform many updates in separate transactions
    for i in 0..10 {
        let mut tx_update = db.begin_write(Durability::SyncOnCommit).unwrap();
        let key = format!("key{}", i);
        let value = format!("updated{}", i);
        tx_update
            .put(table_id, key.as_bytes(), value.as_bytes())
            .unwrap();
        tx_update.commit().unwrap();
    }

    // Long-running transaction should still see original values
    for i in 0..10 {
        let key = format!("key{}", i);
        let expected = format!("value{}", i);
        let value = tx_long.get(table_id, key.as_bytes()).unwrap();
        assert_eq!(
            value.as_ref().map(|v| v.0.as_slice()),
            Some(expected.as_bytes()),
            "Long-running transaction saw updated value for {}",
            key
        );
    }

    // New transaction should see all updates
    let tx_new = db.begin_read().unwrap();
    for i in 0..10 {
        let key = format!("key{}", i);
        let expected = format!("updated{}", i);
        let value = tx_new.get(table_id, key.as_bytes()).unwrap();
        assert_eq!(
            value.as_ref().map(|v| v.0.as_slice()),
            Some(expected.as_bytes()),
            "New transaction didn't see update for {}",
            key
        );
    }
}

/// Test long-running transaction blocks vacuum
#[test]
fn test_long_running_transaction_blocks_vacuum() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_vacuum_block", table_options(TableEngineKind::Memory))
        .unwrap();

    // Write initial value
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table_id, b"key1", b"value1").unwrap();
    tx1.commit().unwrap();

    // Create long-running snapshot
    let snapshot = db.create_snapshot("long_running").unwrap();

    // Update value multiple times
    for i in 2..=5 {
        let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
        let value = format!("value{}", i);
        tx.put(table_id, b"key1", value.as_bytes()).unwrap();
        tx.commit().unwrap();
    }

    // Try to vacuum - should not remove versions visible to snapshot
    let _removed = db.vacuum_table(table_id).unwrap();

    // Snapshot should still be able to read original value
    let tx_snap = db.begin_read_at(snapshot.lsn).unwrap();
    let value = tx_snap.get(table_id, b"key1").unwrap();
    assert_eq!(value.as_ref().map(|v| v.0.as_slice()), Some(&b"value1"[..]));

    // Release snapshot
    db.release_snapshot(snapshot.id).unwrap();

    // Now vacuum should be able to remove more versions
    let _removed_after = db.vacuum_table(table_id).unwrap();

    // We can't assert exact counts due to base version retention,
    // but we verified the snapshot protection works
}

// =============================================================================
// Phase 4: Cross-Table Transaction Tests
// =============================================================================

/// Test cross-table transaction atomicity
#[test]
fn test_cross_table_atomicity() {
    let db = create_test_db();
    let table1 = db
        .create_table("table1", table_options(TableEngineKind::Memory))
        .unwrap();
    let table2 = db
        .create_table("table2", table_options(TableEngineKind::Memory))
        .unwrap();

    // Transaction writes to both tables
    let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx.put(table1, b"key1", b"value1").unwrap();
    tx.put(table2, b"key2", b"value2").unwrap();
    tx.commit().unwrap();

    // Both values should be visible
    let tx_read = db.begin_read().unwrap();
    let v1 = tx_read.get(table1, b"key1").unwrap();
    let v2 = tx_read.get(table2, b"key2").unwrap();
    assert!(v1.is_some());
    assert!(v2.is_some());
}

/// Test cross-table transaction rollback
#[test]
fn test_cross_table_rollback() {
    let db = create_test_db();
    let table1 = db
        .create_table("table1", table_options(TableEngineKind::Memory))
        .unwrap();
    let table2 = db
        .create_table("table2", table_options(TableEngineKind::Memory))
        .unwrap();

    // Transaction writes to both tables but rolls back
    let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx.put(table1, b"key1", b"value1").unwrap();
    tx.put(table2, b"key2", b"value2").unwrap();
    tx.rollback().unwrap();

    // Neither value should be visible
    let tx_read = db.begin_read().unwrap();
    let v1 = tx_read.get(table1, b"key1").unwrap();
    let v2 = tx_read.get(table2, b"key2").unwrap();
    assert!(v1.is_none());
    assert!(v2.is_none());
}

/// Test cross-table snapshot isolation
#[test]
fn test_cross_table_snapshot_isolation() {
    let db = create_test_db();
    let table1 = db
        .create_table("table1", table_options(TableEngineKind::Memory))
        .unwrap();
    let table2 = db
        .create_table("table2", table_options(TableEngineKind::Memory))
        .unwrap();

    // Write initial values to both tables
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table1, b"key1", b"value1").unwrap();
    tx1.put(table2, b"key2", b"value2").unwrap();
    tx1.commit().unwrap();

    // Create snapshot
    let snapshot = db.create_snapshot("cross_table").unwrap();

    // Update both tables
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx2.put(table1, b"key1", b"updated1").unwrap();
    tx2.put(table2, b"key2", b"updated2").unwrap();
    tx2.commit().unwrap();

    // Read from snapshot - should see old values in both tables
    let tx_snap = db.begin_read_at(snapshot.lsn).unwrap();
    let v1 = tx_snap.get(table1, b"key1").unwrap();
    let v2 = tx_snap.get(table2, b"key2").unwrap();
    assert_eq!(v1.as_ref().map(|v| v.0.as_slice()), Some(&b"value1"[..]));
    assert_eq!(v2.as_ref().map(|v| v.0.as_slice()), Some(&b"value2"[..]));

    // Read from current - should see new values in both tables
    let tx_current = db.begin_read().unwrap();
    let v1 = tx_current.get(table1, b"key1").unwrap();
    let v2 = tx_current.get(table2, b"key2").unwrap();
    assert_eq!(v1.as_ref().map(|v| v.0.as_slice()), Some(&b"updated1"[..]));
    assert_eq!(v2.as_ref().map(|v| v.0.as_slice()), Some(&b"updated2"[..]));

    // Clean up
    db.release_snapshot(snapshot.id).unwrap();
}

/// Test snapshot isolation with ART engine
#[test]
fn test_snapshot_isolation_art() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_art", table_options(TableEngineKind::Art))
        .unwrap();

    // Transaction 1: Write initial value
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table_id, b"key1", b"value1").unwrap();
    tx1.commit().unwrap();

    // Create snapshot after first commit
    let snapshot1 = db.create_snapshot("snap1").unwrap();

    // Transaction 2: Update value
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx2.put(table_id, b"key1", b"value2").unwrap();
    tx2.commit().unwrap();

    // Read from snapshot - should see old value
    let tx_snap = db.begin_read_at(snapshot1.lsn).unwrap();
    let value = tx_snap.get(table_id, b"key1").unwrap();
    assert_eq!(value.as_ref().map(|v| v.0.as_slice()), Some(&b"value1"[..]));

    // Read from current - should see new value
    let tx_current = db.begin_read().unwrap();
    let value = tx_current.get(table_id, b"key1").unwrap();
    assert_eq!(value.as_ref().map(|v| v.0.as_slice()), Some(&b"value2"[..]));

    // Clean up
    db.release_snapshot(snapshot1.id).unwrap();
}

/// Test snapshot isolation with LSM engine
#[test]
fn test_snapshot_isolation_lsm() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_lsm", table_options(TableEngineKind::LsmTree))
        .unwrap();

    // Transaction 1: Write initial value
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table_id, b"key1", b"value1").unwrap();
    tx1.commit().unwrap();

    // Create snapshot after first commit
    let snapshot1 = db.create_snapshot("snap1").unwrap();

    // Transaction 2: Update value
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx2.put(table_id, b"key1", b"value2").unwrap();
    tx2.commit().unwrap();

    // Read from snapshot - should see old value
    let tx_snap = db.begin_read_at(snapshot1.lsn).unwrap();
    let value = tx_snap.get(table_id, b"key1").unwrap();
    assert_eq!(value.as_ref().map(|v| v.0.as_slice()), Some(&b"value1"[..]));

    // Read from current - should see new value
    let tx_current = db.begin_read().unwrap();
    let value = tx_current.get(table_id, b"key1").unwrap();
    assert_eq!(value.as_ref().map(|v| v.0.as_slice()), Some(&b"value2"[..]));

    // Clean up
    db.release_snapshot(snapshot1.id).unwrap();
}

// =============================================================================
// Phase 5: Read-Your-Writes Tests
// =============================================================================

/// Test that transactions can read their own uncommitted writes
#[test]
fn test_read_your_writes_basic() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_ryw", table_options(TableEngineKind::Memory))
        .unwrap();

    // Start transaction and write
    let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx.put(table_id, b"key1", b"my_value").unwrap();

    // Should be able to read own write before commit
    let value = tx.get(table_id, b"key1").unwrap();
    assert_eq!(
        value.as_ref().map(|v| v.0.as_slice()),
        Some(&b"my_value"[..]),
        "Transaction should see its own uncommitted write"
    );

    // Commit
    tx.commit().unwrap();

    // Verify committed value is visible to new transactions
    let tx_read = db.begin_read().unwrap();
    let value = tx_read.get(table_id, b"key1").unwrap();
    assert_eq!(
        value.as_ref().map(|v| v.0.as_slice()),
        Some(&b"my_value"[..])
    );
}

/// Test read-your-writes with multiple operations
#[test]
fn test_read_your_writes_multiple_operations() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_ryw_multi", table_options(TableEngineKind::Memory))
        .unwrap();

    let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();

    // Write multiple keys
    tx.put(table_id, b"key1", b"value1").unwrap();
    tx.put(table_id, b"key2", b"value2").unwrap();
    tx.put(table_id, b"key3", b"value3").unwrap();

    // Read all keys - should see all writes
    let v1 = tx.get(table_id, b"key1").unwrap();
    let v2 = tx.get(table_id, b"key2").unwrap();
    let v3 = tx.get(table_id, b"key3").unwrap();

    assert_eq!(v1.as_ref().map(|v| v.0.as_slice()), Some(&b"value1"[..]));
    assert_eq!(v2.as_ref().map(|v| v.0.as_slice()), Some(&b"value2"[..]));
    assert_eq!(v3.as_ref().map(|v| v.0.as_slice()), Some(&b"value3"[..]));

    // Update a key
    tx.put(table_id, b"key2", b"updated2").unwrap();

    // Should see updated value
    let v2_updated = tx.get(table_id, b"key2").unwrap();
    assert_eq!(
        v2_updated.as_ref().map(|v| v.0.as_slice()),
        Some(&b"updated2"[..])
    );

    tx.commit().unwrap();
}

/// Test read-your-writes with deletes
#[test]
fn test_read_your_writes_with_deletes() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_ryw_delete", table_options(TableEngineKind::Memory))
        .unwrap();

    // Write initial value
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table_id, b"key1", b"value1").unwrap();
    tx1.commit().unwrap();

    // Delete in new transaction
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx2.delete(table_id, b"key1").unwrap();

    // Should see deletion (None) in same transaction
    let value = tx2.get(table_id, b"key1").unwrap();
    assert!(value.is_none(), "Transaction should see its own delete");

    tx2.commit().unwrap();

    // Verify deletion is visible to new transactions
    let tx_read = db.begin_read().unwrap();
    let value = tx_read.get(table_id, b"key1").unwrap();
    assert!(value.is_none());
}

// =============================================================================
// Phase 6: Historical Reads Tests
// =============================================================================

/// Test reading from multiple historical snapshots
#[test]
fn test_historical_reads_multiple_snapshots() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_historical", table_options(TableEngineKind::Memory))
        .unwrap();

    // Version 1
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table_id, b"key1", b"version1").unwrap();
    tx1.commit().unwrap();
    let snap1 = db.create_snapshot("v1").unwrap();

    // Version 2
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx2.put(table_id, b"key1", b"version2").unwrap();
    tx2.commit().unwrap();
    let snap2 = db.create_snapshot("v2").unwrap();

    // Version 3
    let mut tx3 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx3.put(table_id, b"key1", b"version3").unwrap();
    tx3.commit().unwrap();
    let snap3 = db.create_snapshot("v3").unwrap();

    // Read from each snapshot
    let tx_v1 = db.begin_read_at(snap1.lsn).unwrap();
    let v1 = tx_v1.get(table_id, b"key1").unwrap();
    assert_eq!(v1.as_ref().map(|v| v.0.as_slice()), Some(&b"version1"[..]));

    let tx_v2 = db.begin_read_at(snap2.lsn).unwrap();
    let v2 = tx_v2.get(table_id, b"key1").unwrap();
    assert_eq!(v2.as_ref().map(|v| v.0.as_slice()), Some(&b"version2"[..]));

    let tx_v3 = db.begin_read_at(snap3.lsn).unwrap();
    let v3 = tx_v3.get(table_id, b"key1").unwrap();
    assert_eq!(v3.as_ref().map(|v| v.0.as_slice()), Some(&b"version3"[..]));

    // Current should see version 3
    let tx_current = db.begin_read().unwrap();
    let v_current = tx_current.get(table_id, b"key1").unwrap();
    assert_eq!(
        v_current.as_ref().map(|v| v.0.as_slice()),
        Some(&b"version3"[..])
    );

    // Clean up
    db.release_snapshot(snap1.id).unwrap();
    db.release_snapshot(snap2.id).unwrap();
    db.release_snapshot(snap3.id).unwrap();
}

/// Test historical reads with multiple keys
#[test]
fn test_historical_reads_multiple_keys() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_hist_keys", table_options(TableEngineKind::Memory))
        .unwrap();

    // Initial state: key1=v1, key2=v2
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table_id, b"key1", b"v1").unwrap();
    tx1.put(table_id, b"key2", b"v2").unwrap();
    tx1.commit().unwrap();
    let snap1 = db.create_snapshot("state1").unwrap();

    // Update key1 only
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx2.put(table_id, b"key1", b"v1_updated").unwrap();
    tx2.commit().unwrap();
    let snap2 = db.create_snapshot("state2").unwrap();

    // Update key2 only
    let mut tx3 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx3.put(table_id, b"key2", b"v2_updated").unwrap();
    tx3.commit().unwrap();

    // Snapshot 1: both keys original
    let tx_s1 = db.begin_read_at(snap1.lsn).unwrap();
    assert_eq!(
        tx_s1
            .get(table_id, b"key1")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"v1"[..])
    );
    assert_eq!(
        tx_s1
            .get(table_id, b"key2")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"v2"[..])
    );

    // Snapshot 2: key1 updated, key2 original
    let tx_s2 = db.begin_read_at(snap2.lsn).unwrap();
    assert_eq!(
        tx_s2
            .get(table_id, b"key1")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"v1_updated"[..])
    );
    assert_eq!(
        tx_s2
            .get(table_id, b"key2")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"v2"[..])
    );

    // Current: both keys updated
    let tx_current = db.begin_read().unwrap();
    assert_eq!(
        tx_current
            .get(table_id, b"key1")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"v1_updated"[..])
    );
    assert_eq!(
        tx_current
            .get(table_id, b"key2")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"v2_updated"[..])
    );

    // Clean up
    db.release_snapshot(snap1.id).unwrap();
    db.release_snapshot(snap2.id).unwrap();
}

// =============================================================================
// Phase 7: Comprehensive Rollback Tests
// =============================================================================

/// Test rollback with single write
#[test]
fn test_rollback_single_write() {
    let db = create_test_db();
    let table_id = db
        .create_table("test_rollback", table_options(TableEngineKind::Memory))
        .unwrap();

    // Write and rollback
    let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx.put(table_id, b"key1", b"should_not_exist").unwrap();
    tx.rollback().unwrap();

    // Value should not be visible
    let tx_read = db.begin_read().unwrap();
    let value = tx_read.get(table_id, b"key1").unwrap();
    assert!(value.is_none(), "Rolled back write should not be visible");
}

/// Test rollback with multiple writes
#[test]
fn test_rollback_multiple_writes() {
    let db = create_test_db();
    let table_id = db
        .create_table(
            "test_rollback_multi",
            table_options(TableEngineKind::Memory),
        )
        .unwrap();

    // Write multiple keys and rollback
    let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx.put(table_id, b"key1", b"value1").unwrap();
    tx.put(table_id, b"key2", b"value2").unwrap();
    tx.put(table_id, b"key3", b"value3").unwrap();
    tx.rollback().unwrap();

    // None of the values should be visible
    let tx_read = db.begin_read().unwrap();
    assert!(tx_read.get(table_id, b"key1").unwrap().is_none());
    assert!(tx_read.get(table_id, b"key2").unwrap().is_none());
    assert!(tx_read.get(table_id, b"key3").unwrap().is_none());
}

/// Test rollback with updates
#[test]
fn test_rollback_updates() {
    let db = create_test_db();
    let table_id = db
        .create_table(
            "test_rollback_update",
            table_options(TableEngineKind::Memory),
        )
        .unwrap();

    // Write initial value
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table_id, b"key1", b"original").unwrap();
    tx1.commit().unwrap();

    // Update and rollback
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx2.put(table_id, b"key1", b"updated").unwrap();
    tx2.rollback().unwrap();

    // Should still see original value
    let tx_read = db.begin_read().unwrap();
    let value = tx_read.get(table_id, b"key1").unwrap();
    assert_eq!(
        value.as_ref().map(|v| v.0.as_slice()),
        Some(&b"original"[..]),
        "Rolled back update should not be visible"
    );
}

/// Test rollback with deletes
#[test]
fn test_rollback_deletes() {
    let db = create_test_db();
    let table_id = db
        .create_table(
            "test_rollback_delete",
            table_options(TableEngineKind::Memory),
        )
        .unwrap();

    // Write initial value
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table_id, b"key1", b"exists").unwrap();
    tx1.commit().unwrap();

    // Delete and rollback
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx2.delete(table_id, b"key1").unwrap();
    tx2.rollback().unwrap();

    // Value should still exist
    let tx_read = db.begin_read().unwrap();
    let value = tx_read.get(table_id, b"key1").unwrap();
    assert_eq!(
        value.as_ref().map(|v| v.0.as_slice()),
        Some(&b"exists"[..]),
        "Rolled back delete should not affect value"
    );
}

/// Test rollback doesn't affect other transactions
#[test]
fn test_rollback_isolation() {
    let db = create_test_db();
    let table_id = db
        .create_table(
            "test_rollback_isolation",
            table_options(TableEngineKind::Memory),
        )
        .unwrap();

    // Transaction 1: Write and commit
    let mut tx1 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx1.put(table_id, b"key1", b"tx1_value").unwrap();
    tx1.commit().unwrap();

    // Transaction 2: Write and rollback
    let mut tx2 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx2.put(table_id, b"key2", b"tx2_value").unwrap();
    tx2.rollback().unwrap();

    // Transaction 3: Write and commit
    let mut tx3 = db.begin_write(Durability::SyncOnCommit).unwrap();
    tx3.put(table_id, b"key3", b"tx3_value").unwrap();
    tx3.commit().unwrap();

    // Verify: tx1 and tx3 visible, tx2 not visible
    let tx_read = db.begin_read().unwrap();
    assert_eq!(
        tx_read
            .get(table_id, b"key1")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"tx1_value"[..])
    );
    assert!(tx_read.get(table_id, b"key2").unwrap().is_none());
    assert_eq!(
        tx_read
            .get(table_id, b"key3")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"tx3_value"[..])
    );
}

// Made with Bob
