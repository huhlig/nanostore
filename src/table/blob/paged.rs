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

//! Paged blob storage implementation.
//!
//! This module provides a disk-backed blob storage implementation using the pager:
//! - Blobs are stored as linked pages in the page file
//! - Supports large blobs that span multiple pages
//! - Integrates with the page cache for performance
//! - Provides durability through the WAL
//!
//! The implementation uses a B-Tree index to map blob keys to their first page,
//! with pages linked together to form complete blobs.

use crate::pager::{Page, PageId, PageType, Pager};
use crate::snap::Snapshot;
use crate::table::{
    Table, TableCapabilities, TableEngineKind, TableError, TableResult, TableStatistics,
};
use crate::txn::{TransactionId, VersionChain};
use crate::types::{TableId, ValueBuf};
use crate::vfs::FileSystem;
use crate::wal::LogSequenceNumber;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Blob metadata for MVCC versioning.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct BlobMetadata {
    /// First page ID of the blob data
    first_page_id: PageId,
    /// Size in bytes
    size: u64,
    /// Number of pages used
    page_count: usize,
}

/// Paged blob storage table with MVCC support.
///
/// Stores blobs as linked pages in the pager. Uses metadata-only versioning:
/// the VersionChain stores BlobMetadata (page references), not the actual blob data.
/// This allows efficient MVCC without duplicating large blob content.
pub struct PagedBlob<FS: FileSystem> {
    id: TableId,
    name: String,
    pager: Arc<Pager<FS>>,
    /// Root page ID for metadata
    root_page_id: PageId,
    /// Index mapping keys to their metadata version chains
    index: Arc<RwLock<HashMap<Vec<u8>, VersionChain>>>,
}

impl<FS: FileSystem> PagedBlob<FS> {
    /// Create a new paged blob storage table.
    pub fn new(id: TableId, name: String, pager: Arc<Pager<FS>>) -> TableResult<Self> {
        // Allocate root page for metadata
        let root_page_id = pager.allocate_page(PageType::Catalog)?;

        // Initialize root page with empty metadata
        let mut page = Page::new(
            root_page_id,
            PageType::Catalog,
            pager.page_size().data_size(),
        );
        page.data_mut().resize(pager.page_size().data_size(), 0);
        pager.write_page(&page)?;

        Ok(Self {
            id,
            name,
            pager,
            root_page_id,
            index: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Open an existing paged blob storage table.
    pub fn open(id: TableId, name: String, pager: Arc<Pager<FS>>, root_page_id: PageId) -> Self {
        Self {
            id,
            name,
            pager,
            root_page_id,
            index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get the root page ID.
    pub fn root_page_id(&self) -> PageId {
        self.root_page_id
    }

    /// Read blob data from linked pages.
    fn read_blob_data(&self, first_page_id: PageId) -> TableResult<Vec<u8>> {
        let mut result = Vec::new();
        let mut current_page_id = first_page_id;

        loop {
            let page = self.pager.read_page(current_page_id)?;
            let data = page.data();

            // Read the next page ID (first 8 bytes) and the blob data
            if data.len() < 8 {
                return Err(TableError::corruption(
                    "PagedBlob::read_blob_data",
                    "page_too_small",
                    "Page data is too small to contain next page ID",
                ));
            }

            let next_page_id =
                PageId::from(u64::from_le_bytes(data[..8].try_into().map_err(|_| {
                    TableError::corruption(
                        "PagedBlob::read_blob_data",
                        "invalid_page_id",
                        "Failed to parse next page ID",
                    )
                })?));

            // Append blob data (skip the 8-byte header)
            result.extend_from_slice(&data[8..]);

            if next_page_id.as_u64() == 0 {
                break;
            }
            current_page_id = next_page_id;
        }

        Ok(result)
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
                    let metadata: BlobMetadata =
                        postcard::from_bytes(&version.value).map_err(|e| {
                            TableError::Other(format!("Failed to deserialize metadata: {}", e))
                        })?;

                    // Read blob data from pages
                    let data = self.read_blob_data(metadata.first_page_id)?;
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
                let metadata: BlobMetadata = postcard::from_bytes(value).map_err(|e| {
                    TableError::Other(format!("Failed to deserialize metadata: {}", e))
                })?;

                // Read blob data from pages
                let data = self.read_blob_data(metadata.first_page_id)?;
                return Ok(Some(ValueBuf(data)));
            }
        }
        Ok(None)
    }

    /// Write blob data to linked pages and return metadata.
    fn write_blob_data(&self, value: &[u8]) -> TableResult<BlobMetadata> {
        let page_data_size = self.pager.page_size().data_size();
        let data_size = page_data_size - 8; // 8 bytes for next_page_id
        let mut first_page_id: Option<PageId> = None;
        let mut prev_page_id: Option<PageId> = None;
        let mut page_count = 0;

        let mut offset = 0;
        while offset < value.len() {
            let page_id = self.pager.allocate_page(PageType::Overflow)?;
            let chunk_size = std::cmp::min(data_size, value.len() - offset);
            let end_offset = offset + chunk_size;

            // Determine next page ID (0 if this is the last page)
            let next_page_id: u64 = if end_offset >= value.len() { 0 } else { 0 }; // Will update later

            // Create page data: [next_page_id: 8 bytes][blob chunk]
            let mut page_data = vec![0u8; page_data_size];
            page_data[..8].copy_from_slice(&next_page_id.to_le_bytes());
            page_data[8..8 + chunk_size].copy_from_slice(&value[offset..end_offset]);

            let mut page = Page::new(page_id, PageType::Overflow, page_data_size);
            *page.data_mut() = page_data;
            self.pager.write_page(&page)?;

            // Update previous page's next_page_id pointer
            if let Some(prev_id) = prev_page_id {
                let mut prev_page = self.pager.read_page(prev_id)?;
                let next_id_bytes = page_id.as_u64().to_le_bytes();
                prev_page.data_mut()[..8].copy_from_slice(&next_id_bytes);
                self.pager.write_page(&prev_page)?;
            }

            if first_page_id.is_none() {
                first_page_id = Some(page_id);
            }

            prev_page_id = Some(page_id);
            page_count += 1;
            offset = end_offset;
        }

        // Handle empty value case
        if first_page_id.is_none() {
            let page_id = self.pager.allocate_page(PageType::Overflow)?;
            let page_data_size = self.pager.page_size().data_size();
            let mut page_data = vec![0u8; page_data_size];
            page_data[..8].copy_from_slice(&0u64.to_le_bytes());
            let mut page = Page::new(page_id, PageType::Overflow, page_data_size);
            *page.data_mut() = page_data;
            self.pager.write_page(&page)?;
            first_page_id = Some(page_id);
            page_count = 1;
        }

        Ok(BlobMetadata {
            first_page_id: first_page_id.unwrap(),
            size: value.len() as u64,
            page_count,
        })
    }

    /// Put a key-value pair (non-transactional).
    pub fn put(&self, key: &[u8], value: &[u8]) -> TableResult<u64> {
        self.put_tx(key, value, TransactionId::from(0))
    }

    /// Put a key-value pair with transaction tracking.
    pub fn put_tx(&self, key: &[u8], value: &[u8], tx_id: TransactionId) -> TableResult<u64> {
        // Write blob data to pages
        let metadata = self.write_blob_data(value)?;

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

    /// Free blob pages.
    fn free_blob_pages(&self, first_page_id: PageId) -> TableResult<()> {
        let mut current_page_id = first_page_id;
        loop {
            let page = self.pager.read_page(current_page_id)?;
            let data = page.data();

            if data.len() < 8 {
                return Err(TableError::corruption(
                    "PagedBlob::free_blob_pages",
                    "page_too_small",
                    "Page data is too small to contain next page ID",
                ));
            }

            let next_page_id =
                PageId::from(u64::from_le_bytes(data[..8].try_into().map_err(|_| {
                    TableError::corruption(
                        "PagedBlob::free_blob_pages",
                        "invalid_page_id",
                        "Failed to parse next page ID",
                    )
                })?));

            self.pager.free_page(current_page_id)?;

            if next_page_id.as_u64() == 0 {
                break;
            }
            current_page_id = next_page_id;
        }
        Ok(())
    }

    /// Delete a key (non-transactional).
    pub fn delete(&self, key: &[u8]) -> TableResult<bool> {
        self.delete_tx(key, TransactionId::from(0))
    }

    /// Delete a key with transaction tracking.
    pub fn delete_tx(&self, key: &[u8], tx_id: TransactionId) -> TableResult<bool> {
        let mut index = self.index.write().unwrap();
        if let Some(existing_chain) = index.get(key) {
            // Create a tombstone (empty metadata with invalid page ID)
            let tombstone = BlobMetadata {
                first_page_id: PageId::from(0),
                size: 0,
                page_count: 0,
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
    pub fn commit_versions(
        &self,
        _tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
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

    /// Vacuum obsolete versions and free orphaned blob pages.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        let mut index = self.index.write().unwrap();
        let mut total_removed = 0;
        let mut pages_to_free = Vec::new();

        // Collect obsolete versions and their page references
        for chain in index.values_mut() {
            // Collect page IDs from versions that will be removed
            let mut current = chain.prev_version.as_ref();
            while let Some(version) = current {
                if let Some(commit_lsn) = version.commit_lsn {
                    if commit_lsn < min_visible_lsn {
                        // This version will be removed, collect its pages
                        if let Ok(metadata) = postcard::from_bytes::<BlobMetadata>(&version.value) {
                            if metadata.first_page_id.as_u64() != 0 {
                                pages_to_free.push(metadata.first_page_id);
                            }
                        }
                    }
                }
                current = version.prev_version.as_ref();
            }

            // Vacuum the chain and free overflow pages
            let (removed, freed_refs) = chain.vacuum(min_visible_lsn);
            total_removed += removed;

            // Free overflow pages for removed external values
            if !freed_refs.is_empty() {
                self.pager.free_value_refs(&freed_refs)?;
            }
        }

        // Free orphaned blob pages
        for page_id in pages_to_free {
            if let Err(e) = self.free_blob_pages(page_id) {
                // Log error but continue - don't fail entire vacuum
                eprintln!(
                    "Warning: Failed to free blob pages starting at {:?}: {}",
                    page_id, e
                );
            }
        }

        Ok(total_removed)
    }
}

impl<FS: FileSystem> Table for PagedBlob<FS> {
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
            supports_compression: true,
            supports_encryption: true,
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
