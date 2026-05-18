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

//! Paged B-Tree implementation for disk-backed storage.
//!
//! This module provides a persistent B-Tree implementation that uses the pager
//! layer for disk storage. Features include:
//! - Disk-backed B-Tree with configurable order
//! - MVCC support through version chains
//! - Efficient range scans and point lookups
//! - Node split and merge operations
//! - Integration with the pager for page management
//!
//! The B-Tree uses two types of nodes:
//! - Internal nodes: Store keys and child page pointers
//! - Leaf nodes: Store key-value pairs with version chains

use crate::pager::{Page, PageId, PageType, Pager};
use crate::snap::Snapshot;
use crate::table::{
    BatchOps, BatchReport, DenseOrdered, Flushable, KeyStatistics, MutableTable, OrderedScan,
    PointLookup, SearchableTable, SpecialtyTableCapabilities, SpecialtyTableCursor,
    SpecialtyTableStats, Table, TableCapabilities, TableCursor, TableEngineKind, TableReader,
    TableResult, TableStatistics, TableWriter, ValueStatistics, VerificationReport, WriteBatch,
};
use crate::txn::{TransactionId, VersionChain};
use crate::types::{Bound, ScanBounds, TableId, ValueBuf};
use crate::vfs::FileSystem;
use crate::wal::LogSequenceNumber;
use metrics::{counter, histogram};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tracing::{debug, instrument};

/// Default B-Tree order (maximum keys per node).
const DEFAULT_ORDER: usize = 64;

/// Minimum keys per node (except root).
const MIN_KEYS: usize = DEFAULT_ORDER / 2;

// =============================================================================
// Statistics Structures
// =============================================================================

/// Internal structure to accumulate statistics during tree traversal.
struct TreeStatistics {
    row_count: u64,
    total_size_bytes: u64,
    tree_depth: usize,
    internal_node_count: u64,
    leaf_node_count: u64,
    key_min_size: usize,
    key_max_size: usize,
    key_total_size: u64,
    value_min_size: usize,
    value_max_size: usize,
    value_total_size: u64,
}

impl TreeStatistics {
    fn new() -> Self {
        Self {
            row_count: 0,
            total_size_bytes: 0,
            tree_depth: 0,
            internal_node_count: 0,
            leaf_node_count: 0,
            key_min_size: usize::MAX,
            key_max_size: 0,
            key_total_size: 0,
            value_min_size: usize::MAX,
            value_max_size: 0,
            value_total_size: 0,
        }
    }

    fn key_avg_size(&self) -> f64 {
        if self.row_count == 0 {
            0.0
        } else {
            self.key_total_size as f64 / self.row_count as f64
        }
    }

    fn value_avg_size(&self) -> f64 {
        if self.row_count == 0 {
            0.0
        } else {
            self.value_total_size as f64 / self.row_count as f64
        }
    }
}

// =============================================================================
// Node Structures
// =============================================================================

/// B-Tree node type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeType {
    Internal,
    Leaf,
}

/// Internal node entry (key + child pointer).
#[derive(Debug, Clone)]
struct InternalEntry {
    key: Vec<u8>,
    child_page_id: PageId,
}

/// Leaf node entry (key + version chain).
#[derive(Debug, Clone)]
struct LeafEntry {
    key: Vec<u8>,
    chain: VersionChain,
}

/// B-Tree node (either internal or leaf).
#[derive(Debug, Clone)]
enum BTreeNode {
    Internal {
        /// Keys and child pointers (keys.len() == children.len() - 1)
        entries: Vec<InternalEntry>,
        /// Rightmost child pointer
        rightmost_child: PageId,
    },
    Leaf {
        /// Key-value pairs with version chains
        entries: Vec<LeafEntry>,
        /// Next leaf page for sequential scans (0 if none)
        next_leaf: PageId,
    },
}

impl BTreeNode {
    /// Create a new internal node.
    fn new_internal() -> Self {
        BTreeNode::Internal {
            entries: Vec::new(),
            rightmost_child: PageId::from(0),
        }
    }

    /// Create a new leaf node.
    fn new_leaf() -> Self {
        BTreeNode::Leaf {
            entries: Vec::new(),
            next_leaf: PageId::from(0),
        }
    }

    /// Get the node type.
    fn node_type(&self) -> NodeType {
        match self {
            BTreeNode::Internal { .. } => NodeType::Internal,
            BTreeNode::Leaf { .. } => NodeType::Leaf,
        }
    }

    /// Get the number of keys in the node.
    fn key_count(&self) -> usize {
        match self {
            BTreeNode::Internal { entries, .. } => entries.len(),
            BTreeNode::Leaf { entries, .. } => entries.len(),
        }
    }

    /// Check if the node is full.
    fn is_full(&self) -> bool {
        self.key_count() >= DEFAULT_ORDER
    }

    /// Check if the node has minimum keys.
    fn has_minimum_keys(&self) -> bool {
        self.key_count() >= MIN_KEYS
    }

    /// Serialize the node to bytes.
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Write node type (1 byte)
        match self {
            BTreeNode::Internal {
                entries,
                rightmost_child,
            } => {
                bytes.push(0); // Internal node

                // Write number of entries (4 bytes)
                bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());

                // Write rightmost child (8 bytes)
                bytes.extend_from_slice(&rightmost_child.to_bytes());

                // Write each entry
                for entry in entries {
                    // Key length (4 bytes)
                    bytes.extend_from_slice(&(entry.key.len() as u32).to_le_bytes());
                    // Key data
                    bytes.extend_from_slice(&entry.key);
                    // Child page ID (8 bytes)
                    bytes.extend_from_slice(&entry.child_page_id.to_bytes());
                }
            }
            BTreeNode::Leaf { entries, next_leaf } => {
                bytes.push(1); // Leaf node

                // Write number of entries (4 bytes)
                bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());

                // Write next leaf pointer (8 bytes)
                bytes.extend_from_slice(&next_leaf.to_bytes());

                // Write each entry
                for entry in entries {
                    // Key length (4 bytes)
                    bytes.extend_from_slice(&(entry.key.len() as u32).to_le_bytes());
                    // Key data
                    bytes.extend_from_slice(&entry.key);
                    // Version chain (serialized using postcard)
                    let chain_bytes = postcard::to_allocvec(&entry.chain).unwrap();
                    bytes.extend_from_slice(&(chain_bytes.len() as u32).to_le_bytes());
                    bytes.extend_from_slice(&chain_bytes);
                }
            }
        }

        bytes
    }

    /// Deserialize the node from bytes.
    fn from_bytes(bytes: &[u8]) -> TableResult<Self> {
        if bytes.is_empty() {
            return Err(crate::table::TableError::corruption(
                "BTreeNode::from_bytes",
                "empty_data",
                "Empty node data",
            ));
        }

        let node_type = bytes[0];
        let mut offset = 1;

        match node_type {
            0 => {
                // Internal node
                if bytes.len() < offset + 4 {
                    return Err(crate::table::TableError::corruption(
                        "BTreeNode::from_bytes",
                        "truncated_data",
                        "Insufficient data for entry count",
                    ));
                }
                let entry_count =
                    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;

                if bytes.len() < offset + 8 {
                    return Err(crate::table::TableError::corruption(
                        "BTreeNode::from_bytes",
                        "truncated_data",
                        "Insufficient data for rightmost child",
                    ));
                }
                let rightmost_child = PageId::from(u64::from_le_bytes(
                    bytes[offset..offset + 8].try_into().unwrap(),
                ));
                offset += 8;

                let mut entries = Vec::with_capacity(entry_count);
                for _ in 0..entry_count {
                    if bytes.len() < offset + 4 {
                        return Err(crate::table::TableError::corruption(
                            "BTreeNode::from_bytes",
                            "truncated_data",
                            "Insufficient data for key length",
                        ));
                    }
                    let key_len =
                        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
                    offset += 4;

                    if bytes.len() < offset + key_len {
                        return Err(crate::table::TableError::corruption(
                            "BTreeNode::from_bytes",
                            "truncated_data",
                            "Insufficient data for key",
                        ));
                    }
                    let key = bytes[offset..offset + key_len].to_vec();
                    offset += key_len;

                    if bytes.len() < offset + 8 {
                        return Err(crate::table::TableError::corruption(
                            "BTreeNode::from_bytes",
                            "truncated_data",
                            "Insufficient data for child page ID",
                        ));
                    }
                    let child_page_id = PageId::from(u64::from_le_bytes(
                        bytes[offset..offset + 8].try_into().unwrap(),
                    ));
                    offset += 8;

                    entries.push(InternalEntry { key, child_page_id });
                }

                Ok(BTreeNode::Internal {
                    entries,
                    rightmost_child,
                })
            }
            1 => {
                // Leaf node
                if bytes.len() < offset + 4 {
                    return Err(crate::table::TableError::corruption(
                        "BTreeNode::from_bytes",
                        "truncated_data",
                        "Insufficient data for entry count",
                    ));
                }
                let entry_count =
                    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;

                if bytes.len() < offset + 8 {
                    return Err(crate::table::TableError::corruption(
                        "BTreeNode::from_bytes",
                        "truncated_data",
                        "Insufficient data for next leaf",
                    ));
                }
                let next_leaf = PageId::from(u64::from_le_bytes(
                    bytes[offset..offset + 8].try_into().unwrap(),
                ));
                offset += 8;

                let mut entries = Vec::with_capacity(entry_count);
                for _ in 0..entry_count {
                    if bytes.len() < offset + 4 {
                        return Err(crate::table::TableError::corruption(
                            "BTreeNode::from_bytes",
                            "truncated_data",
                            "Insufficient data for key length",
                        ));
                    }
                    let key_len =
                        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
                    offset += 4;

                    if bytes.len() < offset + key_len {
                        return Err(crate::table::TableError::corruption(
                            "BTreeNode::from_bytes",
                            "truncated_data",
                            "Insufficient data for key",
                        ));
                    }
                    let key = bytes[offset..offset + key_len].to_vec();
                    offset += key_len;

                    if bytes.len() < offset + 4 {
                        return Err(crate::table::TableError::corruption(
                            "BTreeNode::from_bytes",
                            "truncated_data",
                            "Insufficient data for chain length",
                        ));
                    }
                    let chain_len =
                        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
                    offset += 4;

                    if bytes.len() < offset + chain_len {
                        return Err(crate::table::TableError::corruption(
                            "BTreeNode::from_bytes",
                            "truncated_data",
                            "Insufficient data for version chain",
                        ));
                    }
                    let chain: VersionChain =
                        postcard::from_bytes(&bytes[offset..offset + chain_len]).map_err(|e| {
                            crate::table::TableError::corruption(
                                "BTreeNode::from_bytes",
                                "deserialization_error",
                                format!("Failed to deserialize version chain: {}", e),
                            )
                        })?;
                    offset += chain_len;

                    entries.push(LeafEntry { key, chain });
                }

                Ok(BTreeNode::Leaf { entries, next_leaf })
            }
            _ => Err(crate::table::TableError::corruption(
                "BTreeNode::from_bytes",
                "invalid_node_type",
                format!("Invalid node type: {}", node_type),
            )),
        }
    }
}

// =============================================================================
// Paged B-Tree
// =============================================================================

/// Paged B-Tree table using the pager for disk storage.
pub struct PagedBTree<FS: FileSystem> {
    id: TableId,
    name: String,
    pager: Arc<Pager<FS>>,
    /// Root page ID wrapped in Arc<RwLock> to allow atomic updates during root splits
    root_page_id: Arc<RwLock<PageId>>,
    /// Row count wrapped in Arc<RwLock> for atomic updates
    row_count: Arc<RwLock<u64>>,
}

impl<FS: FileSystem> PagedBTree<FS> {
    /// Create a new paged B-Tree table.
    pub fn new(id: TableId, name: String, pager: Arc<Pager<FS>>) -> TableResult<Self> {
        // Allocate root page (initially a leaf)
        let root_page_id = pager.allocate_page(PageType::BTreeLeaf)?;
        let root_node = BTreeNode::new_leaf();

        // Write root node to disk
        let mut page = Page::new(
            root_page_id,
            PageType::BTreeLeaf,
            pager.page_size().data_size(),
        );
        page.data_mut().extend_from_slice(&root_node.to_bytes());
        pager.write_page(&page)?;
        pager.set_root_btree_page(root_page_id)?;
        pager.set_btree_row_count(0)?;

        Ok(Self {
            id,
            name,
            pager,
            root_page_id: Arc::new(RwLock::new(root_page_id)),
            row_count: Arc::new(RwLock::new(0)),
        })
    }

    /// Open an existing paged B-Tree table.
    pub fn open(id: TableId, name: String, pager: Arc<Pager<FS>>, root_page_id: PageId) -> Self {
        let row_count = pager.btree_row_count();
        Self {
            id,
            name,
            pager,
            root_page_id: Arc::new(RwLock::new(root_page_id)),
            row_count: Arc::new(RwLock::new(row_count)),
        }
    }

    /// Get the current root page ID.
    pub fn get_root_page_id(&self) -> PageId {
        *self.root_page_id.read().unwrap()
    }

    /// Update the root page ID (used during root splits).
    fn set_root_page_id(&self, new_root: PageId) -> TableResult<()> {
        self.pager
            .set_root_btree_page(new_root)
            .map_err(crate::table::TableError::from)?;
        *self.root_page_id.write().unwrap() = new_root;
        Ok(())
    }

    /// Get the current row count.
    fn get_row_count(&self) -> u64 {
        *self.row_count.read().unwrap()
    }

    /// Increment the row count and persist it.
    fn increment_row_count(&self) -> TableResult<()> {
        let new_count = {
            let mut count = self.row_count.write().unwrap();
            *count += 1;
            *count
        };
        self.pager
            .set_btree_row_count(new_count)
            .map_err(crate::table::TableError::from)?;
        Ok(())
    }

    /// Decrement the row count and persist it.
    fn decrement_row_count(&self) -> TableResult<()> {
        let new_count = {
            let mut count = self.row_count.write().unwrap();
            if *count > 0 {
                *count -= 1;
            }
            *count
        };
        self.pager
            .set_btree_row_count(new_count)
            .map_err(crate::table::TableError::from)?;
        Ok(())
    }

    /// Read a node from disk.
    #[instrument(skip(self), fields(page_id = %page_id))]
    fn read_node(&self, page_id: PageId) -> TableResult<BTreeNode> {
        let start = Instant::now();
        let page = self.pager.read_page(page_id)?;
        let result = BTreeNode::from_bytes(page.data());

        if result.is_ok() {
            counter!("btree.node_read").increment(1);
            histogram!("btree.read_duration").record(start.elapsed().as_secs_f64());
        }

        result
    }

    /// Write a node to disk.
    #[instrument(skip(self, node), fields(page_id = %page_id))]
    fn write_node(&self, page_id: PageId, node: &BTreeNode) -> TableResult<()> {
        let start = Instant::now();
        debug!("Writing BTree node");

        let page_type = match node.node_type() {
            NodeType::Internal => PageType::BTreeInternal,
            NodeType::Leaf => PageType::BTreeLeaf,
        };

        let mut page = Page::new(page_id, page_type, self.pager.page_size().data_size());
        page.data_mut().extend_from_slice(&node.to_bytes());
        self.pager.write_page(&page)?;

        counter!("btree.node_write").increment(1);
        histogram!("btree.write_duration").record(start.elapsed().as_secs_f64());
        Ok(())
    }

    /// Search for a key in the tree, returning the leaf page ID and position.
    #[instrument(skip(self, key), fields(key_len = key.len()))]
    fn search(&self, key: &[u8]) -> TableResult<(PageId, usize)> {
        let start = Instant::now();
        debug!("BTree search operation");

        let (leaf_page_id, pos, _path) = self.search_with_path(key)?;

        histogram!("btree.search_duration").record(start.elapsed().as_secs_f64());
        Ok((leaf_page_id, pos))
    }

    /// Search for a key in the tree, tracking the path from root to leaf.
    /// Returns the leaf page ID, position, and path (list of (parent_page_id, child_page_id) tuples).
    fn search_with_path(&self, key: &[u8]) -> TableResult<(PageId, usize, Vec<(PageId, PageId)>)> {
        let mut current_page_id = self.get_root_page_id();
        let mut path = Vec::new();

        loop {
            let node = self.read_node(current_page_id)?;

            match node {
                BTreeNode::Internal {
                    entries,
                    rightmost_child,
                } => {
                    // Binary search for the appropriate child
                    // In our representation: entries[i].child_page_id contains keys < entries[i].key
                    // So for a key >= entries[i].key, we need to go to the next child
                    let pos = entries.binary_search_by(|e| e.key.as_slice().cmp(key));
                    let child_page_id = match pos {
                        Ok(idx) => {
                            // Found exact match at idx
                            // Keys >= entries[idx].key go to the right of this entry
                            if idx + 1 < entries.len() {
                                entries[idx + 1].child_page_id
                            } else {
                                rightmost_child
                            }
                        }
                        Err(idx) => {
                            // Key would be inserted at position idx
                            // This means key < entries[idx].key (or idx == len)
                            if idx < entries.len() {
                                entries[idx].child_page_id
                            } else {
                                rightmost_child
                            }
                        }
                    };

                    // Track this parent and which child we're going to
                    path.push((current_page_id, child_page_id));
                    current_page_id = child_page_id;
                }
                BTreeNode::Leaf { entries, .. } => {
                    // Found the leaf node
                    let pos = entries.binary_search_by(|e| e.key.as_slice().cmp(key));
                    let idx = match pos {
                        Ok(i) => i,
                        Err(i) => i,
                    };
                    return Ok((current_page_id, idx, path));
                }
            }
        }
    }

    /// Get a value for a key at a specific snapshot.
    fn get_internal(
        &self,
        key: &[u8],
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Option<ValueBuf>> {
        let (leaf_page_id, pos) = self.search(key)?;
        let node = self.read_node(leaf_page_id)?;

        if let BTreeNode::Leaf { entries, .. } = node
            && pos < entries.len()
            && entries[pos].key == key
        {
            let snapshot = Snapshot::new(
                crate::snap::SnapshotId::from(0),
                String::new(),
                snapshot_lsn,
                0,
                0,
                Vec::new(),
            );
            if let Some(value) = entries[pos].chain.find_visible_version(&snapshot) {
                if value.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(ValueBuf(value.to_vec())));
            }
        }

        Ok(None)
    }

    /// Split a full node into two nodes.
    /// Returns the new right sibling page ID and the median key that should be promoted to parent.
    fn split_node(&self, page_id: PageId, node: &BTreeNode) -> TableResult<(PageId, Vec<u8>)> {
        let mid = node.key_count() / 2;

        match node {
            BTreeNode::Internal {
                entries,
                rightmost_child,
            } => {
                // Split internal node
                let median_key = entries[mid].key.clone();

                // Left node keeps entries [0..mid]
                let left_entries = entries[..mid].to_vec();
                let left_rightmost = entries[mid].child_page_id;

                // Right node gets entries [mid+1..]
                let right_entries = entries[mid + 1..].to_vec();
                let right_rightmost = *rightmost_child;

                // Create new right sibling
                let right_page_id = self.pager.allocate_page(PageType::BTreeInternal)?;
                let right_node = BTreeNode::Internal {
                    entries: right_entries,
                    rightmost_child: right_rightmost,
                };

                // Update left node (original page)
                let left_node = BTreeNode::Internal {
                    entries: left_entries,
                    rightmost_child: left_rightmost,
                };

                // Write both nodes
                self.write_node(page_id, &left_node)?;
                self.write_node(right_page_id, &right_node)?;

                Ok((right_page_id, median_key))
            }
            BTreeNode::Leaf { entries, next_leaf } => {
                // Split leaf node
                let median_key = entries[mid].key.clone();

                // Left node keeps entries [0..mid]
                let left_entries = entries[..mid].to_vec();

                // Right node gets entries [mid..]
                let right_entries = entries[mid..].to_vec();

                // Create new right sibling
                let right_page_id = self.pager.allocate_page(PageType::BTreeLeaf)?;
                let right_node = BTreeNode::Leaf {
                    entries: right_entries,
                    next_leaf: *next_leaf,
                };

                // Update left node to point to new right sibling
                let left_node = BTreeNode::Leaf {
                    entries: left_entries,
                    next_leaf: right_page_id,
                };

                // Write both nodes
                self.write_node(page_id, &left_node)?;
                self.write_node(right_page_id, &right_node)?;

                Ok((right_page_id, median_key))
            }
        }
    }

    /// Merge two adjacent nodes (left and right).
    /// The right node is merged into the left node, and the right node is freed.
    /// Returns true if merge was successful.
    fn merge_nodes(
        &self,
        left_page_id: PageId,
        right_page_id: PageId,
        separator_key: &[u8],
    ) -> TableResult<bool> {
        let left_node = self.read_node(left_page_id)?;
        let right_node = self.read_node(right_page_id)?;

        // Ensure nodes are of the same type
        if left_node.node_type() != right_node.node_type() {
            return Ok(false);
        }

        match (left_node, right_node) {
            (
                BTreeNode::Internal {
                    entries: left_entries,
                    rightmost_child: left_rightmost,
                },
                BTreeNode::Internal {
                    entries: right_entries,
                    rightmost_child: right_rightmost,
                },
            ) => {
                // Check if merge is possible
                if left_entries.len() + right_entries.len() + 1 > DEFAULT_ORDER {
                    return Ok(false);
                }

                let mut merged_entries = left_entries;

                // Add separator key with left's rightmost child
                merged_entries.push(InternalEntry {
                    key: separator_key.to_vec(),
                    child_page_id: left_rightmost,
                });

                // Add all right entries
                merged_entries.extend(right_entries);

                // Create merged node
                let merged_node = BTreeNode::Internal {
                    entries: merged_entries,
                    rightmost_child: right_rightmost,
                };

                // Write merged node and free right node
                self.write_node(left_page_id, &merged_node)?;
                self.pager.free_page(right_page_id)?;

                Ok(true)
            }
            (
                BTreeNode::Leaf {
                    entries: left_entries,
                    ..
                },
                BTreeNode::Leaf {
                    entries: right_entries,
                    next_leaf: right_next,
                },
            ) => {
                // Check if merge is possible
                if left_entries.len() + right_entries.len() > DEFAULT_ORDER {
                    return Ok(false);
                }

                let mut merged_entries = left_entries;

                // Add all right entries
                merged_entries.extend(right_entries);

                // Create merged node (left now points to right's next)
                let merged_node = BTreeNode::Leaf {
                    entries: merged_entries,
                    next_leaf: right_next,
                };

                // Write merged node and free right node
                self.write_node(left_page_id, &merged_node)?;
                self.pager.free_page(right_page_id)?;

                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Redistribute keys between two adjacent nodes to balance them.
    /// Returns true if redistribution was successful.
    fn redistribute_keys(
        &self,
        left_page_id: PageId,
        right_page_id: PageId,
        separator_key: &[u8],
    ) -> TableResult<Option<Vec<u8>>> {
        let left_node = self.read_node(left_page_id)?;
        let right_node = self.read_node(right_page_id)?;

        // Ensure nodes are of the same type
        if left_node.node_type() != right_node.node_type() {
            return Ok(None);
        }

        let total_keys = left_node.key_count() + right_node.key_count();
        let target_left = total_keys / 2;

        match (left_node, right_node) {
            (
                BTreeNode::Internal {
                    entries: left_entries,
                    rightmost_child: left_rightmost,
                },
                BTreeNode::Internal {
                    entries: right_entries,
                    rightmost_child: right_rightmost,
                },
            ) => {
                let mut left_entries = left_entries;
                let mut right_entries = right_entries;
                let left_count = left_entries.len();

                if left_count < target_left {
                    // Move keys from right to left
                    let to_move = target_left - left_count;

                    // Add separator with left's rightmost as child
                    left_entries.push(InternalEntry {
                        key: separator_key.to_vec(),
                        child_page_id: left_rightmost,
                    });

                    // Move entries from right to left
                    for _ in 0..to_move.saturating_sub(1) {
                        if let Some(entry) = right_entries.first() {
                            left_entries.push(entry.clone());
                            right_entries.remove(0);
                        }
                    }

                    // New separator is the first key in right
                    let new_separator = if let Some(entry) = right_entries.first() {
                        let sep = entry.key.clone();
                        let new_left_rightmost = entry.child_page_id;
                        right_entries.remove(0);

                        // Update nodes
                        let new_left = BTreeNode::Internal {
                            entries: left_entries,
                            rightmost_child: new_left_rightmost,
                        };
                        let new_right = BTreeNode::Internal {
                            entries: right_entries,
                            rightmost_child: right_rightmost,
                        };

                        self.write_node(left_page_id, &new_left)?;
                        self.write_node(right_page_id, &new_right)?;

                        Some(sep)
                    } else {
                        None
                    };

                    Ok(new_separator)
                } else {
                    // Move keys from left to right
                    let to_move = left_count - target_left;

                    // Take entries from end of left
                    let mut moved_entries = Vec::new();
                    for _ in 0..to_move {
                        if let Some(entry) = left_entries.pop() {
                            moved_entries.insert(0, entry);
                        }
                    }

                    if moved_entries.is_empty() {
                        return Ok(None);
                    }

                    // New separator is the last moved key
                    let new_separator = moved_entries.last().unwrap().key.clone();
                    let new_left_rightmost = moved_entries.last().unwrap().child_page_id;
                    moved_entries.pop();

                    // Add separator to right with old left_rightmost
                    moved_entries.push(InternalEntry {
                        key: separator_key.to_vec(),
                        child_page_id: left_rightmost,
                    });
                    moved_entries.extend(right_entries);

                    // Update nodes
                    let new_left = BTreeNode::Internal {
                        entries: left_entries,
                        rightmost_child: new_left_rightmost,
                    };
                    let new_right = BTreeNode::Internal {
                        entries: moved_entries,
                        rightmost_child: right_rightmost,
                    };

                    self.write_node(left_page_id, &new_left)?;
                    self.write_node(right_page_id, &new_right)?;

                    Ok(Some(new_separator))
                }
            }
            (
                BTreeNode::Leaf {
                    entries: left_entries,
                    next_leaf: left_next,
                },
                BTreeNode::Leaf {
                    entries: right_entries,
                    next_leaf: right_next,
                },
            ) => {
                let mut left_entries = left_entries;
                let mut right_entries = right_entries;
                let left_count = left_entries.len();

                if left_count < target_left {
                    // Move keys from right to left
                    let to_move = target_left - left_count;

                    for _ in 0..to_move {
                        if let Some(entry) = right_entries.first() {
                            left_entries.push(entry.clone());
                            right_entries.remove(0);
                        }
                    }

                    // New separator is the first key in right
                    let new_separator = right_entries.first().map(|e| e.key.clone());

                    // Update nodes
                    let new_left = BTreeNode::Leaf {
                        entries: left_entries,
                        next_leaf: left_next,
                    };
                    let new_right = BTreeNode::Leaf {
                        entries: right_entries,
                        next_leaf: right_next,
                    };

                    self.write_node(left_page_id, &new_left)?;
                    self.write_node(right_page_id, &new_right)?;

                    Ok(new_separator)
                } else {
                    // Move keys from left to right
                    let to_move = left_count - target_left;

                    let mut moved_entries = Vec::new();
                    for _ in 0..to_move {
                        if let Some(entry) = left_entries.pop() {
                            moved_entries.insert(0, entry);
                        }
                    }

                    if moved_entries.is_empty() {
                        return Ok(None);
                    }

                    // New separator is the first moved key
                    let new_separator = moved_entries.first().map(|e| e.key.clone());

                    moved_entries.extend(right_entries);

                    // Update nodes
                    let new_left = BTreeNode::Leaf {
                        entries: left_entries,
                        next_leaf: left_next,
                    };
                    let new_right = BTreeNode::Leaf {
                        entries: moved_entries,
                        next_leaf: right_next,
                    };

                    self.write_node(left_page_id, &new_left)?;
                    self.write_node(right_page_id, &new_right)?;

                    Ok(new_separator)
                }
            }
            _ => Ok(None),
        }
    }

    /// Insert a key-value pair into the tree, handling splits as needed.
    fn insert_internal(
        &self,
        key: Vec<u8>,
        value: Vec<u8>,
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        // Find the leaf page with path
        let (leaf_page_id, pos, path) = self.search_with_path(&key)?;
        let mut node = self.read_node(leaf_page_id)?;

        let is_new_key = if let BTreeNode::Leaf { ref entries, .. } = node {
            !(pos < entries.len() && entries[pos].key == key)
        } else {
            false
        };

        if let BTreeNode::Leaf {
            ref mut entries, ..
        } = node
        {
            // Check if key already exists
            if pos < entries.len() && entries[pos].key == key {
                // Update existing entry's version chain by prepending new version
                let old_chain = entries[pos].chain.clone();
                let mut new_chain = old_chain.prepend(value, tx_id);
                // Commit immediately if commit_lsn > 0
                if commit_lsn.as_u64() > 0 {
                    new_chain.commit(commit_lsn);
                }
                entries[pos].chain = new_chain;
            } else {
                // Insert new entry with new version chain
                let mut chain = VersionChain::new(value, tx_id);
                // Commit immediately if commit_lsn > 0
                if commit_lsn.as_u64() > 0 {
                    chain.commit(commit_lsn);
                }
                entries.insert(
                    pos,
                    LeafEntry {
                        key: key.clone(),
                        chain,
                    },
                );
            }

            // Write the updated node first
            self.write_node(leaf_page_id, &node)?;

            // Check if node needs to be split after writing
            if node.is_full() {
                self.split_and_propagate(leaf_page_id, &node, path)?;
            }
        }

        // Increment row count if this is a new key
        if is_new_key {
            self.increment_row_count()?;
        }

        Ok(())
    }

    /// Split a node and propagate the split up the tree.
    /// The path parameter contains (parent_page_id, child_page_id) tuples from root to the node being split.
    fn split_and_propagate(
        &self,
        page_id: PageId,
        node: &BTreeNode,
        path: Vec<(PageId, PageId)>,
    ) -> TableResult<()> {
        let current_root = self.get_root_page_id();

        // Split the node
        let (right_page_id, median_key) = self.split_node(page_id, node)?;

        // If this is the root, create a new root
        if page_id == current_root {
            // Create new root
            let new_root_page_id = self.pager.allocate_page(PageType::BTreeInternal)?;
            let new_root = BTreeNode::Internal {
                entries: vec![InternalEntry {
                    key: median_key,
                    child_page_id: page_id,
                }],
                rightmost_child: right_page_id,
            };

            self.write_node(new_root_page_id, &new_root)?;

            // Update root pointer atomically and persist it for durability
            self.set_root_page_id(new_root_page_id)?;

            Ok(())
        } else {
            // Insert median key into parent
            self.insert_into_parent(page_id, median_key, right_page_id, path)?;
            Ok(())
        }
    }

    /// Insert a key and right child pointer into a parent node.
    /// This is called after splitting a child node.
    /// The path contains (parent_page_id, child_page_id) tuples from root to the child that was split.
    fn insert_into_parent(
        &self,
        left_child: PageId,
        key: Vec<u8>,
        right_child: PageId,
        mut path: Vec<(PageId, PageId)>,
    ) -> TableResult<()> {
        // Get the parent info (last element in path)
        let (parent_page_id, _child_that_was_followed) = match path.pop() {
            Some(info) => info,
            None => {
                // No parent means we're at root, which should have been handled already
                return Err(crate::table::TableError::corruption(
                    "PagedBTree::split_child",
                    "missing_parent",
                    "No parent found for non-root split",
                ));
            }
        };

        // Read the parent node
        let mut parent_node = self.read_node(parent_page_id)?;

        if let BTreeNode::Internal {
            ref mut entries,
            ref mut rightmost_child,
        } = parent_node
        {
            // In our B-Tree structure:
            // - entries[i].child_page_id contains keys < entries[i].key
            // - Keys >= entries[i].key go to the next child (entries[i+1].child_page_id or rightmost_child)
            //
            // After splitting left_child into left_child and right_child with median key:
            // - left_child now contains keys < median
            // - right_child contains keys >= median
            // - We need to insert median as a separator
            //
            // Strategy: Find where left_child is, insert median_key with right_child as the "next" pointer

            // Check if left_child is the rightmost child
            if *rightmost_child == left_child {
                // The split child was the rightmost child
                // Add new entry: median_key points to left_child (keys < median go left)
                // Update rightmost_child to right_child (keys >= median go right)
                entries.push(InternalEntry {
                    key,
                    child_page_id: left_child,
                });
                *rightmost_child = right_child;
            } else {
                // Find where left_child appears in the parent
                let mut insert_pos = None;

                // Check each entry's child pointer
                for (i, entry) in entries.iter().enumerate() {
                    if entry.child_page_id == left_child {
                        // entries[i].child_page_id == left_child
                        // This means keys < entries[i].key go to left_child
                        // After split: keys < median go to left_child, keys >= median go to right_child
                        // We need to insert median_key at position i, with right_child as the next pointer
                        insert_pos = Some(i);
                        break;
                    }
                }

                // Also check if left_child is the "next" child after an entry
                if insert_pos.is_none() {
                    for i in 0..entries.len() {
                        let next_child = if i + 1 < entries.len() {
                            entries[i + 1].child_page_id
                        } else {
                            *rightmost_child
                        };
                        if next_child == left_child {
                            // Keys >= entries[i].key go to left_child
                            // After split: we need to insert median at i+1
                            insert_pos = Some(i + 1);
                            break;
                        }
                    }
                }

                match insert_pos {
                    Some(pos)
                        if pos < entries.len() && entries[pos].child_page_id == left_child =>
                    {
                        // Case 1: left_child is at entries[pos].child_page_id
                        // entries[pos] = {key: K, child: left_child}
                        // This means left_child contains keys < K
                        // After split: left_child has keys < median, right_child has keys >= median
                        // We need to insert median at pos with left_child, and update entries[pos] to point to right_child
                        entries.insert(
                            pos,
                            InternalEntry {
                                key,
                                child_page_id: left_child,
                            },
                        );
                        // Now entries[pos+1] is the old entry, update its child to right_child
                        entries[pos + 1].child_page_id = right_child;
                    }
                    Some(pos) => {
                        // Case 2: left_child is the "next" child after entries[pos-1]
                        // This means keys >= entries[pos-1].key go to left_child
                        // After split: we insert median at pos with left_child
                        // The next entry (or rightmost) should point to right_child
                        entries.insert(
                            pos,
                            InternalEntry {
                                key,
                                child_page_id: left_child,
                            },
                        );
                        // Update the next entry's child to right_child
                        if pos + 1 < entries.len() {
                            entries[pos + 1].child_page_id = right_child;
                        } else {
                            *rightmost_child = right_child;
                        }
                    }
                    None => {
                        return Err(crate::table::TableError::corruption(
                            "PagedBTree::merge_or_redistribute",
                            "missing_child",
                            format!(
                                "Could not find left_child {:?} in parent {:?}",
                                left_child, parent_page_id
                            ),
                        ));
                    }
                }
            }

            // Write the updated parent
            self.write_node(parent_page_id, &parent_node)?;

            // Check if parent is now full and needs to split
            if parent_node.is_full() {
                self.split_and_propagate(parent_page_id, &parent_node, path)?;
            }
        }

        Ok(())
    }

    fn update_separator_after_child_change(
        &self,
        parent_page_id: PageId,
        child_page_id: PageId,
        replacement_key: Vec<u8>,
    ) -> TableResult<()> {
        let mut parent_node = self.read_node(parent_page_id)?;
        if let BTreeNode::Internal {
            ref mut entries,
            rightmost_child,
        } = parent_node
        {
            for i in 0..entries.len() {
                let next_child = if i + 1 < entries.len() {
                    entries[i + 1].child_page_id
                } else {
                    rightmost_child
                };
                if next_child == child_page_id {
                    entries[i].key = replacement_key;
                    self.write_node(parent_page_id, &parent_node)?;
                    return Ok(());
                }
            }
        }

        Err(crate::table::TableError::corruption(
            "PagedBTree::update_separator_after_child_change",
            "missing_child_reference",
            format!(
                "Could not find child {:?} in parent {:?} as right-side child",
                child_page_id, parent_page_id
            ),
        ))
    }

    fn rebalance_leaf_after_delete(
        &self,
        leaf_page_id: PageId,
        path: &[(PageId, PageId)],
    ) -> TableResult<()> {
        if path.is_empty() {
            return Ok(());
        }

        let (parent_page_id, _child_page_id) = *path.last().unwrap();
        let parent_node = self.read_node(parent_page_id)?;
        let (entries, rightmost_child) = match parent_node {
            BTreeNode::Internal {
                entries,
                rightmost_child,
            } => (entries, rightmost_child),
            _ => {
                return Err(crate::table::TableError::corruption(
                    "PagedBTree::rebalance_leaf_after_delete",
                    "parent_not_internal",
                    format!("Parent {:?} was not an internal node", parent_page_id),
                ))
            }
        };

        let mut children = entries
            .iter()
            .map(|entry| entry.child_page_id)
            .collect::<Vec<_>>();
        children.push(rightmost_child);

        let child_index = children
            .iter()
            .position(|&child| child == leaf_page_id)
            .ok_or_else(|| {
                crate::table::TableError::corruption(
                    "PagedBTree::rebalance_leaf_after_delete",
                    "missing_child",
                    format!(
                        "Could not find leaf {:?} in parent {:?}",
                        leaf_page_id, parent_page_id
                    ),
                )
            })?;

        if child_index > 0 {
            let left_sibling_page_id = children[child_index - 1];
            let separator_index = child_index - 1;
            if let Some(new_separator) = self.redistribute_keys(
                left_sibling_page_id,
                leaf_page_id,
                &entries[separator_index].key,
            )? {
                self.update_separator_after_child_change(
                    parent_page_id,
                    leaf_page_id,
                    new_separator,
                )?;
                return Ok(());
            }

            if self.merge_nodes(
                left_sibling_page_id,
                leaf_page_id,
                &entries[separator_index].key,
            )? {
                let mut updated_parent = self.read_node(parent_page_id)?;
                if let BTreeNode::Internal {
                    ref mut entries,
                    ref mut rightmost_child,
                } = updated_parent
                {
                    entries.remove(separator_index);
                    if child_index == children.len() - 1 {
                        *rightmost_child = left_sibling_page_id;
                    }
                    self.write_node(parent_page_id, &updated_parent)?;
                    return Ok(());
                }
            }
        }

        if child_index + 1 < children.len() {
            let right_sibling_page_id = children[child_index + 1];
            let separator_index = child_index;
            if let Some(new_separator) = self.redistribute_keys(
                leaf_page_id,
                right_sibling_page_id,
                &entries[separator_index].key,
            )? {
                self.update_separator_after_child_change(
                    parent_page_id,
                    right_sibling_page_id,
                    new_separator,
                )?;
                return Ok(());
            }

            if self.merge_nodes(
                leaf_page_id,
                right_sibling_page_id,
                &entries[separator_index].key,
            )? {
                let mut updated_parent = self.read_node(parent_page_id)?;
                if let BTreeNode::Internal {
                    ref mut entries,
                    ref mut rightmost_child,
                } = updated_parent
                {
                    entries.remove(separator_index);
                    if separator_index >= entries.len() {
                        *rightmost_child = leaf_page_id;
                    }
                    self.write_node(parent_page_id, &updated_parent)?;
                    return Ok(());
                }
            }
        }

        Ok(())
    }

    /// Delete a key from the tree, handling merges and redistributions as needed.
    fn delete_internal(
        &self,
        key: &[u8],
        tx_id: TransactionId,
        _commit_lsn: LogSequenceNumber,
    ) -> TableResult<bool> {
        // Find the leaf page
        let (leaf_page_id, pos, path) = self.search_with_path(key)?;
        let mut node = self.read_node(leaf_page_id)?;

        if let BTreeNode::Leaf {
            ref mut entries, ..
        } = node
        {
            // Check if key exists
            if pos < entries.len() && entries[pos].key == key {
                // Mark as deleted in version chain by prepending empty value
                let old_chain = entries[pos].chain.clone();
                let new_chain = old_chain.prepend(Vec::new(), tx_id);
                // Leave uncommitted - will be committed by commit_versions()
                entries[pos].chain = new_chain;

                // Write updated node
                self.write_node(leaf_page_id, &node)?;

                // Decrement row count
                self.decrement_row_count()?;

                // Check if node needs rebalancing
                if !node.has_minimum_keys() && leaf_page_id != self.get_root_page_id() {
                    self.rebalance_leaf_after_delete(leaf_page_id, &path)?;
                }

                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Commit all uncommitted versions created by the given transaction.
    ///
    /// Traverses all leaf pages and marks versions created by tx_id with the given commit_lsn.
    fn commit_versions_for_tx(
        &self,
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        let mut current_page_id = self.get_root_page_id();

        // Navigate to leftmost leaf
        loop {
            let node = self.read_node(current_page_id)?;
            match &node {
                BTreeNode::Internal { entries, .. } => {
                    // Follow leftmost child
                    current_page_id =
                        entries
                            .first()
                            .map(|e| e.child_page_id)
                            .unwrap_or(match &node {
                                BTreeNode::Internal {
                                    rightmost_child, ..
                                } => *rightmost_child,
                                _ => unreachable!(),
                            });
                }
                BTreeNode::Leaf { .. } => break,
            }
        }

        // Traverse all leaf pages and commit versions
        loop {
            let node = self.read_node(current_page_id)?;
            if let BTreeNode::Leaf { entries, next_leaf } = node {
                let mut new_entries = entries.clone();

                for entry in new_entries.iter_mut() {
                    Self::commit_chain_recursive(&mut entry.chain, tx_id, commit_lsn);
                }

                // Write back the updated node
                let updated_node = BTreeNode::Leaf {
                    entries: new_entries,
                    next_leaf,
                };
                self.write_node(current_page_id, &updated_node)?;

                if next_leaf == PageId::from(0) {
                    break;
                }
                current_page_id = next_leaf;
            } else {
                unreachable!("Expected leaf node");
            }
        }

        Ok(())
    }

    /// Recursively commit versions in a chain created by tx_id.
    fn commit_chain_recursive(
        chain: &mut VersionChain,
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) {
        if chain.created_by == tx_id && chain.commit_lsn.is_none() {
            chain.commit(commit_lsn);
        }
        if let Some(ref mut prev) = chain.prev_version {
            Self::commit_chain_recursive(prev, tx_id, commit_lsn);
        }
    }

    /// Collect statistics by traversing the entire tree.
    fn collect_tree_statistics(&self) -> TableResult<TreeStatistics> {
        let mut stats = TreeStatistics::new();
        let root_page_id = self.get_root_page_id();
        let page_size = self.pager.page_size().to_u32() as u64;
        
        // Traverse the tree depth-first to collect statistics
        self.collect_node_statistics(root_page_id, 0, &mut stats, page_size)?;
        
        // Ensure min sizes are valid (handle empty tree case)
        if stats.row_count == 0 {
            stats.key_min_size = 0;
            stats.value_min_size = 0;
        }
        
        Ok(stats)
    }

    /// Recursively collect statistics for a node and its children.
    fn collect_node_statistics(
        &self,
        page_id: PageId,
        depth: usize,
        stats: &mut TreeStatistics,
        page_size: u64,
    ) -> TableResult<()> {
        let node = self.read_node(page_id)?;
        
        // Update tree depth
        if depth > stats.tree_depth {
            stats.tree_depth = depth;
        }
        
        // Add page size to total
        stats.total_size_bytes += page_size;
        
        match node {
            BTreeNode::Internal { entries, rightmost_child } => {
                stats.internal_node_count += 1;
                
                // Recursively process all children
                for entry in &entries {
                    self.collect_node_statistics(entry.child_page_id, depth + 1, stats, page_size)?;
                }
                self.collect_node_statistics(rightmost_child, depth + 1, stats, page_size)?;
            }
            BTreeNode::Leaf { entries, next_leaf: _ } => {
                stats.leaf_node_count += 1;
                
                // Process each entry in the leaf
                for entry in &entries {
                    // Count non-tombstone entries (tombstones have empty values)
                    if !entry.chain.value.is_empty() {
                        stats.row_count += 1;
                        
                        // Track key statistics
                        let key_size = entry.key.len();
                        stats.key_min_size = stats.key_min_size.min(key_size);
                        stats.key_max_size = stats.key_max_size.max(key_size);
                        stats.key_total_size += key_size as u64;
                        
                        // Track value statistics
                        let value_size = entry.chain.value.len();
                        stats.value_min_size = stats.value_min_size.min(value_size);
                        stats.value_max_size = stats.value_max_size.max(value_size);
                        stats.value_total_size += value_size as u64;
                    }
                }
            }
        }
        
        Ok(())
    }
}

impl<FS: FileSystem> Table for PagedBTree<FS> {
    fn table_id(&self) -> TableId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> TableEngineKind {
        TableEngineKind::BTree
    }

    fn capabilities(&self) -> TableCapabilities {
        TableCapabilities {
            ordered: true,
            point_lookup: true,
            prefix_scan: true,
            reverse_scan: true,
            range_delete: true,
            merge_operator: false,
            mvcc_native: true,
            append_optimized: false,
            memory_resident: false,
            disk_resident: true,
            supports_compression: true,
            supports_encryption: true,
        }
    }


    fn stats(&self) -> TableResult<TableStatistics> {
        let start = Instant::now();
        
        // Collect statistics by traversing the tree
        let stats = self.collect_tree_statistics()?;
        
        histogram!("btree.stats_collection_duration").record(start.elapsed().as_secs_f64());
        
        Ok(TableStatistics {
            row_count: Some(stats.row_count),
            total_size_bytes: Some(stats.total_size_bytes),
            key_stats: Some(crate::table::KeyStatistics {
                min_size: stats.key_min_size,
                max_size: stats.key_max_size,
                avg_size: stats.key_avg_size(),
                distinct_count: Some(stats.row_count), // Each key is unique in a B-Tree
            }),
            value_stats: Some(crate::table::ValueStatistics {
                min_size: stats.value_min_size,
                max_size: stats.value_max_size,
                avg_size: stats.value_avg_size(),
                null_count: Some(0), // B-Tree doesn't store nulls
            }),
            histogram: None, // TODO: Implement histogram buckets if needed
            last_updated_lsn: None, // TODO: Track last update LSN if needed
        })
    }
}

impl<FS: FileSystem> SearchableTable for PagedBTree<FS> {
    type Reader<'a>
        = PagedBTreeReader<'a, FS>
    where
        Self: 'a;
    type Writer<'a>
        = PagedBTreeWriter<'a, FS>
    where
        Self: 'a;

    fn reader(&self, snapshot_lsn: LogSequenceNumber) -> TableResult<Self::Reader<'_>> {
        Ok(PagedBTreeReader {
            table: self,
            snapshot_lsn,
        })
    }

    fn writer(
        &self,
        tx_id: TransactionId,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self::Writer<'_>> {
        Ok(PagedBTreeWriter {
            table: self,
            tx_id,
            snapshot_lsn,
            pending_changes: Vec::new(),
        })
    }
}

// =============================================================================
// Reader and Writer
// =============================================================================

/// Read-only view of the paged B-Tree at a specific snapshot.
pub struct PagedBTreeReader<'a, FS: FileSystem> {
    table: &'a PagedBTree<FS>,
    snapshot_lsn: LogSequenceNumber,
}

impl<'a, FS: FileSystem> PointLookup for PagedBTreeReader<'a, FS> {
    fn get(&self, key: &[u8], snapshot_lsn: LogSequenceNumber) -> TableResult<Option<ValueBuf>> {
        self.table.get_internal(key, snapshot_lsn)
    }

    fn get_stream(
        &self,
        key: &[u8],
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Option<Box<dyn crate::table::ValueStream + '_>>> {
        use crate::table::SliceValueStream;

        // For now, use default implementation that loads the full value
        // TODO: Implement true streaming with ValueRef and overflow chains
        // This requires modifying VersionChain to store ValueRef information
        self.get(key, snapshot_lsn).map(|opt| {
            opt.map(|value_buf| {
                Box::new(SliceValueStream::new(value_buf.0))
                    as Box<dyn crate::table::ValueStream + '_>
            })
        })
    }
}

impl<'a, FS: FileSystem> OrderedScan for PagedBTreeReader<'a, FS> {
    type Cursor<'b>
        = PagedBTreeCursor<'b, FS>
    where
        Self: 'b;

    fn scan(
        &self,
        bounds: ScanBounds,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self::Cursor<'_>> {
        Ok(PagedBTreeCursor::new(self.table, bounds, snapshot_lsn))
    }
}

impl<'a, FS: FileSystem> TableReader for PagedBTreeReader<'a, FS> {
    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }

    fn approximate_len(&self) -> TableResult<Option<u64>> {
        Ok(Some(self.table.get_row_count()))
    }
}

/// Write view of the paged B-Tree for a specific transaction.
pub struct PagedBTreeWriter<'a, FS: FileSystem> {
    table: &'a PagedBTree<FS>,
    tx_id: TransactionId,
    snapshot_lsn: LogSequenceNumber,
    pending_changes: Vec<(Vec<u8>, Option<Vec<u8>>)>,
}

impl<'a, FS: FileSystem> MutableTable for PagedBTreeWriter<'a, FS> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> TableResult<u64> {
        self.pending_changes
            .push((key.to_vec(), Some(value.to_vec())));
        // Return approximate size: key + value + overhead
        Ok((key.len() + value.len() + 16) as u64)
    }

    fn put_stream(
        &mut self,
        key: &[u8],
        stream: &mut dyn crate::table::ValueStream,
    ) -> TableResult<u64> {
        // Check size hint to determine storage strategy
        let size_hint = stream.size_hint();
        let max_inline = self.max_inline_size().unwrap_or(4096);

        // If small enough or no size hint, use default implementation (inline storage)
        if let Some(size) = size_hint {
            if size <= max_inline as u64 {
                // Read entire value and store inline
                let mut buffer = Vec::with_capacity(size as usize);
                let mut temp_buf = vec![0u8; 8192];
                loop {
                    let n = stream.read(&mut temp_buf)?;
                    if n == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&temp_buf[..n]);
                }
                return self.put(key, &buffer);
            }
        } else {
            // No size hint, use default implementation
            return MutableTable::put_stream(self, key, stream);
        }

        // Large value: stream to overflow pages
        let mut buffer = Vec::new();
        let mut temp_buf = vec![0u8; 8192];
        loop {
            let n = stream.read(&mut temp_buf)?;
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&temp_buf[..n]);
        }

        // For now, allocate overflow chain during flush
        // Store the full value in pending_changes
        // TODO: Optimize to stream directly during flush
        self.pending_changes
            .push((key.to_vec(), Some(buffer.clone())));

        Ok((key.len() + buffer.len() + 16) as u64)
    }

    fn delete(&mut self, key: &[u8]) -> TableResult<bool> {
        // Check if key exists
        let exists = self.table.get_internal(key, self.snapshot_lsn)?.is_some();
        if exists {
            self.pending_changes.push((key.to_vec(), None));
        }
        Ok(exists)
    }

    fn range_delete(&mut self, bounds: ScanBounds) -> TableResult<u64> {
        // Create a cursor to scan the range
        let mut cursor = PagedBTreeCursor::new(self.table, bounds.clone(), self.snapshot_lsn);

        let mut deleted_count = 0u64;

        loop {
            if !cursor.valid() {
                break;
            }

            if let Some(key) = cursor.key() {
                self.pending_changes.push((key.to_vec(), None));
                deleted_count += 1;
            }

            cursor.next()?;
        }

        Ok(deleted_count)
    }

    fn max_inline_size(&self) -> Option<usize> {
        // Use 4KB as default inline threshold
        // Values larger than this will use overflow pages
        Some(4096)
    }
}

impl<'a, FS: FileSystem> BatchOps for PagedBTreeWriter<'a, FS> {
    fn batch_get(&self, keys: &[&[u8]]) -> TableResult<Vec<Option<ValueBuf>>> {
        let mut results = Vec::with_capacity(keys.len());
        for key in keys {
            results.push(self.table.get_internal(key, self.snapshot_lsn)?);
        }
        Ok(results)
    }

    fn apply_batch<'b>(&mut self, batch: WriteBatch<'b>) -> TableResult<BatchReport> {
        let mut report = BatchReport {
            attempted: batch.mutations.len() as u64,
            ..Default::default()
        };

        for mutation in batch.mutations {
            match mutation {
                crate::table::Mutation::Put { key, value } => {
                    self.put(&key, &value)?;
                    report.applied += 1;
                    report.bytes_written += key.len() as u64 + value.len() as u64;
                }
                crate::table::Mutation::Delete { key } => {
                    if self.delete(&key)? {
                        report.deleted += 1;
                    }
                    report.applied += 1;
                }
                crate::table::Mutation::RangeDelete { bounds } => {
                    let deleted = self.range_delete(bounds)?;
                    report.deleted += deleted;
                    report.applied += 1;
                }
                crate::table::Mutation::Merge { .. } => {
                    // Merge not supported
                    continue;
                }
            }
        }

        Ok(report)
    }
}

impl<'a, FS: FileSystem> Flushable for PagedBTreeWriter<'a, FS> {
    fn flush(&mut self) -> TableResult<()> {
        if self.pending_changes.is_empty() {
            return Ok(());
        }

        // Apply all pending changes (versions remain uncommitted)
        for (key, value_opt) in self.pending_changes.drain(..) {
            match value_opt {
                Some(value) => {
                    // Insert or update (versions left uncommitted)
                    self.table.insert_internal(
                        key,
                        value,
                        self.tx_id,
                        LogSequenceNumber::from(0),
                    )?;
                }
                None => {
                    // Delete (versions left uncommitted)
                    self.table
                        .delete_internal(&key, self.tx_id, LogSequenceNumber::from(0))?;
                }
            }
        }

        Ok(())
    }
}

impl<'a, FS: FileSystem> PagedBTreeWriter<'a, FS> {
    /// Mark all versions created by this transaction as committed.
    ///
    /// This must be called after flush() to make the changes visible to readers.
    /// The commit_lsn is obtained from the WAL after writing the COMMIT record.
    pub fn commit_versions(&self, commit_lsn: LogSequenceNumber) -> TableResult<()> {
        self.table.commit_versions_for_tx(self.tx_id, commit_lsn)
    }
}

impl<'a, FS: FileSystem> TableWriter for PagedBTreeWriter<'a, FS> {
    fn tx_id(&self) -> TransactionId {
        self.tx_id
    }

    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }
}

// =============================================================================
// Cursor
// =============================================================================

/// Cursor for iterating over the paged B-Tree.
pub struct PagedBTreeCursor<'a, FS: FileSystem> {
    table: &'a PagedBTree<FS>,
    snapshot_lsn: LogSequenceNumber,
    bounds: ScanBounds,
    current_page_id: PageId,
    current_position: usize,
    current_key: Option<Vec<u8>>,
    current_value: Option<Vec<u8>>,
    exhausted: bool,
    /// Track if initial positioning has been performed
    initialized: bool,
}

impl<'a, FS: FileSystem> PagedBTreeCursor<'a, FS> {
    fn new(table: &'a PagedBTree<FS>, bounds: ScanBounds, snapshot_lsn: LogSequenceNumber) -> Self {
        let mut cursor = Self {
            table,
            snapshot_lsn,
            bounds,
            current_page_id: PageId::from(0),
            current_position: 0,
            current_key: None,
            current_value: None,
            exhausted: false,
            initialized: false,
        };
        // Position at first valid entry (consistent with MemoryBTree)
        let _ = cursor.first();
        cursor
    }

    fn is_in_bounds(&self, key: &[u8]) -> bool {
        match &self.bounds {
            ScanBounds::All => true,
            ScanBounds::Prefix(prefix) => key.starts_with(&prefix.0),
            ScanBounds::Range { start, end } => {
                let after_start = match start {
                    Bound::Included(k) => key >= k.0.as_slice(),
                    Bound::Excluded(k) => key > k.0.as_slice(),
                    Bound::Unbounded => true,
                };
                let before_end = match end {
                    Bound::Included(k) => key <= k.0.as_slice(),
                    Bound::Excluded(k) => key < k.0.as_slice(),
                    Bound::Unbounded => true,
                };
                after_start && before_end
            }
        }
    }

    /// Navigate to the leftmost leaf page.
    fn find_leftmost_leaf(&self) -> TableResult<PageId> {
        let mut current_page_id = self.table.get_root_page_id();

        loop {
            let node = self.table.read_node(current_page_id)?;
            match node {
                BTreeNode::Internal { entries, .. } => {
                    // Follow leftmost child
                    if entries.is_empty() {
                        return Err(crate::table::TableError::corruption(
                            "PagedBTree::delete_internal",
                            "empty_node",
                            "Empty internal node",
                        ));
                    }
                    current_page_id = entries[0].child_page_id;
                }
                BTreeNode::Leaf { .. } => {
                    return Ok(current_page_id);
                }
            }
        }
    }

    /// Navigate to the rightmost leaf page.
    fn find_rightmost_leaf(&self) -> TableResult<PageId> {
        let mut current_page_id = self.table.get_root_page_id();

        loop {
            let node = self.table.read_node(current_page_id)?;
            match node {
                BTreeNode::Internal {
                    rightmost_child, ..
                } => {
                    // Follow rightmost child
                    current_page_id = rightmost_child;
                }
                BTreeNode::Leaf { .. } => {
                    return Ok(current_page_id);
                }
            }
        }
    }

    /// Load the current entry at the cursor position, checking MVCC visibility.
    fn load_current_entry(&mut self) -> TableResult<()> {
        let node = self.table.read_node(self.current_page_id)?;

        if let BTreeNode::Leaf { entries, .. } = node {
            if self.current_position < entries.len() {
                let entry = &entries[self.current_position];

                // Check bounds
                if !self.is_in_bounds(&entry.key) {
                    self.exhausted = true;
                    self.current_key = None;
                    self.current_value = None;
                    return Ok(());
                }

                // Check MVCC visibility
                let snapshot = Snapshot::new(
                    crate::snap::SnapshotId::from(0),
                    String::new(),
                    self.snapshot_lsn,
                    0,
                    0,
                    Vec::new(),
                );

                if let Some(value) = entry.chain.find_visible_version(&snapshot) {
                    if value.is_empty() {
                        self.current_key = None;
                        self.current_value = None;
                    } else {
                        self.current_key = Some(entry.key.clone());
                        self.current_value = Some(value.to_vec());
                    }
                } else {
                    // Version not visible, mark as exhausted at this position
                    self.current_key = None;
                    self.current_value = None;
                }
            } else {
                self.current_key = None;
                self.current_value = None;
            }
        }

        Ok(())
    }

    /// Advance to the next visible entry, skipping invisible versions.
    fn advance_to_next_visible(&mut self) -> TableResult<()> {
        loop {
            let node = self.table.read_node(self.current_page_id)?;

            if let BTreeNode::Leaf { entries, next_leaf } = node {
                // Try to advance within current leaf
                while self.current_position < entries.len() {
                    self.load_current_entry()?;

                    // Check if we found a valid entry or hit bounds
                    if self.current_key.is_some() {
                        return Ok(());
                    }

                    // If exhausted (out of bounds), stop searching
                    if self.exhausted {
                        return Ok(());
                    }

                    self.current_position += 1;
                }

                // Move to next leaf if available
                if next_leaf.as_u64() != 0 {
                    self.current_page_id = next_leaf;
                    self.current_position = 0;
                } else {
                    self.exhausted = true;
                    return Ok(());
                }
            } else {
                return Err(crate::table::TableError::corruption(
                    "PagedBTreeCursor::next",
                    "wrong_node_type",
                    "Expected leaf node",
                ));
            }
        }
    }

    /// Move backward to the previous visible entry.
    fn retreat_to_prev_visible(&mut self) -> TableResult<()> {
        loop {
            // Try to move backward within current leaf
            if self.current_position > 0 {
                self.current_position -= 1;
                self.load_current_entry()?;
                if self.current_key.is_some() {
                    return Ok(());
                }
            } else {
                // Need to find previous leaf - this requires parent tracking
                // For now, mark as exhausted (reverse iteration without parent pointers
                // would require maintaining a stack or scanning from root)
                self.exhausted = true;
                self.current_key = None;
                self.current_value = None;
                return Ok(());
            }
        }
    }
}

impl<'a, FS: FileSystem> TableCursor for PagedBTreeCursor<'a, FS> {
    fn valid(&self) -> bool {
        !self.exhausted && self.current_key.is_some()
    }

    fn key(&self) -> Option<&[u8]> {
        self.current_key.as_deref()
    }

    fn value(&self) -> Option<&[u8]> {
        self.current_value.as_deref()
    }

    fn next(&mut self) -> TableResult<()> {
        // Ensure cursor is initialized before advancing
        if !self.initialized {
            return self.first();
        }

        if self.exhausted {
            return Ok(());
        }

        // Move to next position
        self.current_position += 1;
        self.advance_to_next_visible()
    }

    fn prev(&mut self) -> TableResult<()> {
        // Ensure cursor is initialized before retreating
        if !self.initialized {
            return self.last();
        }

        if self.exhausted {
            return Ok(());
        }

        self.retreat_to_prev_visible()
    }

    fn seek(&mut self, key: &[u8]) -> TableResult<()> {
        self.initialized = true; // Mark as initialized since we're explicitly positioning

        // Reset exhausted state
        self.exhausted = false;

        // Check if key is within bounds
        if !self.is_in_bounds(key) {
            // Position at first entry if key is before or at start bound
            if let ScanBounds::Range { start, .. } = &self.bounds.clone() {
                let before_or_at_start = match start {
                    Bound::Included(k) => key < k.0.as_slice(),
                    Bound::Excluded(k) => key <= k.0.as_slice(), // Include equal for excluded bounds
                    Bound::Unbounded => false,
                };
                if before_or_at_start {
                    // Seek to the start bound instead of calling first() to avoid recursion
                    match start {
                        Bound::Included(k) => {
                            let start_key = k.0.clone();
                            return self.seek(&start_key);
                        }
                        Bound::Excluded(k) => {
                            let start_key = k.0.clone();
                            // Use tree search directly to avoid recursion when seeking to excluded bound
                            let (leaf_page_id, pos) = self.table.search(&start_key)?;
                            self.current_page_id = leaf_page_id;
                            self.current_position = pos;

                            // Advance past the excluded key
                            // Note: We don't call advance_to_next_visible first because it will
                            // mark us as exhausted when it sees the excluded key is out of bounds.
                            // Instead, we increment position and then advance.
                            self.current_position += 1;
                            return self.advance_to_next_visible();
                        }
                        Bound::Unbounded => {
                            // Navigate to leftmost leaf
                            self.current_page_id = self.find_leftmost_leaf()?;
                            self.current_position = 0;
                            return self.advance_to_next_visible();
                        }
                    }
                }
            }

            // Key is after end bound
            self.exhausted = true;
            self.current_key = None;
            self.current_value = None;
            return Ok(());
        }

        // Use tree search to find the leaf and position
        let (leaf_page_id, pos) = self.table.search(key)?;
        self.current_page_id = leaf_page_id;
        self.current_position = pos;

        // Load the entry at this position (or advance if not visible)
        self.advance_to_next_visible()
    }

    fn seek_for_prev(&mut self, key: &[u8]) -> TableResult<()> {
        self.initialized = true; // Mark as initialized since we're explicitly positioning

        // Reset exhausted state
        self.exhausted = false;

        // Check if key is within bounds
        if !self.is_in_bounds(key) {
            // Position at last entry if key is after end bound
            if let ScanBounds::Range { end, .. } = &self.bounds {
                let after_end = match end {
                    Bound::Included(k) => key > k.0.as_slice(),
                    Bound::Excluded(k) => key >= k.0.as_slice(),
                    Bound::Unbounded => false,
                };
                if after_end {
                    return self.last();
                }
            }

            // Key is before start bound
            self.exhausted = true;
            self.current_key = None;
            self.current_value = None;
            return Ok(());
        }

        // Use tree search to find the leaf and position
        let (leaf_page_id, pos) = self.table.search(key)?;
        self.current_page_id = leaf_page_id;
        self.current_position = pos;

        // Check if we found exact match or need to go to previous
        let node = self.table.read_node(leaf_page_id)?;
        if let BTreeNode::Leaf { entries, .. } = node {
            if pos < entries.len() && entries[pos].key.as_slice() == key {
                // Found exact match, position here
                self.load_current_entry()?;
            } else {
                // Key not found or positioned after target, retreat to previous visible entry
                self.retreat_to_prev_visible()?;
            }
        }

        Ok(())
    }

    fn first(&mut self) -> TableResult<()> {
        self.initialized = true; // Mark as initialized since we're explicitly positioning

        // Reset exhausted state
        self.exhausted = false;

        // For bounded scans, seek to the start bound instead of going to leftmost leaf
        match &self.bounds.clone() {
            ScanBounds::All => {
                // Navigate to leftmost leaf
                self.current_page_id = self.find_leftmost_leaf()?;
                self.current_position = 0;
                self.advance_to_next_visible()
            }
            ScanBounds::Prefix(prefix) => {
                // Seek to the prefix start
                self.seek(&prefix.0)
            }
            ScanBounds::Range { start, .. } => {
                match start {
                    Bound::Included(k) => self.seek(&k.0),
                    Bound::Excluded(k) => {
                        // Seek to key and advance past it
                        self.seek(&k.0)?;
                        if self.valid() && self.key() == Some(&k.0[..]) {
                            self.next()
                        } else {
                            Ok(())
                        }
                    }
                    Bound::Unbounded => {
                        // Navigate to leftmost leaf
                        self.current_page_id = self.find_leftmost_leaf()?;
                        self.current_position = 0;
                        self.advance_to_next_visible()
                    }
                }
            }
        }
    }

    fn last(&mut self) -> TableResult<()> {
        self.initialized = true; // Mark as initialized since we're explicitly positioning

        // Reset exhausted state
        self.exhausted = false;

        // Navigate to rightmost leaf
        self.current_page_id = self.find_rightmost_leaf()?;

        // Find last entry in the leaf
        let node = self.table.read_node(self.current_page_id)?;
        if let BTreeNode::Leaf { entries, .. } = node {
            if entries.is_empty() {
                self.exhausted = true;
                self.current_key = None;
                self.current_value = None;
                return Ok(());
            }

            // Start from last entry and work backwards to find visible entry
            self.current_position = entries.len() - 1;
            self.load_current_entry()?;

            // If not visible or out of bounds, retreat
            if self.current_key.is_none() {
                self.retreat_to_prev_visible()?;
            }
        }

        Ok(())
    }

    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }
}

// Made with Bob

// =============================================================================
// DenseOrdered Specialty Table Implementation
// =============================================================================

/// Specialty cursor for index operations on paged B-Tree.
///
/// For secondary indexes, the "index_key" is the indexed field value,
/// and the "primary_key" is the pointer back to the main table record.
pub struct PagedBTreeSpecialtyCursor<'a, FS: FileSystem> {
    inner: PagedBTreeCursor<'a, FS>,
}

impl<'a, FS: FileSystem> SpecialtyTableCursor for PagedBTreeSpecialtyCursor<'a, FS> {
    fn valid(&self) -> bool {
        self.inner.valid()
    }

    fn index_key(&self) -> Option<&[u8]> {
        self.inner.key()
    }

    fn primary_key(&self) -> Option<&[u8]> {
        self.inner.value()
    }

    fn next(&mut self) -> TableResult<()> {
        self.inner.next()
    }

    fn prev(&mut self) -> TableResult<()> {
        self.inner.prev()
    }

    fn seek(&mut self, index_key: &[u8]) -> TableResult<()> {
        self.inner.seek(index_key)
    }
}

impl<FS: FileSystem> DenseOrdered for PagedBTree<FS> {
    type Cursor<'a>
        = PagedBTreeSpecialtyCursor<'a, FS>
    where
        Self: 'a;

    fn table_id(&self) -> TableId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> SpecialtyTableCapabilities {
        SpecialtyTableCapabilities {
            exact: true,
            approximate: false,
            ordered: true,
            sparse: false,
            supports_delete: true,
            supports_range_query: true,
            supports_prefix_query: true,
            supports_scoring: false,
            supports_incremental_rebuild: false,
            may_be_stale: false,
        }
    }

    fn insert_entry(
        &mut self,
        index_key: &[u8],
        primary_key: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        // For a secondary index, we store: index_key -> primary_key
        // This allows lookups by the indexed field to find the primary key
        self.insert_internal(index_key.to_vec(), primary_key.to_vec(), tx_id, commit_lsn)
    }

    fn delete_entry(
        &mut self,
        index_key: &[u8],
        primary_key: &[u8],
        _tx_id: TransactionId,
        _commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        // For secondary indexes, we need to delete the specific index_key -> primary_key mapping
        // First, verify that the entry exists and points to the expected primary key

        let (leaf_page_id, pos) = self.search(index_key)?;
        let mut leaf_node = self.read_node(leaf_page_id)?;

        if let BTreeNode::Leaf {
            ref mut entries, ..
        } = leaf_node
            && pos < entries.len()
            && entries[pos].key == index_key
        {
            // Check if the value matches the expected primary key
            let snapshot = Snapshot::new(
                crate::snap::SnapshotId::from(0),
                String::new(),
                LogSequenceNumber::from(u64::MAX),
                0,
                0,
                Vec::new(),
            );

            if let Some(stored_primary_key) = entries[pos].chain.find_visible_version(&snapshot)
                && stored_primary_key == primary_key
            {
                // Remove the entry
                entries.remove(pos);
                self.write_node(leaf_page_id, &leaf_node)?;
                counter!("btree.index_delete").increment(1);
            }
        }

        Ok(())
    }

    fn scan(&self, bounds: ScanBounds) -> TableResult<Self::Cursor<'_>> {
        let inner = PagedBTreeCursor::new(
            self,
            bounds,
            LogSequenceNumber::from(u64::MAX), // Use max LSN to see all versions
        );
        Ok(PagedBTreeSpecialtyCursor { inner })
    }

    fn stats(&self) -> TableResult<SpecialtyTableStats> {
        // Traverse the tree to count entries
        let root_page_id = self.get_root_page_id();
        let entry_count = self.count_entries(root_page_id)?;

        Ok(SpecialtyTableStats {
            entry_count: Some(entry_count),
            size_bytes: None,                 // Would need to track page usage
            distinct_keys: Some(entry_count), // For B-Tree, each entry is distinct
            stale_entries: Some(0),
            last_updated_lsn: None,
        })
    }

    fn verify(&self) -> TableResult<VerificationReport> {
        // Basic verification: check tree structure
        let root_page_id = self.get_root_page_id();
        let mut report = VerificationReport {
            checked_items: 0,
            errors: Vec::new(),
            warnings: Vec::new(),
        };

        // Verify the tree structure recursively
        self.verify_node(root_page_id, &mut report)?;

        Ok(report)
    }
}

impl<FS: FileSystem> PagedBTree<FS> {
    /// Count total entries in the tree (helper for stats).
    fn count_entries(&self, page_id: PageId) -> TableResult<u64> {
        let node = self.read_node(page_id)?;

        match node {
            BTreeNode::Internal {
                entries,
                rightmost_child,
            } => {
                let mut count = 0;
                for entry in &entries {
                    count += self.count_entries(entry.child_page_id)?;
                }
                count += self.count_entries(rightmost_child)?;
                Ok(count)
            }
            BTreeNode::Leaf { entries, .. } => Ok(entries.len() as u64),
        }
    }

    /// Verify node structure recursively (helper for verify).
    fn verify_node(&self, page_id: PageId, report: &mut VerificationReport) -> TableResult<()> {
        let node = self.read_node(page_id)?;
        report.checked_items += 1;

        match node {
            BTreeNode::Internal {
                entries,
                rightmost_child,
            } => {
                // Verify internal node structure
                for entry in &entries {
                    self.verify_node(entry.child_page_id, report)?;
                }
                self.verify_node(rightmost_child, report)?;
            }
            BTreeNode::Leaf { .. } => {
                // Leaf node - nothing more to verify
            }
        }

        Ok(())
    }

    /// Vacuum obsolete versions from all entries in the tree.
    ///
    /// Recursively traverses the B-Tree and calls VersionChain::vacuum() on each
    /// leaf entry, removing versions older than min_visible_lsn while preserving
    /// one old version as a base.
    ///
    /// Returns the total count of removed versions.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        let root_page_id = *self.root_page_id.read().unwrap();
        if root_page_id == PageId::from(0) {
            return Ok(0);
        }
        self.vacuum_node(root_page_id, min_visible_lsn)
    }

    /// Vacuum a single node recursively.
    fn vacuum_node(
        &self,
        page_id: PageId,
        min_visible_lsn: LogSequenceNumber,
    ) -> TableResult<usize> {
        let node = self.read_node(page_id)?;
        let mut total_removed = 0;

        match node {
            BTreeNode::Internal {
                entries,
                rightmost_child,
            } => {
                // Recursively vacuum all child nodes
                for entry in &entries {
                    total_removed += self.vacuum_node(entry.child_page_id, min_visible_lsn)?;
                }
                total_removed += self.vacuum_node(rightmost_child, min_visible_lsn)?;
            }
            BTreeNode::Leaf {
                mut entries,
                next_leaf,
            } => {
                // Vacuum each entry's version chain
                for entry in &mut entries {
                    let removed = entry.chain.vacuum(min_visible_lsn);
                    total_removed += removed;
                }

                // Write the updated node back to disk
                let updated_node = BTreeNode::Leaf { entries, next_leaf };
                self.write_node(page_id, &updated_node)?;
            }
        }

        Ok(total_removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_serialization() {
        // Test internal node
        let mut internal = BTreeNode::new_internal();
        if let BTreeNode::Internal {
            entries,
            rightmost_child,
        } = &mut internal
        {
            entries.push(InternalEntry {
                key: b"key1".to_vec(),
                child_page_id: PageId::from(10),
            });
            *rightmost_child = PageId::from(20);
        }

        let bytes = internal.to_bytes();
        let deserialized = BTreeNode::from_bytes(&bytes).unwrap();
        assert_eq!(deserialized.node_type(), NodeType::Internal);
        assert_eq!(deserialized.key_count(), 1);

        // Test leaf node
        let mut leaf = BTreeNode::new_leaf();
        if let BTreeNode::Leaf { entries, .. } = &mut leaf {
            entries.push(LeafEntry {
                key: b"key1".to_vec(),
                chain: VersionChain::new(b"value1".to_vec(), TransactionId::from(1)),
            });
        }

        let bytes = leaf.to_bytes();
        let deserialized = BTreeNode::from_bytes(&bytes).unwrap();
        assert_eq!(deserialized.node_type(), NodeType::Leaf);
        assert_eq!(deserialized.key_count(), 1);

    #[test]
    fn test_statistics_collection() {
        use crate::pager::PagerConfig;
        use crate::vfs::MemoryFileSystem;
        
        // Create an empty BTree
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default();
        let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());
        let btree = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();
        
        // Collect statistics on empty tree - should not panic
        let stats = Table::stats(&btree).unwrap();
        
        // Verify empty tree statistics
        assert_eq!(stats.row_count, Some(0), "Empty tree should have 0 rows");
        assert!(stats.total_size_bytes.is_some(), "Total size should be set");
        assert!(stats.total_size_bytes.unwrap() > 0, "Total size should include at least root page");
        
        // Key and value stats should be present even for empty tree
        assert!(stats.key_stats.is_some(), "Key stats should be set");
        assert!(stats.value_stats.is_some(), "Value stats should be set");
    }
    }
}
