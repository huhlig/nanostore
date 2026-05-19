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

//! Pager implementation - Main page management logic

use crate::pager::{
    CacheConfig, FileHeader, FreeList, FreeListPage, Page, PageCache, PageId, PageSize, PageTable,
    PageType, PagerConfig, PagerError, PagerResult, PinTable, Superblock,
};
use crate::vfs::{File, FileSystem};
use metrics::{counter, gauge, histogram};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, instrument, warn};

/// Pager - Manages page-level storage operations
///
/// The Pager provides:
/// - Page allocation and deallocation
/// - Page reading and writing with checksums
/// - Free list management
/// - Optional compression and encryption
/// - Superblock management
/// - LRU page cache for performance
/// - Page pinning to prevent concurrent free/read corruption
pub struct Pager<FS: FileSystem> {
    /// VFS file handle
    file: Arc<RwLock<FS::File>>,
    /// Pager configuration
    config: PagerConfig,
    /// File header
    header: Arc<RwLock<FileHeader>>,
    /// Superblock
    superblock: Arc<RwLock<Superblock>>,
    /// Free list manager (lock-free)
    free_list: Arc<FreeList>,
    /// Page cache (optional)
    cache: Option<PageCache>,
    /// Pin table for reference counting
    pin_table: PinTable,
    /// Page table for fine-grained locking
    page_table: PageTable,
}

impl<FS: FileSystem> Pager<FS> {
    /// Create a new database file with the given configuration
    #[instrument(skip(fs, config), fields(path = %path))]
    pub fn create(fs: &FS, path: &str, config: PagerConfig) -> PagerResult<Self> {
        debug!("Creating new pager");
        // Validate configuration
        config.validate().map_err(PagerError::ConfigError)?;

        // Create the file
        let mut file = fs.create_file(path)?;

        // Create file header
        let header = FileHeader::new(config.page_size, config.compression, config.encryption);

        // Write file header to page 0
        let header_bytes = header.to_bytes();
        let mut page0_data = vec![0u8; config.page_size.to_u32() as usize];
        page0_data[0..FileHeader::SIZE].copy_from_slice(&header_bytes);
        file.write_to_offset(0, &page0_data)?;

        // Create superblock
        let superblock = Superblock::new();

        // Write superblock to page 1
        let mut superblock_page = Page::new(
            PageId::from(1),
            PageType::Superblock,
            config.page_size.data_size(),
        );
        superblock_page
            .data_mut()
            .extend_from_slice(&superblock.to_bytes());
        let page1_data = superblock_page.to_bytes(
            config.page_size.to_u32() as usize,
            config.encryption_key.as_ref(),
        )?;
        file.write_to_offset(config.page_size.to_u32() as u64, &page1_data)?;

        // Sync to disk
        file.sync_all()?;

        // Create free list
        let free_list = FreeList::new();

        // Create cache if enabled
        let cache = if config.cache_capacity > 0 {
            let cache_config = CacheConfig::new()
                .with_capacity(config.cache_capacity)
                .with_write_back(config.cache_write_back);
            Some(PageCache::new(cache_config))
        } else {
            None
        };

        Ok(Self {
            file: Arc::new(RwLock::new(file)),
            config,
            header: Arc::new(RwLock::new(header)),
            superblock: Arc::new(RwLock::new(superblock)),
            free_list: Arc::new(free_list),
            cache,
            pin_table: PinTable::new(),
            page_table: PageTable::new(),
        })
    }

    /// Open an existing database file
    #[instrument(skip(fs), fields(path = %path))]
    pub fn open(fs: &FS, path: &str) -> PagerResult<Self> {
        debug!("Opening existing pager");
        let mut file = fs.open_file(path)?;

        // Read and parse file header from page 0
        let page_size_guess = PageSize::Size4KB.to_u32() as usize;
        let mut header_data = vec![0u8; page_size_guess];
        file.read_at_offset(0, &mut header_data)?;
        let header = FileHeader::from_bytes(&header_data)?;

        // Now we know the actual page size
        let page_size = header.page_size.to_u32() as usize;

        // Read superblock from page 1
        let mut superblock_data = vec![0u8; page_size];
        file.read_at_offset(page_size as u64, &mut superblock_data)?;
        let superblock_page = Page::from_bytes(&superblock_data, true, None)?;
        let superblock = Superblock::from_bytes(superblock_page.data())?;

        // Create configuration from header
        let config = PagerConfig {
            page_size: header.page_size,
            compression: header.compression,
            encryption: header.encryption,
            encryption_key: None, // Will need to be provided separately for encrypted databases
            enable_checksums: true,
            cache_capacity: 1000,   // Default cache capacity
            cache_write_back: true, // Default to write-back
        };

        // Initialize free list from persisted free-list pages
        let free_list = FreeList::from_state(
            superblock.first_free_list_page,
            superblock.last_free_list_page,
            superblock.free_pages,
        );

        if superblock.free_pages > 0 && superblock.first_free_list_page != PageId::from(0) {
            let mut all_free_pages = Vec::new();
            let mut current_page_id = superblock.first_free_list_page;

            while current_page_id != PageId::from(0) {
                let offset = current_page_id.as_u64() * page_size as u64;
                let mut free_list_page_data = vec![0u8; page_size];
                file.read_at_offset(offset, &mut free_list_page_data)?;

                let page = Page::from_bytes(&free_list_page_data, true, None)?;
                let free_list_page = FreeListPage::from_bytes(page.data())?;

                all_free_pages.extend(free_list_page.free_pages.iter().copied());
                current_page_id = free_list_page.next_page;
            }

            free_list.set_free_pages(all_free_pages);
        }

        // Create cache if enabled
        let cache = if config.cache_capacity > 0 {
            let cache_config = CacheConfig::new()
                .with_capacity(config.cache_capacity)
                .with_write_back(config.cache_write_back);
            Some(PageCache::new(cache_config))
        } else {
            None
        };

        Ok(Self {
            file: Arc::new(RwLock::new(file)),
            config,
            header: Arc::new(RwLock::new(header)),
            superblock: Arc::new(RwLock::new(superblock)),
            free_list: Arc::new(free_list),
            cache,
            pin_table: PinTable::new(),
            page_table: PageTable::new(),
        })
    }

    /// Get the page size
    pub fn page_size(&self) -> PageSize {
        self.config.page_size
    }

    /// Get the total number of pages
    pub fn total_pages(&self) -> u64 {
        self.superblock.read().total_pages
    }

    /// Get the number of free pages
    pub fn free_pages(&self) -> u64 {
        self.free_list.total_free()
    }

    /// Allocate a new page
    ///
    /// This will either:
    /// 1. Reuse a page from the free list, or
    /// 2. Grow the database by allocating a new page
    ///
    /// # Lock Ordering
    /// Follows the hierarchy: superblock → header → page_table → file
    #[instrument(skip(self), fields(page_type = ?page_type, page_id, from_freelist))]
    pub fn allocate_page(&self, page_type: PageType) -> PagerResult<PageId> {
        let start = Instant::now();
        debug!("Allocating page");

        // STEP 1: Lock-free allocation from free list or superblock
        // Lock ordering: superblock first (level 2)
        let (page_id, from_freelist) = if let Some(page_id) = self.free_list.pop_page() {
            // Got a page from free list - mark it allocated in superblock
            let mut superblock = self.superblock.write();
            superblock.mark_page_allocated();
            drop(superblock); // Release immediately
            counter!("nanostore.pager.page.reused").increment(1);
            debug!("Page allocated from free list");
            (page_id, true)
        } else {
            // No free pages - allocate a new one
            let mut superblock = self.superblock.write();
            let page_id = superblock.allocate_new_page();
            drop(superblock); // Release immediately
            counter!("nanostore.pager.page.grown").increment(1);
            debug!("Page allocated by growing database");
            (page_id, false)
        };

        // Record span fields
        tracing::Span::current().record("page_id", page_id.as_u64());
        tracing::Span::current().record("from_freelist", from_freelist);

        counter!("nanostore.pager.page.allocated").increment(1);

        // STEP 2: Prepare data (no locks held)
        let mut page = Page::new(page_id, page_type, self.config.page_size.data_size());
        page.header.compression = self.config.compression;
        page.header.encryption = self.config.encryption;

        let page_size = self.config.page_size.to_u32() as usize;
        let page_bytes = page.to_bytes(page_size, self.config.encryption_key.as_ref())?;

        // STEP 3: Collect metadata (lock ordering: superblock → header)
        let (header_data, superblock_data) = {
            let free_pages = self.free_list.total_free();

            // Lock superblock first (level 2)
            let superblock_data = {
                let superblock = self.superblock.read();
                superblock.clone()
            };

            // Then lock header (level 3)
            let header_data = {
                let mut header = self.header.write();
                header.total_pages = superblock_data.total_pages;
                header.free_pages = free_pages;
                header.first_free_list_page_id = 0;
                header.update_modified_timestamp();
                header.clone()
            };

            (header_data, superblock_data)
        };

        // STEP 4: Acquire page lock (level 4), then file lock (level 6)
        let _page_lock = self.page_table.write_lock(page_id);

        {
            let mut file = self.file.write();
            file.write_to_offset(page_id.as_u64() * page_size as u64, &page_bytes)?;
            let header_bytes = header_data.to_bytes();
            let mut page0_data = vec![0u8; page_size];
            page0_data[0..FileHeader::SIZE].copy_from_slice(&header_bytes);
            file.write_to_offset(0, &page0_data)?;

            let mut superblock_page = Page::new(
                PageId::from(1),
                PageType::Superblock,
                self.config.page_size.data_size(),
            );
            superblock_page.header.compression = self.config.compression;
            superblock_page.header.encryption = self.config.encryption;
            superblock_page
                .data_mut()
                .extend_from_slice(&superblock_data.to_bytes());
            let superblock_bytes =
                superblock_page.to_bytes(page_size, self.config.encryption_key.as_ref())?;
            file.write_to_offset(page_size as u64, &superblock_bytes)?;
        }

        histogram!("nanostore.pager.allocate.duration_seconds").record(start.elapsed().as_secs_f64());
        debug!("Page allocated successfully");
        Ok(page_id)
    }

    /// Free a page (add it to the free list)
    ///
    /// # Lock Ordering
    /// Follows the hierarchy: pin_table → superblock → header → page_table → file
    #[instrument(skip(self), fields(page_id = %page_id))]
    pub fn free_page(&self, page_id: PageId) -> PagerResult<()> {
        let start = Instant::now();
        debug!("Freeing page");

        if page_id == PageId::from(0) || page_id == PageId::from(1) {
            warn!("Attempted to free reserved page");
            counter!("nanostore.pager.error", "type" => "invalid_page_id").increment(1);
            return Err(PagerError::InvalidPageId(page_id));
        }

        // STEP 1: Check if page is pinned (level 1 - pin_table)
        // This prevents freeing pages that are currently being read
        if self.pin_table.is_pinned(page_id) {
            warn!("Attempted to free pinned page");
            counter!("nanostore.pager.error", "type" => "page_pinned").increment(1);
            return Err(PagerError::PagePinned(page_id));
        }

        let page_size = self.config.page_size.to_u32() as usize;
        let offset = page_id.as_u64() * page_size as u64;

        // STEP 2: Acquire page lock (level 4), then file lock (level 6) to verify page
        let _page_lock = self.page_table.write_lock(page_id);

        {
            let mut file = self.file.write();
            let mut buffer = vec![0u8; page_size];
            file.read_at_offset(offset, &mut buffer)?;
            let page = Page::from_bytes(
                &buffer,
                self.config.enable_checksums,
                self.config.encryption_key.as_ref(),
            )?;
            if page.page_type() == PageType::Free || page.page_type() == PageType::FreeList {
                return Err(PagerError::PageAlreadyFree(page_id));
            }

            let mut free_page =
                Page::new(page_id, PageType::Free, self.config.page_size.data_size());
            free_page.header.compression = self.config.compression;
            free_page.header.encryption = self.config.encryption;
            let free_page_bytes =
                free_page.to_bytes(page_size, self.config.encryption_key.as_ref())?;
            file.write_to_offset(offset, &free_page_bytes)?;
        }
        // File lock released here

        // STEP 3: Add to free list (lock-free, no ordering needed)
        self.free_list.push_page(page_id);

        // STEP 4: Update superblock (level 2)
        {
            let mut superblock = self.superblock.write();
            superblock.mark_page_freed();
        }

        // STEP 5: Collect metadata (lock ordering: superblock → header)
        let (header_data, superblock_data) = {
            let free_pages = self.free_list.total_free();

            // Lock superblock first (level 2)
            let superblock_data = {
                let superblock = self.superblock.read();
                superblock.clone()
            };

            // Then lock header (level 3)
            let header_data = {
                let mut header = self.header.write();
                header.total_pages = superblock_data.total_pages;
                header.free_pages = free_pages;
                header.first_free_list_page_id = 0;
                header.update_modified_timestamp();
                header.clone()
            };

            (header_data, superblock_data)
        };

        // STEP 6: Write metadata to disk (file lock - level 6)
        // Note: page_lock is still held, which is fine since we're writing to different pages
        {
            let mut file = self.file.write();
            let header_bytes = header_data.to_bytes();
            let mut page0_data = vec![0u8; page_size];
            page0_data[0..FileHeader::SIZE].copy_from_slice(&header_bytes);
            file.write_to_offset(0, &page0_data)?;

            let mut superblock_page = Page::new(
                PageId::from(1),
                PageType::Superblock,
                self.config.page_size.data_size(),
            );
            superblock_page.header.compression = self.config.compression;
            superblock_page.header.encryption = self.config.encryption;
            superblock_page
                .data_mut()
                .extend_from_slice(&superblock_data.to_bytes());
            let superblock_bytes =
                superblock_page.to_bytes(page_size, self.config.encryption_key.as_ref())?;
            file.write_to_offset(page_size as u64, &superblock_bytes)?;
        }

        counter!("nanostore.pager.page.freed").increment(1);
        histogram!("nanostore.pager.free.duration_seconds").record(start.elapsed().as_secs_f64());
        gauge!("nanostore.pager.freelist.size").set(self.free_list.total_free() as f64);
        debug!("Page freed successfully");
        Ok(())
    }

    /// Read a page from disk (with caching)
    ///
    /// # Lock Ordering
    /// Follows the hierarchy: pin_table → page_table → cache → file
    #[instrument(skip(self), fields(page_id = %page_id, cache_hit))]
    pub fn read_page(&self, page_id: PageId) -> PagerResult<Page> {
        let start = Instant::now();
        debug!("Reading page");

        if page_id.as_u64() >= self.total_pages() {
            counter!("nanostore.pager.error", "type" => "page_not_found").increment(1);
            return Err(PagerError::PageNotFound(page_id));
        }

        // Try cache first (level 5 - cache)
        if let Some(cache) = &self.cache
            && let Some(page) = cache.get(page_id)
        {
            tracing::Span::current().record("cache_hit", true);
            debug!("Cache hit");
            counter!("nanostore.pager.page.read").increment(1);
            histogram!("nanostore.pager.read.duration_seconds").record(start.elapsed().as_secs_f64());
            return Ok(page);
        }

        tracing::Span::current().record("cache_hit", false);
        debug!("Cache miss - reading from disk");

        // STEP 1: Pin the page (level 1 - pin_table)
        // This ensures the page cannot be freed and reallocated while we're reading it
        self.pin_table.pin(page_id);

        // STEP 2: Acquire page-level read lock (level 4 - page_table)
        // Multiple threads can read different pages concurrently (different shards)
        let _page_lock = self.page_table.read_lock(page_id);

        // Cache miss - read from disk
        let page_size = self.config.page_size.to_u32() as usize;
        let offset = page_id.as_u64() * page_size as u64;

        let result = (|| {
            let mut buffer = vec![0u8; page_size];
            // STEP 3: Acquire file lock (level 6 - file)
            // Note: VFS File trait requires &mut self for read_at_offset
            let mut file = self.file.write();
            file.read_at_offset(offset, &mut buffer)?;
            drop(file); // Release file lock early

            let page = Page::from_bytes(
                &buffer,
                self.config.enable_checksums,
                self.config.encryption_key.as_ref(),
            )?;

            // STEP 4: Update cache (level 5 - cache)
            if let Some(cache) = &self.cache {
                // If evicted page is dirty, write it to disk
                if let Some(evicted_page) = cache.put(page.clone(), false) {
                    self.write_page_to_disk(&evicted_page)?;
                }
            }

            Ok(page)
        })();

        // CRITICAL: Always unpin the page (level 1), even if an error occurred
        self.pin_table.unpin(page_id);

        if result.is_ok() {
            let page_size = self.config.page_size.to_u32() as u64;
            counter!("nanostore.pager.page.read").increment(1);
            counter!("nanostore.pager.bytes.read").increment(page_size);
            histogram!("nanostore.pager.read.duration_seconds").record(start.elapsed().as_secs_f64());
            debug!("Page read successfully");
        }

        result
    }

    /// Write a page to disk (with caching)
    #[instrument(skip(self, page), fields(page_id = %page.page_id(), write_through))]
    pub fn write_page(&self, page: &Page) -> PagerResult<()> {
        let start = Instant::now();
        debug!("Writing page");

        let page_size = self.config.page_size.to_u32() as u64;
        if let Some(cache) = &self.cache {
            // Keep the cache updated, but also persist the page immediately so
            // reopened pagers observe the latest on-disk bytes.
            if self.config.cache_write_back {
                tracing::Span::current().record("write_through", false);
                if let Some(evicted_page) = cache.put(page.clone(), true) {
                    self.write_page_to_disk(&evicted_page)?;
                }
                self.write_page_to_disk(page)?;
                cache.mark_clean(page.page_id());
                counter!("nanostore.pager.page.write").increment(1);
                counter!("nanostore.pager.bytes.written").increment(page_size);
                histogram!("nanostore.pager.write.duration_seconds")
                    .record(start.elapsed().as_secs_f64());
                debug!("Page written successfully");
                return Ok(());
            }
            // Write-through mode: write to disk and update cache
            tracing::Span::current().record("write_through", true);
            self.write_page_to_disk(page)?;
            cache.put(page.clone(), false);
            counter!("nanostore.pager.page.write").increment(1);
            counter!("nanostore.pager.bytes.written").increment(page_size);
            histogram!("nanostore.pager.write.duration_seconds").record(start.elapsed().as_secs_f64());
            debug!("Page written successfully");
            return Ok(());
        }

        // No cache - write directly to disk
        let result = self.write_page_to_disk(page);

        if result.is_ok() {
            counter!("nanostore.pager.page.write").increment(1);
            counter!("nanostore.pager.bytes.written").increment(page_size);
            histogram!("nanostore.pager.write.duration_seconds").record(start.elapsed().as_secs_f64());
            debug!("Page written successfully");
        }

        result
    }

    /// Write a page directly to disk (bypassing cache)
    ///
    /// # Lock Ordering
    /// Follows the hierarchy: page_table → file
    fn write_page_to_disk(&self, page: &Page) -> PagerResult<()> {
        // STEP 1: Acquire page-level write lock (level 4 - page_table)
        // Only one thread can write to a page at a time, but different pages can be written concurrently
        let _page_lock = self.page_table.write_lock(page.page_id());

        let page_size = self.config.page_size.to_u32() as usize;
        let offset = page.page_id().as_u64() * page_size as u64;

        let buffer = page.to_bytes(page_size, self.config.encryption_key.as_ref())?;

        // STEP 2: Acquire file lock (level 6 - file)
        let mut file = self.file.write();
        file.write_to_offset(offset, &buffer)?;

        Ok(())
    }

    /// Flush all dirty pages from cache to disk
    #[instrument(skip(self), fields(dirty_count))]
    pub fn flush_cache(&self) -> PagerResult<()> {
        let start = Instant::now();
        if let Some(cache) = &self.cache {
            let dirty_pages = cache.get_dirty_pages();
            let dirty_count = dirty_pages.len();
            tracing::Span::current().record("dirty_count", dirty_count);
            debug!("Flushing dirty pages");

            for (page_id, page) in dirty_pages {
                self.write_page_to_disk(&page)?;
                cache.mark_clean(page_id);
            }

            histogram!("nanostore.pager.flush.duration_seconds").record(start.elapsed().as_secs_f64());
            debug!(
                flushed_count = dirty_count,
                duration_ms = start.elapsed().as_millis(),
                "Dirty pages flushed"
            );
        }
        Ok(())
    }

    /// Get cache statistics
    pub fn cache_stats(&self) -> Option<crate::pager::CacheStats> {
        let stats = self.cache.as_ref().map(|c| c.stats());

        // Update metrics gauges with current cache stats
        if let Some(ref s) = stats {
            gauge!("nanostore.pager.cache.size").set(s.current_size as f64);
            gauge!("nanostore.pager.cache.dirty_pages").set(s.dirty_pages as f64);
        }

        stats
    }

    /// Clear the cache
    pub fn clear_cache(&self) -> PagerResult<()> {
        if let Some(cache) = &self.cache {
            let dirty_pages = cache.clear();
            // Write any dirty pages to disk
            for (_, page) in dirty_pages {
                self.write_page_to_disk(&page)?;
            }
        }
        Ok(())
    }

    /// Sync all changes to disk
    #[instrument(skip(self))]
    pub fn sync(&self) -> PagerResult<()> {
        let start = Instant::now();
        debug!("Syncing to disk");

        // Flush cache first
        self.flush_cache()?;

        let fsync_start = Instant::now();
        let mut file = self.file.write();
        file.sync_all()?;
        drop(file);

        histogram!("nanostore.pager.fsync.duration_seconds")
            .record(fsync_start.elapsed().as_secs_f64());
        histogram!("nanostore.pager.sync.duration_seconds").record(start.elapsed().as_secs_f64());
        debug!(duration_ms = start.elapsed().as_millis(), "Sync completed");
        Ok(())
    }

    /// Read a free list page
    fn read_free_list_page(&self, page_id: PageId) -> PagerResult<FreeListPage> {
        let page = self.read_page(page_id)?;
        FreeListPage::from_bytes(page.data())
    }

    /// Write a free list page
    fn write_free_list_page(
        &self,
        page_id: PageId,
        free_list_page: &FreeListPage,
    ) -> PagerResult<()> {
        let mut page = Page::new(
            page_id,
            PageType::FreeList,
            self.config.page_size.data_size(),
        );
        page.header.compression = self.config.compression;
        page.header.encryption = self.config.encryption;
        page.data_mut()
            .extend_from_slice(&free_list_page.to_bytes());
        self.write_page(&page)
    }

    /// Write the file header
    fn write_header(&self, header: &FileHeader) -> PagerResult<()> {
        let header_bytes = header.to_bytes();
        let mut page0_data = vec![0u8; self.config.page_size.to_u32() as usize];
        page0_data[0..FileHeader::SIZE].copy_from_slice(&header_bytes);

        let mut file = self.file.write();
        file.write_to_offset(0, &page0_data)?;
        Ok(())
    }

    /// Write the superblock
    fn write_superblock(&self, superblock: &Superblock) -> PagerResult<()> {
        let mut page = Page::new(
            PageId::from(1),
            PageType::Superblock,
            self.config.page_size.data_size(),
        );
        page.header.compression = self.config.compression;
        page.header.encryption = self.config.encryption;
        page.data_mut().extend_from_slice(&superblock.to_bytes());
        self.write_page(&page)
    }

    /// Persist the B-Tree root page ID to the superblock.
    pub fn set_root_btree_page(&self, root_page_id: PageId) -> PagerResult<()> {
        let superblock_snapshot = {
            let mut superblock = self.superblock.write();
            superblock.root_btree_page = root_page_id;
            superblock.clone()
        };

        self.write_superblock(&superblock_snapshot)
    }

    /// Read the persisted B-Tree root page ID from the superblock.
    pub fn root_btree_page(&self) -> PageId {
        self.superblock.read().root_btree_page
    }

    /// Persist the B-Tree row count to the superblock.
    pub fn set_btree_row_count(&self, row_count: u64) -> PagerResult<()> {
        let superblock_snapshot = {
            let mut superblock = self.superblock.write();
            superblock.btree_row_count = row_count;
            superblock.clone()
        };

        self.write_superblock(&superblock_snapshot)
    }

    /// Read the persisted B-Tree row count from the superblock.
    pub fn btree_row_count(&self) -> u64 {
        self.superblock.read().btree_row_count
    }

    // =========================================================================
    // Overflow Page Chain Methods
    // =========================================================================

    /// Write data to an overflow page with header and checksum
    ///
    /// Returns the page ID of the written page.
    #[instrument(skip(self, data), fields(data_len = data.len()))]
    pub fn write_overflow_page(
        &self,
        page_id: PageId,
        data: &[u8],
        next_page_id: Option<PageId>,
    ) -> PagerResult<()> {
        use crate::pager::page::{OverflowPageHeader, calculate_crc32};

        debug!("Writing overflow page");

        // Calculate checksum
        let checksum = calculate_crc32(data);

        // Create overflow header
        let header = OverflowPageHeader::new(
            next_page_id.map(|id| id.as_u64() as u32).unwrap_or(0),
            data.len() as u32,
            checksum,
        );

        // Create page with overflow data
        let mut page = Page::new(
            page_id,
            PageType::Overflow,
            self.config.page_size.data_size(),
        );
        page.header.compression = self.config.compression;
        page.header.encryption = self.config.encryption;

        // Write header and data to page
        page.data_mut().extend_from_slice(&header.to_bytes());
        page.data_mut().extend_from_slice(data);

        self.write_page(&page)?;

        counter!("pager.overflow_page_write").increment(1);
        Ok(())
    }

    /// Link two overflow pages together
    ///
    /// Updates the first page's header to point to the second page.
    #[instrument(skip(self), fields(from = %from_page_id, to = %to_page_id))]
    pub fn link_overflow_pages(&self, from_page_id: PageId, to_page_id: PageId) -> PagerResult<()> {
        use crate::pager::page::OverflowPageHeader;

        debug!("Linking overflow pages");

        // Read the current page
        let page = self.read_page(from_page_id)?;

        // Parse the overflow header
        let mut header = OverflowPageHeader::from_bytes(page.data())?;

        // Update next_page_id to link to the new page
        header.next_page_id = to_page_id.as_u64() as u32;

        // Extract the data (skip header)
        let data = &page.data()[OverflowPageHeader::SIZE..];

        // Create a new page with the updated header
        let mut new_page = Page::new(
            from_page_id,
            PageType::Overflow,
            self.config.page_size.data_size(),
        );
        new_page.header.compression = self.config.compression;
        new_page.header.encryption = self.config.encryption;

        // Write updated header and original data to page
        new_page.data_mut().extend_from_slice(&header.to_bytes());
        new_page.data_mut().extend_from_slice(data);

        self.write_page(&new_page)?;

        Ok(())
    }

    /// Allocate and write a chain of overflow pages for the given data
    ///
    /// Returns a vector of allocated page IDs in chain order.
    #[instrument(skip(self, data), fields(data_len = data.len()))]
    pub fn allocate_overflow_chain(&self, data: &[u8]) -> PagerResult<Vec<PageId>> {
        use crate::pager::page::OverflowPageHeader;

        debug!("Allocating overflow chain");

        if data.is_empty() {
            return Ok(Vec::new());
        }

        // Calculate how much data fits in each overflow page
        let page_data_size = self.config.page_size.data_size() - OverflowPageHeader::SIZE;

        // Calculate number of pages needed
        let num_pages = data.len().div_ceil(page_data_size);

        // Allocate all pages first
        let mut page_ids = Vec::with_capacity(num_pages);
        for _ in 0..num_pages {
            let page_id = self.allocate_page(PageType::Overflow)?;
            page_ids.push(page_id);
        }

        // Write data to pages
        for (i, page_id) in page_ids.iter().enumerate() {
            let start = i * page_data_size;
            let end = ((i + 1) * page_data_size).min(data.len());
            let chunk = &data[start..end];

            let next_page_id = if i + 1 < page_ids.len() {
                Some(page_ids[i + 1])
            } else {
                None
            };

            self.write_overflow_page(*page_id, chunk, next_page_id)?;
        }

        counter!("pager.overflow_chain_allocated").increment(1);
        histogram!("pager.overflow_chain_pages").record(num_pages as f64);

        Ok(page_ids)
    }

    /// Read data from an overflow page chain
    ///
    /// Reads and validates all pages in the chain, returning the complete data.
    #[instrument(skip(self), fields(first_page = %first_page_id))]
    pub fn read_overflow_chain(&self, first_page_id: PageId) -> PagerResult<Vec<u8>> {
        use crate::pager::page::{OverflowPageHeader, calculate_crc32};

        debug!("Reading overflow chain");

        let mut result = Vec::new();
        let mut current_page_id = first_page_id;
        let mut pages_read = 0;

        loop {
            // Read the page
            let page = self.read_page(current_page_id)?;

            // Verify it's an overflow page
            if page.page_type() != PageType::Overflow {
                return Err(PagerError::InternalError(format!(
                    "Expected overflow page, got {:?}",
                    page.page_type()
                )));
            }

            // Parse header
            let header = OverflowPageHeader::from_bytes(page.data())?;

            // Extract data (skip header)
            let data_start = OverflowPageHeader::SIZE;
            let data_end = data_start + header.data_length as usize;

            if data_end > page.data().len() {
                return Err(PagerError::InternalError(format!(
                    "Overflow page data length {} exceeds page size",
                    header.data_length
                )));
            }

            let data = &page.data()[data_start..data_end];

            // Verify checksum
            let actual_checksum = calculate_crc32(data);
            if actual_checksum != header.checksum {
                return Err(PagerError::InternalError(format!(
                    "Overflow page checksum mismatch: expected 0x{:08X}, got 0x{:08X}",
                    header.checksum, actual_checksum
                )));
            }

            // Append data to result
            result.extend_from_slice(data);
            pages_read += 1;

            // Check if this is the last page
            if header.is_last() {
                break;
            }

            // Move to next page
            current_page_id = PageId::from(header.next_page_id as u64);
        }

        counter!("pager.overflow_chain_read").increment(1);
        histogram!("pager.overflow_chain_pages_read").record(pages_read as f64);

        Ok(result)
    }

    /// Free all pages in an overflow chain
    ///
    /// Walks the chain and frees each page.
    #[instrument(skip(self), fields(first_page = %first_page_id))]
    pub fn free_overflow_chain(&self, first_page_id: PageId) -> PagerResult<()> {
        use crate::pager::page::OverflowPageHeader;

        debug!("Freeing overflow chain");

        let mut current_page_id = first_page_id;
        let mut pages_freed = 0;

        loop {
            // Read the page to get the next page ID
            let page = self.read_page(current_page_id)?;

            // Verify it's an overflow page
            if page.page_type() != PageType::Overflow {
                return Err(PagerError::InternalError(format!(
                    "Expected overflow page, got {:?}",
                    page.page_type()
                )));
            }

            // Parse header to get next page
            let header = OverflowPageHeader::from_bytes(page.data())?;
            let next_page_id = if header.is_last() {
                None
            } else {
                Some(PageId::from(header.next_page_id as u64))
            };

            // Free the current page
            self.free_page(current_page_id)?;
            pages_freed += 1;

            // Move to next page or exit
            match next_page_id {
                Some(next_id) => current_page_id = next_id,
                None => break,
            }
        }

        counter!("pager.overflow_chain_freed").increment(1);
        histogram!("pager.overflow_chain_pages_freed").record(pages_freed as f64);

        Ok(())
    }

    /// Free overflow pages referenced by a ValueRef
    ///
    /// This is a convenience method for vacuum operations that need to clean up
    /// external values. It handles both SinglePage and OverflowChain variants.
    ///
    /// # Arguments
    /// * `value_ref` - The ValueRef to free
    ///
    /// # Returns
    /// * `Ok(usize)` - Number of pages freed
    /// * `Err(PagerError)` - If freeing fails
    #[instrument(skip(self))]
    pub fn free_value_ref(&self, value_ref: &crate::types::ValueRef) -> PagerResult<usize> {
        use crate::types::ValueRef;

        match value_ref {
            ValueRef::Inline => {
                // Inline values don't use overflow pages
                Ok(0)
            }
            ValueRef::SinglePage { page_id, .. } => {
                // Free single overflow page
                self.free_page(PageId::from(*page_id as u64))?;
                counter!("pager.vacuum_pages_freed").increment(1);
                Ok(1)
            }
            ValueRef::OverflowChain {
                first_page_id,
                page_count,
                ..
            } => {
                // Free entire overflow chain
                self.free_overflow_chain(PageId::from(*first_page_id as u64))?;
                counter!("pager.vacuum_pages_freed").increment(*page_count as u64);
                Ok(*page_count as usize)
            }
        }
    }

    /// Free multiple overflow pages referenced by ValueRefs
    ///
    /// This is a batch operation for vacuum that processes multiple freed values.
    /// It continues on error and returns the total number of pages freed and any errors.
    ///
    /// # Arguments
    /// * `value_refs` - Slice of ValueRefs to free
    ///
    /// # Returns
    /// * `Ok(usize)` - Total number of pages freed
    /// * `Err(PagerError)` - If any freeing operation fails (after attempting all)
    #[instrument(skip(self, value_refs), fields(count = value_refs.len()))]
    pub fn free_value_refs(&self, value_refs: &[crate::types::ValueRef]) -> PagerResult<usize> {
        let mut total_freed = 0;
        let mut errors = Vec::new();

        for (idx, value_ref) in value_refs.iter().enumerate() {
            match self.free_value_ref(value_ref) {
                Ok(freed) => total_freed += freed,
                Err(e) => {
                    warn!("Failed to free ValueRef at index {}: {:?}", idx, e);
                    errors.push((idx, e));
                }
            }
        }

        if !errors.is_empty() {
            // Return first error but log that we had multiple
            warn!(
                "Failed to free {} out of {} ValueRefs",
                errors.len(),
                value_refs.len()
            );
            return Err(errors.into_iter().next().unwrap().1);
        }

        counter!("pager.vacuum_batch_freed").increment(1);
        histogram!("pager.vacuum_batch_size").record(value_refs.len() as f64);
        histogram!("pager.vacuum_total_pages_freed").record(total_freed as f64);

        Ok(total_freed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::MemoryFileSystem;

    #[test]
    fn test_pager_create() {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default();

        let pager = Pager::create(&fs, "test.db", config).unwrap();
        assert_eq!(pager.total_pages(), 2); // Header + Superblock
        assert_eq!(pager.free_pages(), 0);
    }

    #[test]
    fn test_pager_open() {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default();

        // Create database
        {
            let _pager = Pager::create(&fs, "test.db", config.clone()).unwrap();
        }

        // Open database
        let pager = Pager::open(&fs, "test.db").unwrap();
        assert_eq!(pager.total_pages(), 2);
    }

    #[test]
    fn test_page_allocation() {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default();
        let pager = Pager::create(&fs, "test.db", config).unwrap();

        // Allocate a new page
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        assert_eq!(page_id, PageId::from(2));
        assert_eq!(pager.total_pages(), 3);
    }

    #[test]
    fn test_page_read_write() {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default();
        let pager = Pager::create(&fs, "test.db", config).unwrap();

        // Allocate and write a page
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
        page.data_mut().extend_from_slice(b"test data");

        pager.write_page(&page).unwrap();

        // Read it back
        let read_page = pager.read_page(page_id).unwrap();
        assert_eq!(read_page.page_id(), page_id);
        assert_eq!(read_page.page_type(), PageType::BTreeLeaf);
        assert_eq!(&read_page.data()[0..9], b"test data");
    }

    #[test]
    fn test_page_free_and_reuse() {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default();
        let pager = Pager::create(&fs, "test.db", config).unwrap();

        // Allocate a page
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        assert_eq!(page_id, PageId::from(2));
        assert_eq!(pager.free_pages(), 0);

        // Free the page
        pager.free_page(page_id).unwrap();
        assert_eq!(pager.free_pages(), 1);

        // Allocate again - should reuse the freed page
        let reused_page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        assert_eq!(reused_page_id, page_id);
        assert_eq!(pager.free_pages(), 0);
    }
}
