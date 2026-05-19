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

//! Free list management for tracking available pages

use crate::pager::{PageId, PagerError, PagerResult};
use crossbeam::queue::SegQueue;
use metrics::{counter, gauge};
use std::sync::atomic::{AtomicU64, Ordering};

/// Free list page structure
///
/// Each free list page contains:
/// - A list of free page IDs
/// - A pointer to the next free list page (if any)
///
/// Layout:
/// - Bytes 0-7: Next free list page ID (0 if last)
/// - Bytes 8-15: Number of entries in this page
/// - Bytes 16+: Array of free page IDs (u64 each)
pub struct FreeListPage {
    /// Next free list page ID (0 if this is the last page)
    pub next_page: PageId,
    /// Free page IDs stored in this page
    pub free_pages: Vec<PageId>,
}

impl FreeListPage {
    /// Header size (next_page + count)
    const HEADER_SIZE: usize = 16;

    /// Create a new empty free list page
    pub fn new() -> Self {
        Self {
            next_page: PageId::from(0),
            free_pages: Vec::new(),
        }
    }

    /// Calculate maximum number of entries that fit in a page
    pub fn max_entries(page_data_size: usize) -> usize {
        (page_data_size - Self::HEADER_SIZE) / 8
    }

    /// Check if the free list page is full
    pub fn is_full(&self, page_data_size: usize) -> bool {
        self.free_pages.len() >= Self::max_entries(page_data_size)
    }

    /// Add a free page ID to this list
    pub fn add_page(&mut self, page_id: PageId, page_data_size: usize) -> PagerResult<()> {
        if self.is_full(page_data_size) {
            return Err(PagerError::InternalError(
                "Free list page is full".to_string(),
            ));
        }
        self.free_pages.push(page_id);
        Ok(())
    }

    /// Remove and return a free page ID from this list
    pub fn pop_page(&mut self) -> Option<PageId> {
        self.free_pages.pop()
    }

    /// Check if the free list page is empty
    pub fn is_empty(&self) -> bool {
        self.free_pages.is_empty()
    }

    /// Get the number of free pages in this list
    pub fn len(&self) -> usize {
        self.free_pages.len()
    }

    /// Serialize the free list page to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Next page ID
        bytes.extend_from_slice(&self.next_page.to_bytes());

        // Number of entries
        bytes.extend_from_slice(&(self.free_pages.len() as u64).to_le_bytes());

        // Free page IDs
        for page_id in &self.free_pages {
            bytes.extend_from_slice(&page_id.to_bytes());
        }

        bytes
    }

    /// Deserialize the free list page from bytes
    pub fn from_bytes(bytes: &[u8]) -> PagerResult<Self> {
        if bytes.len() < Self::HEADER_SIZE {
            return Err(PagerError::InternalError(
                "Insufficient bytes for free list page".to_string(),
            ));
        }

        let next_page = PageId::from(u64::from_le_bytes(bytes[0..8].try_into().unwrap()));
        let count = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;

        let mut free_pages = Vec::with_capacity(count);
        let mut offset = Self::HEADER_SIZE;

        for _ in 0..count {
            if offset + 8 > bytes.len() {
                return Err(PagerError::InternalError(
                    "Insufficient bytes for free page entries".to_string(),
                ));
            }
            let page_id = PageId::from(u64::from_le_bytes(
                bytes[offset..offset + 8].try_into().unwrap(),
            ));
            free_pages.push(page_id);
            offset += 8;
        }

        Ok(Self {
            next_page,
            free_pages,
        })
    }
}

impl Default for FreeListPage {
    fn default() -> Self {
        Self::new()
    }
}

/// Free list manager
///
/// Manages the chain of free list pages and provides
/// efficient allocation and deallocation of pages.
///
/// Uses lock-free data structures for wait-free concurrent access:
/// - SegQueue for the free page stack (lock-free MPMC queue)
/// - AtomicU64 for the total free counter
pub struct FreeList {
    /// First free list page ID persisted in metadata (0 if none)
    first_page: AtomicU64,
    /// Last free list page ID persisted in metadata (0 if none)
    last_page: AtomicU64,
    /// Total number of free pages available for reuse (atomic counter)
    total_free: AtomicU64,
    /// Lock-free queue of reusable page IDs
    free_pages: SegQueue<PageId>,
}

impl FreeList {
    /// Create a new empty free list
    pub fn new() -> Self {
        Self {
            first_page: AtomicU64::new(0),
            last_page: AtomicU64::new(0),
            total_free: AtomicU64::new(0),
            free_pages: SegQueue::new(),
        }
    }

    /// Create a free list from existing state
    pub fn from_state(first_page: PageId, last_page: PageId, total_free: u64) -> Self {
        Self {
            first_page: AtomicU64::new(first_page.as_u64()),
            last_page: AtomicU64::new(last_page.as_u64()),
            total_free: AtomicU64::new(total_free),
            free_pages: SegQueue::new(),
        }
    }

    /// Get the first free list page ID
    pub fn first_page(&self) -> PageId {
        PageId::from(self.first_page.load(Ordering::Acquire))
    }

    /// Get the last free list page ID
    pub fn last_page(&self) -> PageId {
        PageId::from(self.last_page.load(Ordering::Acquire))
    }

    /// Get the total number of free pages
    pub fn total_free(&self) -> u64 {
        self.total_free.load(Ordering::Acquire)
    }

    /// Check if the free list is empty
    pub fn is_empty(&self) -> bool {
        self.total_free() == 0
    }

    /// Update the first page ID
    pub fn set_first_page(&self, page_id: PageId) {
        self.first_page.store(page_id.as_u64(), Ordering::Release);
        if self.last_page.load(Ordering::Acquire) == 0 {
            self.last_page.store(page_id.as_u64(), Ordering::Release);
        }
    }

    /// Update the last page ID
    pub fn set_last_page(&self, page_id: PageId) {
        self.last_page.store(page_id.as_u64(), Ordering::Release);
        if self.first_page.load(Ordering::Acquire) == 0 {
            self.first_page.store(page_id.as_u64(), Ordering::Release);
        }
    }

    /// Push a page ID onto the reusable stack (lock-free)
    pub fn push_page(&self, page_id: PageId) {
        self.free_pages.push(page_id);
        let new_total = self.total_free.fetch_add(1, Ordering::AcqRel) + 1;
        counter!("nanostore.pager.freelist.push").increment(1);
        gauge!("nanostore.pager.freelist.size").set(new_total as f64);
    }

    /// Pop a page ID from the reusable stack (lock-free)
    pub fn pop_page(&self) -> Option<PageId> {
        let page_id = self.free_pages.pop();
        if page_id.is_some() {
            let prev = self.total_free.fetch_sub(1, Ordering::AcqRel);
            counter!("nanostore.pager.freelist.pop").increment(1);
            gauge!("nanostore.pager.freelist.size").set((prev - 1) as f64);
            if prev == 1 {
                // Was the last page
                self.first_page.store(0, Ordering::Release);
                self.last_page.store(0, Ordering::Release);
            }
        }
        page_id
    }

    /// Replace the in-memory free page stack from persistent state
    pub fn set_free_pages(&self, mut free_pages: Vec<PageId>) {
        free_pages.sort_unstable();
        let count = free_pages.len() as u64;

        // Clear existing queue
        while self.free_pages.pop().is_some() {}

        // Push all pages
        for page_id in free_pages {
            self.free_pages.push(page_id);
        }

        self.total_free.store(count, Ordering::Release);
        if count == 0 {
            self.first_page.store(0, Ordering::Release);
            self.last_page.store(0, Ordering::Release);
        }
    }

    /// Increment the free page count
    pub fn increment_free(&self) {
        self.total_free.fetch_add(1, Ordering::AcqRel);
    }

    /// Decrement the free page count
    pub fn decrement_free(&self) {
        let prev = self.total_free.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            // Was the last page
            self.first_page.store(0, Ordering::Release);
            self.last_page.store(0, Ordering::Release);
        }
    }
}

impl Default for FreeList {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_free_list_page_creation() {
        let page = FreeListPage::new();
        assert_eq!(page.next_page, PageId::from(0));
        assert!(page.is_empty());
        assert_eq!(page.len(), 0);
    }

    #[test]
    fn test_free_list_page_add_remove() {
        let mut page = FreeListPage::new();
        let page_data_size = 4000;

        page.add_page(PageId::from(10), page_data_size).unwrap();
        page.add_page(PageId::from(20), page_data_size).unwrap();
        page.add_page(PageId::from(30), page_data_size).unwrap();

        assert_eq!(page.len(), 3);
        assert_eq!(page.pop_page(), Some(PageId::from(30)));
        assert_eq!(page.pop_page(), Some(PageId::from(20)));
        assert_eq!(page.pop_page(), Some(PageId::from(10)));
        assert_eq!(page.pop_page(), None);
        assert!(page.is_empty());
    }

    #[test]
    fn test_free_list_page_serialization() {
        let mut page = FreeListPage::new();
        page.next_page = PageId::from(42);
        page.add_page(PageId::from(100), 4000).unwrap();
        page.add_page(PageId::from(200), 4000).unwrap();

        let bytes = page.to_bytes();
        let deserialized = FreeListPage::from_bytes(&bytes).unwrap();

        assert_eq!(deserialized.next_page, PageId::from(42));
        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized.free_pages[0], PageId::from(100));
        assert_eq!(deserialized.free_pages[1], PageId::from(200));
    }

    #[test]
    fn test_free_list_page_max_entries() {
        let page_data_size = 4000;
        let max = FreeListPage::max_entries(page_data_size);

        // (4000 - 16) / 8 = 498
        assert_eq!(max, 498);
    }

    #[test]
    fn test_free_list_manager() {
        let free_list = FreeList::new();
        assert!(free_list.is_empty());
        assert_eq!(free_list.total_free(), 0);

        free_list.set_first_page(PageId::from(10));
        free_list.increment_free();

        assert_eq!(free_list.first_page(), PageId::from(10));
        assert_eq!(free_list.total_free(), 1);
        assert!(!free_list.is_empty());

        free_list.decrement_free();
        assert!(free_list.is_empty());
        assert_eq!(free_list.first_page(), PageId::from(0));
    }
}
