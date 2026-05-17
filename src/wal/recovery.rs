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

//! WAL recovery - Handles crash recovery and replay
//!
//! This module implements WAL (Write-Ahead Log) recovery with checkpoint optimization.
//!
//! # Checkpoint Optimization
//!
//! The recovery process now utilizes checkpoint information to:
//!
//! 1. **Track checkpoint state**: Records the LSN and active transactions at each checkpoint
//! 2. **Validate recovery consistency**: Ensures that transactions active at checkpoint time
//!    are properly accounted for during recovery (either committed, rolled back, or still active)
//! 3. **Detect corruption**: Warns if checkpoint state doesn't match recovery state, which may
//!    indicate WAL corruption or truncation
//!
//! # Recovery Process
//!
//! 1. Read all WAL records sequentially
//! 2. Track transaction states (Active, Committed, RolledBack)
//! 3. When encountering a checkpoint:
//!    - Store the checkpoint LSN and active transactions
//!    - Validate that checkpoint's active transactions match current recovery state
//! 4. Build final result with committed writes and active transactions
//! 5. Validate that all checkpoint active transactions are accounted for
//!
//! # Future Optimizations
//!
//! The checkpoint information could be used for incremental recovery:
//! - Skip replaying transactions committed before the last checkpoint (if their effects
//!   are already persisted to the database)
//! - Start recovery from the last checkpoint instead of the beginning of the WAL
//! - Implement parallel recovery for independent transactions

use crate::txn::TransactionId;
use crate::types::TableId;
use crate::vfs::FileSystem;
use crate::wal::{
    LogSequenceNumber, RecordData, WalError, WalReader, WalRecord, WalResult, WriteOpType,
};
use std::collections::{BTreeMap, HashSet};

/// Transaction state during recovery
#[derive(Debug, Clone, PartialEq, Eq)]
enum TransactionState {
    /// Transaction has begun but not committed/rolled back
    Active,
    /// Transaction has been committed
    Committed,
    /// Transaction has been rolled back
    RolledBack,
}

/// Write operation recorded during recovery
#[derive(Debug, Clone)]
pub struct RecoveredWrite {
    /// Table ID
    pub table_id: TableId,
    /// Operation type
    pub op_type: WriteOpType,
    /// Key
    pub key: Vec<u8>,
    /// Value (empty for delete)
    pub value: Vec<u8>,
}

/// Recovery result containing committed operations
#[derive(Debug)]
pub struct RecoveryResult {
    /// Last checkpoint LSN (if any)
    pub last_checkpoint_lsn: Option<LogSequenceNumber>,
    /// Committed writes that need to be applied
    pub committed_writes: Vec<RecoveredWrite>,
    /// Active transactions at the end of recovery
    pub active_transactions: HashSet<TransactionId>,
    /// Total records processed
    pub records_processed: usize,
}

/// WAL recovery manager
pub struct WalRecovery {
    /// Transaction states
    transactions: BTreeMap<TransactionId, TransactionState>,
    /// Writes per transaction
    transaction_writes: BTreeMap<TransactionId, Vec<RecoveredWrite>>,
    /// Last checkpoint LSN
    last_checkpoint_lsn: Option<LogSequenceNumber>,
    /// Active transactions at last checkpoint
    checkpoint_active_txns: Option<HashSet<TransactionId>>,
    /// Records processed
    records_processed: usize,
}

impl WalRecovery {
    /// Create a new recovery manager
    pub fn new() -> Self {
        Self {
            transactions: BTreeMap::new(),
            transaction_writes: BTreeMap::new(),
            last_checkpoint_lsn: None,
            checkpoint_active_txns: None,
            records_processed: 0,
        }
    }

    /// Perform recovery from a WAL file
    pub fn recover<FS: FileSystem>(fs: &FS, wal_path: &str) -> WalResult<RecoveryResult> {
        Self::recover_with_key(fs, wal_path, None)
    }

    /// Perform recovery from a WAL file with an optional encryption key
    pub fn recover_with_key<FS: FileSystem>(
        fs: &FS,
        wal_path: &str,
        encryption_key: Option<[u8; 32]>,
    ) -> WalResult<RecoveryResult> {
        let mut recovery = Self::new();
        let mut reader = WalReader::open(fs, wal_path, encryption_key)?;

        // Read and process all records
        while let Some(record) = reader.read_next()? {
            recovery.process_record(record)?;
        }

        // Build recovery result
        Ok(recovery.build_result())
    }

    /// Process a single WAL record
    fn process_record(&mut self, record: WalRecord) -> WalResult<()> {
        self.records_processed += 1;

        match record.data {
            RecordData::Begin { txn_id } => {
                self.process_begin(txn_id)?;
            }
            RecordData::Write {
                txn_id,
                table_id,
                op_type,
                key,
                value,
            } => {
                self.process_write(txn_id, table_id, op_type, key, value)?;
            }
            RecordData::Commit { txn_id } => {
                self.process_commit(txn_id)?;
            }
            RecordData::Rollback { txn_id } => {
                self.process_rollback(txn_id)?;
            }
            RecordData::Checkpoint { lsn, active_txns } => {
                self.process_checkpoint(lsn, active_txns)?;
            }
            RecordData::Prepare { txn_id } => {
                // Prepare records indicate a transaction entered the prepare phase
                // If we see a PREPARE without a subsequent COMMIT or ROLLBACK,
                // the transaction was interrupted and should be rolled back during recovery
                // For now, we just track it like a normal active transaction
                self.process_begin(txn_id)?;
            }
        }

        Ok(())
    }

    /// Process a BEGIN record
    fn process_begin(&mut self, txn_id: TransactionId) -> WalResult<()> {
        // Mark transaction as active
        self.transactions.insert(txn_id, TransactionState::Active);
        self.transaction_writes.insert(txn_id, Vec::new());
        Ok(())
    }

    /// Process a WRITE record
    fn process_write(
        &mut self,
        txn_id: TransactionId,
        table_id: TableId,
        op_type: WriteOpType,
        key: Vec<u8>,
        value: Vec<u8>,
    ) -> WalResult<()> {
        // Check if transaction exists
        if !self.transactions.contains_key(&txn_id) {
            return Err(WalError::RecoveryError {
                lsn: LogSequenceNumber::from(0), // LSN not available in this context
                operation: "Write".to_string(),
                details: format!("Write for unknown transaction {}", txn_id),
            });
        }

        // Add write to transaction
        let writes = self.transaction_writes.get_mut(&txn_id).unwrap();
        writes.push(RecoveredWrite {
            table_id,
            op_type,
            key,
            value,
        });

        Ok(())
    }

    /// Process a COMMIT record
    fn process_commit(&mut self, txn_id: TransactionId) -> WalResult<()> {
        // Check if transaction exists
        if !self.transactions.contains_key(&txn_id) {
            return Err(WalError::RecoveryError {
                lsn: LogSequenceNumber::from(0), // LSN not available in this context
                operation: "Commit".to_string(),
                details: format!("Commit for unknown transaction {}", txn_id),
            });
        }

        // Mark transaction as committed
        self.transactions
            .insert(txn_id, TransactionState::Committed);
        Ok(())
    }

    /// Process a ROLLBACK record
    fn process_rollback(&mut self, txn_id: TransactionId) -> WalResult<()> {
        // Check if transaction exists
        if !self.transactions.contains_key(&txn_id) {
            return Err(WalError::RecoveryError {
                lsn: LogSequenceNumber::from(0), // LSN not available in this context
                operation: "Rollback".to_string(),
                details: format!("Rollback for unknown transaction {}", txn_id),
            });
        }

        // Mark transaction as rolled back and discard writes
        self.transactions
            .insert(txn_id, TransactionState::RolledBack);
        self.transaction_writes.remove(&txn_id);
        Ok(())
    }

    /// Process a CHECKPOINT record
    fn process_checkpoint(
        &mut self,
        lsn: LogSequenceNumber,
        active_txns: Vec<TransactionId>,
    ) -> WalResult<()> {
        self.last_checkpoint_lsn = Some(lsn);

        // Store the active transactions at checkpoint for validation
        let checkpoint_txns: HashSet<TransactionId> = active_txns.iter().copied().collect();

        // Validate that checkpoint active_txns matches our current state
        // This helps detect WAL corruption or inconsistencies
        let current_active: HashSet<TransactionId> = self
            .transactions
            .iter()
            .filter_map(|(txn_id, state)| {
                if *state == TransactionState::Active {
                    Some(*txn_id)
                } else {
                    None
                }
            })
            .collect();

        // Check if checkpoint's active transactions match our tracked active transactions
        if checkpoint_txns != current_active {
            // Log a warning but don't fail - the checkpoint might be slightly stale
            // or there might be a race condition during checkpoint creation
            eprintln!(
                "Warning: Checkpoint active transactions mismatch at LSN {}. \
                 Checkpoint has {} active txns, recovery has {} active txns.",
                lsn,
                checkpoint_txns.len(),
                current_active.len()
            );
        }

        self.checkpoint_active_txns = Some(checkpoint_txns);

        // Note: We don't remove any transaction state or writes here.
        // The checkpoint just marks a point in time. All committed transactions
        // before and after the checkpoint should still be recovered.
        //
        // Future optimization: We could potentially skip replaying transactions
        // that were committed before the checkpoint, if we had a way to know
        // that their effects were already persisted to the database.

        Ok(())
    }

    /// Build the recovery result
    fn build_result(self) -> RecoveryResult {
        let mut committed_writes = Vec::new();
        let mut active_transactions = HashSet::new();

        // Final validation: if we had a checkpoint, verify active transactions
        // Do this before consuming self.transactions
        if let Some(checkpoint_txns) = &self.checkpoint_active_txns {
            // Check if any checkpoint active transactions are still tracked
            // (they should either be committed, rolled back, or still active)
            for checkpoint_txn in checkpoint_txns {
                if !self.transactions.contains_key(checkpoint_txn) {
                    eprintln!(
                        "Warning: Transaction {} was active at checkpoint but not found in recovery. \
                         This might indicate WAL truncation or corruption.",
                        checkpoint_txn
                    );
                }
            }
        }

        // Collect committed writes and active transactions
        for (txn_id, state) in self.transactions {
            match state {
                TransactionState::Committed => {
                    if let Some(writes) = self.transaction_writes.get(&txn_id) {
                        committed_writes.extend(writes.clone());
                    }
                }
                TransactionState::Active => {
                    active_transactions.insert(txn_id);
                }
                TransactionState::RolledBack => {
                    // Ignore rolled back transactions
                }
            }
        }

        RecoveryResult {
            last_checkpoint_lsn: self.last_checkpoint_lsn,
            committed_writes,
            active_transactions,
            records_processed: self.records_processed,
        }
    }
}

impl Default for WalRecovery {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::MemoryFileSystem;
    use crate::wal::{WalWriter, WalWriterConfig};

    #[test]
    fn test_recovery_committed_transaction() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, path, config).unwrap();

        // Write a committed transaction
        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"key1".to_vec(),
                b"value1".to_vec(),
            )
            .unwrap();
        writer.write_commit(TransactionId::from(1)).unwrap();
        writer.flush().unwrap();

        // Recover
        let result = WalRecovery::recover(&fs, path).unwrap();

        assert_eq!(result.committed_writes.len(), 1);
        assert_eq!(result.committed_writes[0].table_id, TableId::from(1));
        assert_eq!(result.committed_writes[0].key, b"key1");
        assert_eq!(result.committed_writes[0].value, b"value1");
        assert!(result.active_transactions.is_empty());
        assert_eq!(result.records_processed, 3);
    }

    #[test]
    fn test_recovery_rolled_back_transaction() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, path, config).unwrap();

        // Write a rolled back transaction
        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"key1".to_vec(),
                b"value1".to_vec(),
            )
            .unwrap();
        writer.write_rollback(TransactionId::from(1)).unwrap();
        writer.flush().unwrap();

        // Recover
        let result = WalRecovery::recover(&fs, path).unwrap();

        assert_eq!(result.committed_writes.len(), 0);
        assert!(result.active_transactions.is_empty());
        assert_eq!(result.records_processed, 3);
    }

    #[test]
    fn test_recovery_active_transaction() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, path, config).unwrap();

        // Write an incomplete transaction
        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"key1".to_vec(),
                b"value1".to_vec(),
            )
            .unwrap();
        writer.flush().unwrap();

        // Recover
        let result = WalRecovery::recover(&fs, path).unwrap();

        assert_eq!(result.committed_writes.len(), 0);
        assert_eq!(result.active_transactions.len(), 1);
        assert!(result.active_transactions.contains(&TransactionId::from(1)));
        assert_eq!(result.records_processed, 2);
    }

    #[test]
    fn test_recovery_multiple_transactions() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, path, config).unwrap();

        // Transaction 1: committed
        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"key1".to_vec(),
                b"value1".to_vec(),
            )
            .unwrap();
        writer.write_commit(TransactionId::from(1)).unwrap();

        // Transaction 2: rolled back
        writer.write_begin(TransactionId::from(2)).unwrap();
        writer
            .write_operation(
                TransactionId::from(2),
                TableId::from(2),
                WriteOpType::Put,
                b"key2".to_vec(),
                b"value2".to_vec(),
            )
            .unwrap();
        writer.write_rollback(TransactionId::from(2)).unwrap();

        // Transaction 3: active
        writer.write_begin(TransactionId::from(3)).unwrap();
        writer
            .write_operation(
                TransactionId::from(3),
                TableId::from(3),
                WriteOpType::Put,
                b"key3".to_vec(),
                b"value3".to_vec(),
            )
            .unwrap();

        writer.flush().unwrap();

        // Recover
        let result = WalRecovery::recover(&fs, path).unwrap();

        assert_eq!(result.committed_writes.len(), 1);
        assert_eq!(result.committed_writes[0].table_id, TableId::from(1));
        assert_eq!(result.active_transactions.len(), 1);
        assert!(result.active_transactions.contains(&TransactionId::from(3)));
        assert_eq!(result.records_processed, 8);
    }

    #[test]
    fn test_recovery_with_checkpoint() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, path, config).unwrap();

        // Transaction 1: committed before checkpoint
        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"key1".to_vec(),
                b"value1".to_vec(),
            )
            .unwrap();
        writer.write_commit(TransactionId::from(1)).unwrap();

        // Transaction 2: active at checkpoint
        writer.write_begin(TransactionId::from(2)).unwrap();
        writer
            .write_operation(
                TransactionId::from(2),
                TableId::from(2),
                WriteOpType::Put,
                b"key2".to_vec(),
                b"value2".to_vec(),
            )
            .unwrap();

        // Checkpoint
        writer.write_checkpoint().unwrap();

        // Transaction 2: committed after checkpoint
        writer.write_commit(TransactionId::from(2)).unwrap();

        writer.flush().unwrap();

        // Recover
        let result = WalRecovery::recover(&fs, path).unwrap();

        assert_eq!(result.committed_writes.len(), 2);
        assert!(result.active_transactions.is_empty());
        assert!(result.last_checkpoint_lsn.is_some());
    }

    #[test]
    fn test_recovery_delete_operation() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, path, config).unwrap();

        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Delete,
                b"key1".to_vec(),
                vec![],
            )
            .unwrap();
        writer.write_commit(TransactionId::from(1)).unwrap();
        writer.flush().unwrap();

        let result = WalRecovery::recover(&fs, path).unwrap();

        assert_eq!(result.committed_writes.len(), 1);
        assert_eq!(result.committed_writes[0].op_type, WriteOpType::Delete);
        assert_eq!(result.committed_writes[0].key, b"key1");
        assert!(result.committed_writes[0].value.is_empty());
    }

    #[test]
    fn test_checkpoint_validation_matching_state() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, path, config).unwrap();

        // Transaction 1: active before checkpoint
        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"key1".to_vec(),
                b"value1".to_vec(),
            )
            .unwrap();

        // Transaction 2: active before checkpoint
        writer.write_begin(TransactionId::from(2)).unwrap();
        writer
            .write_operation(
                TransactionId::from(2),
                TableId::from(2),
                WriteOpType::Put,
                b"key2".to_vec(),
                b"value2".to_vec(),
            )
            .unwrap();

        // Checkpoint with both transactions active
        writer.write_checkpoint().unwrap();

        // Transaction 1: committed after checkpoint
        writer.write_commit(TransactionId::from(1)).unwrap();

        // Transaction 2: still active
        writer.flush().unwrap();

        // Recover and verify
        let result = WalRecovery::recover(&fs, path).unwrap();

        assert!(result.last_checkpoint_lsn.is_some());
        assert_eq!(result.committed_writes.len(), 1);
        assert_eq!(result.active_transactions.len(), 1);
        assert!(result.active_transactions.contains(&TransactionId::from(2)));
    }

    #[test]
    fn test_checkpoint_validation_all_committed() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, path, config).unwrap();

        // Transaction 1: active before checkpoint
        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"key1".to_vec(),
                b"value1".to_vec(),
            )
            .unwrap();

        // Transaction 2: active before checkpoint
        writer.write_begin(TransactionId::from(2)).unwrap();
        writer
            .write_operation(
                TransactionId::from(2),
                TableId::from(2),
                WriteOpType::Put,
                b"key2".to_vec(),
                b"value2".to_vec(),
            )
            .unwrap();

        // Checkpoint with both transactions active
        writer.write_checkpoint().unwrap();

        // Both transactions committed after checkpoint
        writer.write_commit(TransactionId::from(1)).unwrap();
        writer.write_commit(TransactionId::from(2)).unwrap();
        writer.flush().unwrap();

        // Recover and verify
        let result = WalRecovery::recover(&fs, path).unwrap();

        assert!(result.last_checkpoint_lsn.is_some());
        assert_eq!(result.committed_writes.len(), 2);
        assert!(result.active_transactions.is_empty());
    }

    #[test]
    fn test_checkpoint_validation_mixed_outcomes() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, path, config).unwrap();

        // Transaction 1: active before checkpoint
        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"key1".to_vec(),
                b"value1".to_vec(),
            )
            .unwrap();

        // Transaction 2: active before checkpoint
        writer.write_begin(TransactionId::from(2)).unwrap();
        writer
            .write_operation(
                TransactionId::from(2),
                TableId::from(2),
                WriteOpType::Put,
                b"key2".to_vec(),
                b"value2".to_vec(),
            )
            .unwrap();

        // Transaction 3: active before checkpoint
        writer.write_begin(TransactionId::from(3)).unwrap();
        writer
            .write_operation(
                TransactionId::from(3),
                TableId::from(3),
                WriteOpType::Put,
                b"key3".to_vec(),
                b"value3".to_vec(),
            )
            .unwrap();

        // Checkpoint with all three transactions active
        writer.write_checkpoint().unwrap();

        // Transaction 1: committed after checkpoint
        writer.write_commit(TransactionId::from(1)).unwrap();

        // Transaction 2: rolled back after checkpoint
        writer.write_rollback(TransactionId::from(2)).unwrap();

        // Transaction 3: still active
        writer.flush().unwrap();

        // Recover and verify
        let result = WalRecovery::recover(&fs, path).unwrap();

        assert!(result.last_checkpoint_lsn.is_some());
        assert_eq!(result.committed_writes.len(), 1);
        assert_eq!(result.committed_writes[0].table_id, TableId::from(1));
        assert_eq!(result.active_transactions.len(), 1);
        assert!(result.active_transactions.contains(&TransactionId::from(3)));
    }

    #[test]
    fn test_multiple_checkpoints() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, path, config).unwrap();

        // Transaction 1: committed before first checkpoint
        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"key1".to_vec(),
                b"value1".to_vec(),
            )
            .unwrap();
        writer.write_commit(TransactionId::from(1)).unwrap();

        // First checkpoint (no active transactions)
        writer.write_checkpoint().unwrap();

        // Transaction 2: active at second checkpoint
        writer.write_begin(TransactionId::from(2)).unwrap();
        writer
            .write_operation(
                TransactionId::from(2),
                TableId::from(2),
                WriteOpType::Put,
                b"key2".to_vec(),
                b"value2".to_vec(),
            )
            .unwrap();

        // Second checkpoint (transaction 2 active)
        writer.write_checkpoint().unwrap();

        // Transaction 2: committed after second checkpoint
        writer.write_commit(TransactionId::from(2)).unwrap();
        writer.flush().unwrap();

        // Recover and verify
        let result = WalRecovery::recover(&fs, path).unwrap();

        // Should use the last checkpoint
        assert!(result.last_checkpoint_lsn.is_some());
        assert_eq!(result.committed_writes.len(), 2);
        assert!(result.active_transactions.is_empty());
    }

    #[test]
    fn test_checkpoint_with_no_active_transactions() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, path, config).unwrap();

        // Transaction 1: committed before checkpoint
        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"key1".to_vec(),
                b"value1".to_vec(),
            )
            .unwrap();
        writer.write_commit(TransactionId::from(1)).unwrap();

        // Checkpoint with no active transactions
        writer.write_checkpoint().unwrap();

        // Transaction 2: started after checkpoint
        writer.write_begin(TransactionId::from(2)).unwrap();
        writer
            .write_operation(
                TransactionId::from(2),
                TableId::from(2),
                WriteOpType::Put,
                b"key2".to_vec(),
                b"value2".to_vec(),
            )
            .unwrap();
        writer.write_commit(TransactionId::from(2)).unwrap();
        writer.flush().unwrap();

        // Recover and verify
        let result = WalRecovery::recover(&fs, path).unwrap();

        assert!(result.last_checkpoint_lsn.is_some());
        assert_eq!(result.committed_writes.len(), 2);
        assert!(result.active_transactions.is_empty());
    }
}
