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

//! Superblock - Database state and metadata

use crate::pager::{PageId, PagerError, PagerResult};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Superblock structure (stored in page 1)
///
/// The superblock contains critical database state information:
/// - Free list management
/// - Page allocation state
/// - Transaction state
/// - Database statistics
///
/// Layout (fits within page data section):
/// - Bytes 0-7: Magic number for validation (u64)
/// - Bytes 8-15: Superblock version (u64)
/// - Bytes 16-23: Total allocated pages (u64)
/// - Bytes 24-31: Total free pages (u64)
/// - Bytes 32-39: First free list page ID (u64)
/// - Bytes 40-47: Last free list page ID (u64)
/// - Bytes 48-55: Next page ID to allocate (u64)
/// - Bytes 56-63: Transaction counter (u64)
/// - Bytes 64-71: Last checkpoint LSN (u64)
/// - Bytes 72-79: Root B-Tree page ID (u64)
/// - Bytes 80-87: B-Tree row count (u64)
/// - Bytes 88-95: Reserved (u64)
/// - Bytes 96-127: Reserved (32 bytes)
#[derive(Debug)]
pub struct Superblock {
    /// Magic number for validation
    magic: u64,
    /// Superblock version
    version: u64,
    /// Total number of allocated pages
    pub total_pages: u64,
    /// Total number of free pages
    pub free_pages: u64,
    /// First free list page ID (0 if no free list)
    pub first_free_list_page: PageId,
    /// Last free list page ID (0 if no free list)
    pub last_free_list_page: PageId,
    /// Next page ID to allocate (grows the database) - ATOMIC for thread safety
    next_page_id: Arc<AtomicU64>,
    /// Transaction counter (incremented on each transaction)
    pub transaction_counter: u64,
    /// Last checkpoint log sequence number
    pub last_checkpoint_lsn: u64,
    /// Root B-Tree page ID (0 if empty database)
    pub root_btree_page: PageId,
    /// B-Tree row count (total number of rows)
    pub btree_row_count: u64,
}

impl Clone for Superblock {
    fn clone(&self) -> Self {
        Self {
            magic: self.magic,
            version: self.version,
            total_pages: self.total_pages,
            free_pages: self.free_pages,
            first_free_list_page: self.first_free_list_page,
            last_free_list_page: self.last_free_list_page,
            // Clone the Arc, not the AtomicU64 - this shares the same atomic counter
            next_page_id: Arc::clone(&self.next_page_id),
            transaction_counter: self.transaction_counter,
            last_checkpoint_lsn: self.last_checkpoint_lsn,
            root_btree_page: self.root_btree_page,
            btree_row_count: self.btree_row_count,
        }
    }
}

impl Superblock {
    /// Magic number for superblock validation
    const MAGIC: u64 = 0x4E4B5355504552; // "NKSUPER" in ASCII

    /// Current superblock version
    const VERSION: u64 = 1;

    /// Size of the superblock in bytes
    pub const SIZE: usize = 128;

    /// Create a new superblock with default values
    pub fn new() -> Self {
        Self {
            magic: Self::MAGIC,
            version: Self::VERSION,
            total_pages: 2, // Header (0) + Superblock (1)
            free_pages: 0,
            first_free_list_page: PageId::from(0),
            last_free_list_page: PageId::from(0),
            next_page_id: Arc::new(AtomicU64::new(2)), // Next page to allocate
            transaction_counter: 0,
            last_checkpoint_lsn: 0,
            root_btree_page: PageId::from(0),
            btree_row_count: 0,
        }
    }

    /// Get the current next page ID (for serialization/inspection)
    pub fn next_page_id(&self) -> PageId {
        PageId::from(self.next_page_id.load(Ordering::SeqCst))
    }

    /// Serialize the superblock to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::SIZE);

        bytes.extend_from_slice(&self.magic.to_le_bytes());
        bytes.extend_from_slice(&self.version.to_le_bytes());
        bytes.extend_from_slice(&self.total_pages.to_le_bytes());
        bytes.extend_from_slice(&self.free_pages.to_le_bytes());
        bytes.extend_from_slice(&self.first_free_list_page.to_bytes());
        bytes.extend_from_slice(&self.last_free_list_page.to_bytes());
        bytes.extend_from_slice(&self.next_page_id.load(Ordering::SeqCst).to_le_bytes());
        bytes.extend_from_slice(&self.transaction_counter.to_le_bytes());
        bytes.extend_from_slice(&self.last_checkpoint_lsn.to_le_bytes());
        bytes.extend_from_slice(&self.root_btree_page.to_bytes());
        bytes.extend_from_slice(&self.btree_row_count.to_le_bytes());

        // Add reserved bytes
        bytes.resize(Self::SIZE, 0);

        bytes
    }

    /// Deserialize the superblock from bytes
    pub fn from_bytes(bytes: &[u8]) -> PagerResult<Self> {
        if bytes.len() < Self::SIZE {
            return Err(PagerError::invalid_superblock(
                "size",
                format!("{}", Self::SIZE),
                format!("{}", bytes.len()),
            ));
        }

        let magic = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        if magic != Self::MAGIC {
            return Err(PagerError::invalid_superblock(
                "magic",
                format!("0x{:X}", Self::MAGIC),
                format!("0x{:X}", magic),
            ));
        }

        let version = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        if version != Self::VERSION {
            return Err(PagerError::invalid_superblock(
                "version",
                format!("{}", Self::VERSION),
                format!("{}", version),
            ));
        }

        let total_pages = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let free_pages = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        let first_free_list_page =
            PageId::from(u64::from_le_bytes(bytes[32..40].try_into().unwrap()));
        let last_free_list_page =
            PageId::from(u64::from_le_bytes(bytes[40..48].try_into().unwrap()));
        let next_page_id_value = u64::from_le_bytes(bytes[48..56].try_into().unwrap());
        let transaction_counter = u64::from_le_bytes(bytes[56..64].try_into().unwrap());
        let last_checkpoint_lsn = u64::from_le_bytes(bytes[64..72].try_into().unwrap());
        let root_btree_page = PageId::from(u64::from_le_bytes(bytes[72..80].try_into().unwrap()));
        let btree_row_count = u64::from_le_bytes(bytes[80..88].try_into().unwrap());

        Ok(Self {
            magic,
            version,
            total_pages,
            free_pages,
            first_free_list_page,
            last_free_list_page,
            next_page_id: Arc::new(AtomicU64::new(next_page_id_value)),
            transaction_counter,
            last_checkpoint_lsn,
            root_btree_page,
            btree_row_count,
        })
    }

    /// Increment the transaction counter
    pub fn increment_transaction(&mut self) {
        self.transaction_counter += 1;
    }

    /// Update checkpoint LSN
    pub fn update_checkpoint(&mut self, lsn: u64) {
        self.last_checkpoint_lsn = lsn;
    }

    /// Allocate a new page (grows the database)
    ///
    /// This method uses atomic fetch_add to ensure thread-safe page ID generation.
    /// Multiple threads can call this simultaneously without risk of duplicate page IDs.
    pub fn allocate_new_page(&mut self) -> PageId {
        // Atomically fetch the current value and increment it
        // This is the KEY FIX for the race condition - fetch_add is atomic!
        let page_id = self.next_page_id.fetch_add(1, Ordering::SeqCst);
        self.total_pages += 1;
        PageId::from(page_id)
    }

    /// Mark a page as freed (add to free list)
    pub fn mark_page_freed(&mut self) {
        self.free_pages += 1;
    }

    /// Mark a page as allocated (remove from free list)
    pub fn mark_page_allocated(&mut self) {
        if self.free_pages > 0 {
            self.free_pages -= 1;
        }
    }
}

impl Default for Superblock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_superblock_creation() {
        let sb = Superblock::new();
        assert_eq!(sb.total_pages, 2);
        assert_eq!(sb.free_pages, 0);
        assert_eq!(sb.next_page_id(), PageId::from(2));
        assert_eq!(sb.transaction_counter, 0);
    }

    #[test]
    fn test_superblock_serialization() {
        let sb = Superblock::new();
        let bytes = sb.to_bytes();
        assert_eq!(bytes.len(), Superblock::SIZE);

        let deserialized = Superblock::from_bytes(&bytes).unwrap();
        assert_eq!(deserialized.total_pages, sb.total_pages);
        assert_eq!(deserialized.free_pages, sb.free_pages);
        assert_eq!(deserialized.next_page_id(), sb.next_page_id());
    }

    #[test]
    fn test_invalid_magic() {
        let mut bytes = vec![0u8; Superblock::SIZE];
        bytes[0..8].copy_from_slice(&0u64.to_le_bytes());

        let result = Superblock::from_bytes(&bytes);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PagerError::InvalidSuperblock { .. }
        ));
    }

    #[test]
    fn test_page_allocation() {
        let mut sb = Superblock::new();
        assert_eq!(sb.next_page_id(), PageId::from(2));
        assert_eq!(sb.total_pages, 2);

        let page_id = sb.allocate_new_page();
        assert_eq!(page_id, PageId::from(2));
        assert_eq!(sb.next_page_id(), PageId::from(3));
        assert_eq!(sb.total_pages, 3);
    }

    #[test]
    fn test_transaction_counter() {
        let mut sb = Superblock::new();
        assert_eq!(sb.transaction_counter, 0);

        sb.increment_transaction();
        assert_eq!(sb.transaction_counter, 1);

        sb.increment_transaction();
        assert_eq!(sb.transaction_counter, 2);
    }

    #[test]
    fn test_free_page_tracking() {
        let mut sb = Superblock::new();
        assert_eq!(sb.free_pages, 0);

        sb.mark_page_freed();
        assert_eq!(sb.free_pages, 1);

        sb.mark_page_allocated();
        assert_eq!(sb.free_pages, 0);
    }
}
