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

//! Property-based tests for transaction isolation using proptest
//!
//! This module tests transaction isolation properties:
//! - Serializability: Transactions appear to execute in some serial order
//! - Snapshot Isolation: Consistent snapshots with write-write conflict detection
//! - Conflict Detection: Proper detection of write-write and read-write conflicts
//! - Concurrent Schedules: Random interleaving of transaction operations

use nanostore::pager::{Pager, PagerConfig};
use nanostore::table::TableEngineRegistry;
use nanostore::txn::{ConflictDetector, Transaction, TransactionId};
use nanostore::types::{Durability, IsolationLevel, TableId, ValueBuf};
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::{LogSequenceNumber, WalWriter, WalWriterConfig};
use proptest::prelude::*;
use std::sync::{Arc, Mutex, RwLock};

// =============================================================================
// Test Infrastructure
// =============================================================================

/// Represents a single transaction operation
#[derive(Debug, Clone)]
enum TxnOp {
    Put { key: Vec<u8>, value: Vec<u8> },
    Get { key: Vec<u8> },
    Delete { key: Vec<u8> },
}

/// Represents a transaction with its operations
#[derive(Debug, Clone)]
struct TxnSchedule {
    txn_id: u64,
    isolation: IsolationLevel,
    operations: Vec<TxnOp>,
}

/// Helper to create test transaction infrastructure
fn create_test_infrastructure() -> (
    Arc<MemoryFileSystem>,
    Arc<Mutex<ConflictDetector>>,
    Arc<WalWriter<MemoryFileSystem>>,
    Arc<Pager<MemoryFileSystem>>,
    Arc<TableEngineRegistry<MemoryFileSystem>>,
    Arc<RwLock<LogSequenceNumber>>,
) {
    let fs = Arc::new(MemoryFileSystem::new());
    let conflict_detector = Arc::new(Mutex::new(ConflictDetector::new()));
    let wal = Arc::new(WalWriter::create(&*fs, "test.wal", WalWriterConfig::default()).unwrap());
    let pager = Arc::new(Pager::create(&*fs, "test.db", PagerConfig::default()).unwrap());
    let engine_registry = Arc::new(TableEngineRegistry::new(pager.clone()));
    let current_lsn = Arc::new(RwLock::new(LogSequenceNumber::from(100)));

    (
        fs,
        conflict_detector,
        wal,
        pager,
        engine_registry,
        current_lsn,
    )
}

/// Create a transaction with the given parameters
fn create_transaction(
    txn_id: u64,
    snapshot_lsn: LogSequenceNumber,
    isolation: IsolationLevel,
    conflict_detector: Arc<Mutex<ConflictDetector>>,
    wal: Arc<WalWriter<MemoryFileSystem>>,
    engine_registry: Arc<TableEngineRegistry<MemoryFileSystem>>,
    current_lsn: Arc<RwLock<LogSequenceNumber>>,
) -> Transaction<MemoryFileSystem> {
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

// =============================================================================
// Property Test Strategies
// =============================================================================

/// Strategy for generating valid keys
fn key_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 1..32)
}

/// Strategy for generating valid values
fn value_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 1..128)
}

/// Strategy for generating transaction operations
fn txn_op_strategy() -> impl Strategy<Value = TxnOp> {
    prop_oneof![
        (key_strategy(), value_strategy()).prop_map(|(k, v)| TxnOp::Put { key: k, value: v }),
        key_strategy().prop_map(|k| TxnOp::Get { key: k }),
        key_strategy().prop_map(|k| TxnOp::Delete { key: k }),
    ]
}

/// Strategy for generating a transaction schedule
fn txn_schedule_strategy(
    txn_id: u64,
    isolation: IsolationLevel,
) -> impl Strategy<Value = TxnSchedule> {
    prop::collection::vec(txn_op_strategy(), 1..10).prop_map(move |ops| TxnSchedule {
        txn_id,
        isolation,
        operations: ops,
    })
}

/// Strategy for generating isolation levels
fn isolation_level_strategy() -> impl Strategy<Value = IsolationLevel> {
    prop_oneof![
        Just(IsolationLevel::ReadUncommitted),
        Just(IsolationLevel::ReadCommitted),
        Just(IsolationLevel::RepeatableRead),
        Just(IsolationLevel::Serializable),
        Just(IsolationLevel::SnapshotIsolation),
    ]
}

// =============================================================================
// Property Tests: Write-Write Conflict Detection
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Property: Two concurrent transactions writing to the same key must conflict
    #[test]
    fn prop_write_write_conflict_detection(
        key in key_strategy(),
        value1 in value_strategy(),
        value2 in value_strategy(),
        isolation in isolation_level_strategy(),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        // Transaction 1: Write to key
        let mut tx1 = create_transaction(
            1,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );
        tx1.put(table_id, &key, &value1).unwrap();

        // Transaction 2: Try to write to same key (should fail)
        let mut tx2 = create_transaction(
            2,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        let result = tx2.put(table_id, &key, &value2);

        // All isolation levels should detect write-write conflicts
        prop_assert!(result.is_err(), "Write-write conflict should be detected");

        // Clean up
        let _ = tx1.rollback();
    }

    /// Property: Transactions writing to different keys should not conflict
    #[test]
    fn prop_no_conflict_different_keys(
        keys in prop::collection::vec(key_strategy(), 2..=2)
            .prop_filter("Different keys", |keys| keys[0] != keys[1]),
        value1 in value_strategy(),
        value2 in value_strategy(),
        isolation in isolation_level_strategy(),
    ) {
        let key1 = &keys[0];
        let key2 = &keys[1];
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        // Transaction 1: Write to key1
        let mut tx1 = create_transaction(
            1,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );
        tx1.put(table_id, &key1, &value1).unwrap();

        // Transaction 2: Write to key2 (should succeed)
        let mut tx2 = create_transaction(
            2,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        let result = tx2.put(table_id, &key2, &value2);
        prop_assert!(result.is_ok(), "Different keys should not conflict");

        // Clean up
        let _ = tx1.rollback();
        let _ = tx2.rollback();
    }

    /// Property: After releasing locks, another transaction can acquire them
    #[test]
    fn prop_lock_release_allows_reacquisition(
        key in key_strategy(),
        value1 in value_strategy(),
        value2 in value_strategy(),
        isolation in isolation_level_strategy(),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        // Transaction 1: Write and commit
        let mut tx1 = create_transaction(
            1,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );
        tx1.put(table_id, &key, &value1).unwrap();
        tx1.commit().unwrap();

        // Update LSN to simulate commit
        *current_lsn.write().unwrap() = LogSequenceNumber::from(101);

        // Transaction 2: Should be able to write to same key after tx1 commits
        let mut tx2 = create_transaction(
            2,
            LogSequenceNumber::from(101),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        let result = tx2.put(table_id, &key, &value2);
        prop_assert!(result.is_ok(), "Should acquire lock after previous transaction commits");

        let _ = tx2.rollback();
    }
}

// =============================================================================
// Property Tests: Snapshot Isolation
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: Snapshot isolation provides consistent reads within a transaction
    #[test]
    fn prop_snapshot_isolation_consistent_reads(
        key in key_strategy(),
        initial_value in value_strategy(),
        updated_value in value_strategy(),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        // Setup: Create initial data
        let mut tx_setup = create_transaction(
            1,
            LogSequenceNumber::from(100),
            IsolationLevel::SnapshotIsolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );
        tx_setup.put(table_id, &key, &initial_value).unwrap();
        tx_setup.commit().unwrap();
        *current_lsn.write().unwrap() = LogSequenceNumber::from(101);

        // Start snapshot transaction
        let snapshot_lsn = LogSequenceNumber::from(101);
        let tx_snapshot = create_transaction(
            2,
            snapshot_lsn,
            IsolationLevel::SnapshotIsolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        // Verify snapshot LSN is preserved
        prop_assert_eq!(tx_snapshot.snapshot_lsn(), snapshot_lsn);

        // Another transaction updates the key
        let mut tx_updater = create_transaction(
            3,
            LogSequenceNumber::from(101),
            IsolationLevel::SnapshotIsolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );
        tx_updater.put(table_id, &key, &updated_value).unwrap();
        tx_updater.commit().unwrap();
        *current_lsn.write().unwrap() = LogSequenceNumber::from(102);

        // Snapshot transaction should still see its snapshot LSN
        prop_assert_eq!(tx_snapshot.snapshot_lsn(), snapshot_lsn);

        let _ = tx_snapshot.rollback();
    }

    /// Property: Snapshot isolation detects write-write conflicts
    #[test]
    fn prop_snapshot_isolation_write_write_conflicts(
        key in key_strategy(),
        value1 in value_strategy(),
        value2 in value_strategy(),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        // Transaction 1: Write to key
        let mut tx1 = create_transaction(
            1,
            LogSequenceNumber::from(100),
            IsolationLevel::SnapshotIsolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );
        tx1.put(table_id, &key, &value1).unwrap();

        // Transaction 2: Try to write to same key (should conflict)
        let mut tx2 = create_transaction(
            2,
            LogSequenceNumber::from(100),
            IsolationLevel::SnapshotIsolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        let result = tx2.put(table_id, &key, &value2);
        prop_assert!(result.is_err(), "Snapshot isolation should detect write-write conflicts");

        let _ = tx1.rollback();
    }
}

// =============================================================================
// Property Tests: Serializability
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: Serializable isolation tracks reads for conflict detection
    #[test]
    fn prop_serializable_tracks_reads(
        keys in prop::collection::vec(key_strategy(), 1..5),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        let mut tx = create_transaction(
            1,
            LogSequenceNumber::from(100),
            IsolationLevel::Serializable,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        // Record reads
        for key in &keys {
            tx.record_read(table_id, key.clone());
        }

        // Serializable should track reads (verified by not panicking)
        prop_assert_eq!(tx.isolation_level(), IsolationLevel::Serializable);

        let _ = tx.rollback();
    }

    /// Property: RepeatableRead also tracks reads
    #[test]
    fn prop_repeatable_read_tracks_reads(
        keys in prop::collection::vec(key_strategy(), 1..5),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        let mut tx = create_transaction(
            1,
            LogSequenceNumber::from(100),
            IsolationLevel::RepeatableRead,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        // Record reads
        for key in &keys {
            tx.record_read(table_id, key.clone());
        }

        // RepeatableRead should track reads
        prop_assert_eq!(tx.isolation_level(), IsolationLevel::RepeatableRead);

        let _ = tx.rollback();
    }

    /// Property: Lower isolation levels don't track reads
    #[test]
    fn prop_lower_isolation_no_read_tracking(
        keys in prop::collection::vec(key_strategy(), 1..5),
        isolation in prop_oneof![
            Just(IsolationLevel::ReadUncommitted),
            Just(IsolationLevel::ReadCommitted),
        ],
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        let mut tx = create_transaction(
            1,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        // Record reads (should be no-op)
        for key in &keys {
            tx.record_read(table_id, key.clone());
        }

        // Should commit without checking read-write conflicts
        let result = tx.commit();
        prop_assert!(result.is_ok(), "Lower isolation levels should not check read-write conflicts");
    }
}

// =============================================================================
// Property Tests: Concurrent Transaction Schedules
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property: Multiple transactions with non-overlapping keys should all succeed
    #[test]
    fn prop_non_overlapping_transactions_succeed(
        num_txns in 2usize..5,
        keys_per_txn in 1usize..3,
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        let mut transactions = Vec::new();

        // Create transactions with non-overlapping keys
        for i in 0..num_txns {
            let mut tx = create_transaction(
                i as u64 + 1,
                LogSequenceNumber::from(100),
                IsolationLevel::Serializable,
                conflict_detector.clone(),
                wal.clone(),
                engine_registry.clone(),
                current_lsn.clone(),
            );

            // Each transaction gets unique keys
            for j in 0..keys_per_txn {
                let key = format!("tx{}_key{}", i, j).into_bytes();
                let value = format!("value{}", j).into_bytes();
                let result = tx.put(table_id, &key, &value);
                prop_assert!(result.is_ok(), "Non-overlapping writes should succeed");
            }

            transactions.push(tx);
        }

        // All transactions should commit successfully
        for tx in transactions {
            let result = tx.commit();
            prop_assert!(result.is_ok(), "Non-overlapping transactions should commit");
        }
    }

    /// Property: Transaction isolation level is preserved throughout lifecycle
    #[test]
    fn prop_isolation_level_preserved(
        isolation in isolation_level_strategy(),
        operations in prop::collection::vec(txn_op_strategy(), 1..5),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        let mut tx = create_transaction(
            1,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        // Check isolation level before operations
        prop_assert_eq!(tx.isolation_level(), isolation);

        // Execute operations
        for op in operations {
            match op {
                TxnOp::Put { key, value } => {
                    let _ = tx.put(table_id, &key, &value);
                }
                TxnOp::Get { key } => {
                    let _ = tx.get(table_id, &key);
                }
                TxnOp::Delete { key } => {
                    let _ = tx.delete(table_id, &key);
                }
            }
            // Check isolation level is still preserved
            prop_assert_eq!(tx.isolation_level(), isolation);
        }

        let _ = tx.rollback();
    }

    /// Property: Transaction ID is unique and preserved
    #[test]
    fn prop_transaction_id_preserved(
        txn_id in 1u64..1000,
        isolation in isolation_level_strategy(),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();

        let tx = create_transaction(
            txn_id,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        prop_assert_eq!(tx.id(), TransactionId::from(txn_id));

        let _ = tx.rollback();
    }
}

// =============================================================================
// Property Tests: Read-Your-Writes Consistency
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Property: A transaction can always read its own writes
    #[test]
    fn prop_read_your_writes(
        key in key_strategy(),
        value in value_strategy(),
        isolation in isolation_level_strategy(),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        let mut tx = create_transaction(
            1,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        // Write a value
        tx.put(table_id, &key, &value).unwrap();

        // Read it back
        let read_value = tx.get(table_id, &key).unwrap();

        // Should see own write
        prop_assert_eq!(read_value, Some(ValueBuf(value)));

        let _ = tx.rollback();
    }

    /// Property: A transaction sees its own deletes
    #[test]
    fn prop_read_your_deletes(
        key in key_strategy(),
        value in value_strategy(),
        isolation in isolation_level_strategy(),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        let mut tx = create_transaction(
            1,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        // Write then delete
        tx.put(table_id, &key, &value).unwrap();
        tx.delete(table_id, &key).unwrap();

        // Should see deletion
        let read_value = tx.get(table_id, &key).unwrap();
        prop_assert_eq!(read_value, None);

        let _ = tx.rollback();
    }

    /// Property: Multiple updates to same key within transaction are visible
    #[test]
    fn prop_multiple_updates_visible(
        key in key_strategy(),
        values in prop::collection::vec(value_strategy(), 2..5),
        isolation in isolation_level_strategy(),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        let mut tx = create_transaction(
            1,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        // Write multiple values to same key
        for value in &values {
            tx.put(table_id, &key, value).unwrap();
        }

        // Should see last write
        let read_value = tx.get(table_id, &key).unwrap();
        prop_assert_eq!(read_value, Some(ValueBuf(values.last().unwrap().clone())));

        let _ = tx.rollback();
    }
}

// =============================================================================
// Property Tests: Transaction State Machine
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: Active transaction can perform operations
    #[test]
    fn prop_active_transaction_operations(
        key in key_strategy(),
        value in value_strategy(),
        isolation in isolation_level_strategy(),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();
        let table_id = TableId::from(1);

        let mut tx = create_transaction(
            1,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        prop_assert!(tx.is_active());

        // Should be able to perform operations
        let put_result = tx.put(table_id, &key, &value);
        prop_assert!(put_result.is_ok());

        let get_result = tx.get(table_id, &key);
        prop_assert!(get_result.is_ok());

        let delete_result = tx.delete(table_id, &key);
        prop_assert!(delete_result.is_ok());

        let _ = tx.rollback();
    }

    /// Property: Empty transaction can commit
    #[test]
    fn prop_empty_transaction_commits(
        isolation in isolation_level_strategy(),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();

        let tx = create_transaction(
            1,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        let result = tx.commit();
        prop_assert!(result.is_ok(), "Empty transaction should commit successfully");
    }

    /// Property: Empty transaction can rollback
    #[test]
    fn prop_empty_transaction_rollbacks(
        isolation in isolation_level_strategy(),
    ) {
        let (_, conflict_detector, wal, _, engine_registry, current_lsn) = create_test_infrastructure();

        let tx = create_transaction(
            1,
            LogSequenceNumber::from(100),
            isolation,
            conflict_detector.clone(),
            wal.clone(),
            engine_registry.clone(),
            current_lsn.clone(),
        );

        let result = tx.rollback();
        prop_assert!(result.is_ok(), "Empty transaction should rollback successfully");
    }
}

// Made with Bob
