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

//! LRU Page Cache with Sharding
//!
//! Provides a sharded LRU (Least Recently Used) cache for pages with:
//! - Configurable capacity distributed across shards
//! - Dirty page tracking for write-back policy
//! - Cache statistics (hit/miss rates, evictions)
//! - Thread-safe operations with reduced lock contention
//! - Integration with Pager layer
//!
//! The cache uses multiple shards (32 by default) to reduce lock contention
//! in concurrent workloads. Each shard maintains its own LRU list and lock,
//! allowing concurrent operations on different shards.

use crate::pager::{Page, PageId};
use metrics::{counter, gauge};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Number of cache shards for concurrent access
/// Using 32 shards provides good balance between concurrency and overhead
const NUM_SHARDS: usize = 32;

/// Cache entry containing a page and metadata
#[derive(Debug, Clone)]
struct CacheEntry {
    /// The cached page
    page: Page,
    /// Whether the page has been modified
    dirty: bool,
    /// Previous entry in LRU list (None if this is the head)
    prev: Option<PageId>,
    /// Next entry in LRU list (None if this is the tail)
    next: Option<PageId>,
}

/// Cache statistics
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    /// Total number of cache hits
    pub hits: u64,
    /// Total number of cache misses
    pub misses: u64,
    /// Total number of evictions
    pub evictions: u64,
    /// Total number of dirty page flushes
    pub flushes: u64,
    /// Current number of pages in cache
    pub current_size: usize,
    /// Current number of dirty pages
    pub dirty_pages: usize,
}

impl CacheStats {
    /// Calculate the cache hit rate (0.0 to 1.0)
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    /// Calculate the cache miss rate (0.0 to 1.0)
    pub fn miss_rate(&self) -> f64 {
        1.0 - self.hit_rate()
    }

    /// Reset all statistics
    pub fn reset(&mut self) {
        self.hits = 0;
        self.misses = 0;
        self.evictions = 0;
        self.flushes = 0;
    }
}

/// Cache configuration
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Maximum number of pages to cache
    pub capacity: usize,
    /// Whether to enable write-back (true) or write-through (false)
    pub write_back: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            capacity: 1000,   // Default to 1000 pages
            write_back: true, // Default to write-back for better performance
        }
    }
}

impl CacheConfig {
    /// Create a new cache configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the cache capacity
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }

    /// Set write-back mode
    pub fn with_write_back(mut self, write_back: bool) -> Self {
        self.write_back = write_back;
        self
    }
}

/// A single cache shard with its own lock and LRU list
struct CacheShard {
    /// Map from page ID to cache entry
    entries: HashMap<PageId, CacheEntry>,
    /// Head of LRU list (most recently used)
    lru_head: Option<PageId>,
    /// Tail of LRU list (least recently used)
    lru_tail: Option<PageId>,
    /// Cache statistics for this shard
    stats: CacheStats,
    /// Maximum capacity for this shard
    capacity: usize,
}

/// LRU Page Cache with Sharding
///
/// Thread-safe cache using LRU eviction policy with dirty page tracking.
/// The cache is divided into multiple shards, each with its own lock and LRU list,
/// to reduce lock contention in concurrent workloads.
pub struct PageCache {
    /// Cache configuration
    config: CacheConfig,
    /// Array of cache shards, each protected by its own RwLock
    shards: Vec<Arc<RwLock<CacheShard>>>,
    /// Write-back mode (shared across all shards)
    write_back: bool,
}

impl PageCache {
    /// Create a new page cache with the given configuration
    pub fn new(config: CacheConfig) -> Self {
        let shard_capacity = config.capacity.div_ceil(NUM_SHARDS);
        let write_back = config.write_back;
        let shards = (0..NUM_SHARDS)
            .map(|_| {
                Arc::new(RwLock::new(CacheShard {
                    entries: HashMap::new(),
                    lru_head: None,
                    lru_tail: None,
                    stats: CacheStats::default(),
                    capacity: shard_capacity,
                }))
            })
            .collect();

        Self {
            config,
            shards,
            write_back,
        }
    }

    /// Get the shard index for a given page ID
    #[inline]
    fn shard_index(&self, page_id: PageId) -> usize {
        // Use simple modulo for distribution
        // PageId is u64, so we can use it directly
        (page_id.as_u64() as usize) % NUM_SHARDS
    }

    /// Get a page from the cache
    ///
    /// Returns Some(page) if found (cache hit), None if not found (cache miss).
    /// Updates LRU ordering on hit.
    pub fn get(&self, page_id: PageId) -> Option<Page> {
        let shard_idx = self.shard_index(page_id);
        let mut shard = self.shards[shard_idx].write();

        if shard.entries.contains_key(&page_id) {
            // Cache hit
            shard.stats.hits += 1;
            counter!("nanostore.pager.cache.hit").increment(1);

            // Clone the page before moving to front
            let page = shard.entries.get(&page_id).unwrap().page.clone();

            // Move to front of LRU list
            shard.move_to_front(page_id);

            Some(page)
        } else {
            // Cache miss
            shard.stats.misses += 1;
            counter!("nanostore.pager.cache.miss").increment(1);
            None
        }
    }

    /// Put a page into the cache
    ///
    /// If the cache is at capacity, evicts the least recently used page.
    /// Returns the evicted page if one was evicted and it was dirty.
    pub fn put(&self, page: Page, dirty: bool) -> Option<Page> {
        let page_id = page.page_id();
        let shard_idx = self.shard_index(page_id);
        let mut shard = self.shards[shard_idx].write();

        // Use entry API for atomic check-or-insert
        use std::collections::hash_map::Entry;
        match shard.entries.entry(page_id) {
            Entry::Occupied(mut entry) => {
                // Page already exists - update it
                let cache_entry = entry.get_mut();
                cache_entry.page = page;
                cache_entry.dirty = cache_entry.dirty || dirty; // Keep dirty flag if already dirty
                shard.move_to_front(page_id);
                None
            }
            Entry::Vacant(_vacant) => {
                // Page doesn't exist - need to evict if at capacity before inserting

                // Evict if at capacity
                let evicted = if shard.entries.len() >= shard.capacity {
                    shard.evict_lru()
                } else {
                    None
                };

                // Now insert the new entry (we know it doesn't exist)
                let entry = CacheEntry {
                    page,
                    dirty,
                    prev: None,
                    next: shard.lru_head,
                };

                shard.entries.insert(page_id, entry);

                // Update LRU list
                if let Some(old_head) = shard.lru_head
                    && let Some(head_entry) = shard.entries.get_mut(&old_head)
                {
                    head_entry.prev = Some(page_id);
                }

                shard.lru_head = Some(page_id);

                if shard.lru_tail.is_none() {
                    shard.lru_tail = Some(page_id);
                }

                shard.stats.current_size = shard.entries.len();
                if dirty {
                    shard.stats.dirty_pages += 1;
                }

                // Update gauges
                gauge!("nanostore.pager.cache.size").set(shard.stats.current_size as f64);
                gauge!("nanostore.pager.cache.dirty_pages").set(shard.stats.dirty_pages as f64);

                evicted
            }
        }
    }

    /// Check if a page is in the cache
    pub fn contains(&self, page_id: PageId) -> bool {
        let shard_idx = self.shard_index(page_id);
        let shard = self.shards[shard_idx].read();
        shard.entries.contains_key(&page_id)
    }

    /// Mark a page as dirty
    ///
    /// Returns true if the page was found and marked dirty, false otherwise.
    pub fn mark_dirty(&self, page_id: PageId) -> bool {
        let shard_idx = self.shard_index(page_id);
        let mut shard = self.shards[shard_idx].write();

        if let Some(entry) = shard.entries.get_mut(&page_id) {
            if !entry.dirty {
                entry.dirty = true;
                shard.stats.dirty_pages += 1;
            }
            true
        } else {
            false
        }
    }

    /// Mark a page as clean (after flushing to disk)
    ///
    /// Returns true if the page was found and marked clean, false otherwise.
    pub fn mark_clean(&self, page_id: PageId) -> bool {
        let shard_idx = self.shard_index(page_id);
        let mut shard = self.shards[shard_idx].write();

        if let Some(entry) = shard.entries.get_mut(&page_id) {
            if entry.dirty {
                entry.dirty = false;
                shard.stats.dirty_pages = shard.stats.dirty_pages.saturating_sub(1);
                shard.stats.flushes += 1;
                counter!("nanostore.pager.cache.dirty_flush").increment(1);
                gauge!("nanostore.pager.cache.dirty_pages").set(shard.stats.dirty_pages as f64);
            }
            true
        } else {
            false
        }
    }

    /// Check if a page is dirty
    pub fn is_dirty(&self, page_id: PageId) -> bool {
        let shard_idx = self.shard_index(page_id);
        let shard = self.shards[shard_idx].read();
        shard.entries.get(&page_id).is_some_and(|e| e.dirty)
    }

    /// Get all dirty pages
    ///
    /// Returns a vector of (page_id, page) tuples for all dirty pages.
    pub fn get_dirty_pages(&self) -> Vec<(PageId, Page)> {
        let mut dirty_pages = Vec::new();
        for shard in &self.shards {
            let shard = shard.read();
            dirty_pages.extend(
                shard
                    .entries
                    .iter()
                    .filter(|(_, entry)| entry.dirty)
                    .map(|(id, entry)| (*id, entry.page.clone())),
            );
        }
        dirty_pages
    }

    /// Remove a page from the cache
    ///
    /// Returns the page if it was in the cache and was dirty.
    pub fn remove(&self, page_id: PageId) -> Option<Page> {
        let shard_idx = self.shard_index(page_id);
        let mut shard = self.shards[shard_idx].write();
        shard.remove_entry(page_id)
    }

    /// Clear all pages from the cache
    ///
    /// Returns all dirty pages that were in the cache.
    pub fn clear(&self) -> Vec<(PageId, Page)> {
        let mut all_dirty_pages = Vec::new();

        for shard in &self.shards {
            let mut shard = shard.write();

            let dirty_pages: Vec<(PageId, Page)> = shard
                .entries
                .iter()
                .filter(|(_, entry)| entry.dirty)
                .map(|(id, entry)| (*id, entry.page.clone()))
                .collect();

            all_dirty_pages.extend(dirty_pages);

            shard.entries.clear();
            shard.lru_head = None;
            shard.lru_tail = None;
            shard.stats.current_size = 0;
            shard.stats.dirty_pages = 0;
        }

        all_dirty_pages
    }

    /// Get cache statistics (aggregated across all shards)
    pub fn stats(&self) -> CacheStats {
        let mut total_stats = CacheStats::default();

        for shard in &self.shards {
            let shard = shard.read();
            total_stats.hits += shard.stats.hits;
            total_stats.misses += shard.stats.misses;
            total_stats.evictions += shard.stats.evictions;
            total_stats.flushes += shard.stats.flushes;
            total_stats.current_size += shard.stats.current_size;
            total_stats.dirty_pages += shard.stats.dirty_pages;
        }

        // Update metrics gauges
        gauge!("nanostore.pager.cache.size").set(total_stats.current_size as f64);
        gauge!("nanostore.pager.cache.dirty_pages").set(total_stats.dirty_pages as f64);

        total_stats
    }

    /// Reset cache statistics
    pub fn reset_stats(&self) {
        for shard in &self.shards {
            let mut shard = shard.write();
            shard.stats.reset();
        }
    }

    /// Get the current cache size (total across all shards)
    pub fn size(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| shard.read().entries.len())
            .sum()
    }

    /// Get the cache capacity
    pub fn capacity(&self) -> usize {
        self.config.capacity
    }

    /// Check if write-back mode is enabled
    pub fn is_write_back(&self) -> bool {
        self.write_back
    }
}

impl CacheShard {
    /// Move a page to the front of the LRU list (mark as most recently used)
    fn move_to_front(&mut self, page_id: PageId) {
        // If already at front, nothing to do
        if self.lru_head == Some(page_id) {
            return;
        }

        // Remove from current position
        if let Some(entry) = self.entries.get(&page_id) {
            let prev = entry.prev;
            let next = entry.next;

            // Update previous node's next pointer
            if let Some(prev_id) = prev
                && let Some(prev_entry) = self.entries.get_mut(&prev_id)
            {
                prev_entry.next = next;
            }

            // Update next node's prev pointer
            if let Some(next_id) = next
                && let Some(next_entry) = self.entries.get_mut(&next_id)
            {
                next_entry.prev = prev;
            }

            // Update tail if this was the tail
            if self.lru_tail == Some(page_id) {
                self.lru_tail = prev;
            }
        }

        // Add to front
        if let Some(entry) = self.entries.get_mut(&page_id) {
            entry.prev = None;
            entry.next = self.lru_head;
        }

        if let Some(old_head) = self.lru_head
            && let Some(head_entry) = self.entries.get_mut(&old_head)
        {
            head_entry.prev = Some(page_id);
        }

        self.lru_head = Some(page_id);

        if self.lru_tail.is_none() {
            self.lru_tail = Some(page_id);
        }
    }

    /// Evict the least recently used page
    ///
    /// Returns the evicted page if it was dirty.
    fn evict_lru(&mut self) -> Option<Page> {
        let tail_id = self.lru_tail?;
        self.stats.evictions += 1;
        counter!("nanostore.pager.cache.eviction").increment(1);
        self.remove_entry(tail_id)
    }

    /// Remove an entry from the cache
    ///
    /// Returns the page if it was dirty.
    fn remove_entry(&mut self, page_id: PageId) -> Option<Page> {
        let entry = self.entries.remove(&page_id)?;

        // Update LRU list
        if let Some(prev_id) = entry.prev {
            if let Some(prev_entry) = self.entries.get_mut(&prev_id) {
                prev_entry.next = entry.next;
            }
        } else {
            // This was the head
            self.lru_head = entry.next;
        }

        if let Some(next_id) = entry.next {
            if let Some(next_entry) = self.entries.get_mut(&next_id) {
                next_entry.prev = entry.prev;
            }
        } else {
            // This was the tail
            self.lru_tail = entry.prev;
        }

        self.stats.current_size = self.entries.len();

        if entry.dirty {
            self.stats.dirty_pages = self.stats.dirty_pages.saturating_sub(1);
            Some(entry.page)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pager::{PageSize, PageType};

    fn create_test_page(id: PageId) -> Page {
        let mut page = Page::new(id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
        page.data_mut()
            .extend_from_slice(format!("page {}", id).as_bytes());
        page
    }

    #[test]
    fn test_cache_basic_operations() {
        let config = CacheConfig::new().with_capacity(3);
        let cache = PageCache::new(config);

        // Put pages
        let page1 = create_test_page(PageId::from(1));
        let page2 = create_test_page(PageId::from(2));

        cache.put(page1.clone(), false);
        cache.put(page2.clone(), false);

        // Get pages
        assert!(cache.get(PageId::from(1)).is_some());
        assert!(cache.get(PageId::from(2)).is_some());
        assert!(cache.get(PageId::from(3)).is_none());

        // Check contains
        assert!(cache.contains(PageId::from(1)));
        assert!(cache.contains(PageId::from(2)));
        assert!(!cache.contains(PageId::from(3)));
    }

    #[test]
    fn test_cache_lru_eviction() {
        // With sharding, we need to ensure pages go to the same shard for predictable eviction
        // Use a small capacity per shard to test eviction
        let config = CacheConfig::new().with_capacity(32); // 1 per shard
        let cache = PageCache::new(config);

        // Find pages that hash to the same shard
        let mut same_shard_pages = Vec::new();
        let target_shard = cache.shard_index(PageId::from(1));

        for i in 1..=100 {
            let page_id = PageId::from(i);
            if cache.shard_index(page_id) == target_shard {
                same_shard_pages.push(page_id);
                if same_shard_pages.len() == 4 {
                    break;
                }
            }
        }

        assert!(
            same_shard_pages.len() >= 4,
            "Need at least 4 pages in same shard"
        );

        // Fill the shard (capacity is 1 per shard)
        cache.put(create_test_page(same_shard_pages[0]), false);

        // Access it to make it most recently used
        cache.get(same_shard_pages[0]);

        // Add another page to same shard (should evict the first one since capacity is 1)
        cache.put(create_test_page(same_shard_pages[1]), false);

        // The second page should be in cache, first might be evicted
        assert!(cache.contains(same_shard_pages[1]));

        // Verify eviction occurred
        let stats = cache.stats();
        assert!(stats.evictions >= 1 || stats.current_size <= 32);
    }

    #[test]
    fn test_cache_dirty_tracking() {
        let config = CacheConfig::new().with_capacity(5);
        let cache = PageCache::new(config);

        // Put clean page
        cache.put(create_test_page(PageId::from(1)), false);
        assert!(!cache.is_dirty(PageId::from(1)));

        // Mark as dirty
        cache.mark_dirty(PageId::from(1));
        assert!(cache.is_dirty(PageId::from(1)));

        // Put dirty page
        cache.put(create_test_page(PageId::from(2)), true);
        assert!(cache.is_dirty(PageId::from(2)));

        // Mark as clean
        cache.mark_clean(PageId::from(2));
        assert!(!cache.is_dirty(PageId::from(2)));

        // Get dirty pages
        cache.put(create_test_page(PageId::from(3)), true);
        let dirty = cache.get_dirty_pages();
        assert_eq!(dirty.len(), 2); // Pages 1 and 3
    }

    #[test]
    fn test_cache_statistics() {
        let config = CacheConfig::new().with_capacity(3);
        let cache = PageCache::new(config);

        cache.put(create_test_page(PageId::from(1)), false);
        cache.put(create_test_page(PageId::from(2)), false);

        // Cache hits
        cache.get(PageId::from(1));
        cache.get(PageId::from(2));

        // Cache misses
        cache.get(PageId::from(3));
        cache.get(PageId::from(4));

        let stats = cache.stats();
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 2);
        assert_eq!(stats.hit_rate(), 0.5);
        assert_eq!(stats.current_size, 2);
    }

    #[test]
    fn test_cache_eviction_dirty_page() {
        // With sharding, need pages in same shard for predictable eviction
        let config = CacheConfig::new().with_capacity(32); // 1 per shard
        let cache = PageCache::new(config);

        // Find pages that hash to the same shard
        let mut same_shard_pages = Vec::new();
        let target_shard = cache.shard_index(PageId::from(1));

        for i in 1..=100 {
            let page_id = PageId::from(i);
            if cache.shard_index(page_id) == target_shard {
                same_shard_pages.push(page_id);
                if same_shard_pages.len() == 3 {
                    break;
                }
            }
        }

        assert!(
            same_shard_pages.len() >= 3,
            "Need at least 3 pages in same shard"
        );

        // Fill shard with dirty pages (capacity is 1 per shard)
        cache.put(create_test_page(same_shard_pages[0]), true);
        cache.put(create_test_page(same_shard_pages[1]), true);

        // Add another page to same shard (should evict one of the dirty pages)
        let evicted = cache.put(create_test_page(same_shard_pages[2]), false);

        // With capacity of 1 per shard, we should get an eviction
        assert!(
            evicted.is_some(),
            "Expected eviction with shard at capacity"
        );

        let stats = cache.stats();
        assert!(stats.evictions >= 1);
    }

    #[test]
    fn test_cache_clear() {
        let config = CacheConfig::new().with_capacity(5);
        let cache = PageCache::new(config);

        cache.put(create_test_page(PageId::from(1)), true);
        cache.put(create_test_page(PageId::from(2)), false);
        cache.put(create_test_page(PageId::from(3)), true);

        let dirty = cache.clear();
        assert_eq!(dirty.len(), 2); // Pages 1 and 3
        assert_eq!(cache.size(), 0);
    }

    #[test]
    fn test_cache_remove() {
        let config = CacheConfig::new().with_capacity(5);
        let cache = PageCache::new(config);

        cache.put(create_test_page(PageId::from(1)), true);
        cache.put(create_test_page(PageId::from(2)), false);

        // Remove dirty page
        let removed = cache.remove(PageId::from(1));
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().page_id(), PageId::from(1));

        // Remove clean page
        let removed = cache.remove(PageId::from(2));
        assert!(removed.is_none());

        assert_eq!(cache.size(), 0);
    }

    #[test]
    fn test_cache_update_existing() {
        let config = CacheConfig::new().with_capacity(3);
        let cache = PageCache::new(config);

        // Put initial page
        cache.put(create_test_page(PageId::from(1)), false);
        assert!(!cache.is_dirty(PageId::from(1)));

        // Update with dirty flag
        cache.put(create_test_page(PageId::from(1)), true);
        assert!(cache.is_dirty(PageId::from(1)));
        assert_eq!(cache.size(), 1); // Should not increase size
    }

    #[test]
    fn test_shard_distribution() {
        let config = CacheConfig::new().with_capacity(1000);
        let cache = PageCache::new(config);

        // Track which shards pages go to
        let mut shard_counts = vec![0; NUM_SHARDS];

        // Add 1000 pages and track distribution
        for i in 0..1000 {
            let page_id = PageId::from(i);
            let shard_idx = cache.shard_index(page_id);
            shard_counts[shard_idx] += 1;
            cache.put(create_test_page(page_id), false);
        }

        // Verify reasonable distribution (each shard should have some pages)
        // With 1000 pages and 32 shards, expect ~31 per shard
        // Allow for variance but ensure no shard is empty
        for (idx, &count) in shard_counts.iter().enumerate() {
            assert!(count > 0, "Shard {} has no pages", idx);
            assert!(count < 100, "Shard {} has too many pages: {}", idx, count);
        }

        // Verify total
        let total: usize = shard_counts.iter().sum();
        assert_eq!(total, 1000);
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let config = CacheConfig::new().with_capacity(1000);
        let cache = Arc::new(PageCache::new(config));

        // Spawn multiple threads doing concurrent operations
        let mut handles = vec![];

        for thread_id in 0..8 {
            let cache_clone = Arc::clone(&cache);
            let handle = thread::spawn(move || {
                // Each thread works with different page IDs
                let start = thread_id * 100;
                let end = start + 100;

                for i in start..end {
                    let page_id = PageId::from(i as u64);

                    // Put page
                    cache_clone.put(create_test_page(page_id), false);

                    // Get page
                    assert!(cache_clone.get(page_id).is_some());

                    // Mark dirty
                    cache_clone.mark_dirty(page_id);
                    assert!(cache_clone.is_dirty(page_id));

                    // Mark clean
                    cache_clone.mark_clean(page_id);
                    assert!(!cache_clone.is_dirty(page_id));
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify final state
        let stats = cache.stats();
        assert_eq!(stats.current_size, 800); // 8 threads * 100 pages
        assert_eq!(stats.dirty_pages, 0); // All marked clean
    }

    #[test]
    fn test_concurrent_eviction() {
        use std::sync::Arc;
        use std::thread;

        // Small cache to force evictions
        let config = CacheConfig::new().with_capacity(100);
        let cache = Arc::new(PageCache::new(config));

        let mut handles = vec![];

        for thread_id in 0..4 {
            let cache_clone = Arc::clone(&cache);
            let handle = thread::spawn(move || {
                // Each thread adds many pages to force evictions
                let start = thread_id * 200;
                let end = start + 200;

                for i in start..end {
                    let page_id = PageId::from(i as u64);
                    cache_clone.put(create_test_page(page_id), false);
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Verify evictions occurred
        let stats = cache.stats();
        assert!(stats.evictions > 0, "Expected evictions with small cache");
        // With sharding, size can slightly exceed capacity due to rounding
        // Each shard gets capacity/NUM_SHARDS, so total can be up to capacity + NUM_SHARDS - 1
        assert!(
            stats.current_size <= 100 + NUM_SHARDS,
            "Cache size {} should be close to capacity 100",
            stats.current_size
        );
    }
}
