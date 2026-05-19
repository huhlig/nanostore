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

//! Database file header

use crate::pager::{CompressionType, EncryptionType, PageSize, PagerError, PagerResult};

/// Magic number for nanostore database files: "NKDB" in ASCII
const MAGIC: [u8; 4] = [0x4E, 0x4B, 0x44, 0x42]; // "NKDB"

/// Current file format version
const VERSION: u16 = 1;

/// File header (occupies first page of database file)
///
/// Layout (256 bytes):
/// - Bytes 0-3: Magic number "NKDB" (4 bytes)
/// - Bytes 4-5: Format version (u16)
/// - Bytes 6-7: Reserved (u16)
/// - Bytes 8-11: Page size (u32)
/// - Byte 12: Compression type (u8)
/// - Byte 13: Encryption type (u8)
/// - Bytes 14-15: Reserved (u16)
/// - Bytes 16-23: Total pages (u64)
/// - Bytes 24-31: Free pages (u64)
/// - Bytes 32-39: Superblock page ID (u64)
/// - Bytes 40-47: First free list page ID (u64)
/// - Bytes 48-55: Catalog page ID (u64)
/// - Bytes 56-79: Reserved (24 bytes)
/// - Bytes 80-111: Database UUID (32 bytes)
/// - Bytes 112-143: Creation timestamp (32 bytes)
/// - Bytes 144-175: Last modified timestamp (32 bytes)
/// - Bytes 176-255: Reserved (80 bytes)
#[derive(Debug, Clone)]
pub struct FileHeader {
    /// Format version
    pub version: u16,
    /// Page size
    pub page_size: PageSize,
    /// Compression type
    pub compression: CompressionType,
    /// Encryption type
    pub encryption: EncryptionType,
    /// Total number of pages in the database
    pub total_pages: u64,
    /// Number of free pages
    pub free_pages: u64,
    /// Superblock page ID (typically page 1)
    pub superblock_page_id: u64,
    /// First free list page ID
    pub first_free_list_page_id: u64,
    /// Catalog page ID (typically page 2)
    pub catalog_page_id: u64,
    /// Database UUID (for replication/backup identification)
    pub database_uuid: [u8; 32],
    /// Creation timestamp (Unix timestamp as bytes)
    pub created_at: [u8; 32],
    /// Last modified timestamp (Unix timestamp as bytes)
    pub modified_at: [u8; 32],
}

impl FileHeader {
    /// Size of the file header in bytes
    pub const SIZE: usize = 256;

    /// Create a new file header with default values
    pub fn new(
        page_size: PageSize,
        compression: CompressionType,
        encryption: EncryptionType,
    ) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Generate a simple UUID (in production, use a proper UUID library)
        let mut uuid = [0u8; 32];
        uuid[0..8].copy_from_slice(
            &SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .to_le_bytes()[0..8],
        );

        // Get current timestamp
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut timestamp = [0u8; 32];
        timestamp[0..8].copy_from_slice(&now.to_le_bytes());

        Self {
            version: VERSION,
            page_size,
            compression,
            encryption,
            total_pages: 3, // Header page (0) + Superblock page (1) + Catalog page (2)
            free_pages: 0,
            superblock_page_id: 1,
            first_free_list_page_id: 0, // 0 means no free list yet
            catalog_page_id: 2,
            database_uuid: uuid,
            created_at: timestamp,
            modified_at: timestamp,
        }
    }

    /// Serialize the file header to bytes
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];

        // Magic number (4 bytes)
        bytes[0..4].copy_from_slice(&MAGIC);

        // Version (2 bytes)
        bytes[4..6].copy_from_slice(&self.version.to_le_bytes());

        // Page size (4 bytes)
        bytes[8..12].copy_from_slice(&self.page_size.to_u32().to_le_bytes());

        // Compression type (1 byte)
        bytes[12] = self.compression.to_u8();

        // Encryption type (1 byte)
        bytes[13] = self.encryption.to_u8();

        // Total pages (8 bytes)
        bytes[16..24].copy_from_slice(&self.total_pages.to_le_bytes());

        // Free pages (8 bytes)
        bytes[24..32].copy_from_slice(&self.free_pages.to_le_bytes());

        // Superblock page ID (8 bytes)
        bytes[32..40].copy_from_slice(&self.superblock_page_id.to_le_bytes());

        // First free list page ID (8 bytes)
        bytes[40..48].copy_from_slice(&self.first_free_list_page_id.to_le_bytes());

        // Catalog page ID (8 bytes)
        bytes[48..56].copy_from_slice(&self.catalog_page_id.to_le_bytes());

        // Database UUID (32 bytes)
        bytes[80..112].copy_from_slice(&self.database_uuid);

        // Creation timestamp (32 bytes)
        bytes[112..144].copy_from_slice(&self.created_at);

        // Last modified timestamp (32 bytes)
        bytes[144..176].copy_from_slice(&self.modified_at);

        bytes
    }

    /// Deserialize the file header from bytes
    pub fn from_bytes(bytes: &[u8]) -> PagerResult<Self> {
        if bytes.len() < Self::SIZE {
            return Err(PagerError::invalid_file_header(
                u32::from_le_bytes(MAGIC),
                0,
                "Insufficient bytes for file header",
            ));
        }

        // Verify magic number
        let found_magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let expected_magic = u32::from_le_bytes(MAGIC);
        if bytes[0..4] != MAGIC {
            return Err(PagerError::invalid_file_header(
                expected_magic,
                found_magic,
                "Invalid magic number",
            ));
        }

        // Parse version
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != VERSION {
            return Err(PagerError::invalid_file_header(
                expected_magic,
                found_magic,
                format!("Unsupported version: {}", version),
            ));
        }

        // Parse page size
        let page_size_value = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let page_size = PageSize::from_u32(page_size_value)
            .ok_or_else(|| PagerError::InvalidPageSize(page_size_value))?;

        // Parse compression type
        let compression = CompressionType::from_u8(bytes[12]).ok_or_else(|| {
            PagerError::invalid_file_header(
                expected_magic,
                found_magic,
                format!("Invalid compression type: {}", bytes[12]),
            )
        })?;

        // Parse encryption type
        let encryption = EncryptionType::from_u8(bytes[13]).ok_or_else(|| {
            PagerError::invalid_file_header(
                expected_magic,
                found_magic,
                format!("Invalid encryption type: {}", bytes[13]),
            )
        })?;

        // Parse counts
        let total_pages = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let free_pages = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        let superblock_page_id = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
        let first_free_list_page_id = u64::from_le_bytes(bytes[40..48].try_into().unwrap());
        let catalog_page_id = u64::from_le_bytes(bytes[48..56].try_into().unwrap());

        // Parse UUID and timestamps
        let database_uuid: [u8; 32] = bytes[80..112].try_into().unwrap();
        let created_at: [u8; 32] = bytes[112..144].try_into().unwrap();
        let modified_at: [u8; 32] = bytes[144..176].try_into().unwrap();

        Ok(Self {
            version,
            page_size,
            compression,
            encryption,
            total_pages,
            free_pages,
            superblock_page_id,
            first_free_list_page_id,
            catalog_page_id,
            database_uuid,
            created_at,
            modified_at,
        })
    }

    /// Update the last modified timestamp
    pub fn update_modified_timestamp(&mut self) {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.modified_at = [0u8; 32];
        self.modified_at[0..8].copy_from_slice(&now.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_header_serialization() {
        let header = FileHeader::new(
            PageSize::Size4KB,
            CompressionType::None,
            EncryptionType::None,
        );

        let bytes = header.to_bytes();
        assert_eq!(bytes.len(), FileHeader::SIZE);

        // Verify magic number
        assert_eq!(&bytes[0..4], &MAGIC);

        let deserialized = FileHeader::from_bytes(&bytes).unwrap();
        assert_eq!(deserialized.version, VERSION);
        assert_eq!(deserialized.page_size, PageSize::Size4KB);
        assert_eq!(deserialized.compression, CompressionType::None);
        assert_eq!(deserialized.encryption, EncryptionType::None);
        assert_eq!(deserialized.total_pages, 3); // Header (0) + Superblock (1) + Catalog (2)
        assert_eq!(deserialized.superblock_page_id, 1);
        assert_eq!(deserialized.catalog_page_id, 2);
    }

    #[test]
    fn test_invalid_magic_number() {
        let mut bytes = [0u8; FileHeader::SIZE];
        bytes[0..4].copy_from_slice(b"XXXX");

        let result = FileHeader::from_bytes(&bytes);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PagerError::InvalidFileHeader { .. }
        ));
    }

    #[test]
    fn test_invalid_page_size() {
        let header = FileHeader::new(
            PageSize::Size4KB,
            CompressionType::None,
            EncryptionType::None,
        );

        let mut bytes = header.to_bytes();
        // Set invalid page size
        bytes[8..12].copy_from_slice(&12345u32.to_le_bytes());

        let result = FileHeader::from_bytes(&bytes);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PagerError::InvalidPageSize(_)
        ));
    }

    #[test]
    fn test_update_modified_timestamp() {
        let mut header = FileHeader::new(
            PageSize::Size4KB,
            CompressionType::None,
            EncryptionType::None,
        );

        let original_timestamp = header.modified_at;

        // Sleep to ensure timestamp changes (1 second for reliable test)
        std::thread::sleep(std::time::Duration::from_secs(1));

        header.update_modified_timestamp();

        // Timestamps should be different
        assert_ne!(header.modified_at, original_timestamp);
    }
}
