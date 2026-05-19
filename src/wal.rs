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

//! Write-Ahead Log (WAL) implementation
//!
//! The WAL provides durability and crash recovery for nanostore. All modifications
//! are first written to the WAL before being applied to the database, ensuring
//! that committed transactions survive crashes.
//!
//! # Features
//!
//! - **ACID Transactions**: Full support for BEGIN, COMMIT, ROLLBACK
//! - **Crash Recovery**: Automatic recovery from crashes using WAL replay
//! - **Checkpointing**: Periodic checkpoints to limit recovery time
//! - **Buffered Writes**: Configurable write buffering for performance
//! - **Checksums**: SHA-256 checksums for data integrity
//!
//! # Record Types
//!
//! - `BEGIN`: Start a new transaction
//! - `WRITE`: Record a write operation (put/delete)
//! - `COMMIT`: Commit a transaction
//! - `ROLLBACK`: Rollback a transaction
//! - `CHECKPOINT`: Mark a checkpoint with active transactions
//!
//! # Example Usage
//!
//! ```rust,ignore
//! use nanostore::wal::{WalWriter, WalWriterConfig, WriteOpType};
//! use nanostore::vfs::LocalFileSystem;
//!
//! let fs = LocalFileSystem::new();
//! let config = WalWriterConfig::default();
//! let writer = WalWriter::create(&fs, "database.wal", config)?;
//!
//! // Begin transaction
//! writer.write_begin(1)?;
//!
//! // Write operations
//! writer.write_operation(
//!     1,
//!     "users".to_string(),
//!     WriteOpType::Put,
//!     b"user:1".to_vec(),
//!     b"Alice".to_vec(),
//! )?;
//!
//! // Commit transaction
//! writer.write_commit(1)?;
//! ```
//!
//! # Recovery
//!
//! ```rust,ignore
//! use nanostore::wal::WalRecovery;
//! use nanostore::vfs::LocalFileSystem;
//!
//! let fs = LocalFileSystem::new();
//! let result = WalRecovery::recover(&fs, "database.wal")?;
//!
//! // Apply committed writes
//! for write in result.committed_writes {
//!     // Apply to database...
//! }
//!
//! // Handle active transactions
//! for txn_id in result.active_transactions {
//!     // Rollback or continue...
//! }
//! ```

mod commit;
mod error;
mod reader;
mod record;
mod recovery;
mod writer;

pub use self::commit::{GroupCommitConfig, GroupCommitCoordinator, GroupCommitMetrics};
pub use self::error::{WalError, WalResult};
pub use self::reader::{WalReader, WalRecordIterator};
pub use self::record::{LogSequenceNumber, RecordData, RecordType, WalRecord, WriteOpType};
pub use self::recovery::{RecoveredWrite, RecoveryResult, WalRecovery};
pub use self::writer::{WalWriter, WalWriterConfig};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::txn::TransactionId;
    use crate::types::TableId;
    use crate::vfs::MemoryFileSystem;

    #[test]
    fn test_wal_end_to_end() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();

        // Create writer and write some transactions
        {
            let writer = WalWriter::create(&fs, path, config.clone()).unwrap();

            // Transaction 1: committed
            writer.write_begin(TransactionId::from(1)).unwrap();
            writer
                .write_operation(
                    TransactionId::from(1),
                    TableId::from(1),
                    WriteOpType::Put,
                    b"user:1".to_vec(),
                    b"Alice".to_vec(),
                )
                .unwrap();
            writer
                .write_operation(
                    TransactionId::from(1),
                    TableId::from(1),
                    WriteOpType::Put,
                    b"user:2".to_vec(),
                    b"Bob".to_vec(),
                )
                .unwrap();
            writer.write_commit(TransactionId::from(1)).unwrap();

            // Transaction 2: rolled back
            writer.write_begin(TransactionId::from(2)).unwrap();
            writer
                .write_operation(
                    TransactionId::from(2),
                    TableId::from(1),
                    WriteOpType::Delete,
                    b"user:1".to_vec(),
                    vec![],
                )
                .unwrap();
            writer.write_rollback(TransactionId::from(2)).unwrap();

            // Transaction 3: active (crash simulation)
            writer.write_begin(TransactionId::from(3)).unwrap();
            writer
                .write_operation(
                    TransactionId::from(3),
                    TableId::from(1),
                    WriteOpType::Put,
                    b"user:3".to_vec(),
                    b"Charlie".to_vec(),
                )
                .unwrap();

            writer.flush().unwrap();
        }

        // Recover
        let result = WalRecovery::recover(&fs, path).unwrap();

        // Verify recovery result
        assert_eq!(result.committed_writes.len(), 2);
        assert_eq!(result.committed_writes[0].table_id, TableId::from(1));
        assert_eq!(result.committed_writes[0].key, b"user:1");
        assert_eq!(result.committed_writes[0].value, b"Alice");
        assert_eq!(result.committed_writes[1].key, b"user:2");
        assert_eq!(result.committed_writes[1].value, b"Bob");

        assert_eq!(result.active_transactions.len(), 1);
        assert!(result.active_transactions.contains(&TransactionId::from(3)));

        assert_eq!(result.records_processed, 9); // BEGIN(1), WRITE(1), WRITE(1), COMMIT(1), BEGIN(2), WRITE(2), ROLLBACK(2), BEGIN(3), WRITE(3)
    }

    #[test]
    fn test_wal_with_checkpoint() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();

        {
            let writer = WalWriter::create(&fs, path, config).unwrap();

            // Transaction 1
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

            // Transaction 2 (active)
            writer.write_begin(TransactionId::from(2)).unwrap();
            writer
                .write_operation(
                    TransactionId::from(2),
                    TableId::from(1),
                    WriteOpType::Put,
                    b"key2".to_vec(),
                    b"value2".to_vec(),
                )
                .unwrap();

            // Checkpoint
            let checkpoint_lsn = writer.write_checkpoint().unwrap();

            // Transaction 2 commits after checkpoint
            writer.write_commit(TransactionId::from(2)).unwrap();

            // Transaction 3
            writer.write_begin(TransactionId::from(3)).unwrap();
            writer
                .write_operation(
                    TransactionId::from(3),
                    TableId::from(1),
                    WriteOpType::Put,
                    b"key3".to_vec(),
                    b"value3".to_vec(),
                )
                .unwrap();
            writer.write_commit(TransactionId::from(3)).unwrap();

            writer.flush().unwrap();

            assert!(checkpoint_lsn > LogSequenceNumber::from(0));
        }

        // Recover
        let result = WalRecovery::recover(&fs, path).unwrap();

        assert_eq!(result.committed_writes.len(), 3);
        assert!(result.last_checkpoint_lsn.is_some());
        assert!(result.active_transactions.is_empty());
    }

    #[test]
    fn test_wal_reader_iteration() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();

        {
            let writer = WalWriter::create(&fs, path, config).unwrap();
            writer.write_begin(TransactionId::from(1)).unwrap();
            writer
                .write_operation(
                    TransactionId::from(1),
                    TableId::from(1),
                    WriteOpType::Put,
                    b"key".to_vec(),
                    b"value".to_vec(),
                )
                .unwrap();
            writer.write_commit(TransactionId::from(1)).unwrap();
            writer.flush().unwrap();
        }

        let reader = WalReader::open(&fs, path, None).unwrap();
        let iter = WalRecordIterator::new(reader);
        let records: Vec<_> = iter.collect::<Result<Vec<_>, _>>().unwrap();

        assert_eq!(records.len(), 3);
        assert!(matches!(
            records[0].data,
            RecordData::Begin { txn_id } if txn_id == TransactionId::from(1)
        ));
        assert!(matches!(records[1].data, RecordData::Write { .. }));
        assert!(matches!(
            records[2].data,
            RecordData::Commit { txn_id } if txn_id == TransactionId::from(1)
        ));
    }

    #[test]
    fn test_wal_multiple_tables() {
        let fs = MemoryFileSystem::new();
        let path = "test.wal";
        let config = WalWriterConfig::default();

        {
            let writer = WalWriter::create(&fs, path, config).unwrap();
            writer.write_begin(TransactionId::from(1)).unwrap();

            // Write to multiple tables in same transaction
            writer
                .write_operation(
                    TransactionId::from(1),
                    TableId::from(1),
                    WriteOpType::Put,
                    b"user:1".to_vec(),
                    b"Alice".to_vec(),
                )
                .unwrap();
            writer
                .write_operation(
                    TransactionId::from(1),
                    TableId::from(2),
                    WriteOpType::Put,
                    b"post:1".to_vec(),
                    b"Hello".to_vec(),
                )
                .unwrap();
            writer
                .write_operation(
                    TransactionId::from(1),
                    TableId::from(3),
                    WriteOpType::Put,
                    b"comment:1".to_vec(),
                    b"Nice!".to_vec(),
                )
                .unwrap();

            writer.write_commit(TransactionId::from(1)).unwrap();
            writer.flush().unwrap();
        }

        let result = WalRecovery::recover(&fs, path).unwrap();

        assert_eq!(result.committed_writes.len(), 3);
        assert_eq!(result.committed_writes[0].table_id, TableId::from(1));
        assert_eq!(result.committed_writes[1].table_id, TableId::from(2));
        assert_eq!(result.committed_writes[2].table_id, TableId::from(3));
    }
}
