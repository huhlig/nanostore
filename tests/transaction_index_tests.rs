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

//! Tests for transaction layer unified object operations.
//!
//! These tests verify that the transaction layer correctly handles both tables
//! and indexes uniformly using ObjectId, without automatic index maintenance.

use nanostore::pager::{Pager, PagerConfig};
use nanostore::table::TableEngineRegistry;
use nanostore::txn::{ConflictDetector, Transaction, TransactionId};
use nanostore::types::{Durability, IsolationLevel, TableId};
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::{LogSequenceNumber, WalWriter, WalWriterConfig};
use std::sync::{Arc, Mutex, RwLock};

/// Test context that holds shared resources
struct TestContext {
    fs: MemoryFileSystem,
    conflict_detector: Arc<Mutex<ConflictDetector>>,
    wal: Arc<WalWriter<MemoryFileSystem>>,
    engine_registry: Arc<TableEngineRegistry<MemoryFileSystem>>,
}

impl TestContext {
    fn new() -> Self {
        let fs = MemoryFileSystem::new();
        let conflict_detector = Arc::new(Mutex::new(ConflictDetector::new()));
        let wal = Arc::new(WalWriter::create(&fs, "test.wal", WalWriterConfig::default()).unwrap());
        let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());
        let engine_registry = Arc::new(TableEngineRegistry::new(pager));

        Self {
            fs,
            conflict_detector,
            wal,
            engine_registry,
        }
    }

    fn create_transaction(
        &self,
        txn_id: TransactionId,
        snapshot_lsn: LogSequenceNumber,
        isolation: IsolationLevel,
    ) -> Transaction<MemoryFileSystem> {
        let current_lsn = Arc::new(RwLock::new(snapshot_lsn));

        Transaction::new(
            txn_id,
            snapshot_lsn,
            isolation,
            Durability::SyncOnCommit,
            Arc::clone(&self.conflict_detector),
            Arc::clone(&self.wal),
            Arc::clone(&self.engine_registry),
            current_lsn,
        )
    }
}

/// Helper to create test dependencies for Transaction (for simple tests)
fn create_test_transaction(
    txn_id: TransactionId,
    snapshot_lsn: LogSequenceNumber,
    isolation: IsolationLevel,
) -> Transaction<MemoryFileSystem> {
    let ctx = TestContext::new();
    ctx.create_transaction(txn_id, snapshot_lsn, isolation)
}

#[test]
fn test_index_put_and_get() {
    // Create a transaction
    let mut txn = create_test_transaction(
        TransactionId::from(1),
        LogSequenceNumber::from(100),
        IsolationLevel::ReadCommitted,
    );

    // Create an index ObjectId (indexes are just specialty tables)
    let index_id = TableId::from(1000);

    // Put a value into the index using unified API
    let key = b"index_key_1";
    let value = b"index_value_1";
    txn.put(index_id, key, value).unwrap();

    // Get the value back from the index using unified API
    let result = txn.get(index_id, key).unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().as_ref(), value);
}

#[test]
fn test_index_delete() {
    // Create a transaction
    let mut txn = create_test_transaction(
        TransactionId::from(2),
        LogSequenceNumber::from(200),
        IsolationLevel::ReadCommitted,
    );

    // Create an index ObjectId
    let index_id = TableId::from(2000);

    // Put a value into the index
    let key = b"index_key_2";
    let value = b"index_value_2";
    txn.put(index_id, key, value).unwrap();

    // Verify it exists
    assert!(txn.get(index_id, key).unwrap().is_some());

    // Delete the key using unified API
    let existed = txn.delete(index_id, key).unwrap();
    assert!(existed);

    // Verify it's gone
    assert!(txn.get(index_id, key).unwrap().is_none());
}

#[test]
fn test_index_and_table_operations_independent() {
    // Create a transaction
    let mut txn = create_test_transaction(
        TransactionId::from(3),
        LogSequenceNumber::from(300),
        IsolationLevel::ReadCommitted,
    );

    // Create table and index ObjectIds (both use same type now)
    let table_id = TableId::from(100);
    let index_id = TableId::from(3000);

    // Use the same key for both table and index
    let key = b"shared_key";
    let table_value = b"table_value";
    let index_value = b"index_value";

    // Put into table using unified API
    txn.put(table_id, key, table_value).unwrap();

    // Put into index (explicit operation, not automatic) using unified API
    txn.put(index_id, key, index_value).unwrap();

    // Verify both exist independently using unified API
    let table_result = txn.get(table_id, key).unwrap();
    assert_eq!(table_result.unwrap().as_ref(), table_value);

    let index_result = txn.get(index_id, key).unwrap();
    assert_eq!(index_result.unwrap().as_ref(), index_value);

    // Delete from table doesn't affect index
    txn.delete(table_id, key).unwrap();
    assert!(txn.get(table_id, key).unwrap().is_none());
    assert!(txn.get(index_id, key).unwrap().is_some());
}

#[test]
fn test_index_write_conflict_detection() {
    // Create shared context so transactions share conflict detector
    let ctx = TestContext::new();

    // Create two transactions
    let mut txn1 = ctx.create_transaction(
        TransactionId::from(4),
        LogSequenceNumber::from(400),
        IsolationLevel::ReadCommitted,
    );

    let mut txn2 = ctx.create_transaction(
        TransactionId::from(5),
        LogSequenceNumber::from(400),
        IsolationLevel::ReadCommitted,
    );

    // Create an index ObjectId
    let index_id = TableId::from(4000);
    let key = b"conflict_key";

    // First transaction writes to index using unified API
    txn1.put(index_id, key, b"value1").unwrap();

    // Second transaction tries to write to same index key - should conflict
    let result = txn2.put(index_id, key, b"value2");
    assert!(result.is_err());
}

#[test]
fn test_index_operations_in_write_set() {
    // Create a transaction
    let mut txn = create_test_transaction(
        TransactionId::from(6),
        LogSequenceNumber::from(600),
        IsolationLevel::ReadCommitted,
    );

    // Create table and index ObjectIds
    let table_id = TableId::from(200);
    let index_id = TableId::from(5000);

    // Perform multiple operations using unified API
    txn.put(table_id, b"table_key_1", b"table_value_1").unwrap();
    txn.put(index_id, b"index_key_1", b"index_value_1").unwrap();
    txn.put(table_id, b"table_key_2", b"table_value_2").unwrap();
    txn.put(index_id, b"index_key_2", b"index_value_2").unwrap();

    // All operations should be visible within the transaction using unified API
    assert!(txn.get(table_id, b"table_key_1").unwrap().is_some());
    assert!(txn.get(table_id, b"table_key_2").unwrap().is_some());
    assert!(txn.get(index_id, b"index_key_1").unwrap().is_some());
    assert!(txn.get(index_id, b"index_key_2").unwrap().is_some());
}

#[test]
fn test_no_automatic_index_maintenance() {
    // This test documents that index maintenance is NOT automatic
    let mut txn = create_test_transaction(
        TransactionId::from(7),
        LogSequenceNumber::from(700),
        IsolationLevel::ReadCommitted,
    );

    let table_id = TableId::from(300);
    let index_id = TableId::from(6000);

    // Put a value into the table using unified API
    txn.put(table_id, b"key", b"value").unwrap();

    // The index is NOT automatically updated
    // This is the caller's responsibility
    assert!(txn.get(index_id, b"key").unwrap().is_none());

    // Caller must explicitly maintain the index using unified API
    txn.put(index_id, b"key", b"value").unwrap();
    assert!(txn.get(index_id, b"key").unwrap().is_some());
}

// Made with Bob
