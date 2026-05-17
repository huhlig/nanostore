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

//! Paged Bloom filter table implementation.
//!
//! This module provides a standalone Bloom filter table that stores its bitmap
//! across multiple pages, supporting large-scale approximate membership testing.
//! Unlike the in-memory BloomFilter in lsm/bloom.rs, this implementation is
//! designed for persistent storage and can handle filters larger than memory.

use crate::pager::{Page, PageId, PageType, Pager};
use crate::table::{
    ApproximateMembership, SpecialtyTableCapabilities, SpecialtyTableStats, Table, TableEngineKind,
    TableError, TableResult, VerificationReport,
};
use crate::types::TableId;
use crate::vfs::FileSystem;
use std::sync::{Arc, RwLock};

/// Paged Bloom filter table for persistent approximate membership testing.
///
/// Stores the bloom filter bitmap across multiple pages, allowing filters
/// larger than available memory. The filter is divided into page-sized chunks,
/// with each chunk stored in a separate page.
pub struct PagedBloomFilter<FS: FileSystem> {
    /// Table identifier
    table_id: TableId,

    /// Table name
    name: String,

    /// Pager for page management
    pager: Arc<Pager<FS>>,

    /// Root page containing metadata
    root_page_id: PageId,

    /// Number of bits in the filter
    num_bits: usize,

    /// Number of hash functions
    num_hash_functions: usize,

    /// Number of items inserted
    num_items: RwLock<usize>,

    /// Page IDs storing the bitmap (in order)
    bitmap_pages: Vec<PageId>,

    /// Bits per page (derived from page size)
    bits_per_page: usize,
}

/// Metadata stored in the root page
#[repr(C)]
struct BloomFilterMetadata {
    /// Magic number for validation
    magic: u32,
    /// Version number
    version: u32,
    /// Number of bits in the filter
    num_bits: u64,
    /// Number of hash functions
    num_hash_functions: u32,
    /// Number of items inserted
    num_items: u64,
    /// Number of bitmap pages
    num_bitmap_pages: u32,
    /// Reserved for future use
    _reserved: [u8; 32],
}

const BLOOM_MAGIC: u32 = 0x424C4F4D; // "BLOM"
const BLOOM_VERSION: u32 = 1;

impl<FS: FileSystem> PagedBloomFilter<FS> {
    /// Create a new paged bloom filter.
    ///
    /// # Arguments
    ///
    /// * `table_id` - Unique identifier for this table
    /// * `name` - Human-readable name
    /// * `pager` - Pager for page management
    /// * `num_items` - Expected number of items
    /// * `bits_per_key` - Number of bits per key (affects false positive rate)
    /// * `num_hash_functions` - Number of hash functions (None = auto-calculate)
    pub fn new(
        table_id: TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        num_items: usize,
        bits_per_key: usize,
        num_hash_functions: Option<usize>,
    ) -> TableResult<Self> {
        let num_bits = num_items * bits_per_key;
        let num_hash_functions = num_hash_functions.unwrap_or_else(|| {
            // Optimal k = (m/n) * ln(2)
            ((bits_per_key as f64) * 0.693).ceil() as usize
        });

        // Allocate root page
        let root_page_id = pager.allocate_page(PageType::BloomMeta)?;

        // Calculate bits per page (page size - header overhead)
        let bits_per_page = pager.page_size().data_size() * 8;

        // Calculate number of pages needed
        let num_pages = num_bits.div_ceil(bits_per_page);

        // Allocate bitmap pages
        let mut bitmap_pages = Vec::with_capacity(num_pages);
        for _ in 0..num_pages {
            let page_id = pager.allocate_page(PageType::BloomData)?;
            bitmap_pages.push(page_id);

            // Initialize page with zeros
            let mut page = Page::new(page_id, PageType::BloomData, pager.page_size().data_size());
            page.data_mut().resize(pager.page_size().data_size(), 0);
            pager.write_page(&page)?;
        }

        let filter = Self {
            table_id,
            name,
            pager,
            root_page_id,
            num_bits,
            num_hash_functions,
            num_items: RwLock::new(0),
            bitmap_pages,
            bits_per_page,
        };

        // Write metadata to root page
        filter.write_metadata()?;

        Ok(filter)
    }

    /// Open an existing paged bloom filter.
    pub fn open(
        table_id: TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        root_page_id: PageId,
    ) -> TableResult<Self> {
        // Read metadata from root page
        let page = pager.read_page(root_page_id)?;

        let metadata = unsafe { &*(page.data().as_ptr() as *const BloomFilterMetadata) };

        // Validate magic number
        if metadata.magic != BLOOM_MAGIC {
            return Err(TableError::corruption(
                "bloom_filter_metadata",
                "invalid_magic",
                "Invalid bloom filter magic number",
            ));
        }

        // Validate version
        if metadata.version != BLOOM_VERSION {
            return Err(TableError::InvalidFormatVersion(metadata.version));
        }

        let num_bits = metadata.num_bits as usize;
        let num_hash_functions = metadata.num_hash_functions as usize;
        let num_items = metadata.num_items as usize;
        let num_bitmap_pages = metadata.num_bitmap_pages as usize;

        // Calculate bits per page
        let bits_per_page = pager.page_size().data_size() * 8;

        // Read bitmap page IDs from metadata page
        let page_ids_offset = std::mem::size_of::<BloomFilterMetadata>();
        let page_ids_data = &page.data()[page_ids_offset..];
        let mut bitmap_pages = Vec::with_capacity(num_bitmap_pages);

        for i in 0..num_bitmap_pages {
            let offset = i * std::mem::size_of::<PageId>();
            let page_id_bytes = &page_ids_data[offset..offset + std::mem::size_of::<PageId>()];
            let page_id = PageId::from(u64::from_le_bytes(page_id_bytes.try_into().unwrap()));
            bitmap_pages.push(page_id);
        }

        Ok(Self {
            table_id,
            name,
            pager,
            root_page_id,
            num_bits,
            num_hash_functions,
            num_items: RwLock::new(num_items),
            bitmap_pages,
            bits_per_page,
        })
    }

    /// Insert a key into the bloom filter.
    pub fn insert(&self, key: &[u8]) -> TableResult<()> {
        let (h1, h2) = self.hash_key(key);

        for i in 0..self.num_hash_functions {
            let bit_pos = self.get_bit_position(h1, h2, i);
            self.set_bit(bit_pos)?;
        }

        {
            let mut num_items = self.num_items.write().unwrap();
            *num_items += 1;
        }
        self.write_metadata()?;

        Ok(())
    }

    /// Check if a key might be in the set.
    pub fn contains(&self, key: &[u8]) -> TableResult<bool> {
        let (h1, h2) = self.hash_key(key);

        for i in 0..self.num_hash_functions {
            let bit_pos = self.get_bit_position(h1, h2, i);
            if !self.get_bit(bit_pos)? {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Calculate the expected false positive rate.
    pub fn false_positive_rate(&self) -> f64 {
        let num_items = *self.num_items.read().unwrap();
        if num_items == 0 {
            return 0.0;
        }

        let k = self.num_hash_functions as f64;
        let m = self.num_bits as f64;
        let n = num_items as f64;

        // FPR = (1 - e^(-kn/m))^k
        (1.0 - ((-k * n) / m).exp()).powf(k)
    }

    /// Clear all bits in the filter.
    pub fn clear(&self) -> TableResult<()> {
        for &page_id in &self.bitmap_pages {
            let mut page = Page::new(
                page_id,
                PageType::BloomData,
                self.pager.page_size().data_size(),
            );
            page.data_mut()
                .resize(self.pager.page_size().data_size(), 0);
            self.pager.write_page(&page)?;
        }

        *self.num_items.write().unwrap() = 0;
        self.write_metadata()?;

        Ok(())
    }

    /// Write metadata to root page.
    fn write_metadata(&self) -> TableResult<()> {
        let mut page = Page::new(
            self.root_page_id,
            PageType::BloomMeta,
            self.pager.page_size().data_size(),
        );
        page.data_mut()
            .resize(self.pager.page_size().data_size(), 0);

        let metadata = BloomFilterMetadata {
            magic: BLOOM_MAGIC,
            version: BLOOM_VERSION,
            num_bits: self.num_bits as u64,
            num_hash_functions: self.num_hash_functions as u32,
            num_items: *self.num_items.read().unwrap() as u64,
            num_bitmap_pages: self.bitmap_pages.len() as u32,
            _reserved: [0; 32],
        };

        // Write metadata
        let metadata_bytes = unsafe {
            std::slice::from_raw_parts(
                &metadata as *const _ as *const u8,
                std::mem::size_of::<BloomFilterMetadata>(),
            )
        };
        page.data_mut()[..metadata_bytes.len()].copy_from_slice(metadata_bytes);

        // Write bitmap page IDs
        let page_ids_offset = std::mem::size_of::<BloomFilterMetadata>();
        for (i, &page_id) in self.bitmap_pages.iter().enumerate() {
            let offset = page_ids_offset + i * std::mem::size_of::<PageId>();
            let page_id_bytes = page_id.to_bytes();
            page.data_mut()[offset..offset + page_id_bytes.len()].copy_from_slice(&page_id_bytes);
        }

        self.pager.write_page(&page)?;
        Ok(())
    }

    /// Hash a key using two independent hash functions.
    fn hash_key(&self, key: &[u8]) -> (u64, u64) {
        // FNV-1a hash
        let mut h1 = 0xcbf29ce484222325u64;
        for &byte in key {
            h1 ^= byte as u64;
            h1 = h1.wrapping_mul(0x100000001b3);
        }

        // Simple multiplicative hash
        let mut h2 = 0u64;
        for &byte in key {
            h2 = h2.wrapping_mul(31).wrapping_add(byte as u64);
        }

        (h1, h2)
    }

    /// Get bit position using double hashing.
    fn get_bit_position(&self, h1: u64, h2: u64, i: usize) -> usize {
        let hash = h1.wrapping_add((i as u64).wrapping_mul(h2));
        (hash % (self.num_bits as u64)) as usize
    }

    /// Set a bit at the given position.
    fn set_bit(&self, pos: usize) -> TableResult<()> {
        let page_idx = pos / self.bits_per_page;
        let bit_in_page = pos % self.bits_per_page;
        let byte_in_page = bit_in_page / 8;
        let bit_in_byte = bit_in_page % 8;

        let page_id = self.bitmap_pages[page_idx];
        let mut page = self.pager.read_page(page_id)?;
        if page.data().len() <= byte_in_page {
            page.data_mut()
                .resize(self.pager.page_size().data_size(), 0);
        }

        page.data_mut()[byte_in_page] |= 1 << bit_in_byte;
        self.pager.write_page(&page)?;

        Ok(())
    }

    /// Get a bit at the given position.
    fn get_bit(&self, pos: usize) -> TableResult<bool> {
        let page_idx = pos / self.bits_per_page;
        let bit_in_page = pos % self.bits_per_page;
        let byte_in_page = bit_in_page / 8;
        let bit_in_byte = bit_in_page % 8;

        let page_id = self.bitmap_pages[page_idx];
        let page = self.pager.read_page(page_id)?;

        Ok((page.data()[byte_in_page] & (1 << bit_in_byte)) != 0)
    }

    /// Get the root page ID.
    pub fn root_page_id(&self) -> PageId {
        self.root_page_id
    }
}

// =============================================================================
// Table trait implementation
// =============================================================================

impl<FS: FileSystem> Table for PagedBloomFilter<FS> {
    fn table_id(&self) -> TableId {
        self.table_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> TableEngineKind {
        TableEngineKind::Bloom
    }

    fn capabilities(&self) -> crate::table::TableCapabilities {
        crate::table::TableCapabilities {
            ordered: false,
            point_lookup: true, // Can check membership
            prefix_scan: false,
            reverse_scan: false,
            range_delete: false,
            merge_operator: false,
            mvcc_native: false,
            append_optimized: false,
            memory_resident: false,
            disk_resident: true,
            supports_compression: false,
            supports_encryption: false,
        }
    }

    fn stats(&self) -> TableResult<crate::table::TableStatistics> {
        let size_bytes = self.bitmap_pages.len() * self.pager.page_size().to_u32() as usize;
        Ok(crate::table::TableStatistics {
            row_count: Some(*self.num_items.read().unwrap() as u64),
            total_size_bytes: Some(size_bytes as u64),
            key_stats: None,
            value_stats: None,
            histogram: None,
            last_updated_lsn: None,
        })
    }
}

// =============================================================================
// ApproximateMembership trait implementation
// =============================================================================

impl<FS: FileSystem> ApproximateMembership for PagedBloomFilter<FS> {
    fn table_id(&self) -> TableId {
        self.table_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> SpecialtyTableCapabilities {
        SpecialtyTableCapabilities {
            exact: false,
            approximate: true,
            ordered: false,
            sparse: false,
            supports_delete: false,
            supports_range_query: false,
            supports_prefix_query: false,
            supports_scoring: false,
            supports_incremental_rebuild: false,
            may_be_stale: false,
        }
    }

    fn insert_key(
        &mut self,
        key: &[u8],
        _tx_id: crate::txn::TransactionId,
        _commit_lsn: crate::wal::LogSequenceNumber,
    ) -> TableResult<()> {
        self.insert(key)
    }

    fn might_contain(&self, key: &[u8]) -> TableResult<bool> {
        self.contains(key)
    }

    fn false_positive_rate(&self) -> Option<f64> {
        Some(PagedBloomFilter::false_positive_rate(self))
    }

    fn stats(&self) -> TableResult<SpecialtyTableStats> {
        let size_bytes = self.bitmap_pages.len() * self.pager.page_size().to_u32() as usize;
        Ok(SpecialtyTableStats {
            entry_count: Some(*self.num_items.read().unwrap() as u64),
            size_bytes: Some(size_bytes as u64),
            distinct_keys: Some(*self.num_items.read().unwrap() as u64),
            stale_entries: Some(0),
            last_updated_lsn: None,
        })
    }

    fn verify(&self) -> TableResult<VerificationReport> {
        let mut report = VerificationReport {
            checked_items: 1,
            errors: Vec::new(),
            warnings: Vec::new(),
        };

        // Verify metadata consistency
        let expected_pages = self.num_bits.div_ceil(self.bits_per_page);
        if self.bitmap_pages.len() != expected_pages {
            report.errors.push(crate::table::ConsistencyError {
                error_type: crate::table::ConsistencyErrorType::InvalidPointer,
                location: "bloom_filter_metadata".to_string(),
                description: format!(
                    "Bitmap page count mismatch: expected {}, got {}",
                    expected_pages,
                    self.bitmap_pages.len()
                ),
                severity: crate::table::Severity::Error,
            });
        }

        // Warn if false positive rate is high
        if *self.num_items.read().unwrap() > 0 {
            let fpr = self.false_positive_rate();
            if fpr > 0.1 {
                report.warnings.push(crate::table::ConsistencyWarning {
                    location: "bloom_filter".to_string(),
                    description: format!(
                        "High false positive rate: {:.2}% (consider increasing bits_per_key)",
                        fpr * 100.0
                    ),
                });
            }
        }

        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pager::{PageSize, PagerConfig};
    use crate::vfs::MemoryFileSystem;

    fn create_test_pager() -> Arc<Pager<MemoryFileSystem>> {
        let fs = Arc::new(MemoryFileSystem::new());
        let pager = Pager::create(
            fs.as_ref(),
            "test.db",
            PagerConfig::new().with_page_size(PageSize::Size4KB),
        )
        .unwrap();
        Arc::new(pager)
    }

    #[test]
    fn test_create_and_insert() {
        let pager = create_test_pager();
        let filter = PagedBloomFilter::new(
            TableId::from(1),
            "test_bloom".to_string(),
            pager,
            100,
            10,
            None,
        )
        .unwrap();

        filter.insert(b"key1").unwrap();
        filter.insert(b"key2").unwrap();
        filter.insert(b"key3").unwrap();

        assert!(filter.contains(b"key1").unwrap());
        assert!(filter.contains(b"key2").unwrap());
        assert!(filter.contains(b"key3").unwrap());
    }

    #[test]
    fn test_persistence() {
        let pager = create_test_pager();
        let table_id = TableId::from(1);
        let name = "test_bloom".to_string();

        // Create and populate filter
        let root_page_id = {
            let filter =
                PagedBloomFilter::new(table_id, name.clone(), pager.clone(), 100, 10, None)
                    .unwrap();

            filter.insert(b"key1").unwrap();
            filter.insert(b"key2").unwrap();

            filter.root_page_id()
        };

        // Reopen and verify
        let filter = PagedBloomFilter::open(table_id, name, pager, root_page_id).unwrap();

        assert!(filter.contains(b"key1").unwrap());
        assert!(filter.contains(b"key2").unwrap());
        assert_eq!(*filter.num_items.read().unwrap(), 2);
    }

    #[test]
    fn test_false_positive_rate() {
        let pager = create_test_pager();
        let num_items = 1000;
        let filter = PagedBloomFilter::new(
            TableId::from(1),
            "test_bloom".to_string(),
            pager,
            num_items,
            10,
            None,
        )
        .unwrap();

        // Insert items
        for i in 0..num_items {
            filter.insert(&i.to_le_bytes()).unwrap();
        }

        // Check false positive rate
        let mut false_positives = 0;
        let test_items = 10000;
        for i in num_items..(num_items + test_items) {
            if filter.contains(&i.to_le_bytes()).unwrap() {
                false_positives += 1;
            }
        }

        let actual_fpr = false_positives as f64 / test_items as f64;

        // With 10 bits per key, expect roughly 1% FPR
        assert!(
            actual_fpr < 0.05,
            "Actual FPR ({:.4}) should be less than 5%",
            actual_fpr
        );
    }

    #[test]
    fn test_clear() {
        let pager = create_test_pager();
        let filter = PagedBloomFilter::new(
            TableId::from(1),
            "test_bloom".to_string(),
            pager,
            100,
            10,
            None,
        )
        .unwrap();

        filter.insert(b"key1").unwrap();
        assert!(filter.contains(b"key1").unwrap());

        filter.clear().unwrap();
        assert_eq!(*filter.num_items.read().unwrap(), 0);
    }
}

// Made with Bob
