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

//! R-Tree node structures.

use super::mbr::Mbr;
use crate::pager::PageId;
use crate::snap::Snapshot;
use crate::txn::{TransactionId, VersionChain};
use crate::types::KeyBuf;
use crate::wal::LogSequenceNumber;

/// R-Tree node type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeType {
    /// Internal node containing child pointers
    Internal,
    /// Leaf node containing object IDs
    Leaf,
}

/// Entry in an internal node.
#[derive(Debug, Clone)]
pub struct InternalEntry {
    /// Minimum bounding rectangle of all children
    pub mbr: Mbr,
    /// Page ID of child node
    pub child_page_id: PageId,
}

impl InternalEntry {
    /// Create a new internal entry.
    pub fn new(mbr: Mbr, child_page_id: PageId) -> Self {
        Self { mbr, child_page_id }
    }

    /// Serialize the entry to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = self.mbr.to_bytes();
        bytes.extend_from_slice(&self.child_page_id.to_bytes());
        bytes
    }

    /// Deserialize an entry from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 8 {
            return Err("Insufficient bytes for internal entry".to_string());
        }

        let mbr_len = bytes.len() - 8;
        let mbr = Mbr::from_bytes(&bytes[..mbr_len])?;

        let page_id_bytes: [u8; 8] = bytes[mbr_len..].try_into().unwrap();
        let child_page_id = PageId::from(u64::from_le_bytes(page_id_bytes));

        Ok(Self { mbr, child_page_id })
    }
}

/// Entry in a leaf node.
#[derive(Debug, Clone)]
pub struct LeafEntry {
    /// Minimum bounding rectangle of the object
    pub mbr: Mbr,
    /// Object identifier
    pub object_id: KeyBuf,
    /// Version chain for MVCC support
    pub version_chain: VersionChain,
}

impl LeafEntry {
    /// Create a new leaf entry with a version chain.
    pub fn new(mbr: Mbr, object_id: KeyBuf, tx_id: TransactionId) -> Self {
        // Create a version chain with empty value (geometry is stored in MBR)
        let version_chain = VersionChain::new(Vec::new(), tx_id);
        Self {
            mbr,
            object_id,
            version_chain,
        }
    }

    /// Create a leaf entry from existing components.
    pub fn from_parts(mbr: Mbr, object_id: KeyBuf, version_chain: VersionChain) -> Self {
        Self {
            mbr,
            object_id,
            version_chain,
        }
    }

    /// Check if this entry is visible to the given snapshot.
    /// Returns false if the visible version is a tombstone.
    pub fn is_visible(&self, snapshot: &Snapshot) -> bool {
        match self.version_chain.find_visible_inline(snapshot) {
            Some(value) => !Self::is_tombstone(value),
            None => false,
        }
    }

    /// Check if a version value is a tombstone marker.
    /// Tombstones are marked with a single byte [0xFF].
    fn is_tombstone(value: &[u8]) -> bool {
        value == &[0xFF]
    }

    /// Create a tombstone marker value.
    fn tombstone_marker() -> Vec<u8> {
        vec![0xFF]
    }

    /// Commit this entry's version at the given LSN.
    pub fn commit(&mut self, lsn: LogSequenceNumber) {
        self.version_chain.commit(lsn);
    }

    /// Prepend a new version to this entry's chain.
    /// For deletions, use prepend_tombstone instead.
    pub fn prepend_version(&mut self, tx_id: TransactionId) {
        let old_chain = std::mem::replace(
            &mut self.version_chain,
            VersionChain::new(Vec::new(), tx_id),
        );
        self.version_chain = old_chain.prepend(Vec::new(), tx_id);
    }

    /// Prepend a tombstone version to mark this entry as deleted.
    pub fn prepend_tombstone(&mut self, tx_id: TransactionId) {
        let old_chain = std::mem::replace(
            &mut self.version_chain,
            VersionChain::new(Self::tombstone_marker(), tx_id),
        );
        self.version_chain = old_chain.prepend(Self::tombstone_marker(), tx_id);
    }

    /// Vacuum old versions from this entry's chain.
    /// Returns (removed_count, freed_refs) where freed_refs contains ValueRefs that need cleanup.
    pub fn vacuum(
        &mut self,
        min_visible_lsn: LogSequenceNumber,
    ) -> (usize, Vec<crate::types::ValueRef>) {
        self.version_chain.vacuum(min_visible_lsn)
    }

    /// Serialize the entry to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = self.mbr.to_bytes();

        // Write object ID length and data
        let id_data = self.object_id.as_ref();
        let id_len = id_data.len() as u32;
        bytes.extend_from_slice(&id_len.to_le_bytes());
        bytes.extend_from_slice(id_data);

        // Serialize version chain using postcard
        let chain_bytes = postcard::to_allocvec(&self.version_chain).unwrap_or_default();
        bytes.extend_from_slice(&(chain_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&chain_bytes);

        bytes
    }

    /// Deserialize an entry from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 4 {
            return Err("Insufficient bytes for leaf entry".to_string());
        }

        // Find where MBR ends (need to parse it to know the length)
        let dimensions = bytes[0] as usize;
        let mbr_len = 1 + dimensions * 2 * 8;

        if bytes.len() < mbr_len + 4 {
            return Err("Insufficient bytes for leaf entry".to_string());
        }

        let mbr = Mbr::from_bytes(&bytes[..mbr_len])?;

        let id_len_bytes: [u8; 4] = bytes[mbr_len..mbr_len + 4].try_into().unwrap();
        let id_len = u32::from_le_bytes(id_len_bytes) as usize;

        if bytes.len() < mbr_len + 4 + id_len {
            return Err("Insufficient bytes for object ID".to_string());
        }

        let object_id = KeyBuf(bytes[mbr_len + 4..mbr_len + 4 + id_len].to_vec());

        // Deserialize version chain
        let chain_len_offset = mbr_len + 4 + id_len;
        if bytes.len() < chain_len_offset + 4 {
            return Err("Insufficient bytes for version chain length".to_string());
        }

        let chain_len_bytes: [u8; 4] = bytes[chain_len_offset..chain_len_offset + 4]
            .try_into()
            .unwrap();
        let chain_len = u32::from_le_bytes(chain_len_bytes) as usize;

        if bytes.len() < chain_len_offset + 4 + chain_len {
            return Err("Insufficient bytes for version chain data".to_string());
        }

        let version_chain =
            postcard::from_bytes(&bytes[chain_len_offset + 4..chain_len_offset + 4 + chain_len])
                .map_err(|e| format!("Failed to deserialize version chain: {}", e))?;

        Ok(Self {
            mbr,
            object_id,
            version_chain,
        })
    }
}

/// R-Tree node (either internal or leaf).
#[derive(Debug, Clone)]
pub enum RTreeNode {
    /// Internal node with child pointers
    Internal {
        /// Entries (MBR + child page ID)
        entries: Vec<InternalEntry>,
        /// Parent page ID (0 if root)
        parent_page_id: PageId,
        /// Level in the tree (0 = leaf level)
        level: u32,
    },
    /// Leaf node with object IDs
    Leaf {
        /// Entries (MBR + object ID)
        entries: Vec<LeafEntry>,
        /// Parent page ID (0 if root)
        parent_page_id: PageId,
        /// Next leaf page for sequential scans (0 if none)
        next_leaf: PageId,
    },
}

impl RTreeNode {
    /// Create a new internal node.
    pub fn new_internal(level: u32) -> Self {
        RTreeNode::Internal {
            entries: Vec::new(),
            parent_page_id: PageId::from(0),
            level,
        }
    }

    /// Create a new leaf node.
    pub fn new_leaf() -> Self {
        RTreeNode::Leaf {
            entries: Vec::new(),
            parent_page_id: PageId::from(0),
            next_leaf: PageId::from(0),
        }
    }

    /// Get the node type.
    pub fn node_type(&self) -> NodeType {
        match self {
            RTreeNode::Internal { .. } => NodeType::Internal,
            RTreeNode::Leaf { .. } => NodeType::Leaf,
        }
    }

    /// Get the number of entries in the node.
    pub fn entry_count(&self) -> usize {
        match self {
            RTreeNode::Internal { entries, .. } => entries.len(),
            RTreeNode::Leaf { entries, .. } => entries.len(),
        }
    }

    /// Check if the node is empty.
    pub fn is_empty(&self) -> bool {
        self.entry_count() == 0
    }

    /// Get the parent page ID.
    pub fn parent_page_id(&self) -> PageId {
        match self {
            RTreeNode::Internal { parent_page_id, .. } => *parent_page_id,
            RTreeNode::Leaf { parent_page_id, .. } => *parent_page_id,
        }
    }

    /// Set the parent page ID.
    pub fn set_parent_page_id(&mut self, page_id: PageId) {
        match self {
            RTreeNode::Internal { parent_page_id, .. } => *parent_page_id = page_id,
            RTreeNode::Leaf { parent_page_id, .. } => *parent_page_id = page_id,
        }
    }

    /// Calculate the MBR that encompasses all entries in this node.
    pub fn calculate_mbr(&self, dimensions: usize) -> Mbr {
        let mut mbr = Mbr::empty(dimensions);

        match self {
            RTreeNode::Internal { entries, .. } => {
                for entry in entries {
                    mbr.expand(&entry.mbr);
                }
            }
            RTreeNode::Leaf { entries, .. } => {
                for entry in entries {
                    mbr.expand(&entry.mbr);
                }
            }
        }

        mbr
    }

    /// Add an internal entry to this node.
    /// Returns an error if this is not an internal node.
    pub fn add_internal_entry(&mut self, entry: InternalEntry) -> Result<(), String> {
        match self {
            RTreeNode::Internal { entries, .. } => {
                entries.push(entry);
                Ok(())
            }
            RTreeNode::Leaf { .. } => Err("Cannot add internal entry to leaf node".to_string()),
        }
    }

    /// Add a leaf entry to this node.
    /// Returns an error if this is not a leaf node.
    pub fn add_leaf_entry(&mut self, entry: LeafEntry) -> Result<(), String> {
        match self {
            RTreeNode::Leaf { entries, .. } => {
                entries.push(entry);
                Ok(())
            }
            RTreeNode::Internal { .. } => Err("Cannot add leaf entry to internal node".to_string()),
        }
    }

    /// Remove an entry at the given index.
    pub fn remove_entry(&mut self, index: usize) -> Result<(), String> {
        match self {
            RTreeNode::Internal { entries, .. } => {
                if index >= entries.len() {
                    return Err("Index out of bounds".to_string());
                }
                entries.remove(index);
                Ok(())
            }
            RTreeNode::Leaf { entries, .. } => {
                if index >= entries.len() {
                    return Err("Index out of bounds".to_string());
                }
                entries.remove(index);
                Ok(())
            }
        }
    }

    /// Get internal entries (returns None if this is a leaf node).
    pub fn internal_entries(&self) -> Option<&Vec<InternalEntry>> {
        match self {
            RTreeNode::Internal { entries, .. } => Some(entries),
            RTreeNode::Leaf { .. } => None,
        }
    }

    /// Get mutable internal entries (returns None if this is a leaf node).
    pub fn internal_entries_mut(&mut self) -> Option<&mut Vec<InternalEntry>> {
        match self {
            RTreeNode::Internal { entries, .. } => Some(entries),
            RTreeNode::Leaf { .. } => None,
        }
    }

    /// Get leaf entries (returns None if this is an internal node).
    pub fn leaf_entries(&self) -> Option<&Vec<LeafEntry>> {
        match self {
            RTreeNode::Leaf { entries, .. } => Some(entries),
            RTreeNode::Internal { .. } => None,
        }
    }

    /// Get mutable leaf entries (returns None if this is an internal node).
    pub fn leaf_entries_mut(&mut self) -> Option<&mut Vec<LeafEntry>> {
        match self {
            RTreeNode::Leaf { entries, .. } => Some(entries),
            RTreeNode::Internal { .. } => None,
        }
    }

    /// Get the level of this node (0 for leaf nodes).
    pub fn level(&self) -> u32 {
        match self {
            RTreeNode::Internal { level, .. } => *level,
            RTreeNode::Leaf { .. } => 0,
        }
    }

    /// Serialize the node to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        match self {
            RTreeNode::Internal {
                entries,
                parent_page_id,
                level,
            } => {
                // Node type (0 = internal)
                bytes.push(0);
                // Level
                bytes.extend_from_slice(&level.to_le_bytes());
                // Parent page ID
                bytes.extend_from_slice(&PageId::to_bytes(parent_page_id));
                // Entry count
                bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());

                // Entries
                for entry in entries {
                    bytes.extend_from_slice(&entry.mbr.to_bytes());
                    bytes.extend_from_slice(&entry.child_page_id.to_bytes());
                }
            }
            RTreeNode::Leaf {
                entries,
                parent_page_id,
                next_leaf,
            } => {
                // Node type (1 = leaf)
                bytes.push(1);
                // Parent page ID
                bytes.extend_from_slice(&PageId::to_bytes(parent_page_id));
                // Next leaf page ID
                bytes.extend_from_slice(&PageId::to_bytes(next_leaf));
                // Entry count
                bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());

                // Entries
                for entry in entries {
                    let entry_bytes = entry.to_bytes();
                    bytes.extend_from_slice(&(entry_bytes.len() as u16).to_le_bytes());
                    bytes.extend_from_slice(&entry_bytes);
                }
            }
        }

        bytes
    }

    /// Deserialize a node from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty() {
            return Err("Empty byte array".to_string());
        }

        let node_type = bytes[0];
        let mut offset = 1;

        match node_type {
            0 => {
                // Internal node
                if bytes.len() < 1 + 4 + 8 + 4 {
                    return Err("Insufficient bytes for internal node header".to_string());
                }

                let level_bytes: [u8; 4] = bytes[offset..offset + 4].try_into().unwrap();
                let level = u32::from_le_bytes(level_bytes);
                offset += 4;

                let parent_bytes: [u8; 8] = bytes[offset..offset + 8].try_into().unwrap();
                let parent_page_id = <PageId as From<u64>>::from(u64::from_le_bytes(parent_bytes));
                offset += 8;

                let count_bytes: [u8; 4] = bytes[offset..offset + 4].try_into().unwrap();
                let entry_count = u32::from_le_bytes(count_bytes) as usize;
                offset += 4;

                let entry_size = 1 + (2 * bytes[17] as usize * 8) + 8;
                let mut entries = Vec::with_capacity(entry_count);
                for _ in 0..entry_count {
                    if offset + entry_size > bytes.len() {
                        return Err("Insufficient bytes for internal entry data".to_string());
                    }

                    let entry = InternalEntry::from_bytes(&bytes[offset..offset + entry_size])?;
                    entries.push(entry);
                    offset += entry_size;
                }

                Ok(RTreeNode::Internal {
                    entries,
                    parent_page_id,
                    level,
                })
            }
            1 => {
                // Leaf node
                if bytes.len() < 1 + 8 + 8 + 4 {
                    return Err("Insufficient bytes for leaf node header".to_string());
                }

                let parent_bytes: [u8; 8] = bytes[offset..offset + 8].try_into().unwrap();
                let parent_page_id = <PageId as From<u64>>::from(u64::from_le_bytes(parent_bytes));
                offset += 8;

                let next_bytes: [u8; 8] = bytes[offset..offset + 8].try_into().unwrap();
                let next_leaf = <PageId as From<u64>>::from(u64::from_le_bytes(next_bytes));
                offset += 8;

                let count_bytes: [u8; 4] = bytes[offset..offset + 4].try_into().unwrap();
                let entry_count = u32::from_le_bytes(count_bytes) as usize;
                offset += 4;

                let mut entries = Vec::with_capacity(entry_count);
                for _ in 0..entry_count {
                    if offset + 2 > bytes.len() {
                        return Err("Insufficient bytes for leaf entry length".to_string());
                    }

                    let len_bytes: [u8; 2] = bytes[offset..offset + 2].try_into().unwrap();
                    let entry_len = u16::from_le_bytes(len_bytes) as usize;
                    offset += 2;

                    if offset + entry_len > bytes.len() {
                        return Err("Insufficient bytes for leaf entry data".to_string());
                    }

                    let entry = LeafEntry::from_bytes(&bytes[offset..offset + entry_len])?;
                    entries.push(entry);
                    offset += entry_len;
                }

                Ok(RTreeNode::Leaf {
                    entries,
                    parent_page_id,
                    next_leaf,
                })
            }
            _ => Err(format!("Invalid node type: {}", node_type)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::GeoPoint;

    #[test]
    fn test_internal_entry_serialization() {
        let mbr = Mbr::from_points_2d(GeoPoint { x: 0.0, y: 0.0 }, GeoPoint { x: 1.0, y: 1.0 });
        let entry = InternalEntry::new(mbr, PageId::from(42));

        let bytes = entry.to_bytes();
        let deserialized = InternalEntry::from_bytes(&bytes).unwrap();

        assert_eq!(entry.mbr, deserialized.mbr);
        assert_eq!(entry.child_page_id, deserialized.child_page_id);
    }

    #[test]
    fn test_leaf_entry_serialization() {
        let mbr = Mbr::from_points_2d(GeoPoint { x: 0.0, y: 0.0 }, GeoPoint { x: 1.0, y: 1.0 });
        let object_id = KeyBuf(b"test_object".to_vec());
        let tx_id = TransactionId::from(1);
        let entry = LeafEntry::new(mbr, object_id.clone(), tx_id);

        let bytes = entry.to_bytes();
        let deserialized = LeafEntry::from_bytes(&bytes).unwrap();

        assert_eq!(entry.mbr, deserialized.mbr);
        assert_eq!(entry.object_id, deserialized.object_id);
    }

    #[test]
    fn test_node_serialization() {
        let mut node = RTreeNode::new_leaf();

        let mbr = Mbr::from_points_2d(GeoPoint { x: 0.0, y: 0.0 }, GeoPoint { x: 1.0, y: 1.0 });
        let tx_id = TransactionId::from(1);
        let entry = LeafEntry::new(mbr, KeyBuf(b"test".to_vec()), tx_id);
        node.add_leaf_entry(entry).unwrap();

        let bytes = node.to_bytes();
        let deserialized = RTreeNode::from_bytes(&bytes).unwrap();

        assert_eq!(node.node_type(), deserialized.node_type());
        assert_eq!(node.entry_count(), deserialized.entry_count());
    }

    #[test]
    fn test_calculate_mbr() {
        let mut node = RTreeNode::new_leaf();

        let mbr1 = Mbr::from_points_2d(GeoPoint { x: 0.0, y: 0.0 }, GeoPoint { x: 1.0, y: 1.0 });
        let mbr2 = Mbr::from_points_2d(GeoPoint { x: 2.0, y: 2.0 }, GeoPoint { x: 3.0, y: 3.0 });
        let tx_id = TransactionId::from(1);

        node.add_leaf_entry(LeafEntry::new(mbr1, KeyBuf(b"obj1".to_vec()), tx_id))
            .unwrap();
        node.add_leaf_entry(LeafEntry::new(mbr2, KeyBuf(b"obj2".to_vec()), tx_id))
            .unwrap();

        let combined_mbr = node.calculate_mbr(2);
        assert_eq!(combined_mbr.min[0], 0.0);
        assert_eq!(combined_mbr.min[1], 0.0);
        assert_eq!(combined_mbr.max[0], 3.0);
        assert_eq!(combined_mbr.max[1], 3.0);
    }
}

// Made with Bob
