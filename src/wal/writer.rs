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

//! WAL writer - Handles writing records to the WAL file

use crate::pager::{CompressionType, EncryptionType};
use crate::txn::TransactionId;
use crate::types::TableId;
use crate::vfs::{File, FileSystem};
use crate::wal::commit::{GroupCommitConfig, GroupCommitCoordinator};
use crate::wal::{LogSequenceNumber, RecordData, WalError, WalRecord, WalResult};
use metrics::{counter, gauge, histogram};
use parking_lot::RwLock;
use std::collections::HashSet;
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, instrument, warn};

/// WAL writer configuration
#[derive(Debug, Clone)]
pub struct WalWriterConfig {
    /// Buffer size for batching writes (bytes)
    pub buffer_size: usize,
    /// Whether to sync after each write (ignored if group commit is enabled)
    pub sync_on_write: bool,
    /// Maximum WAL file size (bytes)
    pub max_wal_size: u64,
    /// Compression type for WAL records
    pub compression: CompressionType,
    /// Encryption type for WAL records
    pub encryption: EncryptionType,
    /// Encryption key (32 bytes for AES-256)
    pub encryption_key: Option<[u8; 32]>,
    /// Group commit configuration
    pub group_commit: GroupCommitConfig,
}

impl Default for WalWriterConfig {
    fn default() -> Self {
        Self {
            buffer_size: 64 * 1024,             // 64KB buffer
            sync_on_write: true,                // Sync by default for durability
            max_wal_size: 1024 * 1024 * 1024,   // 1GB max
            compression: CompressionType::None, // No compression by default
            encryption: EncryptionType::None,
            encryption_key: None,
            group_commit: GroupCommitConfig::default(),
        }
    }
}

/// WAL writer state
struct WalWriterState {
    /// Current LSN (incremented for each record)
    current_lsn: LogSequenceNumber,
    /// Current file offset
    current_offset: u64,
    /// Active transactions
    active_txns: HashSet<TransactionId>,
    /// Write buffer
    buffer: Vec<u8>,
}

/// WAL writer - Manages writing records to the WAL file
pub struct WalWriter<FS: FileSystem> {
    /// VFS file handle
    file: Arc<RwLock<FS::File>>,
    /// Configuration
    config: WalWriterConfig,
    /// Encryption type
    encryption: EncryptionType,
    /// Encryption key
    encryption_key: Option<[u8; 32]>,
    /// Writer state
    state: Arc<RwLock<WalWriterState>>,
    /// Group commit coordinator (if enabled)
    group_commit: Option<Arc<GroupCommitCoordinator>>,
}

impl<FS: FileSystem> WalWriter<FS> {
    /// Create a new WAL writer
    #[instrument(skip(fs, config), fields(path = %path))]
    pub fn create(fs: &FS, path: &str, config: WalWriterConfig) -> WalResult<Self> {
        debug!("Creating new WAL writer");
        let file = fs.create_file(path)?;

        let state = WalWriterState {
            current_lsn: LogSequenceNumber::from(1), // Start from LSN 1
            current_offset: 0,
            active_txns: HashSet::new(),
            buffer: Vec::with_capacity(config.buffer_size),
        };

        let file_arc = Arc::new(RwLock::new(file));
        let state_arc = Arc::new(RwLock::new(state));

        // Create group commit coordinator if enabled
        let group_commit = if config.group_commit.enabled {
            let file_clone = file_arc.clone();
            let state_clone = state_arc.clone();

            let coordinator = GroupCommitCoordinator::new(config.group_commit.clone(), move || {
                let mut state = state_clone.write();
                if state.buffer.is_empty() {
                    return Ok(());
                }

                let mut file = file_clone.write();
                file.write_all(&state.buffer).map_err(WalError::IoError)?;

                // Always sync in group commit mode - the coordinator handles timing
                file.sync_data()?;

                state.buffer.clear();
                Ok(())
            });
            Some(Arc::new(coordinator))
        } else {
            None
        };

        Ok(Self {
            file: file_arc,
            encryption: config.encryption,
            encryption_key: config.encryption_key,
            config,
            state: state_arc,
            group_commit,
        })
    }

    /// Open an existing WAL file
    #[instrument(skip(fs, config), fields(path = %path))]
    pub fn open(fs: &FS, path: &str, config: WalWriterConfig) -> WalResult<Self> {
        debug!("Opening existing WAL writer");
        let file = fs.open_file(path)?;

        // Get file size to determine current offset
        let file_size = file.get_size()?;

        // Scan the file to find the last LSN and active transactions
        let (last_lsn, active_txns) = Self::scan_wal_file(fs, path, config.encryption_key)?;

        // Next LSN is last_lsn + 1
        let current_lsn = LogSequenceNumber::from(last_lsn.as_u64() + 1);

        let state = WalWriterState {
            current_lsn,
            current_offset: file_size,
            active_txns,
            buffer: Vec::with_capacity(config.buffer_size),
        };

        let file_arc = Arc::new(RwLock::new(file));
        let state_arc = Arc::new(RwLock::new(state));

        // Create group commit coordinator if enabled
        let group_commit = if config.group_commit.enabled {
            let file_clone = file_arc.clone();
            let state_clone = state_arc.clone();
            let sync_on_write = config.sync_on_write;

            let coordinator = GroupCommitCoordinator::new(config.group_commit.clone(), move || {
                let mut state = state_clone.write();
                if state.buffer.is_empty() {
                    return Ok(());
                }

                let mut file = file_clone.write();
                file.write_all(&state.buffer).map_err(WalError::IoError)?;

                if sync_on_write {
                    file.sync_data()?;
                }

                state.buffer.clear();
                Ok(())
            });
            Some(Arc::new(coordinator))
        } else {
            None
        };

        Ok(Self {
            file: file_arc,
            encryption: config.encryption,
            encryption_key: config.encryption_key,
            config,
            state: state_arc,
            group_commit,
        })
    }

    /// Scan the WAL file to find the last LSN and active transactions.
    fn scan_wal_file(
        fs: &FS,
        path: &str,
        encryption_key: Option<[u8; 32]>,
    ) -> WalResult<(LogSequenceNumber, HashSet<TransactionId>)> {
        use crate::wal::RecordData;
        use crate::wal::reader::WalReader;

        let mut reader = WalReader::open(fs, path, encryption_key)?;
        let mut last_lsn = LogSequenceNumber::from(0);
        let mut active_txns = HashSet::new();

        // Read all records to find last LSN and track active transactions
        while let Some(record) = reader.read_next()? {
            last_lsn = record.lsn;

            match &record.data {
                RecordData::Begin { txn_id } => {
                    active_txns.insert(*txn_id);
                }
                RecordData::Commit { txn_id } | RecordData::Rollback { txn_id } => {
                    active_txns.remove(txn_id);
                }
                RecordData::Checkpoint {
                    lsn: _,
                    active_txns: checkpoint_txns,
                } => {
                    // Checkpoint records contain the definitive list of active transactions
                    // at that point, so we can reset our tracking
                    active_txns.clear();
                    active_txns.extend(checkpoint_txns.iter().copied());
                }
                RecordData::Write { .. } => {
                    // Write records don't change transaction state
                }
                RecordData::Prepare { .. } => {
                    // Prepare records don't change transaction state
                    // Transaction remains active until commit or rollback
                }
            }
        }

        Ok((last_lsn, active_txns))
    }

    /// Write a BEGIN record
    #[instrument(skip(self), fields(txn_id = %txn_id))]
    pub fn write_begin(&self, txn_id: TransactionId) -> WalResult<LogSequenceNumber> {
        let start = Instant::now();
        debug!("Writing BEGIN record");
        let mut state = self.state.write();

        // Check if transaction already exists
        if state.active_txns.contains(&txn_id) {
            warn!("Transaction already exists");
            counter!("wal.error", "type" => "transaction_already_exists").increment(1);
            return Err(WalError::TransactionAlreadyExists { txn_id });
        }

        // Create record
        let lsn = state.current_lsn;
        let record = WalRecord::new(
            lsn,
            RecordData::Begin { txn_id },
            self.config.compression,
            self.encryption,
        );

        // Write record
        self.write_record_internal(&mut state, record)?;

        // Track active transaction
        state.active_txns.insert(txn_id);

        counter!("wal.write").increment(1);
        histogram!("wal.write_duration").record(start.elapsed().as_secs_f64());
        gauge!("wal.active_transactions").set(state.active_txns.len() as f64);

        Ok(lsn)
    }

    /// Write a WRITE record
    #[instrument(skip(self, key, value), fields(txn_id = %txn_id, table_id = %table_id, key_len = key.len(), value_len = value.len()))]
    pub fn write_operation(
        &self,
        txn_id: TransactionId,
        table_id: TableId,
        op_type: crate::wal::WriteOpType,
        key: Vec<u8>,
        value: Vec<u8>,
    ) -> WalResult<LogSequenceNumber> {
        let start = Instant::now();
        let bytes_written = key.len() + value.len();
        debug!("Writing WRITE record");
        let mut state = self.state.write();

        // Check if transaction exists
        if !state.active_txns.contains(&txn_id) {
            warn!("Transaction not found");
            counter!("wal.error", "type" => "transaction_not_found").increment(1);
            return Err(WalError::TransactionNotFound { txn_id });
        }

        // Create record
        let lsn = state.current_lsn;
        let record = WalRecord::new(
            lsn,
            RecordData::Write {
                txn_id,
                table_id,
                op_type,
                key,
                value,
            },
            self.config.compression,
            self.encryption,
        );

        // Write record
        self.write_record_internal(&mut state, record)?;

        counter!("wal.write").increment(1);
        counter!("wal.bytes_written").increment(bytes_written as u64);
        histogram!("wal.write_duration").record(start.elapsed().as_secs_f64());

        Ok(lsn)
    }

    /// Write a COMMIT record
    #[instrument(skip(self), fields(txn_id = %txn_id))]
    pub fn write_commit(&self, txn_id: TransactionId) -> WalResult<LogSequenceNumber> {
        let start = Instant::now();
        debug!("Writing COMMIT record");
        let lsn = {
            let mut state = self.state.write();

            // Check if transaction exists
            if !state.active_txns.contains(&txn_id) {
                warn!("Transaction not found");
                counter!("wal.error", "type" => "transaction_not_found").increment(1);
                return Err(WalError::TransactionNotFound { txn_id });
            }

            // Create record
            let lsn = state.current_lsn;
            let record = WalRecord::new(
                lsn,
                RecordData::Commit { txn_id },
                self.config.compression,
                self.encryption,
            );

            // Write record to buffer
            self.write_record_internal(&mut state, record)?;

            // Remove from active transactions
            state.active_txns.remove(&txn_id);

            gauge!("wal.active_transactions").set(state.active_txns.len() as f64);

            lsn
        };

        // If group commit is enabled, submit to coordinator
        // Otherwise, flush immediately
        if let Some(coordinator) = &self.group_commit {
            coordinator.submit_commit(txn_id, lsn)?;
        } else if self.config.sync_on_write {
            let sync_start = Instant::now();
            self.flush()?;
            histogram!("wal.sync_duration").record(sync_start.elapsed().as_secs_f64());
        }

        counter!("wal.write").increment(1);
        histogram!("wal.write_duration").record(start.elapsed().as_secs_f64());

        Ok(lsn)
    }

    /// Write a PREPARE record (two-phase commit)
    #[instrument(skip(self), fields(txn_id = %txn_id))]
    pub fn write_prepare(&self, txn_id: TransactionId) -> WalResult<LogSequenceNumber> {
        let start = Instant::now();
        debug!("Writing PREPARE record");
        let mut state = self.state.write();

        // Check if transaction exists
        if !state.active_txns.contains(&txn_id) {
            warn!("Transaction not found");
            counter!("wal.error", "type" => "transaction_not_found").increment(1);
            return Err(WalError::TransactionNotFound { txn_id });
        }

        // Create record
        let lsn = state.current_lsn;
        let record = WalRecord::new(
            lsn,
            RecordData::Prepare { txn_id },
            self.config.compression,
            self.encryption,
        );

        // Write record
        self.write_record_internal(&mut state, record)?;

        // Transaction remains active after prepare
        // It will be removed on commit or rollback

        counter!("wal.write").increment(1);
        histogram!("wal.write_duration").record(start.elapsed().as_secs_f64());

        Ok(lsn)
    }

    /// Write a ROLLBACK record
    pub fn write_rollback(&self, txn_id: TransactionId) -> WalResult<LogSequenceNumber> {
        let mut state = self.state.write();

        // Check if transaction exists
        if !state.active_txns.contains(&txn_id) {
            return Err(WalError::TransactionNotFound { txn_id });
        }

        // Create record
        let lsn = state.current_lsn;
        let record = WalRecord::new(
            lsn,
            RecordData::Rollback { txn_id },
            self.config.compression,
            self.encryption,
        );

        // Write record
        self.write_record_internal(&mut state, record)?;

        // Remove from active transactions
        state.active_txns.remove(&txn_id);

        Ok(lsn)
    }

    /// Write a CHECKPOINT record
    pub fn write_checkpoint(&self) -> WalResult<LogSequenceNumber> {
        let mut state = self.state.write();

        // Create record with current active transactions
        let lsn = state.current_lsn;
        let active_txns: Vec<TransactionId> = state.active_txns.iter().copied().collect();
        let record = WalRecord::new(
            lsn,
            RecordData::Checkpoint { lsn, active_txns },
            self.config.compression,
            self.encryption,
        );

        // Write record
        self.write_record_internal(&mut state, record)?;

        Ok(lsn)
    }

    /// Internal method to write a record
    fn write_record_internal(
        &self,
        state: &mut WalWriterState,
        record: WalRecord,
    ) -> WalResult<()> {
        // Serialize record
        let bytes = record.to_bytes(self.encryption_key.as_ref())?;

        // Check if WAL is full
        if state.current_offset + bytes.len() as u64 > self.config.max_wal_size {
            return Err(WalError::WalFull {
                current_size: state.current_offset + bytes.len() as u64,
                max_size: self.config.max_wal_size,
            });
        }

        // Add to buffer
        state.buffer.extend_from_slice(&bytes);

        // Flush if buffer is full, or if sync is enabled AND group commit is disabled
        let should_flush = state.buffer.len() >= self.config.buffer_size
            || (self.config.sync_on_write && self.group_commit.is_none());

        if should_flush {
            self.flush_internal(state)?;
        }

        // Update state
        state.current_lsn = LogSequenceNumber::from(state.current_lsn.as_u64() + 1);
        state.current_offset += bytes.len() as u64;

        Ok(())
    }

    /// Flush buffered writes to disk
    #[instrument(skip(self))]
    pub fn flush(&self) -> WalResult<()> {
        debug!("Flushing WAL buffer");
        let mut state = self.state.write();
        self.flush_internal(&mut state)
    }

    /// Internal flush implementation
    fn flush_internal(&self, state: &mut WalWriterState) -> WalResult<()> {
        if state.buffer.is_empty() {
            return Ok(());
        }

        let mut file = self.file.write();

        // Write buffer to file
        file.write_all(&state.buffer).map_err(WalError::IoError)?;

        // Sync if configured AND group commit is disabled
        // When group commit is enabled, syncing is handled by the coordinator
        if self.config.sync_on_write && self.group_commit.is_none() {
            counter!("wal.sync").increment(1);
            file.sync_data()?;
        }

        // Clear buffer
        state.buffer.clear();

        gauge!("wal.size_bytes").set(state.current_offset as f64);

        Ok(())
    }

    /// Get current LSN
    pub fn current_lsn(&self) -> LogSequenceNumber {
        self.state.read().current_lsn
    }

    /// Get active transactions
    pub fn active_transactions(&self) -> Vec<TransactionId> {
        self.state.read().active_txns.iter().copied().collect()
    }

    /// Get current file size
    pub fn file_size(&self) -> u64 {
        self.state.read().current_offset
    }

    /// Truncate the WAL file (used after checkpoint)
    pub fn truncate(&self) -> WalResult<()> {
        let mut state = self.state.write();
        let mut file = self.file.write();

        // Flush any pending writes
        if !state.buffer.is_empty() {
            file.write_all(&state.buffer).map_err(WalError::IoError)?;
            state.buffer.clear();
        }

        // Truncate file
        file.truncate()?;

        // Reset state
        state.current_offset = 0;

        Ok(())
    }

    /// Get group commit metrics (if enabled)
    pub fn group_commit_metrics(&self) -> Option<&crate::wal::GroupCommitMetrics> {
        self.group_commit.as_ref().map(|gc| gc.metrics())
    }

    /// Check if group commit is enabled
    pub fn is_group_commit_enabled(&self) -> bool {
        self.group_commit.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::MemoryFileSystem;
    use crate::wal::WriteOpType;

    #[test]
    fn test_create_wal_writer() {
        let fs = MemoryFileSystem::new();
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "test.wal", config).unwrap();

        assert_eq!(writer.current_lsn(), LogSequenceNumber::from(1));
        assert_eq!(writer.file_size(), 0);
    }

    #[test]
    fn test_write_begin() {
        let fs = MemoryFileSystem::new();
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "test.wal", config).unwrap();

        let lsn = writer.write_begin(TransactionId::from(1)).unwrap();
        assert_eq!(lsn, LogSequenceNumber::from(1));
        assert_eq!(writer.current_lsn(), LogSequenceNumber::from(2));
        assert_eq!(writer.active_transactions(), vec![TransactionId::from(1)]);
    }

    #[test]
    fn test_write_operation() {
        let fs = MemoryFileSystem::new();
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "test.wal", config).unwrap();

        writer.write_begin(TransactionId::from(1)).unwrap();
        let lsn = writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"key1".to_vec(),
                b"value1".to_vec(),
            )
            .unwrap();

        assert_eq!(lsn, LogSequenceNumber::from(2));
        assert_eq!(writer.current_lsn(), LogSequenceNumber::from(3));
    }

    #[test]
    fn test_write_commit() {
        let fs = MemoryFileSystem::new();
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "test.wal", config).unwrap();

        writer.write_begin(TransactionId::from(1)).unwrap();
        let lsn = writer.write_commit(TransactionId::from(1)).unwrap();

        assert_eq!(lsn, LogSequenceNumber::from(2));
        assert_eq!(writer.current_lsn(), LogSequenceNumber::from(3));
        assert!(writer.active_transactions().is_empty());
    }

    #[test]
    fn test_write_rollback() {
        let fs = MemoryFileSystem::new();
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "test.wal", config).unwrap();

        writer.write_begin(TransactionId::from(1)).unwrap();
        let lsn = writer.write_rollback(TransactionId::from(1)).unwrap();

        assert_eq!(lsn, LogSequenceNumber::from(2));
        assert_eq!(writer.current_lsn(), LogSequenceNumber::from(3));
        assert!(writer.active_transactions().is_empty());
    }

    #[test]
    fn test_write_checkpoint() {
        let fs = MemoryFileSystem::new();
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "test.wal", config).unwrap();

        writer.write_begin(TransactionId::from(1)).unwrap();
        writer.write_begin(TransactionId::from(2)).unwrap();

        let lsn = writer.write_checkpoint().unwrap();
        assert_eq!(lsn, LogSequenceNumber::from(3));

        let active = writer.active_transactions();
        assert_eq!(active.len(), 2);
        assert!(active.contains(&TransactionId::from(1)));
        assert!(active.contains(&TransactionId::from(2)));
    }

    #[test]
    fn test_transaction_not_found() {
        let fs = MemoryFileSystem::new();
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "test.wal", config).unwrap();

        let result = writer.write_commit(TransactionId::from(999));
        assert!(
            matches!(result, Err(WalError::TransactionNotFound { txn_id }) if txn_id == TransactionId::from(999))
        );
    }

    #[test]
    fn test_transaction_already_exists() {
        let fs = MemoryFileSystem::new();
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "test.wal", config).unwrap();

        writer.write_begin(TransactionId::from(1)).unwrap();
        let result = writer.write_begin(TransactionId::from(1));
        assert!(
            matches!(result, Err(WalError::TransactionAlreadyExists { txn_id }) if txn_id == TransactionId::from(1))
        );
    }
}
