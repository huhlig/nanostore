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

//! Page structures and types

use crate::pager::{CompressionType, EncryptionType, PagerError, PagerResult};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::Rng;
use sha2::{Digest, Sha256};
use std::io::Cursor;

/// Page identifier inside a single-file database. (0-based)
#[derive(
    Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct PageId(u64);

impl PageId {
    #[must_use]
    pub fn as_u64(&self) -> u64 {
        self.0
    }
    pub fn to_bytes(&self) -> [u8; 8] {
        self.0.to_le_bytes()
    }
}

impl From<u64> for PageId {
    fn from(value: u64) -> Self {
        PageId(value)
    }
}

impl std::fmt::Display for PageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PageId({})", self.0)
    }
}

/// Page type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageType {
    /// Free page (available for allocation)
    Free = 0,
    /// Superblock page (database metadata)
    Superblock = 1,
    /// Free list page (tracks free pages)
    FreeList = 2,
    /// B-Tree internal node
    BTreeInternal = 3,
    /// B-Tree leaf node
    BTreeLeaf = 4,
    /// Hash table bucket page
    HashBucket = 5,
    /// Adaptive Radix Tree (ART) Node4 (up to 4 children)
    ArtNode4 = 6,
    /// Adaptive Radix Tree (ART) Node16 (up to 16 children)
    ArtNode16 = 7,
    /// Adaptive Radix Tree (ART) Node48 (up to 48 children)
    ArtNode48 = 8,
    /// Adaptive Radix Tree (ART) Node256 (up to 256 children)
    ArtNode256 = 9,
    /// LSM level metadata
    LsmMeta = 10,
    /// LSM data page
    LsmData = 11,
    /// Catalog page (table metadata)
    Catalog = 12,
    /// Overflow page (for large values)
    Overflow = 13,
    /// Bloom filter metadata page
    BloomMeta = 14,
    /// Bloom filter data page
    BloomData = 15,
    /// Bloom filter page (legacy, use BloomMeta/BloomData)
    BloomFilter = 16,
    /// Inverted index page
    InvertedIndex = 17,
    /// R-Tree node page (spatial indexing)
    RTreeNode = 18,
    /// Graph adjacency list page
    GraphAdjList = 19,
    /// Vector index page (for similarity search)
    VectorIndex = 20,
    /// Time series bucket page
    TimeSeriesBucket = 21,
    /// Index metadata page
    IndexMetadata = 33,
}

impl PageType {
    /// Convert from u8 representation
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(PageType::Free),
            1 => Some(PageType::Superblock),
            2 => Some(PageType::FreeList),
            3 => Some(PageType::BTreeInternal),
            4 => Some(PageType::BTreeLeaf),
            5 => Some(PageType::HashBucket),
            6 => Some(PageType::ArtNode4),
            7 => Some(PageType::ArtNode16),
            8 => Some(PageType::ArtNode48),
            9 => Some(PageType::ArtNode256),
            10 => Some(PageType::LsmMeta),
            11 => Some(PageType::LsmData),
            12 => Some(PageType::Catalog),
            13 => Some(PageType::Overflow),
            16 => Some(PageType::BloomFilter),
            17 => Some(PageType::InvertedIndex),
            18 => Some(PageType::RTreeNode),
            19 => Some(PageType::GraphAdjList),
            20 => Some(PageType::VectorIndex),
            21 => Some(PageType::TimeSeriesBucket),
            33 => Some(PageType::IndexMetadata),
            _ => None,
        }
    }

    /// Convert to u8 representation
    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Page header (32 bytes)
///
/// Layout:
/// - Bytes 0-7: Page ID (u64)
/// - Byte 8: Page type (u8)
/// - Byte 9: Compression type (u8)
/// - Byte 10: Encryption type (u8)
/// - Byte 11: Flags (u8)
/// - Bytes 12-15: Uncompressed size (u32)
/// - Bytes 16-19: Compressed size (u32)
/// - Bytes 20-23: Reserved (u32)
/// - Bytes 24-31: Reserved (u64)
#[derive(Debug, Clone)]
pub struct PageHeader {
    /// Page identifier
    pub page_id: PageId,
    /// Page type
    pub page_type: PageType,
    /// Compression type used
    pub compression: CompressionType,
    /// Encryption type used
    pub encryption: EncryptionType,
    /// Flags (reserved for future use)
    pub flags: u8,
    /// Uncompressed data size
    pub uncompressed_size: u32,
    /// Compressed data size (0 if not compressed)
    pub compressed_size: u32,
}

impl PageHeader {
    /// Size of the page header in bytes
    pub const SIZE: usize = 32;

    /// Create a new page header
    pub fn new(page_id: PageId, page_type: PageType) -> Self {
        Self {
            page_id,
            page_type,
            compression: CompressionType::None,
            encryption: EncryptionType::None,
            flags: 0,
            uncompressed_size: 0,
            compressed_size: 0,
        }
    }

    /// Serialize the header to bytes
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];

        // Page ID (8 bytes)
        bytes[0..8].copy_from_slice(&self.page_id.to_bytes());

        // Page type (1 byte)
        bytes[8] = self.page_type.to_u8();

        // Compression type (1 byte)
        bytes[9] = self.compression.to_u8();

        // Encryption type (1 byte)
        bytes[10] = self.encryption.to_u8();

        // Flags (1 byte)
        bytes[11] = self.flags;

        // Uncompressed size (4 bytes)
        bytes[12..16].copy_from_slice(&self.uncompressed_size.to_le_bytes());

        // Compressed size (4 bytes)
        bytes[16..20].copy_from_slice(&self.compressed_size.to_le_bytes());

        // Reserved bytes remain 0

        bytes
    }

    /// Deserialize the header from bytes
    pub fn from_bytes(bytes: &[u8]) -> PagerResult<Self> {
        if bytes.len() < Self::SIZE {
            return Err(PagerError::InternalError(
                "Insufficient bytes for page header".to_string(),
            ));
        }

        let page_id = PageId::from(u64::from_le_bytes(bytes[0..8].try_into().unwrap()));

        let page_type =
            PageType::from_u8(bytes[8]).ok_or_else(|| PagerError::InvalidPageType(bytes[8]))?;

        let compression = CompressionType::from_u8(bytes[9]).ok_or_else(|| {
            PagerError::InternalError(format!("Invalid compression type: {}", bytes[9]))
        })?;

        let encryption = EncryptionType::from_u8(bytes[10]).ok_or_else(|| {
            PagerError::InternalError(format!("Invalid encryption type: {}", bytes[10]))
        })?;

        let flags = bytes[11];

        let uncompressed_size = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let compressed_size = u32::from_le_bytes(bytes[16..20].try_into().unwrap());

        Ok(Self {
            page_id,
            page_type,
            compression,
            encryption,
            flags,
            uncompressed_size,
            compressed_size,
        })
    }
}

/// Page structure
///
/// Layout: [Header: 32B][Data: variable][Checksum: 32B]
#[derive(Debug, Clone)]
pub struct Page {
    /// Page header
    pub header: PageHeader,
    /// Page data (uncompressed, unencrypted)
    pub data: Vec<u8>,
}

impl Page {
    /// Checksum size (SHA-256)
    pub const CHECKSUM_SIZE: usize = 32;

    /// Create a new page
    pub fn new(page_id: PageId, page_type: PageType, data_capacity: usize) -> Self {
        Self {
            header: PageHeader::new(page_id, page_type),
            data: Vec::with_capacity(data_capacity),
        }
    }

    /// Calculate SHA-256 checksum of the page
    pub fn calculate_checksum(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.header.to_bytes());
        // Only hash the actual data length, not padding
        hasher.update((self.data.len() as u32).to_le_bytes());
        hasher.update(&self.data);
        hasher.finalize().into()
    }

    /// Verify the checksum of the page
    pub fn verify_checksum(&self, expected: &[u8; 32]) -> bool {
        let actual = self.calculate_checksum();
        actual == *expected
    }

    /// Serialize the page to bytes (with checksum)
    pub fn to_bytes(
        &self,
        page_size: usize,
        encryption_key: Option<&[u8; 32]>,
    ) -> PagerResult<Vec<u8>> {
        let mut bytes = Vec::with_capacity(page_size);

        // Update header with actual data size
        let mut header = self.header.clone();
        header.uncompressed_size = self.data.len() as u32;

        // Compress data if needed
        let compressed_data = match header.compression {
            CompressionType::None => {
                header.compressed_size = self.data.len() as u32;
                self.data.clone()
            }
            CompressionType::Lz4 => {
                let compressed = lz4_flex::compress_prepend_size(&self.data);
                header.compressed_size = compressed.len() as u32;
                compressed
            }
            CompressionType::Zstd => {
                let compressed = zstd::encode_all(Cursor::new(&self.data), 3).map_err(|e| {
                    PagerError::compression_error(
                        header.page_id,
                        CompressionType::Zstd,
                        e.to_string(),
                    )
                })?;
                header.compressed_size = compressed.len() as u32;
                compressed
            }
        };

        // Encrypt data if needed (after compression)
        let data_to_write = match header.encryption {
            EncryptionType::None => compressed_data,
            EncryptionType::Aes256Gcm => {
                // Check if encryption key is provided
                let key = encryption_key.ok_or(PagerError::MissingEncryptionKey)?;

                // Generate random 12-byte nonce
                let mut nonce_bytes = [0u8; 12];
                rand::rng().fill_bytes(&mut nonce_bytes);
                let nonce = Nonce::from_slice(&nonce_bytes);

                // Create cipher
                let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));

                // Encrypt the data
                let encrypted = cipher
                    .encrypt(nonce, compressed_data.as_ref())
                    .map_err(|e| {
                        PagerError::encryption_error(
                            header.page_id,
                            EncryptionType::Aes256Gcm,
                            e.to_string(),
                        )
                    })?;

                // Prepend nonce to encrypted data
                let mut result = Vec::with_capacity(12 + encrypted.len());
                result.extend_from_slice(&nonce_bytes);
                result.extend_from_slice(&encrypted);

                // Update compressed_size to reflect encrypted data size (including nonce)
                header.compressed_size = result.len() as u32;

                result
            }
        };

        // Write header (with updated compressed_size if encrypted)
        bytes.extend_from_slice(&header.to_bytes());

        // Write data (compressed and/or encrypted)
        bytes.extend_from_slice(&data_to_write);

        // Pad to page_size - CHECKSUM_SIZE
        let data_end = page_size - Self::CHECKSUM_SIZE;
        if bytes.len() < data_end {
            bytes.resize(data_end, 0);
        }

        // Calculate and append checksum on final data
        let checksum = {
            let mut hasher = Sha256::new();
            hasher.update(header.to_bytes());
            hasher.update((data_to_write.len() as u32).to_le_bytes());
            hasher.update(&data_to_write);
            hasher.finalize()
        };
        bytes.extend_from_slice(&checksum);

        Ok(bytes)
    }

    /// Deserialize the page from bytes (with checksum verification)
    pub fn from_bytes(
        bytes: &[u8],
        verify_checksum: bool,
        encryption_key: Option<&[u8; 32]>,
    ) -> PagerResult<Self> {
        if bytes.len() < PageHeader::SIZE + Self::CHECKSUM_SIZE {
            return Err(PagerError::InternalError(
                "Insufficient bytes for page".to_string(),
            ));
        }

        // Parse header
        let header = PageHeader::from_bytes(&bytes[0..PageHeader::SIZE])?;
        let page_id = header.page_id;
        let compressed_len = header.compressed_size as usize;

        // Extract data (encrypted and/or compressed, only the actual data, not padding)
        let data_start = PageHeader::SIZE;
        let data_end = data_start + compressed_len;

        if data_end > bytes.len() - Self::CHECKSUM_SIZE {
            return Err(PagerError::InternalError(
                "Invalid compressed data length in header".to_string(),
            ));
        }

        let encrypted_data = &bytes[data_start..data_end];

        // Extract checksum
        let checksum_start = bytes.len() - Self::CHECKSUM_SIZE;
        let checksum: [u8; 32] = bytes[checksum_start..]
            .try_into()
            .map_err(|_| PagerError::InternalError("Invalid checksum size".to_string()))?;

        // Verify checksum on encrypted data if requested
        if verify_checksum {
            let mut hasher = Sha256::new();
            hasher.update(header.to_bytes());
            hasher.update((encrypted_data.len() as u32).to_le_bytes());
            hasher.update(encrypted_data);
            let actual_checksum: [u8; 32] = hasher.finalize().into();

            if actual_checksum != checksum {
                return Err(PagerError::ChecksumMismatch(page_id));
            }
        }

        // Decrypt data if needed (before decompression)
        let compressed_data = match header.encryption {
            EncryptionType::None => encrypted_data.to_vec(),
            EncryptionType::Aes256Gcm => {
                // Check if encryption key is provided
                let key = encryption_key.ok_or(PagerError::MissingEncryptionKey)?;

                // Extract nonce (first 12 bytes)
                if encrypted_data.len() < 12 {
                    return Err(PagerError::decryption_error(
                        header.page_id,
                        EncryptionType::Aes256Gcm,
                        "Insufficient data for nonce",
                    ));
                }
                let nonce = Nonce::from_slice(&encrypted_data[0..12]);
                let ciphertext = &encrypted_data[12..];

                // Create cipher
                let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));

                // Decrypt the data
                cipher.decrypt(nonce, ciphertext).map_err(|e| {
                    PagerError::decryption_error(
                        header.page_id,
                        EncryptionType::Aes256Gcm,
                        e.to_string(),
                    )
                })?
            }
        };

        // Decompress data if needed
        let data = match header.compression {
            CompressionType::None => compressed_data,
            CompressionType::Lz4 => {
                lz4_flex::decompress_size_prepended(&compressed_data).map_err(|e| {
                    PagerError::decompression_error(
                        header.page_id,
                        CompressionType::Lz4,
                        e.to_string(),
                    )
                })?
            }
            CompressionType::Zstd => {
                zstd::decode_all(Cursor::new(&compressed_data)).map_err(|e| {
                    PagerError::decompression_error(
                        header.page_id,
                        CompressionType::Zstd,
                        e.to_string(),
                    )
                })?
            }
        };

        // Verify decompressed size matches header
        if data.len() != header.uncompressed_size as usize {
            return Err(PagerError::decompression_error(
                header.page_id,
                header.compression,
                format!(
                    "Decompressed size {} does not match expected size {}",
                    data.len(),
                    header.uncompressed_size
                ),
            ));
        }

        Ok(Self { header, data })
    }

    /// Get the page ID
    pub fn page_id(&self) -> PageId {
        self.header.page_id
    }

    /// Get the page type
    pub fn page_type(&self) -> PageType {
        self.header.page_type
    }

    /// Get the data slice
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Get mutable data slice
    pub fn data_mut(&mut self) -> &mut Vec<u8> {
        &mut self.data
    }
}

/// Overflow page header (32 bytes)
///
/// Layout:
/// - Bytes 0-3: Magic number (0x4F564C46 = "OVLF")
/// - Bytes 4-7: Next page ID (0 if last page)
/// - Bytes 8-11: Data length (bytes of data in this page)
/// - Bytes 12-15: Checksum (CRC32 of data)
/// - Bytes 16-31: Reserved (for future use)
#[derive(Debug, Clone)]
pub struct OverflowPageHeader {
    /// Magic number for overflow pages ("OVLF" = 0x4F564C46)
    pub magic: u32,
    /// Next page ID in the chain (0 if last page)
    pub next_page_id: u32,
    /// Length of data in this page
    pub data_length: u32,
    /// CRC32 checksum of the data
    pub checksum: u32,
}

impl OverflowPageHeader {
    /// Size of the overflow page header in bytes
    pub const SIZE: usize = 32;

    /// Magic number for overflow pages ("OVLF")
    pub const MAGIC: u32 = 0x4F564C46;

    /// Create a new overflow page header
    pub fn new(next_page_id: u32, data_length: u32, checksum: u32) -> Self {
        Self {
            magic: Self::MAGIC,
            next_page_id,
            data_length,
            checksum,
        }
    }

    /// Serialize the header to bytes
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];

        // Magic (4 bytes)
        bytes[0..4].copy_from_slice(&self.magic.to_le_bytes());

        // Next page ID (4 bytes)
        bytes[4..8].copy_from_slice(&self.next_page_id.to_le_bytes());

        // Data length (4 bytes)
        bytes[8..12].copy_from_slice(&self.data_length.to_le_bytes());

        // Checksum (4 bytes)
        bytes[12..16].copy_from_slice(&self.checksum.to_le_bytes());

        // Reserved bytes remain 0

        bytes
    }

    /// Deserialize the header from bytes
    pub fn from_bytes(bytes: &[u8]) -> PagerResult<Self> {
        if bytes.len() < Self::SIZE {
            return Err(PagerError::InternalError(
                "Insufficient bytes for overflow page header".to_string(),
            ));
        }

        let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        if magic != Self::MAGIC {
            return Err(PagerError::InternalError(format!(
                "Invalid overflow page magic: expected 0x{:08X}, got 0x{:08X}",
                Self::MAGIC,
                magic
            )));
        }

        let next_page_id = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let data_length = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let checksum = u32::from_le_bytes(bytes[12..16].try_into().unwrap());

        Ok(Self {
            magic,
            next_page_id,
            data_length,
            checksum,
        })
    }

    /// Check if this is the last page in the chain
    pub fn is_last(&self) -> bool {
        self.next_page_id == 0
    }
}

/// Calculate CRC32 checksum for data
pub fn calculate_crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_type_conversion() {
        assert_eq!(PageType::from_u8(0), Some(PageType::Free));
        assert_eq!(PageType::from_u8(1), Some(PageType::Superblock));
        assert_eq!(PageType::from_u8(255), None);

        assert_eq!(PageType::Free.to_u8(), 0);
        assert_eq!(PageType::Superblock.to_u8(), 1);
    }

    #[test]
    fn test_page_header_serialization() {
        let header = PageHeader::new(PageId::from(42), PageType::BTreeLeaf);
        let bytes = header.to_bytes();
        let deserialized = PageHeader::from_bytes(&bytes).unwrap();

        assert_eq!(deserialized.page_id, PageId::from(42));
        assert_eq!(deserialized.page_type, PageType::BTreeLeaf);
    }

    #[test]
    fn test_page_checksum() {
        let mut page = Page::new(PageId::from(1), PageType::BTreeLeaf, 100);
        page.data.extend_from_slice(b"test data");

        let checksum = page.calculate_checksum();
        assert!(page.verify_checksum(&checksum));

        // Modify data and verify checksum fails
        page.data[0] = b'X';
        assert!(!page.verify_checksum(&checksum));
    }

    #[test]
    fn test_page_serialization() {
        let mut page = Page::new(PageId::from(5), PageType::BTreeInternal, 100);
        page.data.extend_from_slice(b"test page data");

        let bytes = page.to_bytes(4096, None).unwrap();
        assert_eq!(bytes.len(), 4096);

        let deserialized = Page::from_bytes(&bytes, true, None).unwrap();
        assert_eq!(deserialized.page_id(), PageId::from(5));
        assert_eq!(deserialized.page_type(), PageType::BTreeInternal);
        assert_eq!(deserialized.data(), b"test page data");
    }

    #[test]
    fn test_page_compression_lz4() {
        let mut page = Page::new(PageId::from(10), PageType::BTreeLeaf, 1000);
        page.header.compression = CompressionType::Lz4;

        // Add compressible data
        let test_data = b"This is test data that should compress well. ".repeat(20);
        page.data.extend_from_slice(&test_data);

        let bytes = page.to_bytes(4096, None).unwrap();
        assert_eq!(bytes.len(), 4096);

        let deserialized = Page::from_bytes(&bytes, true, None).unwrap();
        assert_eq!(deserialized.page_id(), PageId::from(10));
        assert_eq!(deserialized.page_type(), PageType::BTreeLeaf);
        assert_eq!(deserialized.data(), &test_data[..]);
        assert_eq!(deserialized.header.compression, CompressionType::Lz4);

        // Verify compression actually happened
        assert!(deserialized.header.compressed_size < deserialized.header.uncompressed_size);
    }

    #[test]
    fn test_page_compression_zstd() {
        let mut page = Page::new(PageId::from(11), PageType::BTreeLeaf, 1000);
        page.header.compression = CompressionType::Zstd;

        // Add compressible data
        let test_data = b"Zstd compression test data. ".repeat(30);
        page.data.extend_from_slice(&test_data);

        let bytes = page.to_bytes(4096, None).unwrap();
        assert_eq!(bytes.len(), 4096);

        let deserialized = Page::from_bytes(&bytes, true, None).unwrap();
        assert_eq!(deserialized.page_id(), PageId::from(11));
        assert_eq!(deserialized.page_type(), PageType::BTreeLeaf);
        assert_eq!(deserialized.data(), &test_data[..]);
        assert_eq!(deserialized.header.compression, CompressionType::Zstd);

        // Verify compression actually happened
        assert!(deserialized.header.compressed_size < deserialized.header.uncompressed_size);
    }

    #[test]
    fn test_page_no_compression() {
        let mut page = Page::new(PageId::from(12), PageType::BTreeLeaf, 100);
        page.header.compression = CompressionType::None;
        page.data.extend_from_slice(b"uncompressed data");

        let bytes = page.to_bytes(4096, None).unwrap();

        let deserialized = Page::from_bytes(&bytes, true, None).unwrap();
        assert_eq!(deserialized.data(), b"uncompressed data");
        assert_eq!(deserialized.header.compression, CompressionType::None);
        assert_eq!(
            deserialized.header.compressed_size,
            deserialized.header.uncompressed_size
        );
    }

    #[test]
    fn test_page_checksum_with_compression() {
        let mut page = Page::new(PageId::from(13), PageType::BTreeLeaf, 100);
        page.header.compression = CompressionType::Lz4;
        page.data.extend_from_slice(b"test data for checksum");

        let bytes = page.to_bytes(4096, None).unwrap();

        // Valid checksum should pass
        let deserialized = Page::from_bytes(&bytes, true, None).unwrap();
        assert_eq!(deserialized.data(), b"test data for checksum");

        // Corrupt the compressed data and verify checksum fails
        let mut corrupted = bytes.clone();
        corrupted[PageHeader::SIZE + 5] ^= 0xFF;

        let result = Page::from_bytes(&corrupted, true, None);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PagerError::ChecksumMismatch(_)
        ));
    }

    #[test]
    fn test_page_encryption_aes256gcm() {
        let mut page = Page::new(PageId::from(14), PageType::BTreeLeaf, 100);
        page.header.encryption = EncryptionType::Aes256Gcm;
        page.data.extend_from_slice(b"secret data to encrypt");

        let key = [42u8; 32];
        let bytes = page.to_bytes(4096, Some(&key)).unwrap();
        assert_eq!(bytes.len(), 4096);

        // Decrypt with correct key
        let deserialized = Page::from_bytes(&bytes, true, Some(&key)).unwrap();
        assert_eq!(deserialized.page_id(), PageId::from(14));
        assert_eq!(deserialized.page_type(), PageType::BTreeLeaf);
        assert_eq!(deserialized.data(), b"secret data to encrypt");
        assert_eq!(deserialized.header.encryption, EncryptionType::Aes256Gcm);
    }

    #[test]
    fn test_page_encryption_wrong_key() {
        let mut page = Page::new(PageId::from(15), PageType::BTreeLeaf, 100);
        page.header.encryption = EncryptionType::Aes256Gcm;
        page.data.extend_from_slice(b"secret data");

        let key1 = [42u8; 32];
        let key2 = [99u8; 32];

        let bytes = page.to_bytes(4096, Some(&key1)).unwrap();

        // Try to decrypt with wrong key
        let result = Page::from_bytes(&bytes, true, Some(&key2));
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PagerError::DecryptionError { .. }
        ));
    }

    #[test]
    fn test_page_encryption_missing_key() {
        let mut page = Page::new(PageId::from(16), PageType::BTreeLeaf, 100);
        page.header.encryption = EncryptionType::Aes256Gcm;
        page.data.extend_from_slice(b"secret data");

        // Try to encrypt without key
        let result = page.to_bytes(4096, None);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PagerError::MissingEncryptionKey
        ));
    }

    #[test]
    fn test_page_encryption_and_compression() {
        let mut page = Page::new(PageId::from(17), PageType::BTreeLeaf, 1000);
        page.header.compression = CompressionType::Lz4;
        page.header.encryption = EncryptionType::Aes256Gcm;

        // Add compressible data
        let test_data = b"This data will be compressed then encrypted. ".repeat(20);
        page.data.extend_from_slice(&test_data);

        let key = [123u8; 32];
        let bytes = page.to_bytes(4096, Some(&key)).unwrap();
        assert_eq!(bytes.len(), 4096);

        // Decrypt and decompress
        let deserialized = Page::from_bytes(&bytes, true, Some(&key)).unwrap();
        assert_eq!(deserialized.page_id(), PageId::from(17));
        assert_eq!(deserialized.data(), &test_data[..]);
        assert_eq!(deserialized.header.compression, CompressionType::Lz4);
        assert_eq!(deserialized.header.encryption, EncryptionType::Aes256Gcm);
    }

    #[test]
    fn test_page_encryption_missing_key_on_read() {
        let mut page = Page::new(PageId::from(18), PageType::BTreeLeaf, 100);
        page.header.encryption = EncryptionType::Aes256Gcm;
        page.data.extend_from_slice(b"secret data");

        let key = [42u8; 32];
        let bytes = page.to_bytes(4096, Some(&key)).unwrap();

        // Try to decrypt without key
        let result = Page::from_bytes(&bytes, true, None);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PagerError::MissingEncryptionKey
        ));
    }
}
