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

//! In-memory blob storage implementation.
//!
//! This module provides a memory-resident blob storage implementation optimized for:
//! - Temporary blob storage and intermediate results
//! - Fast in-memory operations without disk I/O
//! - Testing and development
//! - Small to medium-sized blobs that fit in memory
//!
//! The implementation uses a HashMap to store blobs by key, with memory tracking
//! and optional size limits.

use crate::snap::Snapshot;
use crate::table::{
    Table, TableCapabilities, TableEngineKind, TableError, TableResult, TableStatistics,
};
use crate::txn::{TransactionId, VersionChain};
use crate::types::{TableId, ValueBuf};
use crate::wal::LogSequenceNumber;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Default memory budget for in-memory blob storage (64MB).
const DEFAULT_MEMORY_BUDGET: usize = 64 * 1024 * 1024;

/// Default maximum blob size (16MB).
const DEFAULT_MAX_BLOB_SIZE: u64 = 16 * 1024 * 1024;

/// Default inline threshold (4KB - blobs smaller than this should be stored inline).
const DEFAULT_INLINE_THRESHOLD: usize = 4 * 1024;

/// Blob metadata for MVCC versioning.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct BlobMetadata {
    /// Blob data
    data: Vec<u8>,
    /// Size in bytes
    size: u64,
}

/// In-memory blob storage table with MVCC support.
///
/// Stores blobs in a HashMap with memory tracking and size limits.
/// Uses metadata-only versioning: the VersionChain stores BlobMetadata,
/// not the raw blob data, for efficient MVCC support.
pub struct MemoryBlob {
    id: TableId,
    name: String,
    /// Shared blob metadata with version chains protected by RwLock for concurrent reads
    /// Maps key -> VersionChain of BlobMetadata
    metadata: Arc<RwLock<HashMap<Vec<u8>, VersionChain>>>,
    /// Memory usage tracking
    memory_usage: Arc<RwLock<usize>>,
    /// Memory budget in bytes
    memory_budget: usize,
    /// Maximum blob size
    max_blob_size: u64,
    /// Inline threshold
    inline_threshold: usize,
}

impl MemoryBlob {
    /// Create a new in-memory blob storage table.
    pub fn new(id: TableId, name: String) -> Self {
        Self::with_config(
            id,
            name,
            DEFAULT_MEMORY_BUDGET,
            DEFAULT_MAX_BLOB_SIZE,
            DEFAULT_INLINE_THRESHOLD,
        )
    }

    /// Create a new in-memory blob storage table with custom configuration.
    pub fn with_config(
        id: TableId,
        name: String,
        memory_budget: usize,
        max_blob_size: u64,
        inline_threshold: usize,
    ) -> Self {
        Self {
            id,
            name,
            metadata: Arc::new(RwLock::new(HashMap::new())),
            memory_usage: Arc::new(RwLock::new(0)),
            memory_budget,
            max_blob_size,
            inline_threshold,
        }
    }

    /// Get current memory usage.
    fn get_memory_usage(&self) -> usize {
        *self.memory_usage.read().unwrap()
    }

    /// Update memory usage by delta (can be negative).
    fn update_memory_usage(&self, delta: isize) {
        let mut usage = self.memory_usage.write().unwrap();
        if delta < 0 {
            *usage = usage.saturating_sub(delta.unsigned_abs());
        } else {
            *usage = usage.saturating_add(delta as usize);
        }
    }

    /// Estimate memory usage of a key-value pair.
    fn estimate_entry_size(key: &[u8], value: &[u8]) -> usize {
        key.len() + value.len() + std::mem::size_of::<Vec<u8>>() * 2
    }

    /// Get a value by key (non-transactional, returns latest committed version).
    pub fn get(&self, key: &[u8]) -> TableResult<Option<ValueBuf>> {
        let store = self.metadata.read().unwrap();
        if let Some(chain) = store.get(key) {
            // Find the latest committed version
            let mut current = Some(chain);
            while let Some(version) = current {
                if version.commit_lsn.is_some() {
                    // Deserialize metadata from inline value
                    let metadata_bytes = version.value.as_inline().ok_or_else(|| {
                        TableError::Other("BlobMetadata must be stored inline".to_string())
                    })?;
                    let metadata: BlobMetadata =
                        postcard::from_bytes(metadata_bytes).map_err(|e| {
                            TableError::Other(format!("Failed to deserialize metadata: {}", e))
                        })?;
                    return Ok(Some(ValueBuf(metadata.data)));
                }
                current = version.prev_version.as_deref();
            }
        }
        Ok(None)
    }

    /// Get a value by key with snapshot visibility.
    pub fn get_snapshot(&self, key: &[u8], snapshot: &Snapshot) -> TableResult<Option<ValueBuf>> {
        let store = self.metadata.read().unwrap();
        if let Some(chain) = store.get(key) {
            if let Some(version_value) = chain.find_visible_version(snapshot) {
                // Deserialize metadata from inline value
                let metadata_bytes = version_value.as_inline().ok_or_else(|| {
                    TableError::Other("BlobMetadata must be stored inline".to_string())
                })?;
                let metadata: BlobMetadata = postcard::from_bytes(metadata_bytes).map_err(|e| {
                    TableError::Other(format!("Failed to deserialize metadata: {}", e))
                })?;
                return Ok(Some(ValueBuf(metadata.data)));
            }
        }
        Ok(None)
    }

    /// Put a key-value pair (non-transactional, creates uncommitted version).
    pub fn put(&self, key: &[u8], value: &[u8]) -> TableResult<u64> {
        self.put_tx(key, value, TransactionId::from(0))
    }

    /// Put a key-value pair with transaction tracking.
    pub fn put_tx(&self, key: &[u8], value: &[u8], tx_id: TransactionId) -> TableResult<u64> {
        // Check value size limit
        if value.len() as u64 > self.max_blob_size {
            return Err(TableError::Other(format!(
                "Value size {} exceeds maximum {}",
                value.len(),
                self.max_blob_size
            )));
        }

        let mut store = self.metadata.write().unwrap();

        // Create metadata
        let metadata = BlobMetadata {
            data: value.to_vec(),
            size: value.len() as u64,
        };

        // Serialize metadata
        let metadata_bytes = postcard::to_allocvec(&metadata)
            .map_err(|e| TableError::Other(format!("Failed to serialize metadata: {}", e)))?;

        // Calculate memory delta
        let new_size = Self::estimate_entry_size(key, value);
        let old_size = store
            .get(key)
            .and_then(|chain| {
                // Get size of latest committed version
                let mut current = Some(chain);
                while let Some(version) = current {
                    if version.commit_lsn.is_some() {
                        if let Some(metadata_bytes) = version.value.as_inline() {
                            if let Ok(meta) = postcard::from_bytes::<BlobMetadata>(metadata_bytes) {
                                return Some(Self::estimate_entry_size(key, &meta.data));
                            }
                        }
                    }
                    current = version.prev_version.as_deref();
                }
                None
            })
            .unwrap_or(0);
        let delta = new_size as isize - old_size as isize;

        // Check memory budget
        let new_usage = (self.get_memory_usage() as isize + delta) as usize;
        if new_usage > self.memory_budget {
            return Err(TableError::Other(format!(
                "Memory budget exceeded: {} > {}",
                new_usage, self.memory_budget
            )));
        }

        // Create or prepend to version chain
        let new_chain = if let Some(existing_chain) = store.remove(key) {
            existing_chain.prepend(metadata_bytes, tx_id)
        } else {
            VersionChain::new(metadata_bytes, tx_id)
        };

        store.insert(key.to_vec(), new_chain);
        self.update_memory_usage(delta);

        Ok(value.len() as u64 + key.len() as u64 + 16) // +16 for overhead
    }

    /// Delete a key (non-transactional).
    pub fn delete(&self, key: &[u8]) -> TableResult<bool> {
        self.delete_tx(key, TransactionId::from(0))
    }

    /// Delete a key with transaction tracking.
    pub fn delete_tx(&self, key: &[u8], tx_id: TransactionId) -> TableResult<bool> {
        let mut store = self.metadata.write().unwrap();
        if let Some(existing_chain) = store.get(key) {
            // Create a tombstone (empty metadata)
            let tombstone = BlobMetadata {
                data: Vec::new(),
                size: 0,
            };
            let tombstone_bytes = postcard::to_allocvec(&tombstone)
                .map_err(|e| TableError::Other(format!("Failed to serialize tombstone: {}", e)))?;

            // Prepend tombstone to version chain
            let new_chain = existing_chain.clone().prepend(tombstone_bytes, tx_id);
            store.insert(key.to_vec(), new_chain);

            // Update memory usage (tombstone is small)
            self.update_memory_usage(-(Self::estimate_entry_size(key, &[]) as isize));
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Commit all uncommitted versions for a transaction.
    pub fn commit_versions(
        &self,
        _tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        let mut store = self.metadata.write().unwrap();
        for chain in store.values_mut() {
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

    /// Vacuum obsolete versions older than the minimum visible LSN.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        let mut store = self.metadata.write().unwrap();
        let mut total_removed = 0;

        for chain in store.values_mut() {
            let (removed, _freed_refs) = chain.vacuum(min_visible_lsn);
            total_removed += removed;
            // Note: MemoryBlob doesn't use overflow pages, so freed_refs should be empty
        }

        Ok(total_removed)
    }

    /// Get the maximum inline size.
    pub fn max_inline_size(&self) -> usize {
        self.inline_threshold
    }

    /// Get the maximum value size.
    pub fn max_value_size(&self) -> u64 {
        self.max_blob_size
    }
}

impl Table for MemoryBlob {
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
            memory_resident: true,
            disk_resident: false,
            supports_compression: false,
            supports_encryption: false,
        }
    }

    fn stats(&self) -> TableResult<TableStatistics> {
        let metadata = self.metadata.read().unwrap();
        let memory_usage = self.get_memory_usage();

        Ok(TableStatistics {
            row_count: Some(metadata.len() as u64),
            page_count: None, // Memory-based implementation doesn't use pages
            total_size_bytes: Some(memory_usage as u64),
            key_stats: None,
            value_stats: None,
            histogram: None,
            last_updated_lsn: Some(LogSequenceNumber::default()),
        })
    }
}

// Made with Bob
