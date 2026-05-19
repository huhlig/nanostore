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

//! Tests for MVCC conflict detection

use nanostore::txn::{ConflictDetector, TransactionError, TransactionId};
use nanostore::types::TableId;
use std::collections::HashSet;

#[test]
fn test_no_conflict_different_keys() {
    let mut detector = ConflictDetector::new();
    let txn1 = TransactionId::from(1);
    let txn2 = TransactionId::from(2);
    let table = TableId::from(1);

    // Transaction 1 locks key "a"
    detector.acquire_write_lock(table, b"a".to_vec(), txn1);

    // Transaction 2 should be able to lock key "b" without conflict
    let result = detector.check_write_conflict(table, b"b", txn2);
    assert!(result.is_ok());

    detector.acquire_write_lock(table, b"b".to_vec(), txn2);
}

#[test]
fn test_write_write_conflict_same_key() {
    let mut detector = ConflictDetector::new();
    let txn1 = TransactionId::from(1);
    let txn2 = TransactionId::from(2);
    let table = TableId::from(1);

    // Transaction 1 locks key "a"
    detector.acquire_write_lock(table, b"a".to_vec(), txn1);

    // Transaction 2 tries to lock the same key - should conflict
    let result = detector.check_write_conflict(table, b"a", txn2);
    assert!(result.is_err());

    match result {
        Err(TransactionError::WriteWriteConflict {
            object_id,
            key,
            holder_txn_id,
            requester_txn_id,
        }) => {
            assert_eq!(object_id, table);
            assert_eq!(key, b"a");
            assert_eq!(holder_txn_id, txn1);
            assert_eq!(requester_txn_id, txn2);
        }
        _ => panic!("Expected WriteWriteConflict error"),
    }
}

#[test]
fn test_same_transaction_can_relock() {
    let mut detector = ConflictDetector::new();
    let txn1 = TransactionId::from(1);
    let table = TableId::from(1);

    // Transaction 1 locks key "a"
    detector.acquire_write_lock(table, b"a".to_vec(), txn1);

    // Same transaction can check the same key without conflict
    let result = detector.check_write_conflict(table, b"a", txn1);
    assert!(result.is_ok());

    // And can re-acquire the lock
    detector.acquire_write_lock(table, b"a".to_vec(), txn1);
}

#[test]
fn test_release_locks() {
    let mut detector = ConflictDetector::new();
    let txn1 = TransactionId::from(1);
    let txn2 = TransactionId::from(2);
    let table = TableId::from(1);

    // Transaction 1 locks multiple keys
    detector.acquire_write_lock(table, b"a".to_vec(), txn1);
    detector.acquire_write_lock(table, b"b".to_vec(), txn1);
    detector.acquire_write_lock(table, b"c".to_vec(), txn1);

    // Transaction 2 should conflict on all keys
    assert!(detector.check_write_conflict(table, b"a", txn2).is_err());
    assert!(detector.check_write_conflict(table, b"b", txn2).is_err());
    assert!(detector.check_write_conflict(table, b"c", txn2).is_err());

    // Release all locks for transaction 1
    detector.release_locks(txn1);

    // Transaction 2 should now be able to lock all keys
    assert!(detector.check_write_conflict(table, b"a", txn2).is_ok());
    assert!(detector.check_write_conflict(table, b"b", txn2).is_ok());
    assert!(detector.check_write_conflict(table, b"c", txn2).is_ok());
}

#[test]
fn test_release_locks_selective() {
    let mut detector = ConflictDetector::new();
    let txn1 = TransactionId::from(1);
    let txn2 = TransactionId::from(2);
    let table = TableId::from(1);

    // Both transactions lock different keys
    detector.acquire_write_lock(table, b"a".to_vec(), txn1);
    detector.acquire_write_lock(table, b"b".to_vec(), txn2);

    // Release only transaction 1's locks
    detector.release_locks(txn1);

    // Transaction 2's lock should still be held
    assert!(detector.check_write_conflict(table, b"b", txn1).is_err());

    // But transaction 1's lock should be released
    assert!(detector.check_write_conflict(table, b"a", txn2).is_ok());
}

#[test]
fn test_different_tables_no_conflict() {
    let mut detector = ConflictDetector::new();
    let txn1 = TransactionId::from(1);
    let txn2 = TransactionId::from(2);
    let table1 = TableId::from(1);
    let table2 = TableId::from(2);

    // Transaction 1 locks key "a" in table 1
    detector.acquire_write_lock(table1, b"a".to_vec(), txn1);

    // Transaction 2 should be able to lock key "a" in table 2
    let result = detector.check_write_conflict(table2, b"a", txn2);
    assert!(result.is_ok());

    detector.acquire_write_lock(table2, b"a".to_vec(), txn2);
}

#[test]
fn test_read_write_conflict_detection() {
    let mut detector = ConflictDetector::new();
    let txn1 = TransactionId::from(1);
    let txn2 = TransactionId::from(2);
    let table = TableId::from(1);

    // Transaction 1 locks key "a" for writing
    detector.acquire_write_lock(table, b"a".to_vec(), txn1);

    // Transaction 2 has read key "a" and "b"
    let mut read_set = HashSet::new();
    read_set.insert((table, b"a".to_vec()));
    read_set.insert((table, b"b".to_vec()));

    // Should detect read-write conflict on key "a"
    let result = detector.check_read_write_conflicts(&read_set, txn2);
    assert!(result.is_err());

    match result {
        Err(TransactionError::ReadWriteConflict {
            object_id,
            key,
            reader_txn_id,
            writer_txn_id,
        }) => {
            assert_eq!(object_id, table);
            assert_eq!(key, b"a");
            assert_eq!(reader_txn_id, txn2);
            assert_eq!(writer_txn_id, txn1);
        }
        _ => panic!("Expected ReadWriteConflict error"),
    }
}

#[test]
fn test_read_write_no_conflict_different_keys() {
    let mut detector = ConflictDetector::new();
    let txn1 = TransactionId::from(1);
    let txn2 = TransactionId::from(2);
    let table = TableId::from(1);

    // Transaction 1 locks key "a" for writing
    detector.acquire_write_lock(table, b"a".to_vec(), txn1);

    // Transaction 2 has read only key "b"
    let mut read_set = HashSet::new();
    read_set.insert((table, b"b".to_vec()));

    // Should not detect conflict
    let result = detector.check_read_write_conflicts(&read_set, txn2);
    assert!(result.is_ok());
}

#[test]
fn test_read_write_same_transaction_no_conflict() {
    let mut detector = ConflictDetector::new();
    let txn1 = TransactionId::from(1);
    let table = TableId::from(1);

    // Transaction 1 locks key "a" for writing
    detector.acquire_write_lock(table, b"a".to_vec(), txn1);

    // Same transaction has read key "a"
    let mut read_set = HashSet::new();
    read_set.insert((table, b"a".to_vec()));

    // Should not conflict with own writes
    let result = detector.check_read_write_conflicts(&read_set, txn1);
    assert!(result.is_ok());
}

#[test]
fn test_empty_read_set() {
    let detector = ConflictDetector::new();
    let txn1 = TransactionId::from(1);
    let read_set = HashSet::new();

    // Empty read set should never conflict
    let result = detector.check_read_write_conflicts(&read_set, txn1);
    assert!(result.is_ok());
}

#[test]
fn test_multiple_transactions_complex_scenario() {
    let mut detector = ConflictDetector::new();
    let txn1 = TransactionId::from(1);
    let txn2 = TransactionId::from(2);
    let txn3 = TransactionId::from(3);
    let table = TableId::from(1);

    // Transaction 1 locks keys "a" and "b"
    detector.acquire_write_lock(table, b"a".to_vec(), txn1);
    detector.acquire_write_lock(table, b"b".to_vec(), txn1);

    // Transaction 2 locks key "c"
    detector.acquire_write_lock(table, b"c".to_vec(), txn2);

    // Transaction 3 should conflict on "a" and "b" but not "c"
    assert!(detector.check_write_conflict(table, b"a", txn3).is_err());
    assert!(detector.check_write_conflict(table, b"b", txn3).is_err());
    assert!(detector.check_write_conflict(table, b"c", txn3).is_err());

    // Release transaction 1's locks
    detector.release_locks(txn1);

    // Now transaction 3 can lock "a" and "b" but not "c"
    assert!(detector.check_write_conflict(table, b"a", txn3).is_ok());
    assert!(detector.check_write_conflict(table, b"b", txn3).is_ok());
    assert!(detector.check_write_conflict(table, b"c", txn3).is_err());

    // Acquire locks for transaction 3
    detector.acquire_write_lock(table, b"a".to_vec(), txn3);
    detector.acquire_write_lock(table, b"b".to_vec(), txn3);

    // Release transaction 2's locks
    detector.release_locks(txn2);

    // Now transaction 3 can lock "c"
    assert!(detector.check_write_conflict(table, b"c", txn3).is_ok());
}

// Made with Bob
