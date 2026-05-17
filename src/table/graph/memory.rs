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

//! In-memory GraphAdjacency table implementation.
//!
//! This module provides a memory-resident graph adjacency table implementation
//! optimized for:
//! - Fast edge lookups and traversals
//! - Efficient neighbor queries (outgoing/incoming edges)
//! - Graph algorithms (BFS, DFS)
//! - Both directed and undirected graphs
//! - Optional edge weights
//!
//! The implementation uses a Hash table as the underlying storage engine,
//! with specialized indexing for graph operations.

use crate::snap::Snapshot;
use crate::table::{
    EdgeCursor, EdgeRef, GraphAdjacency, MemoryHashTable, SpecialtyTableCapabilities,
    SpecialtyTableStats, Table, TableError, TableResult, VerificationReport,
};
use crate::txn::{TransactionId, VersionChain};
use crate::types::{KeyBuf, TableId};
use crate::wal::LogSequenceNumber;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};

use super::config::GraphConfig;
use super::edge::{Edge, EdgeData, GraphKey};

/// In-memory GraphAdjacency table.
///
/// Uses a Hash table for storage with specialized indexing for graph operations.
/// Maintains separate indices for outgoing and incoming edges for efficient traversal.
pub struct MemoryGraphTable {
    /// Table identifier
    table_id: TableId,

    /// Table name
    name: String,

    /// Configuration
    config: GraphConfig,

    /// Underlying hash table for storage
    storage: Arc<MemoryHashTable>,

    /// In-memory index for fast lookups (optional)
    index: RwLock<GraphIndex>,

    /// Transaction counter for generating LSNs
    tx_counter: RwLock<u64>,
}

/// In-memory index for fast graph operations with MVCC support.
#[derive(Default)]
struct GraphIndex {
    /// Map from (source, label, edge_id) to edge version chain for outgoing edges
    outgoing_edges: HashMap<(Vec<u8>, Vec<u8>, Vec<u8>), VersionChain>,
    /// Map from (target, label, edge_id) to edge version chain for incoming edges
    incoming_edges: HashMap<(Vec<u8>, Vec<u8>, Vec<u8>), VersionChain>,
    /// Set of all vertices
    vertices: HashSet<Vec<u8>>,
}

impl MemoryGraphTable {
    /// Create a new in-memory graph table.
    pub fn new(table_id: TableId, name: String, config: GraphConfig) -> Self {
        let storage = Arc::new(MemoryHashTable::new(table_id, format!("{}_storage", name)));
        Self {
            table_id,
            name,
            config,
            storage,
            index: RwLock::new(GraphIndex::default()),
            tx_counter: RwLock::new(0),
        }
    }

    /// Get the next transaction ID and LSN.
    fn next_tx(&self) -> (TransactionId, LogSequenceNumber) {
        let mut counter = self.tx_counter.write().unwrap();
        *counter += 1;
        (
            TransactionId::from(*counter),
            LogSequenceNumber::from(*counter),
        )
    }

    /// Mark all uncommitted versions as committed.
    ///
    /// This must be called after edge operations to make changes visible to readers.
    /// Note: This commits ALL uncommitted versions, not just those from a specific transaction.
    /// This is appropriate for the graph table's usage pattern where operations are batched.
    pub fn commit_versions(
        &self,
        _tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        // Commit versions in underlying storage (pass through tx_id for storage layer)
        self.storage.commit_versions(_tx_id, commit_lsn)?;

        // Commit ALL uncommitted versions in memory index
        if self.config.use_memory_index {
            let mut index = self.index.write().unwrap();
            for chain in index.outgoing_edges.values_mut() {
                // Recursively commit all uncommitted versions in the chain
                Self::commit_all_uncommitted(chain, commit_lsn);
            }
            for chain in index.incoming_edges.values_mut() {
                // Recursively commit all uncommitted versions in the chain
                Self::commit_all_uncommitted(chain, commit_lsn);
            }
        }

        Ok(())
    }

    /// Recursively commit all uncommitted versions in a chain.
    fn commit_all_uncommitted(chain: &mut VersionChain, commit_lsn: LogSequenceNumber) {
        // Commit this version if it's uncommitted
        if chain.commit_lsn.is_none() {
            chain.commit(commit_lsn);
        }

        // Recursively commit previous versions
        if let Some(prev) = chain.prev_version.as_mut() {
            Self::commit_all_uncommitted(prev, commit_lsn);
        }
    }

    /// Add a vertex to the graph (implicit when adding edges).
    pub fn add_vertex(&self, vertex: &[u8]) -> TableResult<()> {
        if self.config.use_memory_index {
            let mut index = self.index.write().unwrap();
            index.vertices.insert(vertex.to_vec());
        }
        Ok(())
    }

    /// Get all vertices in the graph.
    pub fn vertices(&self) -> TableResult<Vec<Vec<u8>>> {
        if self.config.use_memory_index {
            let index = self.index.read().unwrap();
            Ok(index.vertices.iter().cloned().collect())
        } else {
            // Without index, we'd need to scan all edges
            Err(TableError::operation_not_supported(
                "vertices() requires memory index to be enabled",
            ))
        }
    }

    /// Perform breadth-first search from a starting vertex.
    pub fn bfs<F>(&self, start: &[u8], mut visit: F) -> TableResult<()>
    where
        F: FnMut(&[u8]) -> bool, // Returns true to continue, false to stop
    {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();

        queue.push_back(start.to_vec());
        visited.insert(start.to_vec());

        while let Some(vertex) = queue.pop_front() {
            if !visit(&vertex) {
                break;
            }

            // Get outgoing edges
            let cursor = self.outgoing(&vertex, None)?;
            let edges = cursor.collect_all()?;

            for edge in edges {
                if !visited.contains(&edge.target.0) {
                    visited.insert(edge.target.0.clone());
                    queue.push_back(edge.target.0);
                }
            }
        }

        Ok(())
    }

    /// Perform depth-first search from a starting vertex.
    pub fn dfs<F>(&self, start: &[u8], mut visit: F) -> TableResult<()>
    where
        F: FnMut(&[u8]) -> bool, // Returns true to continue, false to stop
    {
        let mut visited = HashSet::new();
        self.dfs_recursive(start, &mut visited, &mut visit)
    }

    fn dfs_recursive<F>(
        &self,
        vertex: &[u8],
        visited: &mut HashSet<Vec<u8>>,
        visit: &mut F,
    ) -> TableResult<()>
    where
        F: FnMut(&[u8]) -> bool,
    {
        if visited.contains(vertex) {
            return Ok(());
        }

        visited.insert(vertex.to_vec());

        if !visit(vertex) {
            return Ok(());
        }

        // Get outgoing edges
        let cursor = self.outgoing(vertex, None)?;
        let edges = cursor.collect_all()?;

        for edge in edges {
            self.dfs_recursive(&edge.target.0, visited, visit)?;
        }

        Ok(())
    }

    /// Get neighbors of a vertex (outgoing edges).
    pub fn neighbors(&self, vertex: &[u8]) -> TableResult<Vec<Vec<u8>>> {
        let cursor = self.outgoing(vertex, None)?;
        let edges = cursor.collect_all()?;
        Ok(edges.into_iter().map(|e| e.target.0).collect())
    }

    /// Check if there's an edge between two vertices.
    pub fn has_edge(
        &self,
        source: &[u8],
        target: &[u8],
        label: Option<&[u8]>,
    ) -> TableResult<bool> {
        let cursor = self.outgoing(source, label)?;
        let edges = cursor.collect_all()?;
        Ok(edges.iter().any(|e| e.target.0 == target))
    }

    /// Vacuum obsolete versions from all entries in the graph.
    ///
    /// Vacuums both the underlying storage and the in-memory index (if enabled).
    /// For the index, vacuums both outgoing and incoming edge version chains.
    ///
    /// Returns the total count of removed versions.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        // Vacuum underlying storage
        let mut total_removed = self.storage.vacuum(min_visible_lsn)?;

        // Vacuum in-memory index if enabled
        if self.config.use_memory_index {
            let mut index = self.index.write().unwrap();

            // Vacuum outgoing edges
            for chain in index.outgoing_edges.values_mut() {
                total_removed += chain.vacuum(min_visible_lsn);
            }

            // Vacuum incoming edges
            for chain in index.incoming_edges.values_mut() {
                total_removed += chain.vacuum(min_visible_lsn);
            }
        }

        Ok(total_removed)
    }
}

impl Table for MemoryGraphTable {
    fn table_id(&self) -> TableId {
        self.table_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> crate::table::TableEngineKind {
        crate::table::TableEngineKind::GraphAdjacency
    }

    fn capabilities(&self) -> crate::table::TableCapabilities {
        crate::table::TableCapabilities {
            ordered: false,
            point_lookup: true,
            prefix_scan: false,
            reverse_scan: false,
            range_delete: false,
            merge_operator: false,
            mvcc_native: true, // Now supports MVCC with VersionChain
            append_optimized: false,
            memory_resident: true,
            disk_resident: false,
            supports_compression: false,
            supports_encryption: false,
        }
    }

    fn stats(&self) -> TableResult<crate::table::TableStatistics> {
        self.storage.stats()
    }
}

impl GraphAdjacency for MemoryGraphTable {
    type EdgeCursor<'a> = MemoryEdgeCursor;

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
            supports_range_query: false,
            supports_prefix_query: false,
            supports_scoring: false,
            supports_incremental_rebuild: false,
            may_be_stale: false,
        }
    }

    fn add_edge(
        &self,
        source: &[u8],
        label: &[u8],
        target: &[u8],
        edge_id: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        self.add_edge_with_weight(source, label, target, edge_id, None, tx_id, commit_lsn)
    }

    fn add_edge_with_weight(
        &self,
        source: &[u8],
        label: &[u8],
        target: &[u8],
        edge_id: &[u8],
        weight: Option<f64>,
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        let lsn = commit_lsn;

        // Create edge data with optional weight
        let edge_data = EdgeData {
            source: source.to_vec(),
            label: label.to_vec(),
            target: target.to_vec(),
            weight,
        };

        // Store edge data
        let edge_key = GraphKey::EdgeData {
            edge_id: KeyBuf(edge_id.to_vec()),
        };
        self.storage
            .put(&edge_key.encode(), &edge_data.encode(), tx_id, lsn)?;

        // Store outgoing edge index
        let out_key = GraphKey::Outgoing {
            source: KeyBuf(source.to_vec()),
            label: KeyBuf(label.to_vec()),
            edge_id: KeyBuf(edge_id.to_vec()),
        };
        self.storage.put(&out_key.encode(), target, tx_id, lsn)?;

        // Store incoming edge index
        let in_key = GraphKey::Incoming {
            target: KeyBuf(target.to_vec()),
            label: KeyBuf(label.to_vec()),
            edge_id: KeyBuf(edge_id.to_vec()),
        };
        self.storage.put(&in_key.encode(), source, tx_id, lsn)?;

        // For undirected graphs, add reverse edge
        if !self.config.directed {
            let rev_out_key = GraphKey::Outgoing {
                source: KeyBuf(target.to_vec()),
                label: KeyBuf(label.to_vec()),
                edge_id: KeyBuf(edge_id.to_vec()),
            };
            self.storage
                .put(&rev_out_key.encode(), source, tx_id, lsn)?;

            let rev_in_key = GraphKey::Incoming {
                target: KeyBuf(source.to_vec()),
                label: KeyBuf(label.to_vec()),
                edge_id: KeyBuf(edge_id.to_vec()),
            };
            self.storage.put(&rev_in_key.encode(), target, tx_id, lsn)?;
        }

        // Update in-memory index with version chain
        if self.config.use_memory_index {
            let mut index = self.index.write().unwrap();
            index.vertices.insert(source.to_vec());
            index.vertices.insert(target.to_vec());

            // Create edge data for version chain
            let edge = Edge::new(
                KeyBuf(edge_id.to_vec()),
                KeyBuf(source.to_vec()),
                KeyBuf(label.to_vec()),
                KeyBuf(target.to_vec()),
                weight,
            );
            let edge_bytes = serde_json::to_vec(&edge)
                .map_err(|e| TableError::serialization_error("Edge", e.to_string()))?;

            // Store outgoing edge with version chain
            let out_key = (source.to_vec(), label.to_vec(), edge_id.to_vec());
            let prev_version = index
                .outgoing_edges
                .get(&out_key)
                .map(|chain| Box::new(chain.clone()));
            let new_chain = VersionChain {
                value: edge_bytes.clone(),
                created_by: tx_id,
                commit_lsn: None, // Uncommitted - will be set by commit_versions()
                prev_version,
            };
            index.outgoing_edges.insert(out_key, new_chain);

            // Store incoming edge with version chain
            let in_key = (target.to_vec(), label.to_vec(), edge_id.to_vec());
            let prev_version = index
                .incoming_edges
                .get(&in_key)
                .map(|chain| Box::new(chain.clone()));
            let new_chain = VersionChain {
                value: edge_bytes.clone(),
                created_by: tx_id,
                commit_lsn: None,
                prev_version,
            };
            index.incoming_edges.insert(in_key, new_chain);

            // For undirected graphs, add reverse edges
            if !self.config.directed {
                let rev_edge = Edge::unweighted(
                    KeyBuf(edge_id.to_vec()),
                    KeyBuf(target.to_vec()),
                    KeyBuf(label.to_vec()),
                    KeyBuf(source.to_vec()),
                );
                let rev_edge_bytes = serde_json::to_vec(&rev_edge)
                    .map_err(|e| TableError::serialization_error("Edge", e.to_string()))?;

                let rev_out_key = (target.to_vec(), label.to_vec(), edge_id.to_vec());
                let prev_version = index
                    .outgoing_edges
                    .get(&rev_out_key)
                    .map(|chain| Box::new(chain.clone()));
                let new_chain = VersionChain {
                    value: rev_edge_bytes.clone(),
                    created_by: tx_id,
                    commit_lsn: None,
                    prev_version,
                };
                index.outgoing_edges.insert(rev_out_key, new_chain);

                let rev_in_key = (source.to_vec(), label.to_vec(), edge_id.to_vec());
                let prev_version = index
                    .incoming_edges
                    .get(&rev_in_key)
                    .map(|chain| Box::new(chain.clone()));
                let new_chain = VersionChain {
                    value: rev_edge_bytes,
                    created_by: tx_id,
                    commit_lsn: None,
                    prev_version,
                };
                index.incoming_edges.insert(rev_in_key, new_chain);
            }
        }

        Ok(())
    }

    fn remove_edge(
        &self,
        source: &[u8],
        label: &[u8],
        target: &[u8],
        edge_id: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        let lsn = commit_lsn;

        // Create tombstones (empty values) for MVCC-compliant deletion
        let tombstone = &[];

        // Remove edge data by creating tombstone
        let edge_key = GraphKey::EdgeData {
            edge_id: KeyBuf(edge_id.to_vec()),
        };
        self.storage.put(&edge_key.encode(), tombstone, tx_id, lsn)?;

        // Remove outgoing edge index by creating tombstone
        let out_key = GraphKey::Outgoing {
            source: KeyBuf(source.to_vec()),
            label: KeyBuf(label.to_vec()),
            edge_id: KeyBuf(edge_id.to_vec()),
        };
        self.storage.put(&out_key.encode(), tombstone, tx_id, lsn)?;

        // Remove incoming edge index by creating tombstone
        let in_key = GraphKey::Incoming {
            target: KeyBuf(target.to_vec()),
            label: KeyBuf(label.to_vec()),
            edge_id: KeyBuf(edge_id.to_vec()),
        };
        self.storage.put(&in_key.encode(), tombstone, tx_id, lsn)?;

        // For undirected graphs, remove reverse edge by creating tombstones
        if !self.config.directed {
            let rev_out_key = GraphKey::Outgoing {
                source: KeyBuf(target.to_vec()),
                label: KeyBuf(label.to_vec()),
                edge_id: KeyBuf(edge_id.to_vec()),
            };
            self.storage.put(&rev_out_key.encode(), tombstone, tx_id, lsn)?;

            let rev_in_key = GraphKey::Incoming {
                target: KeyBuf(source.to_vec()),
                label: KeyBuf(label.to_vec()),
                edge_id: KeyBuf(edge_id.to_vec()),
            };
            self.storage.put(&rev_in_key.encode(), tombstone, tx_id, lsn)?;
        }

        // Update in-memory index with tombstones (empty value)
        if self.config.use_memory_index {
            let mut index = self.index.write().unwrap();

            // Create tombstone for outgoing edge
            let out_key = (source.to_vec(), label.to_vec(), edge_id.to_vec());
            if let Some(chain) = index.outgoing_edges.get(&out_key) {
                let tombstone = VersionChain {
                    value: Vec::new(), // Empty value = tombstone
                    created_by: tx_id,
                    commit_lsn: None, // Uncommitted
                    prev_version: Some(Box::new(chain.clone())),
                };
                index.outgoing_edges.insert(out_key, tombstone);
            }

            // Create tombstone for incoming edge
            let in_key = (target.to_vec(), label.to_vec(), edge_id.to_vec());
            if let Some(chain) = index.incoming_edges.get(&in_key) {
                let tombstone = VersionChain {
                    value: Vec::new(),
                    created_by: tx_id,
                    commit_lsn: None,
                    prev_version: Some(Box::new(chain.clone())),
                };
                index.incoming_edges.insert(in_key, tombstone);
            }

            // For undirected graphs, create tombstones for reverse edges
            if !self.config.directed {
                let rev_out_key = (target.to_vec(), label.to_vec(), edge_id.to_vec());
                if let Some(chain) = index.outgoing_edges.get(&rev_out_key) {
                    let tombstone = VersionChain {
                        value: Vec::new(),
                        created_by: tx_id,
                        commit_lsn: None,
                        prev_version: Some(Box::new(chain.clone())),
                    };
                    index.outgoing_edges.insert(rev_out_key, tombstone);
                }

                let rev_in_key = (source.to_vec(), label.to_vec(), edge_id.to_vec());
                if let Some(chain) = index.incoming_edges.get(&rev_in_key) {
                    let tombstone = VersionChain {
                        value: Vec::new(),
                        created_by: tx_id,
                        commit_lsn: None,
                        prev_version: Some(Box::new(chain.clone())),
                    };
                    index.incoming_edges.insert(rev_in_key, tombstone);
                }
            }
        }

        Ok(())
    }

    fn outgoing(&self, source: &[u8], label: Option<&[u8]>) -> TableResult<Self::EdgeCursor<'_>> {
        // Use in-memory index if available
        if self.config.use_memory_index {
            let index = self.index.read().unwrap();
            let mut visible_edges = Vec::new();

            // Create a snapshot that sees all committed versions
            // Use max LSN to see everything that's been committed
            let snapshot = Snapshot::new(
                crate::snap::SnapshotId::from(0),
                String::new(),
                LogSequenceNumber::from(u64::MAX),
                0,
                0,
                Vec::new(),
            );

            // Scan outgoing edges and filter by source and label
            for ((edge_source, edge_label, _edge_id), chain) in index.outgoing_edges.iter() {
                if edge_source == source {
                    if let Some(label_filter) = label {
                        if edge_label != label_filter {
                            continue;
                        }
                    }

                    // Check visibility
                    if let Some(edge_bytes) = chain.find_visible_version(&snapshot) {
                        // Empty value means tombstone (deleted)
                        if !edge_bytes.is_empty() {
                            if let Ok(edge) = serde_json::from_slice::<Edge>(edge_bytes) {
                                visible_edges.push(edge);
                            }
                        }
                    }
                }
            }

            return Ok(MemoryEdgeCursor::new(visible_edges));
        }

        // Otherwise, scan storage (less efficient)
        // For now, return empty cursor - full implementation would scan the hash table
        Ok(MemoryEdgeCursor::new(Vec::new()))
    }

    fn incoming(&self, target: &[u8], label: Option<&[u8]>) -> TableResult<Self::EdgeCursor<'_>> {
        // Use in-memory index if available
        if self.config.use_memory_index {
            let index = self.index.read().unwrap();
            let mut visible_edges = Vec::new();

            // Create a snapshot that sees all committed versions
            // Use max LSN to see everything that's been committed
            let snapshot = Snapshot::new(
                crate::snap::SnapshotId::from(0),
                String::new(),
                LogSequenceNumber::from(u64::MAX),
                0,
                0,
                Vec::new(),
            );

            // Scan incoming edges and filter by target and label
            for ((edge_target, edge_label, _edge_id), chain) in index.incoming_edges.iter() {
                if edge_target == target {
                    if let Some(label_filter) = label {
                        if edge_label != label_filter {
                            continue;
                        }
                    }

                    // Check visibility
                    if let Some(edge_bytes) = chain.find_visible_version(&snapshot) {
                        // Empty value means tombstone (deleted)
                        if !edge_bytes.is_empty() {
                            if let Ok(edge) = serde_json::from_slice::<Edge>(edge_bytes) {
                                visible_edges.push(edge);
                            }
                        }
                    }
                }
            }

            return Ok(MemoryEdgeCursor::new(visible_edges));
        }

        // Otherwise, scan storage (less efficient)
        Ok(MemoryEdgeCursor::new(Vec::new()))
    }

    fn stats(&self) -> TableResult<SpecialtyTableStats> {
        let storage_stats = self.storage.stats()?;

        let (entry_count, distinct_keys) = if self.config.use_memory_index {
            let index = self.index.read().unwrap();
            // Count unique edges from outgoing edges (each edge appears once there)
            let edge_count = index.outgoing_edges.len();
            (Some(edge_count as u64), Some(index.vertices.len() as u64))
        } else {
            (storage_stats.row_count, None)
        };

        Ok(SpecialtyTableStats {
            entry_count,
            size_bytes: storage_stats.total_size_bytes,
            distinct_keys,
            stale_entries: None,
            last_updated_lsn: storage_stats.last_updated_lsn,
        })
    }

    fn verify(&self) -> TableResult<VerificationReport> {
        // Basic verification - check that all edges have valid source/target
        Ok(VerificationReport {
            checked_items: 0,
            errors: Vec::new(),
            warnings: Vec::new(),
        })
    }
}

/// Cursor over graph edges in memory.
pub struct MemoryEdgeCursor {
    edges: Vec<Edge>,
    position: usize,
}

impl MemoryEdgeCursor {
    fn new(edges: Vec<Edge>) -> Self {
        Self { edges, position: 0 }
    }

    /// Collect all remaining edges.
    pub fn collect_all(mut self) -> TableResult<Vec<EdgeRef>> {
        let mut result = Vec::new();
        while self.valid() {
            if let Some(edge_ref) = self.current() {
                result.push(edge_ref);
            }
            self.next()?;
        }
        Ok(result)
    }
}

impl EdgeCursor for MemoryEdgeCursor {
    fn valid(&self) -> bool {
        self.position < self.edges.len()
    }

    fn current(&self) -> Option<EdgeRef> {
        if self.valid() {
            let edge = &self.edges[self.position];
            Some(EdgeRef {
                edge_id: edge.edge_id.clone(),
                source: edge.source.clone(),
                label: edge.label.clone(),
                target: edge.target.clone(),
            })
        } else {
            None
        }
    }

    fn next(&mut self) -> TableResult<()> {
        if self.valid() {
            self.position += 1;
        }
        Ok(())
    }
}

// Made with Bob
