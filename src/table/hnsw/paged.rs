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

//! Paged HNSW vector search implementation.
//!
//! This implementation stores the HNSW graph structure across multiple pages,
//! allowing it to scale beyond available memory. The graph is organized as:
//!
//! - Root page: Contains metadata and entry point information
//! - Node pages: Store vector data and neighbor lists for each layer
//! - Index pages: Map vector IDs to node page locations
//!
//! The HNSW algorithm maintains a hierarchical graph where each node exists
//! in one or more layers, with connections to M neighbors per layer.

use crate::pager::{Page, PageId, PageType, Pager};
use crate::snap::Snapshot;
use crate::table::{
    HnswVector, SpecialtyTableCapabilities, SpecialtyTableStats, Table, TableEngineKind,
    TableError, TableResult, VectorHit, VectorMetric, VectorSearch, VectorSearchOptions,
    VerificationReport,
};
use crate::txn::{TransactionId, VersionChain};
use crate::types::{KeyBuf, TableId};
use crate::vfs::FileSystem;
use crate::wal::LogSequenceNumber;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::{Arc, RwLock};

/// Paged HNSW vector search table.
///
/// Implements the HNSW algorithm for approximate nearest neighbor search
/// with persistent storage across multiple pages.
pub struct PagedHnswVector<FS: FileSystem> {
    /// Table identifier
    table_id: TableId,

    /// Table name
    name: String,

    /// Pager for page management
    pager: Arc<Pager<FS>>,

    /// Root page containing metadata
    root_page_id: PageId,

    /// Configuration
    config: RwLock<HnswConfig>,

    /// Current entry point (top-level node)
    entry_point: RwLock<Option<NodeId>>,

    /// Maximum layer currently in use
    max_layer: RwLock<usize>,

    /// Number of vectors inserted
    num_vectors: RwLock<usize>,

    /// Map from vector ID to node ID
    id_to_node: RwLock<HashMap<KeyBuf, NodeId>>,

    /// Random number generator state for layer selection
    rng_state: RwLock<u64>,
}

/// HNSW configuration parameters.
#[derive(Clone, Debug)]
pub struct HnswConfig {
    /// Number of dimensions in vectors
    pub dimensions: usize,

    /// Distance metric to use
    pub metric: VectorMetric,

    /// Maximum number of bidirectional connections per node per layer (M)
    pub max_connections: usize,

    /// Maximum connections for layer 0 (typically 2*M)
    pub max_connections_layer0: usize,

    /// Size of dynamic candidate list during construction (ef_construction)
    pub ef_construction: usize,

    /// Multiplier for layer selection probability
    pub ml: f64,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            dimensions: 128,
            metric: VectorMetric::Cosine,
            max_connections: 16,
            max_connections_layer0: 32,
            ef_construction: 200,
            ml: 1.0 / (16.0_f64).ln(),
        }
    }
}

/// Internal node identifier (page-based)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct NodeId(u32);

impl NodeId {
    fn as_u32(&self) -> u32 {
        self.0
    }
}

impl From<u32> for NodeId {
    fn from(v: u32) -> Self {
        NodeId(v)
    }
}

/// Node data stored in pages
#[derive(Clone, Debug)]
struct HnswNode {
    /// Vector ID (user-provided key)
    id: KeyBuf,

    /// Vector data
    vector: Vec<f32>,

    /// Layer this node exists in (0 = bottom layer, always present)
    layer: usize,

    /// Neighbors at each layer (layer -> list of neighbor node IDs)
    neighbors: Vec<Vec<NodeId>>,

    /// Version chain for MVCC support
    version_chain: VersionChain,
}

impl HnswNode {
    /// Create a new HNSW node with a version chain.
    fn new(id: KeyBuf, vector: Vec<f32>, layer: usize, tx_id: TransactionId) -> Self {
        // Create a version chain with empty value (vector is stored separately)
        let version_chain = VersionChain::new(Vec::new(), tx_id);
        Self {
            id,
            vector,
            layer,
            neighbors: vec![Vec::new(); layer + 1],
            version_chain,
        }
    }

    /// Check if this node is visible to the given snapshot.
    /// Returns false if the visible version is a tombstone.
    fn is_visible(&self, snapshot: &Snapshot) -> bool {
        match self.version_chain.find_visible_version(snapshot) {
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

    /// Commit this node's version at the given LSN.
    fn commit(&mut self, lsn: LogSequenceNumber) {
        self.version_chain.commit(lsn);
    }

    /// Prepend a new version to this node's chain.
    /// For deletions, use prepend_tombstone instead.
    fn prepend_version(&mut self, tx_id: TransactionId) {
        let old_chain = std::mem::replace(
            &mut self.version_chain,
            VersionChain::new(Vec::new(), tx_id),
        );
        self.version_chain = old_chain.prepend(Vec::new(), tx_id);
    }

    /// Prepend a tombstone version to mark this node as deleted.
    fn prepend_tombstone(&mut self, tx_id: TransactionId) {
        let old_chain = std::mem::replace(
            &mut self.version_chain,
            VersionChain::new(Self::tombstone_marker(), tx_id),
        );
        self.version_chain = old_chain.prepend(Self::tombstone_marker(), tx_id);
    }

    /// Vacuum old versions from this node's chain.
    fn vacuum(&mut self, min_visible_lsn: LogSequenceNumber) -> usize {
        self.version_chain.vacuum(min_visible_lsn)
    }
}

/// Metadata stored in the root page
#[repr(C)]
struct HnswMetadata {
    /// Magic number for validation
    magic: u32,

    /// Version number
    version: u32,

    /// Number of dimensions
    dimensions: u32,

    /// Distance metric (0=Cosine, 1=Dot, 2=Euclidean, 3=Manhattan)
    metric: u32,

    /// Maximum connections per layer
    max_connections: u32,

    /// Maximum connections for layer 0
    max_connections_layer0: u32,

    /// ef_construction parameter
    ef_construction: u32,

    /// ml parameter (stored as f64)
    ml: f64,

    /// Entry point node ID (0 if none)
    entry_point: u32,

    /// Maximum layer in use
    max_layer: u32,

    /// Number of vectors
    num_vectors: u64,

    /// Reserved for future use
    _reserved: [u8; 64],
}

const HNSW_MAGIC: u32 = 0x484E5357; // "HNSW"
const HNSW_VERSION: u32 = 1;

/// Candidate for priority queue during search
#[derive(Clone, Debug)]
struct Candidate {
    node_id: NodeId,
    distance: f32,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.distance == other.distance
    }
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse ordering for min-heap behavior
        other
            .distance
            .partial_cmp(&self.distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

impl<FS: FileSystem> PagedHnswVector<FS> {
    /// Create a new paged HNSW vector search table.
    ///
    /// # Arguments
    ///
    /// * `table_id` - Unique identifier for this table
    /// * `name` - Human-readable name
    /// * `pager` - Pager for page management
    /// * `config` - HNSW configuration parameters
    pub fn new(
        table_id: TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        config: HnswConfig,
    ) -> TableResult<Self> {
        // Allocate root page
        let root_page_id = pager
            .allocate_page(PageType::VectorIndex)
            .map_err(|e| TableError::Other(format!("Failed to allocate root page: {}", e)))?;

        // Initialize metadata
        let metadata = HnswMetadata {
            magic: HNSW_MAGIC,
            version: HNSW_VERSION,
            dimensions: config.dimensions as u32,
            metric: match config.metric {
                VectorMetric::Cosine => 0,
                VectorMetric::Dot => 1,
                VectorMetric::Euclidean => 2,
                VectorMetric::Manhattan => 3,
            },
            max_connections: config.max_connections as u32,
            max_connections_layer0: config.max_connections_layer0 as u32,
            ef_construction: config.ef_construction as u32,
            ml: config.ml,
            entry_point: 0,
            max_layer: 0,
            num_vectors: 0,
            _reserved: [0; 64],
        };

        // Write metadata to root page
        Self::write_metadata(&pager, root_page_id, &metadata)?;

        Ok(Self {
            table_id,
            name,
            pager,
            root_page_id,
            config: RwLock::new(config),
            entry_point: RwLock::new(None),
            max_layer: RwLock::new(0),
            num_vectors: RwLock::new(0),
            id_to_node: RwLock::new(HashMap::new()),
            rng_state: RwLock::new(12345), // Simple seed
        })
    }

    /// Load an existing paged HNSW vector search table.
    pub fn load(
        table_id: TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        root_page_id: PageId,
    ) -> TableResult<Self> {
        // Read metadata from root page
        let metadata = Self::read_metadata(&pager, root_page_id)?;

        // Validate magic number
        if metadata.magic != HNSW_MAGIC {
            return Err(TableError::corruption(
                "HNSW metadata",
                "magic number mismatch",
                format!(
                    "expected 0x{:08X}, got 0x{:08X}",
                    HNSW_MAGIC, metadata.magic
                ),
            ));
        }

        // Validate version
        if metadata.version != HNSW_VERSION {
            return Err(TableError::InvalidFormatVersion(metadata.version));
        }

        let config = HnswConfig {
            dimensions: metadata.dimensions as usize,
            metric: match metadata.metric {
                0 => VectorMetric::Cosine,
                1 => VectorMetric::Dot,
                2 => VectorMetric::Euclidean,
                3 => VectorMetric::Manhattan,
                _ => {
                    return Err(TableError::corruption(
                        "HNSW metadata",
                        "invalid metric",
                        format!("metric value: {}", metadata.metric),
                    ));
                }
            },
            max_connections: metadata.max_connections as usize,
            max_connections_layer0: metadata.max_connections_layer0 as usize,
            ef_construction: metadata.ef_construction as usize,
            ml: metadata.ml,
        };

        let entry_point = if metadata.entry_point == 0 {
            None
        } else {
            Some(NodeId(metadata.entry_point))
        };

        // TODO: Load id_to_node mapping from pages
        let id_to_node = HashMap::new();

        Ok(Self {
            table_id,
            name,
            pager,
            root_page_id,
            config: RwLock::new(config),
            entry_point: RwLock::new(entry_point),
            max_layer: RwLock::new(metadata.max_layer as usize),
            num_vectors: RwLock::new(metadata.num_vectors as usize),
            id_to_node: RwLock::new(id_to_node),
            rng_state: RwLock::new(12345),
        })
    }

    /// Write metadata to root page
    fn write_metadata(
        pager: &Arc<Pager<FS>>,
        root_page_id: PageId,
        metadata: &HnswMetadata,
    ) -> TableResult<()> {
        // Write metadata as bytes
        let metadata_bytes = unsafe {
            std::slice::from_raw_parts(
                metadata as *const HnswMetadata as *const u8,
                std::mem::size_of::<HnswMetadata>(),
            )
        };

        // Create a new page with metadata
        let page_size = pager.page_size().to_u32() as usize;
        let mut page = Page::new(root_page_id, PageType::VectorIndex, page_size);

        // Initialize page data with metadata
        page.data_mut().extend_from_slice(metadata_bytes);

        pager
            .write_page(&page)
            .map_err(|e| TableError::Other(format!("Failed to write root page: {}", e)))?;

        Ok(())
    }

    /// Read metadata from root page
    fn read_metadata(pager: &Arc<Pager<FS>>, root_page_id: PageId) -> TableResult<HnswMetadata> {
        let page = pager
            .read_page(root_page_id)
            .map_err(|e| TableError::Other(format!("Failed to read root page: {}", e)))?;

        // Read metadata from bytes
        let metadata_bytes = &page.data()[..std::mem::size_of::<HnswMetadata>()];
        let metadata = unsafe { std::ptr::read(metadata_bytes.as_ptr() as *const HnswMetadata) };

        Ok(metadata)
    }

    /// Calculate distance between two vectors
    fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        let config = self.config.read().unwrap();
        match config.metric {
            VectorMetric::Cosine => {
                // Cosine distance = 1 - cosine_similarity
                let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
                let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
                let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
                1.0 - (dot / (norm_a * norm_b))
            }
            VectorMetric::Dot => {
                // Negative dot product (lower is better)
                -a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>()
            }
            VectorMetric::Euclidean => {
                // Euclidean distance
                a.iter()
                    .zip(b.iter())
                    .map(|(x, y)| (x - y) * (x - y))
                    .sum::<f32>()
                    .sqrt()
            }
            VectorMetric::Manhattan => {
                // Manhattan distance
                a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
            }
        }
    }

    /// Select a random layer for a new node
    fn select_layer(&self) -> usize {
        let mut rng_state = self.rng_state.write().unwrap();

        // Simple LCG random number generator
        *rng_state = rng_state.wrapping_mul(1664525).wrapping_add(1013904223);
        let uniform = (*rng_state as f64) / (u64::MAX as f64);

        // Use exponential distribution for layer selection

        (-uniform.ln() * self.config.read().unwrap().ml).floor() as usize
    }

    /// Search for nearest neighbors at a specific layer
    fn search_layer(
        &self,
        query: &[f32],
        entry_points: Vec<NodeId>,
        ef: usize,
        layer: usize,
    ) -> TableResult<Vec<Candidate>> {
        let mut visited = HashSet::new();
        let mut candidates = BinaryHeap::new();
        let mut results = BinaryHeap::new();

        // Initialize with entry points
        for ep in entry_points {
            if visited.insert(ep) {
                let node = self.load_node(ep)?;
                let dist = self.distance(query, &node.vector);
                let candidate = Candidate {
                    node_id: ep,
                    distance: dist,
                };
                candidates.push(candidate.clone());
                results.push(candidate);
            }
        }

        // Greedy search
        while let Some(current) = candidates.pop() {
            // Check if we should continue
            if let Some(furthest) = results.peek()
                && current.distance > furthest.distance
            {
                break;
            }

            // Get neighbors at this layer
            let node = self.load_node(current.node_id)?;
            if layer < node.neighbors.len() {
                for &neighbor_id in &node.neighbors[layer] {
                    if visited.insert(neighbor_id) {
                        let neighbor = self.load_node(neighbor_id)?;
                        let dist = self.distance(query, &neighbor.vector);
                        let candidate = Candidate {
                            node_id: neighbor_id,
                            distance: dist,
                        };

                        if results.len() < ef || dist < results.peek().unwrap().distance {
                            candidates.push(candidate.clone());
                            results.push(candidate);

                            // Prune results if needed
                            if results.len() > ef {
                                results.pop();
                            }
                        }
                    }
                }
            }
        }

        // Convert to sorted vector
        let mut result_vec: Vec<_> = results.into_iter().collect();
        result_vec.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(result_vec)
    }

    /// Load a node from storage
    fn load_node(&self, node_id: NodeId) -> TableResult<HnswNode> {
        let page_id = PageId::from(node_id.0 as u64);

        let page = self
            .pager
            .read_page(page_id)
            .map_err(|e| TableError::Other(format!("Failed to read node page: {}", e)))?;

        Self::deserialize_node(page.data())
    }

    /// Store a node to storage
    fn store_node(&self, node: &HnswNode) -> TableResult<NodeId> {
        let page_id = self
            .pager
            .allocate_page(PageType::VectorIndex)
            .map_err(|e| TableError::Other(format!("Failed to allocate node page: {}", e)))?;

        let data = Self::serialize_node(node)?;

        let page_size = self.pager.page_size().to_u32() as usize;
        let mut page = Page::new(page_id, PageType::VectorIndex, page_size);
        page.data_mut().extend_from_slice(&data);

        self.pager
            .write_page(&page)
            .map_err(|e| TableError::Other(format!("Failed to write node page: {}", e)))?;

        Ok(NodeId(page_id.as_u64() as u32))
    }

    /// Update an existing node in storage
    fn update_node(&self, node_id: NodeId, node: &HnswNode) -> TableResult<()> {
        let page_id = PageId::from(node_id.as_u32() as u64);

        let data = Self::serialize_node(node)?;

        let page_size = self.pager.page_size().to_u32() as usize;
        let mut page = Page::new(page_id, PageType::VectorIndex, page_size);
        page.data_mut().extend_from_slice(&data);

        self.pager
            .write_page(&page)
            .map_err(|e| TableError::Other(format!("Failed to update node page: {}", e)))?;

        Ok(())
    }

    /// Serialize a node to bytes
    fn serialize_node(node: &HnswNode) -> TableResult<Vec<u8>> {
        let mut data = Vec::new();

        // Vector ID length + data
        let id_bytes = node.id.as_ref();
        let id_len = id_bytes.len() as u32;
        data.extend_from_slice(&id_len.to_le_bytes());
        data.extend_from_slice(id_bytes);

        // Vector length + data
        let vec_len = node.vector.len() as u32;
        data.extend_from_slice(&vec_len.to_le_bytes());
        for &v in &node.vector {
            data.extend_from_slice(&v.to_le_bytes());
        }

        // Layer
        data.extend_from_slice(&(node.layer as u32).to_le_bytes());

        // Neighbors: number of layers, then for each layer: count + node IDs
        data.extend_from_slice(&(node.neighbors.len() as u32).to_le_bytes());
        for layer_neighbors in &node.neighbors {
            data.extend_from_slice(&(layer_neighbors.len() as u32).to_le_bytes());
            for &n in layer_neighbors {
                data.extend_from_slice(&n.0.to_le_bytes());
            }
        }

        // Serialize version chain using postcard
        let chain_bytes = postcard::to_allocvec(&node.version_chain).unwrap_or_default();
        data.extend_from_slice(&(chain_bytes.len() as u32).to_le_bytes());
        data.extend_from_slice(&chain_bytes);

        Ok(data)
    }

    /// Deserialize a node from bytes
    fn deserialize_node(data: &[u8]) -> TableResult<HnswNode> {
        let mut pos = 0;

        // Read vector ID
        let id_len = u32::from_le_bytes(
            data[pos..pos + 4]
                .try_into()
                .map_err(|e| TableError::Other(format!("Failed to read id length: {}", e)))?,
        ) as usize;
        pos += 4;
        let id = KeyBuf(data[pos..pos + id_len].to_vec());
        pos += id_len;

        // Read vector
        let vec_len = u32::from_le_bytes(
            data[pos..pos + 4]
                .try_into()
                .map_err(|e| TableError::Other(format!("Failed to read vector length: {}", e)))?,
        ) as usize;
        pos += 4;
        let mut vector = Vec::with_capacity(vec_len);
        for _ in 0..vec_len {
            let v =
                f32::from_le_bytes(data[pos..pos + 4].try_into().map_err(|e| {
                    TableError::Other(format!("Failed to read vector element: {}", e))
                })?);
            vector.push(v);
            pos += 4;
        }

        // Read layer
        let layer = u32::from_le_bytes(
            data[pos..pos + 4]
                .try_into()
                .map_err(|e| TableError::Other(format!("Failed to read layer: {}", e)))?,
        ) as usize;
        pos += 4;

        // Read neighbors
        let num_layers = u32::from_le_bytes(
            data[pos..pos + 4]
                .try_into()
                .map_err(|e| TableError::Other(format!("Failed to read num layers: {}", e)))?,
        ) as usize;
        pos += 4;
        let mut neighbors = Vec::with_capacity(num_layers);
        for _ in 0..num_layers {
            let count =
                u32::from_le_bytes(data[pos..pos + 4].try_into().map_err(|e| {
                    TableError::Other(format!("Failed to read neighbor count: {}", e))
                })?) as usize;
            pos += 4;
            let mut layer_neighbors = Vec::with_capacity(count);
            for _ in 0..count {
                let n = NodeId(u32::from_le_bytes(data[pos..pos + 4].try_into().map_err(
                    |e| TableError::Other(format!("Failed to read neighbor id: {}", e)),
                )?));
                layer_neighbors.push(n);
                pos += 4;
            }
            neighbors.push(layer_neighbors);
        }

        // Deserialize version chain
        let chain_len = u32::from_le_bytes(data[pos..pos + 4].try_into().map_err(|e| {
            TableError::Other(format!("Failed to read version chain length: {}", e))
        })?) as usize;
        pos += 4;

        let version_chain = postcard::from_bytes(&data[pos..pos + chain_len]).map_err(|e| {
            TableError::Other(format!("Failed to deserialize version chain: {}", e))
        })?;

        Ok(HnswNode {
            id,
            vector,
            layer,
            neighbors,
            version_chain,
        })
    }

    /// Select M neighbors from candidates using heuristic
    fn select_neighbors(
        &self,
        candidates: Vec<Candidate>,
        m: usize,
        _layer: usize,
        _extend_candidates: bool,
    ) -> Vec<NodeId> {
        // Simple heuristic: select M closest neighbors
        candidates.into_iter().take(m).map(|c| c.node_id).collect()
    }

    /// Add bidirectional connections between nodes
    fn connect_nodes(
        &self,
        node_id: NodeId,
        neighbors: Vec<NodeId>,
        layer: usize,
    ) -> TableResult<()> {
        // Load the node, add neighbors, and store it back
        let mut node = self.load_node(node_id)?;
        if layer >= node.neighbors.len() {
            node.neighbors.resize(layer + 1, Vec::new());
        }
        for n in &neighbors {
            if !node.neighbors[layer].contains(n) {
                node.neighbors[layer].push(*n);
            }
        }
        self.update_node(node_id, &node)?;

        // Add reverse connections
        for neighbor_id in neighbors {
            let mut neighbor = self.load_node(neighbor_id)?;
            if layer >= neighbor.neighbors.len() {
                neighbor.neighbors.resize(layer + 1, Vec::new());
            }
            if !neighbor.neighbors[layer].contains(&node_id) {
                neighbor.neighbors[layer].push(node_id);
            }
            self.update_node(neighbor_id, &neighbor)?;
        }

        Ok(())
    }

    /// Prune connections if a node has too many neighbors
    fn prune_connections(&self, node_id: NodeId, layer: usize) -> TableResult<()> {
        let max_connections = if layer == 0 {
            self.config.read().unwrap().max_connections_layer0
        } else {
            self.config.read().unwrap().max_connections
        };

        let mut node = self.load_node(node_id)?;
        if layer < node.neighbors.len() && node.neighbors[layer].len() > max_connections {
            // Keep only the closest neighbors (simple heuristic: keep first M)
            node.neighbors[layer].truncate(max_connections);
            self.update_node(node_id, &node)?;
        }

        Ok(())
    }
}

impl<FS: FileSystem> Table for PagedHnswVector<FS> {
    fn table_id(&self) -> TableId {
        self.table_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> TableEngineKind {
        TableEngineKind::VectorHnsw
    }

    fn capabilities(&self) -> crate::table::TableCapabilities {
        crate::table::TableCapabilities {
            ordered: false,
            point_lookup: false,
            prefix_scan: false,
            reverse_scan: false,
            range_delete: false,
            merge_operator: false,
            mvcc_native: false,
            append_optimized: false,
            memory_resident: true,
            disk_resident: false,
            supports_compression: false,
            supports_encryption: false,
        }
    }

    fn stats(&self) -> TableResult<crate::table::TableStatistics> {
        Ok(crate::table::TableStatistics {
            row_count: Some(*self.num_vectors.read().unwrap() as u64),
            total_size_bytes: Some(0), // TODO: Calculate actual size
            key_stats: None,
            value_stats: None,
            histogram: None,
            last_updated_lsn: None,
        })
    }
}

impl<FS: FileSystem> VectorSearch for PagedHnswVector<FS> {
    fn table_id(&self) -> TableId {
        self.table_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> SpecialtyTableCapabilities {
        SpecialtyTableCapabilities {
            exact: false,
            approximate: true, // HNSW is approximate nearest neighbor
            ordered: false,
            sparse: false,
            supports_delete: true,
            supports_range_query: false,
            supports_prefix_query: false,
            supports_scoring: true, // Returns distance scores
            supports_incremental_rebuild: false,
            may_be_stale: false,
        }
    }

    fn dimensions(&self) -> usize {
        self.config.read().unwrap().dimensions
    }

    fn metric(&self) -> VectorMetric {
        self.config.read().unwrap().metric
    }

    fn insert_vector(
        &self,
        id: &[u8],
        vector: &[f32],
        _tx_id: crate::txn::TransactionId,
        _commit_lsn: crate::wal::LogSequenceNumber,
    ) -> TableResult<()> {
        // Validate vector dimensions
        if vector.len() != self.config.read().unwrap().dimensions {
            return Err(TableError::invalid_value(
                "vector",
                format!(
                    "dimension mismatch: expected {}, got {}",
                    self.config.read().unwrap().dimensions,
                    vector.len()
                ),
            ));
        }

        let id_buf = KeyBuf(id.to_vec());

        // Check if vector already exists
        if self.id_to_node.read().unwrap().contains_key(&id_buf) {
            return Err(TableError::Other(format!(
                "Vector with ID {:?} already exists",
                id_buf
            )));
        }

        // Select layer for new node
        let layer = self.select_layer();

        // Get entry point
        let entry_point = *self.entry_point.read().unwrap();

        let node_id = if let Some(ep) = entry_point {
            // Insert into existing graph
            let max_layer = *self.max_layer.read().unwrap();

            // Create initial node with empty neighbors
            // Use transaction ID 0 for non-transactional insert (will be committed immediately)
            let initial_node = HnswNode::new(
                id_buf.clone(),
                vector.to_vec(),
                layer,
                TransactionId::from(0),
            );
            let node_id = self.store_node(&initial_node)?;

            // Search from top layer down to layer+1
            let mut current_nearest = vec![ep];
            for lc in (layer + 1..=max_layer).rev() {
                current_nearest = self
                    .search_layer(vector, current_nearest, 1, lc)?
                    .into_iter()
                    .map(|c| c.node_id)
                    .collect();
            }

            // Insert at layers 0..=layer
            for lc in 0..=layer {
                let m = if lc == 0 {
                    self.config.read().unwrap().max_connections_layer0
                } else {
                    self.config.read().unwrap().max_connections
                };

                let candidates = self.search_layer(
                    vector,
                    current_nearest.clone(),
                    self.config.read().unwrap().ef_construction,
                    lc,
                )?;

                let neighbors = self.select_neighbors(candidates, m, lc, true);

                // Add bidirectional connections
                self.connect_nodes(node_id, neighbors.clone(), lc)?;

                // Update neighbors' connections
                for neighbor_id in neighbors {
                    self.prune_connections(neighbor_id, lc)?;
                }

                // Update current_nearest for next layer
                current_nearest = vec![node_id];
            }

            // Update max layer if needed
            if layer > max_layer {
                *self.max_layer.write().unwrap() = layer;
                *self.entry_point.write().unwrap() = Some(node_id);
            }

            node_id
        } else {
            // First node - becomes entry point
            // Use transaction ID 0 for non-transactional insert (will be committed immediately)
            let node = HnswNode::new(
                id_buf.clone(),
                vector.to_vec(),
                layer,
                TransactionId::from(0),
            );
            let node_id = self.store_node(&node)?;
            *self.entry_point.write().unwrap() = Some(node_id);
            *self.max_layer.write().unwrap() = layer;
            node_id
        };

        // Update mappings
        self.id_to_node.write().unwrap().insert(id_buf, node_id);
        *self.num_vectors.write().unwrap() += 1;

        Ok(())
    }

    fn delete_vector(
        &self,
        id: &[u8],
        _tx_id: crate::txn::TransactionId,
        _commit_lsn: crate::wal::LogSequenceNumber,
    ) -> TableResult<()> {
        let id_buf = KeyBuf(id.to_vec());

        // Find node
        let _node_id = self
            .id_to_node
            .write()
            .unwrap()
            .remove(&id_buf)
            .ok_or_else(|| TableError::key_not_found(format!("Vector with ID {:?}", id_buf)))?;

        // TODO: Implement graph repair after deletion
        // This involves:
        // 1. Loading the node to get its neighbors
        // 2. Removing all connections to this node from neighbors
        // 3. Reconnecting neighbors to maintain graph connectivity
        // 4. Updating entry point if this was the entry point

        *self.num_vectors.write().unwrap() -= 1;

        Ok(())
    }

    fn search_vector<'a>(
        &self,
        query: &[f32],
        options: VectorSearchOptions<'a>,
    ) -> TableResult<Vec<VectorHit>> {
        // Validate query dimensions
        if query.len() != self.config.read().unwrap().dimensions {
            return Err(TableError::invalid_value(
                "query",
                format!(
                    "dimension mismatch: expected {}, got {}",
                    self.config.read().unwrap().dimensions,
                    query.len()
                ),
            ));
        }

        let entry_point = *self.entry_point.read().unwrap();
        if entry_point.is_none() {
            return Ok(Vec::new());
        }

        let ep = entry_point.unwrap();
        let max_layer = *self.max_layer.read().unwrap();
        let ef = options
            .ef_search
            .unwrap_or(self.config.read().unwrap().ef_construction);

        // Search from top layer down to layer 0
        let mut current_nearest = vec![ep];
        for lc in (1..=max_layer).rev() {
            current_nearest = self
                .search_layer(query, current_nearest, 1, lc)?
                .into_iter()
                .map(|c| c.node_id)
                .collect();
        }

        // Final search at layer 0
        let candidates = self.search_layer(query, current_nearest, ef, 0)?;

        // Convert to VectorHit and apply limit
        let mut results = Vec::new();
        for candidate in candidates.into_iter().take(options.limit) {
            let node = self.load_node(candidate.node_id)?;
            results.push(VectorHit {
                id: node.id,
                distance: candidate.distance,
            });
        }

        Ok(results)
    }

    fn stats(&self) -> TableResult<SpecialtyTableStats> {
        Ok(SpecialtyTableStats {
            entry_count: Some(*self.num_vectors.read().unwrap() as u64),
            size_bytes: Some(0), // TODO: Calculate actual size
            distinct_keys: Some(*self.num_vectors.read().unwrap() as u64),
            stale_entries: None,
            last_updated_lsn: None,
        })
    }

    fn verify(&self) -> TableResult<VerificationReport> {
        // TODO: Implement verification
        Ok(VerificationReport {
            checked_items: *self.num_vectors.read().unwrap() as u64,
            errors: Vec::new(),
            warnings: Vec::new(),
        })
    }
}

impl<FS: FileSystem> HnswVector for PagedHnswVector<FS> {
    fn set_ef_construction(&self, ef: usize) {
        self.config.write().unwrap().ef_construction = ef;
    }

    fn set_max_connections(&self, m: usize) {
        self.config.write().unwrap().max_connections = m;
        self.config.write().unwrap().max_connections_layer0 = m * 2;
    }
}

// MVCC transaction support methods
impl<FS: FileSystem> PagedHnswVector<FS> {
    /// Insert a vector with transaction tracking.
    pub fn insert_vector_tx(
        &self,
        id: &[u8],
        vector: &[f32],
        tx_id: TransactionId,
    ) -> TableResult<()> {
        // Validate vector dimensions
        if vector.len() != self.config.read().unwrap().dimensions {
            return Err(TableError::invalid_value(
                "vector",
                format!(
                    "dimension mismatch: expected {}, got {}",
                    self.config.read().unwrap().dimensions,
                    vector.len()
                ),
            ));
        }

        let id_buf = KeyBuf(id.to_vec());

        // Check if vector already exists
        if self.id_to_node.read().unwrap().contains_key(&id_buf) {
            return Err(TableError::Other(format!(
                "Vector with ID {:?} already exists",
                id_buf
            )));
        }

        // Select layer for new node
        let layer = self.select_layer();

        // Get entry point
        let entry_point = *self.entry_point.read().unwrap();

        let node_id = if let Some(ep) = entry_point {
            // Insert into existing graph
            let max_layer = *self.max_layer.read().unwrap();

            // Create initial node with transaction tracking
            let initial_node = HnswNode::new(id_buf.clone(), vector.to_vec(), layer, tx_id);
            let node_id = self.store_node(&initial_node)?;

            // Search from top layer down to layer+1
            let mut current_nearest = vec![ep];
            for lc in (layer + 1..=max_layer).rev() {
                current_nearest = self
                    .search_layer(vector, current_nearest, 1, lc)?
                    .into_iter()
                    .map(|c| c.node_id)
                    .collect();
            }

            // Insert at layers 0..=layer
            for lc in 0..=layer {
                let m = if lc == 0 {
                    self.config.read().unwrap().max_connections_layer0
                } else {
                    self.config.read().unwrap().max_connections
                };

                let candidates = self.search_layer(
                    vector,
                    current_nearest.clone(),
                    self.config.read().unwrap().ef_construction,
                    lc,
                )?;

                let neighbors = self.select_neighbors(candidates, m, lc, true);

                // Add bidirectional connections
                self.connect_nodes(node_id, neighbors.clone(), lc)?;

                // Update neighbors' connections
                for neighbor_id in neighbors {
                    self.prune_connections(neighbor_id, lc)?;
                }

                // Update current_nearest for next layer
                current_nearest = vec![node_id];
            }

            // Update max layer if needed
            if layer > max_layer {
                *self.max_layer.write().unwrap() = layer;
                *self.entry_point.write().unwrap() = Some(node_id);
            }

            node_id
        } else {
            // First node - becomes entry point
            let node = HnswNode::new(id_buf.clone(), vector.to_vec(), layer, tx_id);
            let node_id = self.store_node(&node)?;
            *self.entry_point.write().unwrap() = Some(node_id);
            *self.max_layer.write().unwrap() = layer;
            node_id
        };

        // Update mapping
        self.id_to_node.write().unwrap().insert(id_buf, node_id);
        *self.num_vectors.write().unwrap() += 1;

        Ok(())
    }

    /// Delete a vector with transaction tracking.
    pub fn delete_vector_tx(&self, id: &[u8], tx_id: TransactionId) -> TableResult<()> {
        let id_buf = KeyBuf(id.to_vec());

        // Find the node
        let node_id = self
            .id_to_node
            .read()
            .unwrap()
            .get(&id_buf)
            .copied()
            .ok_or_else(|| TableError::key_not_found(format!("Vector with ID {:?}", id_buf)))?;

        // Load the node and mark it with a tombstone
        let mut node = self.load_node(node_id)?;
        node.prepend_tombstone(tx_id);
        self.update_node(node_id, &node)?;

        Ok(())
    }

    /// Search for vectors respecting snapshot visibility.
    pub fn search_vector_snapshot(
        &self,
        query: &[f32],
        limit: usize,
        ef_search: Option<usize>,
        snapshot: &Snapshot,
    ) -> TableResult<Vec<VectorHit>> {
        // Validate query dimensions
        if query.len() != self.config.read().unwrap().dimensions {
            return Err(TableError::invalid_value(
                "query",
                format!(
                    "dimension mismatch: expected {}, got {}",
                    self.config.read().unwrap().dimensions,
                    query.len()
                ),
            ));
        }

        let entry_point = *self.entry_point.read().unwrap();
        if entry_point.is_none() {
            return Ok(Vec::new());
        }

        let ep = entry_point.unwrap();
        let max_layer = *self.max_layer.read().unwrap();
        let ef = ef_search.unwrap_or(self.config.read().unwrap().ef_construction);

        // Search from top layer down to layer 0
        let mut current_nearest = vec![ep];
        for lc in (1..=max_layer).rev() {
            current_nearest = self
                .search_layer(query, current_nearest, 1, lc)?
                .into_iter()
                .map(|c| c.node_id)
                .collect();
        }

        // Final search at layer 0
        let candidates = self.search_layer(query, current_nearest, ef, 0)?;

        // Convert to VectorHit, filter by visibility, and apply limit
        let mut results = Vec::new();
        for candidate in candidates {
            let node = self.load_node(candidate.node_id)?;

            // Check visibility
            if node.is_visible(snapshot) {
                results.push(VectorHit {
                    id: node.id,
                    distance: candidate.distance,
                });

                if results.len() >= limit {
                    break;
                }
            }
        }

        Ok(results)
    }

    /// Commit all versions created by the given transaction.
    pub fn commit_versions(
        &self,
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        // Iterate through all nodes and commit matching versions
        let id_to_node = self.id_to_node.read().unwrap();
        for &node_id in id_to_node.values() {
            let mut node = self.load_node(node_id)?;

            if node.version_chain.created_by == tx_id && node.version_chain.commit_lsn.is_none() {
                node.commit(commit_lsn);
                self.update_node(node_id, &node)?;
            }
        }

        Ok(())
    }

    /// Vacuum old versions that are no longer visible.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        let mut total_removed = 0;

        // Iterate through all nodes and vacuum old versions
        let id_to_node = self.id_to_node.read().unwrap();
        for &node_id in id_to_node.values() {
            let mut node = self.load_node(node_id)?;
            let removed = node.vacuum(min_visible_lsn);

            if removed > 0 {
                total_removed += removed;
                self.update_node(node_id, &node)?;
            }
        }

        Ok(total_removed)
    }
}

// Made with Bob
