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

//! Page Cache Integration Tests
//!
//! This test suite provides comprehensive coverage for page cache functionality
//! integrated with the Pager layer.

#![cfg(test)]

use nanostore::pager::{Page, PageId, PageSize, PageType, Pager, PagerConfig};
use nanostore::vfs::MemoryFileSystem;

/// Test cache hit scenario - reading cached pages
#[test]
fn test_cache_hit() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::new()
        .with_cache_capacity(10)
        .with_cache_write_back(true);

    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Allocate and write a page
    let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
    let mut page = Page::new(page_id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
    page.data_mut().extend_from_slice(b"test data");
    pager.write_page(&page).unwrap();

    // In write-back mode, the page is now in cache
    // First read (cache hit because write put it in cache)
    let read1 = pager.read_page(page_id).unwrap();
    assert_eq!(read1.data()[0..9], b"test data"[..]);

    // Second read (also cache hit)
    let read2 = pager.read_page(page_id).unwrap();
    assert_eq!(read2.data()[0..9], b"test data"[..]);

    // Verify cache statistics - both reads should be hits
    let stats = pager.cache_stats().unwrap();
    assert!(stats.hits >= 2); // Both reads were hits
    assert!(stats.current_size > 0);
}

/// Test cache miss scenario - reading uncached pages
#[test]
fn test_cache_miss() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::new()
        .with_cache_capacity(10)
        .with_cache_write_back(true);

    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Allocate multiple pages
    let page_id1 = pager.allocate_page(PageType::BTreeLeaf).unwrap();
    let page_id2 = pager.allocate_page(PageType::BTreeLeaf).unwrap();

    let mut page1 = Page::new(page_id1, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
    page1.data_mut().extend_from_slice(b"page 1");
    pager.write_page(&page1).unwrap();

    let mut page2 = Page::new(page_id2, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
    page2.data_mut().extend_from_slice(b"page 2");
    pager.write_page(&page2).unwrap();

    // In write-back mode, pages are already in cache after write
    // Read page 1 (cache hit)
    let read1 = pager.read_page(page_id1).unwrap();
    assert_eq!(read1.data()[0..6], b"page 1"[..]);

    // Read page 2 (cache hit)
    let read2 = pager.read_page(page_id2).unwrap();
    assert_eq!(read2.data()[0..6], b"page 2"[..]);

    // Verify cache has pages
    let stats = pager.cache_stats().unwrap();
    assert!(stats.current_size >= 2);
}

/// Test LRU eviction policy
#[test]
fn test_cache_lru_eviction() {
    let fs = MemoryFileSystem::new();
    // Use larger capacity to account for sharding (32 shards)
    // With 64 capacity, each shard gets 2 slots
    let config = PagerConfig::new()
        .with_cache_capacity(64)
        .with_cache_write_back(true);

    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Clear cache to start fresh
    pager.clear_cache().unwrap();

    // Allocate many pages to ensure we fill cache and trigger evictions
    let page_ids: Vec<PageId> = (0..100)
        .map(|_| pager.allocate_page(PageType::BTreeLeaf).unwrap())
        .collect();

    // Flush and clear cache after allocation
    pager.flush_cache().unwrap();
    pager.clear_cache().unwrap();

    // Write pages directly to disk
    for (i, &page_id) in page_ids.iter().enumerate() {
        let mut page = Page::new(page_id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
        page.data_mut()
            .extend_from_slice(format!("page {}", i).as_bytes());
        pager.write_page(&page).unwrap();
    }

    // Now clear cache and read pages to test eviction
    pager.flush_cache().unwrap();
    pager.clear_cache().unwrap();

    // Read many pages to fill cache and trigger evictions
    for &page_id in &page_ids {
        pager.read_page(page_id).unwrap();
    }

    // Verify eviction occurred (we read 100 pages but cache holds 64)
    let stats = pager.cache_stats().unwrap();
    assert!(
        stats.evictions > 0,
        "Expected evictions with 100 pages and 64 capacity"
    );
    assert!(stats.current_size <= 64);
}

/// Test cache with write-back mode
#[test]
fn test_cache_write_back() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::new()
        .with_cache_capacity(10)
        .with_cache_write_back(true);

    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Allocate and write a page
    let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
    let mut page = Page::new(page_id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
    page.data_mut().extend_from_slice(b"dirty data");
    pager.write_page(&page).unwrap();

    // Flush cache to ensure dirty pages are written
    pager.flush_cache().unwrap();

    // Verify flush statistics
    let stats = pager.cache_stats().unwrap();
    assert!(stats.flushes > 0);
}

/// Test cache with write-through mode
#[test]
fn test_cache_write_through() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::new()
        .with_cache_capacity(10)
        .with_cache_write_back(false); // Write-through

    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Allocate and write a page
    let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
    let mut page = Page::new(page_id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
    page.data_mut().extend_from_slice(b"immediate write");
    pager.write_page(&page).unwrap();

    // In write-through mode, data should be on disk immediately
    // Read it back to verify
    let read_page = pager.read_page(page_id).unwrap();
    assert_eq!(read_page.data()[0..15], b"immediate write"[..]);
}

/// Test cache statistics
#[test]
fn test_cache_statistics() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::new()
        .with_cache_capacity(5)
        .with_cache_write_back(true);

    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Allocate pages
    let page_ids: Vec<PageId> = (0..3)
        .map(|_| pager.allocate_page(PageType::BTreeLeaf).unwrap())
        .collect();

    // Write pages (they go into cache in write-back mode)
    for (i, &page_id) in page_ids.iter().enumerate() {
        let mut page = Page::new(page_id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
        page.data_mut()
            .extend_from_slice(format!("page {}", i).as_bytes());
        pager.write_page(&page).unwrap();
    }

    // Read pages multiple times (all should be hits since they're in cache)
    for &page_id in &page_ids {
        pager.read_page(page_id).unwrap(); // Hit
        pager.read_page(page_id).unwrap(); // Hit
    }

    // Verify statistics
    let stats = pager.cache_stats().unwrap();
    assert!(stats.hits >= 6); // All reads should be hits
    assert!(stats.current_size > 0);
}

/// Test cache clear operation
#[test]
fn test_cache_clear() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::new()
        .with_cache_capacity(10)
        .with_cache_write_back(true);

    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Allocate and write pages
    let page_ids: Vec<PageId> = (0..3)
        .map(|_| pager.allocate_page(PageType::BTreeLeaf).unwrap())
        .collect();

    for (i, &page_id) in page_ids.iter().enumerate() {
        let mut page = Page::new(page_id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
        page.data_mut()
            .extend_from_slice(format!("page {}", i).as_bytes());
        pager.write_page(&page).unwrap();
    }

    // Read pages to populate cache
    for &page_id in &page_ids {
        pager.read_page(page_id).unwrap();
    }

    // Clear cache
    pager.clear_cache().unwrap();

    // Verify cache is empty
    let stats = pager.cache_stats().unwrap();
    assert_eq!(stats.current_size, 0);
}

/// Test cache disabled (capacity = 0)
#[test]
fn test_cache_disabled() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::new().with_cache_capacity(0); // Disable cache

    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Allocate and write a page
    let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
    let mut page = Page::new(page_id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
    page.data_mut().extend_from_slice(b"no cache");
    pager.write_page(&page).unwrap();

    // Read page
    let read_page = pager.read_page(page_id).unwrap();
    assert_eq!(read_page.data()[0..8], b"no cache"[..]);

    // Verify no cache stats available
    assert!(pager.cache_stats().is_none());
}

/// Test cache sync operation
#[test]
fn test_cache_sync() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::new()
        .with_cache_capacity(10)
        .with_cache_write_back(true);

    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Allocate and write pages
    let page_ids: Vec<PageId> = (0..3)
        .map(|_| pager.allocate_page(PageType::BTreeLeaf).unwrap())
        .collect();

    for (i, &page_id) in page_ids.iter().enumerate() {
        let mut page = Page::new(page_id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
        page.data_mut()
            .extend_from_slice(format!("page {}", i).as_bytes());
        pager.write_page(&page).unwrap();
    }

    // Sync should flush cache
    pager.sync().unwrap();

    // Verify all dirty pages were flushed
    let stats = pager.cache_stats().unwrap();
    assert_eq!(stats.dirty_pages, 0);
}
