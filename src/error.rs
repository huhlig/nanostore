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

//! Unified error type for nanostore
//!
//! This module provides a unified error type that wraps all subsystem-specific
//! error types, enabling seamless error propagation across layer boundaries
//! without manual `map_err` conversions.

use crate::pager::PagerError;
use crate::table::TableError;
use crate::txn::{CursorError, TransactionError};
use crate::vfs::FileSystemError;
use crate::wal::WalError;
use metrics::counter;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tracing::{Level, error, warn};

/// Result type for Nanostore operations
pub type NanostoreResult<T> = Result<T, NanostoreError>;

/// Unified error type for all Nanostore operations
///
/// This enum wraps all subsystem-specific error types, providing automatic
/// conversion via `From` implementations. This eliminates the need for manual
/// `map_err` calls when errors cross subsystem boundaries.
///
/// # Example
///
/// ```rust,ignore
/// fn operation() -> NanostoreResult<()> {
///     // Pager errors automatically convert to NanostoreError
///     let page = pager.read_page(page_id)?;
///
///     // WAL errors also automatically convert
///     wal.write_record(record)?;
///
///     Ok(())
/// }
/// ```
#[derive(Debug, Error)]
pub enum NanostoreError {
    /// Pager subsystem error
    #[error("Pager error: {0}")]
    Pager(#[from] PagerError),

    /// WAL subsystem error
    #[error("WAL error: {0}")]
    Wal(#[from] WalError),

    /// Table subsystem error
    #[error("Table error: {0}")]
    Table(#[from] TableError),

    /// Transaction subsystem error
    #[error("Transaction error: {0}")]
    Transaction(#[from] TransactionError),

    /// Cursor subsystem error
    #[error("Cursor error: {0}")]
    Cursor(#[from] CursorError),

    /// Virtual file system error
    #[error("VFS error: {0}")]
    Vfs(#[from] FileSystemError),

    /// Standard I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Generic error for cases not covered by specific subsystems
    #[error("{0}")]
    Other(String),
}

impl NanostoreError {
    /// Create a generic error from a string
    pub fn other(msg: impl Into<String>) -> Self {
        NanostoreError::Other(msg.into())
    }

    /// Check if this error is a pager error
    pub fn is_pager(&self) -> bool {
        matches!(self, NanostoreError::Pager(_))
    }

    /// Check if this error is a WAL error
    pub fn is_wal(&self) -> bool {
        matches!(self, NanostoreError::Wal(_))
    }

    /// Check if this error is a table error
    pub fn is_table(&self) -> bool {
        matches!(self, NanostoreError::Table(_))
    }

    /// Check if this error is a transaction error
    pub fn is_transaction(&self) -> bool {
        matches!(self, NanostoreError::Transaction(_))
    }

    /// Check if this error is a cursor error
    pub fn is_cursor(&self) -> bool {
        matches!(self, NanostoreError::Cursor(_))
    }

    /// Check if this error is a VFS error
    pub fn is_vfs(&self) -> bool {
        matches!(self, NanostoreError::Vfs(_))
    }

    /// Check if this error is an I/O error
    pub fn is_io(&self) -> bool {
        matches!(self, NanostoreError::Io(_))
    }

    /// Get the underlying pager error, if any
    pub fn as_pager(&self) -> Option<&PagerError> {
        match self {
            NanostoreError::Pager(e) => Some(e),
            _ => None,
        }
    }

    /// Get the underlying WAL error, if any
    pub fn as_wal(&self) -> Option<&WalError> {
        match self {
            NanostoreError::Wal(e) => Some(e),
            _ => None,
        }
    }

    /// Get the underlying table error, if any
    pub fn as_table(&self) -> Option<&TableError> {
        match self {
            NanostoreError::Table(e) => Some(e),
            _ => None,
        }
    }

    /// Get the underlying transaction error, if any
    pub fn as_transaction(&self) -> Option<&TransactionError> {
        match self {
            NanostoreError::Transaction(e) => Some(e),
            _ => None,
        }
    }

    /// Get the underlying cursor error, if any
    pub fn as_cursor(&self) -> Option<&CursorError> {
        match self {
            NanostoreError::Cursor(e) => Some(e),
            _ => None,
        }
    }

    /// Get the underlying VFS error, if any
    pub fn as_vfs(&self) -> Option<&FileSystemError> {
        match self {
            NanostoreError::Vfs(e) => Some(e),
            _ => None,
        }
    }

    /// Get the underlying I/O error, if any
    pub fn as_io(&self) -> Option<&std::io::Error> {
        match self {
            NanostoreError::Io(e) => Some(e),
            _ => None,
        }
    }
    /// Classify this error for observability purposes.
    pub fn classification(&self) -> ErrorClassification {
        <Self as ErrorTelemetry>::classification(self)
    }

    /// Convenience accessor for severity derived from the classification.
    pub fn severity(&self) -> ErrorSeverity {
        self.classification().severity
    }

    /// Record this error using the shared observability layer.
    #[must_use]
    pub fn record(&self) -> ErrorObservation {
        record_error(self)
    }
}

impl ErrorTelemetry for NanostoreError {
    fn classification(&self) -> ErrorClassification {
        match self {
            NanostoreError::Pager(err) => err.classification(),
            NanostoreError::Wal(err) => err.classification(),
            NanostoreError::Table(err) => err.classification(),
            NanostoreError::Transaction(err) => err.classification(),
            NanostoreError::Cursor(err) => err.classification(),
            NanostoreError::Vfs(err) => err.classification(),
            NanostoreError::Io(_) => ErrorClassification {
                subsystem: "io",
                category: "io",
                variant: "io_error",
                severity: ErrorSeverity::Error,
            },
            NanostoreError::Other(_) => ErrorClassification {
                subsystem: "nanostore",
                category: "internal",
                variant: "other",
                severity: ErrorSeverity::Error,
            },
        }
    }
}

/// Structured classification for observed errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorClassification {
    /// Top-level subsystem that emitted the error.
    pub subsystem: &'static str,
    /// Coarse-grained category used for metrics and dashboards.
    pub category: &'static str,
    /// Stable variant identifier suitable for labels.
    pub variant: &'static str,
    /// Severity level for logging and alerting.
    pub severity: ErrorSeverity,
}

/// Severity assigned to an observed error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSeverity {
    Warning,
    Error,
    Critical,
}

impl ErrorSeverity {
    /// Returns the string form used in metrics labels.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
        }
    }

    fn tracing_level(self) -> Level {
        match self {
            Self::Warning => Level::WARN,
            Self::Error | Self::Critical => Level::ERROR,
        }
    }
}

/// Summary returned after an error observation is recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorObservation {
    /// Error classification used for metrics and logs.
    pub classification: ErrorClassification,
    /// Unix timestamp at which the error was recorded.
    pub timestamp_secs: u64,
    /// Human-readable error message.
    pub message: String,
}

/// Trait implemented by error types that can be classified centrally.
pub trait ErrorTelemetry {
    /// Classify the error into subsystem/category/severity dimensions.
    fn classification(&self) -> ErrorClassification;
}

impl ErrorTelemetry for PagerError {
    fn classification(&self) -> ErrorClassification {
        match self {
            PagerError::VfsError(_) => {
                classification("pager", "dependency", "vfs_error", ErrorSeverity::Error)
            }
            PagerError::InvalidPageId(_) => classification(
                "pager",
                "validation",
                "invalid_page_id",
                ErrorSeverity::Warning,
            ),
            PagerError::PageNotFound(_) => classification(
                "pager",
                "not_found",
                "page_not_found",
                ErrorSeverity::Warning,
            ),
            PagerError::ChecksumMismatch(_) => classification(
                "pager",
                "corruption",
                "checksum_mismatch",
                ErrorSeverity::Critical,
            ),
            PagerError::InvalidPageSize(_) => classification(
                "pager",
                "validation",
                "invalid_page_size",
                ErrorSeverity::Error,
            ),
            PagerError::InvalidFileHeader { .. } => classification(
                "pager",
                "corruption",
                "invalid_file_header",
                ErrorSeverity::Critical,
            ),
            PagerError::InvalidSuperblock { .. } => classification(
                "pager",
                "corruption",
                "invalid_superblock",
                ErrorSeverity::Critical,
            ),
            PagerError::CompressionError { .. } => classification(
                "pager",
                "encoding",
                "compression_error",
                ErrorSeverity::Error,
            ),
            PagerError::DecompressionError { .. } => classification(
                "pager",
                "encoding",
                "decompression_error",
                ErrorSeverity::Error,
            ),
            PagerError::EncryptionError { .. } => classification(
                "pager",
                "encryption",
                "encryption_error",
                ErrorSeverity::Error,
            ),
            PagerError::DecryptionError { .. } => classification(
                "pager",
                "encryption",
                "decryption_error",
                ErrorSeverity::Critical,
            ),
            PagerError::MissingEncryptionKey => classification(
                "pager",
                "configuration",
                "missing_encryption_key",
                ErrorSeverity::Error,
            ),
            PagerError::ConfigError(_) => classification(
                "pager",
                "configuration",
                "config_error",
                ErrorSeverity::Error,
            ),
            PagerError::StorageFull => classification(
                "pager",
                "resource_exhaustion",
                "storage_full",
                ErrorSeverity::Error,
            ),
            PagerError::PageAlreadyAllocated(_) => classification(
                "pager",
                "consistency",
                "page_already_allocated",
                ErrorSeverity::Error,
            ),
            PagerError::PageAlreadyFree(_) => classification(
                "pager",
                "consistency",
                "page_already_free",
                ErrorSeverity::Error,
            ),
            PagerError::PagePinned(_) => classification(
                "pager",
                "resource_busy",
                "page_pinned",
                ErrorSeverity::Warning,
            ),
            PagerError::InvalidPageType(_) => classification(
                "pager",
                "validation",
                "invalid_page_type",
                ErrorSeverity::Error,
            ),
            PagerError::IoError(_) => {
                classification("pager", "io", "io_error", ErrorSeverity::Error)
            }
            PagerError::InternalError(_) => classification(
                "pager",
                "internal",
                "internal_error",
                ErrorSeverity::Critical,
            ),
        }
    }
}

impl ErrorTelemetry for WalError {
    fn classification(&self) -> ErrorClassification {
        match self {
            WalError::VfsError(_) => {
                classification("wal", "dependency", "vfs_error", ErrorSeverity::Error)
            }
            WalError::InvalidRecord { .. } => {
                classification("wal", "corruption", "invalid_record", ErrorSeverity::Error)
            }
            WalError::ChecksumMismatch { .. } => classification(
                "wal",
                "corruption",
                "checksum_mismatch",
                ErrorSeverity::Critical,
            ),
            WalError::CorruptedWal { .. } => classification(
                "wal",
                "corruption",
                "corrupted_wal",
                ErrorSeverity::Critical,
            ),
            WalError::TransactionNotFound { .. } => classification(
                "wal",
                "not_found",
                "transaction_not_found",
                ErrorSeverity::Warning,
            ),
            WalError::TransactionAlreadyExists { .. } => classification(
                "wal",
                "consistency",
                "transaction_already_exists",
                ErrorSeverity::Warning,
            ),
            WalError::InvalidTransactionState { .. } => classification(
                "wal",
                "validation",
                "invalid_transaction_state",
                ErrorSeverity::Error,
            ),
            WalError::WalFull { .. } => classification(
                "wal",
                "resource_exhaustion",
                "wal_full",
                ErrorSeverity::Error,
            ),
            WalError::RecoveryError { .. } => {
                classification("wal", "recovery", "recovery_error", ErrorSeverity::Critical)
            }
            WalError::CheckpointError { .. } => classification(
                "wal",
                "checkpoint",
                "checkpoint_error",
                ErrorSeverity::Error,
            ),
            WalError::IoError(_) => classification("wal", "io", "io_error", ErrorSeverity::Error),
            WalError::SerializationError { .. } => classification(
                "wal",
                "encoding",
                "serialization_error",
                ErrorSeverity::Error,
            ),
            WalError::DeserializationError { .. } => classification(
                "wal",
                "encoding",
                "deserialization_error",
                ErrorSeverity::Error,
            ),
            WalError::EncryptionError { .. } => classification(
                "wal",
                "encryption",
                "encryption_error",
                ErrorSeverity::Error,
            ),
            WalError::DecryptionError { .. } => classification(
                "wal",
                "encryption",
                "decryption_error",
                ErrorSeverity::Critical,
            ),
            WalError::MissingEncryptionKey { .. } => classification(
                "wal",
                "configuration",
                "missing_encryption_key",
                ErrorSeverity::Error,
            ),
            WalError::InternalError(_) => {
                classification("wal", "internal", "internal_error", ErrorSeverity::Critical)
            }
        }
    }
}

impl ErrorTelemetry for TableError {
    fn classification(&self) -> ErrorClassification {
        match self {
            TableError::KeyNotFound { .. } => classification(
                "table",
                "not_found",
                "key_not_found",
                ErrorSeverity::Warning,
            ),
            TableError::InvalidKey { .. } => {
                classification("table", "validation", "invalid_key", ErrorSeverity::Warning)
            }
            TableError::InvalidValue { .. } => classification(
                "table",
                "validation",
                "invalid_value",
                ErrorSeverity::Warning,
            ),
            TableError::TableFull { .. } => classification(
                "table",
                "resource_exhaustion",
                "table_full",
                ErrorSeverity::Error,
            ),
            TableError::Corruption { .. } => {
                classification("table", "corruption", "corruption", ErrorSeverity::Critical)
            }
            TableError::ChecksumMismatch { .. } => classification(
                "table",
                "corruption",
                "checksum_mismatch",
                ErrorSeverity::Critical,
            ),
            TableError::InvalidFormatVersion(_) => classification(
                "table",
                "validation",
                "invalid_format_version",
                ErrorSeverity::Error,
            ),
            TableError::MigrationNotSupported { .. } => classification(
                "table",
                "unsupported",
                "migration_not_supported",
                ErrorSeverity::Warning,
            ),
            TableError::MigrationFailed(_) => classification(
                "table",
                "migration",
                "migration_failed",
                ErrorSeverity::Error,
            ),
            TableError::CompactionFailed { .. } => classification(
                "table",
                "compaction",
                "compaction_failed",
                ErrorSeverity::Error,
            ),
            TableError::VacuumFailed { .. } => classification(
                "table",
                "maintenance",
                "vacuum_failed",
                ErrorSeverity::Error,
            ),
            TableError::CheckpointFailed { .. } => classification(
                "table",
                "checkpoint",
                "checkpoint_failed",
                ErrorSeverity::Error,
            ),
            TableError::FlushFailed { .. } => {
                classification("table", "flush", "flush_failed", ErrorSeverity::Error)
            }
            TableError::EvictionFailed { .. } => {
                classification("table", "eviction", "eviction_failed", ErrorSeverity::Error)
            }
            TableError::StatisticsRefreshFailed(_) => classification(
                "table",
                "maintenance",
                "statistics_refresh_failed",
                ErrorSeverity::Warning,
            ),
            TableError::VerificationFailed(_) => classification(
                "table",
                "verification",
                "verification_failed",
                ErrorSeverity::Error,
            ),
            TableError::RepairFailed(_) => {
                classification("table", "repair", "repair_failed", ErrorSeverity::Error)
            }
            TableError::BatchFailed { .. } => {
                classification("table", "batch", "batch_failed", ErrorSeverity::Error)
            }
            TableError::CursorError { .. } => {
                classification("table", "cursor", "cursor_error", ErrorSeverity::Warning)
            }
            TableError::InvalidScanBounds { .. } => classification(
                "table",
                "validation",
                "invalid_scan_bounds",
                ErrorSeverity::Warning,
            ),
            TableError::TransactionConflict { .. } => classification(
                "table",
                "conflict",
                "transaction_conflict",
                ErrorSeverity::Warning,
            ),
            TableError::SnapshotNotFound { .. } => classification(
                "table",
                "not_found",
                "snapshot_not_found",
                ErrorSeverity::Warning,
            ),
            TableError::MemoryLimitExceeded { .. } => classification(
                "table",
                "resource_exhaustion",
                "memory_limit_exceeded",
                ErrorSeverity::Error,
            ),
            TableError::MemtableFull => classification(
                "table",
                "resource_exhaustion",
                "memtable_full",
                ErrorSeverity::Warning,
            ),
            TableError::MemtableImmutable => classification(
                "table",
                "state",
                "memtable_immutable",
                ErrorSeverity::Warning,
            ),
            TableError::MemtableNotImmutable => classification(
                "table",
                "state",
                "memtable_not_immutable",
                ErrorSeverity::Warning,
            ),
            TableError::CompactionAlreadyRunning => classification(
                "table",
                "state",
                "compaction_already_running",
                ErrorSeverity::Warning,
            ),
            TableError::CompactionThreadPanic => classification(
                "table",
                "internal",
                "compaction_thread_panic",
                ErrorSeverity::Critical,
            ),
            TableError::InvalidLevel { .. } => {
                classification("table", "validation", "invalid_level", ErrorSeverity::Error)
            }
            TableError::SStableIdExists { .. } => classification(
                "table",
                "consistency",
                "sstable_id_exists",
                ErrorSeverity::Error,
            ),
            TableError::SStableIdNotFound { .. } => classification(
                "table",
                "not_found",
                "sstable_id_not_found",
                ErrorSeverity::Warning,
            ),
            TableError::ManifestError { .. } => {
                classification("table", "manifest", "manifest_error", ErrorSeverity::Error)
            }
            TableError::InvalidOperationState { .. } => classification(
                "table",
                "state",
                "invalid_operation_state",
                ErrorSeverity::Error,
            ),
            TableError::SerializationError { .. } => classification(
                "table",
                "encoding",
                "serialization_error",
                ErrorSeverity::Error,
            ),
            TableError::Pager(_) => {
                classification("table", "dependency", "pager_error", ErrorSeverity::Error)
            }
            TableError::Wal(_) => {
                classification("table", "dependency", "wal_error", ErrorSeverity::Error)
            }
            TableError::Io(_) => classification("table", "io", "io_error", ErrorSeverity::Error),
            TableError::ValueRefNotFound { .. } => classification(
                "table",
                "not_found",
                "value_ref_not_found",
                ErrorSeverity::Warning,
            ),
            TableError::InvalidValueRef { .. } => classification(
                "table",
                "validation",
                "invalid_value_ref",
                ErrorSeverity::Warning,
            ),
            TableError::StaleValueRef { .. } => classification(
                "table",
                "corruption",
                "stale_value_ref",
                ErrorSeverity::Error,
            ),
            TableError::ValueTooLarge { .. } => classification(
                "table",
                "resource_exhaustion",
                "value_too_large",
                ErrorSeverity::Error,
            ),
            TableError::Other(_) => {
                classification("table", "internal", "other", ErrorSeverity::Error)
            }
        }
    }
}

impl ErrorTelemetry for TransactionError {
    fn classification(&self) -> ErrorClassification {
        match self {
            TransactionError::InvalidState { .. } => classification(
                "transaction",
                "state",
                "invalid_state",
                ErrorSeverity::Warning,
            ),
            TransactionError::WriteWriteConflict { .. } => classification(
                "transaction",
                "conflict",
                "write_write_conflict",
                ErrorSeverity::Warning,
            ),
            TransactionError::ReadWriteConflict { .. } => classification(
                "transaction",
                "conflict",
                "read_write_conflict",
                ErrorSeverity::Warning,
            ),
            TransactionError::SerializationConflict { .. } => classification(
                "transaction",
                "conflict",
                "serialization_conflict",
                ErrorSeverity::Warning,
            ),
            TransactionError::TransactionNotFound { .. } => classification(
                "transaction",
                "not_found",
                "transaction_not_found",
                ErrorSeverity::Warning,
            ),
            TransactionError::Deadlock { .. } => {
                classification("transaction", "deadlock", "deadlock", ErrorSeverity::Error)
            }
            TransactionError::Other(_) => {
                classification("transaction", "internal", "other", ErrorSeverity::Error)
            }
        }
    }
}

impl ErrorTelemetry for CursorError {
    fn classification(&self) -> ErrorClassification {
        match self {
            CursorError::InvalidState { .. } => {
                classification("cursor", "state", "invalid_state", ErrorSeverity::Warning)
            }
            CursorError::InvalidPosition { .. } => classification(
                "cursor",
                "validation",
                "invalid_position",
                ErrorSeverity::Warning,
            ),
            CursorError::Transaction(_) => classification(
                "cursor",
                "dependency",
                "transaction_error",
                ErrorSeverity::Error,
            ),
            CursorError::Other(_) => {
                classification("cursor", "internal", "other", ErrorSeverity::Error)
            }
        }
    }
}

impl ErrorTelemetry for FileSystemError {
    fn classification(&self) -> ErrorClassification {
        match self {
            FileSystemError::InvalidPath { .. } => {
                classification("vfs", "validation", "invalid_path", ErrorSeverity::Warning)
            }
            FileSystemError::PathExists { .. } => {
                classification("vfs", "conflict", "path_exists", ErrorSeverity::Warning)
            }
            FileSystemError::PathMissing { .. } => {
                classification("vfs", "not_found", "path_missing", ErrorSeverity::Warning)
            }
            FileSystemError::ParentMissing { .. } => {
                classification("vfs", "not_found", "parent_missing", ErrorSeverity::Warning)
            }
            FileSystemError::FileAlreadyLocked { .. } => classification(
                "vfs",
                "resource_busy",
                "file_already_locked",
                ErrorSeverity::Warning,
            ),
            FileSystemError::PermissionDenied { .. } => classification(
                "vfs",
                "permission",
                "permission_denied",
                ErrorSeverity::Error,
            ),
            FileSystemError::AlreadyLocked { .. } => classification(
                "vfs",
                "resource_busy",
                "already_locked",
                ErrorSeverity::Warning,
            ),
            FileSystemError::InvalidOperation { .. } => classification(
                "vfs",
                "validation",
                "invalid_operation",
                ErrorSeverity::Warning,
            ),
            FileSystemError::UnsupportedOperation { .. } => classification(
                "vfs",
                "unsupported",
                "unsupported_operation",
                ErrorSeverity::Warning,
            ),
            FileSystemError::InternalError(_) => {
                classification("vfs", "internal", "internal_error", ErrorSeverity::Critical)
            }
            FileSystemError::IOError(_) => {
                classification("vfs", "io", "io_error", ErrorSeverity::Error)
            }
            FileSystemError::WrappedError(_) => {
                classification("vfs", "dependency", "wrapped_error", ErrorSeverity::Error)
            }
        }
    }
}

fn classification(
    subsystem: &'static str,
    category: &'static str,
    variant: &'static str,
    severity: ErrorSeverity,
) -> ErrorClassification {
    ErrorClassification {
        subsystem,
        category,
        variant,
        severity,
    }
}

/// Record metrics and emit a structured tracing event for an error.
#[must_use]
pub fn record_error<E>(error: &E) -> ErrorObservation
where
    E: ErrorTelemetry + std::fmt::Display,
{
    let classification = error.classification();
    let timestamp_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let message = error.to_string();

    counter!(
        "nanostore.error.total",
        "subsystem" => classification.subsystem,
        "category" => classification.category,
        "variant" => classification.variant,
        "severity" => classification.severity.as_str()
    )
    .increment(1);

    counter!(
        "nanostore.error.category.total",
        "category" => classification.category,
        "severity" => classification.severity.as_str()
    )
    .increment(1);

    match classification.severity.tracing_level() {
        Level::WARN => warn!(
            subsystem = classification.subsystem,
            category = classification.category,
            variant = classification.variant,
            severity = classification.severity.as_str(),
            timestamp_secs = timestamp_secs,
            error = %message,
            "nanostore error recorded"
        ),
        _ => error!(
            subsystem = classification.subsystem,
            category = classification.category,
            variant = classification.variant,
            severity = classification.severity.as_str(),
            timestamp_secs = timestamp_secs,
            error = %message,
            "nanostore error recorded"
        ),
    };

    ErrorObservation {
        classification,
        timestamp_secs,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::NanostoreError;
    use crate::pager::{CompressionType, EncryptionType, PageId};
    use crate::table::TableError;
    use crate::txn::TransactionId;
    use crate::types::TableId;
    use crate::wal::LogSequenceNumber;
    use tracing_test::traced_test;

    #[test]
    fn classifies_pager_corruption_as_critical() {
        let err = PagerError::ChecksumMismatch(PageId::from(7));
        let classification = err.classification();

        assert_eq!(classification.subsystem, "pager");
        assert_eq!(classification.category, "corruption");
        assert_eq!(classification.variant, "checksum_mismatch");
        assert_eq!(classification.severity, ErrorSeverity::Critical);
    }

    #[test]
    fn classifies_transaction_conflicts_as_warnings() {
        let err = TransactionError::write_write_conflict(
            TableId::from(9_u64),
            b"key".to_vec(),
            TransactionId::from(1),
            TransactionId::from(2),
        );
        let classification = err.classification();

        assert_eq!(classification.subsystem, "transaction");
        assert_eq!(classification.category, "conflict");
        assert_eq!(classification.severity, ErrorSeverity::Warning);
    }

    #[test]
    fn classifies_unified_error_by_wrapped_subsystem() {
        let err = NanostoreError::from(WalError::corrupted_wal(42, "truncated", "unexpected EOF"));
        let classification = err.classification();

        assert_eq!(classification.subsystem, "wal");
        assert_eq!(classification.category, "corruption");
        assert_eq!(classification.variant, "corrupted_wal");
        assert_eq!(classification.severity, ErrorSeverity::Critical);
    }

    #[test]
    fn records_error_observation_details() {
        let err = TableError::corruption("sstable-1", "checksum", "footer mismatch");
        let observation = record_error(&err);

        assert_eq!(observation.classification.subsystem, "table");
        assert_eq!(observation.classification.category, "corruption");
        assert!(!observation.message.is_empty());
        assert!(observation.timestamp_secs > 0);
    }

    #[test]
    #[traced_test]
    fn emits_structured_log_for_recorded_error() {
        let err = WalError::encryption_error(
            LogSequenceNumber::from(10),
            "AES256-GCM",
            "failed to encrypt record",
        );

        let observation = record_error(&err);

        assert_eq!(observation.classification.subsystem, "wal");
        assert!(logs_contain("nanostore error recorded"));
        assert!(logs_contain(r#"subsystem="wal""#));
        assert!(logs_contain(r#"variant="encryption_error""#));
    }

    #[test]
    fn classifies_remaining_subsystems() {
        let vfs = FileSystemError::path_missing("missing.db");
        let cursor = CursorError::invalid_position("before first");
        let pager = PagerError::compression_error(PageId::from(1), CompressionType::Lz4, "failed");
        let pager_encryption =
            PagerError::encryption_error(PageId::from(2), EncryptionType::Aes256Gcm, "failed");

        assert_eq!(vfs.classification().subsystem, "vfs");
        assert_eq!(cursor.classification().subsystem, "cursor");
        assert_eq!(pager.classification().category, "encoding");
        assert_eq!(pager_encryption.classification().category, "encryption");
    }

    #[test]
    fn test_error_conversion() {
        // Test that errors convert properly
        let pager_err = PagerError::StorageFull;
        let nanostore_err: NanostoreError = pager_err.into();
        assert!(nanostore_err.is_pager());
        assert!(nanostore_err.as_pager().is_some());
    }

    #[test]
    fn test_error_type_checks() {
        let err = NanostoreError::other("test error");
        assert!(!err.is_pager());
        assert!(!err.is_wal());
        assert!(!err.is_table());
        assert!(!err.is_transaction());
        assert!(!err.is_cursor());
        assert!(!err.is_vfs());
        assert!(!err.is_io());
    }

    #[test]
    fn test_error_extraction() {
        let pager_err = PagerError::StorageFull;
        let nanostore_err: NanostoreError = pager_err.into();

        assert!(nanostore_err.as_pager().is_some());
        assert!(nanostore_err.as_wal().is_none());
        assert!(nanostore_err.as_table().is_none());
    }

    #[test]
    fn test_error_classification() {
        let err: NanostoreError = PagerError::StorageFull.into();
        let classification = err.classification();

        assert_eq!(classification.subsystem, "pager");
        assert_eq!(classification.category, "resource_exhaustion");
        assert_eq!(classification.variant, "storage_full");
        assert_eq!(classification.severity, ErrorSeverity::Error);
    }

    #[test]
    fn test_error_severity() {
        let err: NanostoreError = WalError::corrupted_wal(128, "checksum", "record damaged").into();
        assert_eq!(err.severity(), ErrorSeverity::Critical);
    }

    #[test]
    fn test_error_recording_returns_observation() {
        let err: NanostoreError = TableError::key_not_found("missing-key").into();
        let observation = err.record();

        assert_eq!(observation.classification.subsystem, "table");
        assert_eq!(observation.classification.category, "not_found");
        assert_eq!(observation.classification.variant, "key_not_found");
        assert_eq!(observation.classification.severity, ErrorSeverity::Warning);
        assert!(!observation.message.is_empty());
        assert!(observation.timestamp_secs > 0);
    }
}

// Made with Bob
