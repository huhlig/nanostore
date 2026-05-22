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
    BatchOps, BatchReport, DenseOrdered, Flushable, MutableTable, OrderedScan, PointLookup,
    SearchableTable, SpecialtyTableCapabilities, SpecialtyTableCursor, SpecialtyTableStats, Table,
    TableCapabilities, TableCursor, TableEngineKind, TableError, TableReader, TableResult,
    TableStatistics, TableWriter, VerificationReport, WriteBatch,
};
use crate::txn::{TransactionId, VersionChain};
use crate::types::{Bound, ScanBounds, TableId, ValueBuf, ValueRef};
use crate::vfs::FileSystem;
use crate::wal::LogSequenceNumber;
use dashmap::DashMap;
use parking_lot::RwLock as ParkingLotRwLock;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tracing::{debug, instrument};

/// Calculate a conservative maximum B-Tree order based on page size.
///
/// Since we have multiple factors that affect final page size:
/// - Variable-length keys/values (especially string keys which can be huge)
/// - 3 compression algorithms (None, LZ4, Zstd) with different expansion characteristics
/// - Encryption (AES-256-GCM adds 12-byte nonce + 16-byte tag = 28 bytes overhead)
/// - MVCC version chains (variable length)
///
/// We use a very conservative estimate to minimize the chance of write failures.
/// The safety margin accounts for worst-case compression expansion (~15%) and encryption overhead.
///
/// Formula:
/// - Available space = page_data_size (already excludes header/checksum)
/// - Safety margin = 40% (for compression expansion, encryption, and overhead)
/// - Target size = available_space * 0.60
/// - Conservative estimate: 30 bytes per entry (works for small-medium keys/values)
/// - Order = target_size / 30
///
/// For variable-length data or unfavorable compression, nodes may need to split earlier.
/// The write_node method will detect if a node doesn't fit and return an error,
/// which triggers a split-and-retry cycle.
fn calculate_max_order(page_data_size: usize) -> usize {
    // Use 60% of available space to leave large margin for:
    // - Compression expansion (worst case ~15%)
    // - Encryption overhead (28 bytes for AES-256-GCM)
    // - Serialization overhead
    // - Variable-length keys/values
    let target_size = (page_data_size * 3) / 5; // 60%

    // Conservative estimate: 30 bytes per entry on average
    // This works well for small-medium keys/values
    // For large keys/values, nodes will split earlier
    const BYTES_PER_ENTRY: usize = 30;

    // Calculate order, with minimum of 4 (B-Tree requirement)
    let order = target_size / BYTES_PER_ENTRY;
    order.max(4)
}

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

/// Entry in the optimistic path tracking.
/// Records a node's page ID, version, and which child index was followed.
#[derive(Debug, Clone)]
struct PathEntry {
    page_id: PageId,
    version: PageVersion,
    child_index: usize, // Index of the child that was followed (for internal nodes)
}

/// Page version for optimistic concurrency control.
///
/// Each page has a version number that is incremented on every write.
/// This allows readers to detect if a page has been modified since they read it.
///
/// Uses u64 (8 bytes) for future-proofing and consistency with other IDs.
/// The DEFAULT_ORDER reduction (256→220) provides ample space for this overhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PageVersion(u64);

impl PageVersion {
    /// Create an initial version (0).
    fn initial() -> Self {
        PageVersion(0)
    }

    /// Increment the version.
    fn increment(&self) -> Self {
        PageVersion(self.0.wrapping_add(1))
    }

    /// Get the raw version number.
    fn get(&self) -> u64 {
        self.0
    }
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
///
/// Each node includes a version field for optimistic concurrency control.
/// The version is incremented on every write to detect concurrent modifications.
#[derive(Debug, Clone)]
enum BTreeNode {
    Internal {
        /// Page version for optimistic concurrency control
        version: PageVersion,
        /// Keys and child pointers (`keys.len()` == `children.len()` - 1)
        entries: Vec<InternalEntry>,
        /// Rightmost child pointer
        rightmost_child: PageId,
    },
    Leaf {
        /// Page version for optimistic concurrency control
        version: PageVersion,
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
            version: PageVersion::initial(),
            entries: Vec::new(),
            rightmost_child: PageId::from(0),
        }
    }

    /// Create a new leaf node.
    fn new_leaf() -> Self {
        BTreeNode::Leaf {
            version: PageVersion::initial(),
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

    /// Check if the node is full (count-based check).
    /// Note: This is a preliminary check. The authoritative check is size-based
    /// via `PagedBTree::node_fits_in_page()` which accounts for variable-length keys/values.
    fn is_full(&self, max_order: usize) -> bool {
        self.key_count() >= max_order
    }

    /// Check if the node has minimum keys.
    fn has_minimum_keys(&self, min_keys: usize) -> bool {
        self.key_count() >= min_keys
    }

    /// Get the current version of this node.
    fn get_version(&self) -> PageVersion {
        match self {
            BTreeNode::Internal { version, .. } => *version,
            BTreeNode::Leaf { version, .. } => *version,
        }
    }

    /// Create a new node with an incremented version.
    fn with_incremented_version(&self) -> Self {
        match self {
            BTreeNode::Internal {
                version,
                entries,
                rightmost_child,
            } => BTreeNode::Internal {
                version: version.increment(),
                entries: entries.clone(),
                rightmost_child: *rightmost_child,
            },
            BTreeNode::Leaf {
                version,
                entries,
                next_leaf,
            } => BTreeNode::Leaf {
                version: version.increment(),
                entries: entries.clone(),
                next_leaf: *next_leaf,
            },
        }
    }

    /// Serialize the node to bytes.
    ///
    /// Returns the serialized bytes. Note: This does NOT include compression,
    /// which happens at the pager level. The serialized size should be well
    /// under the page data size to allow for compression overhead.
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Write node type (1 byte)
        match self {
            BTreeNode::Internal {
                version,
                entries,
                rightmost_child,
            } => {
                bytes.push(0); // Internal node

                // Write version (8 bytes)
                bytes.extend_from_slice(&version.get().to_le_bytes());

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
            BTreeNode::Leaf {
                version,
                entries,
                next_leaf,
            } => {
                bytes.push(1); // Leaf node

                // Write version (8 bytes)
                bytes.extend_from_slice(&version.get().to_le_bytes());

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

        // Safety check: Warn if serialized size is approaching page limits
        // Page data size is typically 4032 bytes. We want to stay well under
        // that to allow for compression overhead and future growth.
        const WARN_THRESHOLD: usize = 3500; // Conservative threshold
        if bytes.len() > WARN_THRESHOLD {
            tracing::warn!(
                "BTreeNode serialized to {} bytes (threshold: {}). Consider reducing DEFAULT_ORDER or implementing size-based splitting.",
                bytes.len(),
                WARN_THRESHOLD
            );
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
                // Read version (8 bytes)
                if bytes.len() < offset + 8 {
                    return Err(crate::table::TableError::corruption(
                        "BTreeNode::from_bytes",
                        "truncated_data",
                        "Insufficient data for version",
                    ));
                }
                let version = PageVersion(u64::from_le_bytes(
                    bytes[offset..offset + 8].try_into().unwrap(),
                ));
                offset += 8;

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
                    version,
                    entries,
                    rightmost_child,
                })
            }
            1 => {
                // Leaf node
                // Read version (8 bytes)
                if bytes.len() < offset + 8 {
                    return Err(crate::table::TableError::corruption(
                        "BTreeNode::from_bytes",
                        "truncated_data",
                        "Insufficient data for version",
                    ));
                }
                let version = PageVersion(u64::from_le_bytes(
                    bytes[offset..offset + 8].try_into().unwrap(),
                ));
                offset += 8;

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

                Ok(BTreeNode::Leaf {
                    version,
                    entries,
                    next_leaf,
                })
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
///
/// # Dynamic Node Sizing
///
/// This B-Tree implementation uses **dynamic node sizing** to prevent page overflow errors.
/// Unlike traditional B-Trees with fixed order, we adapt to variable-length data and
/// unpredictable compression/encryption overhead.
///
/// ## The Problem
///
/// Fixed B-Tree order (e.g., `const DEFAULT_ORDER = 220`) fails when:
/// - **Variable-length keys/values**: String keys can be arbitrarily large
/// - **Compression variability**: 3 algorithms (None, LZ4, Zstd) with different characteristics
///   - Worst case: incompressible data expands by ~15%
/// - **Encryption overhead**: AES-256-GCM adds 28 bytes (12-byte nonce + 16-byte tag)
/// - **MVCC version chains**: Variable length depending on transaction history
/// - **Page size variability**: Pages can be 4KB to 64KB
///
/// A node with 220 small entries might fit, but 220 large entries will overflow the page,
/// causing write failures like: `compressed data length 4053 exceeds available space 4032`
///
/// ## The Solution: Dual-Check Strategy
///
/// We use **two complementary checks** to prevent overflow:
///
/// 1. **Count-based check (preliminary)**: `node.keys.len() >= max_order`
///    - Fast, conservative estimate based on average entry size
///    - Computed at initialization: `max_order = (page_size * 0.60) / 30 bytes`
///    - 40% safety margin accounts for compression expansion and encryption
///
/// 2. **Size-based check (authoritative)**: `node.to_bytes().len() > max_node_size`
///    - Actual serialized size check before writing
///    - Catches cases where entries are larger than average
///    - Triggers split even if count is below max_order
///
/// ## Implementation Details
///
/// - `max_order`: Computed from page size, used for preliminary checks
/// - `min_keys`: `max_order / 2`, minimum keys per node (B-Tree property)
/// - `max_node_size`: 60% of available page space (40% safety margin)
///
/// Both checks are used together:
/// ```rust,ignore
/// if node.is_full(self.max_order) || !self.node_fits_in_page(&node) {
///     // Split the node
/// }
/// ```
///
/// This ensures nodes never exceed page capacity, regardless of data characteristics.
///
/// ## Trade-offs
///
/// - **Pro**: Prevents all page overflow errors
/// - **Pro**: Adapts to actual data characteristics
/// - **Pro**: Works with any page size (4KB-64KB)
/// - **Con**: May split nodes earlier than necessary (conservative)
/// - **Con**: Slight overhead from size checks (but prevents expensive error recovery)
///
/// The conservative approach is intentional: better to split early than fail writes.
pub struct PagedBTree<FS: FileSystem> {
    id: TableId,
    name: String,
    pager: Arc<Pager<FS>>,
    /// Root page ID wrapped in Arc<RwLock> to allow atomic updates during root splits
    root_page_id: Arc<RwLock<PageId>>,
    /// Row count wrapped in Arc<RwLock> for atomic updates
    row_count: Arc<RwLock<u64>>,
    /// Per-page latches for B-tree structure modifications (latch coupling)
    /// Uses `DashMap` for concurrent access to different pages
    /// Each page has an `RwLock` for read/write latching
    page_latches: Arc<DashMap<PageId, Arc<ParkingLotRwLock<()>>>>,
    /// Maximum keys per node (computed from page size with 40% safety margin)
    /// Used for preliminary count-based checks. See struct-level docs for details.
    max_order: usize,
    /// Minimum keys per node (except root): `max_order / 2`
    /// Maintains B-Tree balance property.
    min_keys: usize,
    /// Maximum serialized size for a node (60% of page data size)
    /// Used for authoritative size-based checks. See struct-level docs for details.
    max_node_size: usize,
}

impl<FS: FileSystem> PagedBTree<FS> {
    /// Create a new paged B-Tree table.
    pub fn new(id: TableId, name: String, pager: Arc<Pager<FS>>) -> TableResult<Self> {
        // Calculate order based on page size
        let page_data_size = pager.page_size().data_size();
        let max_order = calculate_max_order(page_data_size);
        let min_keys = max_order / 2;

        // Set maximum node size with 40% safety margin for compression expansion and encryption
        let max_node_size = (page_data_size * 3) / 5; // 60% of available space

        // Allocate root page (initially a leaf)
        let root_page_id = pager.allocate_page(PageType::BTreeLeaf)?;
        let root_node = BTreeNode::new_leaf();

        // Write root node to disk
        let mut page = Page::new(root_page_id, PageType::BTreeLeaf, page_data_size);
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
            page_latches: Arc::new(DashMap::new()),
            max_order,
            min_keys,
            max_node_size,
        })
    }

    /// Open an existing paged B-Tree table.
    #[must_use]
    pub fn open(id: TableId, name: String, pager: Arc<Pager<FS>>, root_page_id: PageId) -> Self {
        // Calculate order based on page size
        let page_data_size = pager.page_size().data_size();
        let max_order = calculate_max_order(page_data_size);
        let min_keys = max_order / 2;

        // Set maximum node size with 40% safety margin for compression expansion and encryption
        let max_node_size = (page_data_size * 3) / 5; // 60% of available space

        let row_count = pager.btree_row_count();
        Self {
            id,
            name,
            pager,
            root_page_id: Arc::new(RwLock::new(root_page_id)),
            row_count: Arc::new(RwLock::new(row_count)),
            page_latches: Arc::new(DashMap::new()),
            max_order,
            min_keys,
            max_node_size,
        }
    }

    /// Check if a node fits within the page size limit.
    /// This is the authoritative check for whether a node can be written.
    fn node_fits_in_page(&self, node: &BTreeNode) -> bool {
        let serialized_size = node.to_bytes().len();
        serialized_size <= self.max_node_size
    }

    /// Get the current root page ID.
    #[must_use]
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
    // =============================================================================
    // Page Latching for Concurrency Control
    // =============================================================================

    /// Get or create a latch for a page.
    /// Returns the Arc<RwLock> that can be used to acquire read/write guards.
    fn get_page_latch(&self, page_id: PageId) -> Arc<ParkingLotRwLock<()>> {
        self.page_latches
            .entry(page_id)
            .or_insert_with(|| Arc::new(ParkingLotRwLock::new(())))
            .clone()
    }

    /// Check if a node is "safe" for the given operation.
    /// Safe means the operation won't cause a split or merge that propagates upward.
    fn is_node_safe(&self, node: &BTreeNode, is_insert: bool) -> bool {
        if is_insert {
            // Safe for insert if not full (won't split)
            // Use both count-based and size-based checks
            !node.is_full(self.max_order) && self.node_fits_in_page(node)
        } else {
            // Safe for delete if has more than minimum keys (won't merge/redistribute)
            node.key_count() > self.min_keys
        }
    }

    /// Read a node from disk.
    #[instrument(skip(self), fields(page_id = %page_id))]
    fn read_node(&self, page_id: PageId) -> TableResult<BTreeNode> {
        let start = Instant::now();
        let page = self.pager.read_page(page_id)?;
        let result = BTreeNode::from_bytes(page.data());

        if result.is_ok() {
            crate::table::metrics::btree::record_node_read();
            crate::table::metrics::btree::record_node_read_duration(start);
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

        crate::table::metrics::btree::record_node_write();
        crate::table::metrics::btree::record_node_write_duration(start);
        Ok(())
    }

    /// Search for a key in the tree, returning the leaf page ID and position.
    #[instrument(skip(self, key), fields(key_len = key.len()))]
    fn search(&self, key: &[u8]) -> TableResult<(PageId, usize)> {
        let start = Instant::now();
        debug!("BTree search operation");

        let (leaf_page_id, pos, _path) = self.search_with_path(key)?;

        crate::table::metrics::btree::record_search_duration(start);
        Ok((leaf_page_id, pos))
    }

    /// Search for a key in the tree, tracking the path from root to leaf.
    /// Returns the leaf page ID, position, and path (list of (`parent_page_id`, `child_page_id`) tuples).
    fn search_with_path(&self, key: &[u8]) -> TableResult<(PageId, usize, Vec<(PageId, PageId)>)> {
        let mut current_page_id = self.get_root_page_id();
        let mut path = Vec::new();

        loop {
            let node = self.read_node(current_page_id)?;

            match node {
                BTreeNode::Internal {
                    entries,
                    rightmost_child,
                    version,
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

    /// Search for a key optimistically, recording page versions along the path.
    /// Returns the leaf page ID, position, and the optimistic path with versions.
    /// This is used for optimistic concurrency control - the path can be validated later.
    fn search_optimistic(&self, key: &[u8]) -> TableResult<(PageId, usize, Vec<PathEntry>)> {
        let mut current_page_id = self.get_root_page_id();
        let mut path = Vec::new();

        loop {
            // Read node WITHOUT holding any latch (optimistic read)
            let node = self.read_node(current_page_id)?;
            let version = node.get_version();

            match node {
                BTreeNode::Internal {
                    entries,
                    rightmost_child,
                    ..
                } => {
                    // Binary search for the appropriate child
                    let pos = entries.binary_search_by(|e| e.key.as_slice().cmp(key));
                    let (child_page_id, child_index) = match pos {
                        Ok(idx) => {
                            // Found exact match at idx
                            // Keys >= entries[idx].key go to the right of this entry
                            if idx + 1 < entries.len() {
                                (entries[idx + 1].child_page_id, idx + 1)
                            } else {
                                (rightmost_child, entries.len())
                            }
                        }
                        Err(idx) => {
                            // Key would be inserted at position idx
                            // This means key < entries[idx].key (or idx == len)
                            if idx < entries.len() {
                                (entries[idx].child_page_id, idx)
                            } else {
                                (rightmost_child, entries.len())
                            }
                        }
                    };

                    // Record this node in the path with its version
                    path.push(PathEntry {
                        page_id: current_page_id,
                        version,
                        child_index,
                    });

                    current_page_id = child_page_id;
                }
                BTreeNode::Leaf { entries, .. } => {
                    // Found the leaf node
                    let pos = entries.binary_search_by(|e| e.key.as_slice().cmp(key));
                    let idx = match pos {
                        Ok(i) => i,
                        Err(i) => i,
                    };

                    // Record the leaf in the path (though we won't validate it the same way)
                    path.push(PathEntry {
                        page_id: current_page_id,
                        version,
                        child_index: idx,
                    });

                    return Ok((current_page_id, idx, path));
                }
            }
        }
    }

    /// Validate an optimistic path by checking if any nodes have been modified.
    /// Returns Ok(true) if the path is still valid, Ok(false) if there was a conflict.
    /// This should be called while holding a latch on the target node.
    fn validate_optimistic_path(&self, path: &[PathEntry]) -> TableResult<bool> {
        // Validate all nodes in the path (except the last one, which is the leaf we're modifying)
        // We validate from parent to root to detect conflicts early
        for entry in path.iter().rev().skip(1) {
            let node = self.read_node(entry.page_id)?;
            let current_version = node.get_version();

            if current_version != entry.version {
                // Version mismatch - node was modified since we read it
                crate::table::metrics::btree::record_optimistic_conflict();
                return Ok(false);
            }
        }

        Ok(true)
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
            if let Some(value) = entries[pos].chain.find_visible_inline(&snapshot) {
                if value.is_empty() {
                    return Ok(None);
                }

                // Check if this is an encoded ValueRef (external value)
                if value.len() >= 1 && (value[0] == 0x01 || value[0] == 0x02) {
                    // Try to decode as ValueRef
                    if let Ok(value_ref) = ValueRef::decode(value) {
                        if value_ref.requires_overflow() {
                            // Read from overflow pages
                            match value_ref {
                                ValueRef::SinglePage {
                                    page_id,
                                    offset,
                                    length,
                                } => {
                                    let page =
                                        self.pager.read_page(PageId::from(page_id as u64))?;
                                    let data = &page.data()
                                        [offset as usize..offset as usize + length as usize];
                                    return Ok(Some(ValueBuf(data.to_vec())));
                                }
                                ValueRef::OverflowChain {
                                    first_page_id,
                                    total_length,
                                    ..
                                } => {
                                    let data = self
                                        .pager
                                        .read_overflow_chain(PageId::from(first_page_id as u64))?;
                                    if data.len() != total_length as usize {
                                        return Err(TableError::corruption(
                                            "get_internal",
                                            "overflow_chain_length_mismatch",
                                            format!(
                                                "Expected {} bytes, got {}",
                                                total_length,
                                                data.len()
                                            ),
                                        ));
                                    }
                                    return Ok(Some(ValueBuf(data)));
                                }
                                ValueRef::Inline => {
                                    // This shouldn't happen, but fall through to return the raw value
                                }
                            }
                        }
                    }
                }

                // Return inline value
                return Ok(Some(ValueBuf(value.to_vec())));
            }
        }

        Ok(None)
    }

    /// Split a full node into two nodes.
    /// Returns the new right sibling page ID and the median key that should be promoted to parent.
    #[instrument(skip(self, node), fields(page_id = %page_id))]
    fn split_node(&self, page_id: PageId, node: &BTreeNode) -> TableResult<(PageId, Vec<u8>)> {
        let mid = node.key_count() / 2;

        // Record split metric
        crate::table::metrics::btree::record_split();

        match node {
            BTreeNode::Internal {
                version,
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
                    version: PageVersion::initial(),
                    entries: right_entries,
                    rightmost_child: right_rightmost,
                };

                // Update left node (original page) with incremented version
                let left_node = BTreeNode::Internal {
                    version: version.increment(),
                    entries: left_entries,
                    rightmost_child: left_rightmost,
                };

                // Write both nodes
                self.write_node(page_id, &left_node)?;
                self.write_node(right_page_id, &right_node)?;

                debug!("Split internal node");
                Ok((right_page_id, median_key))
            }
            BTreeNode::Leaf {
                version,
                entries,
                next_leaf,
            } => {
                // Split leaf node
                let median_key = entries[mid].key.clone();

                // Left node keeps entries [0..mid]
                let left_entries = entries[..mid].to_vec();

                // Right node gets entries [mid..]
                let right_entries = entries[mid..].to_vec();

                // Create new right sibling
                let right_page_id = self.pager.allocate_page(PageType::BTreeLeaf)?;
                let right_node = BTreeNode::Leaf {
                    version: PageVersion::initial(),
                    entries: right_entries,
                    next_leaf: *next_leaf,
                };

                // Update left node to point to new right sibling
                let left_node = BTreeNode::Leaf {
                    version: version.increment(),
                    entries: left_entries,
                    next_leaf: right_page_id,
                };

                // Write both nodes
                self.write_node(page_id, &left_node)?;
                self.write_node(right_page_id, &right_node)?;

                debug!("Split leaf node");
                Ok((right_page_id, median_key))
            }
        }
    }

    /// Merge two adjacent nodes (left and right).
    /// The right node is merged into the left node, and the right node is freed.
    /// Returns true if merge was successful.
    #[instrument(skip(self, separator_key), fields(left_page_id = %left_page_id, right_page_id = %right_page_id))]
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
                    version: left_version,
                    entries: left_entries,
                    rightmost_child: left_rightmost,
                },
                BTreeNode::Internal {
                    entries: right_entries,
                    rightmost_child: right_rightmost,
                    ..
                },
            ) => {
                // Check if merge is possible
                if left_entries.len() + right_entries.len() + 1 > self.max_order {
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

                // Create merged node with incremented version
                let merged_node = BTreeNode::Internal {
                    version: left_version.increment(),
                    entries: merged_entries,
                    rightmost_child: right_rightmost,
                };

                // Write merged node and free right node
                self.write_node(left_page_id, &merged_node)?;
                self.pager.free_page(right_page_id)?;

                // Record merge metric
                crate::table::metrics::btree::record_merge();
                debug!("Merged internal nodes");

                Ok(true)
            }
            (
                BTreeNode::Leaf {
                    version: left_version,
                    entries: left_entries,
                    ..
                },
                BTreeNode::Leaf {
                    entries: right_entries,
                    next_leaf: right_next,
                    ..
                },
            ) => {
                // Check if merge is possible
                if left_entries.len() + right_entries.len() > self.max_order {
                    return Ok(false);
                }

                let mut merged_entries = left_entries;

                // Add all right entries
                merged_entries.extend(right_entries);

                // Create merged node (left now points to right's next) with incremented version
                let merged_node = BTreeNode::Leaf {
                    version: left_version.increment(),
                    entries: merged_entries,
                    next_leaf: right_next,
                };

                // Write merged node and free right node
                self.write_node(left_page_id, &merged_node)?;
                self.pager.free_page(right_page_id)?;

                // Record merge metric
                crate::table::metrics::btree::record_merge();
                debug!("Merged leaf nodes");

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
                    version: left_version,
                    entries: left_entries,
                    rightmost_child: left_rightmost,
                },
                BTreeNode::Internal {
                    version: right_version,
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

                        // Update nodes with incremented versions
                        let new_left = BTreeNode::Internal {
                            version: left_version.increment(),
                            entries: left_entries,
                            rightmost_child: new_left_rightmost,
                        };
                        let new_right = BTreeNode::Internal {
                            version: right_version.increment(),
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

                    // Update nodes with incremented versions
                    let new_left = BTreeNode::Internal {
                        version: left_version.increment(),
                        entries: left_entries,
                        rightmost_child: new_left_rightmost,
                    };
                    let new_right = BTreeNode::Internal {
                        version: right_version.increment(),
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
                    version: left_version,
                    entries: left_entries,
                    next_leaf: left_next,
                },
                BTreeNode::Leaf {
                    version: right_version,
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

                    // Update nodes with incremented versions
                    let new_left = BTreeNode::Leaf {
                        version: left_version.increment(),
                        entries: left_entries,
                        next_leaf: left_next,
                    };
                    let new_right = BTreeNode::Leaf {
                        version: right_version.increment(),
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

                    // Update nodes with incremented versions
                    let new_left = BTreeNode::Leaf {
                        version: left_version.increment(),
                        entries: left_entries,
                        next_leaf: left_next,
                    };
                    let new_right = BTreeNode::Leaf {
                        version: right_version.increment(),
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

    /// Insert a key-value pair into the tree with optimistic concurrency control.
    ///
    /// This implementation uses optimistic concurrency:
    /// - Traverses tree WITHOUT latches, recording page versions
    /// - Acquires write latch only on the target leaf
    /// - Validates that the path hasn't changed (version check)
    /// - Retries from root if conflict detected
    ///
    /// This dramatically reduces lock contention compared to latch coupling.
    fn insert_internal(
        &self,
        key: Vec<u8>,
        value: Vec<u8>,
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        const MAX_RETRIES: usize = 10;
        let mut retry_count = 0;

        loop {
            // Phase 1: Optimistic traversal (no latches)
            let (leaf_page_id, _pos, optimistic_path) = self.search_optimistic(&key)?;

            // Phase 2: Acquire write latch on leaf
            let leaf_latch = self.get_page_latch(leaf_page_id);
            let _leaf_guard = leaf_latch.write();

            // Phase 3: Validate the optimistic path
            if !self.validate_optimistic_path(&optimistic_path)? {
                // Conflict detected - path changed during traversal
                crate::table::metrics::btree::record_optimistic_retry();
                retry_count += 1;

                if retry_count >= MAX_RETRIES {
                    return Err(crate::table::TableError::Other(format!(
                        "Insert failed after {} retries due to high contention",
                        MAX_RETRIES
                    )));
                }

                // Drop the latch and retry from root
                drop(_leaf_guard);
                continue;
            }

            // Phase 4: Path is valid, perform the insert
            // Re-read the leaf node (we have the latch, so it's stable)
            let node = self.read_node(leaf_page_id)?;

            let (version, mut entries, next_leaf, is_new_key) = if let BTreeNode::Leaf {
                version,
                entries,
                next_leaf,
            } = node
            {
                // Find insertion position
                let current_pos = entries.binary_search_by(|e| e.key.as_slice().cmp(&key));
                let pos = match current_pos {
                    Ok(i) => i,
                    Err(i) => i,
                };

                let is_new_key = current_pos.is_err();
                let mut entries = entries;

                // Perform the insertion/update
                if pos < entries.len() && entries[pos].key == key {
                    // Update existing entry's version chain
                    let old_chain = entries[pos].chain.clone();
                    let mut new_chain = old_chain.prepend(value, tx_id);
                    if commit_lsn.as_u64() > 0 {
                        new_chain.commit(commit_lsn);
                    }
                    entries[pos].chain = new_chain;
                } else {
                    // Insert new entry
                    let mut chain = VersionChain::new(value, tx_id);
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

                (version, entries, next_leaf, is_new_key)
            } else {
                // Node type changed - this is a conflict, retry
                crate::table::metrics::btree::record_optimistic_retry();
                retry_count += 1;

                if retry_count >= MAX_RETRIES {
                    return Err(crate::table::TableError::corruption(
                        "PagedBTree::insert_internal",
                        "node_type_changed",
                        "Leaf node changed to internal during insert",
                    ));
                }

                drop(_leaf_guard);
                continue;
            };

            // Reconstruct the modified node with incremented version
            let modified_node = BTreeNode::Leaf {
                version: version.increment(),
                entries,
                next_leaf,
            };

            // Write the updated node (still holding latch)
            self.write_node(leaf_page_id, &modified_node)?;

            // Check if split is needed (count-based OR size-based)
            if modified_node.is_full(self.max_order) || !self.node_fits_in_page(&modified_node) {
                // Convert optimistic path to old-style path for split_and_propagate
                // TODO: Phase 4 will make split_and_propagate optimistic too
                let old_style_path: Vec<(PageId, PageId)> = optimistic_path
                    .iter()
                    .take(optimistic_path.len() - 1) // Exclude the leaf itself
                    .zip(optimistic_path.iter().skip(1))
                    .map(|(parent, child)| (parent.page_id, child.page_id))
                    .collect();

                self.split_and_propagate(leaf_page_id, &modified_node, old_style_path)?;
            }

            // Update row count if new key
            if is_new_key {
                self.increment_row_count()?;
            }

            // Success!
            crate::table::metrics::btree::record_optimistic_success();

            // Leaf latch released when _leaf_guard goes out of scope
            return Ok(());
        }
    }

    /// Split a node and propagate the split up the tree.
    /// The path parameter contains (`parent_page_id`, `child_page_id`) tuples from root to the node being split.
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
                version: PageVersion::initial(),
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
    /// The path contains (`parent_page_id`, `child_page_id`) tuples from root to the child that was split.
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
            ..
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

            // Check if parent is now full and needs to split (count-based OR size-based)
            if parent_node.is_full(self.max_order) || !self.node_fits_in_page(&parent_node) {
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
            ..
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
                ..
            } => (entries, rightmost_child),
            _ => {
                return Err(crate::table::TableError::corruption(
                    "PagedBTree::rebalance_leaf_after_delete",
                    "parent_not_internal",
                    format!("Parent {:?} was not an internal node", parent_page_id),
                ));
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
                    ..
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
                    ..
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

                // Do not rebalance on MVCC tombstone insert.
                // The physical key remains present in the leaf; only its visible
                // version changes. Rebalancing based on key count here can merge or
                // redistribute populated leaves and corrupt parent separator routing.

                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Commit all uncommitted versions created by the given transaction.
    ///
    /// Traverses all leaf pages and marks versions created by `tx_id` with the given `commit_lsn`.
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
            if let BTreeNode::Leaf {
                version,
                entries,
                next_leaf,
            } = node
            {
                let mut new_entries = entries.clone();

                for entry in new_entries.iter_mut() {
                    Self::commit_chain_recursive(&mut entry.chain, tx_id, commit_lsn);
                }

                // Write back the updated node with incremented version
                let updated_node = BTreeNode::Leaf {
                    version: version.increment(),
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
            BTreeNode::Internal {
                entries,
                rightmost_child,
                ..
            } => {
                stats.internal_node_count += 1;

                // Recursively process all children
                for entry in &entries {
                    self.collect_node_statistics(entry.child_page_id, depth + 1, stats, page_size)?;
                }
                self.collect_node_statistics(rightmost_child, depth + 1, stats, page_size)?;
            }
            BTreeNode::Leaf { entries, .. } => {
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
        let _start = Instant::now();

        // Collect statistics by traversing the tree
        let stats = self.collect_tree_statistics()?;

        // Update tree structure metrics
        crate::table::metrics::btree::set_tree_height(stats.tree_depth);
        crate::table::metrics::btree::set_internal_nodes(stats.internal_node_count);
        crate::table::metrics::btree::set_leaf_nodes(stats.leaf_node_count);

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
            histogram: None,        // TODO: Implement histogram buckets if needed
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
            streaming_contexts: HashMap::new(),
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

/// Stream for reading a value from a single overflow page
struct SinglePageStream<'a, FS: FileSystem> {
    pager: &'a Pager<FS>,
    page_id: PageId,
    offset: usize,
    length: usize,
    position: usize,
    buffer: Option<Vec<u8>>,
}

impl<'a, FS: FileSystem> SinglePageStream<'a, FS> {
    fn new(pager: &'a Pager<FS>, page_id: PageId, offset: usize, length: usize) -> Self {
        Self {
            pager,
            page_id,
            offset,
            length,
            position: 0,
            buffer: None,
        }
    }

    fn load_data(&mut self) -> TableResult<()> {
        if self.buffer.is_some() {
            return Ok(());
        }

        let page = self
            .pager
            .read_page(self.page_id)
            .map_err(TableError::Pager)?;

        if self.offset + self.length > page.data().len() {
            return Err(TableError::corruption(
                format!("SinglePage at page {}", self.page_id.as_u64()),
                "value_overflow",
                format!(
                    "Value extends beyond page boundary: offset={}, length={}, page_size={}",
                    self.offset,
                    self.length,
                    page.data().len()
                ),
            ));
        }

        let data = &page.data()[self.offset..self.offset + self.length];
        self.buffer = Some(data.to_vec());
        Ok(())
    }
}

impl<'a, FS: FileSystem> crate::table::ValueStream for SinglePageStream<'a, FS> {
    fn read(&mut self, buf: &mut [u8]) -> TableResult<usize> {
        // Load data on first read
        self.load_data()?;

        let buffer = self.buffer.as_ref().unwrap();
        let remaining = buffer.len() - self.position;

        if remaining == 0 {
            return Ok(0);
        }

        let to_copy = remaining.min(buf.len());
        buf[..to_copy].copy_from_slice(&buffer[self.position..self.position + to_copy]);
        self.position += to_copy;
        Ok(to_copy)
    }

    fn size_hint(&self) -> Option<u64> {
        Some(self.length as u64)
    }
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
        use crate::pager::OverflowChainStream;
        use crate::table::SliceValueStream;

        // Get the value using existing get_internal
        let value_opt = self.table.get_internal(key, snapshot_lsn)?;

        match value_opt {
            None => Ok(None),
            Some(value_buf) => {
                let value = &value_buf.0;

                // Check if this is an encoded ValueRef
                // ValueRef encoding: 0x01 = SinglePage, 0x02 = OverflowChain, 0x00 = Inline
                if value.len() >= 1 && (value[0] == 0x01 || value[0] == 0x02) {
                    // Decode ValueRef
                    let value_ref = ValueRef::decode(value).map_err(|e| {
                        TableError::corruption(
                            "ValueRef decode",
                            "invalid_encoding",
                            format!("Failed to decode ValueRef: {:?}", e),
                        )
                    })?;

                    match value_ref {
                        ValueRef::SinglePage {
                            page_id,
                            offset,
                            length,
                        } => {
                            // Return stream for single page
                            Ok(Some(Box::new(SinglePageStream::new(
                                &self.table.pager,
                                PageId::from(page_id as u64),
                                offset as usize,
                                length as usize,
                            ))))
                        }
                        ValueRef::OverflowChain {
                            first_page_id,
                            total_length,
                            ..
                        } => {
                            // Return stream for overflow chain
                            Ok(Some(Box::new(OverflowChainStream::new(
                                &self.table.pager,
                                PageId::from(first_page_id as u64),
                                total_length,
                            ))))
                        }
                        ValueRef::Inline => {
                            // This shouldn't happen - inline values aren't encoded as ValueRef
                            // Return the raw value as a stream
                            Ok(Some(Box::new(SliceValueStream::new(value.to_vec()))))
                        }
                    }
                } else {
                    // Regular inline value - return as stream
                    Ok(Some(Box::new(SliceValueStream::new(value.to_vec()))))
                }
            }
        }
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

// =============================================================================
// Streaming Infrastructure
// =============================================================================

/// Represents a pending change in the writer's buffer.
#[derive(Debug, Clone)]
enum PendingChange {
    /// Insert/update with inline value
    Inline { key: Vec<u8>, value: Vec<u8> },
    /// Insert/update with external value (already written to overflow pages)
    External { key: Vec<u8>, value_ref: ValueRef },
    /// Delete operation
    Delete { key: Vec<u8> },
}

/// Context for streaming a value to overflow pages.
/// Tracks allocated pages for rollback capability.
#[derive(Debug)]
struct StreamingContext {
    /// Pages allocated so far (for rollback)
    allocated_pages: Vec<PageId>,
    /// Total bytes written
    total_written: u64,
}

impl StreamingContext {
    /// Create a new streaming context
    fn new() -> Self {
        Self {
            allocated_pages: Vec::new(),
            total_written: 0,
        }
    }

    /// Add a page to the context
    fn add_page(&mut self, page_id: PageId, bytes_written: usize) {
        self.allocated_pages.push(page_id);
        self.total_written += bytes_written as u64;
    }

    /// Rollback by freeing all allocated pages
    fn rollback<FS: FileSystem>(&mut self, pager: &mut Pager<FS>) -> TableResult<()> {
        for page_id in self.allocated_pages.drain(..) {
            pager.free_page(page_id)?;
        }
        self.total_written = 0;
        Ok(())
    }

    /// Commit the context, returning allocated pages and total bytes written
    fn commit(self) -> (Vec<PageId>, u64) {
        (self.allocated_pages, self.total_written)
    }
}

// =============================================================================
// Writer
// =============================================================================
/// Helper stream that combines buffered data with a remaining stream.
/// Used for adaptive streaming when switching from inline to external storage.
struct CompositeStream<'a> {
    /// Buffered data to read first
    buffered: Vec<u8>,
    /// Current chunk to read
    current_chunk: Vec<u8>,
    /// Position in buffered data
    buffer_pos: usize,
    /// Position in current chunk
    chunk_pos: usize,
    /// Remaining stream
    remaining: &'a mut dyn crate::table::ValueStream,
    /// Whether we've started reading from remaining stream
    reading_remaining: bool,
}

impl<'a> CompositeStream<'a> {
    fn new(
        buffered: Vec<u8>,
        current_chunk: Vec<u8>,
        remaining: &'a mut dyn crate::table::ValueStream,
    ) -> Self {
        Self {
            buffered,
            current_chunk,
            buffer_pos: 0,
            chunk_pos: 0,
            remaining,
            reading_remaining: false,
        }
    }
}

impl<'a> crate::table::ValueStream for CompositeStream<'a> {
    fn read(&mut self, buf: &mut [u8]) -> TableResult<usize> {
        let mut total_read = 0;

        // First, read from buffered data
        if self.buffer_pos < self.buffered.len() {
            let to_copy = (buf.len() - total_read).min(self.buffered.len() - self.buffer_pos);
            buf[total_read..total_read + to_copy]
                .copy_from_slice(&self.buffered[self.buffer_pos..self.buffer_pos + to_copy]);
            self.buffer_pos += to_copy;
            total_read += to_copy;
        }

        // Then read from current chunk
        if total_read < buf.len() && self.chunk_pos < self.current_chunk.len() {
            let to_copy = (buf.len() - total_read).min(self.current_chunk.len() - self.chunk_pos);
            buf[total_read..total_read + to_copy]
                .copy_from_slice(&self.current_chunk[self.chunk_pos..self.chunk_pos + to_copy]);
            self.chunk_pos += to_copy;
            total_read += to_copy;
        }

        // Finally, read from remaining stream
        if total_read < buf.len() {
            self.reading_remaining = true;
            let n = self.remaining.read(&mut buf[total_read..])?;
            total_read += n;
        }

        Ok(total_read)
    }

    fn size_hint(&self) -> Option<u64> {
        // We don't know the total size
        None
    }
}

/// Write view of the paged B-Tree for a specific transaction.
pub struct PagedBTreeWriter<'a, FS: FileSystem> {
    table: &'a PagedBTree<FS>,
    tx_id: TransactionId,
    snapshot_lsn: LogSequenceNumber,
    pending_changes: Vec<PendingChange>,
    streaming_contexts: HashMap<Vec<u8>, StreamingContext>,
}

impl<'a, FS: FileSystem> MutableTable for PagedBTreeWriter<'a, FS> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> TableResult<u64> {
        self.pending_changes.push(PendingChange::Inline {
            key: key.to_vec(),
            value: value.to_vec(),
        });
        // Return approximate size: key + value + overhead
        Ok((key.len() + value.len() + 16) as u64)
    }

    fn put_stream(
        &mut self,
        key: &[u8],
        stream: &mut dyn crate::table::ValueStream,
    ) -> TableResult<u64> {
        let size_hint = stream.size_hint();
        let max_inline = self.max_inline_size().unwrap_or(4096);

        // Strategy 1: Known small value - buffer inline
        if let Some(size) = size_hint {
            if size <= max_inline as u64 {
                return self.put_stream_inline(key, stream, size);
            }
        }

        // Strategy 2: Known large value - stream directly
        if let Some(size) = size_hint {
            if size > max_inline as u64 {
                return self.put_stream_external(key, stream, Some(size));
            }
        }

        // Strategy 3: Unknown size - start inline, switch if needed
        self.put_stream_adaptive(key, stream, max_inline)
    }

    fn delete(&mut self, key: &[u8]) -> TableResult<bool> {
        // Check if key exists
        let exists = self.table.get_internal(key, self.snapshot_lsn)?.is_some();
        if exists {
            self.pending_changes
                .push(PendingChange::Delete { key: key.to_vec() });
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
                self.pending_changes
                    .push(PendingChange::Delete { key: key.to_vec() });
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

        // Collect all pending changes first to avoid borrow checker issues
        let changes: Vec<PendingChange> = self.pending_changes.drain(..).collect();

        // Apply all pending changes (versions remain uncommitted)
        for change in changes {
            match change {
                PendingChange::Inline { key, value } => {
                    // Check if we're replacing an external value
                    if let Some(old_value) = self.table.get_internal(&key, self.snapshot_lsn)? {
                        if Self::is_external_value(old_value.as_ref()) {
                            let value_ref = ValueRef::decode(old_value.as_ref()).map_err(|e| {
                                TableError::InvalidValueRef {
                                    details: format!("Failed to decode ValueRef: {:?}", e),
                                }
                            })?;
                            self.free_value_ref(&value_ref)?;
                        }
                    }

                    // Insert or update with inline value (versions left uncommitted)
                    self.table.insert_internal(
                        key,
                        value,
                        self.tx_id,
                        LogSequenceNumber::from(0),
                    )?;
                }
                PendingChange::External { key, value_ref } => {
                    // Check if we're replacing an external value
                    if let Some(old_value) = self.table.get_internal(&key, self.snapshot_lsn)? {
                        if Self::is_external_value(old_value.as_ref()) {
                            let old_ref = ValueRef::decode(old_value.as_ref()).map_err(|e| {
                                TableError::InvalidValueRef {
                                    details: format!("Failed to decode ValueRef: {:?}", e),
                                }
                            })?;
                            self.free_value_ref(&old_ref)?;
                        }
                    }

                    // Insert or update with external value (overflow pages already allocated)
                    // The value_ref points to the overflow chain that was written during put_stream
                    // Serialize the ValueRef and store it as the value
                    let value_bytes = value_ref.encode();
                    self.table.insert_internal(
                        key,
                        value_bytes,
                        self.tx_id,
                        LogSequenceNumber::from(0),
                    )?;
                }
                PendingChange::Delete { key } => {
                    // Check if we're deleting an external value
                    if let Some(old_value) = self.table.get_internal(&key, self.snapshot_lsn)? {
                        if Self::is_external_value(old_value.as_ref()) {
                            let value_ref = ValueRef::decode(old_value.as_ref()).map_err(|e| {
                                TableError::InvalidValueRef {
                                    details: format!("Failed to decode ValueRef: {:?}", e),
                                }
                            })?;
                            self.free_value_ref(&value_ref)?;
                        }
                    }

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

    /// Check if a value is stored externally (in overflow pages).
    ///
    /// External values are encoded with specific formats:
    /// - SinglePage: [0x01][page_id: u32][offset: u16][length: u32] (11 bytes)
    /// - OverflowChain: [0x02][first_page_id: u32][total_length: u64][page_count: u32] (17 bytes)
    ///
    /// We check both the tag byte AND the expected length to avoid false positives
    /// with inline values that happen to start with 0x01 or 0x02.
    fn is_external_value(value: &[u8]) -> bool {
        match value.first() {
            Some(&0x01) => value.len() == 11, // SinglePage
            Some(&0x02) => value.len() == 17, // OverflowChain
            _ => false,
        }
    }

    /// Free overflow pages referenced by a ValueRef.
    ///
    /// This is called when deleting or replacing values to prevent orphaned pages.
    fn free_value_ref(&self, value_ref: &ValueRef) -> TableResult<()> {
        match value_ref {
            ValueRef::Inline => {
                // No pages to free
                Ok(())
            }
            ValueRef::SinglePage { page_id, .. } => {
                // Free single page
                self.table.pager.free_page(PageId::from(*page_id as u64))?;
                Ok(())
            }
            ValueRef::OverflowChain { first_page_id, .. } => {
                // Free entire overflow chain
                self.table
                    .pager
                    .free_overflow_chain(PageId::from(*first_page_id as u64))?;
                Ok(())
            }
        }
    }

    /// Stream a small value inline (buffer in memory)
    fn put_stream_inline(
        &mut self,
        key: &[u8],
        stream: &mut dyn crate::table::ValueStream,
        expected_size: u64,
    ) -> TableResult<u64> {
        let mut buffer = Vec::with_capacity(expected_size as usize);
        let mut temp_buf = vec![0u8; 8192];

        loop {
            let n = stream.read(&mut temp_buf)?;
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&temp_buf[..n]);
        }

        let total_size = buffer.len();
        self.pending_changes.push(PendingChange::Inline {
            key: key.to_vec(),
            value: buffer,
        });

        Ok((key.len() + total_size + 16) as u64)
    }

    /// Stream a large value directly to overflow pages
    fn put_stream_external(
        &mut self,
        key: &[u8],
        stream: &mut dyn crate::table::ValueStream,
        _size_hint: Option<u64>,
    ) -> TableResult<u64> {
        use crate::pager::OverflowPageHeader;

        // Calculate page data capacity
        let page_size = self.table.pager.page_size().data_size();
        let usable_size = page_size - OverflowPageHeader::SIZE;

        // Initialize streaming context
        let mut ctx = StreamingContext::new();

        let mut temp_buf = vec![0u8; 8192];
        let mut page_buffer = Vec::with_capacity(usable_size);
        let mut first_page_id: Option<PageId> = None;
        let mut prev_page_id: Option<PageId> = None;

        // Stream data chunk by chunk
        loop {
            let n = match stream.read(&mut temp_buf) {
                Ok(n) => n,
                Err(e) => {
                    // Rollback: free all allocated pages
                    for page_id in ctx.allocated_pages.iter() {
                        let _ = self.table.pager.free_page(*page_id);
                    }
                    return Err(e);
                }
            };

            if n == 0 {
                // Flush final partial page if any
                if !page_buffer.is_empty() {
                    match self.write_overflow_chunk(
                        &mut ctx,
                        &page_buffer,
                        None,
                        &mut first_page_id,
                        &mut prev_page_id,
                    ) {
                        Ok(_) => {}
                        Err(e) => {
                            for page_id in ctx.allocated_pages.iter() {
                                let _ = self.table.pager.free_page(*page_id);
                            }
                            return Err(e);
                        }
                    }
                }
                break;
            }

            let mut offset = 0;

            while offset < n {
                let remaining_in_page = usable_size - page_buffer.len();
                let to_copy = (n - offset).min(remaining_in_page);

                page_buffer.extend_from_slice(&temp_buf[offset..offset + to_copy]);
                offset += to_copy;

                // Page full? Write it
                if page_buffer.len() >= usable_size {
                    match self.write_overflow_chunk(
                        &mut ctx,
                        &page_buffer,
                        None,
                        &mut first_page_id,
                        &mut prev_page_id,
                    ) {
                        Ok(_) => {}
                        Err(e) => {
                            for page_id in ctx.allocated_pages.iter() {
                                let _ = self.table.pager.free_page(*page_id);
                            }
                            return Err(e);
                        }
                    }
                    page_buffer.clear();
                }
            }
        }

        // Get committed pages and total length
        let (pages, total_len) = ctx.commit();

        if pages.is_empty() {
            // Empty stream - store as inline empty value
            self.pending_changes.push(PendingChange::Inline {
                key: key.to_vec(),
                value: Vec::new(),
            });
            return Ok((key.len() + 16) as u64);
        }

        // Create ValueRef based on page count
        let value_ref = if pages.len() == 1 {
            ValueRef::SinglePage {
                page_id: pages[0].as_u64() as u32,
                offset: OverflowPageHeader::SIZE as u16,
                length: total_len as u32,
            }
        } else {
            ValueRef::OverflowChain {
                first_page_id: pages[0].as_u64() as u32,
                total_length: total_len,
                page_count: pages.len() as u32,
            }
        };

        // Store in pending changes
        self.pending_changes.push(PendingChange::External {
            key: key.to_vec(),
            value_ref,
        });

        Ok((key.len() + total_len as usize + 16) as u64)
    }

    /// Write a chunk to an overflow page
    fn write_overflow_chunk(
        &mut self,
        ctx: &mut StreamingContext,
        data: &[u8],
        next_page_id: Option<PageId>,
        first_page_id: &mut Option<PageId>,
        prev_page_id: &mut Option<PageId>,
    ) -> TableResult<()> {
        // Allocate new page
        let page_id = self
            .table
            .pager
            .allocate_page(PageType::Overflow)
            .map_err(TableError::Pager)?;
        ctx.add_page(page_id, data.len());

        if first_page_id.is_none() {
            *first_page_id = Some(page_id);
        }

        // Link previous page to this one if exists
        if let Some(prev_id) = *prev_page_id {
            self.table
                .pager
                .link_overflow_pages(prev_id, page_id)
                .map_err(TableError::Pager)?;
        }

        // Write data to page
        self.table
            .pager
            .write_overflow_page(page_id, data, next_page_id)
            .map_err(TableError::Pager)?;

        *prev_page_id = Some(page_id);

        Ok(())
    }

    /// Adaptive streaming: start inline, switch to external if size exceeds threshold
    fn put_stream_adaptive(
        &mut self,
        key: &[u8],
        stream: &mut dyn crate::table::ValueStream,
        max_inline: usize,
    ) -> TableResult<u64> {
        let mut buffer = Vec::with_capacity(max_inline);
        let mut temp_buf = vec![0u8; 8192];

        // Try to buffer inline first
        loop {
            let n = stream.read(&mut temp_buf)?;
            if n == 0 {
                // Entire value fits inline
                let total_size = buffer.len();
                self.pending_changes.push(PendingChange::Inline {
                    key: key.to_vec(),
                    value: buffer,
                });
                return Ok((key.len() + total_size + 16) as u64);
            }

            // Check if adding this chunk would exceed threshold
            if buffer.len() + n > max_inline {
                // Switch to external streaming
                // Create a composite stream: buffered data + remaining stream
                let mut composite = CompositeStream::new(buffer, temp_buf[..n].to_vec(), stream);
                return self.put_stream_external(key, &mut composite, None);
            }

            buffer.extend_from_slice(&temp_buf[..n]);
        }
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

                if let Some(value) = entry.chain.find_visible_inline(&snapshot) {
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

            if let BTreeNode::Leaf {
                entries, next_leaf, ..
            } = node
            {
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

            if let Some(stored_primary_key) = entries[pos].chain.find_visible_inline(&snapshot)
                && stored_primary_key == primary_key
            {
                // Remove the entry
                entries.remove(pos);
                self.write_node(leaf_page_id, &leaf_node)?;
                crate::table::metrics::record_delete("btree");
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
                ..
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
                ..
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
                ..
            } => {
                // Recursively vacuum all child nodes
                for entry in &entries {
                    total_removed += self.vacuum_node(entry.child_page_id, min_visible_lsn)?;
                }
                total_removed += self.vacuum_node(rightmost_child, min_visible_lsn)?;
            }
            BTreeNode::Leaf {
                version,
                mut entries,
                next_leaf,
            } => {
                // Vacuum each entry's version chain and free overflow pages
                for entry in &mut entries {
                    let (removed, freed_refs) = entry.chain.vacuum(min_visible_lsn);
                    total_removed += removed;

                    // Free overflow pages for removed external values
                    if !freed_refs.is_empty() {
                        self.pager.free_value_refs(&freed_refs)?;
                    }
                }

                // Write the updated node back to disk with incremented version
                let updated_node = BTreeNode::Leaf {
                    version: version.increment(),
                    entries,
                    next_leaf,
                };
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
            ..
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
    }
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
        assert!(
            stats.total_size_bytes.unwrap() > 0,
            "Total size should include at least root page"
        );

        // Key and value stats should be present even for empty tree
        assert!(stats.key_stats.is_some(), "Key stats should be set");
        assert!(stats.value_stats.is_some(), "Value stats should be set");
    }
}
