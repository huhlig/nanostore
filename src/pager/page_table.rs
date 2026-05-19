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

//! Page-level locking mechanism for fine-grained concurrency control
//!
//! This module provides a sharded locking mechanism that allows concurrent
//! access to different pages while preventing conflicts on the same page.
//! By using multiple lock shards, we reduce contention compared to a single
//! global lock.

use crate::pager::PageId;
use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Page table with sharded locks for fine-grained concurrency
///
/// The page table maintains a fixed number of lock shards. Each page is
/// mapped to a shard using a hash function. This allows multiple pages
/// to be accessed concurrently as long as they map to different shards.
///
/// # Design Rationale
///
/// - **Sharding**: Reduces lock contention by distributing pages across multiple locks
/// - **Power-of-2 shards**: Enables fast modulo operation using bitwise AND
/// - **Read/Write locks**: Allows multiple concurrent readers per shard
/// - **Simple mapping**: Page ID modulo shard count for even distribution
///
/// # Performance Characteristics
///
/// - Lock acquisition: O(1)
/// - Memory overhead: O(shard_count) locks
/// - Contention reduction: ~shard_count times fewer conflicts
///
/// # Recommended Shard Counts
///
/// - Low concurrency (1-4 threads): 16 shards
/// - Medium concurrency (4-8 threads): 64 shards
/// - High concurrency (8+ threads): 128-256 shards
#[derive(Debug)]
pub struct PageTable {
    /// Array of lock shards, each protecting a subset of pages
    shards: Vec<RwLock<()>>,
    /// Number of shards (always a power of 2)
    shard_count: usize,
    /// Bit mask for fast modulo (shard_count - 1)
    shard_mask: usize,
}

impl PageTable {
    /// Default number of shards for balanced performance
    pub const DEFAULT_SHARD_COUNT: usize = 64;

    /// Create a new page table with the default number of shards
    pub fn new() -> Self {
        Self::with_shard_count(Self::DEFAULT_SHARD_COUNT)
    }

    /// Create a new page table with a specific number of shards
    ///
    /// The shard count will be rounded up to the next power of 2 for
    /// efficient modulo operations using bitwise AND.
    ///
    /// # Arguments
    ///
    /// * `shard_count` - Desired number of shards (will be rounded to power of 2)
    ///
    /// # Examples
    ///
    /// ```
    /// use nanostore::pager::PageTable;
    ///
    /// // Create with 64 shards (already power of 2)
    /// let table = PageTable::with_shard_count(64);
    ///
    /// // Create with 100 shards (rounded up to 128)
    /// let table = PageTable::with_shard_count(100);
    /// ```
    pub fn with_shard_count(shard_count: usize) -> Self {
        // Ensure at least 1 shard
        let shard_count = shard_count.max(1);

        // Round up to next power of 2
        let shard_count = shard_count.next_power_of_two();

        // Create the shards
        let shards = (0..shard_count).map(|_| RwLock::new(())).collect();

        Self {
            shards,
            shard_count,
            shard_mask: shard_count - 1,
        }
    }

    /// Get the shard index for a given page ID
    ///
    /// Uses bitwise AND with the shard mask for fast modulo operation.
    /// This ensures even distribution of pages across shards.
    #[inline]
    fn shard_index(&self, page_id: PageId) -> usize {
        (page_id.as_u64() as usize) & self.shard_mask
    }

    /// Acquire a read lock for a page
    ///
    /// Multiple threads can hold read locks for pages in the same shard
    /// simultaneously, as long as no thread holds a write lock.
    ///
    /// # Arguments
    ///
    /// * `page_id` - The page to lock for reading
    ///
    /// # Returns
    ///
    /// A read guard that automatically releases the lock when dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// use nanostore::pager::{PageTable, PageId};
    ///
    /// let table = PageTable::new();
    /// let page_id = PageId::from(42);
    ///
    /// {
    ///     let _guard = table.read_lock(page_id);
    ///     // Page is locked for reading
    ///     // Multiple threads can read concurrently
    /// }
    /// // Lock automatically released
    /// ```
    #[inline]
    pub fn read_lock(&self, page_id: PageId) -> RwLockReadGuard<'_, ()> {
        self.shards[self.shard_index(page_id)].read()
    }

    /// Acquire a write lock for a page
    ///
    /// Only one thread can hold a write lock for pages in a shard at a time.
    /// Write locks are exclusive with both read and write locks.
    ///
    /// # Arguments
    ///
    /// * `page_id` - The page to lock for writing
    ///
    /// # Returns
    ///
    /// A write guard that automatically releases the lock when dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// use nanostore::pager::{PageTable, PageId};
    ///
    /// let table = PageTable::new();
    /// let page_id = PageId::from(42);
    ///
    /// {
    ///     let _guard = table.write_lock(page_id);
    ///     // Page is locked for writing
    ///     // No other thread can read or write
    /// }
    /// // Lock automatically released
    /// ```
    #[inline]
    pub fn write_lock(&self, page_id: PageId) -> RwLockWriteGuard<'_, ()> {
        self.shards[self.shard_index(page_id)].write()
    }

    /// Try to acquire a read lock for a page without blocking
    ///
    /// Returns `Some(guard)` if the lock was acquired, `None` if it would block.
    ///
    /// # Arguments
    ///
    /// * `page_id` - The page to try locking for reading
    #[inline]
    pub fn try_read_lock(&self, page_id: PageId) -> Option<RwLockReadGuard<'_, ()>> {
        self.shards[self.shard_index(page_id)].try_read()
    }

    /// Try to acquire a write lock for a page without blocking
    ///
    /// Returns `Some(guard)` if the lock was acquired, `None` if it would block.
    ///
    /// # Arguments
    ///
    /// * `page_id` - The page to try locking for writing
    #[inline]
    pub fn try_write_lock(&self, page_id: PageId) -> Option<RwLockWriteGuard<'_, ()>> {
        self.shards[self.shard_index(page_id)].try_write()
    }

    /// Get the number of shards in this page table
    pub fn shard_count(&self) -> usize {
        self.shard_count
    }

    /// Check if two pages would map to the same shard
    ///
    /// Useful for understanding potential lock contention.
    pub fn same_shard(&self, page_id1: PageId, page_id2: PageId) -> bool {
        self.shard_index(page_id1) == self.shard_index(page_id2)
    }
}

impl Default for PageTable {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for PageTable {
    fn clone(&self) -> Self {
        // Create a new page table with the same shard count
        Self::with_shard_count(self.shard_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_page_table_creation() {
        let table = PageTable::new();
        assert_eq!(table.shard_count(), PageTable::DEFAULT_SHARD_COUNT);
    }

    #[test]
    fn test_page_table_custom_shard_count() {
        // Test power of 2
        let table = PageTable::with_shard_count(64);
        assert_eq!(table.shard_count(), 64);

        // Test rounding up to power of 2
        let table = PageTable::with_shard_count(100);
        assert_eq!(table.shard_count(), 128);

        // Test minimum
        let table = PageTable::with_shard_count(0);
        assert_eq!(table.shard_count(), 1);
    }

    #[test]
    fn test_shard_distribution() {
        let table = PageTable::with_shard_count(16);
        let mut shard_counts = vec![0; 16];

        // Test that pages distribute evenly across shards
        for i in 0..1000 {
            let page_id = PageId::from(i);
            let shard = table.shard_index(page_id);
            shard_counts[shard] += 1;
        }

        // Each shard should have roughly 1000/16 = 62.5 pages
        // Allow some variance (40-80 pages per shard)
        for count in shard_counts {
            assert!((40..=80).contains(&count), "Uneven distribution: {}", count);
        }
    }

    #[test]
    fn test_read_lock() {
        let table = PageTable::new();
        let page_id = PageId::from(42);

        let _guard = table.read_lock(page_id);
        // Lock acquired successfully
    }

    #[test]
    fn test_write_lock() {
        let table = PageTable::new();
        let page_id = PageId::from(42);

        let _guard = table.write_lock(page_id);
        // Lock acquired successfully
    }

    #[test]
    fn test_concurrent_reads() {
        let table = Arc::new(PageTable::new());
        let page_id = PageId::from(42);
        let mut handles = vec![];

        // Multiple threads can read concurrently
        for _ in 0..10 {
            let table_clone = Arc::clone(&table);
            let handle = thread::spawn(move || {
                let _guard = table_clone.read_lock(page_id);
                thread::sleep(std::time::Duration::from_millis(10));
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_concurrent_different_shards() {
        let table = Arc::new(PageTable::with_shard_count(16));
        let mut handles = vec![];

        // Find two pages in different shards
        let page1 = PageId::from(0);
        let mut page2 = PageId::from(1);

        for i in 0..1000 {
            page2 = PageId::from(i);
            if !table.same_shard(page1, page2) {
                break;
            }
        }

        assert!(
            !table.same_shard(page1, page2),
            "Could not find pages in different shards"
        );

        // These should be able to acquire write locks concurrently
        let table_clone = Arc::clone(&table);
        let handle1 = thread::spawn(move || {
            let _guard = table_clone.write_lock(page1);
            thread::sleep(std::time::Duration::from_millis(50));
        });

        let table_clone = Arc::clone(&table);
        let handle2 = thread::spawn(move || {
            let _guard = table_clone.write_lock(page2);
            thread::sleep(std::time::Duration::from_millis(50));
        });

        handles.push(handle1);
        handles.push(handle2);

        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_try_lock() {
        let table = PageTable::new();
        let page_id = PageId::from(42);

        // Acquire write lock
        let _write_guard = table.write_lock(page_id);

        // Try to acquire read lock should fail
        assert!(table.try_read_lock(page_id).is_none());

        // Try to acquire write lock should fail
        assert!(table.try_write_lock(page_id).is_none());
    }

    #[test]
    fn test_same_shard() {
        let table = PageTable::with_shard_count(16);

        // Pages with same modulo should be in same shard
        let page1 = PageId::from(0);
        let page2 = PageId::from(16);
        assert!(table.same_shard(page1, page2));

        // Pages with different modulo should be in different shards
        let page3 = PageId::from(1);
        assert!(!table.same_shard(page1, page3));
    }

    #[test]
    fn test_lock_guard_drop() {
        let table = PageTable::new();
        let page_id = PageId::from(42);

        {
            let _guard = table.write_lock(page_id);
            // Lock is held
        }
        // Lock should be released after guard is dropped

        // Should be able to acquire lock again
        let _guard = table.write_lock(page_id);
    }
}

// Made with Bob
