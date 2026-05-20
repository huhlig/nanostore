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

//! WAL error types

use crate::txn::TransactionId;
use crate::vfs::FileSystemError;
use crate::wal::LogSequenceNumber;
use thiserror::Error;

/// Result type for WAL operations
pub type WalResult<T> = Result<T, WalError>;

/// WAL error types
#[derive(Debug, Error)]
pub enum WalError {
    /// VFS error
    #[error("VFS error: {0}")]
    VfsError(#[from] FileSystemError),

    /// Invalid WAL record
    #[error("Invalid WAL record at LSN {lsn}: {details}")]
    InvalidRecord {
        lsn: LogSequenceNumber,
        details: String,
    },

    /// Checksum mismatch
    #[error(
        "Checksum mismatch at LSN {lsn}, offset {offset}: expected {expected:x}, found {found:x}"
    )]
    ChecksumMismatch {
        lsn: LogSequenceNumber,
        offset: u64,
        expected: u32,
        found: u32,
    },

    /// Corrupted WAL file
    #[error("Corrupted WAL at offset {offset}: {corruption_type} - {details}")]
    CorruptedWal {
        offset: u64,
        corruption_type: String,
        details: String,
    },

    /// Transaction not found
    #[error("Transaction not found: {txn_id}")]
    TransactionNotFound { txn_id: TransactionId },

    /// Transaction already exists
    #[error("Transaction already exists: {txn_id}")]
    TransactionAlreadyExists { txn_id: TransactionId },

    /// Invalid transaction state
    #[error("Invalid transaction state for {txn_id}: {current_state}, attempted {operation}")]
    InvalidTransactionState {
        txn_id: TransactionId,
        current_state: String,
        operation: String,
    },

    /// WAL is full
    #[error("WAL is full: current size {current_size} bytes, max size {max_size} bytes")]
    WalFull { current_size: u64, max_size: u64 },

    /// Recovery error
    #[error("Recovery error at LSN {lsn}: {operation} - {details}")]
    RecoveryError {
        lsn: LogSequenceNumber,
        operation: String,
        details: String,
    },

    /// Checkpoint error
    #[error("Checkpoint error at LSN {lsn}: {details}")]
    CheckpointError {
        lsn: LogSequenceNumber,
        details: String,
    },

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// Serialization error
    #[error("Serialization error for {record_type} at LSN {lsn}: {details}")]
    SerializationError {
        lsn: LogSequenceNumber,
        record_type: String,
        details: String,
    },

    /// Deserialization error
    #[error("Deserialization error at offset {offset}: {details}")]
    DeserializationError { offset: u64, details: String },

    /// Encryption error
    #[error("Encryption error for {encryption_type} at LSN {lsn}: {details}")]
    EncryptionError {
        lsn: LogSequenceNumber,
        encryption_type: String,
        details: String,
    },

    /// Decryption error
    #[error("Decryption error for {encryption_type} at offset {offset}: {details}")]
    DecryptionError {
        offset: u64,
        encryption_type: String,
        details: String,
    },

    /// Missing encryption key
    #[error("Missing encryption key for {encryption_type}")]
    MissingEncryptionKey { encryption_type: String },

    /// Commit notification error
    #[error("Commit notification failed for transaction {txn_id} at LSN {lsn}: {details}")]
    CommitNotificationError {
        txn_id: TransactionId,
        lsn: LogSequenceNumber,
        details: String,
    },

    /// Internal error
    #[error("Internal error: {0}")]
    InternalError(String),
}

impl WalError {
    /// Create an invalid record error
    pub fn invalid_record(lsn: LogSequenceNumber, details: impl Into<String>) -> Self {
        Self::InvalidRecord {
            lsn,
            details: details.into(),
        }
    }

    /// Create a checksum mismatch error
    pub fn checksum_mismatch(
        lsn: LogSequenceNumber,
        offset: u64,
        expected: u32,
        found: u32,
    ) -> Self {
        Self::ChecksumMismatch {
            lsn,
            offset,
            expected,
            found,
        }
    }

    /// Create a corrupted WAL error
    pub fn corrupted_wal(
        offset: u64,
        corruption_type: impl Into<String>,
        details: impl Into<String>,
    ) -> Self {
        Self::CorruptedWal {
            offset,
            corruption_type: corruption_type.into(),
            details: details.into(),
        }
    }

    /// Create a transaction not found error
    pub fn transaction_not_found(txn_id: TransactionId) -> Self {
        Self::TransactionNotFound { txn_id }
    }

    /// Create a transaction already exists error
    pub fn transaction_already_exists(txn_id: TransactionId) -> Self {
        Self::TransactionAlreadyExists { txn_id }
    }

    /// Create an invalid transaction state error
    pub fn invalid_transaction_state(
        txn_id: TransactionId,
        current_state: impl Into<String>,
        operation: impl Into<String>,
    ) -> Self {
        Self::InvalidTransactionState {
            txn_id,
            current_state: current_state.into(),
            operation: operation.into(),
        }
    }

    /// Create a WAL full error
    pub fn wal_full(current_size: u64, max_size: u64) -> Self {
        Self::WalFull {
            current_size,
            max_size,
        }
    }

    /// Create a recovery error
    pub fn recovery_error(
        lsn: LogSequenceNumber,
        operation: impl Into<String>,
        details: impl Into<String>,
    ) -> Self {
        Self::RecoveryError {
            lsn,
            operation: operation.into(),
            details: details.into(),
        }
    }

    /// Create a checkpoint error
    pub fn checkpoint_error(lsn: LogSequenceNumber, details: impl Into<String>) -> Self {
        Self::CheckpointError {
            lsn,
            details: details.into(),
        }
    }

    /// Create a serialization error
    pub fn serialization_error(
        lsn: LogSequenceNumber,
        record_type: impl Into<String>,
        details: impl Into<String>,
    ) -> Self {
        Self::SerializationError {
            lsn,
            record_type: record_type.into(),
            details: details.into(),
        }
    }

    /// Create a deserialization error
    pub fn deserialization_error(offset: u64, details: impl Into<String>) -> Self {
        Self::DeserializationError {
            offset,
            details: details.into(),
        }
    }

    /// Create an encryption error
    pub fn encryption_error(
        lsn: LogSequenceNumber,
        encryption_type: impl Into<String>,
        details: impl Into<String>,
    ) -> Self {
        Self::EncryptionError {
            lsn,
            encryption_type: encryption_type.into(),
            details: details.into(),
        }
    }

    /// Create a decryption error
    pub fn decryption_error(
        offset: u64,
        encryption_type: impl Into<String>,
        details: impl Into<String>,
    ) -> Self {
        Self::DecryptionError {
            offset,
            encryption_type: encryption_type.into(),
            details: details.into(),
        }
    }

    /// Create a missing encryption key error
    pub fn missing_encryption_key(encryption_type: impl Into<String>) -> Self {
        Self::MissingEncryptionKey {
            encryption_type: encryption_type.into(),
        }
    }

    /// Create a commit notification error
    pub fn commit_notification_error(
        txn_id: TransactionId,
        lsn: LogSequenceNumber,
        details: impl Into<String>,
    ) -> Self {
        Self::CommitNotificationError {
            txn_id,
            lsn,
            details: details.into(),
        }
    }
}
