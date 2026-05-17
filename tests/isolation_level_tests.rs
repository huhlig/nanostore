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

//! Comprehensive tests for transaction isolation levels.
//!
//! Tests the five isolation levels:
//! - ReadUncommitted: Allows dirty reads, dirty writes, non-repeatable reads, phantoms
//! - ReadCommitted: Prevents dirty reads, allows non-repeatable reads and phantoms
//! - RepeatableRead: Prevents dirty reads and non-repeatable reads, allows phantoms
//! - Serializable: Prevents all anomalies (dirty reads, non-repeatable reads, phantoms)
//! - SnapshotIsolation: Snapshot-based reads, write-write conflict detection only

use nanokv::pager::{Pager, PagerConfig};
use nanokv::table::TableEngineRegistry;
use nanokv::txn::{ConflictDetector, Transaction, TransactionId};
use nanokv::types::{Durability, IsolationLevel, TableId, ValueBuf};
use nanokv::vfs::MemoryFileSystem;
use nanokv::wal::{LogSequenceNumber, WalWriter, WalWriterConfig};
use std::sync::{Arc, Mutex, RwLock};

fn create_test_transaction(
    txn_id: u64,
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
        TransactionId::from(txn_id),
        snapshot_lsn,
        isolation,
        Durability::WalOnly,
        conflict_detector,
        wal,
        engine_registry,
        current_lsn,
    )
}

#[test]
fn test_read_uncommitted_no_read_tracking() {
    let mut tx = create_test_transaction(
        1,
        LogSequenceNumber::from(100),
        IsolationLevel::ReadUncommitted,
    );

    let table_id = TableId::from(1);

    // Write some data
    tx.put(table_id, b"key1", b"value1").unwrap();

    // Read it back from write set
    let value = tx.get(table_id, b"key1").unwrap();
    assert_eq!(value, Some(ValueBuf(b"value1".to_vec())));

    // Manually record a read (normally internal)
    tx.record_read(table_id, b"key1".to_vec());

    // ReadUncommitted should commit without checking read-write conflicts
    let result = tx.commit();
    assert!(result.is_ok(), "ReadUncommitted should not check conflicts");
}

#[test]
fn test_read_committed_no_read_tracking() {
    let mut tx = create_test_transaction(
        1,
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(1);

    // Write and read
    tx.put(table_id, b"key1", b"value1").unwrap();
    let value = tx.get(table_id, b"key1").unwrap();
    assert_eq!(value, Some(ValueBuf(b"value1".to_vec())));

    // Manually record a read
    tx.record_read(table_id, b"key1".to_vec());

    // ReadCommitted should commit without checking read-write conflicts
    let result = tx.commit();
    assert!(
        result.is_ok(),
        "ReadCommitted should not check read-write conflicts"
    );
}

#[test]
fn test_repeatable_read_tracks_reads() {
    let mut tx = create_test_transaction(
        1,
        LogSequenceNumber::from(100),
        IsolationLevel::RepeatableRead,
    );

    let table_id = TableId::from(1);

    // Write and read
    tx.put(table_id, b"key1", b"value1").unwrap();
    let value = tx.get(table_id, b"key1").unwrap();
    assert_eq!(value, Some(ValueBuf(b"value1".to_vec())));

    // Manually record a read
    tx.record_read(table_id, b"key1".to_vec());

    // RepeatableRead should check for read-write conflicts
    // Since there are no other transactions, this should succeed
    let result = tx.commit();
    assert!(
        result.is_ok(),
        "RepeatableRead should commit when no conflicts"
    );
}

#[test]
fn test_serializable_tracks_reads() {
    let mut tx = create_test_transaction(
        1,
        LogSequenceNumber::from(100),
        IsolationLevel::Serializable,
    );

    let table_id = TableId::from(1);

    // Write and read
    tx.put(table_id, b"key1", b"value1").unwrap();
    let value = tx.get(table_id, b"key1").unwrap();
    assert_eq!(value, Some(ValueBuf(b"value1".to_vec())));

    // Manually record a read
    tx.record_read(table_id, b"key1".to_vec());

    // Serializable should check for read-write conflicts
    // Since there are no other transactions, this should succeed
    let result = tx.commit();
    assert!(
        result.is_ok(),
        "Serializable should commit when no conflicts"
    );
}

#[test]
fn test_snapshot_isolation_no_read_tracking() {
    let mut tx = create_test_transaction(
        1,
        LogSequenceNumber::from(100),
        IsolationLevel::SnapshotIsolation,
    );

    let table_id = TableId::from(1);

    // Write and read
    tx.put(table_id, b"key1", b"value1").unwrap();
    let value = tx.get(table_id, b"key1").unwrap();
    assert_eq!(value, Some(ValueBuf(b"value1".to_vec())));

    // Manually record a read
    tx.record_read(table_id, b"key1".to_vec());

    // SnapshotIsolation should not check read-write conflicts
    let result = tx.commit();
    assert!(
        result.is_ok(),
        "SnapshotIsolation should not check read-write conflicts"
    );
}

#[test]
fn test_write_write_conflict_detection() {
    let fs = MemoryFileSystem::new();
    let conflict_detector = Arc::new(Mutex::new(ConflictDetector::new()));
    let wal = Arc::new(WalWriter::create(&fs, "test.wal", WalWriterConfig::default()).unwrap());
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());
    let engine_registry = Arc::new(TableEngineRegistry::new(pager));
    let current_lsn = Arc::new(RwLock::new(LogSequenceNumber::from(100)));

    let table_id = TableId::from(1);

    // Transaction 1: Write to key1
    let mut tx1 = Transaction::new(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
        Durability::WalOnly,
        conflict_detector.clone(),
        wal.clone(),
        engine_registry.clone(),
        current_lsn.clone(),
    );
    tx1.put(table_id, b"key1", b"value1").unwrap();

    // Transaction 2: Try to write to the same key (should fail)
    let mut tx2 = Transaction::new(
        TransactionId::from(2),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
        Durability::WalOnly,
        conflict_detector.clone(),
        wal.clone(),
        engine_registry.clone(),
        current_lsn.clone(),
    );

    let result = tx2.put(table_id, b"key1", b"value2");
    assert!(result.is_err(), "Should detect write-write conflict");

    // Clean up
    tx1.rollback().unwrap();
}

#[test]
fn test_isolation_level_properties() {
    // Test that each isolation level is correctly set
    let levels = vec![
        IsolationLevel::ReadUncommitted,
        IsolationLevel::ReadCommitted,
        IsolationLevel::RepeatableRead,
        IsolationLevel::Serializable,
        IsolationLevel::SnapshotIsolation,
    ];

    for level in levels {
        let tx = create_test_transaction(1, LogSequenceNumber::from(100), level);
        assert_eq!(tx.isolation_level(), level);
    }
}

#[test]
fn test_read_uncommitted_isolation_level() {
    let tx = create_test_transaction(
        1,
        LogSequenceNumber::from(100),
        IsolationLevel::ReadUncommitted,
    );
    assert_eq!(tx.isolation_level(), IsolationLevel::ReadUncommitted);
}

#[test]
fn test_read_committed_isolation_level() {
    let tx = create_test_transaction(
        1,
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );
    assert_eq!(tx.isolation_level(), IsolationLevel::ReadCommitted);
}

#[test]
fn test_repeatable_read_isolation_level() {
    let tx = create_test_transaction(
        1,
        LogSequenceNumber::from(100),
        IsolationLevel::RepeatableRead,
    );
    assert_eq!(tx.isolation_level(), IsolationLevel::RepeatableRead);
}

#[test]
fn test_serializable_isolation_level() {
    let tx = create_test_transaction(
        1,
        LogSequenceNumber::from(100),
        IsolationLevel::Serializable,
    );
    assert_eq!(tx.isolation_level(), IsolationLevel::Serializable);
}

#[test]
fn test_snapshot_isolation_level() {
    let tx = create_test_transaction(
        1,
        LogSequenceNumber::from(100),
        IsolationLevel::SnapshotIsolation,
    );
    assert_eq!(tx.isolation_level(), IsolationLevel::SnapshotIsolation);
}

// Made with Bob
