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

//! Integration tests for the Transaction layer.
//!
//! These tests verify the transaction state machine, read/write set tracking,
//! isolation levels, and commit/rollback behavior.

use nanostore::pager::{Pager, PagerConfig};
use nanostore::table::TableEngineRegistry;
use nanostore::txn::{ConflictDetector, Transaction, TransactionId};
use nanostore::types::{Durability, IsolationLevel, TableId};
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::{LogSequenceNumber, WalWriter, WalWriterConfig};
use std::sync::{Arc, Mutex, RwLock};

/// Helper to create a shared conflict detector for tests
fn create_conflict_detector() -> Arc<Mutex<ConflictDetector>> {
    Arc::new(Mutex::new(ConflictDetector::new()))
}

/// Helper to create test dependencies for Transaction
fn create_test_transaction(
    txn_id: TransactionId,
    snapshot_lsn: LogSequenceNumber,
    isolation: IsolationLevel,
) -> Transaction<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let conflict_detector = Arc::new(Mutex::new(ConflictDetector::new()));
    let wal = Arc::new(WalWriter::create(&fs, "test.wal", WalWriterConfig::default()).unwrap());
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());
    let engine_registry = Arc::new(TableEngineRegistry::new(pager));
    let current_lsn = Arc::new(RwLock::new(snapshot_lsn));

    Transaction::new(
        txn_id,
        snapshot_lsn,
        isolation,
        Durability::SyncOnCommit,
        conflict_detector,
        wal,
        engine_registry,
        current_lsn,
    )
}

/// Test basic transaction creation and state
#[test]
fn test_transaction_creation() {
    let tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    assert_eq!(tx.id(), TransactionId::from(1));
    assert_eq!(tx.snapshot_lsn(), LogSequenceNumber::from(100));
    assert_eq!(tx.isolation_level(), IsolationLevel::ReadCommitted);
    assert!(tx.is_active());
}

/// Test transaction put operation
#[test]
fn test_transaction_put() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);
    let key = b"key1";
    let value = b"value1";

    // Put should succeed
    assert!(tx.put(table_id, key, value).is_ok());

    // Should be able to read back from write set
    let result = tx.get(table_id, key).unwrap();
    assert_eq!(result.as_ref().map(|v| v.0.as_slice()), Some(&value[..]));
}

/// Test transaction delete operation
#[test]
fn test_transaction_delete() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);
    let key = b"key1";
    let value = b"value1";

    // Put a value first
    tx.put(table_id, key, value).unwrap();

    // Delete should return true (key existed in write set)
    assert!(tx.delete(table_id, key).unwrap());

    // Reading deleted key should return None
    let result = tx.get(table_id, key).unwrap();
    assert_eq!(result, None);
}

/// Test transaction delete of non-existent key
#[test]
fn test_transaction_delete_nonexistent() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);
    let key = b"nonexistent";

    // Delete of non-existent key should return false
    assert!(!tx.delete(table_id, key).unwrap());
}

/// Test transaction get from write set
#[test]
fn test_transaction_get_from_write_set() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);

    // Put multiple values
    tx.put(table_id, b"key1", b"value1").unwrap();
    tx.put(table_id, b"key2", b"value2").unwrap();
    tx.put(table_id, b"key3", b"value3").unwrap();

    // Should be able to read all values
    assert_eq!(
        tx.get(table_id, b"key1")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"value1"[..])
    );
    assert_eq!(
        tx.get(table_id, b"key2")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"value2"[..])
    );
    assert_eq!(
        tx.get(table_id, b"key3")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"value3"[..])
    );
}

/// Test transaction update (overwrite existing value)
#[test]
fn test_transaction_update() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);
    let key = b"key1";

    // Put initial value
    tx.put(table_id, key, b"value1").unwrap();
    assert_eq!(
        tx.get(table_id, key)
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"value1"[..])
    );

    // Update with new value
    tx.put(table_id, key, b"value2").unwrap();
    assert_eq!(
        tx.get(table_id, key)
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"value2"[..])
    );
}

/// Test transaction commit
#[test]
fn test_transaction_commit() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);
    tx.put(table_id, b"key1", b"value1").unwrap();

    // Commit should succeed
    let commit_info = tx.commit().unwrap();
    assert_eq!(commit_info.tx_id, TransactionId::from(1));
    assert!(commit_info.commit_lsn.as_u64() > 0);
}

/// Test transaction rollback
#[test]
fn test_transaction_rollback() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);
    tx.put(table_id, b"key1", b"value1").unwrap();

    // Rollback should succeed
    assert!(tx.rollback().is_ok());
}

/// Test transaction state transitions
#[test]
fn test_transaction_state_transitions() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    // Initially active
    assert!(tx.is_active());

    // Can prepare
    assert!(tx.prepare().is_ok());

    // After prepare, can commit
    let commit_result = tx.commit();
    assert!(commit_result.is_ok());
}

/// Test transaction operations after commit fail
#[test]
fn test_operations_after_commit_fail() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);
    tx.put(table_id, b"key1", b"value1").unwrap();

    // Commit the transaction
    tx.commit().unwrap();

    // Note: After commit, tx is consumed, so we can't test operations on it
    // This is enforced by Rust's ownership system
}

/// Test transaction with multiple tables
#[test]
fn test_transaction_multiple_tables() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table1 = TableId::from(1);
    let table2 = TableId::from(2);
    let table3 = TableId::from(3);

    // Write to multiple tables
    tx.put(table1, b"key1", b"value1").unwrap();
    tx.put(table2, b"key2", b"value2").unwrap();
    tx.put(table3, b"key3", b"value3").unwrap();

    // Should be able to read from all tables
    assert_eq!(
        tx.get(table1, b"key1")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"value1"[..])
    );
    assert_eq!(
        tx.get(table2, b"key2")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"value2"[..])
    );
    assert_eq!(
        tx.get(table3, b"key3")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"value3"[..])
    );
}

/// Test transaction isolation levels
#[test]
fn test_transaction_isolation_levels() {
    // Test each isolation level can be created
    let levels = [
        IsolationLevel::ReadUncommitted,
        IsolationLevel::ReadCommitted,
        IsolationLevel::RepeatableRead,
        IsolationLevel::Serializable,
        IsolationLevel::SnapshotIsolation,
    ];

    for (i, level) in levels.iter().enumerate() {
        let tx = create_test_transaction(
            TransactionId::from(i as u64),
            LogSequenceNumber::from(100),
            *level,
        );
        assert_eq!(tx.isolation_level(), *level);
    }
}

/// Test transaction read tracking for serializable isolation
#[test]
fn test_transaction_read_tracking_serializable() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::Serializable,
    );

    let table_id = TableId::from(1);

    // Record reads manually (in real implementation, this would be done by get())
    tx.record_read(table_id, b"key1".to_vec());
    tx.record_read(table_id, b"key2".to_vec());
    tx.record_read(table_id, b"key3".to_vec());

    // Read set is tracked internally for conflict detection
    // This test verifies the API works without panicking
}

/// Test transaction read tracking not done for lower isolation levels
#[test]
fn test_transaction_read_tracking_read_committed() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);

    // Record reads - should be no-op for ReadCommitted
    tx.record_read(table_id, b"key1".to_vec());
    tx.record_read(table_id, b"key2".to_vec());

    // Should not affect transaction behavior
    assert!(tx.is_active());
}

/// Test transaction with large write set
#[test]
fn test_transaction_large_write_set() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);

    // Write 1000 key-value pairs
    for i in 0..1000 {
        let key = format!("key{:04}", i);
        let value = format!("value{:04}", i);
        tx.put(table_id, key.as_bytes(), value.as_bytes()).unwrap();
    }

    // Verify all values can be read back
    for i in 0..1000 {
        let key = format!("key{:04}", i);
        let expected_value = format!("value{:04}", i);
        let result = tx.get(table_id, key.as_bytes()).unwrap();
        assert_eq!(
            result.as_ref().map(|v| v.0.as_slice()),
            Some(expected_value.as_bytes())
        );
    }

    // Commit should succeed
    assert!(tx.commit().is_ok());
}

/// Test transaction with mixed operations
#[test]
fn test_transaction_mixed_operations() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);

    // Mix of puts, updates, and deletes
    tx.put(table_id, b"key1", b"value1").unwrap();
    tx.put(table_id, b"key2", b"value2").unwrap();
    tx.put(table_id, b"key3", b"value3").unwrap();

    // Update key2
    tx.put(table_id, b"key2", b"updated_value2").unwrap();

    // Delete key3
    tx.delete(table_id, b"key3").unwrap();

    // Verify final state
    assert_eq!(
        tx.get(table_id, b"key1")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"value1"[..])
    );
    assert_eq!(
        tx.get(table_id, b"key2")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"updated_value2"[..])
    );
    assert_eq!(tx.get(table_id, b"key3").unwrap(), None);
}

/// Test transaction prepare phase
#[test]
fn test_transaction_prepare_phase() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);
    tx.put(table_id, b"key1", b"value1").unwrap();

    // Prepare should succeed
    assert!(tx.prepare().is_ok());

    // After prepare, should be able to commit
    assert!(tx.commit().is_ok());
}

/// Test transaction cannot prepare twice
#[test]
fn test_transaction_cannot_prepare_twice() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    // First prepare succeeds
    assert!(tx.prepare().is_ok());

    // Second prepare should fail
    assert!(tx.prepare().is_err());
}

/// Test transaction with empty write set can commit
#[test]
fn test_transaction_empty_write_set_commit() {
    let tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    // Commit with no writes should succeed
    assert!(tx.commit().is_ok());
}

/// Test transaction with empty write set can rollback
#[test]
fn test_transaction_empty_write_set_rollback() {
    let tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    // Rollback with no writes should succeed
    assert!(tx.rollback().is_ok());
}

/// Test transaction record_write API
#[test]
fn test_transaction_record_write() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);

    // Record writes directly
    tx.record_write(table_id, b"key1".to_vec(), b"value1".to_vec());
    tx.record_write(table_id, b"key2".to_vec(), b"value2".to_vec());

    // Should be able to read back
    assert_eq!(
        tx.get(table_id, b"key1")
            .unwrap()
            .as_ref()
            .map(|v| v.0.as_slice()),
        Some(&b"value1"[..])
    );
}

/// Test transaction record_delete API
#[test]
fn test_transaction_record_delete() {
    let mut tx = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);

    // Put then delete
    tx.record_write(table_id, b"key1".to_vec(), b"value1".to_vec());
    tx.record_delete(table_id, b"key1".to_vec());

    // Should read as None
    assert_eq!(tx.get(table_id, b"key1").unwrap(), None);
}

// Made with Bob
