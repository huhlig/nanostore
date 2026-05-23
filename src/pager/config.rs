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

//! Pager configuration types

/// Compression algorithm types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CompressionType {
    /// No compression
    None = 0,
    /// LZ4 compression (fast, moderate compression)
    Lz4 = 1,
    /// Zstd compression (balanced speed and compression)
    Zstd = 2,
}

impl CompressionType {
    /// Convert from u8 representation
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(CompressionType::None),
            1 => Some(CompressionType::Lz4),
            2 => Some(CompressionType::Zstd),
            _ => None,
        }
    }

    /// Convert to u8 representation
    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Encryption algorithm types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EncryptionType {
    /// No encryption
    None = 0,
    /// AES-256-GCM encryption
    Aes256Gcm = 1,
}

impl EncryptionType {
    /// Convert from u8 representation
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(EncryptionType::None),
            1 => Some(EncryptionType::Aes256Gcm),
            _ => None,
        }
    }

    /// Convert to u8 representation
    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Page size options (must be power of 2)
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PageSize {
    /// 4KB pages (default)
    #[default]
    Size4KB = 4096,
    /// 8KB pages
    Size8KB = 8192,
    /// 16KB pages
    Size16KB = 16384,
    /// 32KB pages
    Size32KB = 32768,
    /// 64KB pages
    Size64KB = 65536,
}

impl PageSize {
    /// Convert from u32 representation
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            4096 => Some(PageSize::Size4KB),
            8192 => Some(PageSize::Size8KB),
            16384 => Some(PageSize::Size16KB),
            32768 => Some(PageSize::Size32KB),
            65536 => Some(PageSize::Size64KB),
            _ => None,
        }
    }

    /// Convert to u32 representation
    pub fn to_u32(self) -> u32 {
        self as u32
    }

    /// Get the size of the page header
    pub fn header_size() -> usize {
        32 // Fixed header size
    }

    /// Get the size of the page checksum
    pub fn checksum_size() -> usize {
        32 // SHA-256 checksum
    }

    /// Get the usable data size for this page size
    pub fn data_size(self) -> usize {
        self.to_u32() as usize - Self::header_size() - Self::checksum_size()
    }
}

/// Pager configuration
#[derive(Debug, Clone)]
pub struct PagerConfig {
    /// Page size
    pub page_size: PageSize,
    /// Compression type
    pub compression: CompressionType,
    /// Encryption type
    pub encryption: EncryptionType,
    /// Encryption key (32 bytes for AES-256)
    pub encryption_key: Option<[u8; 32]>,
    /// Enable checksums (SHA-256)
    pub enable_checksums: bool,
    /// Cache capacity (number of pages, 0 to disable cache)
    pub cache_capacity: usize,
    /// Enable write-back caching (true) or write-through (false)
    pub cache_write_back: bool,
}

impl Default for PagerConfig {
    fn default() -> Self {
        Self {
            page_size: PageSize::default(),
            compression: CompressionType::None,
            encryption: EncryptionType::None,
            encryption_key: None,
            enable_checksums: true,
            cache_capacity: 10_000, // Default to 10,000 pages (~40MB with 4KB pages)
            cache_write_back: true, // Default to write-back for better performance
        }
    }
}

impl PagerConfig {
    /// Create a new pager configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the page size
    pub fn with_page_size(mut self, page_size: PageSize) -> Self {
        self.page_size = page_size;
        self
    }

    /// Set the compression type
    pub fn with_compression(mut self, compression: CompressionType) -> Self {
        self.compression = compression;
        self
    }

    /// Set the encryption type and key
    pub fn with_encryption(mut self, encryption: EncryptionType, key: [u8; 32]) -> Self {
        self.encryption = encryption;
        self.encryption_key = Some(key);
        self
    }

    /// Enable or disable checksums
    pub fn with_checksums(mut self, enable: bool) -> Self {
        self.enable_checksums = enable;
        self
    }

    /// Set the cache capacity (0 to disable cache)
    pub fn with_cache_capacity(mut self, capacity: usize) -> Self {
        self.cache_capacity = capacity;
        self
    }

    /// Enable or disable write-back caching
    pub fn with_cache_write_back(mut self, write_back: bool) -> Self {
        self.cache_write_back = write_back;
        self
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<(), String> {
        // Check encryption key is provided if encryption is enabled
        if self.encryption != EncryptionType::None && self.encryption_key.is_none() {
            return Err("Encryption key required when encryption is enabled".to_string());
        }

        Ok(())
    }
}
