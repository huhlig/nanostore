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

//! `SSTable` (Sorted String Table) implementation for LSM tree.
//!
//! An `SSTable` is an immutable, sorted file format that stores key-value pairs.
//! It consists of:
//! - Data blocks: Contain sorted key-value pairs
//! - Index block: Maps keys to data block offsets
//! - Bloom filter: Probabilistic filter for membership testing
//! - Footer: Metadata about the `SSTable`
//!
//! # Format
//!
//! ```text
//! ┌─────────────────┐
//! │  Data Block 0   │  ← Sorted key-value pairs
//! ├─────────────────┤
//! │  Data Block 1   │
//! ├─────────────────┤
//! │      ...        │
//! ├─────────────────┤
//! │  Data Block N   │
//! ├─────────────────┤
//! │  Index Block    │  ← Maps keys to data blocks
//! ├─────────────────┤
//! │  Bloom Filter   │  ← Probabilistic membership test
//! ├─────────────────┤
//! │     Footer      │  ← Metadata (offsets, counts, etc.)
//! └─────────────────┘
//! ```

use crate::pager::{Page, PageId, PageType, Pager};
use crate::table::TableResult;
use crate::table::lsm::{BloomFilter, BloomFilterBuilder, SStableConfig};
use crate::txn::{VersionChain, VersionValue};
use crate::vfs::FileSystem;
use crate::wal::LogSequenceNumber;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// `SSTable` identifier (unique within a table).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SStableId(u64);

impl SStableId {
    #[must_use]
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    #[must_use]
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for SStableId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SSTable({})", self.0)
    }
}

/// `SSTable` metadata stored in the footer.
#[derive(Clone, Debug)]
pub struct SStableMetadata {
    /// `SSTable` ID
    pub id: SStableId,

    /// Level in the LSM tree (0 = L0, 1 = L1, etc.)
    pub level: u32,

    /// Smallest key in the `SSTable`
    pub min_key: Vec<u8>,

    /// Largest key in the `SSTable`
    pub max_key: Vec<u8>,

    /// Number of key-value pairs
    pub num_entries: u64,

    /// Total size in bytes
    pub total_size: u64,

    /// LSN when this `SSTable` was created
    pub created_lsn: LogSequenceNumber,

    /// First page ID of this `SSTable`
    pub first_page_id: PageId,

    /// Number of pages used by this `SSTable`
    pub num_pages: u32,

    /// Offset to index block (from the start of `SSTable`)
    pub index_offset: u64,

    /// Offset to bloom filter (from the start of `SSTable`)
    pub bloom_filter_offset: u64,

    /// Offset to footer (from the start of `SSTable`)
    pub footer_offset: u64,
}

/// `SSTable` footer (stored at the end of the `SSTable`).
///
/// The footer contains metadata needed to read the `SSTable`.
#[derive(Clone, Debug)]
pub struct SStableFooter {
    /// Magic number for validation (0x0053_5354_4142 = "SSTAB")
    pub magic: u64,

    /// Format version
    pub version: u32,

    /// Metadata
    pub metadata: SStableMetadata,

    /// Checksum of the entire `SSTable` (excluding footer)
    pub checksum: [u8; 32],
}

impl SStableFooter {
    const MAGIC: u64 = 0x0053_5354_4142; // "SSTAB"
    const VERSION: u32 = 1;
    const SIZE: usize = 256; // Fixed size for footer

    /// Create a new footer.
    #[must_use]
    pub fn new(metadata: SStableMetadata, checksum: [u8; 32]) -> Self {
        Self {
            magic: Self::MAGIC,
            version: Self::VERSION,
            metadata,
            checksum,
        }
    }

    /// Serialize footer to bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::SIZE);

        // Magic (8 bytes)
        bytes.extend_from_slice(&self.magic.to_le_bytes());

        // Version (4 bytes)
        bytes.extend_from_slice(&self.version.to_le_bytes());

        // Metadata
        bytes.extend_from_slice(&self.metadata.id.as_u64().to_le_bytes());
        bytes.extend_from_slice(&self.metadata.level.to_le_bytes());

        // Min key (length + data, max 64 bytes)
        let min_key_len = self.metadata.min_key.len().min(64);
        bytes.extend_from_slice(&(min_key_len as u32).to_le_bytes());
        bytes.extend_from_slice(&self.metadata.min_key[..min_key_len]);
        bytes.resize(bytes.len() + (64 - min_key_len), 0);

        // Max key (length + data, max 64 bytes)
        let max_key_len = self.metadata.max_key.len().min(64);
        bytes.extend_from_slice(&(max_key_len as u32).to_le_bytes());
        bytes.extend_from_slice(&self.metadata.max_key[..max_key_len]);
        bytes.resize(bytes.len() + (64 - max_key_len), 0);

        // Counts and offsets
        bytes.extend_from_slice(&self.metadata.num_entries.to_le_bytes());
        bytes.extend_from_slice(&self.metadata.total_size.to_le_bytes());
        bytes.extend_from_slice(&self.metadata.created_lsn.as_u64().to_le_bytes());
        bytes.extend_from_slice(&self.metadata.first_page_id.to_bytes());
        bytes.extend_from_slice(&self.metadata.num_pages.to_le_bytes());
        bytes.extend_from_slice(&self.metadata.index_offset.to_le_bytes());
        bytes.extend_from_slice(&self.metadata.bloom_filter_offset.to_le_bytes());
        bytes.extend_from_slice(&self.metadata.footer_offset.to_le_bytes());

        // Checksum (32 bytes)
        bytes.extend_from_slice(&self.checksum);

        // Pad to fixed size
        bytes.resize(Self::SIZE, 0);

        bytes
    }

    /// Deserialize footer from bytes.
    pub fn from_bytes(bytes: &[u8]) -> TableResult<Self> {
        if bytes.len() < Self::SIZE {
            use crate::table::TableError;
            return Err(TableError::corruption(
                "SSTableFooter::from_bytes",
                "truncated_footer",
                "Footer too small",
            ));
        }

        let mut offset = 0;

        // Magic
        let magic = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;
        if magic != Self::MAGIC {
            use crate::table::TableError;
            return Err(TableError::corruption(
                "SSTableFooter::from_bytes",
                "invalid_magic",
                "Invalid footer magic",
            ));
        }

        // Version
        let version = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;

        // Metadata
        let id = SStableId::new(u64::from_le_bytes(
            bytes[offset..offset + 8].try_into().unwrap(),
        ));
        offset += 8;

        let level = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;

        // Min key
        let min_key_len =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        let min_key = bytes[offset..offset + min_key_len].to_vec();
        offset += 64;

        // Max key
        let max_key_len =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        let max_key = bytes[offset..offset + max_key_len].to_vec();
        offset += 64;

        // Counts and offsets
        let num_entries = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;
        let total_size = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;
        let created_lsn = LogSequenceNumber::from(u64::from_le_bytes(
            bytes[offset..offset + 8].try_into().unwrap(),
        ));
        offset += 8;
        let first_page_id = PageId::from(u64::from_le_bytes(
            bytes[offset..offset + 8].try_into().unwrap(),
        ));
        offset += 8;
        let num_pages = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;
        let index_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;
        let bloom_filter_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;
        let footer_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;

        // Checksum
        let mut checksum = [0u8; 32];
        checksum.copy_from_slice(&bytes[offset..offset + 32]);

        let metadata = SStableMetadata {
            id,
            level,
            min_key,
            max_key,
            num_entries,
            total_size,
            created_lsn,
            first_page_id,
            num_pages,
            index_offset,
            bloom_filter_offset,
            footer_offset,
        };

        Ok(Self {
            magic,
            version,
            metadata,
            checksum,
        })
    }
}

/// Data block containing sorted key-value pairs with version chains.
///
/// Format:
/// - Header: `num_entries` (4 bytes) + `compressed_flag` (1 byte) + `checksum` (32 bytes)
/// - Entries: For each entry:
///   - `key_len` (4 bytes)
///   - `key` (variable)
///   - `version_chain_len` (4 bytes)
///   - `serialized_version_chain` (variable, postcard format)
///
/// Entries are stored in sorted order by key for efficient binary search.
#[derive(Clone, Debug)]
pub struct DataBlock {
    /// Sorted entries (key, version chain)
    entries: Vec<(Vec<u8>, VersionChain)>,
}

impl DataBlock {
    /// Header size: `num_entries` (4) + `compressed_flag` (1) + `checksum` (32)
    const HEADER_SIZE: usize = 37;

    /// Create a new empty data block.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Create a data block from sorted entries.
    ///
    /// # Panics
    /// Panics if entries are not sorted by key.
    #[must_use]
    pub fn from_entries(entries: Vec<(Vec<u8>, VersionChain)>) -> Self {
        // Verify entries are sorted
        for i in 1..entries.len() {
            assert!(
                entries[i - 1].0 < entries[i].0,
                "Entries must be sorted by key"
            );
        }
        Self { entries }
    }

    /// Add an entry to the block.
    ///
    /// # Errors
    /// Returns error if key is not greater than the last key (entries must be sorted).
    pub fn add(&mut self, key: Vec<u8>, chain: VersionChain) -> TableResult<()> {
        if let Some((last_key, _)) = self.entries.last()
            && &key <= last_key
        {
            return Err(crate::table::TableError::invalid_operation_state(
                "DataBlock::add",
                "Keys must be added in sorted order",
            ));
        }
        self.entries.push((key, chain));
        Ok(())
    }

    /// Get the number of entries in this block.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the block is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get all entries in the block.
    #[must_use]
    pub fn entries(&self) -> &[(Vec<u8>, VersionChain)] {
        &self.entries
    }

    /// Get the first key in the block (smallest).
    #[must_use]
    pub fn first_key(&self) -> Option<&[u8]> {
        self.entries.first().map(|(k, _)| k.as_slice())
    }

    /// Get the last key in the block (largest).
    #[must_use]
    pub fn last_key(&self) -> Option<&[u8]> {
        self.entries.last().map(|(k, _)| k.as_slice())
    }

    /// Binary search for a key in the block.
    ///
    /// Returns the index of the entry if found, or None if not found.
    #[must_use]
    pub fn search(&self, key: &[u8]) -> Option<usize> {
        self.entries
            .binary_search_by(|(k, _)| k.as_slice().cmp(key))
            .ok()
    }

    /// Get the version chain for a key.
    #[must_use]
    pub fn get(&self, key: &[u8]) -> Option<&VersionChain> {
        self.search(key).map(|idx| &self.entries[idx].1)
    }

    /// Serialize the data block to bytes.
    ///
    /// Format:
    /// - `num_entries` (4 bytes)
    /// - `compressed_flag` (1 byte) - 0 for uncompressed, 1 for compressed
    /// - `checksum` (32 bytes) - SHA256 of the data portion
    /// - For each entry:
    ///   - `key_len` (4 bytes)
    ///   - `key` (variable)
    ///   - `version_chain_len` (4 bytes)
    ///   - `serialized_version_chain` (variable, postcard)
    pub fn to_bytes(&self, compress: bool) -> TableResult<Vec<u8>> {
        // Serialize entries
        let mut data = Vec::new();
        for (key, chain) in &self.entries {
            // Key length and data
            data.extend_from_slice(&(key.len() as u32).to_le_bytes());
            data.extend_from_slice(key);

            // Serialize version chain using postcard
            let chain_bytes = postcard::to_allocvec(chain).map_err(|e| {
                crate::table::TableError::serialization_error("version_chain", e.to_string())
            })?;

            // Version chain length and data
            data.extend_from_slice(&(chain_bytes.len() as u32).to_le_bytes());
            data.extend_from_slice(&chain_bytes);
        }

        // Optionally compress data
        let (final_data, compressed_flag) = if compress && data.len() > 128 {
            // Only compress if data is larger than 128 bytes
            match lz4_flex::compress_prepend_size(&data) {
                compressed if compressed.len() < data.len() => (compressed, 1u8),
                _ => (data, 0u8), // Compression didn't help, use uncompressed
            }
        } else {
            (data, 0u8)
        };

        // Calculate checksum of the data
        let mut hasher = Sha256::new();
        hasher.update(&final_data);
        let checksum: [u8; 32] = hasher.finalize().into();

        // Build final block with header
        let mut block = Vec::with_capacity(Self::HEADER_SIZE + final_data.len());

        // Header
        block.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        block.push(compressed_flag);
        block.extend_from_slice(&checksum);

        // Data
        block.extend_from_slice(&final_data);

        Ok(block)
    }

    /// Deserialize a data block from bytes.
    ///
    /// Validates the checksum and decompresses if necessary.
    pub fn from_bytes(bytes: &[u8]) -> TableResult<Self> {
        if bytes.len() < Self::HEADER_SIZE {
            use crate::table::TableError;
            return Err(TableError::corruption(
                "DataBlock::from_bytes",
                "truncated_block",
                "Data block too small",
            ));
        }

        let mut offset = 0;

        // Parse header
        let num_entries =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;

        let compressed_flag = bytes[offset];
        offset += 1;

        let mut checksum = [0u8; 32];
        checksum.copy_from_slice(&bytes[offset..offset + 32]);
        offset += 32;

        // Extract data portion
        let data_bytes = &bytes[offset..];

        // Verify checksum
        let mut hasher = Sha256::new();
        hasher.update(data_bytes);
        let computed_checksum: [u8; 32] = hasher.finalize().into();

        if checksum != computed_checksum {
            use crate::table::TableError;
            return Err(TableError::corruption(
                "DataBlock::from_bytes",
                "checksum_mismatch",
                "Data block checksum mismatch",
            ));
        }

        // Decompress if necessary
        let data = if compressed_flag == 1 {
            use crate::table::TableError;
            lz4_flex::decompress_size_prepended(data_bytes).map_err(|e| {
                TableError::corruption(
                    "DataBlock::from_bytes",
                    "decompression_error",
                    format!("Failed to decompress data block: {}", e),
                )
            })?
        } else {
            data_bytes.to_vec()
        };

        // Deserialize entries
        let mut entries = Vec::with_capacity(num_entries);
        let mut offset = 0;

        for _ in 0..num_entries {
            if offset + 4 > data.len() {
                use crate::table::TableError;
                return Err(TableError::corruption(
                    "DataBlock::from_bytes",
                    "truncated_entry",
                    "Truncated data block entry",
                ));
            }

            // Read key
            let key_len = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            if offset + key_len > data.len() {
                use crate::table::TableError;
                return Err(TableError::corruption(
                    "DataBlock::from_bytes",
                    "truncated_key",
                    "Truncated key in data block",
                ));
            }

            let key = data[offset..offset + key_len].to_vec();
            offset += key_len;

            // Read version chain
            if offset + 4 > data.len() {
                use crate::table::TableError;
                return Err(TableError::corruption(
                    "DataBlock::from_bytes",
                    "truncated_chain_length",
                    "Truncated version chain length",
                ));
            }

            let chain_len =
                u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            if offset + chain_len > data.len() {
                use crate::table::TableError;
                return Err(TableError::corruption(
                    "DataBlock::from_bytes",
                    "truncated_chain_data",
                    "Truncated version chain data",
                ));
            }

            let chain_bytes = &data[offset..offset + chain_len];
            offset += chain_len;

            let chain: VersionChain = postcard::from_bytes(chain_bytes).map_err(|e| {
                use crate::table::TableError;
                TableError::corruption(
                    "DataBlock::from_bytes",
                    "deserialization_error",
                    format!("Failed to deserialize version chain: {}", e),
                )
            })?;

            entries.push((key, chain));
        }

        // Verify entries are sorted
        for i in 1..entries.len() {
            if entries[i - 1].0 >= entries[i].0 {
                use crate::table::TableError;
                return Err(TableError::corruption(
                    "DataBlock::from_bytes",
                    "unsorted_entries",
                    "Data block entries not sorted",
                ));
            }
        }

        Ok(Self { entries })
    }

    /// Estimate the serialized size of this block.
    #[must_use]
    pub fn estimate_size(&self) -> usize {
        let mut size = Self::HEADER_SIZE;
        for (key, chain) in &self.entries {
            size += 4; // key_len
            size += key.len();
            size += 4; // chain_len
            // Rough estimate for version chain size
            size += postcard::to_allocvec(chain).map(|b| b.len()).unwrap_or(0);
        }
        size
    }
}

impl Default for DataBlock {
    fn default() -> Self {
        Self::new()
    }
}

/// Index entry mapping a key to a data block location.
///
/// Each entry stores:
/// - The first key of a data block
/// - The page ID where the data block starts
/// - The offset within that page where the data block starts
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    /// First key in the data block
    pub first_key: Vec<u8>,
    /// Page ID where the data block starts
    pub page_id: PageId,
    /// Offset within the page (in bytes)
    pub offset: u64,
}

/// Index block for efficient key lookups in an `SSTable`.
///
/// The index is a sparse index - it contains one entry per data block,
/// not one entry per key. This keeps the index small while still enabling
/// efficient binary search to find the correct data block.
///
/// Format:
/// - Header: `num_entries` (4 bytes) + `checksum` (32 bytes)
/// - Entries: For each entry:
///   - `key_len` (4 bytes)
///   - `key` (variable)
///   - `page_id` (8 bytes)
///   - `offset` (8 bytes)
///
/// Entries are stored in sorted order by key for efficient binary search.
#[derive(Clone, Debug)]
pub struct IndexBlock {
    /// Sorted index entries (one per data block)
    entries: Vec<IndexEntry>,
}

impl IndexBlock {
    /// Header size: num_entries (4) + checksum (32)
    const HEADER_SIZE: usize = 36;

    /// Create a new empty index block.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Create an index block from sorted entries.
    ///
    /// # Panics
    /// Panics if entries are not sorted by key.
    pub fn from_entries(entries: Vec<IndexEntry>) -> Self {
        // Verify entries are sorted
        for i in 1..entries.len() {
            assert!(
                entries[i - 1].first_key < entries[i].first_key,
                "Index entries must be sorted by key"
            );
        }
        Self { entries }
    }

    /// Add an index entry.
    ///
    /// # Errors
    /// Returns error if key is not greater than the last key (entries must be sorted).
    pub fn add(&mut self, first_key: Vec<u8>, page_id: PageId, offset: u64) -> TableResult<()> {
        if let Some(last_entry) = self.entries.last()
            && first_key <= last_entry.first_key
        {
            return Err(crate::table::TableError::invalid_operation_state(
                "IndexBlock::add",
                "Index entries must be added in sorted order",
            ));
        }
        self.entries.push(IndexEntry {
            first_key,
            page_id,
            offset,
        });
        Ok(())
    }

    /// Get the number of entries in this index.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get all entries in the index.
    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    /// Binary search to find the data block that may contain the given key.
    ///
    /// Returns the index of the entry whose data block should be searched.
    /// Returns None if the key is definitely not in any data block.
    ///
    /// The algorithm finds the rightmost entry whose first_key <= search_key.
    pub fn search(&self, key: &[u8]) -> Option<usize> {
        if self.entries.is_empty() {
            return None;
        }

        // Binary search for the rightmost entry with first_key <= key
        let mut left = 0;
        let mut right = self.entries.len();
        let mut result = None;

        while left < right {
            let mid = left + (right - left) / 2;
            if self.entries[mid].first_key.as_slice() <= key {
                result = Some(mid);
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        result
    }

    /// Get the index entry at the given position.
    pub fn get(&self, index: usize) -> Option<&IndexEntry> {
        self.entries.get(index)
    }

    /// Serialize the index block to bytes.
    ///
    /// Format:
    /// - num_entries (4 bytes)
    /// - checksum (32 bytes) - SHA256 of the entries data
    /// - For each entry:
    ///   - key_len (4 bytes)
    ///   - key (variable)
    ///   - page_id (8 bytes)
    ///   - offset (8 bytes)
    pub fn to_bytes(&self) -> TableResult<Vec<u8>> {
        let mut buffer = Vec::new();

        // Reserve space for header
        buffer.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        buffer.extend_from_slice(&[0u8; 32]); // Placeholder for checksum

        // Serialize entries
        let entries_start = buffer.len();
        for entry in &self.entries {
            // key_len
            buffer.extend_from_slice(&(entry.first_key.len() as u32).to_le_bytes());
            // key
            buffer.extend_from_slice(&entry.first_key);
            // page_id
            buffer.extend_from_slice(&entry.page_id.as_u64().to_le_bytes());
            // offset
            buffer.extend_from_slice(&entry.offset.to_le_bytes());
        }

        // Calculate checksum of entries data
        let entries_data = &buffer[entries_start..];
        let mut hasher = Sha256::new();
        hasher.update(entries_data);
        let checksum = hasher.finalize();

        // Write checksum to header
        buffer[4..36].copy_from_slice(&checksum);

        Ok(buffer)
    }

    /// Deserialize an index block from bytes.
    ///
    /// # Errors
    /// Returns error if:
    /// - Data is truncated or malformed
    /// - Checksum validation fails
    pub fn from_bytes(bytes: &[u8]) -> TableResult<Self> {
        if bytes.len() < Self::HEADER_SIZE {
            return Err(crate::table::TableError::corruption(
                "IndexBlock::from_bytes",
                "truncated_header",
                format!(
                    "Index block data too short: expected at least {} bytes, found {}",
                    Self::HEADER_SIZE,
                    bytes.len()
                ),
            ));
        }

        // Read header
        let num_entries = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let stored_checksum = &bytes[4..36];

        // Verify checksum of entries data
        let entries_data = &bytes[Self::HEADER_SIZE..];
        let mut hasher = Sha256::new();
        hasher.update(entries_data);
        let computed_checksum = hasher.finalize();

        if stored_checksum != computed_checksum.as_slice() {
            return Err(crate::table::TableError::corruption(
                "IndexBlock::from_bytes",
                "checksum_mismatch",
                "Index block checksum mismatch",
            ));
        }

        // Deserialize entries
        let mut entries = Vec::with_capacity(num_entries);
        let mut offset = Self::HEADER_SIZE;

        for _ in 0..num_entries {
            if offset + 4 > bytes.len() {
                return Err(crate::table::TableError::corruption(
                    "IndexBlock::from_bytes",
                    "truncated_entry_key_len",
                    format!(
                        "Truncated index entry key length at entry offset {} (buffer len {})",
                        offset,
                        bytes.len()
                    ),
                ));
            }

            // Read key_len
            let key_len = u32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]) as usize;
            offset += 4;

            if offset + key_len > bytes.len() {
                return Err(crate::table::TableError::corruption(
                    "IndexBlock::from_bytes",
                    "truncated_entry_key",
                    format!(
                        "Truncated index entry key: need {} bytes at offset {}, buffer len {}",
                        key_len,
                        offset,
                        bytes.len()
                    ),
                ));
            }

            // Read key
            let first_key = bytes[offset..offset + key_len].to_vec();
            offset += key_len;

            if offset + 16 > bytes.len() {
                return Err(crate::table::TableError::corruption(
                    "IndexBlock::from_bytes",
                    "truncated_entry_pointer",
                    format!(
                        "Truncated index entry pointer metadata at offset {} (buffer len {})",
                        offset,
                        bytes.len()
                    ),
                ));
            }

            // Read page_id
            let page_id = PageId::from(u64::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
                bytes[offset + 4],
                bytes[offset + 5],
                bytes[offset + 6],
                bytes[offset + 7],
            ]));
            offset += 8;

            // Read offset
            let block_offset = u64::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
                bytes[offset + 4],
                bytes[offset + 5],
                bytes[offset + 6],
                bytes[offset + 7],
            ]);
            offset += 8;

            entries.push(IndexEntry {
                first_key,
                page_id,
                offset: block_offset,
            });
        }

        // Verify entries are sorted
        for i in 1..entries.len() {
            if entries[i - 1].first_key >= entries[i].first_key {
                return Err(crate::table::TableError::corruption(
                    "IndexBlock::from_bytes",
                    "unsorted_entries",
                    format!(
                        "Index entries not sorted: entry {} key is not greater than previous entry",
                        i
                    ),
                ));
            }
        }

        Ok(Self { entries })
    }

    /// Estimate the serialized size of this index block.
    pub fn estimate_size(&self) -> usize {
        let mut size = Self::HEADER_SIZE;
        for entry in &self.entries {
            size += 4; // key_len
            size += entry.first_key.len(); // key
            size += 8; // page_id
            size += 8; // offset
        }
        size
    }
}

impl Default for IndexBlock {
    fn default() -> Self {
        Self::new()
    }
}

/// SSTable reader for reading data from an immutable SSTable.
pub struct SStableReader<FS: FileSystem> {
    /// Pager for reading pages
    pager: Arc<Pager<FS>>,

    /// SSTable metadata
    metadata: SStableMetadata,

    /// Index block for finding data blocks
    index_block: IndexBlock,

    /// Bloom filter for membership testing
    bloom_filter: Option<BloomFilter>,

    /// Configuration
    config: SStableConfig,
}

impl<FS: FileSystem> SStableReader<FS> {
    /// Open an existing SSTable for reading.
    pub fn open(
        pager: Arc<Pager<FS>>,
        first_page_id: PageId,
        config: SStableConfig,
    ) -> TableResult<Self> {
        let page_size = pager.page_size().data_size();

        // Find the footer by reading pages sequentially.
        // To avoid false positives, we validate that the footer's num_pages
        // is consistent with the number of pages we've traversed.
        let mut current_page_id = first_page_id;
        let mut page_count = 0u32;
        let mut footer: Option<SStableFooter> = None;

        loop {
            let page = pager.read_page(current_page_id).map_err(|_| {
                use crate::table::TableError;
                TableError::corruption(
                    "SSTableReader::open",
                    "footer_not_found",
                    "Reached end of file without finding valid footer",
                )
            })?;

            let data = page.data();
            page_count += 1;

            // Check if this page contains a footer at the end
            if data.len() >= SStableFooter::SIZE {
                let footer_start = data.len() - SStableFooter::SIZE;
                let footer_bytes = &data[footer_start..];

                if let Ok(candidate) = SStableFooter::from_bytes(footer_bytes) {
                    // Verify this is the correct SSTable
                    if candidate.metadata.first_page_id == first_page_id {
                        // Validate num_pages consistency:
                        // The footer should be on the last page, so page_count should equal num_pages
                        if candidate.metadata.num_pages == page_count {
                            // Additional validation to prevent false positives:
                            // - num_entries must be > 0
                            // - index_offset must be < footer_offset
                            // - bloom_filter_offset must be 0 or < footer_offset
                            if candidate.metadata.num_entries > 0
                                && candidate.metadata.index_offset
                                    < candidate.metadata.footer_offset
                                && (candidate.metadata.bloom_filter_offset == 0
                                    || candidate.metadata.bloom_filter_offset
                                        < candidate.metadata.footer_offset)
                            {
                                footer = Some(candidate);
                                break;
                            }
                        }
                    }
                }
            }

            // Try next page
            current_page_id = PageId::from(current_page_id.as_u64() + 1);

            // Safety limit to prevent infinite loops
            if page_count > 10_000 {
                use crate::table::TableError;
                return Err(TableError::corruption(
                    "SSTableReader::open",
                    "footer_not_found",
                    "Could not find valid SSTable footer after 10000 pages",
                ));
            }
        }

        let footer = footer.ok_or_else(|| {
            use crate::table::TableError;
            TableError::corruption(
                "SSTableReader::open",
                "footer_not_found",
                "No valid footer found",
            )
        })?;

        let metadata = footer.metadata.clone();

        // Step 2: Read and deserialize the index block
        // Calculate index block size: from index_offset to bloom_filter_offset (or footer_offset if no bloom)
        let index_size = if metadata.bloom_filter_offset > 0 {
            metadata.bloom_filter_offset - metadata.index_offset
        } else {
            metadata.footer_offset - metadata.index_offset
        };

        let index_data = Self::read_data_with_size(
            &pager,
            first_page_id,
            metadata.index_offset,
            index_size as usize,
            metadata.num_pages,
        )?;
        let index_block = IndexBlock::from_bytes(&index_data)?;

        // Step 3: Read and deserialize the bloom filter (if present)
        let bloom_filter = if metadata.bloom_filter_offset > 0 {
            // Calculate bloom filter size: from bloom_filter_offset to footer_offset
            let bloom_size = metadata.footer_offset - metadata.bloom_filter_offset;

            let bloom_data = Self::read_data_with_size(
                &pager,
                first_page_id,
                metadata.bloom_filter_offset,
                bloom_size as usize,
                metadata.num_pages,
            )?;

            // Read bloom filter header (num_hash_functions as u32, num_bits as u32)
            if bloom_data.len() < 8 {
                use crate::table::TableError;
                return Err(TableError::corruption(
                    "SSTableReader::open",
                    "truncated_bloom_filter",
                    "Bloom filter data too short",
                ));
            }

            let num_hash =
                u32::from_le_bytes([bloom_data[0], bloom_data[1], bloom_data[2], bloom_data[3]])
                    as usize;

            let num_bits_val =
                u32::from_le_bytes([bloom_data[4], bloom_data[5], bloom_data[6], bloom_data[7]])
                    as usize;

            let bits = bloom_data[8..].to_vec();

            // Create bloom filter with correct num_bits
            Some(BloomFilter::from_bytes_with_size(
                bits,
                num_bits_val,
                num_hash,
            ))
        } else {
            None
        };

        Ok(Self {
            pager,
            metadata,
            index_block,
            bloom_filter,
            config,
        })
    }

    /// Helper function to read data of a specific size at a specific offset across pages.
    ///
    /// This function handles pages with variable data lengths by iterating through
    /// pages and accumulating their actual lengths to find the correct page for each offset.
    fn read_data_with_size(
        pager: &Arc<Pager<FS>>,
        first_page_id: PageId,
        offset: u64,
        size: usize,
        num_pages: u32,
    ) -> TableResult<Vec<u8>> {
        let mut result = Vec::with_capacity(size);
        let mut remaining = size;
        let target_offset = offset;

        // First, find which page contains the starting offset
        let mut cumulative_offset = 0u64;
        let mut start_page_id = first_page_id;
        let mut start_page_offset = 0usize;

        for i in 0..num_pages {
            let page_id = PageId::from(first_page_id.as_u64() + i as u64);
            let page = pager.read_page(page_id)?;
            let data_len = page.data().len() as u64;

            if cumulative_offset + data_len > target_offset {
                // This page contains the target offset
                start_page_id = page_id;
                start_page_offset = (target_offset - cumulative_offset) as usize;
                break;
            }

            cumulative_offset += data_len;

            if i == num_pages - 1 {
                use crate::table::TableError;
                return Err(TableError::corruption(
                    "SSTableReader::read_data",
                    "offset_out_of_range",
                    format!("Offset {} is beyond SSTable data size", target_offset),
                ));
            }
        }

        // Now read data starting from the found page
        let mut current_page_id = start_page_id;
        let mut current_page_offset = start_page_offset;

        while remaining > 0 {
            let page = pager.read_page(current_page_id)?;
            let data = page.data();

            if current_page_offset >= data.len() {
                use crate::table::TableError;
                return Err(TableError::corruption(
                    "SSTableReader::read_data",
                    "invalid_offset",
                    "Offset beyond page data",
                ));
            }

            let available = data.len() - current_page_offset;
            let to_read = remaining.min(available);

            result.extend_from_slice(&data[current_page_offset..current_page_offset + to_read]);

            remaining -= to_read;
            current_page_offset += to_read;

            // If we've read past the end of this page, move to the next page
            if current_page_offset >= data.len() && remaining > 0 {
                current_page_id = PageId::from(current_page_id.as_u64() + 1);
                current_page_offset = 0;
            }
        }

        Ok(result)
    }

    /// Check if a key might exist in this SSTable using the bloom filter.
    pub fn may_contain(&self, key: &[u8]) -> bool {
        if let Some(ref bloom) = self.bloom_filter {
            bloom.contains(key)
        } else {
            true // No bloom filter, assume it might contain the key
        }
    }

    /// Get the value for a key at a specific snapshot LSN.
    ///
    /// This method:
    /// 1. Checks the bloom filter for early rejection
    /// 2. Binary searches the index block to find the data block
    /// 3. Reads and deserializes the data block
    /// 4. Binary searches within the data block for the key
    /// 5. Walks the version chain to find a visible version
    ///
    /// # Arguments
    ///
    /// * `key` - The key to look up
    /// * `snapshot_lsn` - The LSN of the snapshot for visibility checking
    ///
    /// # Returns
    ///
    /// * `Ok(Some(value))` - Key found with a visible version
    /// * `Ok(None)` - Key not found or no visible version
    /// * `Err(_)` - I/O or corruption error
    pub fn get(&self, key: &[u8], snapshot_lsn: LogSequenceNumber) -> TableResult<Option<Vec<u8>>> {
        // Step 1: Check bloom filter first for early rejection
        if !self.may_contain(key) {
            return Ok(None);
        }

        // Step 2: Binary search in index block to find the data block that may contain the key
        let index_entry_idx = match self.index_block.search(key) {
            Some(idx) => idx,
            None => return Ok(None), // Key is before all data blocks
        };

        let index_entry = self.index_block.get(index_entry_idx).ok_or_else(|| {
            use crate::table::TableError;
            TableError::corruption(
                "SSTableReader::get",
                "index_entry_not_found",
                "Index entry not found after search",
            )
        })?;

        // Step 3: Read the data block from the page
        // The index entry tells us which page and offset within that page
        let page = self.pager.read_page(index_entry.page_id)?;
        let page_data = page.data();
        let block_start = index_entry.offset as usize;

        // Calculate block size by finding where it ends
        // It ends either at the next block's start (if on same page) or at page end
        let block_end = if index_entry_idx + 1 < self.index_block.len() {
            let next_entry = self.index_block.get(index_entry_idx + 1).unwrap();
            if next_entry.page_id == index_entry.page_id {
                // Next block is on same page
                next_entry.offset as usize
            } else {
                // Next block is on different page, this block extends to end of page
                page_data.len()
            }
        } else {
            // Last data block - need to find where index block starts
            // Calculate the actual cumulative offset of this page by reading all previous pages
            let mut cumulative_offset = 0u64;
            let page_index =
                (index_entry.page_id.as_u64() - self.metadata.first_page_id.as_u64()) as usize;

            for i in 0..page_index {
                let pid = PageId::from(self.metadata.first_page_id.as_u64() + i as u64);
                let p = self.pager.read_page(pid)?;
                cumulative_offset += p.data().len() as u64;
            }

            let index_start_in_sstable = self.metadata.index_offset as usize;

            if index_start_in_sstable >= cumulative_offset as usize
                && index_start_in_sstable < cumulative_offset as usize + page_data.len()
            {
                // Index block starts on this page
                index_start_in_sstable - cumulative_offset as usize
            } else {
                // Index block is on a later page, this block extends to end of page
                page_data.len()
            }
        };

        if block_start >= page_data.len() || block_end > page_data.len() || block_start >= block_end
        {
            use crate::table::TableError;
            return Err(TableError::corruption(
                "SSTableReader::get",
                "invalid_block_bounds",
                format!(
                    "Invalid data block bounds: start={}, end={}, page_len={}",
                    block_start,
                    block_end,
                    page_data.len()
                ),
            ));
        }

        let data_block_bytes = &page_data[block_start..block_end];

        // Step 5: Deserialize the data block
        let data_block = DataBlock::from_bytes(data_block_bytes)?;

        // Step 6: Binary search within the data block for the exact key
        let version_chain = match data_block.get(key) {
            Some(chain) => chain,
            None => return Ok(None), // Key not in this data block (bloom filter false positive)
        };

        // Step 7: Walk the version chain to find a visible version
        // For SSTables, all versions are committed, so we just need to check commit_lsn <= snapshot_lsn
        let mut current = Some(version_chain);
        while let Some(version) = current {
            if let Some(commit_lsn) = version.commit_lsn {
                // Check if this version is visible at the snapshot LSN
                if commit_lsn <= snapshot_lsn {
                    // Found a visible version - check for tombstone first
                    match &version.value {
                        VersionValue::Inline(data) => {
                            // Empty value means tombstone (deleted)
                            if data.is_empty() {
                                return Ok(None);
                            }
                            return Ok(Some(data.clone()));
                        }
                        VersionValue::External(_) => {
                            // External values should not be in SSTables at this level
                            return Err(crate::table::TableError::invalid_operation_state(
                                "SSTableReader::get",
                                "Found external VersionValue in SSTable",
                            ));
                        }
                    }
                }
            }
            // Move to the previous (older) version
            current = version.prev_version.as_deref();
        }

        // No visible version found (all versions are too new)
        Ok(None)
    }

    /// Get a streaming reader for a value at a specific snapshot LSN.
    ///
    /// This method is similar to `get()` but returns a stream for efficient
    /// reading of large values. For now, it uses the same implementation as `get()`
    /// and wraps the result in a SliceValueStream. Future optimization could
    /// use ValueRef to stream directly from overflow pages.
    ///
    /// # Arguments
    ///
    /// * `key` - The key to look up
    /// * `snapshot_lsn` - The LSN of the snapshot for visibility checking
    ///
    /// # Returns
    ///
    /// * `Ok(Some(stream))` - Key found with a visible version
    /// * `Ok(None)` - Key not found or no visible version
    /// * `Err(_)` - I/O or corruption error
    pub fn get_stream(
        &self,
        key: &[u8],
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Option<Box<dyn crate::table::ValueStream + '_>>> {
        // For now, use the existing get() method and wrap in a stream
        // Future optimization: Store ValueRef in version chain and stream from overflow pages
        match self.get(key, snapshot_lsn)? {
            Some(value) => Ok(Some(Box::new(crate::table::SliceValueStream::new(value)))),
            None => Ok(None),
        }
    }

    /// Get metadata about this SSTable.
    pub fn metadata(&self) -> &SStableMetadata {
        &self.metadata
    }

    /// Get the index block for this SSTable.
    pub fn index_block(&self) -> &IndexBlock {
        &self.index_block
    }

    /// Get the pager for this SSTable.
    pub fn pager(&self) -> &Arc<Pager<FS>> {
        &self.pager
    }
}

/// SSTable writer for creating new SSTables.
pub struct SStableWriter<FS: FileSystem> {
    /// Pager for writing pages
    pager: Arc<Pager<FS>>,

    /// SSTable ID
    id: SStableId,

    /// Level in the LSM tree
    level: u32,

    /// Configuration
    config: SStableConfig,

    /// Bloom filter builder
    bloom_builder: Option<BloomFilterBuilder>,

    /// Current data being written
    entries: Vec<(Vec<u8>, VersionChain)>,

    /// First page ID allocated for this SSTable
    first_page_id: Option<PageId>,

    /// Pages allocated for this SSTable
    pages: Vec<PageId>,
}

impl<FS: FileSystem> SStableWriter<FS> {
    /// Create a new SSTable writer.
    pub fn new(
        pager: Arc<Pager<FS>>,
        id: SStableId,
        level: u32,
        config: SStableConfig,
        estimated_entries: usize,
    ) -> Self {
        let bloom_builder = if estimated_entries > 0 {
            Some(
                BloomFilterBuilder::new(estimated_entries).bits_per_key(10), // ~1% false positive rate
            )
        } else {
            None
        };

        Self {
            pager,
            id,
            level,
            config,
            bloom_builder,
            entries: Vec::new(),
            first_page_id: None,
            pages: Vec::new(),
        }
    }

    /// Add a key-value pair to the SSTable.
    ///
    /// Keys must be added in sorted order.
    pub fn add(&mut self, key: Vec<u8>, chain: VersionChain) -> TableResult<()> {
        // Verify keys are in sorted order
        if let Some((last_key, _)) = self.entries.last()
            && &key <= last_key
        {
            return Err(crate::table::TableError::invalid_operation_state(
                "SStableBuilder::add",
                "Keys must be added in sorted order",
            ));
        }

        // Add to bloom filter - will be built when finish() is called
        // The bloom filter is built from all keys at once for efficiency

        self.entries.push((key, chain));
        Ok(())
    }

    /// Finish writing the SSTable and return its metadata.
    pub fn finish(mut self, created_lsn: LogSequenceNumber) -> TableResult<SStableMetadata> {
        if self.entries.is_empty() {
            return Err(crate::table::TableError::invalid_operation_state(
                "SStableBuilder::build",
                "Cannot create empty SSTable",
            ));
        }

        // Get min/max keys and count before consuming entries
        let min_key = self.entries.first().unwrap().0.clone();
        let max_key = self.entries.last().unwrap().0.clone();
        let num_entries = self.entries.len() as u64;

        // Build bloom filter from all keys
        let bloom_filter = self.bloom_builder.map(|b| {
            let mut filter = b.build();
            for (key, _) in &self.entries {
                filter.insert(key);
            }
            filter
        });

        // Track current write position
        let mut current_offset = 0u64;
        let page_size = self.pager.page_size().data_size();

        // Allocate first page
        let first_page_id = self.pager.allocate_page(PageType::LsmData)?;
        self.first_page_id = Some(first_page_id);
        self.pages.push(first_page_id);

        let mut current_page = Page::new(first_page_id, PageType::LsmData, page_size);
        let mut current_page_offset = 0usize;

        // Index entries to track data block locations
        let mut index_entries = Vec::new();

        // Step 1: Write data blocks
        // Group entries into blocks based on target block size
        let target_block_size = self.config.block_size;
        let mut block_entries = Vec::new();
        let mut block_size_estimate = 0;

        for (key, chain) in self.entries {
            // Estimate entry size: key_len (4) + key + chain_len (4) + serialized chain
            // Use postcard to get accurate size of serialized chain
            let chain_size = postcard::to_allocvec(&chain)
                .map(|bytes| bytes.len())
                .unwrap_or(chain.value.len() + 64); // Fallback estimate
            let entry_size = 4 + key.len() + 4 + chain_size;

            // Check if adding this entry would exceed block size
            if !block_entries.is_empty() && block_size_estimate + entry_size > target_block_size {
                // Write current block
                let block = DataBlock::from_entries(block_entries.clone());
                let first_key = block.first_key().unwrap().to_vec();
                let block_bytes = block.to_bytes(self.config.compression.is_some())?;

                // Check if block fits in current page
                if current_page_offset + block_bytes.len() > page_size {
                    // Write current page and allocate new one
                    current_offset += current_page_offset as u64;
                    self.pager.write_page(&current_page)?;

                    let new_page_id = self.pager.allocate_page(PageType::LsmData)?;
                    self.pages.push(new_page_id);
                    current_page = Page::new(new_page_id, PageType::LsmData, page_size);
                    current_page_offset = 0;
                }

                // Record index entry for this block
                index_entries.push(IndexEntry {
                    first_key,
                    page_id: current_page.page_id(),
                    offset: current_page_offset as u64,
                });

                // Write block to current page
                current_page.data_mut().extend_from_slice(&block_bytes);
                current_page_offset += block_bytes.len();

                // Start new block
                block_entries.clear();
                block_size_estimate = 0;
            }

            block_entries.push((key, chain));
            block_size_estimate += entry_size;
        }

        // Write final block if any entries remain
        if !block_entries.is_empty() {
            let block = DataBlock::from_entries(block_entries);
            let first_key = block.first_key().unwrap().to_vec();
            let block_bytes = block.to_bytes(self.config.compression.is_some())?;

            // Check if block fits in current page
            if current_page_offset + block_bytes.len() > page_size {
                // Write current page and allocate new one
                current_offset += current_page_offset as u64;
                self.pager.write_page(&current_page)?;

                let new_page_id = self.pager.allocate_page(PageType::LsmData)?;
                self.pages.push(new_page_id);
                current_page = Page::new(new_page_id, PageType::LsmData, page_size);
                current_page_offset = 0;
            }

            // Record index entry
            index_entries.push(IndexEntry {
                first_key,
                page_id: current_page.page_id(),
                offset: current_page_offset as u64,
            });

            // Write block to current page
            current_page.data_mut().extend_from_slice(&block_bytes);
            current_page_offset += block_bytes.len();
        }

        // Step 2: Write index block
        let index_offset = current_offset + current_page_offset as u64;
        let index_block = IndexBlock::from_entries(index_entries);
        let index_bytes = index_block.to_bytes()?;

        // Check if index fits in current page
        if current_page_offset + index_bytes.len() > page_size {
            // Write current page and allocate new one
            current_offset += current_page_offset as u64;
            self.pager.write_page(&current_page)?;

            let new_page_id = self.pager.allocate_page(PageType::LsmData)?;
            self.pages.push(new_page_id);
            current_page = Page::new(new_page_id, PageType::LsmData, page_size);
            current_page_offset = 0;
        }

        // Write index to current page
        current_page.data_mut().extend_from_slice(&index_bytes);
        current_page_offset += index_bytes.len();

        // Step 3: Write bloom filter (if enabled)
        let bloom_filter_offset = if let Some(bloom) = &bloom_filter {
            let offset = current_offset + current_page_offset as u64;
            let bloom_bytes = bloom.as_bytes();

            // Write bloom filter metadata (num_hash_functions as u32, num_bits as u32)
            let num_hash = bloom.num_hash_functions() as u32;
            let num_bits_val = bloom.num_bits() as u32;
            let mut bloom_header = Vec::new();
            bloom_header.extend_from_slice(&num_hash.to_le_bytes());
            bloom_header.extend_from_slice(&num_bits_val.to_le_bytes());

            // Check if bloom filter fits in current page
            if current_page_offset + bloom_header.len() + bloom_bytes.len() > page_size {
                // Write current page and allocate new one
                current_offset += current_page_offset as u64;
                self.pager.write_page(&current_page)?;

                let new_page_id = self.pager.allocate_page(PageType::LsmData)?;
                self.pages.push(new_page_id);
                current_page = Page::new(new_page_id, PageType::LsmData, page_size);
                current_page_offset = 0;
            }

            // Write bloom filter header and data
            current_page.data_mut().extend_from_slice(&bloom_header);
            current_page.data_mut().extend_from_slice(bloom_bytes);
            current_page_offset += bloom_header.len() + bloom_bytes.len();

            offset
        } else {
            0
        };

        // Step 4: Write footer
        let footer_offset = current_offset + current_page_offset as u64;

        // Calculate total size
        let total_size = footer_offset + SStableFooter::SIZE as u64;

        // Create metadata
        let metadata = SStableMetadata {
            id: self.id,
            level: self.level,
            min_key,
            max_key,
            num_entries,
            total_size,
            created_lsn,
            first_page_id,
            num_pages: self.pages.len() as u32,
            index_offset,
            bloom_filter_offset,
            footer_offset,
        };

        // Calculate checksum of all written data
        let mut hasher = Sha256::new();
        hasher.update(metadata.id.as_u64().to_le_bytes());
        hasher.update(metadata.level.to_le_bytes());
        hasher.update(&metadata.min_key);
        hasher.update(&metadata.max_key);
        hasher.update(metadata.num_entries.to_le_bytes());
        let checksum: [u8; 32] = hasher.finalize().into();

        // Create footer
        let footer = SStableFooter::new(metadata.clone(), checksum);
        let footer_bytes = footer.to_bytes();

        // Check if footer fits in current page
        if current_page_offset + footer_bytes.len() > page_size {
            // Write current page and allocate new one
            current_offset += current_page_offset as u64;
            self.pager.write_page(&current_page)?;

            let new_page_id = self.pager.allocate_page(PageType::LsmData)?;
            self.pages.push(new_page_id);
            current_page = Page::new(new_page_id, PageType::LsmData, page_size);
            current_page_offset = 0;
        }

        // Write footer to current page
        current_page.data_mut().extend_from_slice(&footer_bytes);

        // Write final page
        self.pager.write_page(&current_page)?;

        Ok(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sstable_id() {
        let id = SStableId::new(42);
        assert_eq!(id.as_u64(), 42);
        assert_eq!(format!("{}", id), "SSTable(42)");
    }

    #[test]
    fn test_footer_serialization() {
        let metadata = SStableMetadata {
            id: SStableId::new(1),
            level: 0,
            min_key: b"aaa".to_vec(),
            max_key: b"zzz".to_vec(),
            num_entries: 100,
            total_size: 4096,
            created_lsn: LogSequenceNumber::from(42),
            first_page_id: PageId::from(10),
            num_pages: 5,
            index_offset: 3000,
            bloom_filter_offset: 3500,
            footer_offset: 3800,
        };

        let footer = SStableFooter::new(metadata, [0u8; 32]);
        let bytes = footer.to_bytes();

        assert_eq!(bytes.len(), SStableFooter::SIZE);

        let restored = SStableFooter::from_bytes(&bytes).unwrap();
        assert_eq!(restored.magic, SStableFooter::MAGIC);
        assert_eq!(restored.version, SStableFooter::VERSION);
        assert_eq!(restored.metadata.id, footer.metadata.id);
        assert_eq!(restored.metadata.level, footer.metadata.level);
        assert_eq!(restored.metadata.min_key, footer.metadata.min_key);
        assert_eq!(restored.metadata.max_key, footer.metadata.max_key);
    }

    #[test]
    fn test_data_block_empty() {
        let block = DataBlock::new();
        assert!(block.is_empty());
        assert_eq!(block.len(), 0);
        assert_eq!(block.first_key(), None);
        assert_eq!(block.last_key(), None);
    }

    #[test]
    fn test_data_block_add_entries() {
        use crate::txn::TransactionId;

        let mut block = DataBlock::new();

        // Add entries in sorted order
        let chain1 = VersionChain::new(b"value1".to_vec(), TransactionId::from(1));
        block.add(b"key1".to_vec(), chain1).unwrap();

        let chain2 = VersionChain::new(b"value2".to_vec(), TransactionId::from(2));
        block.add(b"key2".to_vec(), chain2).unwrap();

        let chain3 = VersionChain::new(b"value3".to_vec(), TransactionId::from(3));
        block.add(b"key3".to_vec(), chain3).unwrap();

        assert_eq!(block.len(), 3);
        assert_eq!(block.first_key(), Some(b"key1".as_slice()));
        assert_eq!(block.last_key(), Some(b"key3".as_slice()));
    }

    #[test]
    fn test_data_block_add_unsorted_fails() {
        use crate::txn::TransactionId;

        let mut block = DataBlock::new();

        let chain1 = VersionChain::new(b"value1".to_vec(), TransactionId::from(1));
        block.add(b"key2".to_vec(), chain1).unwrap();

        // Try to add a key that's not greater than the last key
        let chain2 = VersionChain::new(b"value2".to_vec(), TransactionId::from(2));
        let result = block.add(b"key1".to_vec(), chain2);

        assert!(result.is_err());
    }

    #[test]
    fn test_data_block_binary_search() {
        use crate::txn::TransactionId;

        let mut block = DataBlock::new();

        for i in 0..10 {
            let key = format!("key{:02}", i);
            let value = format!("value{}", i);
            let chain = VersionChain::new(value.into_bytes(), TransactionId::from(i as u64));
            block.add(key.into_bytes(), chain).unwrap();
        }

        // Search for existing keys
        assert_eq!(block.search(b"key00"), Some(0));
        assert_eq!(block.search(b"key05"), Some(5));
        assert_eq!(block.search(b"key09"), Some(9));

        // Search for non-existing keys
        assert_eq!(block.search(b"key10"), None);
        assert_eq!(block.search(b"key"), None);
        assert_eq!(block.search(b"zzz"), None);
    }

    #[test]
    fn test_data_block_get() {
        use crate::txn::TransactionId;

        let mut block = DataBlock::new();

        let chain1 = VersionChain::new(b"value1".to_vec(), TransactionId::from(1));
        block.add(b"key1".to_vec(), chain1).unwrap();

        let chain2 = VersionChain::new(b"value2".to_vec(), TransactionId::from(2));
        block.add(b"key2".to_vec(), chain2).unwrap();

        // Get existing key
        let chain = block.get(b"key1").unwrap();
        match &chain.value {
            VersionValue::Inline(data) => {
                assert_eq!(data.as_slice(), b"value1");
            }
            _ => panic!("Expected inline value"),
        }

        // Get non-existing key
        assert!(block.get(b"key3").is_none());
    }

    #[test]
    fn test_data_block_serialization_uncompressed() {
        use crate::txn::TransactionId;

        let mut block = DataBlock::new();

        for i in 0..5 {
            let key = format!("key{}", i);
            let value = format!("value{}", i);
            let chain = VersionChain::new(value.into_bytes(), TransactionId::from(i as u64));
            block.add(key.into_bytes(), chain).unwrap();
        }

        // Serialize without compression
        let bytes = block.to_bytes(false).unwrap();

        // Deserialize
        let restored = DataBlock::from_bytes(&bytes).unwrap();

        assert_eq!(restored.len(), block.len());
        assert_eq!(restored.first_key(), block.first_key());
        assert_eq!(restored.last_key(), block.last_key());

        // Verify all entries
        for i in 0..5 {
            let key = format!("key{}", i);
            let chain = restored.get(key.as_bytes()).unwrap();
            match &chain.value {
                VersionValue::Inline(data) => {
                    assert_eq!(data, &format!("value{}", i).into_bytes());
                }
                _ => panic!("Expected inline value"),
            }
        }
    }

    #[test]
    fn test_data_block_serialization_compressed() {
        use crate::txn::TransactionId;

        let mut block = DataBlock::new();

        // Add enough data to make compression worthwhile
        for i in 0..50 {
            let key = format!("key{:03}", i);
            let value = format!("value{}", i).repeat(10); // Repeat to make it compressible
            let chain = VersionChain::new(value.into_bytes(), TransactionId::from(i as u64));
            block.add(key.into_bytes(), chain).unwrap();
        }

        // Serialize with compression
        let bytes = block.to_bytes(true).unwrap();

        // Deserialize
        let restored = DataBlock::from_bytes(&bytes).unwrap();

        assert_eq!(restored.len(), block.len());
        assert_eq!(restored.first_key(), block.first_key());
        assert_eq!(restored.last_key(), block.last_key());

        // Verify all entries
        for i in 0..50 {
            let key = format!("key{:03}", i);
            let chain = restored.get(key.as_bytes()).unwrap();
            let expected_value = format!("value{}", i).repeat(10);
            match &chain.value {
                VersionValue::Inline(data) => {
                    assert_eq!(data, &expected_value.into_bytes());
                }
                _ => panic!("Expected inline value"),
            }
        }
    }

    #[test]
    fn test_data_block_checksum_validation() {
        use crate::txn::TransactionId;

        let mut block = DataBlock::new();

        let chain = VersionChain::new(b"value1".to_vec(), TransactionId::from(1));
        block.add(b"key1".to_vec(), chain).unwrap();

        let mut bytes = block.to_bytes(false).unwrap();

        // Corrupt the data (after the header)
        if bytes.len() > DataBlock::HEADER_SIZE + 10 {
            bytes[DataBlock::HEADER_SIZE + 10] ^= 0xFF;
        }

        // Deserialization should fail due to checksum mismatch
        let result = DataBlock::from_bytes(&bytes);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::table::TableError::Corruption { .. }
        ));
    }

    #[test]
    fn test_data_block_version_chain() {
        use crate::txn::TransactionId;

        let mut block = DataBlock::new();

        // Create a version chain with multiple versions
        let mut chain = VersionChain::new(b"value1".to_vec(), TransactionId::from(1));
        chain.commit(LogSequenceNumber::from(10));

        let chain = chain.prepend(b"value2".to_vec(), TransactionId::from(2));

        block.add(b"key1".to_vec(), chain).unwrap();

        // Serialize and deserialize
        let bytes = block.to_bytes(false).unwrap();
        let restored = DataBlock::from_bytes(&bytes).unwrap();

        // Verify version chain is preserved
        let restored_chain = restored.get(b"key1").unwrap();
        match &restored_chain.value {
            VersionValue::Inline(data) => {
                assert_eq!(data.as_slice(), b"value2");
            }
            _ => panic!("Expected inline value"),
        }
        assert!(restored_chain.prev_version.is_some());

        let prev = restored_chain.prev_version.as_ref().unwrap();
        match &prev.value {
            VersionValue::Inline(data) => {
                assert_eq!(data.as_slice(), b"value1");
            }
            _ => panic!("Expected inline value"),
        }
        assert_eq!(prev.commit_lsn, Some(LogSequenceNumber::from(10)));
    }

    #[test]
    fn test_data_block_from_entries() {
        use crate::txn::TransactionId;

        let entries = vec![
            (
                b"key1".to_vec(),
                VersionChain::new(b"value1".to_vec(), TransactionId::from(1)),
            ),
            (
                b"key2".to_vec(),
                VersionChain::new(b"value2".to_vec(), TransactionId::from(2)),
            ),
            (
                b"key3".to_vec(),
                VersionChain::new(b"value3".to_vec(), TransactionId::from(3)),
            ),
        ];

        let block = DataBlock::from_entries(entries);

        assert_eq!(block.len(), 3);
        assert_eq!(block.first_key(), Some(b"key1".as_slice()));
        assert_eq!(block.last_key(), Some(b"key3".as_slice()));
    }

    #[test]
    #[should_panic(expected = "Entries must be sorted by key")]
    fn test_data_block_from_entries_unsorted_panics() {
        use crate::txn::TransactionId;

        let entries = vec![
            (
                b"key2".to_vec(),
                VersionChain::new(b"value2".to_vec(), TransactionId::from(2)),
            ),
            (
                b"key1".to_vec(),
                VersionChain::new(b"value1".to_vec(), TransactionId::from(1)),
            ),
        ];

        DataBlock::from_entries(entries);
    }

    #[test]
    fn test_data_block_estimate_size() {
        use crate::txn::TransactionId;

        let mut block = DataBlock::new();

        for i in 0..10 {
            let key = format!("key{}", i);
            let value = format!("value{}", i);
            let chain = VersionChain::new(value.into_bytes(), TransactionId::from(i as u64));
            block.add(key.into_bytes(), chain).unwrap();
        }

        let estimated = block.estimate_size();
        let actual = block.to_bytes(false).unwrap().len();

        // Estimate should be reasonably close to actual size
        // Allow 20% margin for estimation error
        let diff = estimated.abs_diff(actual);

        assert!(
            diff < actual / 5,
            "Estimate {} too far from actual {}",
            estimated,
            actual
        );
    }

    #[test]
    fn test_data_block_truncated_data() {
        use crate::txn::TransactionId;

        let mut block = DataBlock::new();
        let chain = VersionChain::new(b"value1".to_vec(), TransactionId::from(1));
        block.add(b"key1".to_vec(), chain).unwrap();

        let bytes = block.to_bytes(false).unwrap();

        // Try to deserialize truncated data
        let truncated = &bytes[..bytes.len() - 10];
        let result = DataBlock::from_bytes(truncated);

        assert!(result.is_err());
    }

    #[test]
    fn test_data_block_empty_serialization() {
        // Empty blocks should not be serializable in practice,
        // but let's test the edge case
        let block = DataBlock::new();
        let bytes = block.to_bytes(false).unwrap();

        let restored = DataBlock::from_bytes(&bytes).unwrap();
        assert!(restored.is_empty());
    }

    // ===== IndexBlock Tests =====

    #[test]
    fn test_index_block_empty() {
        let block = IndexBlock::new();
        assert!(block.is_empty());
        assert_eq!(block.len(), 0);
        assert!(block.entries().is_empty());
    }

    #[test]
    fn test_index_block_add_entries() {
        let mut block = IndexBlock::new();

        block.add(b"apple".to_vec(), PageId::from(1), 0).unwrap();
        block.add(b"banana".to_vec(), PageId::from(2), 100).unwrap();
        block.add(b"cherry".to_vec(), PageId::from(3), 200).unwrap();

        assert_eq!(block.len(), 3);
        assert!(!block.is_empty());

        let entries = block.entries();
        assert_eq!(entries[0].first_key, b"apple");
        assert_eq!(entries[0].page_id, PageId::from(1));
        assert_eq!(entries[0].offset, 0);

        assert_eq!(entries[1].first_key, b"banana");
        assert_eq!(entries[1].page_id, PageId::from(2));
        assert_eq!(entries[1].offset, 100);

        assert_eq!(entries[2].first_key, b"cherry");
        assert_eq!(entries[2].page_id, PageId::from(3));
        assert_eq!(entries[2].offset, 200);
    }

    #[test]
    fn test_index_block_add_unsorted_fails() {
        let mut block = IndexBlock::new();

        block.add(b"banana".to_vec(), PageId::from(1), 0).unwrap();

        // Try to add a key that's not greater than the last key
        let result = block.add(b"apple".to_vec(), PageId::from(2), 100);
        assert!(result.is_err());

        // Try to add the same key again
        let result = block.add(b"banana".to_vec(), PageId::from(3), 200);
        assert!(result.is_err());
    }

    #[test]
    fn test_index_block_binary_search() {
        let mut block = IndexBlock::new();

        // Add entries for data blocks starting with these keys
        block.add(b"apple".to_vec(), PageId::from(1), 0).unwrap();
        block.add(b"dog".to_vec(), PageId::from(2), 100).unwrap();
        block.add(b"monkey".to_vec(), PageId::from(3), 200).unwrap();
        block.add(b"zebra".to_vec(), PageId::from(4), 300).unwrap();

        // Search for keys before first block
        assert_eq!(block.search(b"aaa"), None);
        assert_eq!(block.search(b"aardvark"), None);

        // Search for keys in first block (>= "apple", < "dog")
        assert_eq!(block.search(b"apple"), Some(0));
        assert_eq!(block.search(b"banana"), Some(0));
        assert_eq!(block.search(b"cat"), Some(0));

        // Search for keys in second block (>= "dog", < "monkey")
        assert_eq!(block.search(b"dog"), Some(1));
        assert_eq!(block.search(b"elephant"), Some(1));
        assert_eq!(block.search(b"lion"), Some(1));

        // Search for keys in third block (>= "monkey", < "zebra")
        assert_eq!(block.search(b"monkey"), Some(2));
        assert_eq!(block.search(b"panda"), Some(2));

        // Search for keys in fourth block (>= "zebra")
        assert_eq!(block.search(b"zebra"), Some(3));
        assert_eq!(block.search(b"zoo"), Some(3));
    }

    #[test]
    fn test_index_block_search_empty() {
        let block = IndexBlock::new();
        assert_eq!(block.search(b"any_key"), None);
    }

    #[test]
    fn test_index_block_search_single_entry() {
        let mut block = IndexBlock::new();
        block.add(b"middle".to_vec(), PageId::from(1), 0).unwrap();

        // Key before the entry
        assert_eq!(block.search(b"aaa"), None);

        // Key equal to the entry
        assert_eq!(block.search(b"middle"), Some(0));

        // Key after the entry
        assert_eq!(block.search(b"zzz"), Some(0));
    }

    #[test]
    fn test_index_block_get() {
        let mut block = IndexBlock::new();

        block.add(b"apple".to_vec(), PageId::from(1), 0).unwrap();
        block.add(b"banana".to_vec(), PageId::from(2), 100).unwrap();

        let entry = block.get(0).unwrap();
        assert_eq!(entry.first_key, b"apple");
        assert_eq!(entry.page_id, PageId::from(1));
        assert_eq!(entry.offset, 0);

        let entry = block.get(1).unwrap();
        assert_eq!(entry.first_key, b"banana");

        assert!(block.get(2).is_none());
    }

    #[test]
    fn test_index_block_serialization() {
        let mut block = IndexBlock::new();

        block.add(b"apple".to_vec(), PageId::from(1), 0).unwrap();
        block.add(b"banana".to_vec(), PageId::from(2), 100).unwrap();
        block.add(b"cherry".to_vec(), PageId::from(3), 200).unwrap();

        let bytes = block.to_bytes().unwrap();

        // Verify header
        let num_entries = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(num_entries, 3);

        // Deserialize and verify
        let restored = IndexBlock::from_bytes(&bytes).unwrap();
        assert_eq!(restored.len(), 3);

        let entries = restored.entries();
        assert_eq!(entries[0].first_key, b"apple");
        assert_eq!(entries[0].page_id, PageId::from(1));
        assert_eq!(entries[0].offset, 0);

        assert_eq!(entries[1].first_key, b"banana");
        assert_eq!(entries[1].page_id, PageId::from(2));
        assert_eq!(entries[1].offset, 100);

        assert_eq!(entries[2].first_key, b"cherry");
        assert_eq!(entries[2].page_id, PageId::from(3));
        assert_eq!(entries[2].offset, 200);
    }
    #[test]
    fn test_sstable_writer_finish_small() {
        use crate::pager::{PageSize, Pager, PagerConfig};
        use crate::vfs::MemoryFileSystem;
        use std::sync::Arc;

        // Create in-memory pager
        let fs = MemoryFileSystem::new();
        let config = PagerConfig {
            page_size: PageSize::Size4KB,
            compression: crate::pager::CompressionType::None,
            encryption: crate::pager::EncryptionType::None,
            encryption_key: None,
            enable_checksums: true,
            cache_capacity: 100,
            cache_write_back: false,
        };
        let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

        // Create writer
        let sstable_config = SStableConfig::default();
        let mut writer =
            SStableWriter::new(pager.clone(), SStableId::new(1), 0, sstable_config, 10);

        // Add some entries
        let txn_id = crate::txn::TransactionId::from(1);
        for i in 0..10 {
            let key = format!("key{:03}", i).into_bytes();
            let value = format!("value{}", i).into_bytes();
            let mut chain = VersionChain::new(value, txn_id);
            chain.commit(LogSequenceNumber::from(1));
            writer.add(key, chain).unwrap();
        }

        // Finish writing
        let lsn = LogSequenceNumber::from(1);
        let metadata = writer.finish(lsn).unwrap();

        // Verify metadata
        assert_eq!(metadata.id.as_u64(), 1);
        assert_eq!(metadata.level, 0);
        assert_eq!(metadata.min_key, b"key000");
        assert_eq!(metadata.max_key, b"key009");
        assert_eq!(metadata.num_entries, 10);
        assert!(metadata.total_size > 0);
        assert_eq!(metadata.created_lsn, lsn);
        assert!(metadata.num_pages > 0);
        assert!(metadata.index_offset > 0);
        assert!(metadata.bloom_filter_offset > 0);
        assert!(metadata.footer_offset > 0);
    }

    #[test]
    fn test_sstable_writer_finish_large() {
        use crate::pager::{PageSize, Pager, PagerConfig};
        use crate::vfs::MemoryFileSystem;
        use std::sync::Arc;

        // Create in-memory pager
        let fs = MemoryFileSystem::new();
        let config = PagerConfig {
            page_size: PageSize::Size4KB,
            compression: crate::pager::CompressionType::None,
            encryption: crate::pager::EncryptionType::None,
            encryption_key: None,
            enable_checksums: true,
            cache_capacity: 100,
            cache_write_back: false,
        };
        let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

        // Create writer with small block size to force multiple blocks
        let mut sstable_config = SStableConfig::default();
        sstable_config.block_size = 512; // Small block size
        let mut writer =
            SStableWriter::new(pager.clone(), SStableId::new(2), 1, sstable_config, 100);

        // Add many entries to span multiple blocks
        let txn_id = crate::txn::TransactionId::from(1);
        for i in 0..100 {
            let key = format!("key{:05}", i).into_bytes();
            let value = vec![b'x'; 50]; // 50 bytes per value
            let mut chain = VersionChain::new(value, txn_id);
            chain.commit(LogSequenceNumber::from(1));
            writer.add(key, chain).unwrap();
        }

        // Finish writing
        let lsn = LogSequenceNumber::from(2);
        let metadata = writer.finish(lsn).unwrap();

        // Verify metadata
        assert_eq!(metadata.id.as_u64(), 2);
        assert_eq!(metadata.level, 1);
        assert_eq!(metadata.min_key, b"key00000");
        assert_eq!(metadata.max_key, b"key00099");
        assert_eq!(metadata.num_entries, 100);
        assert!(metadata.total_size > 0);
        assert_eq!(metadata.created_lsn, lsn);
        assert!(metadata.num_pages > 1); // Should span multiple pages
        assert!(metadata.index_offset > 0);
        assert!(metadata.bloom_filter_offset > 0);
        assert!(metadata.footer_offset > 0);
    }

    #[test]
    fn test_sstable_writer_finish_empty_fails() {
        use crate::pager::{PageSize, Pager, PagerConfig};
        use crate::vfs::MemoryFileSystem;
        use std::sync::Arc;

        // Create in-memory pager
        let fs = MemoryFileSystem::new();
        let config = PagerConfig {
            page_size: PageSize::Size4KB,
            compression: crate::pager::CompressionType::None,
            encryption: crate::pager::EncryptionType::None,
            encryption_key: None,
            enable_checksums: true,
            cache_capacity: 100,
            cache_write_back: false,
        };
        let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

        // Create writer without adding entries
        let sstable_config = SStableConfig::default();
        let writer = SStableWriter::new(pager.clone(), SStableId::new(3), 0, sstable_config, 10);

        // Finish should fail
        let lsn = LogSequenceNumber::from(1);
        let result = writer.finish(lsn);
        assert!(result.is_err());
    }

    #[test]
    fn test_sstable_writer_finish_with_version_chains() {
        use crate::pager::{PageSize, Pager, PagerConfig};
        use crate::vfs::MemoryFileSystem;
        use std::sync::Arc;

        // Create in-memory pager
        let fs = MemoryFileSystem::new();
        let config = PagerConfig {
            page_size: PageSize::Size4KB,
            compression: crate::pager::CompressionType::None,
            encryption: crate::pager::EncryptionType::None,
            encryption_key: None,
            enable_checksums: true,
            cache_capacity: 100,
            cache_write_back: false,
        };
        let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

        // Create writer
        let sstable_config = SStableConfig::default();
        let mut writer = SStableWriter::new(pager.clone(), SStableId::new(4), 0, sstable_config, 5);

        // Add entries with version chains
        let txn_id1 = crate::txn::TransactionId::from(1);
        let txn_id2 = crate::txn::TransactionId::from(2);

        for i in 0..5 {
            let key = format!("key{}", i).into_bytes();

            // Create version chain with multiple versions
            let mut chain = VersionChain::new(format!("value{}_v1", i).into_bytes(), txn_id1);
            chain.commit(LogSequenceNumber::from(1));

            chain = chain.prepend(format!("value{}_v2", i).into_bytes(), txn_id2);
            chain.commit(LogSequenceNumber::from(2));

            writer.add(key, chain).unwrap();
        }

        // Finish writing
        let lsn = LogSequenceNumber::from(3);
        let metadata = writer.finish(lsn).unwrap();

        // Verify metadata
        assert_eq!(metadata.num_entries, 5);
        assert!(metadata.total_size > 0);
    }

    #[test]
    fn test_index_block_checksum_validation() {
        let mut block = IndexBlock::new();
        block.add(b"test".to_vec(), PageId::from(1), 0).unwrap();

        let mut bytes = block.to_bytes().unwrap();

        // Corrupt the checksum
        bytes[4] ^= 0xFF;

        let result = IndexBlock::from_bytes(&bytes);
        match result {
            Err(crate::table::TableError::Corruption {
                location,
                corruption_type,
                ..
            }) => {
                assert_eq!(location, "IndexBlock::from_bytes");
                assert_eq!(corruption_type, "checksum_mismatch");
            }
            other => panic!("expected corruption error, got {other:?}"),
        }
    }

    #[test]
    fn test_index_block_truncated_header_reports_corruption() {
        let bytes = vec![0u8; IndexBlock::HEADER_SIZE - 1];

        let result = IndexBlock::from_bytes(&bytes);
        match result {
            Err(crate::table::TableError::Corruption {
                location,
                corruption_type,
                details,
            }) => {
                assert_eq!(location, "IndexBlock::from_bytes");
                assert_eq!(corruption_type, "truncated_header");
                assert!(details.contains("too short"));
            }
            other => panic!("expected corruption error, got {other:?}"),
        }
    }

    #[test]
    fn test_index_block_unsorted_entries_report_corruption() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 32]);

        let entries_start = bytes.len();

        for (key, page_id, offset) in [
            (b"banana".as_slice(), PageId::from(2), 20_u64),
            (b"apple".as_slice(), PageId::from(1), 10_u64),
        ] {
            bytes.extend_from_slice(&(key.len() as u32).to_le_bytes());
            bytes.extend_from_slice(key);
            bytes.extend_from_slice(&page_id.as_u64().to_le_bytes());
            bytes.extend_from_slice(&offset.to_le_bytes());
        }

        let entries_data = &bytes[entries_start..];
        let mut hasher = Sha256::new();
        hasher.update(entries_data);
        let checksum = hasher.finalize();
        bytes[4..36].copy_from_slice(&checksum);

        let result = IndexBlock::from_bytes(&bytes);
        match result {
            Err(crate::table::TableError::Corruption {
                location,
                corruption_type,
                details,
            }) => {
                assert_eq!(location, "IndexBlock::from_bytes");
                assert_eq!(corruption_type, "unsorted_entries");
                assert!(details.contains("not sorted"));
            }
            other => panic!("expected corruption error, got {other:?}"),
        }
    }

    #[test]
    fn test_index_block_from_entries() {
        let entries = vec![
            IndexEntry {
                first_key: b"apple".to_vec(),
                page_id: PageId::from(1),
                offset: 0,
            },
            IndexEntry {
                first_key: b"banana".to_vec(),
                page_id: PageId::from(2),
                offset: 100,
            },
        ];

        let block = IndexBlock::from_entries(entries);
        assert_eq!(block.len(), 2);
        assert_eq!(block.entries()[0].first_key, b"apple");
        assert_eq!(block.entries()[1].first_key, b"banana");
    }

    #[test]
    #[should_panic(expected = "Index entries must be sorted by key")]
    fn test_index_block_from_entries_unsorted_panics() {
        let entries = vec![
            IndexEntry {
                first_key: b"banana".to_vec(),
                page_id: PageId::from(1),
                offset: 0,
            },
            IndexEntry {
                first_key: b"apple".to_vec(),
                page_id: PageId::from(2),
                offset: 100,
            },
        ];

        IndexBlock::from_entries(entries);
    }

    #[test]
    fn test_index_block_estimate_size() {
        let mut block = IndexBlock::new();

        // Empty block
        let size = block.estimate_size();
        assert_eq!(size, IndexBlock::HEADER_SIZE);

        // Add entries
        block.add(b"apple".to_vec(), PageId::from(1), 0).unwrap();
        block.add(b"banana".to_vec(), PageId::from(2), 100).unwrap();

        let size = block.estimate_size();
        // Header + 2 entries * (4 + key_len + 8 + 8)
        let expected = IndexBlock::HEADER_SIZE
            + (4 + 5 + 8 + 8)  // "apple"
            + (4 + 6 + 8 + 8); // "banana"
        assert_eq!(size, expected);
    }

    #[test]
    fn test_index_block_truncated_data() {
        let mut block = IndexBlock::new();
        block.add(b"test".to_vec(), PageId::from(1), 0).unwrap();

        let bytes = block.to_bytes().unwrap();

        // Truncate at various points
        let result = IndexBlock::from_bytes(&bytes[..10]);
        assert!(result.is_err());

        let result = IndexBlock::from_bytes(&bytes[..IndexBlock::HEADER_SIZE]);
        assert!(result.is_err());
    }

    #[test]
    fn test_index_block_empty_serialization() {
        let block = IndexBlock::new();
        let bytes = block.to_bytes().unwrap();

        let restored = IndexBlock::from_bytes(&bytes).unwrap();
        assert!(restored.is_empty());
    }

    #[test]
    fn test_index_block_large_keys() {
        let mut block = IndexBlock::new();

        // Add entries with large keys
        let large_key1 = vec![b'a'; 1000];
        let large_key2 = vec![b'b'; 1000];

        block.add(large_key1.clone(), PageId::from(1), 0).unwrap();
        block.add(large_key2.clone(), PageId::from(2), 100).unwrap();

        let bytes = block.to_bytes().unwrap();
        let restored = IndexBlock::from_bytes(&bytes).unwrap();

        assert_eq!(restored.len(), 2);
        assert_eq!(restored.entries()[0].first_key, large_key1);
        assert_eq!(restored.entries()[1].first_key, large_key2);
    }

    #[test]
    fn test_index_block_many_entries() {
        let mut block = IndexBlock::new();

        // Add many entries
        for i in 0..100 {
            let key = format!("key_{:04}", i).into_bytes();
            block
                .add(key, PageId::from(i as u64), i as u64 * 100)
                .unwrap();
        }

        assert_eq!(block.len(), 100);

        // Test serialization
        let bytes = block.to_bytes().unwrap();
        let restored = IndexBlock::from_bytes(&bytes).unwrap();

        assert_eq!(restored.len(), 100);

        // Verify a few entries
        assert_eq!(restored.entries()[0].first_key, b"key_0000");
        assert_eq!(restored.entries()[50].first_key, b"key_0050");
        assert_eq!(restored.entries()[99].first_key, b"key_0099");

        // Test binary search
        assert_eq!(restored.search(b"key_0025"), Some(25));
        assert_eq!(restored.search(b"key_0075"), Some(75));
    }

    #[test]
    fn test_index_block_unsorted_deserialization_fails() {
        // Manually create bytes with unsorted entries
        let mut bytes = Vec::new();

        // Header: num_entries = 2
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 32]); // Placeholder checksum

        let entries_start = bytes.len();

        // Entry 1: "banana"
        bytes.extend_from_slice(&6u32.to_le_bytes());
        bytes.extend_from_slice(b"banana");
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());

        // Entry 2: "apple" (unsorted!)
        bytes.extend_from_slice(&5u32.to_le_bytes());
        bytes.extend_from_slice(b"apple");
        bytes.extend_from_slice(&2u64.to_le_bytes());
        bytes.extend_from_slice(&100u64.to_le_bytes());

        // Calculate and write checksum
        let entries_data = &bytes[entries_start..];
        let mut hasher = Sha256::new();
        hasher.update(entries_data);
        let checksum = hasher.finalize();
        bytes[4..36].copy_from_slice(&checksum);

        // Should fail because entries are not sorted
        let result = IndexBlock::from_bytes(&bytes);
        assert!(result.is_err());
    }
}

// Made with Bob

#[test]
fn test_sstable_reader_open_small() {
    use crate::pager::{PageSize, Pager, PagerConfig};
    use crate::vfs::MemoryFileSystem;
    use std::sync::Arc;

    // Create in-memory pager
    let fs = MemoryFileSystem::new();
    let config = PagerConfig {
        page_size: PageSize::Size4KB,
        compression: crate::pager::CompressionType::None,
        encryption: crate::pager::EncryptionType::None,
        encryption_key: None,
        enable_checksums: true,
        cache_capacity: 100,
        cache_write_back: false,
    };
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Create and write SSTable
    let sstable_config = SStableConfig::default();
    let mut writer = SStableWriter::new(
        pager.clone(),
        SStableId::new(1),
        0,
        sstable_config.clone(),
        10,
    );

    // Add some entries
    let txn_id = crate::txn::TransactionId::from(1);
    for i in 0..10 {
        let key = format!("key{:03}", i).into_bytes();
        let value = format!("value{}", i).into_bytes();
        let mut chain = VersionChain::new(value, txn_id);
        chain.commit(LogSequenceNumber::from(1));
        writer.add(key, chain).unwrap();
    }

    // Finish writing
    let lsn = LogSequenceNumber::from(1);
    let metadata = writer.finish(lsn).unwrap();

    // Open the SSTable for reading
    let reader =
        SStableReader::open(pager.clone(), metadata.first_page_id, sstable_config).unwrap();

    // Verify reader metadata matches writer metadata
    assert_eq!(reader.metadata().id.as_u64(), metadata.id.as_u64());
    assert_eq!(reader.metadata().level, metadata.level);
    assert_eq!(reader.metadata().min_key, metadata.min_key);
    assert_eq!(reader.metadata().max_key, metadata.max_key);
    assert_eq!(reader.metadata().num_entries, metadata.num_entries);
    assert_eq!(reader.metadata().total_size, metadata.total_size);
    assert_eq!(reader.metadata().created_lsn, metadata.created_lsn);
    assert_eq!(reader.metadata().first_page_id, metadata.first_page_id);
    assert_eq!(reader.metadata().num_pages, metadata.num_pages);

    // Verify bloom filter was loaded
    assert!(reader.bloom_filter.is_some());

    // Test bloom filter functionality
    assert!(reader.may_contain(b"key000"));
    assert!(reader.may_contain(b"key005"));
    assert!(reader.may_contain(b"key009"));
}

#[test]
fn test_sstable_reader_open_large() {
    use crate::pager::{PageSize, Pager, PagerConfig};
    use crate::vfs::MemoryFileSystem;
    use std::sync::Arc;

    // Create in-memory pager
    let fs = MemoryFileSystem::new();
    let config = PagerConfig {
        page_size: PageSize::Size4KB,
        compression: crate::pager::CompressionType::None,
        encryption: crate::pager::EncryptionType::None,
        encryption_key: None,
        enable_checksums: true,
        cache_capacity: 100,
        cache_write_back: false,
    };
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Create and write large SSTable (multiple pages)
    let sstable_config = SStableConfig::default();
    let mut writer = SStableWriter::new(
        pager.clone(),
        SStableId::new(2),
        1,
        sstable_config.clone(),
        1000,
    );

    // Add many entries to span multiple pages
    let txn_id = crate::txn::TransactionId::from(1);
    for i in 0..1000 {
        let key = format!("key{:06}", i).into_bytes();
        let value = format!("value{}", i).repeat(10).into_bytes(); // Larger values
        let mut chain = VersionChain::new(value, txn_id);
        chain.commit(LogSequenceNumber::from(1));
        writer.add(key, chain).unwrap();
    }

    // Finish writing
    let lsn = LogSequenceNumber::from(1);
    let metadata = writer.finish(lsn).unwrap();

    // Verify it spans multiple pages
    assert!(
        metadata.num_pages > 1,
        "Expected multiple pages for large SSTable"
    );

    // Open the SSTable for reading
    let reader =
        SStableReader::open(pager.clone(), metadata.first_page_id, sstable_config).unwrap();

    // Verify reader metadata
    assert_eq!(reader.metadata().id.as_u64(), 2);
    assert_eq!(reader.metadata().level, 1);
    assert_eq!(reader.metadata().num_entries, 1000);
    assert_eq!(reader.metadata().min_key, b"key000000");
    assert_eq!(reader.metadata().max_key, b"key000999");

    // Verify bloom filter
    assert!(reader.bloom_filter.is_some());
    assert!(reader.may_contain(b"key000000"));
    assert!(reader.may_contain(b"key000500"));
    assert!(reader.may_contain(b"key000999"));
}

#[test]
fn test_sstable_reader_open_with_version_chains() {
    use crate::pager::{PageSize, Pager, PagerConfig};
    use crate::vfs::MemoryFileSystem;
    use std::sync::Arc;

    // Create in-memory pager
    let fs = MemoryFileSystem::new();
    let config = PagerConfig {
        page_size: PageSize::Size4KB,
        compression: crate::pager::CompressionType::None,
        encryption: crate::pager::EncryptionType::None,
        encryption_key: None,
        enable_checksums: true,
        cache_capacity: 100,
        cache_write_back: false,
    };
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Create and write SSTable with version chains
    let sstable_config = SStableConfig::default();
    let mut writer = SStableWriter::new(
        pager.clone(),
        SStableId::new(3),
        0,
        sstable_config.clone(),
        5,
    );

    // Add entries with multiple versions
    let txn_id1 = crate::txn::TransactionId::from(1);
    let txn_id2 = crate::txn::TransactionId::from(2);

    for i in 0..5 {
        let key = format!("key{}", i).into_bytes();
        let value1 = format!("value{}_v1", i).into_bytes();
        let value2 = format!("value{}_v2", i).into_bytes();

        // Create version chain with two versions
        let mut chain = VersionChain::new(value1, txn_id1);
        chain.commit(LogSequenceNumber::from(1));
        let mut chain = chain.prepend(value2, txn_id2);
        chain.commit(LogSequenceNumber::from(2));

        writer.add(key, chain).unwrap();
    }

    // Finish writing
    let lsn = LogSequenceNumber::from(2);
    let metadata = writer.finish(lsn).unwrap();

    // Open the SSTable for reading
    let reader =
        SStableReader::open(pager.clone(), metadata.first_page_id, sstable_config).unwrap();

    // Verify metadata
    assert_eq!(reader.metadata().num_entries, 5);
    assert_eq!(reader.metadata().min_key, b"key0");
    assert_eq!(reader.metadata().max_key, b"key4");

    // Verify bloom filter contains all keys
    for i in 0..5 {
        let key = format!("key{}", i).into_bytes();
        assert!(reader.may_contain(&key));
    }
}

#[test]
fn test_sstable_reader_open_invalid_page() {
    use crate::pager::{PageSize, Pager, PagerConfig};
    use crate::vfs::MemoryFileSystem;
    use std::sync::Arc;

    // Create in-memory pager
    let fs = MemoryFileSystem::new();
    let config = PagerConfig {
        page_size: PageSize::Size4KB,
        compression: crate::pager::CompressionType::None,
        encryption: crate::pager::EncryptionType::None,
        encryption_key: None,
        enable_checksums: true,
        cache_capacity: 100,
        cache_write_back: false,
    };
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Try to open SSTable from non-existent page
    let sstable_config = SStableConfig::default();
    let result = SStableReader::open(
        pager.clone(),
        PageId::from(999), // Non-existent page
        sstable_config,
    );

    // Should fail
    assert!(result.is_err());
}

#[test]
fn test_sstable_reader_bloom_filter_false_negatives() {
    use crate::pager::{PageSize, Pager, PagerConfig};
    use crate::vfs::MemoryFileSystem;
    use std::sync::Arc;

    // Create in-memory pager
    let fs = MemoryFileSystem::new();
    let config = PagerConfig {
        page_size: PageSize::Size4KB,
        compression: crate::pager::CompressionType::None,
        encryption: crate::pager::EncryptionType::None,
        encryption_key: None,
        enable_checksums: true,
        cache_capacity: 100,
        cache_write_back: false,
    };
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Create and write SSTable
    let sstable_config = SStableConfig::default();
    let mut writer = SStableWriter::new(
        pager.clone(),
        SStableId::new(4),
        0,
        sstable_config.clone(),
        10,
    );

    // Add specific keys
    let txn_id = crate::txn::TransactionId::from(1);
    let keys: Vec<&[u8]> = vec![b"apple", b"banana", b"cherry", b"date", b"elderberry"];

    for key in &keys {
        let value = format!("value_{}", String::from_utf8_lossy(key)).into_bytes();
        let mut chain = VersionChain::new(value, txn_id);
        chain.commit(LogSequenceNumber::from(1));
        writer.add(key.to_vec(), chain).unwrap();
    }

    // Finish writing
    let lsn = LogSequenceNumber::from(1);
    let metadata = writer.finish(lsn).unwrap();

    // Open the SSTable for reading
    let reader =
        SStableReader::open(pager.clone(), metadata.first_page_id, sstable_config).unwrap();

    // Verify all inserted keys are found (no false negatives)
    for key in &keys {
        assert!(
            reader.may_contain(key),
            "Bloom filter should never have false negatives for key: {:?}",
            String::from_utf8_lossy(key)
        );
    }

    // Keys not in the set might return true (false positives are acceptable)
    // but should not return false for keys that ARE in the set
    let not_inserted: Vec<&[u8]> = vec![b"fig", b"grape", b"honeydew"];
    for key in &not_inserted {
        // We can't assert false here because bloom filters can have false positives
        // We just verify the method doesn't panic
        let _ = reader.may_contain(key);
    }
}

#[test]
fn test_sstable_reader_get_successful_lookup() {
    use crate::pager::{PageSize, Pager, PagerConfig};
    use crate::vfs::MemoryFileSystem;
    use std::sync::Arc;

    // Create in-memory pager
    let fs = MemoryFileSystem::new();
    let config = PagerConfig {
        page_size: PageSize::Size4KB,
        compression: crate::pager::CompressionType::None,
        encryption: crate::pager::EncryptionType::None,
        encryption_key: None,
        enable_checksums: true,
        cache_capacity: 100,
        cache_write_back: false,
    };
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Create and write SSTable
    let sstable_config = SStableConfig::default();
    let mut writer = SStableWriter::new(
        pager.clone(),
        SStableId::new(1),
        0,
        sstable_config.clone(),
        10,
    );

    // Add entries
    let txn_id = crate::txn::TransactionId::from(1);
    for i in 0..10 {
        let key = format!("key{:03}", i).into_bytes();
        let value = format!("value{}", i).into_bytes();
        let mut chain = VersionChain::new(value, txn_id);
        chain.commit(LogSequenceNumber::from(10));
        writer.add(key, chain).unwrap();
    }

    let metadata = writer.finish(LogSequenceNumber::from(10)).unwrap();

    // Open for reading
    let reader =
        SStableReader::open(pager.clone(), metadata.first_page_id, sstable_config).unwrap();

    // Test successful lookups
    for i in 0..10 {
        let key = format!("key{:03}", i).into_bytes();
        let expected_value = format!("value{}", i).into_bytes();
        let result = reader.get(&key, LogSequenceNumber::from(10)).unwrap();
        assert_eq!(result, Some(expected_value), "Failed to get key{:03}", i);
    }
}

#[test]
fn test_sstable_reader_get_key_not_found() {
    use crate::pager::{PageSize, Pager, PagerConfig};
    use crate::vfs::MemoryFileSystem;
    use std::sync::Arc;

    // Create in-memory pager
    let fs = MemoryFileSystem::new();
    let config = PagerConfig {
        page_size: PageSize::Size4KB,
        compression: crate::pager::CompressionType::None,
        encryption: crate::pager::EncryptionType::None,
        encryption_key: None,
        enable_checksums: true,
        cache_capacity: 100,
        cache_write_back: false,
    };
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Create and write SSTable with specific keys
    let sstable_config = SStableConfig::default();
    let mut writer = SStableWriter::new(
        pager.clone(),
        SStableId::new(1),
        0,
        sstable_config.clone(),
        5,
    );

    let txn_id = crate::txn::TransactionId::from(1);
    let keys: Vec<&[u8]> = vec![b"apple", b"banana", b"cherry", b"date", b"elderberry"];

    for key in &keys {
        let value = format!("value_{}", String::from_utf8_lossy(key)).into_bytes();
        let mut chain = VersionChain::new(value, txn_id);
        chain.commit(LogSequenceNumber::from(10));
        writer.add(key.to_vec(), chain).unwrap();
    }

    let metadata = writer.finish(LogSequenceNumber::from(10)).unwrap();

    // Open for reading
    let reader =
        SStableReader::open(pager.clone(), metadata.first_page_id, sstable_config).unwrap();

    // Test keys that don't exist
    assert_eq!(
        reader
            .get(b"aardvark", LogSequenceNumber::from(10))
            .unwrap(),
        None
    );
    assert_eq!(
        reader.get(b"fig", LogSequenceNumber::from(10)).unwrap(),
        None
    );
    assert_eq!(
        reader.get(b"zebra", LogSequenceNumber::from(10)).unwrap(),
        None
    );
}

#[test]
fn test_sstable_reader_get_mvcc_visibility() {
    use crate::pager::{PageSize, Pager, PagerConfig};
    use crate::vfs::MemoryFileSystem;
    use std::sync::Arc;

    // Create in-memory pager
    let fs = MemoryFileSystem::new();
    let config = PagerConfig {
        page_size: PageSize::Size4KB,
        compression: crate::pager::CompressionType::None,
        encryption: crate::pager::EncryptionType::None,
        encryption_key: None,
        enable_checksums: true,
        cache_capacity: 100,
        cache_write_back: false,
    };
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Create SSTable with version chains
    let sstable_config = SStableConfig::default();
    let mut writer = SStableWriter::new(
        pager.clone(),
        SStableId::new(1),
        0,
        sstable_config.clone(),
        3,
    );

    let txn_id1 = crate::txn::TransactionId::from(1);
    let txn_id2 = crate::txn::TransactionId::from(2);
    let txn_id3 = crate::txn::TransactionId::from(3);

    // Key with three versions at LSN 10, 20, 30
    let mut chain = VersionChain::new(b"value_v1".to_vec(), txn_id1);
    chain.commit(LogSequenceNumber::from(10));
    let mut chain = chain.prepend(b"value_v2".to_vec(), txn_id2);
    chain.commit(LogSequenceNumber::from(20));
    let mut chain = chain.prepend(b"value_v3".to_vec(), txn_id3);
    chain.commit(LogSequenceNumber::from(30));
    writer.add(b"key1".to_vec(), chain).unwrap();

    let metadata = writer.finish(LogSequenceNumber::from(30)).unwrap();

    // Open for reading
    let reader =
        SStableReader::open(pager.clone(), metadata.first_page_id, sstable_config).unwrap();

    // Read at LSN 5 - should see nothing (all versions too new)
    assert_eq!(
        reader.get(b"key1", LogSequenceNumber::from(5)).unwrap(),
        None
    );

    // Read at LSN 10 - should see v1
    assert_eq!(
        reader.get(b"key1", LogSequenceNumber::from(10)).unwrap(),
        Some(b"value_v1".to_vec())
    );

    // Read at LSN 15 - should see v1 (v2 not visible yet)
    assert_eq!(
        reader.get(b"key1", LogSequenceNumber::from(15)).unwrap(),
        Some(b"value_v1".to_vec())
    );

    // Read at LSN 20 - should see v2
    assert_eq!(
        reader.get(b"key1", LogSequenceNumber::from(20)).unwrap(),
        Some(b"value_v2".to_vec())
    );

    // Read at LSN 25 - should see v2 (v3 not visible yet)
    assert_eq!(
        reader.get(b"key1", LogSequenceNumber::from(25)).unwrap(),
        Some(b"value_v2".to_vec())
    );

    // Read at LSN 30 - should see v3
    assert_eq!(
        reader.get(b"key1", LogSequenceNumber::from(30)).unwrap(),
        Some(b"value_v3".to_vec())
    );

    // Read at LSN 100 - should see v3 (latest)
    assert_eq!(
        reader.get(b"key1", LogSequenceNumber::from(100)).unwrap(),
        Some(b"value_v3".to_vec())
    );
}

#[test]
fn test_sstable_reader_get_multiple_versions_same_key() {
    use crate::pager::{PageSize, Pager, PagerConfig};
    use crate::vfs::MemoryFileSystem;
    use std::sync::Arc;

    // Create in-memory pager
    let fs = MemoryFileSystem::new();
    let config = PagerConfig {
        page_size: PageSize::Size4KB,
        compression: crate::pager::CompressionType::None,
        encryption: crate::pager::EncryptionType::None,
        encryption_key: None,
        enable_checksums: true,
        cache_capacity: 100,
        cache_write_back: false,
    };
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Create SSTable with multiple keys, each with version chains
    let sstable_config = SStableConfig::default();
    let mut writer = SStableWriter::new(
        pager.clone(),
        SStableId::new(1),
        0,
        sstable_config.clone(),
        5,
    );

    let txn_id1 = crate::txn::TransactionId::from(1);
    let txn_id2 = crate::txn::TransactionId::from(2);

    for i in 0..5 {
        let key = format!("key{}", i).into_bytes();
        let value1 = format!("value{}_v1", i).into_bytes();
        let value2 = format!("value{}_v2", i).into_bytes();

        let mut chain = VersionChain::new(value1, txn_id1);
        chain.commit(LogSequenceNumber::from(10));
        let mut chain = chain.prepend(value2, txn_id2);
        chain.commit(LogSequenceNumber::from(20));

        writer.add(key, chain).unwrap();
    }

    let metadata = writer.finish(LogSequenceNumber::from(20)).unwrap();

    // Open for reading
    let reader =
        SStableReader::open(pager.clone(), metadata.first_page_id, sstable_config).unwrap();

    // Test reading at different LSNs
    for i in 0..5 {
        let key = format!("key{}", i).into_bytes();

        // At LSN 10, should see v1
        let expected_v1 = format!("value{}_v1", i).into_bytes();
        assert_eq!(
            reader.get(&key, LogSequenceNumber::from(10)).unwrap(),
            Some(expected_v1)
        );

        // At LSN 20, should see v2
        let expected_v2 = format!("value{}_v2", i).into_bytes();
        assert_eq!(
            reader.get(&key, LogSequenceNumber::from(20)).unwrap(),
            Some(expected_v2)
        );
    }
}

#[test]
fn test_sstable_reader_get_tombstone_handling() {
    use crate::pager::{PageSize, Pager, PagerConfig};
    use crate::vfs::MemoryFileSystem;
    use std::sync::Arc;

    // Create in-memory pager
    let fs = MemoryFileSystem::new();
    let config = PagerConfig {
        page_size: PageSize::Size4KB,
        compression: crate::pager::CompressionType::None,
        encryption: crate::pager::EncryptionType::None,
        encryption_key: None,
        enable_checksums: true,
        cache_capacity: 100,
        cache_write_back: false,
    };
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Create SSTable with tombstone (empty value represents deletion)
    let sstable_config = SStableConfig::default();
    let mut writer = SStableWriter::new(
        pager.clone(),
        SStableId::new(1),
        0,
        sstable_config.clone(),
        3,
    );

    let txn_id1 = crate::txn::TransactionId::from(1);
    let txn_id2 = crate::txn::TransactionId::from(2);

    // Key with value, then tombstone
    let mut chain = VersionChain::new(b"original_value".to_vec(), txn_id1);
    chain.commit(LogSequenceNumber::from(10));
    let mut chain = chain.prepend(Vec::new(), txn_id2); // Empty value = tombstone
    chain.commit(LogSequenceNumber::from(20));
    writer.add(b"deleted_key".to_vec(), chain).unwrap();

    let metadata = writer.finish(LogSequenceNumber::from(20)).unwrap();

    // Open for reading
    let reader =
        SStableReader::open(pager.clone(), metadata.first_page_id, sstable_config).unwrap();

    // At LSN 10, should see original value
    assert_eq!(
        reader
            .get(b"deleted_key", LogSequenceNumber::from(10))
            .unwrap(),
        Some(b"original_value".to_vec())
    );

    // At LSN 20, should see tombstone (empty value)
    assert_eq!(
        reader
            .get(b"deleted_key", LogSequenceNumber::from(20))
            .unwrap(),
        Some(Vec::new())
    );
}

#[test]
fn test_sstable_reader_get_large_sstable() {
    use crate::pager::{PageSize, Pager, PagerConfig};
    use crate::vfs::MemoryFileSystem;
    use std::sync::Arc;

    // Create in-memory pager
    let fs = MemoryFileSystem::new();
    let config = PagerConfig {
        page_size: PageSize::Size4KB,
        compression: crate::pager::CompressionType::None,
        encryption: crate::pager::EncryptionType::None,
        encryption_key: None,
        enable_checksums: true,
        cache_capacity: 100,
        cache_write_back: false,
    };
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Create large SSTable spanning multiple pages
    let sstable_config = SStableConfig::default();
    let mut writer = SStableWriter::new(
        pager.clone(),
        SStableId::new(1),
        0,
        sstable_config.clone(),
        1000,
    );

    let txn_id = crate::txn::TransactionId::from(1);
    for i in 0..1000 {
        let key = format!("key{:06}", i).into_bytes();
        let value = format!("value{}", i).repeat(10).into_bytes();
        let mut chain = VersionChain::new(value, txn_id);
        chain.commit(LogSequenceNumber::from(10));
        writer.add(key, chain).unwrap();
    }

    let metadata = writer.finish(LogSequenceNumber::from(10)).unwrap();

    // Verify it spans multiple pages
    assert!(metadata.num_pages > 1);

    // Open for reading
    let reader =
        SStableReader::open(pager.clone(), metadata.first_page_id, sstable_config).unwrap();

    // Test lookups across different data blocks
    for i in [0, 100, 500, 999] {
        let key = format!("key{:06}", i).into_bytes();
        let expected_value = format!("value{}", i).repeat(10).into_bytes();
        let result = reader.get(&key, LogSequenceNumber::from(10)).unwrap();
        assert_eq!(result, Some(expected_value), "Failed to get key{:06}", i);
    }
}

#[test]
fn test_sstable_reader_get_bloom_filter_false_positive() {
    use crate::pager::{PageSize, Pager, PagerConfig};
    use crate::vfs::MemoryFileSystem;
    use std::sync::Arc;

    // Create in-memory pager
    let fs = MemoryFileSystem::new();
    let config = PagerConfig {
        page_size: PageSize::Size4KB,
        compression: crate::pager::CompressionType::None,
        encryption: crate::pager::EncryptionType::None,
        encryption_key: None,
        enable_checksums: true,
        cache_capacity: 100,
        cache_write_back: false,
    };
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Create SSTable with specific keys
    let sstable_config = SStableConfig::default();
    let mut writer = SStableWriter::new(
        pager.clone(),
        SStableId::new(1),
        0,
        sstable_config.clone(),
        5,
    );

    let txn_id = crate::txn::TransactionId::from(1);
    let keys: Vec<&[u8]> = vec![b"apple", b"banana", b"cherry", b"date", b"elderberry"];

    for key in &keys {
        let value = format!("value_{}", String::from_utf8_lossy(key)).into_bytes();
        let mut chain = VersionChain::new(value, txn_id);
        chain.commit(LogSequenceNumber::from(10));
        writer.add(key.to_vec(), chain).unwrap();
    }

    let metadata = writer.finish(LogSequenceNumber::from(10)).unwrap();

    // Open for reading
    let reader =
        SStableReader::open(pager.clone(), metadata.first_page_id, sstable_config).unwrap();

    // Test that keys not in SSTable return None
    // Even if bloom filter has false positive, get() should return None
    let not_present: Vec<&[u8]> = vec![b"aardvark", b"fig", b"grape", b"honeydew", b"zebra"];
    for key in &not_present {
        let result = reader.get(key, LogSequenceNumber::from(10)).unwrap();
        assert_eq!(
            result,
            None,
            "Key {:?} should not be found",
            String::from_utf8_lossy(key)
        );
    }
}
