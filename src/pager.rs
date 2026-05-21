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

//! Pager Layer - Block-level storage with page management
//!
//! The Pager provides:
//! - Configurable page sizes (4KB, 8KB, 16KB, 32KB, 64KB)
//! - Page allocation and deallocation
//! - Free list management
//! - Optional compression (LZ4, Zstd)
//! - Optional encryption (AES-256-GCM)
//! - SHA-256 checksums for integrity
//! - Superblock for database metadata

mod cache;
mod config;
mod error;
mod freelist;
mod header;
mod lock_order;
mod overflow_stream;
mod page;
mod page_mapper;
mod page_table;
mod pagefile;
mod pin_table;
mod superblock;

pub use self::cache::{CacheConfig, CacheStats, PageCache};
pub use self::config::{CompressionType, EncryptionType, PageSize, PagerConfig};
pub use self::error::{PagerError, PagerResult};
pub use self::freelist::{FreeList, FreeListPage};
pub use self::header::FileHeader;
pub use self::lock_order::{LockType, assert_lock_order, mark_lock_acquired, mark_lock_released};
pub use self::overflow_stream::OverflowChainStream;
pub use self::page::{OverflowPageHeader, Page, PageHeader, PageId, PageType, calculate_crc32};
pub use self::page_mapper::PageMapper;
pub use self::page_table::PageTable;
pub use self::pagefile::Pager;
pub use self::pin_table::{PinGuard, PinTable};
pub use self::superblock::Superblock;

/// Physical location within the database file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PhysicalLocation {
    pub page_id: PageId,
    pub offset: u32,
    pub length: u32,
}
