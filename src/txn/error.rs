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

use crate::txn::TransactionId;
use crate::types::TableId;

/// Transaction Result Type
pub type TransactionResult<T> = Result<T, TransactionError>;

/// Transaction error type.
#[derive(Debug, thiserror::Error)]
pub enum TransactionError {
    /// Invalid state for the attempted operation
    #[error(
        "Transaction {transaction_id} has invalid state '{current_state}' for operation '{attempted_operation}'"
    )]
    InvalidState {
        transaction_id: TransactionId,
        current_state: String,
        attempted_operation: String,
    },

    /// Write-write conflict: two transactions trying to write the same key
    #[error(
        "Write-write conflict on object {object_id}, key {key:?}: already locked by transaction {holder_txn_id}, requested by transaction {requester_txn_id}"
    )]
    WriteWriteConflict {
        object_id: TableId,
        key: Vec<u8>,
        holder_txn_id: TransactionId,
        requester_txn_id: TransactionId,
    },

    /// Read-write conflict: a transaction read a key that was later modified
    #[error(
        "Read-write conflict on object {object_id}, key {key:?}: read by transaction {reader_txn_id}, modified by transaction {writer_txn_id}"
    )]
    ReadWriteConflict {
        object_id: TableId,
        key: Vec<u8>,
        reader_txn_id: TransactionId,
        writer_txn_id: TransactionId,
    },

    /// Serialization conflict in serializable isolation level
    #[error("Serialization conflict detected for transaction {transaction_id}: {details}")]
    SerializationConflict {
        transaction_id: TransactionId,
        details: String,
    },

    /// Transaction not found in the system
    #[error("Transaction {transaction_id} not found: {context}")]
    TransactionNotFound {
        transaction_id: TransactionId,
        context: String,
    },

    /// Deadlock detected with cycle information
    #[error("Deadlock detected involving transaction {transaction_id}: {cycle_description}")]
    Deadlock {
        transaction_id: TransactionId,
        involved_transactions: Vec<TransactionId>,
        cycle_description: String,
    },

    /// Other error
    #[error("Transaction error: {0}")]
    Other(String),
}

impl TransactionError {
    /// Record abort reason metric based on error type
    pub fn record_abort_metric(&self) {
        let reason = match self {
            Self::InvalidState { .. } => "invalid_state",
            Self::WriteWriteConflict { .. } => "write_write_conflict",
            Self::ReadWriteConflict { .. } => "read_write_conflict",
            Self::SerializationConflict { .. } => "serialization_conflict",
            Self::TransactionNotFound { .. } => "not_found",
            Self::Deadlock { .. } => "deadlock",
            Self::Other(_) => "other",
        };

        metrics::counter!("nanostore.transaction.abort.total", "reason" => reason).increment(1);
    }

    /// Create an invalid state error with full context
    pub fn invalid_state(
        transaction_id: TransactionId,
        current_state: impl Into<String>,
        attempted_operation: impl Into<String>,
    ) -> Self {
        Self::InvalidState {
            transaction_id,
            current_state: current_state.into(),
            attempted_operation: attempted_operation.into(),
        }
    }

    /// Create a write-write conflict error with full context
    pub fn write_write_conflict(
        object_id: TableId,
        key: Vec<u8>,
        holder_txn_id: TransactionId,
        requester_txn_id: TransactionId,
    ) -> Self {
        Self::WriteWriteConflict {
            object_id,
            key,
            holder_txn_id,
            requester_txn_id,
        }
    }

    /// Create a read-write conflict error with full context
    pub fn read_write_conflict(
        object_id: TableId,
        key: Vec<u8>,
        reader_txn_id: TransactionId,
        writer_txn_id: TransactionId,
    ) -> Self {
        Self::ReadWriteConflict {
            object_id,
            key,
            reader_txn_id,
            writer_txn_id,
        }
    }

    /// Create a serialization conflict error with full context
    pub fn serialization_conflict(
        transaction_id: TransactionId,
        details: impl Into<String>,
    ) -> Self {
        Self::SerializationConflict {
            transaction_id,
            details: details.into(),
        }
    }

    /// Create a transaction not found error with full context
    pub fn transaction_not_found(
        transaction_id: TransactionId,
        context: impl Into<String>,
    ) -> Self {
        Self::TransactionNotFound {
            transaction_id,
            context: context.into(),
        }
    }

    /// Create a deadlock error with full context
    pub fn deadlock(
        transaction_id: TransactionId,
        involved_transactions: Vec<TransactionId>,
        cycle_description: impl Into<String>,
    ) -> Self {
        Self::Deadlock {
            transaction_id,
            involved_transactions,
            cycle_description: cycle_description.into(),
        }
    }
}

/// Cursor Result Type
pub type CursorResult<T> = Result<T, CursorError>;

/// Cursor error type.
#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    /// Cursor is in an invalid state for the operation
    #[error("Cursor invalid state: {state} for operation '{operation}'")]
    InvalidState { state: String, operation: String },

    /// Cursor position is invalid
    #[error("Cursor invalid position: {details}")]
    InvalidPosition { details: String },

    /// Transaction error occurred during cursor operation
    #[error("Transaction error in cursor: {0}")]
    Transaction(#[from] TransactionError),

    /// Other cursor error
    #[error("Cursor error: {0}")]
    Other(String),
}

impl CursorError {
    /// Create an invalid state error with full context
    pub fn invalid_state(state: impl Into<String>, operation: impl Into<String>) -> Self {
        Self::InvalidState {
            state: state.into(),
            operation: operation.into(),
        }
    }

    /// Create an invalid position error with full context
    pub fn invalid_position(details: impl Into<String>) -> Self {
        Self::InvalidPosition {
            details: details.into(),
        }
    }
}
