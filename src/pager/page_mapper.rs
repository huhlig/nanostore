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

//! Virtual-to-Physical Page Mapping
//!
//! This module provides an indirection layer between logical page IDs (used by tables)
//! and physical page IDs (actual file positions). This enables efficient database
//! compaction without needing to update all references to moved pages.
//!
//! See docs/VIRTUAL_PHYSICAL_PAGE_MAPPING.md for design details.

use crate::pager::{PageId, PagerError, PagerResult};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Maps virtual page IDs to physical page IDs
///
/// The PageMapper provides an indirection layer that allows pages to be moved
/// in the physical file without updating all references. Only non-identity
/// mappings are stored (i.e., when virtual_id != physical_id).
///
/// # Thread Safety
/// Uses RwLock for concurrent access - multiple readers or single writer.
#[derive(Debug)]
pub struct PageMapper {
    /// Virtual → Physical mapping
    /// Only contains entries for pages that have been remapped
    /// Unmapped pages use identity mapping (virtual == physical)
    mapping: Arc<RwLock<HashMap<PageId, PageId>>>,

    /// Next available virtual page ID
    next_virtual_id: Arc<RwLock<PageId>>,

    /// Dirty flag for persistence tracking
    dirty: Arc<RwLock<bool>>,
}

impl PageMapper {
    /// Create a new empty page mapper
    pub fn new() -> Self {
        Self {
            mapping: Arc::new(RwLock::new(HashMap::new())),
            next_virtual_id: Arc::new(RwLock::new(PageId::from(2))), // Start after header and superblock
            dirty: Arc::new(RwLock::new(false)),
        }
    }

    /// Create a page mapper from existing state
    ///
    /// # Arguments
    /// * `mapping` - Existing virtual-to-physical mappings
    /// * `next_virtual_id` - Next virtual page ID to allocate
    pub fn from_state(mapping: HashMap<PageId, PageId>, next_virtual_id: PageId) -> Self {
        Self {
            mapping: Arc::new(RwLock::new(mapping)),
            next_virtual_id: Arc::new(RwLock::new(next_virtual_id)),
            dirty: Arc::new(RwLock::new(false)),
        }
    }

    /// Translate virtual page ID to physical page ID
    ///
    /// Uses identity mapping (virtual == physical) for unmapped pages.
    ///
    /// # Arguments
    /// * `virtual_id` - Virtual page ID to translate
    ///
    /// # Returns
    /// Physical page ID
    pub fn translate(&self, virtual_id: PageId) -> PageId {
        let mapping = self.mapping.read();
        mapping.get(&virtual_id).copied().unwrap_or(virtual_id)
    }

    /// Allocate a new virtual page ID
    ///
    /// Returns the next available virtual page ID and increments the counter.
    ///
    /// # Returns
    /// New virtual page ID
    pub fn allocate_virtual(&self) -> PageId {
        let mut next_id = self.next_virtual_id.write();
        let id = *next_id;
        *next_id = PageId::from(next_id.as_u64() + 1);
        *self.dirty.write() = true;
        id
    }

    /// Remap a virtual page to a new physical location
    ///
    /// # Arguments
    /// * `virtual_id` - Virtual page ID
    /// * `new_physical_id` - New physical page ID
    pub fn remap(&self, virtual_id: PageId, new_physical_id: PageId) {
        let mut mapping = self.mapping.write();
        if virtual_id == new_physical_id {
            // Identity mapping - remove from map to save space
            mapping.remove(&virtual_id);
        } else {
            mapping.insert(virtual_id, new_physical_id);
        }
        *self.dirty.write() = true;
    }

    /// Remove a mapping (page freed)
    ///
    /// # Arguments
    /// * `virtual_id` - Virtual page ID to unmap
    pub fn unmap(&self, virtual_id: PageId) {
        let mut mapping = self.mapping.write();
        mapping.remove(&virtual_id);
        *self.dirty.write() = true;
    }

    /// Get the next virtual page ID (for inspection/serialization)
    pub fn next_virtual_id(&self) -> PageId {
        *self.next_virtual_id.read()
    }

    /// Check if the mapper has been modified since last clear_dirty()
    pub fn is_dirty(&self) -> bool {
        *self.dirty.read()
    }

    /// Clear the dirty flag (after persistence)
    pub fn clear_dirty(&self) {
        *self.dirty.write() = false;
    }

    /// Get the number of non-identity mappings
    pub fn mapping_count(&self) -> usize {
        self.mapping.read().len()
    }

    /// Get a snapshot of all mappings (for persistence)
    pub fn get_mappings(&self) -> HashMap<PageId, PageId> {
        self.mapping.read().clone()
    }

    /// Find all virtual pages that map to a given physical page
    ///
    /// Used during vacuum to update mappings when moving pages.
    ///
    /// # Arguments
    /// * `physical_id` - Physical page ID to search for
    ///
    /// # Returns
    /// Vector of virtual page IDs that map to this physical page
    pub fn find_virtual_pages_for_physical(&self, physical_id: PageId) -> Vec<PageId> {
        let mapping = self.mapping.read();
        let mut result = Vec::new();

        // Check explicit mappings
        for (&virtual_id, &mapped_physical_id) in mapping.iter() {
            if mapped_physical_id == physical_id {
                result.push(virtual_id);
            }
        }

        // If no explicit mappings found, check for identity mapping
        // This happens when virtual_id == physical_id and there's no explicit mapping
        if result.is_empty() && !mapping.contains_key(&physical_id) {
            // This physical page is used by its identity-mapped virtual page
            result.push(physical_id);
        }

        result
    }

    /// Serialize the page mapper to bytes
    ///
    /// Format:
    /// - Bytes 0-7: Next virtual ID (u64)
    /// - Bytes 8-15: Mapping count (u64)
    /// - Bytes 16+: Mappings as (virtual_id: u64, physical_id: u64) pairs
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Next virtual ID
        let next_id = self.next_virtual_id.read();
        bytes.extend_from_slice(&next_id.as_u64().to_le_bytes());

        // Mapping count
        let mapping = self.mapping.read();
        bytes.extend_from_slice(&(mapping.len() as u64).to_le_bytes());

        // Mappings
        for (&virtual_id, &physical_id) in mapping.iter() {
            bytes.extend_from_slice(&virtual_id.as_u64().to_le_bytes());
            bytes.extend_from_slice(&physical_id.as_u64().to_le_bytes());
        }

        bytes
    }

    /// Deserialize the page mapper from bytes
    pub fn from_bytes(bytes: &[u8]) -> PagerResult<Self> {
        if bytes.len() < 16 {
            return Err(PagerError::InsufficientBuffer {
                structure: "page mapper header".to_string(),
                expected: 16,
                actual: bytes.len(),
            });
        }

        let next_virtual_id = PageId::from(u64::from_le_bytes(bytes[0..8].try_into().unwrap()));
        let count = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;

        let mut mapping = HashMap::with_capacity(count);
        let mut offset = 16;

        for _ in 0..count {
            if offset + 16 > bytes.len() {
                return Err(PagerError::InsufficientBuffer {
                    structure: "page mapper entries".to_string(),
                    expected: offset + 16,
                    actual: bytes.len(),
                });
            }

            let virtual_id = PageId::from(u64::from_le_bytes(
                bytes[offset..offset + 8].try_into().unwrap(),
            ));
            let physical_id = PageId::from(u64::from_le_bytes(
                bytes[offset + 8..offset + 16].try_into().unwrap(),
            ));

            mapping.insert(virtual_id, physical_id);
            offset += 16;
        }

        Ok(Self::from_state(mapping, next_virtual_id))
    }

    /// Rebuild the page mapper by scanning page headers
    ///
    /// This is used for recovery when the superblock mapping is corrupted.
    /// It scans all pages in the file and reconstructs the virtual-to-physical
    /// mapping from the virtual_page_id field in each page header.
    ///
    /// # Arguments
    /// * `page_headers` - Iterator of (physical_page_id, virtual_page_id) pairs from page headers
    ///
    /// # Returns
    /// A new PageMapper with the reconstructed mapping
    pub fn rebuild_from_headers<I>(page_headers: I) -> Self
    where
        I: Iterator<Item = (PageId, PageId)>,
    {
        let mut mapping = HashMap::new();
        let mut max_virtual_id = PageId::from(2); // Start after header and superblock

        for (physical_id, virtual_id) in page_headers {
            // Skip pages with no virtual ID (identity mapping)
            if virtual_id.as_u64() == 0 {
                continue;
            }

            // Track the highest virtual ID seen
            if virtual_id.as_u64() > max_virtual_id.as_u64() {
                max_virtual_id = PageId::from(virtual_id.as_u64());
            }

            // Only store non-identity mappings
            if virtual_id != physical_id {
                mapping.insert(virtual_id, physical_id);
            }
        }

        // Next virtual ID should be one past the highest seen
        let next_virtual_id = PageId::from(max_virtual_id.as_u64() + 1);

        Self::from_state(mapping, next_virtual_id)
    }
}

impl Default for PageMapper {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for PageMapper {
    fn clone(&self) -> Self {
        Self {
            mapping: Arc::new(RwLock::new(self.mapping.read().clone())),
            next_virtual_id: Arc::new(RwLock::new(*self.next_virtual_id.read())),
            dirty: Arc::new(RwLock::new(*self.dirty.read())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_mapper_creation() {
        let mapper = PageMapper::new();
        assert_eq!(mapper.next_virtual_id(), PageId::from(2));
        assert_eq!(mapper.mapping_count(), 0);
        assert!(!mapper.is_dirty());
    }

    #[test]
    fn test_identity_mapping() {
        let mapper = PageMapper::new();
        // Unmapped pages use identity mapping
        assert_eq!(mapper.translate(PageId::from(5)), PageId::from(5));
        assert_eq!(mapper.translate(PageId::from(100)), PageId::from(100));
    }

    #[test]
    fn test_allocate_virtual() {
        let mapper = PageMapper::new();
        assert_eq!(mapper.allocate_virtual(), PageId::from(2));
        assert_eq!(mapper.allocate_virtual(), PageId::from(3));
        assert_eq!(mapper.allocate_virtual(), PageId::from(4));
        assert_eq!(mapper.next_virtual_id(), PageId::from(5));
        assert!(mapper.is_dirty());
    }

    #[test]
    fn test_remap() {
        let mapper = PageMapper::new();

        // Remap virtual page 10 to physical page 20
        mapper.remap(PageId::from(10), PageId::from(20));
        assert_eq!(mapper.translate(PageId::from(10)), PageId::from(20));
        assert_eq!(mapper.mapping_count(), 1);
        assert!(mapper.is_dirty());

        // Identity mapping removes from map
        mapper.remap(PageId::from(10), PageId::from(10));
        assert_eq!(mapper.translate(PageId::from(10)), PageId::from(10));
        assert_eq!(mapper.mapping_count(), 0);
    }

    #[test]
    fn test_unmap() {
        let mapper = PageMapper::new();

        mapper.remap(PageId::from(5), PageId::from(15));
        assert_eq!(mapper.mapping_count(), 1);

        mapper.unmap(PageId::from(5));
        assert_eq!(mapper.mapping_count(), 0);
        assert_eq!(mapper.translate(PageId::from(5)), PageId::from(5));
    }

    #[test]
    fn test_find_virtual_pages_for_physical() {
        let mapper = PageMapper::new();

        // Map virtual 10 → physical 20
        mapper.remap(PageId::from(10), PageId::from(20));

        // Map virtual 11 → physical 20 (multiple virtual pages to same physical)
        mapper.remap(PageId::from(11), PageId::from(20));

        let virtuals = mapper.find_virtual_pages_for_physical(PageId::from(20));
        assert_eq!(virtuals.len(), 2);
        assert!(virtuals.contains(&PageId::from(10)));
        assert!(virtuals.contains(&PageId::from(11)));

        // Identity mapping: physical 5 is used by virtual 5
        let virtuals = mapper.find_virtual_pages_for_physical(PageId::from(5));
        assert_eq!(virtuals.len(), 1);
        assert_eq!(virtuals[0], PageId::from(5));
    }

    #[test]
    fn test_serialization() {
        let mapper = PageMapper::new();

        mapper.allocate_virtual(); // 2
        mapper.allocate_virtual(); // 3
        mapper.remap(PageId::from(10), PageId::from(20));
        mapper.remap(PageId::from(15), PageId::from(25));

        let bytes = mapper.to_bytes();
        let deserialized = PageMapper::from_bytes(&bytes).unwrap();

        assert_eq!(deserialized.next_virtual_id(), PageId::from(4));
        assert_eq!(deserialized.mapping_count(), 2);
        assert_eq!(
            deserialized.translate(PageId::from(10)),
            PageId::from(20)
        );
        assert_eq!(
            deserialized.translate(PageId::from(15)),
            PageId::from(25)
        );
    }

    #[test]
    fn test_dirty_flag() {
        let mapper = PageMapper::new();
        assert!(!mapper.is_dirty());

        mapper.allocate_virtual();
        assert!(mapper.is_dirty());

        mapper.clear_dirty();
        assert!(!mapper.is_dirty());

        mapper.remap(PageId::from(5), PageId::from(10));
        assert!(mapper.is_dirty());
    }

    #[test]
    fn test_from_state() {
        let mut mapping = HashMap::new();
        mapping.insert(PageId::from(10), PageId::from(20));
        mapping.insert(PageId::from(15), PageId::from(25));

        let mapper = PageMapper::from_state(mapping, PageId::from(100));

        assert_eq!(mapper.next_virtual_id(), PageId::from(100));
        assert_eq!(mapper.mapping_count(), 2);
        assert_eq!(mapper.translate(PageId::from(10)), PageId::from(20));
        assert_eq!(mapper.translate(PageId::from(15)), PageId::from(25));
    }

    #[test]
    fn test_rebuild_from_headers() {
        // Simulate page headers: (physical_id, virtual_id)
        let headers = vec![
            (PageId::from(0), PageId::from(0)),   // Header page (identity)
            (PageId::from(1), PageId::from(0)),   // Superblock (identity)
            (PageId::from(2), PageId::from(0)),   // Identity mapping
            (PageId::from(3), PageId::from(0)),   // Identity mapping
            (PageId::from(10), PageId::from(5)),  // Virtual 5 → Physical 10
            (PageId::from(15), PageId::from(8)),  // Virtual 8 → Physical 15
        ];

        let mapper = PageMapper::rebuild_from_headers(headers.into_iter());

        // Should have reconstructed the non-identity mappings
        assert_eq!(mapper.mapping_count(), 2);
        assert_eq!(mapper.translate(PageId::from(5)), PageId::from(10));
        assert_eq!(mapper.translate(PageId::from(8)), PageId::from(15));

        // Next virtual ID should be one past the highest seen (8 + 1 = 9)
        assert_eq!(mapper.next_virtual_id(), PageId::from(9));

        // Identity mappings should still work
        assert_eq!(mapper.translate(PageId::from(2)), PageId::from(2));
        assert_eq!(mapper.translate(PageId::from(3)), PageId::from(3));
    }
}

// Made with Bob
