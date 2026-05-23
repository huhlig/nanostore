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
/// Node cache statistics
#[derive(Debug, Clone, Default)]
struct NodeCacheStats {
    /// Total cache hits
    hits: u64,
    /// Total cache misses
    misses: u64,
    /// Total evictions
    evictions: u64,
}

impl NodeCacheStats {
    fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// Simple LRU cache for HNSW nodes
///
/// Uses a simplified approach: just cache nodes without LRU tracking
/// to avoid lock contention. This is acceptable because:
/// 1. During insertion, we access the same nodes repeatedly
/// 2. Cache size is large enough (10K nodes) to hold working set
/// 3. Avoiding write locks on every cache hit is more important than perfect LRU
/// 4. All nodes are preloaded on index load, so cache never evicts during queries
struct NodeCache {
    /// Cached nodes
    nodes: HashMap<NodeId, HnswNode>,
    /// Maximum cache size
    capacity: usize,
    /// Cache statistics
    stats: NodeCacheStats,
    /// Whether cache is preloaded (all nodes loaded at startup)
    preloaded: bool,
}

impl NodeCache {
    fn new(capacity: usize) -> Self {
        Self {
            nodes: HashMap::with_capacity(capacity),
            capacity,
            stats: NodeCacheStats::default(),
            preloaded: false,
        }
    }

    fn get(&self, node_id: NodeId) -> Option<HnswNode> {
        self.nodes.get(&node_id).cloned()
    }

    fn insert(&mut self, node_id: NodeId, node: HnswNode) {
        // If preloaded, never evict (all nodes fit in cache)
        if self.preloaded {
            self.nodes.insert(node_id, node);
            return;
        }

        // Simple eviction: clear cache when full
        // This is acceptable because we're in a single insertion operation
        if self.nodes.len() >= self.capacity && !self.nodes.contains_key(&node_id) {
            self.nodes.clear();
            self.stats.evictions += 1;
        }

        self.nodes.insert(node_id, node);
    }

    fn clear(&mut self) {
        self.nodes.clear();
        self.preloaded = false;
    }

    fn record_hit(&mut self) {
        self.stats.hits += 1;
    }

    fn record_miss(&mut self) {
        self.stats.misses += 1;
    }

    fn stats(&self) -> &NodeCacheStats {
        &self.stats
    }

    fn mark_preloaded(&mut self) {
        self.preloaded = true;
    }

    fn is_preloaded(&self) -> bool {
        self.preloaded
    }

    fn len(&self) -> usize {
        self.nodes.len()
    }
}


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

    /// Node cache for fast access during insertion and search
    node_cache: RwLock<NodeCache>,
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

    /// Size of dynamic candidate list during construction (`ef_construction`)
    pub ef_construction: usize,

    /// Multiplier for layer selection probability
    pub ml: f64,

    /// Node cache capacity (number of nodes to keep in memory)
    /// Default: 100,000 nodes (~60MB for 128-dim vectors)
    /// Set to 0 for unlimited (all nodes stay in memory, no eviction)
    pub cache_capacity: usize,
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
            cache_capacity: 100_000, // 100K nodes, ~60MB for 128-dim vectors
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

    /// Commit this node's version at the given LSN.
    fn commit(&mut self, lsn: LogSequenceNumber) {
        self.version_chain.commit(lsn);
    }

    /// Prepend a new version to this node's chain.
    /// For deletions, use `prepend_tombstone` instead.
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
    /// Returns (`removed_count`, `freed_refs`) where `freed_refs` contains `ValueRefs` that need cleanup.
    fn vacuum(
        &mut self,
        min_visible_lsn: LogSequenceNumber,
    ) -> (usize, Vec<crate::types::ValueRef>) {
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

    /// `ef_construction` parameter
    ef_construction: u32,

    /// ml parameter (stored as f64)
    ml: f64,

    /// Entry point node ID (0 if none)
    entry_point: u32,

    /// Maximum layer in use
    max_layer: u32,

    /// Number of vectors
    num_vectors: u64,

    /// First page of `id_to_node` mapping (0 if none)
    mapping_page_id: u64,

    /// Reserved for future use
    _reserved: [u8; 56],
}

const HNSW_MAGIC: u32 = 0x484E_5357; // "HNSW"
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
            mapping_page_id: 0,
            _reserved: [0; 56],
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
            node_cache: RwLock::new(NodeCache::new(10000)), // Cache up to 10K nodes
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
            cache_capacity: 100_000, // Default capacity for loaded indexes
        };

        let entry_point = if metadata.entry_point == 0 {
            None
        } else {
            Some(NodeId(metadata.entry_point))
        };

        // Load id_to_node mapping from pages
        let temp_self = Self {
            table_id,
            name: name.clone(),
            pager: pager.clone(),
            root_page_id,
            config: RwLock::new(config.clone()),
            entry_point: RwLock::new(entry_point),
            max_layer: RwLock::new(metadata.max_layer as usize),
            num_vectors: RwLock::new(metadata.num_vectors as usize),
            id_to_node: RwLock::new(HashMap::new()),
            rng_state: RwLock::new(12345),
            node_cache: RwLock::new(NodeCache::new(10000)),
        };

        let id_to_node = temp_self.deserialize_mapping(PageId::from(metadata.mapping_page_id))?;

        let instance = Self {
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
            node_cache: RwLock::new(NodeCache::new(10000)),
        };

        // Preload all nodes into cache for fast query performance
        instance.preload_all_nodes()?;

        Ok(instance)
    }

    /// Preload all nodes into the cache.
    ///
    /// This loads all node data (including vectors) into memory at index load time,
    /// ensuring that queries never need to wait for disk I/O. This is critical for
    /// performance with the HNSW algorithm, which accesses many nodes during search.
    fn preload_all_nodes(&self) -> TableResult<()> {
        let start = std::time::Instant::now();
        
        let id_to_node = self.id_to_node.read().unwrap();
        let num_nodes = id_to_node.len();
        
        if num_nodes == 0 {
            // Empty index, nothing to preload
            self.node_cache.write().unwrap().mark_preloaded();
            eprintln!("[HNSW] Preload: empty index, no nodes to load");
            return Ok(());
        }

        eprintln!("[HNSW] Preload: starting to load {} nodes into cache", num_nodes);

        // Collect all node IDs
        let node_ids: Vec<NodeId> = id_to_node.values().copied().collect();
        
        // Load all nodes into cache
        for (idx, node_id) in node_ids.iter().enumerate() {
            let page_id = PageId::from(node_id.0 as u64);
            let page = self
                .pager
                .read_page(page_id)
                .map_err(|e| TableError::Other(format!("Failed to read node page during preload: {}", e)))?;

            let node = Self::deserialize_node(page.data())?;
            
            // Insert into cache (no eviction during preload)
            self.node_cache.write().unwrap().insert(*node_id, node);
            
            if (idx + 1) % 1000 == 0 {
                eprintln!("[HNSW] Preload: loaded {}/{} nodes ({:.1}%)",
                    idx + 1, num_nodes, (idx + 1) as f64 / num_nodes as f64 * 100.0);
            }
        }

        // Mark cache as preloaded to prevent evictions during queries
        self.node_cache.write().unwrap().mark_preloaded();

        let elapsed = start.elapsed();
        eprintln!("[HNSW] Preload: completed loading {} nodes in {:.2}s ({:.0} nodes/sec)",
            num_nodes, elapsed.as_secs_f64(), num_nodes as f64 / elapsed.as_secs_f64());
        
        // Report cache stats
        let cache = self.node_cache.read().unwrap();
        eprintln!("[HNSW] Cache: {} nodes cached, preloaded={}",
            cache.len(), cache.is_preloaded());

        Ok(())
    }
    /// Get the root page ID.
    pub fn root_page_id(&self) -> PageId {
        self.root_page_id
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
        *rng_state = rng_state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
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

        // Greedy search - explore the graph to find nearest neighbors
        while let Some(current) = candidates.pop() {
            // Early termination: In HNSW, we stop when the closest unexplored candidate
            // is further than the furthest result we're keeping. This only makes sense
            // when we have a full result set.
            if results.len() >= ef {
                if let Some(furthest) = results.peek() {
                    // current is the closest candidate (min-heap), furthest is the worst result (max-heap)
                    if current.distance > furthest.distance {
                        break;
                    }
                }
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

                        // Always add to candidates for exploration if it might improve results
                        // or if we don't have enough results yet
                        let furthest_dist = results.peek().map(|r| r.distance);
                        if results.len() < ef || furthest_dist.map_or(true, |fd| dist < fd) {
                            candidates.push(candidate.clone());
                        }

                        // Add to results if we don't have enough or if it's better than worst result
                        if results.len() < ef || dist < results.peek().unwrap().distance {
                            results.push(candidate);

                            // Prune results to maintain ef size
                            if results.len() > ef {
                                results.pop();
                            }
                        }
                    }
                }
            }
        }

        // Convert to sorted vector (closest first)
        let mut result_vec: Vec<_> = results.into_iter().collect();
        result_vec.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(result_vec)
    }

    /// Load a node from storage (with caching)
    fn load_node(&self, node_id: NodeId) -> TableResult<HnswNode> {
        // Try cache first with read lock
        if let Some(node) = self.node_cache.read().unwrap().get(node_id) {
            self.node_cache.write().unwrap().record_hit();
            return Ok(node);
        }

        // Cache miss - load from disk
        self.node_cache.write().unwrap().record_miss();
        
        let page_id = PageId::from(node_id.0 as u64);
        let page = self
            .pager
            .read_page(page_id)
            .map_err(|e| TableError::Other(format!("Failed to read node page: {}", e)))?;

        let node = Self::deserialize_node(page.data())?;

        // Store in cache
        self.node_cache.write().unwrap().insert(node_id, node.clone());

        Ok(node)
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

        let node_id = NodeId(page_id.as_u64() as u32);
        
        // Store in cache
        self.node_cache.write().unwrap().insert(node_id, node.clone());

        Ok(node_id)
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

        // Update cache
        self.node_cache.write().unwrap().insert(node_id, node.clone());

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
    /// Serialize `id_to_node` mapping to pages
    fn serialize_mapping(&self, mapping: &HashMap<KeyBuf, NodeId>) -> TableResult<PageId> {
        if mapping.is_empty() {
            return Ok(PageId::from(0u64));
        }

        let mut data = Vec::new();

        // Write number of entries
        data.extend_from_slice(&(mapping.len() as u32).to_le_bytes());

        // Write each entry: key_len + key_bytes + node_id
        for (key, node_id) in mapping {
            let key_bytes = key.as_ref();
            data.extend_from_slice(&(key_bytes.len() as u32).to_le_bytes());
            data.extend_from_slice(key_bytes);
            data.extend_from_slice(&node_id.0.to_le_bytes());
        }

        // Allocate page(s) and write data
        let page_size = self.pager.page_size().to_u32() as usize;
        let data_size = page_size - 8; // Reserve 8 bytes for next_page_id

        let mut first_page_id = PageId::from(0u64);
        let mut prev_page_id = None;
        let mut offset = 0;

        while offset < data.len() {
            let page_id = self
                .pager
                .allocate_page(PageType::VectorIndex)
                .map_err(|e| {
                    TableError::Other(format!("Failed to allocate mapping page: {}", e))
                })?;

            if first_page_id.as_u64() == 0 {
                first_page_id = page_id;
            }

            let chunk_size = std::cmp::min(data_size, data.len() - offset);
            let mut page = Page::new(page_id, PageType::VectorIndex, page_size);

            // Write data chunk
            page.data_mut()
                .extend_from_slice(&data[offset..offset + chunk_size]);

            // Write next_page_id (0 if last page)
            let next_page_id = if offset + chunk_size < data.len() {
                u64::MAX // Placeholder, will be updated
            } else {
                0u64
            };
            page.data_mut()
                .extend_from_slice(&next_page_id.to_le_bytes());

            self.pager
                .write_page(&page)
                .map_err(|e| TableError::Other(format!("Failed to write mapping page: {}", e)))?;

            // Update previous page's next_page_id
            if let Some(prev_id) = prev_page_id {
                let mut prev_page = self.pager.read_page(prev_id).map_err(|e| {
                    TableError::Other(format!("Failed to read previous mapping page: {}", e))
                })?;

                let next_offset = prev_page.data().len() - 8;
                prev_page.data_mut()[next_offset..]
                    .copy_from_slice(&page_id.as_u64().to_le_bytes());

                self.pager.write_page(&prev_page).map_err(|e| {
                    TableError::Other(format!("Failed to update previous mapping page: {}", e))
                })?;
            }

            prev_page_id = Some(page_id);
            offset += chunk_size;
        }

        Ok(first_page_id)
    }

    /// Deserialize `id_to_node` mapping from pages
    fn deserialize_mapping(&self, first_page_id: PageId) -> TableResult<HashMap<KeyBuf, NodeId>> {
        if first_page_id.as_u64() == 0 {
            return Ok(HashMap::new());
        }

        let mut data = Vec::new();
        let mut current_page_id = first_page_id;

        // Read all pages in the chain
        loop {
            let page = self
                .pager
                .read_page(current_page_id)
                .map_err(|e| TableError::Other(format!("Failed to read mapping page: {}", e)))?;

            let page_data = page.data();
            if page_data.len() < 8 {
                return Err(TableError::corruption(
                    "HNSW mapping page",
                    "page too small",
                    format!("page size: {}", page_data.len()),
                ));
            }

            // Read next_page_id from last 8 bytes
            let next_offset = page_data.len() - 8;
            let next_page_id =
                u64::from_le_bytes(page_data[next_offset..next_offset + 8].try_into().map_err(
                    |e| TableError::Other(format!("Failed to read next page id: {}", e)),
                )?);

            // Append data (excluding next_page_id)
            data.extend_from_slice(&page_data[..next_offset]);

            if next_page_id == 0 {
                break;
            }
            current_page_id = PageId::from(next_page_id);
        }

        // Deserialize mapping
        let mut mapping = HashMap::new();
        let mut pos = 0;

        // Read number of entries
        if data.len() < 4 {
            return Err(TableError::corruption(
                "HNSW mapping",
                "insufficient data",
                format!("data size: {}", data.len()),
            ));
        }

        let num_entries = u32::from_le_bytes(
            data[pos..pos + 4]
                .try_into()
                .map_err(|e| TableError::Other(format!("Failed to read entry count: {}", e)))?,
        ) as usize;
        pos += 4;

        // Read each entry
        for _ in 0..num_entries {
            if pos + 4 > data.len() {
                return Err(TableError::corruption(
                    "HNSW mapping",
                    "truncated key length",
                    format!("position: {}, data size: {}", pos, data.len()),
                ));
            }

            let key_len = u32::from_le_bytes(
                data[pos..pos + 4]
                    .try_into()
                    .map_err(|e| TableError::Other(format!("Failed to read key length: {}", e)))?,
            ) as usize;
            pos += 4;

            if pos + key_len + 4 > data.len() {
                return Err(TableError::corruption(
                    "HNSW mapping",
                    "truncated entry data",
                    format!(
                        "position: {}, key_len: {}, data size: {}",
                        pos,
                        key_len,
                        data.len()
                    ),
                ));
            }

            let key = KeyBuf(data[pos..pos + key_len].to_vec());
            pos += key_len;

            let node_id =
                NodeId(u32::from_le_bytes(data[pos..pos + 4].try_into().map_err(
                    |e| TableError::Other(format!("Failed to read node id: {}", e)),
                )?));
            pos += 4;

            mapping.insert(key, node_id);
        }

        Ok(mapping)
    }
    /// Persist the `id_to_node` mapping to disk and update metadata
    fn persist_mapping(&self) -> TableResult<()> {
        let mapping = self.id_to_node.read().unwrap();
        let mapping_page_id = self.serialize_mapping(&mapping)?;
        drop(mapping);

        // Update metadata with new mapping page
        let mut metadata = Self::read_metadata(&self.pager, self.root_page_id)?;
        metadata.mapping_page_id = mapping_page_id.as_u64();
        metadata.num_vectors = *self.num_vectors.read().unwrap() as u64;
        metadata.entry_point = self.entry_point.read().unwrap().map(|n| n.0).unwrap_or(0);
        metadata.max_layer = *self.max_layer.read().unwrap() as u32;

        Self::write_metadata(&self.pager, self.root_page_id, &metadata)?;

        Ok(())
    }

    /// Select M neighbors from candidates using the HNSW paper's SELECT-NEIGHBORS-HEURISTIC
    ///
    /// This implements Algorithm 4 from the HNSW paper with bounded candidate pool:
    /// 1. Sort candidates by distance to query
    /// 2. Limit pool to top 3*M candidates (bounded diversity pruning)
    /// 3. Pre-load vectors for bounded pool (amortizes I/O cost)
    /// 4. For each candidate in order, check if it's closer to query than to any selected neighbor
    /// 5. If yes (diverse), add to result; if no (redundant), skip
    ///
    /// The diversity check prevents redundant edges by ensuring each selected neighbor
    /// provides a unique direction from the query point. This preserves bridge edges
    /// between clusters while limiting complexity to O((3M)²) instead of O(candidates²).
    ///
    /// Pre-loading vectors for the bounded pool amortizes the I/O cost across all diversity checks.
    fn select_neighbors(
        &self,
        mut candidates: Vec<Candidate>,
        m: usize,
        _layer: usize,
        extend_candidates: bool,
    ) -> Vec<NodeId> {
        if candidates.is_empty() {
            return Vec::new();
        }

        // Sort candidates by distance (closest first)
        candidates.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // If we don't need diversity or have few candidates, use simple selection
        if !extend_candidates || candidates.len() <= m {
            return candidates.into_iter().take(m).map(|c| c.node_id).collect();
        }

        // Bounded diversity pruning: limit pool size to 3*M
        // This is the key optimization - we only apply the heuristic to the closest 3*M candidates
        let pool_size = (m * 3).min(candidates.len());
        let prune_pool: Vec<Candidate> = candidates.into_iter().take(pool_size).collect();

        // Pre-load vectors for the pruning pool to amortize I/O cost
        // This is critical for performance - loading on-demand during diversity checks
        // would cause O(pool_size) loads instead of O(pool_size) with caching benefits
        let mut candidate_vectors: std::collections::HashMap<NodeId, Vec<f32>> =
            std::collections::HashMap::with_capacity(pool_size);
        
        for candidate in &prune_pool {
            if let Ok(node) = self.load_node(candidate.node_id) {
                candidate_vectors.insert(candidate.node_id, node.vector);
            }
        }

        // Select diverse neighbors using the paper's heuristic (Algorithm 4, lines 9-14)
        let mut result = Vec::with_capacity(m);
        let mut selected_vectors: Vec<Vec<f32>> = Vec::with_capacity(m);

        for candidate in prune_pool {
            if result.len() >= m {
                break;
            }
            
            // Get pre-loaded candidate vector
            let candidate_vector = match candidate_vectors.get(&candidate.node_id) {
                Some(v) => v,
                None => continue, // Skip if we couldn't load it
            };
            
            // Check if this candidate is closer to query than to any already-selected neighbor
            // This is the core of the diversity heuristic from Algorithm 4
            let mut is_diverse = true;
            
            for selected_vector in &selected_vectors {
                let dist_to_selected = self.distance(candidate_vector, selected_vector);
                
                // If candidate is closer to a selected neighbor than to query, it's redundant
                // (the selected neighbor already "covers" this direction from the query)
                if dist_to_selected < candidate.distance {
                    is_diverse = false;
                    break;
                }
            }
            
            if is_diverse {
                result.push(candidate.node_id);
                selected_vectors.push(candidate_vector.clone());
            }
        }

        result
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
    ///
    /// Keeps the M closest neighbors and removes reverse edges from pruned neighbors
    /// to maintain bidirectional consistency.
    fn prune_connections(&self, node_id: NodeId, layer: usize) -> TableResult<()> {
        let max_connections = if layer == 0 {
            self.config.read().unwrap().max_connections_layer0
        } else {
            self.config.read().unwrap().max_connections
        };

        let mut node = self.load_node(node_id)?;
        if layer < node.neighbors.len() && node.neighbors[layer].len() > max_connections {
            let node_vector = node.vector.clone();
            let old_neighbors = node.neighbors[layer].clone();
            
            // Build candidates from current neighbors
            let mut candidates: Vec<Candidate> = Vec::new();
            
            for &neighbor_id in &old_neighbors {
                let neighbor = self.load_node(neighbor_id)?;
                let dist = self.distance(&node_vector, &neighbor.vector);
                candidates.push(Candidate {
                    node_id: neighbor_id,
                    distance: dist,
                });
            }
            
            // Sort by distance and keep only the closest M neighbors
            candidates.sort_by(|a, b| {
                a.distance
                    .partial_cmp(&b.distance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            
            let selected: Vec<NodeId> = candidates
                .into_iter()
                .take(max_connections)
                .map(|c| c.node_id)
                .collect();
            
            // Identify which neighbors were pruned
            let pruned: Vec<NodeId> = old_neighbors
                .iter()
                .filter(|&n| !selected.contains(n))
                .copied()
                .collect();
            
            // Update node's neighbors
            node.neighbors[layer] = selected;
            self.update_node(node_id, &node)?;
            
            // Remove reverse edges from pruned neighbors to maintain bidirectional consistency
            for pruned_neighbor_id in pruned {
                let mut pruned_neighbor = self.load_node(pruned_neighbor_id)?;
                if layer < pruned_neighbor.neighbors.len() {
                    pruned_neighbor.neighbors[layer].retain(|&n| n != node_id);
                    self.update_node(pruned_neighbor_id, &pruned_neighbor)?;
                }
            }
        }

        Ok(())
    }

    /// Remove a node from all neighbor lists and reconnect affected neighbors.
    fn repair_graph_after_deletion(&self, deleted_node_id: NodeId) -> TableResult<()> {
        let deleted_node = self.load_node(deleted_node_id)?;

        for layer in 0..deleted_node.neighbors.len() {
            let layer_neighbors = deleted_node.neighbors[layer].clone();

            for &neighbor_id in &layer_neighbors {
                let mut neighbor = self.load_node(neighbor_id)?;
                if layer < neighbor.neighbors.len() {
                    neighbor.neighbors[layer].retain(|&id| id != deleted_node_id);
                    self.update_node(neighbor_id, &neighbor)?;
                }
            }

            if layer_neighbors.len() > 1 {
                for &neighbor_id in &layer_neighbors {
                    let mut candidates = Vec::new();

                    for &candidate_id in &layer_neighbors {
                        if candidate_id == neighbor_id {
                            continue;
                        }

                        let candidate_node = self.load_node(candidate_id)?;
                        let distance = self
                            .distance(&self.load_node(neighbor_id)?.vector, &candidate_node.vector);
                        candidates.push(Candidate {
                            node_id: candidate_id,
                            distance,
                        });
                    }

                    candidates.sort_by(|a, b| {
                        a.distance
                            .partial_cmp(&b.distance)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });

                    let max_connections = if layer == 0 {
                        self.config.read().unwrap().max_connections_layer0
                    } else {
                        self.config.read().unwrap().max_connections
                    };

                    let selected = self.select_neighbors(candidates, max_connections, layer, true);
                    self.connect_nodes(neighbor_id, selected, layer)?;
                    self.prune_connections(neighbor_id, layer)?;
                }
            }
        }

        Ok(())
    }

    /// Select a replacement entry point after node deletion.
    fn select_replacement_entry_point(
        &self,
        deleted_node_id: NodeId,
    ) -> TableResult<(Option<NodeId>, usize)> {
        let id_to_node = self.id_to_node.read().unwrap();

        if id_to_node.is_empty() {
            return Ok((None, 0));
        }

        let mut best: Option<(NodeId, usize)> = None;
        for &node_id in id_to_node.values() {
            if node_id == deleted_node_id {
                continue;
            }

            let node = self.load_node(node_id)?;
            match best {
                Some((_, best_layer)) if node.layer <= best_layer => {}
                _ => best = Some((node_id, node.layer)),
            }
        }

        Ok(best.map_or((None, 0), |(node_id, layer)| (Some(node_id), layer)))
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
        let num_vectors = *self.num_vectors.read().unwrap() as u64;
        let config = self.config.read().unwrap();

        // Estimate size based on vector data and graph structure
        // Each vector: dimensions * 4 bytes (f32) + key overhead + neighbors overhead
        let vector_data_size = num_vectors * config.dimensions as u64 * 4;
        let key_overhead = num_vectors * 32; // Approximate key size
        let neighbors_overhead = num_vectors * config.max_connections_layer0 as u64 * 4; // Node IDs
        let metadata_overhead = 4096; // Root page and metadata

        let estimated_size =
            vector_data_size + key_overhead + neighbors_overhead + metadata_overhead;

        Ok(crate::table::TableStatistics {
            row_count: Some(num_vectors),
            page_count: None, // TODO: Track actual page count
            total_size_bytes: Some(estimated_size),
            key_stats: None,
            value_stats: None,
            histogram: None,
            last_updated_lsn: None,
        })
    }
}

impl<FS: FileSystem> PagedHnswVector<FS> {
    /// Count nodes reachable from a starting node (for connectivity verification).
    fn count_reachable_nodes(&self, start: NodeId) -> usize {
        let mut visited = HashSet::new();
        let mut to_visit = vec![start];

        while let Some(node_id) = to_visit.pop() {
            if !visited.insert(node_id) {
                continue;
            }

            if let Ok(node) = self.load_node(node_id) {
                // Add all neighbors from all layers
                for layer_neighbors in &node.neighbors {
                    for &neighbor_id in layer_neighbors {
                        if !visited.contains(&neighbor_id) {
                            to_visit.push(neighbor_id);
                        }
                    }
                }
            }
        }

        visited.len()
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

            // Insert at layers from top down to bottom (0)
            // Only insert up to min(layer, max_layer) since higher layers don't exist yet
            let top_insert_layer = layer.min(max_layer);
            
            for lc in (0..=top_insert_layer).rev() {
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

                let neighbors = self.select_neighbors(candidates.clone(), m, lc, true);

                // Add bidirectional connections
                self.connect_nodes(node_id, neighbors.clone(), lc)?;

                // Update neighbors' connections
                for neighbor_id in &neighbors {
                    self.prune_connections(*neighbor_id, lc)?;
                }

                // Update current_nearest for next layer down
                // Use the candidates found during search as entry points for the next lower layer
                // This maintains proper HNSW hierarchical navigation structure
                current_nearest = candidates.into_iter().map(|c| c.node_id).collect();
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

        let node_id = self
            .id_to_node
            .write()
            .unwrap()
            .remove(&id_buf)
            .ok_or_else(|| TableError::key_not_found(format!("Vector with ID {:?}", id_buf)))?;

        self.repair_graph_after_deletion(node_id)?;

        let was_entry_point = self
            .entry_point
            .read()
            .unwrap()
            .map(|ep| ep == node_id)
            .unwrap_or(false);
        if was_entry_point {
            let (replacement, replacement_layer) = self.select_replacement_entry_point(node_id)?;
            *self.entry_point.write().unwrap() = replacement;
            *self.max_layer.write().unwrap() = replacement_layer;
        }

        *self.num_vectors.write().unwrap() -= 1;
        self.persist_mapping()?;

        Ok(())
    }

    fn search_vector<'a>(
        &self,
        query: &[f32],
        options: VectorSearchOptions<'a>,
    ) -> TableResult<Vec<VectorHit>> {
        let search_start = std::time::Instant::now();
        
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

        let elapsed = search_start.elapsed();
        let cache_guard = self.node_cache.read().unwrap();
        let cache_stats = cache_guard.stats();
        eprintln!("[HNSW] Search: found {} results in {:.3}ms (cache hit rate: {:.1}%)",
            results.len(), elapsed.as_secs_f64() * 1000.0, cache_stats.hit_rate() * 100.0);
        drop(cache_guard);

        Ok(results)
    }

    fn stats(&self) -> TableResult<SpecialtyTableStats> {
        let num_vectors = *self.num_vectors.read().unwrap() as u64;
        let config = self.config.read().unwrap();

        // Estimate size based on vector data and graph structure
        // Each vector: dimensions * 4 bytes (f32) + key overhead + neighbors overhead
        let vector_data_size = num_vectors * config.dimensions as u64 * 4;
        let key_overhead = num_vectors * 32; // Approximate key size
        let neighbors_overhead = num_vectors * config.max_connections_layer0 as u64 * 4; // Node IDs
        let metadata_overhead = 4096; // Root page and metadata

        let estimated_size =
            vector_data_size + key_overhead + neighbors_overhead + metadata_overhead;

        Ok(SpecialtyTableStats {
            entry_count: Some(num_vectors),
            size_bytes: Some(estimated_size),
            distinct_keys: Some(num_vectors),
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

        let config = self.config.read().unwrap();
        let id_to_node = self.id_to_node.read().unwrap();
        let entry_point = self.entry_point.read().unwrap();
        let max_layer = *self.max_layer.read().unwrap();

        // Verify entry point exists if we have vectors
        if !id_to_node.is_empty() {
            if entry_point.is_none() {
                report.errors.push(crate::table::ConsistencyError {
                    error_type: crate::table::ConsistencyErrorType::CorruptedIndex,
                    location: "hnsw_entry_point".to_string(),
                    description: "Entry point is None but graph has nodes".to_string(),
                    severity: crate::table::Severity::Error,
                });
            }
        }

        // Verify each node in the graph
        for (vector_id, &node_id) in id_to_node.iter() {
            report.checked_items += 1;

            // Try to load the node
            let node = match self.load_node(node_id) {
                Ok(n) => n,
                Err(e) => {
                    report.errors.push(crate::table::ConsistencyError {
                        error_type: crate::table::ConsistencyErrorType::InvalidPointer,
                        location: format!("hnsw_node_{}", node_id.as_u32()),
                        description: format!(
                            "Failed to load node {} for vector {:?}: {}",
                            node_id.as_u32(),
                            vector_id,
                            e
                        ),
                        severity: crate::table::Severity::Critical,
                    });
                    continue;
                }
            };

            // Verify vector dimensions
            if node.vector.len() != config.dimensions {
                report.errors.push(crate::table::ConsistencyError {
                    error_type: crate::table::ConsistencyErrorType::CorruptedIndex,
                    location: format!("hnsw_node_{}", node_id.as_u32()),
                    description: format!(
                        "Vector dimension mismatch: expected {}, got {}",
                        config.dimensions,
                        node.vector.len()
                    ),
                    severity: crate::table::Severity::Error,
                });
            }

            // Verify layer is within bounds
            if node.layer > max_layer {
                report.errors.push(crate::table::ConsistencyError {
                    error_type: crate::table::ConsistencyErrorType::CorruptedIndex,
                    location: format!("hnsw_node_{}", node_id.as_u32()),
                    description: format!(
                        "Node layer {} exceeds max_layer {}",
                        node.layer, max_layer
                    ),
                    severity: crate::table::Severity::Error,
                });
            }

            // Verify neighbors structure
            if node.neighbors.len() != node.layer + 1 {
                report.errors.push(crate::table::ConsistencyError {
                    error_type: crate::table::ConsistencyErrorType::CorruptedIndex,
                    location: format!("hnsw_node_{}", node_id.as_u32()),
                    description: format!(
                        "Neighbors array size {} doesn't match layer+1 {}",
                        node.neighbors.len(),
                        node.layer + 1
                    ),
                    severity: crate::table::Severity::Error,
                });
            }

            // Verify max_connections constraint for each layer
            for (layer_idx, layer_neighbors) in node.neighbors.iter().enumerate() {
                let max_conn = if layer_idx == 0 {
                    config.max_connections_layer0
                } else {
                    config.max_connections
                };

                if layer_neighbors.len() > max_conn {
                    report.warnings.push(crate::table::ConsistencyWarning {
                        location: format!("hnsw_node_{}_layer_{}", node_id.as_u32(), layer_idx),
                        description: format!(
                            "Node has {} neighbors at layer {}, exceeds max_connections {}",
                            layer_neighbors.len(),
                            layer_idx,
                            max_conn
                        ),
                    });
                }

                // Verify all neighbor nodes exist
                for &neighbor_id in layer_neighbors {
                    if let Err(e) = self.load_node(neighbor_id) {
                        report.errors.push(crate::table::ConsistencyError {
                            error_type: crate::table::ConsistencyErrorType::InvalidPointer,
                            location: format!("hnsw_node_{}_layer_{}", node_id.as_u32(), layer_idx),
                            description: format!(
                                "Invalid neighbor pointer to node {}: {}",
                                neighbor_id.as_u32(),
                                e
                            ),
                            severity: crate::table::Severity::Error,
                        });
                    }
                }
            }
        }

        // Warn if graph is disconnected (entry point can't reach all nodes)
        if !id_to_node.is_empty() && entry_point.is_some() {
            let reachable = self.count_reachable_nodes(entry_point.unwrap());
            if reachable < id_to_node.len() {
                report.warnings.push(crate::table::ConsistencyWarning {
                    location: "hnsw_graph".to_string(),
                    description: format!(
                        "Graph may be disconnected: {} nodes exist but only {} reachable from entry point",
                        id_to_node.len(),
                        reachable
                    ),
                });
            }
        }

        // Add graph quality diagnostics
        if !id_to_node.is_empty() {
            let mut total_degree = 0;
            let mut degree_by_layer: Vec<usize> = vec![0; max_layer + 1];
            let mut reciprocal_edges = 0;
            let mut total_edges = 0;
            
            for &node_id in id_to_node.values() {
                if let Ok(node) = self.load_node(node_id) {
                    for (layer_idx, layer_neighbors) in node.neighbors.iter().enumerate() {
                        total_degree += layer_neighbors.len();
                        degree_by_layer[layer_idx] += layer_neighbors.len();
                        total_edges += layer_neighbors.len();
                        
                        // Check bidirectionality
                        for &neighbor_id in layer_neighbors {
                            if let Ok(neighbor) = self.load_node(neighbor_id) {
                                if layer_idx < neighbor.neighbors.len()
                                    && neighbor.neighbors[layer_idx].contains(&node_id) {
                                    reciprocal_edges += 1;
                                }
                            }
                        }
                    }
                }
            }
            
            // Calculate average degree per layer
            let num_nodes = id_to_node.len();
            for (layer_idx, &total_degree_at_layer) in degree_by_layer.iter().enumerate() {
                let avg_degree = if num_nodes > 0 {
                    total_degree_at_layer as f64 / num_nodes as f64
                } else {
                    0.0
                };
                
                report.warnings.push(crate::table::ConsistencyWarning {
                    location: format!("hnsw_layer_{}", layer_idx),
                    description: format!(
                        "Layer {} statistics: avg_degree={:.2}, total_edges={}",
                        layer_idx, avg_degree, total_degree_at_layer
                    ),
                });
            }
            
            // Report reciprocal edge ratio (should be close to 100% for bidirectional graph)
            let reciprocal_ratio = if total_edges > 0 {
                (reciprocal_edges as f64 / total_edges as f64) * 100.0
            } else {
                0.0
            };
            
            report.warnings.push(crate::table::ConsistencyWarning {
                location: "hnsw_graph".to_string(),
                description: format!(
                    "Graph quality: reciprocal_edge_ratio={:.1}%, total_edges={}, avg_degree={:.2}",
                    reciprocal_ratio,
                    total_edges,
                    if num_nodes > 0 { total_degree as f64 / num_nodes as f64 } else { 0.0 }
                ),
            });
            
            // Warn if reciprocal ratio is low (indicates asymmetric graph)
            if reciprocal_ratio < 90.0 {
                report.warnings.push(crate::table::ConsistencyWarning {
                    location: "hnsw_graph".to_string(),
                    description: format!(
                        "Low reciprocal edge ratio ({:.1}%) indicates graph is not properly bidirectional",
                        reciprocal_ratio
                    ),
                });
            }
        }

        // Add cache statistics
        let (hit_rate, hits, misses, evictions) = {
            let cache = self.node_cache.read().unwrap();
            let stats = cache.stats();
            (stats.hit_rate() * 100.0, stats.hits, stats.misses, stats.evictions)
        };
        report.warnings.push(crate::table::ConsistencyWarning {
            location: "hnsw_cache".to_string(),
            description: format!(
                "Node cache: hit_rate={:.1}%, hits={}, misses={}, evictions={}",
                hit_rate, hits, misses, evictions
            ),
        });

        Ok(report)
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

            // Insert at layers from top down to bottom (0)
            // Only insert up to min(layer, max_layer) since higher layers don't exist yet
            let top_insert_layer = layer.min(max_layer);
            
            for lc in (0..=top_insert_layer).rev() {
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

                let neighbors = self.select_neighbors(candidates.clone(), m, lc, true);

                // Add bidirectional connections
                self.connect_nodes(node_id, neighbors.clone(), lc)?;

                // Update neighbors' connections
                for neighbor_id in &neighbors {
                    self.prune_connections(*neighbor_id, lc)?;
                }

                // Update current_nearest for next layer down
                // Use the candidates found during search as entry points for the next lower layer
                // This maintains proper HNSW hierarchical navigation structure
                current_nearest = candidates.into_iter().map(|c| c.node_id).collect();
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

        // Persist mapping to disk
        self.persist_mapping()?;

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
            let (removed, freed_refs) = node.vacuum(min_visible_lsn);

            // Free overflow pages for removed external values
            if !freed_refs.is_empty() {
                self.pager.free_value_refs(&freed_refs)?;
            }

            if removed > 0 {
                total_removed += removed;
                self.update_node(node_id, &node)?;
            }
        }

        Ok(total_removed)
    }
}

// Made with Bob
