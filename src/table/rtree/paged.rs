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

//! Paged R-Tree implementation for geospatial indexing.

use crate::pager::{PageId, PageType, Pager};
use crate::snap::Snapshot;
use crate::table::{
    GeoHit, GeoPoint, GeoSpatial, GeometryRef, SpecialtyTableCapabilities, SpecialtyTableStats,
    Table, TableCapabilities, TableEngineKind, TableError, TableResult, VerificationReport,
};
use crate::txn::TransactionId;
use crate::types::{KeyBuf, TableId};
use crate::vfs::FileSystem;
use crate::wal::LogSequenceNumber;
use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, RwLock};
use tracing::{debug, instrument};

use super::bulk::str_bulk_load;
use super::config::SpatialConfig;
use super::mbr::Mbr;
use super::node::{InternalEntry, LeafEntry, RTreeNode};
use super::split::{split_internal_entries, split_leaf_entries};

/// Paged R-Tree for geospatial indexing.
///
/// This implementation stores the R-Tree structure across multiple pages,
/// allowing it to scale beyond available memory.
pub struct PagedRTree<FS: FileSystem> {
    /// Table identifier
    table_id: TableId,

    /// Table name
    name: String,

    /// Pager for page management
    pager: Arc<Pager<FS>>,

    /// Root page ID
    root_page_id: RwLock<PageId>,

    /// Configuration
    config: SpatialConfig,

    /// Tree height (number of levels)
    height: RwLock<u32>,

    /// Number of objects indexed
    object_count: RwLock<usize>,

    /// Cache of recently accessed nodes
    node_cache: RwLock<HashMap<PageId, RTreeNode>>,
}

impl<FS: FileSystem> PagedRTree<FS> {
    /// Create a new paged R-Tree.
    pub fn new(
        table_id: TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        config: SpatialConfig,
    ) -> TableResult<Self> {
        config
            .validate()
            .map_err(|e| TableError::Other(format!("Invalid spatial config: {}", e)))?;

        // Allocate root page
        let root_page_id = pager.allocate_page(PageType::RTreeNode)?;

        // Create empty root leaf node
        let root_node = RTreeNode::new_leaf();
        Self::write_node(&pager, root_page_id, &root_node)?;

        Ok(Self {
            table_id,
            name,
            pager,
            root_page_id: RwLock::new(root_page_id),
            config,
            height: RwLock::new(1),
            object_count: RwLock::new(0),
            node_cache: RwLock::new(HashMap::new()),
        })
    }

    /// Open an existing paged R-Tree.
    pub fn open(
        table_id: TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        root_page_id: PageId,
        config: SpatialConfig,
    ) -> TableResult<Self> {
        config
            .validate()
            .map_err(|e| TableError::Other(format!("Invalid spatial config: {}", e)))?;

        // Read root node to determine tree height
        let root_node = Self::read_node(&pager, root_page_id)?;
        let height = Self::calculate_height(&pager, root_page_id, &root_node)?;
        let object_count = Self::count_objects(&pager, root_page_id, &root_node)?;

        Ok(Self {
            table_id,
            name,
            pager,
            root_page_id: RwLock::new(root_page_id),
            config,
            height: RwLock::new(height),
            object_count: RwLock::new(object_count),
            node_cache: RwLock::new(HashMap::new()),
        })
    }

    /// Create a new paged R-Tree using STR bulk loading.
    ///
    /// This is much faster than sequential insertion for large datasets
    /// and produces a better-balanced tree with less overlap.
    pub fn bulk_load(
        table_id: TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        config: SpatialConfig,
        entries: Vec<(Vec<u8>, GeometryRef<'_>)>,
    ) -> TableResult<Self> {
        config
            .validate()
            .map_err(|e| TableError::Other(format!("Invalid spatial config: {}", e)))?;

        // Convert geometries to leaf entries
        // Use transaction ID 0 for bulk load (will be committed immediately)
        let tx_id = TransactionId::from(0);
        let leaf_entries: Vec<LeafEntry> = entries
            .into_iter()
            .map(|(id, geometry)| {
                let mbr = Self::geometry_to_mbr_static(geometry)?;
                Ok(LeafEntry::new(mbr, KeyBuf(id), tx_id))
            })
            .collect::<TableResult<Vec<_>>>()?;

        // Perform bulk loading
        let (root_page_id, height, object_count) = str_bulk_load(&pager, leaf_entries, &config)?;

        Ok(Self {
            table_id,
            name,
            pager,
            root_page_id: RwLock::new(root_page_id),
            config,
            height: RwLock::new(height),
            object_count: RwLock::new(object_count),
            node_cache: RwLock::new(HashMap::new()),
        })
    }

    /// Get the root page ID.
    pub fn root_page_id(&self) -> PageId {
        *self.root_page_id.read().unwrap()
    }

    /// Read a node from a page.
    fn read_node(pager: &Pager<FS>, page_id: PageId) -> TableResult<RTreeNode> {
        let page = pager.read_page(page_id)?;
        RTreeNode::from_bytes(page.data()).map_err(|e| {
            TableError::corruption(
                format!("page {}", page_id),
                "rtree_node_decode",
                format!("Failed to parse node: {}", e),
            )
        })
    }

    /// Write a node to a page.
    fn write_node(pager: &Pager<FS>, page_id: PageId, node: &RTreeNode) -> TableResult<()> {
        let node_bytes = node.to_bytes();
        let page_len = node_bytes.len();
        let header_len = crate::pager::PageHeader::SIZE;
        let checksum_len = crate::pager::Page::CHECKSUM_SIZE;
        let total_page_size = pager.page_size().to_u32() as usize;
        let available_space = total_page_size - header_len - checksum_len;
        
        if page_len > available_space {
            return Err(TableError::Other(format!(
                "R-Tree node too large for page {page_id}: payload={} available={} (entries={})",
                page_len,
                available_space,
                node.entry_count()
            )));
        }
        
        let mut page =
            crate::pager::Page::new(page_id, PageType::RTreeNode, pager.page_size().data_size());
        page.data_mut().extend_from_slice(&node_bytes);
        pager.write_page(&page)?;
        Ok(())
    }
    
    /// Check if a node would fit in a page after adding an entry.
    fn would_fit_in_page(pager: &Pager<FS>, node: &RTreeNode) -> bool {
        let node_bytes = node.to_bytes();
        let page_len = node_bytes.len();
        let header_len = crate::pager::PageHeader::SIZE;
        let checksum_len = crate::pager::Page::CHECKSUM_SIZE;
        let total_page_size = pager.page_size().to_u32() as usize;
        let available_space = total_page_size - header_len - checksum_len;
        
        page_len <= available_space
    }

    /// Calculate the height of the tree.
    fn calculate_height(pager: &Pager<FS>, _page_id: PageId, node: &RTreeNode) -> TableResult<u32> {
        match node {
            RTreeNode::Leaf { .. } => Ok(1),
            RTreeNode::Internal { entries, level, .. } => {
                if entries.is_empty() {
                    Ok(*level + 1)
                } else {
                    // Recursively check first child
                    let child_node = Self::read_node(pager, entries[0].child_page_id)?;
                    let child_height =
                        Self::calculate_height(pager, entries[0].child_page_id, &child_node)?;
                    Ok(child_height + 1)
                }
            }
        }
    }

    /// Count the total number of visible (non-tombstoned) objects in the tree.
    fn count_objects(pager: &Pager<FS>, _page_id: PageId, node: &RTreeNode) -> TableResult<usize> {
        match node {
            RTreeNode::Leaf { entries, .. } => {
                // Count only entries where the latest version is not a tombstone
                let count = entries.iter().filter(|entry| {
                    // Check the latest version's value directly
                    match &entry.version_chain.value {
                        crate::txn::VersionValue::Inline(data) => {
                            // Check if it's not a tombstone (tombstone is [0xFF])
                            data.as_slice() != &[0xFF]
                        }
                        crate::txn::VersionValue::External(_) => {
                            // External values are never tombstones
                            true
                        }
                    }
                }).count();
                Ok(count)
            },
            RTreeNode::Internal { entries, .. } => {
                let mut count = 0;
                for entry in entries {
                    let child_node = Self::read_node(pager, entry.child_page_id)?;
                    count += Self::count_objects(pager, entry.child_page_id, &child_node)?;
                }
                Ok(count)
            }
        }
    }

    /// Insert a geometry into the tree.
    #[instrument(skip(self, geometry))]
    fn insert_internal(
        &self,
        id: &[u8],
        geometry: GeometryRef<'_>,
        tx_id: TransactionId,
    ) -> TableResult<()> {
        let mbr = self.geometry_to_mbr(geometry)?;
        let object_id = KeyBuf(id.to_vec());

        let root_page_id = self.root_page_id();
        let root_node = Self::read_node(&self.pager, root_page_id).map_err(|e| {
            TableError::Other(format!(
                "Failed to read root node {root_page_id} before insert of {:?}: {e}",
                id
            ))
        })?;

        // Check if an entry with this ID already exists
        if let Some((leaf_page_id, entry_index)) =
            self.find_leaf_entry(root_page_id, &root_node, id)?
        {
            // Entry exists - update its version chain and MBR
            let mut leaf_node = Self::read_node(&self.pager, leaf_page_id)?;

            if let Some(entries) = leaf_node.leaf_entries_mut() {
                if let Some(entry) = entries.get_mut(entry_index) {
                    // Update the MBR to the new geometry
                    entry.mbr = mbr;
                    // Prepend a new version to the chain
                    entry.prepend_version(tx_id);
                    Self::write_node(&self.pager, leaf_page_id, &leaf_node)?;
                    return Ok(());
                }
            }

            return Err(TableError::Other(format!(
                "Failed to update entry at index {} in leaf {}",
                entry_index, leaf_page_id
            )));
        }

        // Entry doesn't exist - create a new one
        let entry = LeafEntry::new(mbr, object_id, tx_id);

        // Find the appropriate leaf node
        let leaf_page_id = self.choose_leaf(root_page_id, &root_node, &entry.mbr)?;
        let mut leaf_node = Self::read_node(&self.pager, leaf_page_id).map_err(|e| {
            TableError::Other(format!(
                "Failed to read leaf node {leaf_page_id} before insert of {:?}: {e}",
                id
            ))
        })?;

        // Add entry to leaf
        leaf_node.add_leaf_entry(entry).map_err(TableError::Other)?;

        // Check if split is needed - either by entry count or by page size
        let needs_split = leaf_node.entry_count() > self.config.max_entries_per_node
            || !Self::would_fit_in_page(&self.pager, &leaf_node);
            
        if needs_split {
            self.split_node(leaf_page_id, leaf_node)?;
        } else {
            Self::write_node(&self.pager, leaf_page_id, &leaf_node)?;
        }

        // Increment object count
        *self.object_count.write().unwrap() += 1;

        Ok(())
    }

    /// Choose the best leaf node for inserting an entry.
    fn choose_leaf(&self, page_id: PageId, node: &RTreeNode, mbr: &Mbr) -> TableResult<PageId> {
        match node {
            RTreeNode::Leaf { .. } => Ok(page_id),
            RTreeNode::Internal { entries, .. } => {
                // Find entry with minimum area increase
                let mut best_idx = 0;
                let mut min_increase = f64::INFINITY;
                let mut min_area = f64::INFINITY;

                for (i, entry) in entries.iter().enumerate() {
                    let increase = entry.mbr.area_increase(mbr);
                    let area = entry.mbr.area();

                    if increase < min_increase || (increase == min_increase && area < min_area) {
                        min_increase = increase;
                        min_area = area;
                        best_idx = i;
                    }
                }

                let child_page_id = entries[best_idx].child_page_id;
                let child_node = Self::read_node(&self.pager, child_page_id).map_err(|e| {
                    TableError::Other(format!(
                        "Failed to read child node {child_page_id} during choose_leaf: {e}"
                    ))
                })?;
                self.choose_leaf(child_page_id, &child_node, mbr)
            }
        }
    }

    /// Split a node that has overflowed.
    fn split_node(&self, page_id: PageId, node: RTreeNode) -> TableResult<()> {
        match node {
            RTreeNode::Leaf {
                entries,
                parent_page_id,
                next_leaf,
            } => {
                let split_result =
                    split_leaf_entries(entries, self.config.split_strategy, self.config.dimensions);

                // Create new leaf node for right split
                let new_page_id = self.pager.allocate_page(PageType::RTreeNode)?;

                let mut left_node = RTreeNode::new_leaf();
                left_node.set_parent_page_id(parent_page_id);
                let mut right_node = RTreeNode::new_leaf();
                right_node.set_parent_page_id(parent_page_id);

                for entry in split_result.left {
                    left_node.add_leaf_entry(entry).map_err(TableError::Other)?;
                }
                for entry in split_result.right {
                    right_node
                        .add_leaf_entry(entry)
                        .map_err(TableError::Other)?;
                }

                // Update leaf chain
                if let RTreeNode::Leaf {
                    next_leaf: ref mut left_next,
                    ..
                } = left_node
                {
                    *left_next = new_page_id;
                }
                if let RTreeNode::Leaf {
                    next_leaf: ref mut right_next,
                    ..
                } = right_node
                {
                    *right_next = next_leaf;
                }

                // Write nodes
                Self::write_node(&self.pager, page_id, &left_node)?;
                Self::write_node(&self.pager, new_page_id, &right_node)?;

                // Update parent
                let left_mbr = left_node.calculate_mbr(self.config.dimensions);
                let right_mbr = right_node.calculate_mbr(self.config.dimensions);

                if parent_page_id == PageId::from(0) {
                    // Root split: move the original root contents to a new left child page,
                    // reuse the current page as the internal root, and write the right child separately.
                    let left_page_id = self.pager.allocate_page(PageType::RTreeNode)?;

                    if let RTreeNode::Leaf { parent_page_id, .. } = &mut left_node {
                        *parent_page_id = page_id;
                    }
                    if let RTreeNode::Leaf { parent_page_id, .. } = &mut right_node {
                        *parent_page_id = page_id;
                    }

                    let root_level = *self.height.read().unwrap();
                    let mut root_node = RTreeNode::new_internal(root_level);
                    root_node
                        .add_internal_entry(InternalEntry::new(left_mbr, left_page_id))
                        .map_err(TableError::Other)?;
                    root_node
                        .add_internal_entry(InternalEntry::new(right_mbr, new_page_id))
                        .map_err(TableError::Other)?;

                    Self::write_node(&self.pager, left_page_id, &left_node)?;
                    Self::write_node(&self.pager, new_page_id, &right_node)?;
                    Self::write_node(&self.pager, page_id, &root_node)?;
                    *self.height.write().unwrap() += 1;
                } else {
                    // Update existing parent
                    self.update_parent_after_split(
                        parent_page_id,
                        page_id,
                        left_mbr,
                        new_page_id,
                        right_mbr,
                    )?;
                }
            }
            RTreeNode::Internal {
                entries,
                parent_page_id,
                level,
            } => {
                let split_result = split_internal_entries(
                    entries,
                    self.config.split_strategy,
                    self.config.dimensions,
                );

                let new_page_id = self.pager.allocate_page(PageType::RTreeNode)?;

                let mut left_node = RTreeNode::new_internal(level);
                left_node.set_parent_page_id(parent_page_id);
                let mut right_node = RTreeNode::new_internal(level);
                right_node.set_parent_page_id(parent_page_id);

                for entry in split_result.left {
                    left_node
                        .add_internal_entry(entry)
                        .map_err(TableError::Other)?;
                }
                for entry in split_result.right {
                    right_node
                        .add_internal_entry(entry)
                        .map_err(TableError::Other)?;
                }

                Self::write_node(&self.pager, page_id, &left_node)?;
                Self::write_node(&self.pager, new_page_id, &right_node)?;
                self.update_children_parent_page_ids(page_id, &left_node)?;
                self.update_children_parent_page_ids(new_page_id, &right_node)?;

                let left_mbr = left_node.calculate_mbr(self.config.dimensions);
                let right_mbr = right_node.calculate_mbr(self.config.dimensions);

                if parent_page_id == PageId::from(0) {
                    let left_page_id = self.pager.allocate_page(PageType::RTreeNode)?;

                    if let RTreeNode::Internal { parent_page_id, .. } = &mut left_node {
                        *parent_page_id = page_id;
                    }
                    if let RTreeNode::Internal { parent_page_id, .. } = &mut right_node {
                        *parent_page_id = page_id;
                    }

                    let root_level = *self.height.read().unwrap();
                    let mut root_node = RTreeNode::new_internal(root_level);
                    root_node
                        .add_internal_entry(InternalEntry::new(left_mbr, left_page_id))
                        .map_err(TableError::Other)?;
                    root_node
                        .add_internal_entry(InternalEntry::new(right_mbr, new_page_id))
                        .map_err(TableError::Other)?;

                    Self::write_node(&self.pager, left_page_id, &left_node)?;
                    Self::write_node(&self.pager, new_page_id, &right_node)?;
                    Self::write_node(&self.pager, page_id, &root_node)?;
                    self.update_children_parent_page_ids(left_page_id, &left_node)?;
                    self.update_children_parent_page_ids(new_page_id, &right_node)?;
                    *self.height.write().unwrap() += 1;
                } else {
                    self.update_parent_after_split(
                        parent_page_id,
                        page_id,
                        left_mbr,
                        new_page_id,
                        right_mbr,
                    )?;
                }
            }
        }

        Ok(())
    }

    /// Update parent page IDs for all children of an internal node.
    fn update_children_parent_page_ids(
        &self,
        parent_page_id: PageId,
        node: &RTreeNode,
    ) -> TableResult<()> {
        if let Some(entries) = node.internal_entries() {
            for entry in entries {
                let mut child_node = Self::read_node(&self.pager, entry.child_page_id)?;
                child_node.set_parent_page_id(parent_page_id);
                Self::write_node(&self.pager, entry.child_page_id, &child_node)?;
            }
        }

        Ok(())
    }

    /// Find the leaf page containing the specified object ID.
    fn find_leaf_containing_id(
        &self,
        page_id: PageId,
        object_id: &[u8],
    ) -> TableResult<Option<PageId>> {
        let node = Self::read_node(&self.pager, page_id)?;
        match node {
            RTreeNode::Leaf { entries, .. } => Ok(entries
                .iter()
                .any(|entry| entry.object_id.as_ref() == object_id)
                .then_some(page_id)),
            RTreeNode::Internal { entries, .. } => {
                for entry in entries {
                    if let Some(found_page_id) =
                        self.find_leaf_containing_id(entry.child_page_id, object_id)?
                    {
                        return Ok(Some(found_page_id));
                    }
                }
                Ok(None)
            }
        }
    }

    /// Delete an object from the tree.
    fn delete_internal(&self, id: &[u8]) -> TableResult<()> {
        let root_page_id = self.root_page_id();
        let Some(leaf_page_id) = self.find_leaf_containing_id(root_page_id, id)? else {
            return Ok(());
        };

        let mut leaf_node = Self::read_node(&self.pager, leaf_page_id)?;
        let parent_page_id = leaf_node.parent_page_id();

        let removed = if let Some(entries) = leaf_node.leaf_entries_mut() {
            if let Some(index) = entries
                .iter()
                .position(|entry| entry.object_id.as_ref() == id)
            {
                entries.remove(index);
                true
            } else {
                false
            }
        } else {
            false
        };

        if !removed {
            return Ok(());
        }

        Self::write_node(&self.pager, leaf_page_id, &leaf_node)?;
        self.condense_tree(leaf_page_id, leaf_node, parent_page_id)?;

        let mut object_count = self.object_count.write().unwrap();
        if *object_count > 0 {
            *object_count -= 1;
        }

        Ok(())
    }

    /// Condense tree after deletion, handling underflow and root shrinking.
    fn condense_tree(
        &self,
        mut page_id: PageId,
        mut node: RTreeNode,
        mut parent_page_id: PageId,
    ) -> TableResult<()> {
        loop {
            let is_root = page_id == self.root_page_id();

            if is_root {
                self.adjust_root_after_delete(page_id, node)?;
                return Ok(());
            }

            let needs_underflow_handling = node.entry_count() < self.config.min_entries_per_node;
            if needs_underflow_handling {
                let orphaned_entries = self.collect_entries_for_reinsertion(&node);
                self.remove_child_from_parent(parent_page_id, page_id)?;

                let parent_node = Self::read_node(&self.pager, parent_page_id)?;
                page_id = parent_page_id;
                parent_page_id = parent_node.parent_page_id();
                node = parent_node;

                self.reinsert_entries(orphaned_entries)?;
                continue;
            }

            self.update_node_mbr_in_parent(parent_page_id, page_id, &node)?;
            let parent_node = Self::read_node(&self.pager, parent_page_id)?;
            page_id = parent_page_id;
            parent_page_id = parent_node.parent_page_id();
            node = parent_node;
        }
    }

    /// Remove a child reference from an internal parent node.
    fn remove_child_from_parent(
        &self,
        parent_page_id: PageId,
        child_page_id: PageId,
    ) -> TableResult<()> {
        let mut parent_node = Self::read_node(&self.pager, parent_page_id)?;
        if let Some(entries) = parent_node.internal_entries_mut()
            && let Some(index) = entries
                .iter()
                .position(|entry| entry.child_page_id == child_page_id)
        {
            entries.remove(index);
        }
        Self::write_node(&self.pager, parent_page_id, &parent_node)
    }

    /// Update a node's MBR entry inside its parent.
    fn update_node_mbr_in_parent(
        &self,
        parent_page_id: PageId,
        child_page_id: PageId,
        child_node: &RTreeNode,
    ) -> TableResult<()> {
        let mut parent_node = Self::read_node(&self.pager, parent_page_id)?;
        if let Some(entries) = parent_node.internal_entries_mut()
            && let Some(entry) = entries
                .iter_mut()
                .find(|entry| entry.child_page_id == child_page_id)
        {
            entry.mbr = child_node.calculate_mbr(self.config.dimensions);
        }
        Self::write_node(&self.pager, parent_page_id, &parent_node)
    }

    /// Adjust the root node after deletion, shrinking tree height when possible.
    fn adjust_root_after_delete(
        &self,
        root_page_id: PageId,
        root_node: RTreeNode,
    ) -> TableResult<()> {
        match root_node {
            RTreeNode::Internal { entries, .. } if entries.len() == 1 => {
                let child_page_id = entries[0].child_page_id;
                let mut child_node = Self::read_node(&self.pager, child_page_id)?;
                child_node.set_parent_page_id(PageId::from(0));
                Self::write_node(&self.pager, child_page_id, &child_node)?;
                *self.root_page_id.write().unwrap() = child_page_id;

                let mut height = self.height.write().unwrap();
                if *height > 1 {
                    *height -= 1;
                }
                Ok(())
            }
            RTreeNode::Internal { entries, level, .. } if entries.is_empty() => {
                let empty_root = RTreeNode::new_internal(level);
                Self::write_node(&self.pager, root_page_id, &empty_root)?;
                Ok(())
            }
            RTreeNode::Leaf { entries, .. } if entries.is_empty() => {
                let empty_root = RTreeNode::new_leaf();
                Self::write_node(&self.pager, root_page_id, &empty_root)?;
                *self.height.write().unwrap() = 1;
                Ok(())
            }
            other => {
                Self::write_node(&self.pager, root_page_id, &other)?;
                Ok(())
            }
        }
    }

    /// Reinsert orphaned entries without triggering recursive condense behavior.
    fn reinsert_entries(&self, entries: Vec<LeafEntry>) -> TableResult<()> {
        for entry in entries {
            let root_page_id = self.root_page_id();
            let root_node = Self::read_node(&self.pager, root_page_id)?;
            let leaf_page_id = self.choose_leaf(root_page_id, &root_node, &entry.mbr)?;
            let mut leaf_node = Self::read_node(&self.pager, leaf_page_id)?;

            leaf_node.add_leaf_entry(entry).map_err(TableError::Other)?;

            // Check if split is needed - either by entry count or by page size
            let needs_split = leaf_node.entry_count() > self.config.max_entries_per_node
                || !Self::would_fit_in_page(&self.pager, &leaf_node);
                
            if needs_split {
                self.split_node(leaf_page_id, leaf_node)?;
            } else {
                Self::write_node(&self.pager, leaf_page_id, &leaf_node)?;
            }
        }

        Ok(())
    }

    /// Collect leaf entries to reinsert after an underflowed subtree is removed.
    fn collect_entries_for_reinsertion(&self, node: &RTreeNode) -> Vec<LeafEntry> {
        match node {
            RTreeNode::Leaf { entries, .. } => entries.clone(),
            RTreeNode::Internal { entries, .. } => {
                let mut collected = Vec::new();
                for entry in entries {
                    if let Ok(child_node) = Self::read_node(&self.pager, entry.child_page_id) {
                        collected.extend(self.collect_entries_for_reinsertion(&child_node));
                    }
                }
                collected
            }
        }
    }

    /// Create a new root node after a split.
    fn create_new_root(
        &self,
        left_page_id: PageId,
        left_mbr: Mbr,
        right_page_id: PageId,
        right_mbr: Mbr,
    ) -> TableResult<()> {
        let new_root_page_id = self.pager.allocate_page(PageType::RTreeNode)?;

        let mut new_root = RTreeNode::new_internal(*self.height.read().unwrap());
        new_root
            .add_internal_entry(InternalEntry::new(left_mbr, left_page_id))
            .map_err(TableError::Other)?;
        new_root
            .add_internal_entry(InternalEntry::new(right_mbr, right_page_id))
            .map_err(TableError::Other)?;

        let mut left_node = Self::read_node(&self.pager, left_page_id)?;
        left_node.set_parent_page_id(new_root_page_id);
        Self::write_node(&self.pager, left_page_id, &left_node)?;

        let mut right_node = Self::read_node(&self.pager, right_page_id)?;
        right_node.set_parent_page_id(new_root_page_id);
        Self::write_node(&self.pager, right_page_id, &right_node)?;

        Self::write_node(&self.pager, new_root_page_id, &new_root).map_err(|e| {
            TableError::Other(format!(
                "Failed to write new root page {new_root_page_id}: {e}"
            ))
        })?;

        // Update root page ID and height
        *self.root_page_id.write().unwrap() = new_root_page_id;
        *self.height.write().unwrap() += 1;

        debug!(
            "Created new root at page {}, height now {}",
            new_root_page_id,
            *self.height.read().unwrap()
        );

        Ok(())
    }

    /// Update parent node after a child split.
    fn update_parent_after_split(
        &self,
        parent_page_id: PageId,
        old_child_id: PageId,
        old_mbr: Mbr,
        new_child_id: PageId,
        new_mbr: Mbr,
    ) -> TableResult<()> {
        let mut parent_node = Self::read_node(&self.pager, parent_page_id)?;

        // Find and update the old entry
        if let Some(entries) = parent_node.internal_entries_mut() {
            for entry in entries.iter_mut() {
                if entry.child_page_id == old_child_id {
                    entry.mbr = old_mbr;
                    break;
                }
            }
            // Add new entry
            entries.push(InternalEntry::new(new_mbr, new_child_id));
        }

        // Check if parent needs to split
        if parent_node.entry_count() > self.config.max_entries_per_node {
            self.split_node(parent_page_id, parent_node)?;
        } else {
            Self::write_node(&self.pager, parent_page_id, &parent_node)?;
        }

        Ok(())
    }

    /// Convert a geometry reference to an MBR.
    fn geometry_to_mbr(&self, geometry: GeometryRef<'_>) -> TableResult<Mbr> {
        Self::geometry_to_mbr_static(geometry)
    }

    /// Convert a geometry reference to an MBR (static version).
    fn geometry_to_mbr_static(geometry: GeometryRef<'_>) -> TableResult<Mbr> {
        match geometry {
            GeometryRef::Point(point) => Ok(Mbr::from_point_2d(point)),
            GeometryRef::BoundingBox { min, max } => Ok(Mbr::from_points_2d(min, max)),
            GeometryRef::Wkb(wkb) => Self::parse_wkb_mbr(wkb),
        }
    }

    /// Parse WKB format and extract MBR.
    fn parse_wkb_mbr(wkb: &[u8]) -> TableResult<Mbr> {
        if wkb.len() < 5 {
            return Err(TableError::operation_not_supported("WKB data too short"));
        }

        let byte_order = wkb[0];
        let little_endian = byte_order == 1;

        let read_u32 = |bytes: &[u8], offset: usize| -> u32 {
            let slice = &bytes[offset..offset + 4];
            let arr: [u8; 4] = slice.try_into().unwrap();
            if little_endian {
                u32::from_le_bytes(arr)
            } else {
                u32::from_be_bytes(arr)
            }
        };

        let read_f64 = |bytes: &[u8], offset: usize| -> f64 {
            let slice = &bytes[offset..offset + 8];
            let arr: [u8; 8] = slice.try_into().unwrap();
            if little_endian {
                f64::from_le_bytes(arr)
            } else {
                f64::from_be_bytes(arr)
            }
        };

        let geom_type = read_u32(wkb, 1);
        let base_type = geom_type & 0xFFFF;

        match base_type {
            1 => {
                if wkb.len() < 21 {
                    return Err(TableError::operation_not_supported(
                        "WKB Point data too short",
                    ));
                }
                let x = read_f64(wkb, 5);
                let y = read_f64(wkb, 13);
                Ok(Mbr::from_point_2d(GeoPoint { x, y }))
            }
            2 => {
                if wkb.len() < 9 {
                    return Err(TableError::operation_not_supported(
                        "WKB LineString data too short",
                    ));
                }
                let num_points = read_u32(wkb, 5) as usize;
                if wkb.len() < 9 + num_points * 16 {
                    return Err(TableError::operation_not_supported(
                        "WKB LineString data too short for claimed point count",
                    ));
                }
                let mut min_x = f64::INFINITY;
                let mut min_y = f64::INFINITY;
                let mut max_x = f64::NEG_INFINITY;
                let mut max_y = f64::NEG_INFINITY;
                for i in 0..num_points {
                    let offset = 9 + i * 16;
                    let x = read_f64(wkb, offset);
                    let y = read_f64(wkb, offset + 8);
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
                Ok(Mbr::from_points_2d(
                    GeoPoint { x: min_x, y: min_y },
                    GeoPoint { x: max_x, y: max_y },
                ))
            }
            3 => {
                if wkb.len() < 9 {
                    return Err(TableError::operation_not_supported(
                        "WKB Polygon data too short",
                    ));
                }
                let num_rings = read_u32(wkb, 5) as usize;
                let mut offset = 9;
                let mut min_x = f64::INFINITY;
                let mut min_y = f64::INFINITY;
                let mut max_x = f64::NEG_INFINITY;
                let mut max_y = f64::NEG_INFINITY;
                for _ in 0..num_rings {
                    if offset + 4 > wkb.len() {
                        return Err(TableError::operation_not_supported(
                            "WKB Polygon ring header out of bounds",
                        ));
                    }
                    let num_points = read_u32(wkb, offset) as usize;
                    offset += 4;
                    if offset + num_points * 16 > wkb.len() {
                        return Err(TableError::operation_not_supported(
                            "WKB Polygon ring data out of bounds",
                        ));
                    }
                    for _ in 0..num_points {
                        let x = read_f64(wkb, offset);
                        let y = read_f64(wkb, offset + 8);
                        min_x = min_x.min(x);
                        min_y = min_y.min(y);
                        max_x = max_x.max(x);
                        max_y = max_y.max(y);
                        offset += 16;
                    }
                }
                Ok(Mbr::from_points_2d(
                    GeoPoint { x: min_x, y: min_y },
                    GeoPoint { x: max_x, y: max_y },
                ))
            }
            4..=7 => Self::parse_wkb_multi_geometry(wkb, &read_u32, &read_f64),
            _ => Err(TableError::operation_not_supported(format!(
                "Unsupported WKB geometry type: {}",
                base_type
            ))),
        }
    }

    /// Parse multi-geometry WKB types.
    fn parse_wkb_multi_geometry(
        wkb: &[u8],
        read_u32: &impl Fn(&[u8], usize) -> u32,
        read_f64: &impl Fn(&[u8], usize) -> f64,
    ) -> TableResult<Mbr> {
        if wkb.len() < 9 {
            return Err(TableError::operation_not_supported(
                "WKB multi-geometry data too short",
            ));
        }

        let num_geometries = read_u32(wkb, 5) as usize;
        let mut offset = 9;
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;

        for _ in 0..num_geometries {
            if offset + 5 > wkb.len() {
                return Err(TableError::operation_not_supported(
                    "WKB multi-geometry header out of bounds",
                ));
            }

            let _byte_order = wkb[offset];
            let sub_type = read_u32(wkb, offset + 1) & 0xFFFF;
            offset += 5;

            match sub_type {
                1 => {
                    if offset + 16 > wkb.len() {
                        return Err(TableError::operation_not_supported(
                            "WKB MultiPoint geometry data out of bounds",
                        ));
                    }
                    let x = read_f64(wkb, offset);
                    let y = read_f64(wkb, offset + 8);
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                    offset += 16;
                }
                2 => {
                    if offset + 4 > wkb.len() {
                        return Err(TableError::operation_not_supported(
                            "WKB MultiLineString header out of bounds",
                        ));
                    }
                    let num_points = read_u32(wkb, offset) as usize;
                    offset += 4;
                    if offset + num_points * 16 > wkb.len() {
                        return Err(TableError::operation_not_supported(
                            "WKB MultiLineString data out of bounds",
                        ));
                    }
                    for _ in 0..num_points {
                        let x = read_f64(wkb, offset);
                        let y = read_f64(wkb, offset + 8);
                        min_x = min_x.min(x);
                        min_y = min_y.min(y);
                        max_x = max_x.max(x);
                        max_y = max_y.max(y);
                        offset += 16;
                    }
                }
                3 => {
                    if offset + 4 > wkb.len() {
                        return Err(TableError::operation_not_supported(
                            "WKB MultiPolygon header out of bounds",
                        ));
                    }
                    let num_rings = read_u32(wkb, offset) as usize;
                    offset += 4;
                    for _ in 0..num_rings {
                        if offset + 4 > wkb.len() {
                            return Err(TableError::operation_not_supported(
                                "WKB MultiPolygon ring header out of bounds",
                            ));
                        }
                        let num_points = read_u32(wkb, offset) as usize;
                        offset += 4;
                        if offset + num_points * 16 > wkb.len() {
                            return Err(TableError::operation_not_supported(
                                "WKB MultiPolygon ring data out of bounds",
                            ));
                        }
                        for _ in 0..num_points {
                            let x = read_f64(wkb, offset);
                            let y = read_f64(wkb, offset + 8);
                            min_x = min_x.min(x);
                            min_y = min_y.min(y);
                            max_x = max_x.max(x);
                            max_y = max_y.max(y);
                            offset += 16;
                        }
                    }
                }
                _ => {
                    return Err(TableError::operation_not_supported(format!(
                        "Unsupported geometry type {} in multi-geometry",
                        sub_type
                    )));
                }
            }
        }

        Ok(Mbr::from_points_2d(
            GeoPoint { x: min_x, y: min_y },
            GeoPoint { x: max_x, y: max_y },
        ))
    }

    /// Search for geometries that intersect with a query geometry.
    #[instrument(skip(self, query))]
    fn search_intersects(&self, query: GeometryRef<'_>, limit: usize) -> TableResult<Vec<GeoHit>> {
        let query_mbr = self.geometry_to_mbr(query)?;
        let mut results = Vec::new();

        let root_page_id = self.root_page_id();
        let root_node = Self::read_node(&self.pager, root_page_id)?;

        self.search_intersects_recursive(
            root_page_id,
            &root_node,
            &query_mbr,
            &mut results,
            limit,
            None,
        )?;

        Ok(results)
    }

    /// Recursive helper for intersection search.
    fn search_intersects_recursive(
        &self,
        _page_id: PageId,
        node: &RTreeNode,
        query_mbr: &Mbr,
        results: &mut Vec<GeoHit>,
        limit: usize,
        snapshot: Option<&Snapshot>,
    ) -> TableResult<()> {
        if results.len() >= limit {
            return Ok(());
        }

        match node {
            RTreeNode::Leaf { entries, .. } => {
                for entry in entries {
                    if entry.mbr.intersects(query_mbr) {
                        // Check visibility if snapshot is provided
                        if let Some(snap) = snapshot {
                            if !entry.is_visible(snap) {
                                continue;
                            }
                        } else {
                            // No snapshot: check if latest version is not a tombstone
                            match &entry.version_chain.value {
                                crate::txn::VersionValue::Inline(data) if data.as_slice() == &[0xFF] => {
                                    continue; // Skip tombstoned entries
                                }
                                _ => {} // Not a tombstone, continue processing
                            }
                        }
                        results.push(GeoHit {
                            id: entry.object_id.clone(),
                            distance: None,
                        });
                        if results.len() >= limit {
                            break;
                        }
                    }
                }
            }
            RTreeNode::Internal { entries, .. } => {
                for entry in entries {
                    if entry.mbr.intersects(query_mbr) {
                        let child_node = Self::read_node(&self.pager, entry.child_page_id)?;
                        self.search_intersects_recursive(
                            entry.child_page_id,
                            &child_node,
                            query_mbr,
                            results,
                            limit,
                            snapshot,
                        )?;
                        if results.len() >= limit {
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Search for the nearest geometries to a point.
    #[instrument(skip(self))]
    fn search_nearest(&self, point: GeoPoint, limit: usize) -> TableResult<Vec<GeoHit>> {
        let mut heap = BinaryHeap::new();
        let mut results: Vec<(f64, KeyBuf)> = Vec::new();

        let root_page_id = self.root_page_id();
        let root_node = Self::read_node(&self.pager, root_page_id)?;

        // Priority queue entry: (negative distance in microns, page_id, is_leaf)
        // Use microns (distance * 1_000_000) to avoid floating point comparison issues
        let root_dist = (root_node
            .calculate_mbr(self.config.dimensions)
            .min_distance(point) * 1_000_000.0) as i64;
        heap.push(std::cmp::Reverse((
            -root_dist,
            root_page_id,
            matches!(root_node, RTreeNode::Leaf { .. }),
        )));

        while let Some(std::cmp::Reverse((neg_dist, page_id, _is_leaf))) = heap.pop() {
            // Early termination: if we have enough results and the next node is farther
            // than our worst result, we can stop
            if results.len() >= limit {
                let worst_result_dist = (results.last().unwrap().0 * 1_000_000.0) as i64;
                if -neg_dist > worst_result_dist {
                    break;
                }
            }

            let node = Self::read_node(&self.pager, page_id)?;

            match node {
                RTreeNode::Leaf { entries, .. } => {
                    for entry in entries {
                        // Skip tombstoned entries
                        match &entry.version_chain.value {
                            crate::txn::VersionValue::Inline(data) if data.as_slice() == &[0xFF] => {
                                continue; // Skip tombstoned entries
                            }
                            _ => {} // Not a tombstone, continue processing
                        }
                        
                        let distance = entry.mbr.min_distance(point);
                        results.push((distance, entry.object_id.clone()));
                    }
                    // Sort and keep only top limit after processing each leaf
                    results.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                    results.truncate(limit);
                }
                RTreeNode::Internal { entries, .. } => {
                    for entry in entries {
                        let distance = entry.mbr.min_distance(point);
                        let dist_microns = (distance * 1_000_000.0) as i64;
                        heap.push(std::cmp::Reverse((
                            -dist_microns,
                            entry.child_page_id,
                            false,
                        )));
                    }
                }
            }
        }

        // Final sort by distance
        results.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);

        Ok(results
            .into_iter()
            .map(|(distance, id)| GeoHit {
                id,
                distance: Some(distance as f32),
            })
            .collect())
    }
    /// Insert a geometry with transaction tracking.
    pub fn insert_geometry_tx(
        &self,
        id: &[u8],
        geometry: GeometryRef<'_>,
        tx_id: TransactionId,
    ) -> TableResult<()> {
        self.insert_internal(id, geometry, tx_id)
    }

    /// Delete a geometry with transaction tracking.
    pub fn delete_geometry_tx(&self, id: &[u8], tx_id: TransactionId) -> TableResult<()> {
        // For delete, we mark the entry with a tombstone version
        // The actual deletion happens during vacuum
        let root_page_id = self.root_page_id();
        let root_node = Self::read_node(&self.pager, root_page_id)?;

        // Find the leaf containing this object
        if let Some((leaf_page_id, entry_index)) =
            self.find_leaf_entry(root_page_id, &root_node, id)?
        {
            let mut leaf_node = Self::read_node(&self.pager, leaf_page_id)?;

            if let Some(entries) = leaf_node.leaf_entries_mut() {
                if let Some(entry) = entries.get_mut(entry_index) {
                    // Prepend a tombstone version to mark as deleted
                    entry.prepend_tombstone(tx_id);
                    Self::write_node(&self.pager, leaf_page_id, &leaf_node)?;
                    return Ok(());
                }
            }
        }

        Err(TableError::key_not_found(format!(
            "Geometry {:?} not found",
            id
        )))
    }

    /// Search for geometries that intersect with the query, respecting snapshot visibility.
    pub fn search_intersects_snapshot(
        &self,
        query: GeometryRef<'_>,
        limit: usize,
        snapshot: &Snapshot,
    ) -> TableResult<Vec<GeoHit>> {
        let query_mbr = self.geometry_to_mbr(query)?;
        let mut results = Vec::new();

        let root_page_id = self.root_page_id();
        let root_node = Self::read_node(&self.pager, root_page_id)?;

        self.search_intersects_recursive(
            root_page_id,
            &root_node,
            &query_mbr,
            &mut results,
            limit,
            Some(snapshot),
        )?;

        Ok(results)
    }

    /// Search for nearest geometries to a point, respecting snapshot visibility.
    pub fn search_nearest_snapshot(
        &self,
        point: GeoPoint,
        limit: usize,
        snapshot: &Snapshot,
    ) -> TableResult<Vec<GeoHit>> {
        let mut heap = BinaryHeap::new();
        let mut results = Vec::new();

        let root_page_id = self.root_page_id();
        let root_node = Self::read_node(&self.pager, root_page_id)?;

        heap.push(std::cmp::Reverse((
            -root_node
                .calculate_mbr(self.config.dimensions)
                .min_distance(point) as i64,
            root_page_id,
            matches!(root_node, RTreeNode::Leaf { .. }),
        )));

        while let Some(std::cmp::Reverse((_neg_dist, page_id, _is_leaf))) = heap.pop() {
            if results.len() >= limit {
                break;
            }

            let node = Self::read_node(&self.pager, page_id)?;

            match node {
                RTreeNode::Leaf { entries, .. } => {
                    for entry in entries {
                        // Check visibility
                        if entry.is_visible(snapshot) {
                            let distance = entry.mbr.min_distance(point);
                            results.push((distance, entry.object_id.clone()));
                        }
                    }
                }
                RTreeNode::Internal { entries, .. } => {
                    for entry in entries {
                        let distance = entry.mbr.min_distance(point);
                        heap.push(std::cmp::Reverse((
                            -(distance as i64),
                            entry.child_page_id,
                            false,
                        )));
                    }
                }
            }
        }

        results.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        results.truncate(limit);

        Ok(results
            .into_iter()
            .map(|(distance, id)| GeoHit {
                id,
                distance: Some(distance as f32),
            })
            .collect())
    }

    /// Commit all versions created by the given transaction.
    pub fn commit_versions(
        &self,
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        let root_page_id = self.root_page_id();
        let root_node = Self::read_node(&self.pager, root_page_id)?;
        self.commit_versions_recursive(root_page_id, root_node, tx_id, commit_lsn)
    }

    /// Recursively commit versions in the tree.
    fn commit_versions_recursive(
        &self,
        page_id: PageId,
        mut node: RTreeNode,
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        match &mut node {
            RTreeNode::Leaf { entries, .. } => {
                let mut modified = false;
                for entry in entries.iter_mut() {
                    if entry.version_chain.created_by == tx_id
                        && entry.version_chain.commit_lsn.is_none()
                    {
                        entry.commit(commit_lsn);
                        modified = true;
                    }
                }
                if modified {
                    Self::write_node(&self.pager, page_id, &node)?;
                }
            }
            RTreeNode::Internal { entries, .. } => {
                for entry in entries {
                    let child_node = Self::read_node(&self.pager, entry.child_page_id)?;
                    self.commit_versions_recursive(
                        entry.child_page_id,
                        child_node,
                        tx_id,
                        commit_lsn,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Vacuum old versions that are no longer visible.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        let root_page_id = self.root_page_id();
        let root_node = Self::read_node(&self.pager, root_page_id)?;
        self.vacuum_recursive(root_page_id, root_node, min_visible_lsn)
    }

    /// Recursively vacuum old versions in the tree.
    fn vacuum_recursive(
        &self,
        page_id: PageId,
        mut node: RTreeNode,
        min_visible_lsn: LogSequenceNumber,
    ) -> TableResult<usize> {
        let mut total_removed = 0;

        match &mut node {
            RTreeNode::Leaf { entries, .. } => {
                let mut modified = false;
                for entry in entries.iter_mut() {
                    let (removed, freed_refs) = entry.vacuum(min_visible_lsn);

                    // Free overflow pages for removed external values
                    if !freed_refs.is_empty() {
                        self.pager.free_value_refs(&freed_refs)?;
                    }

                    if removed > 0 {
                        total_removed += removed;
                        modified = true;
                    }
                }
                if modified {
                    Self::write_node(&self.pager, page_id, &node)?;
                }
            }
            RTreeNode::Internal { entries, .. } => {
                for entry in entries {
                    let child_node = Self::read_node(&self.pager, entry.child_page_id)?;
                    total_removed +=
                        self.vacuum_recursive(entry.child_page_id, child_node, min_visible_lsn)?;
                }
            }
        }

        Ok(total_removed)
    }

    /// Find a leaf entry by object ID.
    fn find_leaf_entry(
        &self,
        page_id: PageId,
        node: &RTreeNode,
        id: &[u8],
    ) -> TableResult<Option<(PageId, usize)>> {
        match node {
            RTreeNode::Leaf { entries, .. } => {
                for (index, entry) in entries.iter().enumerate() {
                    if entry.object_id.as_ref() == id {
                        return Ok(Some((page_id, index)));
                    }
                }
                Ok(None)
            }
            RTreeNode::Internal { entries, .. } => {
                // Search all children (we don't have spatial info for the ID)
                for entry in entries {
                    let child_node = Self::read_node(&self.pager, entry.child_page_id)?;
                    if let Some(result) =
                        self.find_leaf_entry(entry.child_page_id, &child_node, id)?
                    {
                        return Ok(Some(result));
                    }
                }
                Ok(None)
            }
        }
    }
}

// Implement Table trait
impl<FS: FileSystem> Table for PagedRTree<FS> {
    fn table_id(&self) -> TableId {
        self.table_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> TableEngineKind {
        TableEngineKind::GeoSpatial
    }

    fn capabilities(&self) -> TableCapabilities {
        TableCapabilities {
            ordered: false,
            point_lookup: true,
            prefix_scan: false,
            reverse_scan: false,
            range_delete: false,
            merge_operator: false,
            mvcc_native: false,
            append_optimized: false,
            memory_resident: false,
            disk_resident: true,
            supports_compression: false,
            supports_encryption: false,
        }
    }

    fn stats(&self) -> TableResult<crate::table::TableStatistics> {
        let count = *self.object_count.read().unwrap() as u64;
        let height = *self.height.read().unwrap() as u64;

        // Estimate size based on tree structure
        // Each object: MBR (4 * 8 bytes for coordinates) + key overhead + value overhead
        let mbr_size = count * 32; // 4 coordinates * 8 bytes each
        let key_overhead = count * 32; // Approximate key size
        let value_overhead = count * 64; // Approximate geometry data

        // Internal nodes: estimate based on tree height and branching factor
        // Typical R-Tree has branching factor around 50-100
        let branching_factor = self.config.max_entries_per_node as u64;
        let internal_nodes = if height > 1 {
            (0..height - 1)
                .map(|level| {
                    let nodes_at_level =
                        count / branching_factor.pow((height - level - 1) as u32).max(1);
                    nodes_at_level.max(1) * 64 // Approximate size per internal node
                })
                .sum()
        } else {
            0
        };

        let estimated_size = mbr_size + key_overhead + value_overhead + internal_nodes + 4096; // +4KB for root

        Ok(crate::table::TableStatistics {
            row_count: Some(count),
            page_count: None, // TODO: Track actual page count
            total_size_bytes: Some(estimated_size),
            key_stats: Some(crate::table::KeyStatistics {
                min_size: 0,
                max_size: 0,
                avg_size: 0.0,
                distinct_count: Some(count),
            }),
            value_stats: Some(crate::table::ValueStatistics {
                min_size: 0,
                max_size: 0,
                avg_size: 0.0,
                null_count: None,
            }),
            histogram: None,
            last_updated_lsn: None,
        })
    }
}

impl<FS: FileSystem> PagedRTree<FS> {
    /// Verify a node and its children recursively.
    fn verify_node_recursive(
        &self,
        page_id: PageId,
        node: &RTreeNode,
        parent_mbr: Option<&Mbr>,
        report: &mut VerificationReport,
    ) -> TableResult<()> {
        // Calculate this node's MBR
        let node_mbr = node.calculate_mbr(self.config.dimensions);

        // Verify MBR is valid
        if !node_mbr.is_valid() {
            report.errors.push(crate::table::ConsistencyError {
                error_type: crate::table::ConsistencyErrorType::CorruptedIndex,
                location: format!("rtree_node_{}", page_id),
                description: "Node has invalid MBR (min > max)".to_string(),
                severity: crate::table::Severity::Error,
            });
        }

        // Verify parent containment
        if let Some(parent) = parent_mbr {
            if !parent.contains(&node_mbr) {
                report.errors.push(crate::table::ConsistencyError {
                    error_type: crate::table::ConsistencyErrorType::CorruptedIndex,
                    location: format!("rtree_node_{}", page_id),
                    description: "Node MBR not contained by parent MBR".to_string(),
                    severity: crate::table::Severity::Error,
                });
            }
        }

        match node {
            RTreeNode::Internal { entries, level, .. } => {
                if entries.is_empty() {
                    report.warnings.push(crate::table::ConsistencyWarning {
                        location: format!("rtree_node_{}", page_id),
                        description: "Internal node has no entries".to_string(),
                    });
                }

                if entries.len() > self.config.max_entries_per_node {
                    report.errors.push(crate::table::ConsistencyError {
                        error_type: crate::table::ConsistencyErrorType::CorruptedIndex,
                        location: format!("rtree_node_{}", page_id),
                        description: format!(
                            "Internal node has {} entries, exceeds max {}",
                            entries.len(),
                            self.config.max_entries_per_node
                        ),
                        severity: crate::table::Severity::Error,
                    });
                }

                for (idx, entry) in entries.iter().enumerate() {
                    if !entry.mbr.is_valid() {
                        report.errors.push(crate::table::ConsistencyError {
                            error_type: crate::table::ConsistencyErrorType::CorruptedIndex,
                            location: format!("rtree_node_{}_entry_{}", page_id, idx),
                            description: "Entry has invalid MBR".to_string(),
                            severity: crate::table::Severity::Error,
                        });
                    }

                    if entry.mbr.dimensions != self.config.dimensions {
                        report.errors.push(crate::table::ConsistencyError {
                            error_type: crate::table::ConsistencyErrorType::CorruptedIndex,
                            location: format!("rtree_node_{}_entry_{}", page_id, idx),
                            description: format!(
                                "Entry MBR dimension mismatch: expected {}, got {}",
                                self.config.dimensions, entry.mbr.dimensions
                            ),
                            severity: crate::table::Severity::Error,
                        });
                    }

                    match Self::read_node(&self.pager, entry.child_page_id) {
                        Ok(child_node) => {
                            if child_node.level() != level - 1 {
                                report.errors.push(crate::table::ConsistencyError {
                                    error_type: crate::table::ConsistencyErrorType::CorruptedIndex,
                                    location: format!("rtree_node_{}", entry.child_page_id),
                                    description: format!(
                                        "Child node level {} doesn't match expected {}",
                                        child_node.level(),
                                        level - 1
                                    ),
                                    severity: crate::table::Severity::Error,
                                });
                            }

                            self.verify_node_recursive(
                                entry.child_page_id,
                                &child_node,
                                Some(&entry.mbr),
                                report,
                            )?;
                        }
                        Err(e) => {
                            report.errors.push(crate::table::ConsistencyError {
                                error_type: crate::table::ConsistencyErrorType::InvalidPointer,
                                location: format!("rtree_node_{}_entry_{}", page_id, idx),
                                description: format!(
                                    "Failed to read child node {}: {}",
                                    entry.child_page_id, e
                                ),
                                severity: crate::table::Severity::Critical,
                            });
                        }
                    }
                }
            }
            RTreeNode::Leaf { entries, .. } => {
                if entries.len() > self.config.max_entries_per_node {
                    report.errors.push(crate::table::ConsistencyError {
                        error_type: crate::table::ConsistencyErrorType::CorruptedIndex,
                        location: format!("rtree_node_{}", page_id),
                        description: format!(
                            "Leaf node has {} entries, exceeds max {}",
                            entries.len(),
                            self.config.max_entries_per_node
                        ),
                        severity: crate::table::Severity::Error,
                    });
                }

                for (idx, entry) in entries.iter().enumerate() {
                    report.checked_items += 1;

                    if !entry.mbr.is_valid() {
                        report.errors.push(crate::table::ConsistencyError {
                            error_type: crate::table::ConsistencyErrorType::CorruptedIndex,
                            location: format!("rtree_node_{}_entry_{}", page_id, idx),
                            description: "Leaf entry has invalid MBR".to_string(),
                            severity: crate::table::Severity::Error,
                        });
                    }

                    if entry.mbr.dimensions != self.config.dimensions {
                        report.errors.push(crate::table::ConsistencyError {
                            error_type: crate::table::ConsistencyErrorType::CorruptedIndex,
                            location: format!("rtree_node_{}_entry_{}", page_id, idx),
                            description: format!(
                                "Leaf entry MBR dimension mismatch: expected {}, got {}",
                                self.config.dimensions, entry.mbr.dimensions
                            ),
                            severity: crate::table::Severity::Error,
                        });
                    }
                }
            }
        }

        Ok(())
    }
}

// Implement GeoSpatial trait
impl<FS: FileSystem> GeoSpatial for PagedRTree<FS> {
    fn table_id(&self) -> TableId {
        self.table_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> SpecialtyTableCapabilities {
        SpecialtyTableCapabilities {
            exact: true,
            approximate: false,
            ordered: false,
            sparse: false,
            supports_delete: true,
            supports_range_query: true,
            supports_prefix_query: false,
            supports_scoring: true,
            supports_incremental_rebuild: false,
            may_be_stale: false,
        }
    }

    fn insert_geometry(
        &self,
        id: &[u8],
        geometry: GeometryRef<'_>,
        tx_id: TransactionId,
        _commit_lsn: crate::wal::LogSequenceNumber,
    ) -> TableResult<()> {
        self.insert_internal(id, geometry, tx_id)
    }

    fn delete_geometry(
        &self,
        id: &[u8],
        tx_id: TransactionId,
        _commit_lsn: crate::wal::LogSequenceNumber,
    ) -> TableResult<()> {
        // Use MVCC tombstone deletion instead of physical removal
        self.delete_geometry_tx(id, tx_id)
    }

    fn intersects(&self, query: GeometryRef<'_>, limit: usize) -> TableResult<Vec<GeoHit>> {
        self.search_intersects(query, limit)
    }

    fn nearest(&self, point: GeoPoint, limit: usize) -> TableResult<Vec<GeoHit>> {
        self.search_nearest(point, limit)
    }

    fn stats(&self) -> TableResult<SpecialtyTableStats> {
        // Count actual visible entries by traversing the tree
        let root_page_id = self.root_page_id();
        let root_node = Self::read_node(&self.pager, root_page_id)?;
        let count = Self::count_objects(&self.pager, root_page_id, &root_node)? as u64;
        let height = *self.height.read().unwrap() as u64;

        // Estimate size based on tree structure
        // Each object: MBR (4 * 8 bytes for coordinates) + key overhead + value overhead
        let mbr_size = count * 32; // 4 coordinates * 8 bytes each
        let key_overhead = count * 32; // Approximate key size
        let value_overhead = count * 64; // Approximate geometry data

        // Internal nodes: estimate based on tree height and branching factor
        // Typical R-Tree has branching factor around 50-100
        let branching_factor = self.config.max_entries_per_node as u64;
        let internal_nodes = if height > 1 {
            (0..height - 1)
                .map(|level| {
                    let nodes_at_level =
                        count / branching_factor.pow((height - level - 1) as u32).max(1);
                    nodes_at_level.max(1) * 64 // Approximate size per internal node
                })
                .sum()
        } else {
            0
        };

        let estimated_size = mbr_size + key_overhead + value_overhead + internal_nodes + 4096; // +4KB for root

        Ok(SpecialtyTableStats {
            entry_count: Some(count),
            size_bytes: Some(estimated_size),
            distinct_keys: Some(count),
            stale_entries: None,
            last_updated_lsn: None,
        })
    }

    fn verify(&self) -> TableResult<VerificationReport> {
        let mut report = VerificationReport {
            checked_items: 0,
            errors: Vec::new(),
            warnings: Vec::new(),
        };

        let root_page_id = self.root_page_id();

        // Verify the tree structure recursively starting from root
        match Self::read_node(&self.pager, root_page_id) {
            Ok(root_node) => {
                self.verify_node_recursive(root_page_id, &root_node, None, &mut report)?;
            }
            Err(e) => {
                report.errors.push(crate::table::ConsistencyError {
                    error_type: crate::table::ConsistencyErrorType::CorruptedPage,
                    location: format!("rtree_root_{}", root_page_id),
                    description: format!("Failed to read root node: {}", e),
                    severity: crate::table::Severity::Critical,
                });
            }
        }

        // Verify object count matches actual count
        let actual_count = *self.object_count.read().unwrap();
        if report.checked_items != actual_count as u64 {
            report.warnings.push(crate::table::ConsistencyWarning {
                location: "rtree_metadata".to_string(),
                description: format!(
                    "Object count mismatch: metadata says {}, found {} entries",
                    actual_count, report.checked_items
                ),
            });
        }

        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_wkb_point(x: f64, y: f64) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(1);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&x.to_le_bytes());
        bytes.extend_from_slice(&y.to_le_bytes());
        bytes
    }

    fn create_wkb_line_string(points: &[(f64, f64)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(1);
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&(points.len() as u32).to_le_bytes());
        for (x, y) in points {
            bytes.extend_from_slice(&x.to_le_bytes());
            bytes.extend_from_slice(&y.to_le_bytes());
        }
        bytes
    }

    fn create_wkb_polygon(rings: &[Vec<(f64, f64)>]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(1);
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&(rings.len() as u32).to_le_bytes());
        for ring in rings {
            bytes.extend_from_slice(&(ring.len() as u32).to_le_bytes());
            for (x, y) in ring {
                bytes.extend_from_slice(&x.to_le_bytes());
                bytes.extend_from_slice(&y.to_le_bytes());
            }
        }
        bytes
    }

    #[test]
    fn test_parse_wkb_point_little_endian() {
        let wkb = create_wkb_point(1.0f64, 2.0f64);
        let mbr = PagedRTree::<crate::vfs::MemoryFileSystem>::parse_wkb_mbr(&wkb).unwrap();
        assert!((mbr.min[0] - 1.0).abs() < 1e-10);
        assert!((mbr.min[1] - 2.0).abs() < 1e-10);
        assert!((mbr.max[0] - 1.0).abs() < 1e-10);
        assert!((mbr.max[1] - 2.0).abs() < 1e-10);
    }

    #[test]
    fn test_parse_wkb_point_big_endian() {
        let mut bytes = Vec::new();
        bytes.push(0);
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1.0f64.to_be_bytes());
        bytes.extend_from_slice(&2.0f64.to_be_bytes());
        let mbr = PagedRTree::<crate::vfs::MemoryFileSystem>::parse_wkb_mbr(&bytes).unwrap();
        assert!((mbr.min[0] - 1.0).abs() < 1e-10);
        assert!((mbr.min[1] - 2.0).abs() < 1e-10);
    }

    #[test]
    fn test_parse_wkb_line_string() {
        let points = vec![(0.0f64, 0.0f64), (1.0f64, 1.0f64), (2.0f64, 0.0f64)];
        let wkb = create_wkb_line_string(&points);
        let mbr = PagedRTree::<crate::vfs::MemoryFileSystem>::parse_wkb_mbr(&wkb).unwrap();
        assert!((mbr.min[0] - 0.0).abs() < 1e-10);
        assert!((mbr.min[1] - 0.0).abs() < 1e-10);
        assert!((mbr.max[0] - 2.0).abs() < 1e-10);
        assert!((mbr.max[1] - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_parse_wkb_polygon() {
        let outer_ring = vec![
            (0.0f64, 0.0f64),
            (4.0f64, 0.0f64),
            (4.0f64, 4.0f64),
            (0.0f64, 4.0f64),
            (0.0f64, 0.0f64),
        ];
        let wkb = create_wkb_polygon(&[outer_ring]);
        let mbr = PagedRTree::<crate::vfs::MemoryFileSystem>::parse_wkb_mbr(&wkb).unwrap();
        assert!((mbr.min[0] - 0.0).abs() < 1e-10);
        assert!((mbr.min[1] - 0.0).abs() < 1e-10);
        assert!((mbr.max[0] - 4.0).abs() < 1e-10);
        assert!((mbr.max[1] - 4.0).abs() < 1e-10);
    }

    #[test]
    fn test_parse_wkb_polygon_with_hole() {
        let outer_ring = vec![
            (0.0f64, 0.0f64),
            (10.0f64, 0.0f64),
            (10.0f64, 10.0f64),
            (0.0f64, 10.0f64),
            (0.0f64, 0.0f64),
        ];
        let hole_ring = vec![
            (2.0f64, 2.0f64),
            (8.0f64, 2.0f64),
            (8.0f64, 8.0f64),
            (2.0f64, 8.0f64),
            (2.0f64, 2.0f64),
        ];
        let wkb = create_wkb_polygon(&[outer_ring, hole_ring]);
        let mbr = PagedRTree::<crate::vfs::MemoryFileSystem>::parse_wkb_mbr(&wkb).unwrap();
        assert!((mbr.min[0] - 0.0).abs() < 1e-10);
        assert!((mbr.min[1] - 0.0).abs() < 1e-10);
        assert!((mbr.max[0] - 10.0).abs() < 1e-10);
        assert!((mbr.max[1] - 10.0).abs() < 1e-10);
    }

    #[test]
    fn test_parse_wkb_multipoint() {
        let mut bytes = Vec::new();
        bytes.push(1);
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());

        for (x, y) in [(1.0f64, 2.0f64), (3.0f64, 4.0f64)] {
            bytes.push(1);
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&x.to_le_bytes());
            bytes.extend_from_slice(&y.to_le_bytes());
        }

        let mbr = PagedRTree::<crate::vfs::MemoryFileSystem>::parse_wkb_mbr(&bytes).unwrap();
        assert!((mbr.min[0] - 1.0).abs() < 1e-10);
        assert!((mbr.min[1] - 2.0).abs() < 1e-10);
        assert!((mbr.max[0] - 3.0).abs() < 1e-10);
        assert!((mbr.max[1] - 4.0).abs() < 1e-10);
    }

    #[test]
    fn test_parse_wkb_invalid_type() {
        let mut bytes = vec![1; 5];
        bytes[1..5].copy_from_slice(&99u32.to_le_bytes());
        let result = PagedRTree::<crate::vfs::MemoryFileSystem>::parse_wkb_mbr(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_wkb_too_short() {
        let bytes = vec![1, 2, 3];
        let result = PagedRTree::<crate::vfs::MemoryFileSystem>::parse_wkb_mbr(&bytes);
        assert!(result.is_err());
    }
}

// Made with Bob
