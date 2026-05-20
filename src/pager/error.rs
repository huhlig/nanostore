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

//! Pager error types

use crate::pager::PageId;
use crate::pager::config::{CompressionType, EncryptionType};
use crate::vfs::FileSystemError;
use thiserror::Error;

/// Result type for pager operations
pub type PagerResult<T> = Result<T, PagerError>;

/// Pager error types
#[derive(Debug, Error)]
pub enum PagerError {
    /// VFS error
    #[error("VFS error: {0}")]
    VfsError(#[from] FileSystemError),

    /// Invalid page ID
    #[error("Invalid page ID: {0}")]
    InvalidPageId(PageId),

    /// Page not found
    #[error("Page not found: {0}")]
    PageNotFound(PageId),

    /// Checksum mismatch
    #[error("Checksum mismatch for page {0}")]
    ChecksumMismatch(PageId),

    /// Invalid page size
    #[error("Invalid page size: {0}")]
    InvalidPageSize(u32),

    /// Invalid file header with structured context
    #[error(
        "Invalid file header: expected magic {expected_magic:#x}, found {found_magic:#x} - {details}"
    )]
    InvalidFileHeader {
        expected_magic: u32,
        found_magic: u32,
        details: String,
    },

    /// Invalid superblock with structured context
    #[error("Invalid superblock field '{field}': expected {expected}, found {found}")]
    InvalidSuperblock {
        field: String,
        expected: String,
        found: String,
    },

    /// Compression error with page and type context
    #[error("Compression error for page {page_id} using {compression_type:?}: {details}")]
    CompressionError {
        page_id: PageId,
        compression_type: CompressionType,
        details: String,
    },

    /// Decompression error with page and type context
    #[error("Decompression error for page {page_id} using {compression_type:?}: {details}")]
    DecompressionError {
        page_id: PageId,
        compression_type: CompressionType,
        details: String,
    },

    /// Encryption error with page and type context
    #[error("Encryption error for page {page_id} using {encryption_type:?}: {details}")]
    EncryptionError {
        page_id: PageId,
        encryption_type: EncryptionType,
        details: String,
    },

    /// Decryption error with page and type context
    #[error("Decryption error for page {page_id} using {encryption_type:?}: {details}")]
    DecryptionError {
        page_id: PageId,
        encryption_type: EncryptionType,
        details: String,
    },

    /// Missing encryption key
    #[error("Encryption key required but not provided")]
    MissingEncryptionKey,

    /// Configuration error
    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// Storage is full (no free pages)
    #[error("Storage is full (no free pages available)")]
    StorageFull,

    /// Page is already allocated
    #[error("Page {0} is already allocated")]
    PageAlreadyAllocated(PageId),

    /// Page is already free
    #[error("Page {0} is already free")]
    PageAlreadyFree(PageId),

    /// Page is pinned (cannot be freed while in use)
    #[error("Page {0} is pinned and cannot be freed")]
    PagePinned(PageId),

    /// Invalid page type
    #[error("Invalid page type: {0}")]
    InvalidPageType(u8),

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// Internal error
    #[error("Internal error: {0}")]
    InternalError(String),
}

impl PagerError {
    /// Create a compression error with full context
    pub fn compression_error(
        page_id: PageId,
        compression_type: CompressionType,
        details: impl Into<String>,
    ) -> Self {
        Self::CompressionError {
            page_id,
            compression_type,
            details: details.into(),
        }
    }

    /// Create a decompression error with full context
    pub fn decompression_error(
        page_id: PageId,
        compression_type: CompressionType,
        details: impl Into<String>,
    ) -> Self {
        Self::DecompressionError {
            page_id,
            compression_type,
            details: details.into(),
        }
    }

    /// Create an encryption error with full context
    pub fn encryption_error(
        page_id: PageId,
        encryption_type: EncryptionType,
        details: impl Into<String>,
    ) -> Self {
        Self::EncryptionError {
            page_id,
            encryption_type,
            details: details.into(),
        }
    }

    /// Create a decryption error with full context
    pub fn decryption_error(
        page_id: PageId,
        encryption_type: EncryptionType,
        details: impl Into<String>,
    ) -> Self {
        Self::DecryptionError {
            page_id,
            encryption_type,
            details: details.into(),
        }
    }

    /// Create an invalid file header error with full context
    pub fn invalid_file_header(
        expected_magic: u32,
        found_magic: u32,
        details: impl Into<String>,
    ) -> Self {
        Self::InvalidFileHeader {
            expected_magic,
            found_magic,
            details: details.into(),
        }
    }

    /// Create an invalid superblock error with full context
    pub fn invalid_superblock(
        field: impl Into<String>,
        expected: impl Into<String>,
        found: impl Into<String>,
    ) -> Self {
        Self::InvalidSuperblock {
            field: field.into(),
            expected: expected.into(),
            found: found.into(),
        }
    }
}
