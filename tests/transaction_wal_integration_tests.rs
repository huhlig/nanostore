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

//! Integration tests for transaction support with WAL durability.
//!
//! These tests verify that transactions correctly integrate with the WAL
//! for durability and crash recovery.

use nanokv::kvdb::{Database, DatabaseErrorKind};
use nanokv::table::{ApproximateMembership, TableEngineKind, TableOptions};
use nanokv::txn::TransactionId;
use nanokv::types::{Bound, Durability, KeyBuf, KeyEncoding, ScanBounds};
use nanokv::vfs::MemoryFileSystem;
use nanokv::wal::LogSequenceNumber;

/// Helper to create default table options for tests
fn default_table_options() -> TableOptions {
    TableOptions {
        engine: TableEngineKind::Memory, // Memory engine for fast tests
        ..TableOptions::default()
    }
}

fn bloom_table_options() -> TableOptions {
    TableOptions {
        engine: TableEngineKind::Bloom,
        ..TableOptions::default()
    }
}

#[test]
fn test_transaction_basic_commit() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    // Create a table first
    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();

    // Begin a write transaction
    let mut txn = db.begin_write(Durability::WalOnly).unwrap();

    // Write some data
    txn.put(table_id, b"key1", b"value1").unwrap();
    txn.put(table_id, b"key2", b"value2").unwrap();

    // Commit the transaction
    let commit_info = txn.commit().unwrap();
    assert!(commit_info.durable_lsn.is_some());

    // Verify data is persisted
    let txn2 = db.begin_read().unwrap();
    let value1 = txn2.get(table_id, b"key1").unwrap();
    assert_eq!(value1.unwrap().0, b"value1");
    let value2 = txn2.get(table_id, b"key2").unwrap();
    assert_eq!(value2.unwrap().0, b"value2");
}

#[test]
fn test_transaction_rollback() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    // Create a table first
    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();

    // Begin a write transaction
    let mut txn = db.begin_write(Durability::WalOnly).unwrap();

    // Write some data
    txn.put(table_id, b"key1", b"value1").unwrap();
    txn.put(table_id, b"key2", b"value2").unwrap();

    // Rollback the transaction
    txn.rollback().unwrap();

    // Verify data is NOT persisted
    let txn2 = db.begin_read().unwrap();
    let value1 = txn2.get(table_id, b"key1").unwrap();
    assert!(value1.is_none());
    let value2 = txn2.get(table_id, b"key2").unwrap();
    assert!(value2.is_none());
}

#[test]
fn test_transaction_read_uncommitted_changes() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    // Create a table first
    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();

    // Begin a write transaction
    let mut txn = db.begin_write(Durability::WalOnly).unwrap();

    // Write some data
    txn.put(table_id, b"key1", b"value1").unwrap();

    // Read uncommitted data within the same transaction
    let value = txn.get(table_id, b"key1").unwrap();
    assert_eq!(value.unwrap().0, b"value1");

    // Commit
    txn.commit().unwrap();
}

#[test]
fn test_transaction_delete() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    // Create a table first
    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();

    // First, insert some data
    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.put(table_id, b"key1", b"value1").unwrap();
        txn.commit().unwrap();
    }

    // Now delete it
    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        let existed = txn.delete(table_id, b"key1").unwrap();
        assert!(existed);
        txn.commit().unwrap();
    }

    // Verify it's gone
    let txn = db.begin_read().unwrap();
    let value = txn.get(table_id, b"key1").unwrap();
    assert!(value.is_none());
}

#[test]
fn test_transaction_update() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    // Create a table first
    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();

    // Insert initial value
    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.put(table_id, b"key1", b"value1").unwrap();
        txn.commit().unwrap();
    }

    // Update the value
    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.put(table_id, b"key1", b"value2").unwrap();
        txn.commit().unwrap();
    }

    // Verify updated value
    let txn = db.begin_read().unwrap();
    let value = txn.get(table_id, b"key1").unwrap();
    assert_eq!(value.unwrap().0, b"value2");
}

#[test]
fn test_multi_table_transaction() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    // Create multiple tables first
    let table1 = db.create_table("table1", default_table_options()).unwrap();
    let table2 = db.create_table("table2", default_table_options()).unwrap();
    let table3 = db.create_table("table3", default_table_options()).unwrap();

    // Write to multiple tables in a single transaction
    let mut txn = db.begin_write(Durability::WalOnly).unwrap();

    txn.put(table1, b"user:1", b"Alice").unwrap();
    txn.put(table2, b"post:1", b"Hello World").unwrap();
    txn.put(table3, b"comment:1", b"Nice post!").unwrap();

    txn.commit().unwrap();

    // Verify all tables have data
    let txn2 = db.begin_read().unwrap();
    assert_eq!(txn2.get(table1, b"user:1").unwrap().unwrap().0, b"Alice");
    assert_eq!(
        txn2.get(table2, b"post:1").unwrap().unwrap().0,
        b"Hello World"
    );
    assert_eq!(
        txn2.get(table3, b"comment:1").unwrap().unwrap().0,
        b"Nice post!"
    );
}

#[test]
fn test_transaction_isolation() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    // Create a table first
    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();

    // Transaction 1: Insert initial data
    {
        let mut txn1 = db.begin_write(Durability::WalOnly).unwrap();
        txn1.put(table_id, b"key1", b"value1").unwrap();
        txn1.commit().unwrap();
    }

    // Transaction 2: Start but don't commit
    let mut txn2 = db.begin_write(Durability::WalOnly).unwrap();
    txn2.put(table_id, b"key2", b"value2").unwrap();

    // Transaction 3: Should not see txn2's uncommitted changes
    let txn3 = db.begin_read().unwrap();
    assert!(txn3.get(table_id, b"key2").unwrap().is_none());
    assert_eq!(txn3.get(table_id, b"key1").unwrap().unwrap().0, b"value1");

    // Now commit txn2
    txn2.commit().unwrap();

    // Transaction 4: Should see txn2's committed changes
    let txn4 = db.begin_read().unwrap();
    assert_eq!(txn4.get(table_id, b"key2").unwrap().unwrap().0, b"value2");
}

#[test]
fn test_transaction_delete_nonexistent_key() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    // Create a table first
    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();
    let mut txn = db.begin_write(Durability::WalOnly).unwrap();

    // Delete a key that doesn't exist
    let existed = txn.delete(table_id, b"nonexistent").unwrap();
    assert!(!existed);

    txn.commit().unwrap();
}

#[test]
fn test_transaction_multiple_operations_same_key() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    // Create a table first
    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();
    let mut txn = db.begin_write(Durability::WalOnly).unwrap();

    // Multiple operations on the same key within a transaction
    txn.put(table_id, b"key1", b"value1").unwrap();
    txn.put(table_id, b"key1", b"value2").unwrap();
    txn.put(table_id, b"key1", b"value3").unwrap();

    // Should see the latest value
    let value = txn.get(table_id, b"key1").unwrap();
    assert_eq!(value.unwrap().0, b"value3");

    txn.commit().unwrap();

    // Verify final value after commit
    let txn2 = db.begin_read().unwrap();
    let value = txn2.get(table_id, b"key1").unwrap();
    assert_eq!(value.unwrap().0, b"value3");
}

#[test]
fn test_transaction_put_delete_put() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    // Create a table first
    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();
    let mut txn = db.begin_write(Durability::WalOnly).unwrap();

    // Put, delete, then put again
    txn.put(table_id, b"key1", b"value1").unwrap();
    txn.delete(table_id, b"key1").unwrap();
    txn.put(table_id, b"key1", b"value2").unwrap();

    // Should see the final put
    let value = txn.get(table_id, b"key1").unwrap();
    assert_eq!(value.unwrap().0, b"value2");

    txn.commit().unwrap();

    // Verify after commit
    let txn2 = db.begin_read().unwrap();
    let value = txn2.get(table_id, b"key1").unwrap();
    assert_eq!(value.unwrap().0, b"value2");
}

#[test]
fn test_empty_transaction_commit() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    // Empty transaction should commit successfully
    let txn = db.begin_write(Durability::WalOnly).unwrap();
    let commit_info = txn.commit().unwrap();
    assert!(commit_info.durable_lsn.is_some());
}

#[test]
fn test_empty_transaction_rollback() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    // Empty transaction should rollback successfully
    let txn = db.begin_write(Durability::WalOnly).unwrap();
    txn.rollback().unwrap();
}

#[test]
fn test_sequential_transactions() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    // Create a table first
    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();

    // Transaction 1
    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.put(table_id, b"key1", b"value1").unwrap();
        txn.commit().unwrap();
    }

    // Transaction 2
    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.put(table_id, b"key2", b"value2").unwrap();
        txn.commit().unwrap();
    }

    // Transaction 3
    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.put(table_id, b"key3", b"value3").unwrap();
        txn.commit().unwrap();
    }

    // Verify all data
    let txn = db.begin_read().unwrap();
    assert_eq!(txn.get(table_id, b"key1").unwrap().unwrap().0, b"value1");
    assert_eq!(txn.get(table_id, b"key2").unwrap().unwrap().0, b"value2");
    assert_eq!(txn.get(table_id, b"key3").unwrap().unwrap().0, b"value3");
}

/// Range delete should remove only keys in the requested bounds and leave other
/// tables untouched when committed.
#[test]
fn test_transaction_range_delete_commit_multi_table() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    let table1 = db.create_table("table1", default_table_options()).unwrap();
    let table2 = db.create_table("table2", default_table_options()).unwrap();

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.put(table1, b"a1", b"v1").unwrap();
        txn.put(table1, b"a2", b"v2").unwrap();
        txn.put(table1, b"b1", b"v3").unwrap();
        txn.put(table2, b"a1", b"other").unwrap();
        txn.commit().unwrap();
    }

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        let deleted = txn
            .range_delete(table1, ScanBounds::Prefix(KeyBuf(b"a".to_vec())))
            .unwrap();
        assert_eq!(deleted, 2);
        txn.commit().unwrap();
    }

    let txn = db.begin_read().unwrap();
    assert!(txn.get(table1, b"a1").unwrap().is_none());
    assert!(txn.get(table1, b"a2").unwrap().is_none());
    assert_eq!(txn.get(table1, b"b1").unwrap().unwrap().0, b"v3");
    assert_eq!(txn.get(table2, b"a1").unwrap().unwrap().0, b"other");
}

/// Range delete should be visible inside the transaction but rolled back when
/// the transaction aborts.
#[test]
fn test_transaction_range_delete_rollback() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.put(table_id, b"key1", b"value1").unwrap();
        txn.put(table_id, b"key2", b"value2").unwrap();
        txn.put(table_id, b"key3", b"value3").unwrap();
        txn.commit().unwrap();
    }

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        let deleted = txn
            .range_delete(
                table_id,
                ScanBounds::Range {
                    start: Bound::Included(KeyBuf(b"key1".to_vec())),
                    end: Bound::Included(KeyBuf(b"key2".to_vec())),
                },
            )
            .unwrap();
        assert_eq!(deleted, 2);

        assert!(txn.get(table_id, b"key1").unwrap().is_none());
        assert!(txn.get(table_id, b"key2").unwrap().is_none());
        assert_eq!(txn.get(table_id, b"key3").unwrap().unwrap().0, b"value3");

        txn.rollback().unwrap();
    }

    let txn = db.begin_read().unwrap();
    assert_eq!(txn.get(table_id, b"key1").unwrap().unwrap().0, b"value1");
    assert_eq!(txn.get(table_id, b"key2").unwrap().unwrap().0, b"value2");
    assert_eq!(txn.get(table_id, b"key3").unwrap().unwrap().0, b"value3");
}

#[test]
fn test_snapshot_lifecycle_create_list_release() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    let snapshot = db.create_snapshot("backup-1").unwrap();
    assert_eq!(snapshot.name, "backup-1");
    assert_eq!(snapshot.lsn.as_u64(), 0);

    let snapshots = db.list_snapshots().unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0], snapshot);

    db.release_snapshot(snapshot.id).unwrap();
    assert!(db.list_snapshots().unwrap().is_empty());
}

#[test]
fn test_snapshot_duplicate_name_rejected() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    db.create_snapshot("backup").unwrap();
    let err = db.create_snapshot("backup").unwrap_err();
    assert_eq!(err.kind, DatabaseErrorKind::InvalidOperation);
    assert!(err.message.contains("already exists"));
}

#[test]
fn test_begin_read_at_named_snapshot_is_allowed_while_pinned() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.put(table_id, b"key1", b"value1").unwrap();
        txn.commit().unwrap();
    }

    let snapshot = db.create_snapshot("named-read").unwrap();
    assert!(snapshot.lsn.as_u64() > 0);

    let historical_txn = db.begin_read_at(snapshot.lsn).unwrap();
    assert_eq!(
        historical_txn.snapshot_lsn().as_u64(),
        snapshot.lsn.as_u64()
    );
    assert_eq!(
        historical_txn.get(table_id, b"key1").unwrap().unwrap().0,
        b"value1"
    );

    let current_txn = db.begin_read().unwrap();
    assert_eq!(
        current_txn.get(table_id, b"key1").unwrap().unwrap().0,
        b"value1"
    );
}

#[test]
fn test_begin_read_at_unpinned_lsn_rejected_after_snapshot_release() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.put(table_id, b"key1", b"value1").unwrap();
        txn.commit().unwrap();
    }

    let snapshot = db.create_snapshot("to-release").unwrap();
    db.release_snapshot(snapshot.id).unwrap();

    let err = match db.begin_read_at(snapshot.lsn) {
        Ok(_) => panic!("expected released snapshot LSN to be rejected"),
        Err(err) => err,
    };
    assert_eq!(err.kind, DatabaseErrorKind::InvalidOperation);
    assert!(err.message.contains("not pinned"));

    let current_txn = db.begin_read().unwrap();
    assert_eq!(
        current_txn.get(table_id, b"key1").unwrap().unwrap().0,
        b"value1"
    );
}

#[test]
fn test_begin_read_at_future_lsn_rejected() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    let err = match db.begin_read_at(1_u64.into()) {
        Ok(_) => panic!("expected future LSN to be rejected"),
        Err(err) => err,
    };
    assert_eq!(err.kind, DatabaseErrorKind::InvalidOperation);
    assert!(err.message.contains("not yet committed"));
}

#[test]
fn test_transaction_bloom_insert_commit() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    let bloom_id = db
        .create_table("bloom_table", bloom_table_options())
        .unwrap();

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();

        txn.with_bloom(bloom_id, |bloom| {
            bloom.insert_key(
                b"member-1",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )?;
            assert!(bloom.might_contain(b"member-1")?);
            Ok(())
        })
        .unwrap();

        txn.commit().unwrap();
    }

    let mut read_txn = db.begin_read().unwrap();
    read_txn
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b"member-1"))
        .unwrap();
}

#[test]
fn test_transaction_bloom_insert_rollback() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    let bloom_id = db
        .create_table("bloom_table", bloom_table_options())
        .unwrap();

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();

        txn.with_bloom(bloom_id, |bloom| {
            bloom.insert_key(
                b"member-rollback",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )?;
            assert!(bloom.might_contain(b"member-rollback")?);
            Ok(())
        })
        .unwrap();

        txn.rollback().unwrap();
    }

    let mut read_txn = db.begin_read().unwrap();
    let contains = read_txn
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b"member-rollback"))
        .unwrap();
    assert!(!contains);
}

#[test]
fn test_transaction_mixed_kv_and_bloom_commit() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();

    let table_id = db
        .create_table("test_table", default_table_options())
        .unwrap();
    let bloom_id = db
        .create_table("bloom_table", bloom_table_options())
        .unwrap();

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.put(table_id, b"key1", b"value1").unwrap();
        txn.with_bloom(bloom_id, |bloom| {
            bloom.insert_key(b"key1", TransactionId::from(1), LogSequenceNumber::from(1))?;
            Ok(())
        })
        .unwrap();
        txn.commit().unwrap();
    }

    let txn = db.begin_read().unwrap();
    assert_eq!(txn.get(table_id, b"key1").unwrap().unwrap().0, b"value1");

    let mut bloom_txn = db.begin_read().unwrap();
    let contains = bloom_txn
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b"key1"))
        .unwrap();
    assert!(contains);
}

// Made with Bob
