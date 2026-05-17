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

//! File-based blob storage implementation.
//!
//! This module provides a file-system-based blob storage implementation:
//! - Each blob is stored as a separate file
//! - Suitable for very large blobs (GB+)
//! - Leverages OS file system caching
//! - Can use filesystem compression/encryption
//!
//! The implementation uses a directory structure to organize blob files,
//! with metadata stored separately for fast lookups.

use crate::snap::Snapshot;
use crate::table::{Table, TableCapabilities, TableEngineKind, TableError, TableResult, TableStatistics};
use crate::txn::{TransactionId, VersionChain};
use crate::types::{TableId, ValueBuf};
use crate::vfs::{File, FileSystem};
use crate::wal::LogSequenceNumber;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Blob metadata for MVCC versioning.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct BlobMetadata {
    /// File path where blob is stored
    file_path: PathBuf,
    /// Size in bytes
    size: u64,
}

/// File-based blob storage table with MVCC support.
///
/// Stores each blob as a separate file in a directory. Uses metadata-only versioning:
/// the VersionChain stores BlobMetadata (file paths), not the actual blob data.
pub struct FileBlob<FS: FileSystem> {
    id: TableId,
    name: String,
    base_path: PathBuf,
    fs: Arc<FS>,
    /// Index mapping keys to their metadata version chains
    index: Arc<RwLock<HashMap<Vec<u8>, VersionChain>>>,
}

impl<FS: FileSystem> FileBlob<FS> {
    /// Create a new file-based blob storage table.
    pub fn new(id: TableId, name: String, base_path: PathBuf, fs: Arc<FS>) -> Self {
        Self {
            id,
            name,
            base_path,
            fs,
            index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Generate a file path for a given key.
    fn key_to_path(&self, key: &[u8]) -> PathBuf {
        // Use hex encoding of the key as the filename
        let hex_key: String = key.iter().map(|b| format!("{:02x}", b)).collect();
        self.base_path.join(hex_key)
    }

    /// Read blob data from file.
    fn read_blob_file(&self, path: &PathBuf) -> TableResult<Vec<u8>> {
        match self.fs.open_file(path.to_str().unwrap_or("")) {
            Ok(mut file) => {
                // Get file size
                let size = file.get_size().map_err(|e| {
                    TableError::Other(format!("Failed to get file size: {}", e))
                })?;

                // Read file contents
                let mut buffer = vec![0u8; size as usize];
                file.read_at_offset(0, &mut buffer).map_err(|e| {
                    TableError::Other(format!("Failed to read file: {}", e))
                })?;

                Ok(buffer)
            }
            Err(e) => Err(TableError::Other(format!("Failed to open file: {}", e))),
        }
    }

    /// Get a value by key (non-transactional, returns latest committed version).
    pub fn get(&self, key: &[u8]) -> TableResult<Option<ValueBuf>> {
        let index = self.index.read().unwrap();
        if let Some(chain) = index.get(key) {
            // Find the latest committed version
            let mut current = Some(chain);
            while let Some(version) = current {
                if version.commit_lsn.is_some() {
                    // Deserialize metadata
                    let metadata: BlobMetadata = postcard::from_bytes(&version.value)
                        .map_err(|e| TableError::Other(format!("Failed to deserialize metadata: {}", e)))?;
                    
                    // Read blob data from file
                    let data = self.read_blob_file(&metadata.file_path)?;
                    return Ok(Some(ValueBuf(data)));
                }
                current = version.prev_version.as_deref();
            }
        }
        Ok(None)
    }

    /// Get a value by key with snapshot visibility.
    pub fn get_snapshot(&self, key: &[u8], snapshot: &Snapshot) -> TableResult<Option<ValueBuf>> {
        let index = self.index.read().unwrap();
        if let Some(chain) = index.get(key) {
            if let Some(value) = chain.find_visible_version(snapshot) {
                // Deserialize metadata
                let metadata: BlobMetadata = postcard::from_bytes(value)
                    .map_err(|e| TableError::Other(format!("Failed to deserialize metadata: {}", e)))?;
                
                // Read blob data from file
                let data = self.read_blob_file(&metadata.file_path)?;
                return Ok(Some(ValueBuf(data)));
            }
        }
        Ok(None)
    }

    /// Put a key-value pair (non-transactional).
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> TableResult<u64> {
        self.put_tx(key, value, TransactionId::from(0))
    }

    /// Put a key-value pair with transaction tracking.
    pub fn put_tx(&mut self, key: &[u8], value: &[u8], tx_id: TransactionId) -> TableResult<u64> {
        let path = self.key_to_path(key);
        let path_str = path.to_str().unwrap_or("");

        // Create or overwrite the file
        let mut file = self.fs.create_file(path_str).map_err(|e| {
            TableError::Other(format!("Failed to create file: {}", e))
        })?;

        file.write_all(value)
            .map_err(|e| TableError::Other(format!("Failed to write file: {}", e)))?;

        // Create metadata
        let metadata = BlobMetadata {
            file_path: path,
            size: value.len() as u64,
        };

        // Serialize metadata
        let metadata_bytes = postcard::to_allocvec(&metadata)
            .map_err(|e| TableError::Other(format!("Failed to serialize metadata: {}", e)))?;

        // Create or prepend to version chain
        let mut index = self.index.write().unwrap();
        let new_chain = if let Some(existing_chain) = index.remove(key) {
            existing_chain.prepend(metadata_bytes, tx_id)
        } else {
            VersionChain::new(metadata_bytes, tx_id)
        };

        index.insert(key.to_vec(), new_chain);

        Ok(value.len() as u64)
    }

    /// Delete a key (non-transactional).
    pub fn delete(&mut self, key: &[u8]) -> TableResult<bool> {
        self.delete_tx(key, TransactionId::from(0))
    }

    /// Delete a key with transaction tracking.
    pub fn delete_tx(&mut self, key: &[u8], tx_id: TransactionId) -> TableResult<bool> {
        let mut index = self.index.write().unwrap();
        if let Some(existing_chain) = index.get(key) {
            // Create a tombstone (empty metadata with empty path)
            let tombstone = BlobMetadata {
                file_path: PathBuf::new(),
                size: 0,
            };
            let tombstone_bytes = postcard::to_allocvec(&tombstone)
                .map_err(|e| TableError::Other(format!("Failed to serialize tombstone: {}", e)))?;

            // Prepend tombstone to version chain
            let new_chain = existing_chain.clone().prepend(tombstone_bytes, tx_id);
            index.insert(key.to_vec(), new_chain);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Commit all uncommitted versions for a transaction.
    pub fn commit_versions(&self, _tx_id: TransactionId, commit_lsn: LogSequenceNumber) -> TableResult<()> {
        let mut index = self.index.write().unwrap();
        for chain in index.values_mut() {
            Self::commit_all_uncommitted(chain, commit_lsn);
        }
        Ok(())
    }

    /// Recursively commit all uncommitted versions in a chain.
    fn commit_all_uncommitted(chain: &mut VersionChain, commit_lsn: LogSequenceNumber) {
        if chain.commit_lsn.is_none() {
            chain.commit(commit_lsn);
        }
        if let Some(prev) = chain.prev_version.as_mut() {
            Self::commit_all_uncommitted(prev, commit_lsn);
        }
    }

    /// Vacuum obsolete versions and delete orphaned blob files.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        let mut index = self.index.write().unwrap();
        let mut total_removed = 0;
        let mut files_to_delete = Vec::new();

        // Collect obsolete versions and their file paths
        for chain in index.values_mut() {
            // Collect file paths from versions that will be removed
            let mut current = chain.prev_version.as_ref();
            while let Some(version) = current {
                if let Some(commit_lsn) = version.commit_lsn {
                    if commit_lsn < min_visible_lsn {
                        // This version will be removed, collect its file path
                        if let Ok(metadata) = postcard::from_bytes::<BlobMetadata>(&version.value) {
                            if !metadata.file_path.as_os_str().is_empty() {
                                files_to_delete.push(metadata.file_path.clone());
                            }
                        }
                    }
                }
                current = version.prev_version.as_ref();
            }

            // Vacuum the chain
            total_removed += chain.vacuum(min_visible_lsn);
        }

        // Delete orphaned blob files
        for path in files_to_delete {
            if let Some(path_str) = path.to_str() {
                if let Err(e) = self.fs.remove_file(path_str) {
                    // Log error but continue - don't fail entire vacuum
                    eprintln!("Warning: Failed to delete blob file {:?}: {}", path, e);
                }
            }
        }

        Ok(total_removed)
    }
}

impl<FS: FileSystem> Table for FileBlob<FS> {
    fn table_id(&self) -> TableId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> TableEngineKind {
        TableEngineKind::Blob
    }

    fn capabilities(&self) -> TableCapabilities {
        TableCapabilities {
            ordered: false,
            point_lookup: true,
            prefix_scan: false,
            reverse_scan: false,
            range_delete: false,
            merge_operator: false,
            mvcc_native: true,
            append_optimized: false,
            memory_resident: false,
            disk_resident: true,
            supports_compression: false,
            supports_encryption: false,
        }
    }

    fn stats(&self) -> TableResult<TableStatistics> {
        let index = self.index.read().unwrap();
        Ok(TableStatistics {
            row_count: Some(index.len() as u64),
            total_size_bytes: None,
            key_stats: None,
            value_stats: None,
            histogram: None,
            last_updated_lsn: None,
        })
    }
}

// Made with Bob
