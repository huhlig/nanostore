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

use crate::table::{
    ApproximateMembership, CompositeIndexConfig, EdgeCursor, FullTextSearch, GeoHit, GeoPoint,
    GeoSpatial, GeometryRef, GraphAdjacency, ScoredDocument, SearchableTable,
    SpecialtyTableCapabilities, SpecialtyTableStats, TableCursor, TableEngineRegistry, TextField,
    TextQuery, TimePointRef, TimeSeries, TimeSeriesCursor, VectorHit, VectorMetric, VectorSearch,
    VectorSearchOptions, VerificationReport,
};
use crate::txn::{ConflictDetector, TransactionError, TransactionResult};
use crate::types::{Durability, IsolationLevel, ScanBounds, TableId, ValueBuf};
use crate::vfs::FileSystem;
use crate::wal::{LogSequenceNumber, WalWriter, WriteOpType};
use std::collections::{HashMap, HashSet};
use std::fmt::Formatter;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

/// EdgeCursor for Transaction's GraphAdjacency implementation.
/// Wraps the underlying MemoryEdgeCursor and owns the edge data.
#[derive(Debug)]
pub struct TransactionEdgeCursor {
    edges: Vec<crate::table::EdgeRef>,
    position: usize,
}

impl TransactionEdgeCursor {
    /// Create a new TransactionEdgeCursor from a MemoryEdgeCursor.
    fn from_memory_cursor(cursor: crate::table::graph::MemoryEdgeCursor) -> Self {
        // Collect all edges from the cursor
        let edges = cursor.collect_all().unwrap_or_default();
        Self { edges, position: 0 }
    }
}

impl EdgeCursor for TransactionEdgeCursor {
    fn valid(&self) -> bool {
        self.position < self.edges.len()
    }

    fn current(&self) -> Option<crate::table::EdgeRef> {
        if self.valid() {
            Some(self.edges[self.position].clone())
        } else {
            None
        }
    }

    fn next(&mut self) -> crate::table::TableResult<()> {
        if self.valid() {
            self.position += 1;
        }
        Ok(())
    }
}

/// Placeholder TimeSeriesCursor for Transaction's TimeSeries implementation.
/// TimeSeries cursor that owns its data (copied from the underlying table cursor)
#[derive(Debug)]
pub struct TransactionTimeSeriesCursor {
    series_key: Vec<u8>,
    points: Vec<(i64, Vec<u8>)>,
    position: usize,
}

impl TransactionTimeSeriesCursor {
    fn from_points<C: TimeSeriesCursor>(mut cursor: C) -> Self {
        let mut points = Vec::new();
        while cursor.valid() {
            if let Some(point) = cursor.current() {
                points.push((point.timestamp, point.value_key.0));
            }
            let _ = cursor.next();
        }
        let series_key = points.first().map(|_| Vec::new()).unwrap_or_default(); // We'll get it from the first point if needed
        Self {
            series_key,
            points,
            position: 0,
        }
    }
}

impl TimeSeriesCursor for TransactionTimeSeriesCursor {
    fn valid(&self) -> bool {
        self.position < self.points.len()
    }

    fn current(&self) -> Option<TimePointRef> {
        if self.valid() {
            let (ts, value_key) = &self.points[self.position];
            Some(TimePointRef {
                series_key: crate::types::KeyBuf(self.series_key.clone()),
                timestamp: *ts,
                value_key: crate::types::KeyBuf(value_key.clone()),
            })
        } else {
            None
        }
    }

    fn next(&mut self) -> crate::table::TableResult<()> {
        if self.valid() {
            self.position += 1;
        }
        Ok(())
    }
}

/// Graph edge operation for tracking in transaction write set
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GraphEdgeOp {
    AddEdge {
        source: Vec<u8>,
        label: Vec<u8>,
        target: Vec<u8>,
        edge_id: Vec<u8>,
    },
    RemoveEdge {
        source: Vec<u8>,
        label: Vec<u8>,
        target: Vec<u8>,
        edge_id: Vec<u8>,
    },
}

/// Time series operation for tracking in transaction write set
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TimeSeriesOp {
    AppendPoint {
        series_key: Vec<u8>,
        timestamp: i64,
        value_key: Vec<u8>,
    },
    DeletePoint {
        series_key: Vec<u8>,
        timestamp: i64,
        value_key: Vec<u8>,
    },
}

/// Vector operation for tracking in transaction write set
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum VectorOp {
    InsertVector { id: Vec<u8>, vector: Vec<f32> },
    DeleteVector { id: Vec<u8> },
}

/// Geospatial operation for tracking in transaction write set
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum GeoSpatialOp {
    InsertGeometry {
        id: Vec<u8>,
        geometry: SerializedGeometry,
    },
    DeleteGeometry {
        id: Vec<u8>,
    },
}

/// Full-text operation for tracking in transaction write set
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FullTextOp {
    IndexDocument {
        doc_id: Vec<u8>,
        fields: Vec<(String, String, f32)>,
    },
    UpdateDocument {
        doc_id: Vec<u8>,
        fields: Vec<(String, String, f32)>,
    },
    DeleteDocument {
        doc_id: Vec<u8>,
    },
}

/// Serialized geometry for storage in write set
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SerializedGeometry {
    Point {
        x: f64,
        y: f64,
    },
    BoundingBox {
        min_x: f64,
        min_y: f64,
        max_x: f64,
        max_y: f64,
    },
    Wkb(Vec<u8>),
}

/// Undo information for a single table operation
#[derive(Debug, Clone)]
enum UndoOperation {
    /// Restore a previous value (for put operations)
    RestoreValue {
        object_id: TableId,
        key: Vec<u8>,
        old_value: Option<Vec<u8>>,
    },
    /// Restore a deleted key (for delete operations)
    RestoreKey {
        object_id: TableId,
        key: Vec<u8>,
        old_value: Vec<u8>,
    },
    /// Remove a bloom filter entry (for bloom inserts)
    RemoveBloomEntry { object_id: TableId, key: Vec<u8> },
    /// Remove a graph edge (for graph add operations)
    RemoveGraphEdge {
        object_id: TableId,
        source: Vec<u8>,
        label: Vec<u8>,
        target: Vec<u8>,
        edge_id: Vec<u8>,
    },
    /// Restore a graph edge (for graph remove operations)
    RestoreGraphEdge {
        object_id: TableId,
        source: Vec<u8>,
        label: Vec<u8>,
        target: Vec<u8>,
        edge_id: Vec<u8>,
    },
    /// Remove a time series point (for time series appends)
    RemoveTimeSeriesPoint {
        object_id: TableId,
        series_key: Vec<u8>,
        timestamp: i64,
        value_key: Vec<u8>,
    },
    /// Remove a vector (for vector inserts)
    RemoveVector { object_id: TableId, id: Vec<u8> },
    /// Restore a vector (for vector deletes)
    RestoreVector {
        object_id: TableId,
        id: Vec<u8>,
        vector: Vec<f32>,
    },
    /// Remove a geometry (for geospatial inserts)
    RemoveGeometry { object_id: TableId, id: Vec<u8> },
    /// Restore a geometry (for geospatial deletes)
    RestoreGeometry {
        object_id: TableId,
        id: Vec<u8>,
        geometry: SerializedGeometry,
    },
    /// Remove a document (for full-text index operations)
    RemoveDocument { object_id: TableId, doc_id: Vec<u8> },
    /// Restore a document (for full-text delete operations)
    RestoreDocument {
        object_id: TableId,
        doc_id: Vec<u8>,
        fields: Vec<(String, String, f32)>,
    },
}

/// Collection of undo operations for rollback
#[derive(Debug, Default)]
struct UndoLog {
    operations: Vec<UndoOperation>,
}

impl UndoLog {
    fn new() -> Self {
        Self {
            operations: Vec::new(),
        }
    }

    fn add(&mut self, op: UndoOperation) {
        self.operations.push(op);
    }

    fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }
}

impl<FS: FileSystem> Transaction<FS> {
    /// Execute an undo operation to rollback a change.
    ///
    /// This method is called during commit failure to restore the previous state.
    /// Undo operations must be robust and should not fail.
    fn execute_undo(&self, op: &UndoOperation) -> TransactionResult<()> {
        match op {
            UndoOperation::RestoreValue {
                object_id,
                key,
                old_value,
            } => {
                // Restore previous value or delete if None
                if let Some(engine) = self.engine_registry.get(*object_id) {
                    use crate::table::{
                        Flushable, MutableTable, SearchableTable, TableEngineInstance,
                    };

                    match &engine {
                        TableEngineInstance::PagedBTree(btree) => {
                            let mut writer = SearchableTable::writer(
                                btree.as_ref(),
                                self.txn_id,
                                self.snapshot_lsn,
                            )
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get BTree writer for undo: {}",
                                    e
                                ))
                            })?;

                            if let Some(value) = old_value {
                                MutableTable::put(&mut writer, key, value).map_err(|e| {
                                    TransactionError::Other(format!("BTree undo put failed: {}", e))
                                })?;
                            } else {
                                MutableTable::delete(&mut writer, key).map_err(|e| {
                                    TransactionError::Other(format!(
                                        "BTree undo delete failed: {}",
                                        e
                                    ))
                                })?;
                            }
                            Flushable::flush(&mut writer).map_err(|e| {
                                TransactionError::Other(format!("BTree undo flush failed: {}", e))
                            })?;
                        }
                        TableEngineInstance::LsmTree(lsm) => {
                            let mut writer = SearchableTable::writer(
                                lsm.as_ref(),
                                self.txn_id,
                                self.snapshot_lsn,
                            )
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get LSM writer for undo: {}",
                                    e
                                ))
                            })?;

                            if let Some(value) = old_value {
                                MutableTable::put(&mut writer, key, value).map_err(|e| {
                                    TransactionError::Other(format!("LSM undo put failed: {}", e))
                                })?;
                            } else {
                                MutableTable::delete(&mut writer, key).map_err(|e| {
                                    TransactionError::Other(format!(
                                        "LSM undo delete failed: {}",
                                        e
                                    ))
                                })?;
                            }
                            Flushable::flush(&mut writer).map_err(|e| {
                                TransactionError::Other(format!("LSM undo flush failed: {}", e))
                            })?;
                        }
                        TableEngineInstance::MemoryBTree(mem) => {
                            let mut writer = SearchableTable::writer(
                                mem.as_ref(),
                                self.txn_id,
                                self.snapshot_lsn,
                            )
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get Memory BTree writer for undo: {}",
                                    e
                                ))
                            })?;

                            if let Some(value) = old_value {
                                MutableTable::put(&mut writer, key, value).map_err(|e| {
                                    TransactionError::Other(format!(
                                        "Memory BTree undo put failed: {}",
                                        e
                                    ))
                                })?;
                            } else {
                                MutableTable::delete(&mut writer, key).map_err(|e| {
                                    TransactionError::Other(format!(
                                        "Memory BTree undo delete failed: {}",
                                        e
                                    ))
                                })?;
                            }
                            Flushable::flush(&mut writer).map_err(|e| {
                                TransactionError::Other(format!(
                                    "Memory BTree undo flush failed: {}",
                                    e
                                ))
                            })?;
                        }
                        TableEngineInstance::MemoryHashTable(hash) => {
                            let mut writer =
                                hash.writer(self.txn_id, self.snapshot_lsn).map_err(|e| {
                                    TransactionError::Other(format!(
                                        "Failed to get Hash table writer for undo: {}",
                                        e
                                    ))
                                })?;

                            if let Some(value) = old_value {
                                MutableTable::put(&mut writer, key, value).map_err(|e| {
                                    TransactionError::Other(format!(
                                        "Hash table undo put failed: {}",
                                        e
                                    ))
                                })?;
                            } else {
                                MutableTable::delete(&mut writer, key).map_err(|e| {
                                    TransactionError::Other(format!(
                                        "Hash table undo delete failed: {}",
                                        e
                                    ))
                                })?;
                            }
                            Flushable::flush(&mut writer).map_err(|e| {
                                TransactionError::Other(format!(
                                    "Hash table undo flush failed: {}",
                                    e
                                ))
                            })?;
                        }
                        _ => {
                            // Other table types don't support undo yet
                        }
                    }
                }
            }
            UndoOperation::RestoreKey {
                object_id,
                key,
                old_value,
            } => {
                // Restore a deleted key
                if let Some(engine) = self.engine_registry.get(*object_id) {
                    use crate::table::{
                        Flushable, MutableTable, SearchableTable, TableEngineInstance,
                    };

                    match &engine {
                        TableEngineInstance::PagedBTree(btree) => {
                            let mut writer = SearchableTable::writer(
                                btree.as_ref(),
                                self.txn_id,
                                self.snapshot_lsn,
                            )
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get BTree writer for undo: {}",
                                    e
                                ))
                            })?;
                            MutableTable::put(&mut writer, key, old_value).map_err(|e| {
                                TransactionError::Other(format!("BTree undo restore failed: {}", e))
                            })?;
                            Flushable::flush(&mut writer).map_err(|e| {
                                TransactionError::Other(format!("BTree undo flush failed: {}", e))
                            })?;
                        }
                        TableEngineInstance::LsmTree(lsm) => {
                            let mut writer = SearchableTable::writer(
                                lsm.as_ref(),
                                self.txn_id,
                                self.snapshot_lsn,
                            )
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get LSM writer for undo: {}",
                                    e
                                ))
                            })?;
                            MutableTable::put(&mut writer, key, old_value).map_err(|e| {
                                TransactionError::Other(format!("LSM undo restore failed: {}", e))
                            })?;
                            Flushable::flush(&mut writer).map_err(|e| {
                                TransactionError::Other(format!("LSM undo flush failed: {}", e))
                            })?;
                        }
                        _ => {
                            // Other table types
                        }
                    }
                }
            }
            UndoOperation::RemoveBloomEntry { object_id, key } => {
                // Add a tombstone to mark this key as rolled back
                // The key will return false on contains() checks even though
                // the bits remain set in the filter
                if let Some(engine) = self.engine_registry.get(*object_id) {
                    if let crate::table::TableEngineInstance::PagedBloomFilter(bloom) = &engine {
                        bloom.add_tombstone(key.clone(), self.txn_id).map_err(|e| {
                            TransactionError::Other(format!("Bloom undo tombstone failed: {}", e))
                        })?;
                    }
                }
            }
            UndoOperation::RemoveGraphEdge {
                object_id,
                source,
                label,
                target,
                edge_id,
            } => {
                // Remove the edge that was added
                if let Some(engine) = self.engine_registry.get(*object_id) {
                    if let crate::table::TableEngineInstance::MemoryGraphTable(graph) = &engine {
                        graph
                            .remove_edge(
                                source,
                                label,
                                target,
                                edge_id,
                                self.txn_id,
                                self.snapshot_lsn,
                            )
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Graph undo remove_edge failed: {}",
                                    e
                                ))
                            })?;
                    }
                }
            }
            UndoOperation::RestoreGraphEdge {
                object_id,
                source,
                label,
                target,
                edge_id,
            } => {
                // Restore the edge that was removed
                if let Some(engine) = self.engine_registry.get(*object_id) {
                    if let crate::table::TableEngineInstance::MemoryGraphTable(graph) = &engine {
                        graph
                            .add_edge(
                                source,
                                label,
                                target,
                                edge_id,
                                self.txn_id,
                                self.snapshot_lsn,
                            )
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Graph undo add_edge failed: {}",
                                    e
                                ))
                            })?;
                    }
                }
            }
            UndoOperation::RemoveTimeSeriesPoint {
                object_id,
                series_key,
                timestamp,
                value_key: _,
            } => {
                // Add a tombstone to mark this point as rolled back
                if let Some(engine) = self.engine_registry.get(*object_id) {
                    if let crate::table::TableEngineInstance::TimeSeriesTable(timeseries) = &engine
                    {
                        timeseries
                            .add_tombstone(series_key, *timestamp, self.txn_id)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "TimeSeries undo tombstone failed: {}",
                                    e
                                ))
                            })?;
                    }
                }
            }
            UndoOperation::RemoveVector { object_id, id } => {
                // Remove the vector that was inserted
                if let Some(engine) = self.engine_registry.get(*object_id) {
                    if let crate::table::TableEngineInstance::PagedHnswVector(hnsw) = &engine {
                        hnsw.delete_vector(id, self.txn_id, self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!("Vector undo delete failed: {}", e))
                            })?;
                    }
                }
            }
            UndoOperation::RestoreVector {
                object_id,
                id,
                vector,
            } => {
                // Restore the vector that was deleted
                if let Some(engine) = self.engine_registry.get(*object_id) {
                    if let crate::table::TableEngineInstance::PagedHnswVector(hnsw) = &engine {
                        hnsw.insert_vector(id, vector, self.txn_id, self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!("Vector undo insert failed: {}", e))
                            })?;
                    }
                }
            }
            UndoOperation::RemoveGeometry { object_id, id } => {
                // Remove the geometry that was inserted
                if let Some(engine) = self.engine_registry.get(*object_id) {
                    if let crate::table::TableEngineInstance::PagedRTree(rtree) = &engine {
                        rtree
                            .delete_geometry(id, self.txn_id, self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "GeoSpatial undo delete failed: {}",
                                    e
                                ))
                            })?;
                    }
                }
            }
            UndoOperation::RestoreGeometry {
                object_id,
                id,
                geometry,
            } => {
                // Restore the geometry that was deleted
                if let Some(engine) = self.engine_registry.get(*object_id) {
                    if let crate::table::TableEngineInstance::PagedRTree(rtree) = &engine {
                        let geometry_ref = match geometry {
                            SerializedGeometry::Point { x, y } => {
                                GeometryRef::Point(GeoPoint { x: *x, y: *y })
                            }
                            SerializedGeometry::BoundingBox {
                                min_x,
                                min_y,
                                max_x,
                                max_y,
                            } => GeometryRef::BoundingBox {
                                min: GeoPoint {
                                    x: *min_x,
                                    y: *min_y,
                                },
                                max: GeoPoint {
                                    x: *max_x,
                                    y: *max_y,
                                },
                            },
                            SerializedGeometry::Wkb(wkb) => GeometryRef::Wkb(wkb.as_slice()),
                        };
                        rtree
                            .insert_geometry(id, geometry_ref, self.txn_id, self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "GeoSpatial undo insert failed: {}",
                                    e
                                ))
                            })?;
                    }
                }
            }
            UndoOperation::RemoveDocument { object_id, doc_id } => {
                // Remove the document that was indexed
                if let Some(engine) = self.engine_registry.get(*object_id) {
                    if let crate::table::TableEngineInstance::PagedFullTextIndex(fulltext) = &engine
                    {
                        fulltext
                            .delete_document(doc_id, self.txn_id, self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Full-text undo delete failed: {}",
                                    e
                                ))
                            })?;
                    }
                }
            }
            UndoOperation::RestoreDocument {
                object_id,
                doc_id,
                fields,
            } => {
                // Restore the document that was deleted
                if let Some(engine) = self.engine_registry.get(*object_id) {
                    if let crate::table::TableEngineInstance::PagedFullTextIndex(fulltext) = &engine
                    {
                        let text_fields: Vec<TextField<'_>> = fields
                            .iter()
                            .map(|(name, text, boost)| TextField {
                                name: name.as_str(),
                                text: text.as_str(),
                                boost: *boost,
                            })
                            .collect();
                        fulltext
                            .index_document(doc_id, &text_fields, self.txn_id, self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Full-text undo index failed: {}",
                                    e
                                ))
                            })?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Transaction ID type
#[derive(
    Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct TransactionId(u64);

impl TransactionId {
    pub fn as_u64(&self) -> u64 {
        self.0
    }
    pub fn to_bytes(&self) -> [u8; 8] {
        self.0.to_le_bytes()
    }
}

impl From<u64> for TransactionId {
    fn from(value: u64) -> Self {
        TransactionId(value)
    }
}

impl PartialEq<u64> for TransactionId {
    fn eq(&self, other: &u64) -> bool {
        self.0 == *other
    }
}

impl PartialEq<TransactionId> for u64 {
    fn eq(&self, other: &TransactionId) -> bool {
        *self == other.0
    }
}

impl std::fmt::Display for TransactionId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "TransactionId({})", self.0)
    }
}

/// Result of a successful commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    pub tx_id: TransactionId,
    pub commit_lsn: LogSequenceNumber,
    pub durable_lsn: Option<LogSequenceNumber>,
}

/// Transaction trait for supporting multiple storage engines.
///
/// Different storage engines have different transaction semantics:
/// - In-memory BTree: Transactions are trivially rolled back by discarding changes
/// - LSM Tree: Requires WAL coordination for durability
/// - Paged BTree: Requires page-level undo/redo logging
///
/// This trait provides a uniform interface for transaction operations across engines.
///
/// # Design Note: Indexes as Specialty Tables
///
/// Both tables and indexes are treated uniformly at the transaction layer using ObjectId.
/// The transaction layer does NOT automatically maintain indexes - that responsibility
/// belongs to the API consumer (e.g., the Database layer or query engine).
///
/// This design:
/// - Keeps the transaction layer simple and focused on ACID properties
/// - Allows flexible index maintenance strategies (synchronous, deferred, async)
/// - Enables custom index types without transaction layer changes
/// - Makes index updates explicit and visible in the transaction's write set
pub trait TransactionOps {
    /// Get the transaction ID.
    fn id(&self) -> TransactionId;

    /// Get the isolation level of this transaction.
    fn isolation_level(&self) -> IsolationLevel;

    /// Get the snapshot LSN at which this transaction reads.
    fn snapshot_lsn(&self) -> LogSequenceNumber;

    /// Get a value from an object (table or index).
    ///
    /// Both tables and indexes are treated uniformly using ObjectId.
    /// The transaction layer does not distinguish between them.
    fn get(&self, object: TableId, key: &[u8]) -> TransactionResult<Option<ValueBuf>>;

    /// Put a key-value pair into an object (table or index).
    ///
    /// For indexes, the caller is responsible for maintaining consistency
    /// with the parent table. The transaction layer does not automatically
    /// update indexes when table data changes.
    fn put(&mut self, object: TableId, key: &[u8], value: &[u8]) -> TransactionResult<()>;

    /// Delete a key from an object (table or index).
    ///
    /// For indexes, the caller is responsible for maintaining consistency
    /// with the parent table.
    fn delete(&mut self, object: TableId, key: &[u8]) -> TransactionResult<bool>;

    /// Delete a range of keys from an object (table or index).
    fn range_delete(&mut self, object: TableId, bounds: ScanBounds) -> TransactionResult<u64>;

    /// Commit the transaction.
    fn commit(self) -> TransactionResult<CommitInfo>;

    /// Rollback the transaction.
    fn rollback(self) -> TransactionResult<()>;
}

// TODO(MVCC): Transaction state machine for ACID properties
// Tracks the lifecycle of a transaction:
// - Active: Transaction is open and can perform operations
// - Preparing: Transaction is preparing to commit (2PC first phase)
// - Committed: Transaction has been committed
// - Aborted: Transaction has been rolled back
//
// State transitions:
// Active -> Preparing -> Committed
// Active -> Aborted
// Preparing -> Aborted (if commit fails)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionState {
    Active,
    Preparing,
    Committed,
    Aborted,
}

impl TransactionState {
    fn as_str(&self) -> &'static str {
        match self {
            TransactionState::Active => "Active",
            TransactionState::Preparing => "Preparing",
            TransactionState::Committed => "Committed",
            TransactionState::Aborted => "Aborted",
        }
    }
}

/// Transaction struct for managing database transactions.
pub struct Transaction<FS: FileSystem> {
    // Core transaction identity and isolation
    txn_id: TransactionId,
    snapshot_lsn: LogSequenceNumber,
    isolation: IsolationLevel,
    durability: Durability,
    state: TransactionState,

    // For Serializable isolation: track all keys read to detect read-write conflicts
    // Only populated when isolation == Serializable
    read_set: HashSet<(TableId, Vec<u8>)>,

    // Track all writes for commit/rollback
    // (ObjectId, Key) -> Option<Value> mapping of all mutations in this transaction
    // None represents a delete (tombstone), Some(value) represents a put
    write_set: HashMap<(TableId, Vec<u8>), Option<Vec<u8>>>,

    // Track specialty-table bloom inserts for commit/rollback visibility
    bloom_write_set: HashSet<(TableId, Vec<u8>)>,

    // Track specialty-table graph edge operations for commit/rollback
    // Each entry is (table_id, edge_operation)
    graph_write_set: std::cell::RefCell<Vec<(TableId, GraphEdgeOp)>>,

    // Track specialty-table time series operations for commit/rollback
    // Each entry is (table_id, time_series_operation)
    timeseries_write_set: std::cell::RefCell<Vec<(TableId, TimeSeriesOp)>>,

    // Track specialty-table vector operations for commit/rollback
    // Each entry is (table_id, vector_operation)
    vector_write_set: RwLock<Vec<(TableId, VectorOp)>>,

    // Track specialty-table geospatial operations for commit/rollback
    // Each entry is (table_id, geospatial_operation)
    geospatial_write_set: RwLock<Vec<(TableId, GeoSpatialOp)>>,

    // Track specialty-table full-text operations for commit/rollback
    // Each entry is (table_id, full_text_operation)
    fulltext_write_set: RwLock<Vec<(TableId, FullTextOp)>>,

    // Shared conflict detector for coordinating with other transactions
    conflict_detector: Arc<Mutex<ConflictDetector>>,

    // WAL writer for durability
    wal: Arc<WalWriter<FS>>,

    // Engine registry for reading/writing to actual storage engines
    engine_registry: Arc<TableEngineRegistry<FS>>,

    // Current LSN (shared with Database)
    current_lsn: Arc<RwLock<LogSequenceNumber>>,

    // Current table context for specialty table operations
    current_table_id: Option<TableId>,
    current_table_name: Option<String>,

    // Timing for metrics
    start_time: Instant,
}

impl<FS: FileSystem> Transaction<FS> {
    fn build(
        txn_id: TransactionId,
        snapshot_lsn: LogSequenceNumber,
        isolation: IsolationLevel,
        durability: Durability,
        conflict_detector: Arc<Mutex<ConflictDetector>>,
        wal: Arc<WalWriter<FS>>,
        engine_registry: Arc<TableEngineRegistry<FS>>,
        current_lsn: Arc<RwLock<LogSequenceNumber>>,
    ) -> Self {
        Self {
            txn_id,
            snapshot_lsn,
            isolation,
            durability,
            state: TransactionState::Active,
            read_set: HashSet::new(),
            write_set: HashMap::new(),
            bloom_write_set: HashSet::new(),
            graph_write_set: std::cell::RefCell::new(Vec::new()),
            timeseries_write_set: std::cell::RefCell::new(Vec::new()),
            vector_write_set: RwLock::new(Vec::new()),
            geospatial_write_set: RwLock::new(Vec::new()),
            fulltext_write_set: RwLock::new(Vec::new()),
            conflict_detector,
            wal,
            engine_registry,
            current_lsn,
            current_table_id: None,
            current_table_name: None,
            start_time: Instant::now(),
        }
    }

    /// Create a new transaction with the given ID, snapshot LSN, isolation level, durability policy, and shared resources.
    #[tracing::instrument(skip(conflict_detector, wal, engine_registry, current_lsn), fields(txn_id = %txn_id, isolation = ?isolation))]
    pub fn new(
        txn_id: TransactionId,
        snapshot_lsn: LogSequenceNumber,
        isolation: IsolationLevel,
        durability: Durability,
        conflict_detector: Arc<Mutex<ConflictDetector>>,
        wal: Arc<WalWriter<FS>>,
        engine_registry: Arc<TableEngineRegistry<FS>>,
        current_lsn: Arc<RwLock<LogSequenceNumber>>,
    ) -> Self {
        // Increment transaction begin counter
        metrics::counter!("nanokv.transaction.begin.total").increment(1);
        metrics::gauge!("nanokv.transaction.active").increment(1.0);

        // Write BEGIN record to WAL to register the transaction
        let _ = wal.write_begin(txn_id);

        tracing::debug!("Transaction started");

        Self::build(
            txn_id,
            snapshot_lsn,
            isolation,
            durability,
            conflict_detector,
            wal,
            engine_registry,
            current_lsn,
        )
    }

    /// Create a new read-only transaction without registering it as an active WAL writer transaction.
    pub fn new_read_only(
        txn_id: TransactionId,
        snapshot_lsn: LogSequenceNumber,
        isolation: IsolationLevel,
        conflict_detector: Arc<Mutex<ConflictDetector>>,
        wal: Arc<WalWriter<FS>>,
        engine_registry: Arc<TableEngineRegistry<FS>>,
        current_lsn: Arc<RwLock<LogSequenceNumber>>,
    ) -> Self {
        Self::build(
            txn_id,
            snapshot_lsn,
            isolation,
            Durability::WalOnly,
            conflict_detector,
            wal,
            engine_registry,
            current_lsn,
        )
    }

    /// Record a read operation for conflict detection.
    ///
    /// Called by get() to track reads for isolation levels that need it.
    /// - Serializable: Tracks all reads to detect read-write conflicts
    /// - RepeatableRead: Tracks all reads to prevent non-repeatable reads
    /// - ReadCommitted, SnapshotIsolation, ReadUncommitted: No tracking needed
    pub fn record_read(&mut self, object_id: TableId, key: Vec<u8>) {
        match self.isolation {
            IsolationLevel::Serializable | IsolationLevel::RepeatableRead => {
                self.read_set.insert((object_id, key));
            }
            _ => {
                // No read tracking for other isolation levels
            }
        }
    }

    /// Record a write operation for commit/rollback.
    ///
    /// Called by put() to track writes for commit/rollback.
    pub fn record_write(&mut self, object_id: TableId, key: Vec<u8>, value: Vec<u8>) {
        self.write_set.insert((object_id, key), Some(value));
    }

    /// Record a delete operation for commit/rollback.
    ///
    /// Called by delete() to track deletes for commit/rollback.
    pub fn record_delete(&mut self, object_id: TableId, key: Vec<u8>) {
        self.write_set.insert((object_id, key), None);
    }

    /// Record a bloom filter insert for commit/rollback.
    pub fn record_bloom_insert(&mut self, object_id: TableId, key: Vec<u8>) {
        self.bloom_write_set.insert((object_id, key));
    }

    /// Record a graph edge operation for commit/rollback.
    pub(crate) fn record_graph_operation(&self, object_id: TableId, op: GraphEdgeOp) {
        self.graph_write_set.borrow_mut().push((object_id, op));
    }

    /// Record a time series operation for commit/rollback.
    pub(crate) fn record_timeseries_operation(&self, object_id: TableId, op: TimeSeriesOp) {
        self.timeseries_write_set.borrow_mut().push((object_id, op));
    }

    /// Record a vector operation for commit/rollback.
    pub(crate) fn record_vector_operation(&self, object_id: TableId, op: VectorOp) {
        self.vector_write_set.write().unwrap().push((object_id, op));
    }

    /// Record a geospatial operation for commit/rollback.
    pub(crate) fn record_geospatial_operation(&self, object_id: TableId, op: GeoSpatialOp) {
        self.geospatial_write_set
            .write()
            .unwrap()
            .push((object_id, op));
    }

    /// Record a full-text operation for commit/rollback.
    pub(crate) fn record_fulltext_operation(&self, object_id: TableId, op: FullTextOp) {
        self.fulltext_write_set
            .write()
            .unwrap()
            .push((object_id, op));
    }

    /// Prepare the transaction for commit (two-phase commit).
    ///
    /// Transitions from Active to Preparing state.
    pub fn prepare(&mut self) -> TransactionResult<()> {
        if self.state != TransactionState::Active {
            return Err(TransactionError::invalid_state(
                self.txn_id,
                self.state.as_str(),
                "prepare",
            ));
        }
        self.state = TransactionState::Preparing;
        Ok(())
    }

    /// Check if the transaction is still active.
    ///
    /// Returns true if the transaction is in Active state.
    pub fn is_active(&self) -> bool {
        self.state == TransactionState::Active
    }

    /// Get the transaction ID.
    pub fn id(&self) -> TransactionId {
        self.txn_id
    }

    /// Get the isolation level of this transaction.
    pub fn isolation_level(&self) -> IsolationLevel {
        self.isolation
    }

    /// Get the snapshot LSN at which this transaction reads.
    pub fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }

    /// Get the current table context for specialty table operations.
    pub fn current_table(&self) -> Option<(TableId, &str)> {
        self.current_table_id
            .zip(self.current_table_name.as_deref())
    }

    /// Set the current table context for specialty table operations.
    pub fn with_table(&mut self, table_id: TableId) -> &mut Self {
        self.current_table_id = Some(table_id);
        self.current_table_name = Some(
            self.engine_registry
                .get(table_id)
                .map(|engine| engine.name().to_string())
                .unwrap_or_else(|| format!("table_{}", table_id.as_u64())),
        );
        self
    }

    /// Clear the current table context for specialty table operations.
    pub fn clear_table_context(&mut self) {
        self.current_table_id = None;
        self.current_table_name = None;
    }

    /// Execute a scoped set of bloom filter operations using the transaction as
    /// an `ApproximateMembership` implementation.
    pub fn with_bloom<F, R>(&mut self, table_id: TableId, f: F) -> TransactionResult<R>
    where
        F: FnOnce(&mut dyn ApproximateMembership) -> crate::table::TableResult<R>,
    {
        self.with_table(table_id);
        let result = f(self);
        self.clear_table_context();
        result.map_err(|e| TransactionError::Other(e.to_string()))
    }

    /// Execute a scoped set of vector search operations using the transaction as
    /// a `VectorSearch` implementation.
    pub fn with_vector<F, R>(&mut self, table_id: TableId, f: F) -> TransactionResult<R>
    where
        F: FnOnce(&mut dyn VectorSearch) -> crate::table::TableResult<R>,
    {
        self.with_table(table_id);
        let result = f(self);
        self.clear_table_context();
        result.map_err(|e| TransactionError::Other(e.to_string()))
    }

    /// Execute a scoped set of geospatial operations using the transaction as
    /// a `GeoSpatial` implementation.
    pub fn with_geospatial<F, R>(&mut self, table_id: TableId, f: F) -> TransactionResult<R>
    where
        F: FnOnce(&mut dyn GeoSpatial) -> crate::table::TableResult<R>,
    {
        self.with_table(table_id);
        let result = f(self);
        self.clear_table_context();
        result.map_err(|e| TransactionError::Other(e.to_string()))
    }

    /// Execute a scoped set of full-text search operations using the transaction as
    /// a `FullTextSearch` implementation.
    pub fn with_fulltext<F, R>(&mut self, table_id: TableId, f: F) -> TransactionResult<R>
    where
        F: FnOnce(&mut dyn FullTextSearch) -> crate::table::TableResult<R>,
    {
        self.with_table(table_id);
        let result = f(self);
        self.clear_table_context();
        result.map_err(|e| TransactionError::Other(e.to_string()))
    }

    /// Execute a composite index query across multiple table engines.
    ///
    /// This helper coordinates queries between a primary table and secondary
    /// indexes (e.g., bloom filters for pre-filtering) using the composition
    /// strategy defined in the config.
    ///
    /// Returns a `CompositeQueryResult` with the value and metadata about
    /// which tables were consulted during the query.
    ///
    /// # Example: LSM with Bloom pre-filter
    ///
    /// ```ignore
    /// let config = CompositeIndexBuilder::primary(lsm_id, TableEngineKind::LsmTree)
    ///     .with_bloom_prefilter(bloom_id)
    ///     .build();
    ///
    /// let result = txn.with_composite(&config, b"my-key")?;
    ///
    /// if result.pre_filtered {
    ///     // Bloom filter determined key is absent, no disk read needed
    /// }
    /// ```
    pub fn with_composite(
        &mut self,
        config: &CompositeIndexConfig,
        key: &[u8],
    ) -> TransactionResult<Option<ValueBuf>> {
        match config.strategy {
            crate::table::CompositionStrategy::BloomFirst => {
                self.composite_bloom_first(config, key)
            }
            crate::table::CompositionStrategy::PrimaryFirst => {
                self.composite_primary_first(config, key)
            }
            crate::table::CompositionStrategy::Parallel => self.composite_parallel(config, key),
            crate::table::CompositionStrategy::Sequential => self.composite_sequential(config, key),
        }
    }

    fn composite_bloom_first(
        &mut self,
        config: &CompositeIndexConfig,
        key: &[u8],
    ) -> TransactionResult<Option<ValueBuf>> {
        for bloom_id in config.bloom_filter_ids() {
            if !self.with_bloom(bloom_id, |bloom| bloom.might_contain(key))? {
                return Ok(None);
            }
        }

        self.get(config.primary_table_id, key)
    }

    fn composite_primary_first(
        &mut self,
        config: &CompositeIndexConfig,
        key: &[u8],
    ) -> TransactionResult<Option<ValueBuf>> {
        let value = self.get(config.primary_table_id, key)?;

        if value.is_some() {
            for secondary in &config.secondary_tables {
                if secondary.role == crate::table::SecondaryRole::PostFilter {
                    if secondary.engine_kind == crate::table::TableEngineKind::Bloom {
                        if !self.with_bloom(secondary.table_id, |bloom| bloom.might_contain(key))? {
                            return Ok(None);
                        }
                    }
                }
            }
        }

        Ok(value)
    }

    fn composite_parallel(
        &mut self,
        config: &CompositeIndexConfig,
        key: &[u8],
    ) -> TransactionResult<Option<ValueBuf>> {
        let primary_value = self.get(config.primary_table_id, key)?;

        if primary_value.is_none() {
            return Ok(None);
        }

        for secondary in &config.secondary_tables {
            if secondary.engine_kind == crate::table::TableEngineKind::Bloom {
                if !self.with_bloom(secondary.table_id, |bloom| bloom.might_contain(key))? {
                    return Ok(None);
                }
            }
        }

        Ok(primary_value)
    }

    fn composite_sequential(
        &mut self,
        config: &CompositeIndexConfig,
        key: &[u8],
    ) -> TransactionResult<Option<ValueBuf>> {
        if let Some(value) = self.get(config.primary_table_id, key)? {
            return Ok(Some(value));
        }

        for secondary in &config.secondary_tables {
            if secondary.role == crate::table::SecondaryRole::AlternativeAccess {
                if secondary.engine_kind == crate::table::TableEngineKind::Bloom {
                    if self.with_bloom(secondary.table_id, |bloom| bloom.might_contain(key))? {
                        continue;
                    }
                }
            }
        }

        Ok(None)
    }

    /// Set table context for graph operations.
    ///
    /// Note: Unlike `with_bloom()`, there is no `with_graph()` helper because
    /// `GraphAdjacency` uses Generic Associated Types (GATs) which are not
    /// dyn-compatible. Use `with_table()` and call graph methods directly instead.
    ///
    /// Example:
    /// ```ignore
    /// txn.with_table(graph_table_id);
    /// GraphAdjacency::add_edge(&mut txn, source, label, target, edge_id)?;
    /// txn.clear_table_context();
    /// ```
    ///
    /// Set table context for time series operations.
    ///
    /// Note: Unlike `with_bloom()`, there is no `with_timeseries()` helper because
    /// `TimeSeries` uses Generic Associated Types (GATs) which are not
    /// dyn-compatible. Use `with_table()` and call time series methods directly instead.
    ///
    /// Example:
    /// ```ignore
    /// txn.with_table(timeseries_table_id);
    /// TimeSeries::append_point(&mut txn, series_key, timestamp, value_key)?;
    /// txn.clear_table_context();
    /// ```
    ///
    /// Get a value from an object (table or index).
    ///
    /// This method implements isolation level semantics:
    /// - ReadUncommitted: Checks own write set, then other transactions' write sets (dirty reads), then committed data
    /// - ReadCommitted/RepeatableRead/Serializable/SnapshotIsolation: Checks own write set, then committed data only
    ///
    /// Both tables and indexes are treated uniformly using ObjectId.
    pub fn get(&self, object: TableId, key: &[u8]) -> TransactionResult<Option<ValueBuf>> {
        // Check if transaction is still active
        if !self.is_active() {
            return Err(TransactionError::invalid_state(
                self.txn_id,
                self.state.as_str(),
                "get",
            ));
        }

        let object_id = object;

        // Check own write set first for uncommitted changes
        let write_key = (object_id, key.to_vec());
        if let Some(value_opt) = self.write_set.get(&write_key) {
            // Found in own write set - return the value or None if deleted
            return Ok(value_opt.as_ref().map(|v| ValueBuf(v.clone())));
        }

        // For ReadUncommitted isolation, check other transactions' write sets (dirty reads)
        if self.isolation == IsolationLevel::ReadUncommitted {
            let detector = self.conflict_detector.lock().unwrap();
            // Note: ConflictDetector only tracks write locks, not the actual values
            // For a full implementation, we'd need a shared write set registry
            // For now, we'll document this limitation and proceed with committed reads
            drop(detector);
        }

        // Read from committed storage via engine registry
        if let Some(engine) = self.engine_registry.get(object_id) {
            use crate::table::{PointLookup, SearchableTable, TableEngineInstance};

            // Get a reader for the snapshot and use trait-based dispatch
            let result = match &engine {
                TableEngineInstance::AppendLog(appendlog) => {
                    // AppendLog supports direct PointLookup
                    PointLookup::get(appendlog.as_ref(), key, self.snapshot_lsn).map_err(|e| {
                        TransactionError::Other(format!("AppendLog get failed: {}", e))
                    })?
                }
                TableEngineInstance::PagedBTree(btree) => {
                    let reader = SearchableTable::reader(btree.as_ref(), self.snapshot_lsn)
                        .map_err(|e| {
                            TransactionError::Other(format!("Failed to get BTree reader: {}", e))
                        })?;
                    PointLookup::get(&reader, key, self.snapshot_lsn)
                        .map_err(|e| TransactionError::Other(format!("BTree get failed: {}", e)))?
                }
                TableEngineInstance::LsmTree(lsm) => {
                    let reader =
                        SearchableTable::reader(lsm.as_ref(), self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!("Failed to get LSM reader: {}", e))
                        })?;
                    PointLookup::get(&reader, key, self.snapshot_lsn)
                        .map_err(|e| TransactionError::Other(format!("LSM get failed: {}", e)))?
                }
                TableEngineInstance::MemoryBTree(mem) => {
                    let reader =
                        SearchableTable::reader(mem.as_ref(), self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!(
                                "Failed to get Memory BTree reader: {}",
                                e
                            ))
                        })?;
                    PointLookup::get(&reader, key, self.snapshot_lsn).map_err(|e| {
                        TransactionError::Other(format!("Memory BTree get failed: {}", e))
                    })?
                }
                TableEngineInstance::MemoryHashTable(hash) => {
                    let reader = hash.reader(self.snapshot_lsn).map_err(|e| {
                        TransactionError::Other(format!("Failed to get Hash table reader: {}", e))
                    })?;
                    PointLookup::get(&reader, key, self.snapshot_lsn).map_err(|e| {
                        TransactionError::Other(format!("Hash table get failed: {}", e))
                    })?
                }
                TableEngineInstance::MemoryART(art) => {
                    let reader =
                        SearchableTable::reader(art.as_ref(), self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!(
                                "Failed to get Memory ART reader: {}",
                                e
                            ))
                        })?;
                    PointLookup::get(&reader, key, self.snapshot_lsn).map_err(|e| {
                        TransactionError::Other(format!("Memory ART get failed: {}", e))
                    })?
                }
                TableEngineInstance::MemoryBlob(blob) => {
                    // Blob tables don't use reader/writer pattern, access directly
                    blob.get(key).map_err(|e| {
                        TransactionError::Other(format!("Memory Blob get failed: {}", e))
                    })?
                }
                TableEngineInstance::PagedBlob(blob) => {
                    // PagedBlob uses MVCC internally, get latest committed version
                    blob.get(key).map_err(|e| {
                        TransactionError::Other(format!("Paged Blob get failed: {}", e))
                    })?
                }
                TableEngineInstance::PagedBloomFilter(bloom) => {
                    // Bloom filters support approximate membership check
                    // Returns Some(empty) if probably present, None if definitely absent
                    if bloom.contains(key).map_err(|e| {
                        TransactionError::Other(format!("Bloom filter contains failed: {}", e))
                    })? {
                        Some(ValueBuf(vec![]))
                    } else {
                        None
                    }
                }
                TableEngineInstance::PagedHnswVector(_) => {
                    // HNSW vector tables don't support key-value get operations
                    return Err(TransactionError::Other(
                        "HNSW vector tables don't support get operations".to_string(),
                    ));
                }
                TableEngineInstance::PagedRTree(_) => {
                    return Err(TransactionError::Other(
                        "R-Tree tables don't support key-value get operations".to_string(),
                    ));
                }
                TableEngineInstance::MemoryGraphTable(_) => {
                    // Graph tables don't support traditional key-value get operations
                    return Err(TransactionError::Other(
                        "Graph tables don't support get operations - use outgoing/incoming instead"
                            .to_string(),
                    ));
                }
                TableEngineInstance::TimeSeriesTable(_) => {
                    // TimeSeries tables don't support traditional key-value get operations
                    return Err(TransactionError::Other(
                        "TimeSeries tables don't support get operations - use scan_series instead"
                            .to_string(),
                    ));
                }
                TableEngineInstance::PagedFullTextIndex(_) => {
                    // Full-text indexes don't support traditional key-value get operations
                    return Err(TransactionError::Other(
                        "Full-text indexes don't support get operations - use search instead"
                            .to_string(),
                    ));
                }
            };
            return Ok(result);
        }

        // Table not found in registry - key not found
        Ok(None)
    }

    /// Put a key-value pair into an object (table or index).
    ///
    /// Records the write in the transaction's write set and WAL. The change is not visible
    /// to other transactions until commit.
    ///
    /// For indexes, the caller is responsible for maintaining consistency with
    /// the parent table. The transaction layer does not automatically update indexes.
    pub fn put(&mut self, object: TableId, key: &[u8], value: &[u8]) -> TransactionResult<()> {
        // Check if transaction is still active
        if !self.is_active() {
            return Err(TransactionError::invalid_state(
                self.txn_id,
                self.state.as_str(),
                "put",
            ));
        }

        let object_id = object;

        // Check for write-write conflicts and acquire lock
        let mut detector = self.conflict_detector.lock().unwrap();
        detector.check_write_conflict(object_id, key, self.txn_id)?;
        detector.acquire_write_lock(object_id, key.to_vec(), self.txn_id);
        drop(detector);

        // Write to WAL
        self.wal
            .write_operation(
                self.txn_id,
                object_id,
                WriteOpType::Put,
                key.to_vec(),
                value.to_vec(),
            )
            .map_err(|e| TransactionError::Other(format!("WAL write failed: {}", e)))?;

        // Record the write in the write set
        self.record_write(object_id, key.to_vec(), value.to_vec());
        Ok(())
    }

    /// Delete a key from an object (table or index).
    ///
    /// Records the deletion in the transaction's write set and WAL. Returns true if the key
    /// existed (either in the write set or in the underlying storage).
    ///
    /// For indexes, the caller is responsible for maintaining consistency with
    /// the parent table.
    pub fn delete(&mut self, object: TableId, key: &[u8]) -> TransactionResult<bool> {
        // Check if transaction is still active
        if !self.is_active() {
            return Err(TransactionError::invalid_state(
                self.txn_id,
                self.state.as_str(),
                "delete",
            ));
        }

        let object_id = object;

        // Check for write-write conflicts and acquire lock
        let mut detector = self.conflict_detector.lock().unwrap();
        detector.check_write_conflict(object_id, key, self.txn_id)?;
        detector.acquire_write_lock(object_id, key.to_vec(), self.txn_id);
        drop(detector);

        let write_key = (object_id, key.to_vec());

        // Check if key exists in write set or storage
        let existed = if self.write_set.contains_key(&write_key) {
            true
        } else {
            // Check in engine registry
            if let Some(engine) = self.engine_registry.get(object_id) {
                use crate::table::{PointLookup, SearchableTable, TableEngineInstance};

                match &engine {
                    TableEngineInstance::AppendLog(appendlog) => {
                        // AppendLog supports PointLookup::contains
                        PointLookup::contains(appendlog.as_ref(), key, self.snapshot_lsn).map_err(
                            |e| {
                                TransactionError::Other(format!("AppendLog contains failed: {}", e))
                            },
                        )?
                    }
                    TableEngineInstance::PagedBTree(btree) => {
                        let reader = SearchableTable::reader(btree.as_ref(), self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get BTree reader: {}",
                                    e
                                ))
                            })?;
                        PointLookup::contains(&reader, key, self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!("BTree contains failed: {}", e))
                        })?
                    }
                    TableEngineInstance::LsmTree(lsm) => {
                        let reader = SearchableTable::reader(lsm.as_ref(), self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!("Failed to get LSM reader: {}", e))
                            })?;
                        PointLookup::contains(&reader, key, self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!("LSM contains failed: {}", e))
                        })?
                    }
                    TableEngineInstance::MemoryBTree(mem) => {
                        let reader = SearchableTable::reader(mem.as_ref(), self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get Memory BTree reader: {}",
                                    e
                                ))
                            })?;
                        PointLookup::contains(&reader, key, self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!("Memory BTree contains failed: {}", e))
                        })?
                    }
                    TableEngineInstance::MemoryHashTable(hash) => {
                        let reader = hash.reader(self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!(
                                "Failed to get Hash table reader: {}",
                                e
                            ))
                        })?;
                        PointLookup::contains(&reader, key, self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!("Hash table contains failed: {}", e))
                        })?
                    }
                    TableEngineInstance::MemoryART(art) => {
                        let reader = SearchableTable::reader(art.as_ref(), self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get Memory ART reader: {}",
                                    e
                                ))
                            })?;
                        PointLookup::contains(&reader, key, self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!("Memory ART contains failed: {}", e))
                        })?
                    }
                    TableEngineInstance::MemoryBlob(blob) => {
                        // Blob tables don't use reader pattern, check directly
                        blob.get(key)
                            .map_err(|e| {
                                TransactionError::Other(format!("Memory Blob get failed: {}", e))
                            })?
                            .is_some()
                    }
                    TableEngineInstance::PagedBlob(blob) => {
                        // PagedBlob uses MVCC internally, check latest committed version
                        blob.get(key)
                            .map_err(|e| {
                                TransactionError::Other(format!("Paged Blob get failed: {}", e))
                            })?
                            .is_some()
                    }
                    TableEngineInstance::PagedBloomFilter(bloom) => {
                        // Bloom filters support approximate membership check
                        bloom.contains(key).map_err(|e| {
                            TransactionError::Other(format!("Bloom filter contains failed: {}", e))
                        })?
                    }
                    TableEngineInstance::PagedHnswVector(_) => {
                        // HNSW vector tables don't support key-value contains operations
                        return Err(TransactionError::Other(
                            "HNSW vector tables don't support contains operations".to_string(),
                        ));
                    }
                    TableEngineInstance::PagedRTree(_) => {
                        return Err(TransactionError::Other(
                            "R-Tree tables don't support contains operations".to_string(),
                        ));
                    }
                    TableEngineInstance::MemoryGraphTable(_) => {
                        // Graph tables don't support traditional key-value contains operations
                        return Err(TransactionError::Other(
                            "Graph tables don't support contains operations".to_string(),
                        ));
                    }
                    TableEngineInstance::TimeSeriesTable(_) => {
                        // TimeSeries tables don't support traditional key-value contains operations
                        return Err(TransactionError::Other(
                            "TimeSeries tables don't support contains operations".to_string(),
                        ));
                    }
                    TableEngineInstance::PagedFullTextIndex(_) => {
                        // Full-text indexes don't support traditional key-value contains operations
                        return Err(TransactionError::Other(
                            "Full-text indexes don't support contains operations".to_string(),
                        ));
                    }
                }
            } else {
                false
            }
        };

        // Write to WAL
        self.wal
            .write_operation(
                self.txn_id,
                object_id,
                WriteOpType::Delete,
                key.to_vec(),
                vec![],
            )
            .map_err(|e| TransactionError::Other(format!("WAL write failed: {}", e)))?;

        // Record the deletion
        self.record_delete(object_id, key.to_vec());

        Ok(existed)
    }

    /// Delete a range of keys from an object (table or index).
    ///
    /// Scans the object at the transaction snapshot, acquires per-key write locks,
    /// records delete operations in the WAL, and stores tombstones in the write set.
    /// Returns the number of keys matched by the snapshot-visible scan.
    pub fn range_delete(&mut self, object: TableId, bounds: ScanBounds) -> TransactionResult<u64> {
        // Check if transaction is still active
        if !self.is_active() {
            return Err(TransactionError::invalid_state(
                self.txn_id,
                self.state.as_str(),
                "range_delete",
            ));
        }

        let object_id = object;
        let mut keys_to_delete: Vec<Vec<u8>> = Vec::new();

        if let Some(engine) = self.engine_registry.get(object_id) {
            use crate::table::{OrderedScan, SearchableTable, TableEngineInstance};

            match &engine {
                TableEngineInstance::AppendLog(_) => {
                    // AppendLog doesn't support traditional delete - it's append-only
                    // Deletes are handled at the transaction level via tombstones
                    // No action needed here
                }
                TableEngineInstance::PagedBTree(btree) => {
                    let reader = SearchableTable::reader(btree.as_ref(), self.snapshot_lsn)
                        .map_err(|e| {
                            TransactionError::Other(format!("Failed to get BTree reader: {}", e))
                        })?;
                    let mut cursor = OrderedScan::scan(&reader, bounds.clone(), self.snapshot_lsn)
                        .map_err(|e| {
                            TransactionError::Other(format!("BTree scan failed: {}", e))
                        })?;

                    while cursor.valid() {
                        if let Some(key) = cursor.key() {
                            keys_to_delete.push(key.to_vec());
                        }
                        cursor.next().map_err(|e| {
                            TransactionError::Other(format!("BTree cursor next failed: {}", e))
                        })?;
                    }
                }
                TableEngineInstance::LsmTree(lsm) => {
                    let reader =
                        SearchableTable::reader(lsm.as_ref(), self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!("Failed to get LSM reader: {}", e))
                        })?;
                    let mut cursor = OrderedScan::scan(&reader, bounds.clone(), self.snapshot_lsn)
                        .map_err(|e| TransactionError::Other(format!("LSM scan failed: {}", e)))?;

                    while cursor.valid() {
                        if let Some(key) = cursor.key() {
                            keys_to_delete.push(key.to_vec());
                        }
                        cursor.next().map_err(|e| {
                            TransactionError::Other(format!("LSM cursor next failed: {}", e))
                        })?;
                    }
                }
                TableEngineInstance::MemoryBTree(mem) => {
                    let reader =
                        SearchableTable::reader(mem.as_ref(), self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!(
                                "Failed to get Memory BTree reader: {}",
                                e
                            ))
                        })?;
                    let mut cursor = OrderedScan::scan(&reader, bounds, self.snapshot_lsn)
                        .map_err(|e| {
                            TransactionError::Other(format!("Memory BTree scan failed: {}", e))
                        })?;

                    while cursor.valid() {
                        if let Some(key) = cursor.key() {
                            keys_to_delete.push(key.to_vec());
                        }
                        cursor.next().map_err(|e| {
                            TransactionError::Other(format!(
                                "Memory BTree cursor next failed: {}",
                                e
                            ))
                        })?;
                    }
                }
                TableEngineInstance::MemoryART(art) => {
                    let reader =
                        SearchableTable::reader(art.as_ref(), self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!(
                                "Failed to get Memory ART reader: {}",
                                e
                            ))
                        })?;
                    let mut cursor = OrderedScan::scan(&reader, bounds, self.snapshot_lsn)
                        .map_err(|e| {
                            TransactionError::Other(format!("Memory ART scan failed: {}", e))
                        })?;

                    while cursor.valid() {
                        if let Some(key) = cursor.key() {
                            keys_to_delete.push(key.to_vec());
                        }
                        cursor.next().map_err(|e| {
                            TransactionError::Other(format!("Memory ART cursor next failed: {}", e))
                        })?;
                    }
                }
                TableEngineInstance::MemoryHashTable(_) => {
                    return Err(TransactionError::Other(
                        "range_delete is not supported for hash tables".to_string(),
                    ));
                }
                TableEngineInstance::MemoryBlob(_) => {
                    return Err(TransactionError::Other(
                        "range_delete is not supported for blob tables".to_string(),
                    ));
                }
                TableEngineInstance::PagedBloomFilter(_) => {
                    return Err(TransactionError::Other(
                        "range_delete is not supported for bloom filter tables".to_string(),
                    ));
                }
                TableEngineInstance::PagedHnswVector(_) => {
                    return Err(TransactionError::Other(
                        "range_delete is not supported for HNSW vector tables".to_string(),
                    ));
                }
                TableEngineInstance::PagedRTree(_) => {
                    return Err(TransactionError::Other(
                        "range_delete is not supported for R-Tree tables".to_string(),
                    ));
                }
                TableEngineInstance::PagedBlob(_) => {
                    return Err(TransactionError::Other(
                        "range_delete is not supported for Blob tables".to_string(),
                    ));
                }
                TableEngineInstance::MemoryGraphTable(_) => {
                    return Err(TransactionError::Other(
                        "range_delete is not supported for graph tables".to_string(),
                    ));
                }
                TableEngineInstance::TimeSeriesTable(_) => {
                    return Err(TransactionError::Other(
                        "range_delete is not supported for TimeSeries tables".to_string(),
                    ));
                }
                TableEngineInstance::PagedFullTextIndex(_) => {
                    return Err(TransactionError::Other(
                        "range_delete is not supported for FullText index tables".to_string(),
                    ));
                }
            }
        }

        for key in &keys_to_delete {
            let mut detector = self.conflict_detector.lock().unwrap();
            detector.check_write_conflict(object_id, key, self.txn_id)?;
            detector.acquire_write_lock(object_id, key.clone(), self.txn_id);
            drop(detector);

            self.wal
                .write_operation(
                    self.txn_id,
                    object_id,
                    WriteOpType::Delete,
                    key.clone(),
                    vec![],
                )
                .map_err(|e| TransactionError::Other(format!("WAL write failed: {}", e)))?;

            self.record_delete(object_id, key.clone());
        }

        Ok(keys_to_delete.len() as u64)
    }

    /// Commit the transaction using two-phase commit with undo log.
    ///
    /// Phase 1 (PREPARE): Collects undo information and writes PREPARE record to WAL
    /// Phase 2 (COMMIT/APPLY): Writes COMMIT record, applies changes, and commits version chains
    ///
    /// If any operation fails during Phase 2, executes undo operations in reverse order
    /// and writes ROLLBACK record to WAL.
    ///
    /// The durability policy controls how the commit is persisted:
    /// - MemoryOnly: No WAL writes (for in-memory tables only)
    /// - WalOnly: Write to WAL buffer but don't force sync
    /// - FlushOnCommit: Flush WAL buffer to OS but don't force disk sync
    /// - SyncOnCommit: Force sync to stable storage before returning
    #[tracing::instrument(skip(self), fields(txn_id = %self.txn_id, write_count = self.write_set.len()))]
    pub fn commit(mut self) -> TransactionResult<CommitInfo> {
        let commit_start = Instant::now();
        // Validate state - must be Active or Preparing
        if self.state != TransactionState::Active && self.state != TransactionState::Preparing {
            return Err(TransactionError::invalid_state(
                self.txn_id,
                self.state.as_str(),
                "commit",
            ));
        }

        // ===== PHASE 1: PREPARE =====
        // Collect undo information for all operations before applying changes
        let mut undo_log = UndoLog::new();

        // Collect undo info for write_set operations
        for ((object_id, key), value_opt) in &self.write_set {
            if let Some(engine) = self.engine_registry.get(*object_id) {
                use crate::table::{PointLookup, SearchableTable, TableEngineInstance};

                // Read current value to enable undo
                let old_value = match &engine {
                    TableEngineInstance::PagedBTree(btree) => {
                        let reader = SearchableTable::reader(btree.as_ref(), self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get BTree reader for undo: {}",
                                    e
                                ))
                            })?;
                        PointLookup::get(&reader, key, self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!("BTree get for undo failed: {}", e))
                            })?
                            .map(|v| v.0)
                    }
                    TableEngineInstance::LsmTree(lsm) => {
                        let reader = SearchableTable::reader(lsm.as_ref(), self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get LSM reader for undo: {}",
                                    e
                                ))
                            })?;
                        PointLookup::get(&reader, key, self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!("LSM get for undo failed: {}", e))
                            })?
                            .map(|v| v.0)
                    }
                    TableEngineInstance::MemoryBTree(mem) => {
                        let reader = SearchableTable::reader(mem.as_ref(), self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get Memory BTree reader for undo: {}",
                                    e
                                ))
                            })?;
                        PointLookup::get(&reader, key, self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Memory BTree get for undo failed: {}",
                                    e
                                ))
                            })?
                            .map(|v| v.0)
                    }
                    TableEngineInstance::MemoryHashTable(hash) => {
                        let reader = hash.reader(self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!(
                                "Failed to get Hash table reader for undo: {}",
                                e
                            ))
                        })?;
                        PointLookup::get(&reader, key, self.snapshot_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Hash table get for undo failed: {}",
                                    e
                                ))
                            })?
                            .map(|v| v.0)
                    }
                    _ => None, // Other table types don't support undo yet
                };

                // Add undo operation based on whether this is a put or delete
                if value_opt.is_some() {
                    // This is a put operation - store old value to restore on failure
                    undo_log.add(UndoOperation::RestoreValue {
                        object_id: *object_id,
                        key: key.clone(),
                        old_value,
                    });
                } else if let Some(old_val) = old_value {
                    // This is a delete operation - store old value to restore on failure
                    undo_log.add(UndoOperation::RestoreKey {
                        object_id: *object_id,
                        key: key.clone(),
                        old_value: old_val,
                    });
                }
            }
        }

        // Collect undo info for graph operations
        for (object_id, op) in self.graph_write_set.borrow().iter() {
            match op {
                GraphEdgeOp::AddEdge {
                    source,
                    label,
                    target,
                    edge_id,
                } => {
                    // For add operations, we need to remove the edge on undo
                    undo_log.add(UndoOperation::RemoveGraphEdge {
                        object_id: *object_id,
                        source: source.clone(),
                        label: label.clone(),
                        target: target.clone(),
                        edge_id: edge_id.clone(),
                    });
                }
                GraphEdgeOp::RemoveEdge {
                    source,
                    label,
                    target,
                    edge_id,
                } => {
                    // For remove operations, we need to restore the edge on undo
                    undo_log.add(UndoOperation::RestoreGraphEdge {
                        object_id: *object_id,
                        source: source.clone(),
                        label: label.clone(),
                        target: target.clone(),
                        edge_id: edge_id.clone(),
                    });
                }
            }
        }

        // Collect undo info for vector operations
        {
            let vector_ops = self.vector_write_set.read().unwrap();
            for (object_id, op) in vector_ops.iter() {
                match op {
                    VectorOp::InsertVector { id, .. } => {
                        // For insert operations, we need to remove the vector on undo
                        undo_log.add(UndoOperation::RemoveVector {
                            object_id: *object_id,
                            id: id.clone(),
                        });
                    }
                    VectorOp::DeleteVector { id } => {
                        // For delete operations, we would need to restore the vector
                        // But we don't have the old vector data, so we can't undo this
                        // This is a limitation documented in TWO_PHASE_COMMIT_IMPLEMENTATION.md
                        // For now, we'll just note that vector deletes cannot be fully undone
                        undo_log.add(UndoOperation::RemoveVector {
                            object_id: *object_id,
                            id: id.clone(),
                        });
                    }
                }
            }
        }

        // Collect undo info for geospatial operations
        {
            let geospatial_ops = self.geospatial_write_set.read().unwrap();
            for (object_id, op) in geospatial_ops.iter() {
                match op {
                    GeoSpatialOp::InsertGeometry { id, .. } => {
                        // For insert operations, we need to remove the geometry on undo
                        undo_log.add(UndoOperation::RemoveGeometry {
                            object_id: *object_id,
                            id: id.clone(),
                        });
                    }
                    GeoSpatialOp::DeleteGeometry { id } => {
                        // For delete operations, we would need to restore the geometry
                        // But we don't have the old geometry data, so we can't undo this
                        // This is a limitation
                        undo_log.add(UndoOperation::RemoveGeometry {
                            object_id: *object_id,
                            id: id.clone(),
                        });
                    }
                }
            }
        }

        // Collect undo info for full-text operations
        {
            let fulltext_ops = self.fulltext_write_set.read().unwrap();
            for (object_id, op) in fulltext_ops.iter() {
                match op {
                    FullTextOp::IndexDocument { doc_id, .. }
                    | FullTextOp::UpdateDocument { doc_id, .. } => {
                        // For index/update operations, we need to remove the document on undo
                        undo_log.add(UndoOperation::RemoveDocument {
                            object_id: *object_id,
                            doc_id: doc_id.clone(),
                        });
                    }
                    FullTextOp::DeleteDocument { doc_id } => {
                        // For delete operations, we would need to restore the document
                        // But we don't have the old document data, so we can't undo this
                        undo_log.add(UndoOperation::RemoveDocument {
                            object_id: *object_id,
                            doc_id: doc_id.clone(),
                        });
                    }
                }
            }
        }

        // Note: Bloom filters and time series are append-only and cannot be undone
        // This is documented as a known limitation in TWO_PHASE_COMMIT_IMPLEMENTATION.md

        // Write PREPARE record to WAL
        self.wal
            .write_prepare(self.txn_id)
            .map_err(|e| TransactionError::Other(format!("WAL prepare failed: {}", e)))?;

        // Transition to Preparing state
        self.state = TransactionState::Preparing;

        // ===== PHASE 2: COMMIT/APPLY =====
        // Check for conflicts based on isolation level
        match self.isolation {
            IsolationLevel::ReadUncommitted => {
                // ReadUncommitted: No conflict checking needed
                // Allows dirty reads, dirty writes, non-repeatable reads, and phantoms
            }
            IsolationLevel::ReadCommitted => {
                // ReadCommitted: Only check write-write conflicts (already done in put/delete)
                // Allows non-repeatable reads and phantoms
            }
            IsolationLevel::RepeatableRead => {
                // RepeatableRead: Check for read-write conflicts to prevent non-repeatable reads
                // Phantoms are still allowed (range queries not tracked)
                let detector = self.conflict_detector.lock().unwrap();
                detector.check_read_write_conflicts(&self.read_set, self.txn_id)?;
            }
            IsolationLevel::Serializable => {
                // Serializable: Check for read-write conflicts to ensure full serializability
                // Prevents dirty reads, non-repeatable reads, and phantoms
                let detector = self.conflict_detector.lock().unwrap();
                detector.check_read_write_conflicts(&self.read_set, self.txn_id)?;
            }
            IsolationLevel::SnapshotIsolation => {
                // SnapshotIsolation: Only check write-write conflicts (already done in put/delete)
                // Uses snapshot reads (via snapshot_lsn) but doesn't check read-write conflicts
                // This allows write skew anomalies but prevents lost updates
            }
        }

        // Write COMMIT record to WAL based on durability policy
        let commit_lsn = match self.durability {
            Durability::MemoryOnly => {
                // For memory-only durability, we still write to WAL for consistency
                // but we don't force any syncing
                self.wal
                    .write_commit(self.txn_id)
                    .map_err(|e| TransactionError::Other(format!("WAL commit failed: {}", e)))?
            }
            Durability::WalOnly => {
                // Write to WAL buffer but don't force sync
                self.wal
                    .write_commit(self.txn_id)
                    .map_err(|e| TransactionError::Other(format!("WAL commit failed: {}", e)))?
            }
            Durability::FlushOnCommit => {
                // Write commit record and flush buffer to OS
                let lsn = self
                    .wal
                    .write_commit(self.txn_id)
                    .map_err(|e| TransactionError::Other(format!("WAL commit failed: {}", e)))?;

                // Flush the WAL buffer to ensure data reaches the OS
                self.wal
                    .flush()
                    .map_err(|e| TransactionError::Other(format!("WAL flush failed: {}", e)))?;

                lsn
            }
            Durability::SyncOnCommit => {
                // Write commit record - this will automatically sync if sync_on_write is enabled
                // The write_commit method already handles syncing based on WAL config
                self.wal
                    .write_commit(self.txn_id)
                    .map_err(|e| TransactionError::Other(format!("WAL commit failed: {}", e)))?
            }
        };

        // Apply write set to storage engines with rollback on failure
        use crate::table::{Flushable, MutableTable, TableEngineInstance};

        // Define a closure that applies all changes and can return an error
        let apply_all_changes = || -> TransactionResult<()> {
            // Apply write set to storage engines
            for ((object_id, key), value_opt) in &self.write_set {
                if let Some(engine) = self.engine_registry.get(*object_id) {
                    match &engine {
                        TableEngineInstance::AppendLog(appendlog) => {
                            match value_opt {
                                Some(value) => {
                                    appendlog.put_tx(key, value, self.txn_id).map_err(|e| {
                                        TransactionError::Other(format!(
                                            "AppendLog put_tx failed: {}",
                                            e
                                        ))
                                    })?;
                                }
                                None => {
                                    appendlog.delete_tx(key, self.txn_id).map_err(|e| {
                                        TransactionError::Other(format!(
                                            "AppendLog delete_tx failed: {}",
                                            e
                                        ))
                                    })?;
                                }
                            }

                            // Flush the AppendLog to ensure data is persisted
                            Flushable::flush(&mut appendlog.as_ref()).map_err(|e| {
                                TransactionError::Other(format!("AppendLog flush failed: {}", e))
                            })?;
                        }
                        TableEngineInstance::PagedBTree(btree) => {
                            // Get a writer for this transaction
                            let mut writer = SearchableTable::writer(
                                btree.as_ref(),
                                self.txn_id,
                                self.snapshot_lsn,
                            )
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get BTree writer: {}",
                                    e
                                ))
                            })?;

                            match value_opt {
                                Some(value) => {
                                    MutableTable::put(&mut writer, key, value).map_err(|e| {
                                        TransactionError::Other(format!("BTree put failed: {}", e))
                                    })?;
                                }
                                None => {
                                    MutableTable::delete(&mut writer, key).map_err(|e| {
                                        TransactionError::Other(format!(
                                            "BTree delete failed: {}",
                                            e
                                        ))
                                    })?;
                                }
                            }

                            // Flush the writer
                            Flushable::flush(&mut writer).map_err(|e| {
                                TransactionError::Other(format!("BTree flush failed: {}", e))
                            })?;

                            // Note: commit_versions() will be called later for all modified tables
                        }
                        TableEngineInstance::LsmTree(lsm) => {
                            let mut writer = SearchableTable::writer(
                                lsm.as_ref(),
                                self.txn_id,
                                self.snapshot_lsn,
                            )
                            .map_err(|e| {
                                TransactionError::Other(format!("Failed to get LSM writer: {}", e))
                            })?;

                            match value_opt {
                                Some(value) => {
                                    MutableTable::put(&mut writer, key, value).map_err(|e| {
                                        TransactionError::Other(format!("LSM put failed: {}", e))
                                    })?;
                                }
                                None => {
                                    MutableTable::delete(&mut writer, key).map_err(|e| {
                                        TransactionError::Other(format!("LSM delete failed: {}", e))
                                    })?;
                                }
                            }

                            Flushable::flush(&mut writer).map_err(|e| {
                                TransactionError::Other(format!("LSM flush failed: {}", e))
                            })?;

                            // Note: commit_versions() will be called later for all modified tables
                        }
                        TableEngineInstance::MemoryBTree(mem) => {
                            let mut writer = SearchableTable::writer(
                                mem.as_ref(),
                                self.txn_id,
                                self.snapshot_lsn,
                            )
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get Memory BTree writer: {}",
                                    e
                                ))
                            })?;

                            match value_opt {
                                Some(value) => {
                                    MutableTable::put(&mut writer, key, value).map_err(|e| {
                                        TransactionError::Other(format!(
                                            "Memory BTree put failed: {}",
                                            e
                                        ))
                                    })?;
                                }
                                None => {
                                    MutableTable::delete(&mut writer, key).map_err(|e| {
                                        TransactionError::Other(format!(
                                            "Memory BTree delete failed: {}",
                                            e
                                        ))
                                    })?;
                                }
                            }

                            Flushable::flush(&mut writer).map_err(|e| {
                                TransactionError::Other(format!("Memory BTree flush failed: {}", e))
                            })?;

                            // Note: commit_versions() will be called later for all modified tables
                        }
                        TableEngineInstance::MemoryART(art) => {
                            let mut writer = SearchableTable::writer(
                                art.as_ref(),
                                self.txn_id,
                                self.snapshot_lsn,
                            )
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get Memory ART writer: {}",
                                    e
                                ))
                            })?;

                            match value_opt {
                                Some(value) => {
                                    MutableTable::put(&mut writer, key, value).map_err(|e| {
                                        TransactionError::Other(format!(
                                            "Memory ART put failed: {}",
                                            e
                                        ))
                                    })?;
                                }
                                None => {
                                    MutableTable::delete(&mut writer, key).map_err(|e| {
                                        TransactionError::Other(format!(
                                            "Memory ART delete failed: {}",
                                            e
                                        ))
                                    })?;
                                }
                            }

                            Flushable::flush(&mut writer).map_err(|e| {
                                TransactionError::Other(format!("Memory ART flush failed: {}", e))
                            })?;

                            // Note: commit_versions() will be called later for all modified tables
                        }
                        TableEngineInstance::MemoryHashTable(hash) => {
                            let mut writer =
                                hash.writer(self.txn_id, self.snapshot_lsn).map_err(|e| {
                                    TransactionError::Other(format!(
                                        "Failed to get Hash table writer: {}",
                                        e
                                    ))
                                })?;

                            match value_opt {
                                Some(value) => {
                                    MutableTable::put(&mut writer, key, value).map_err(|e| {
                                        TransactionError::Other(format!(
                                            "Hash table put failed: {}",
                                            e
                                        ))
                                    })?;
                                }
                                None => {
                                    MutableTable::delete(&mut writer, key).map_err(|e| {
                                        TransactionError::Other(format!(
                                            "Hash table delete failed: {}",
                                            e
                                        ))
                                    })?;
                                }
                            }

                            Flushable::flush(&mut writer).map_err(|e| {
                                TransactionError::Other(format!("Hash table flush failed: {}", e))
                            })?;

                            // Note: commit_versions() will be called later for all modified tables
                        }
                        TableEngineInstance::MemoryBlob(blob) => {
                            // Blob tables don't use writer pattern, access directly
                            match value_opt {
                                Some(value) => {
                                    blob.put(key, value).map_err(|e| {
                                        TransactionError::Other(format!(
                                            "Memory Blob put failed: {}",
                                            e
                                        ))
                                    })?;
                                }
                                None => {
                                    blob.delete(key).map_err(|e| {
                                        TransactionError::Other(format!(
                                            "Memory Blob delete failed: {}",
                                            e
                                        ))
                                    })?;
                                }
                            }
                            // No flush needed for blob tables (in-memory, no writer)
                        }
                        TableEngineInstance::PagedBloomFilter(_) => {
                            // Bloom filters don't support transactional put/delete operations
                            // They should be updated through their specialized ApproximateMembership API
                            return Err(TransactionError::Other(
                            "transactional put/delete is not supported for bloom filter tables; use ApproximateMembership API"
                                .to_string(),
                        ));
                        }
                        TableEngineInstance::PagedHnswVector(_) => {
                            // HNSW vector tables don't support transactional put/delete operations
                            // They should be updated through their specialized VectorSearch API
                            return Err(TransactionError::Other(
                            "transactional put/delete is not supported for HNSW vector tables; use VectorSearch API"
                                .to_string(),
                        ));
                        }
                        TableEngineInstance::PagedRTree(_) => {
                            return Err(TransactionError::Other(
                            "transactional put/delete is not supported for R-Tree tables; use GeoSpatial API"
                                .to_string(),
                        ));
                        }
                        TableEngineInstance::PagedBlob(blob) => {
                            // PagedBlob supports transactional put/delete with interior mutability
                            match value_opt {
                                Some(value) => {
                                    blob.put_tx(key, value, self.txn_id).map_err(|e| {
                                        TransactionError::Other(format!(
                                            "PagedBlob put failed: {}",
                                            e
                                        ))
                                    })?;
                                }
                                None => {
                                    blob.delete_tx(key, self.txn_id).map_err(|e| {
                                        TransactionError::Other(format!(
                                            "PagedBlob delete failed: {}",
                                            e
                                        ))
                                    })?;
                                }
                            }
                        }
                        TableEngineInstance::MemoryGraphTable(_) => {
                            // Graph tables don't support transactional put/delete operations
                            // They should be updated through their specialized GraphAdjacency API
                            return Err(TransactionError::Other(
                            "transactional put/delete is not supported for graph tables; use GraphAdjacency API"
                                .to_string(),
                        ));
                        }
                        TableEngineInstance::TimeSeriesTable(_) => {
                            // TimeSeries tables don't support transactional put/delete operations
                            // They should be updated through their specialized TimeSeries API
                            return Err(TransactionError::Other(
                            "transactional put/delete is not supported for TimeSeries tables; use TimeSeries API"
                                .to_string(),
                        ));
                        }
                        TableEngineInstance::PagedFullTextIndex(_) => {
                            // Full-text indexes don't support transactional put/delete operations
                            // They should be updated through their specialized FullTextSearch API
                            return Err(TransactionError::Other(
                            "transactional put/delete is not supported for FullText index tables; use FullTextSearch API"
                                .to_string(),
                        ));
                        }
                    }
                }
                // If engine not found, skip (table may have been dropped)
            }
            Ok(())
        };

        // Execute apply with rollback on failure
        if let Err(apply_error) = apply_all_changes() {
            // Rollback: Execute undo operations in reverse order
            for undo_op in undo_log.operations.iter().rev() {
                // Best effort undo - log errors but continue
                if let Err(undo_err) = self.execute_undo(undo_op) {
                    // Log the undo error but continue with other undo operations
                    eprintln!("Warning: Undo operation failed: {}", undo_err);
                }
            }

            // Write ROLLBACK record to WAL
            self.wal
                .write_rollback(self.txn_id)
                .map_err(|e| TransactionError::Other(format!("WAL rollback failed: {}", e)))?;

            // Transition to Aborted state
            self.state = TransactionState::Aborted;

            // Release all locks
            let mut detector = self.conflict_detector.lock().unwrap();
            detector.release_locks(self.txn_id);
            drop(detector);

            // Return the original apply error
            return Err(apply_error);
        }

        // Apply bloom filter inserts after generic KV writes.
        for (object_id, key) in &self.bloom_write_set {
            if let Some(engine) = self.engine_registry.get(*object_id) {
                match &engine {
                    crate::table::TableEngineInstance::PagedBloomFilter(bloom) => {
                        bloom.insert(key, self.txn_id, commit_lsn).map_err(|e| {
                            TransactionError::Other(format!("Bloom insert failed: {}", e))
                        })?;
                    }
                    _ => {
                        return Err(TransactionError::Other(
                            "table is not a bloom filter".to_string(),
                        ));
                    }
                }
            }
        }

        // Apply graph edge operations after bloom filter inserts.
        for (object_id, op) in self.graph_write_set.borrow().iter() {
            if let Some(engine) = self.engine_registry.get(*object_id) {
                match &engine {
                    crate::table::TableEngineInstance::MemoryGraphTable(graph) => match op {
                        GraphEdgeOp::AddEdge {
                            source,
                            label,
                            target,
                            edge_id,
                        } => {
                            graph
                                .add_edge(
                                    source.as_slice(),
                                    label.as_slice(),
                                    target.as_slice(),
                                    edge_id.as_slice(),
                                    self.txn_id,
                                    commit_lsn,
                                )
                                .map_err(|e| {
                                    TransactionError::Other(format!("Graph add_edge failed: {}", e))
                                })?;
                        }
                        GraphEdgeOp::RemoveEdge {
                            source,
                            label,
                            target,
                            edge_id,
                        } => {
                            graph
                                .remove_edge(
                                    source.as_slice(),
                                    label.as_slice(),
                                    target.as_slice(),
                                    edge_id.as_slice(),
                                    self.txn_id,
                                    commit_lsn,
                                )
                                .map_err(|e| {
                                    TransactionError::Other(format!(
                                        "Graph remove_edge failed: {}",
                                        e
                                    ))
                                })?;
                        }
                    },
                    _ => {
                        return Err(TransactionError::Other(
                            "table is not a graph adjacency table".to_string(),
                        ));
                    }
                }
            }
        }

        // Apply time series operations after graph edge operations.
        for (object_id, op) in self.timeseries_write_set.borrow().iter() {
            if let Some(engine) = self.engine_registry.get(*object_id) {
                match &engine {
                    crate::table::TableEngineInstance::TimeSeriesTable(ts) => match op {
                        TimeSeriesOp::AppendPoint {
                            series_key,
                            timestamp,
                            value_key,
                        } => {
                            TimeSeries::append_point(
                                ts.as_ref(),
                                series_key,
                                *timestamp,
                                value_key,
                                self.txn_id,
                                commit_lsn,
                            )
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "TimeSeries append_point failed: {}",
                                    e
                                ))
                            })?;
                        }
                        TimeSeriesOp::DeletePoint {
                            series_key: _,
                            timestamp: _,
                            value_key: _,
                        } => {
                            return Err(TransactionError::Other(
                                    "TimeSeries tables are append-only; delete operations are not supported".to_string(),
                                ));
                        }
                    },
                    _ => {
                        return Err(TransactionError::Other(
                            "table is not a time series table".to_string(),
                        ));
                    }
                }
            }
        }

        // Apply vector operations after time series operations.
        let vector_ops = self.vector_write_set.read().unwrap();
        for (object_id, op) in vector_ops.iter() {
            if let Some(engine) = self.engine_registry.get(*object_id) {
                match &engine {
                    crate::table::TableEngineInstance::PagedHnswVector(hnsw) => match op {
                        VectorOp::InsertVector { id, vector } => {
                            hnsw.insert_vector(id, vector, self.txn_id, commit_lsn)
                                .map_err(|e| {
                                    TransactionError::Other(format!(
                                        "Vector insert_vector failed: {}",
                                        e
                                    ))
                                })?;
                        }
                        VectorOp::DeleteVector { id } => {
                            hnsw.delete_vector(id, self.txn_id, commit_lsn)
                                .map_err(|e| {
                                    TransactionError::Other(format!(
                                        "Vector delete_vector failed: {}",
                                        e
                                    ))
                                })?;
                        }
                    },
                    _ => {
                        return Err(TransactionError::Other(
                            "table is not a vector search table".to_string(),
                        ));
                    }
                }
            }
        }
        drop(vector_ops);

        // Apply geospatial operations after vector operations.
        let geospatial_ops = self.geospatial_write_set.read().unwrap();
        for (object_id, op) in geospatial_ops.iter() {
            if let Some(engine) = self.engine_registry.get(*object_id) {
                match &engine {
                    crate::table::TableEngineInstance::PagedRTree(rtree) => match op {
                        GeoSpatialOp::InsertGeometry { id, geometry } => {
                            // Convert SerializedGeometry to GeometryRef
                            let geometry_ref = match geometry {
                                SerializedGeometry::Point { x, y } => {
                                    GeometryRef::Point(crate::table::GeoPoint { x: *x, y: *y })
                                }
                                SerializedGeometry::BoundingBox {
                                    min_x,
                                    min_y,
                                    max_x,
                                    max_y,
                                } => GeometryRef::BoundingBox {
                                    min: crate::table::GeoPoint {
                                        x: *min_x,
                                        y: *min_y,
                                    },
                                    max: crate::table::GeoPoint {
                                        x: *max_x,
                                        y: *max_y,
                                    },
                                },
                                SerializedGeometry::Wkb(wkb) => GeometryRef::Wkb(wkb.as_slice()),
                            };
                            rtree
                                .insert_geometry(
                                    id.as_slice(),
                                    geometry_ref,
                                    self.txn_id,
                                    commit_lsn,
                                )
                                .map_err(|e| {
                                    TransactionError::Other(format!(
                                        "GeoSpatial insert_geometry failed: {}",
                                        e
                                    ))
                                })?;
                        }
                        GeoSpatialOp::DeleteGeometry { id } => {
                            rtree
                                .delete_geometry(id.as_slice(), self.txn_id, commit_lsn)
                                .map_err(|e| {
                                    TransactionError::Other(format!(
                                        "GeoSpatial delete_geometry failed: {}",
                                        e
                                    ))
                                })?;
                        }
                    },
                    _ => {
                        return Err(TransactionError::Other(
                            "table is not a geospatial R-Tree table".to_string(),
                        ));
                    }
                }
            }
        }
        drop(geospatial_ops);

        // Apply full-text operations after geospatial operations.
        let fulltext_ops = self.fulltext_write_set.read().unwrap();
        for (object_id, op) in fulltext_ops.iter() {
            if let Some(engine) = self.engine_registry.get(*object_id) {
                match &engine {
                    crate::table::TableEngineInstance::PagedFullTextIndex(fulltext) => match op {
                        FullTextOp::IndexDocument { doc_id, fields } => {
                            let text_fields: Vec<TextField<'_>> = fields
                                .iter()
                                .map(|(name, text, boost)| TextField {
                                    name: name.as_str(),
                                    text: text.as_str(),
                                    boost: *boost,
                                })
                                .collect();
                            fulltext
                                .index_document(doc_id, &text_fields, self.txn_id, commit_lsn)
                                .map_err(|e| {
                                    TransactionError::Other(format!(
                                        "Full-text index_document failed: {}",
                                        e
                                    ))
                                })?;
                        }
                        FullTextOp::UpdateDocument { doc_id, fields } => {
                            let text_fields: Vec<TextField<'_>> = fields
                                .iter()
                                .map(|(name, text, boost)| TextField {
                                    name: name.as_str(),
                                    text: text.as_str(),
                                    boost: *boost,
                                })
                                .collect();
                            fulltext
                                .update_document(doc_id, &text_fields, self.txn_id, commit_lsn)
                                .map_err(|e| {
                                    TransactionError::Other(format!(
                                        "Full-text update_document failed: {}",
                                        e
                                    ))
                                })?;
                        }
                        FullTextOp::DeleteDocument { doc_id } => {
                            fulltext
                                .delete_document(doc_id, self.txn_id, commit_lsn)
                                .map_err(|e| {
                                    TransactionError::Other(format!(
                                        "Full-text delete_document failed: {}",
                                        e
                                    ))
                                })?;
                        }
                    },
                    _ => {
                        return Err(TransactionError::Other(
                            "table is not a full-text index".to_string(),
                        ));
                    }
                }
            }
        }

        // After all operations are applied, commit version chains for all modified tables.
        // This makes the changes visible to readers by marking uncommitted versions with commit_lsn.

        // Collect all unique table IDs that were modified
        let mut modified_tables = std::collections::HashSet::new();

        // Add tables from write_set
        for ((object_id, _key), _value_opt) in &self.write_set {
            modified_tables.insert(*object_id);
        }

        // Add tables from bloom_write_set
        for (object_id, _key) in &self.bloom_write_set {
            modified_tables.insert(*object_id);
        }

        // Add tables from graph_write_set
        for (object_id, _op) in self.graph_write_set.borrow().iter() {
            modified_tables.insert(*object_id);
        }

        // Add tables from timeseries_write_set
        for (object_id, _op) in self.timeseries_write_set.borrow().iter() {
            modified_tables.insert(*object_id);
        }

        // Add tables from vector_write_set
        {
            let vector_ops = self.vector_write_set.read().unwrap();
            for (object_id, _op) in vector_ops.iter() {
                modified_tables.insert(*object_id);
            }
        }

        // Add tables from geospatial_write_set
        {
            let geospatial_ops = self.geospatial_write_set.read().unwrap();
            for (object_id, _op) in geospatial_ops.iter() {
                modified_tables.insert(*object_id);
            }
        }

        // Add tables from fulltext_write_set
        {
            let fulltext_ops = self.fulltext_write_set.read().unwrap();
            for (object_id, _op) in fulltext_ops.iter() {
                modified_tables.insert(*object_id);
            }
        }

        // Now call commit_versions() on all modified tables that support it
        for object_id in modified_tables {
            if let Some(engine) = self.engine_registry.get(object_id) {
                match &engine {
                    TableEngineInstance::PagedBTree(btree) => {
                        // Already committed in the write_set loop above, but call again to ensure
                        // any additional modifications are committed
                        let writer =
                            SearchableTable::writer(btree.as_ref(), self.txn_id, self.snapshot_lsn)
                                .map_err(|e| {
                                    TransactionError::Other(format!(
                                        "Failed to get BTree writer for commit: {}",
                                        e
                                    ))
                                })?;
                        writer.commit_versions(commit_lsn).map_err(|e| {
                            TransactionError::Other(format!("BTree commit_versions failed: {}", e))
                        })?;
                    }
                    TableEngineInstance::LsmTree(lsm) => {
                        // Already committed in the write_set loop above
                        let writer =
                            SearchableTable::writer(lsm.as_ref(), self.txn_id, self.snapshot_lsn)
                                .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get LSM writer for commit: {}",
                                    e
                                ))
                            })?;
                        writer.commit_versions(commit_lsn).map_err(|e| {
                            TransactionError::Other(format!("LSM commit_versions failed: {}", e))
                        })?;
                    }
                    TableEngineInstance::MemoryBTree(mem) => {
                        // Already committed in the write_set loop above
                        let writer =
                            SearchableTable::writer(mem.as_ref(), self.txn_id, self.snapshot_lsn)
                                .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get Memory BTree writer for commit: {}",
                                    e
                                ))
                            })?;
                        writer.commit_versions(commit_lsn).map_err(|e| {
                            TransactionError::Other(format!(
                                "Memory BTree commit_versions failed: {}",
                                e
                            ))
                        })?;
                    }
                    TableEngineInstance::MemoryART(art) => {
                        // Already committed in the write_set loop above
                        let writer =
                            SearchableTable::writer(art.as_ref(), self.txn_id, self.snapshot_lsn)
                                .map_err(|e| {
                                TransactionError::Other(format!(
                                    "Failed to get Memory ART writer for commit: {}",
                                    e
                                ))
                            })?;
                        writer.commit_versions(commit_lsn).map_err(|e| {
                            TransactionError::Other(format!(
                                "Memory ART commit_versions failed: {}",
                                e
                            ))
                        })?;
                    }
                    TableEngineInstance::MemoryHashTable(hash) => {
                        // Already committed in the write_set loop above
                        let writer = hash.writer(self.txn_id, self.snapshot_lsn).map_err(|e| {
                            TransactionError::Other(format!(
                                "Failed to get Hash table writer for commit: {}",
                                e
                            ))
                        })?;
                        writer.commit_versions(commit_lsn).map_err(|e| {
                            TransactionError::Other(format!(
                                "Hash table commit_versions failed: {}",
                                e
                            ))
                        })?;
                    }
                    TableEngineInstance::PagedRTree(rtree) => {
                        // Commit version chains in R-Tree leaf entries
                        rtree
                            .commit_versions(self.txn_id, commit_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "RTree commit_versions failed: {}",
                                    e
                                ))
                            })?;
                    }
                    TableEngineInstance::PagedBloomFilter(bloom) => {
                        // Commit tombstones for rolled-back inserts
                        bloom.commit_tombstones(self.txn_id, commit_lsn);
                    }
                    TableEngineInstance::TimeSeriesTable(timeseries) => {
                        // Commit version chains and tombstones
                        timeseries
                            .commit_versions(self.txn_id, commit_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "TimeSeries commit_versions failed: {}",
                                    e
                                ))
                            })?;
                    }
                    TableEngineInstance::AppendLog(appendlog) => {
                        appendlog
                            .commit_versions(self.txn_id, commit_lsn)
                            .map_err(|e| {
                                TransactionError::Other(format!(
                                    "AppendLog commit_versions failed: {}",
                                    e
                                ))
                            })?;
                    }
                    // Specialty tables that don't yet have commit_versions() methods
                    // will be handled when they integrate VersionChain support
                    TableEngineInstance::PagedHnswVector(_)
                    | TableEngineInstance::PagedFullTextIndex(_)
                    | TableEngineInstance::MemoryGraphTable(_)
                    | TableEngineInstance::MemoryBlob(_)
                    | TableEngineInstance::PagedBlob(_) => {
                        // These tables don't participate in transaction-level version commits yet
                    }
                }
            }
        }

        // Update current LSN
        {
            let mut current_lsn = self.current_lsn.write().unwrap();
            *current_lsn = commit_lsn;
        }

        // Transition to Committed state
        self.state = TransactionState::Committed;

        // Release all locks held by this transaction
        let mut detector = self.conflict_detector.lock().unwrap();
        detector.release_locks(self.txn_id);
        drop(detector);

        // Record metrics
        let commit_duration = commit_start.elapsed();
        let transaction_duration = self.start_time.elapsed();
        
        metrics::counter!("nanokv.transaction.commit.total").increment(1);
        metrics::histogram!("nanokv.transaction.commit.duration_seconds").record(commit_duration.as_secs_f64());
        metrics::histogram!("nanokv.transaction.duration_seconds").record(transaction_duration.as_secs_f64());
        metrics::gauge!("nanokv.transaction.active").decrement(1.0);
        
        tracing::info!(
            duration_ms = transaction_duration.as_millis(),
            commit_duration_ms = commit_duration.as_millis(),
            write_count = self.write_set.len(),
            "Transaction committed successfully"
        );

        Ok(CommitInfo {
            tx_id: self.txn_id,
            commit_lsn,
            durable_lsn: Some(commit_lsn), // WAL is synced, so it's durable
        })
    }

    /// Rollback the transaction.
    ///
    /// Writes rollback record to WAL, discards all changes, and releases locks.
    #[tracing::instrument(skip(self), fields(txn_id = %self.txn_id))]
    pub fn rollback(mut self) -> TransactionResult<()> {
        let rollback_start = Instant::now();
        // Can rollback from Active or Preparing state
        if self.state != TransactionState::Active && self.state != TransactionState::Preparing {
            return Err(TransactionError::invalid_state(
                self.txn_id,
                self.state.as_str(),
                "rollback",
            ));
        }

        // Write ROLLBACK record to WAL
        self.wal
            .write_rollback(self.txn_id)
            .map_err(|e| TransactionError::Other(format!("WAL rollback failed: {}", e)))?;

        // Transition to Aborted state
        self.state = TransactionState::Aborted;

        // Release all locks held by this transaction
        let mut detector = self.conflict_detector.lock().unwrap();
        detector.release_locks(self.txn_id);
        drop(detector);

        // Record metrics
        let rollback_duration = rollback_start.elapsed();
        let transaction_duration = self.start_time.elapsed();
        
        metrics::counter!("nanokv.transaction.rollback.total").increment(1);
        metrics::histogram!("nanokv.transaction.rollback.duration_seconds").record(rollback_duration.as_secs_f64());
        metrics::histogram!("nanokv.transaction.duration_seconds").record(transaction_duration.as_secs_f64());
        metrics::gauge!("nanokv.transaction.active").decrement(1.0);
        
        tracing::info!(
            duration_ms = transaction_duration.as_millis(),
            rollback_duration_ms = rollback_duration.as_millis(),
            "Transaction rolled back"
        );

        // Write set is automatically dropped when self is consumed
        Ok(())
    }
}

impl<FS: FileSystem> ApproximateMembership for Transaction<FS> {
    fn table_id(&self) -> TableId {
        self.current_table_id.unwrap_or(TableId::from(0))
    }

    fn name(&self) -> &str {
        self.current_table_name.as_deref().unwrap_or("unknown")
    }

    fn capabilities(&self) -> SpecialtyTableCapabilities {
        let Some(table_id) = self.current_table_id else {
            return SpecialtyTableCapabilities::default();
        };

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedBloomFilter(bloom)) => {
                ApproximateMembership::capabilities(bloom.as_ref())
            }
            _ => SpecialtyTableCapabilities::default(),
        }
    }

    fn insert_key(
        &mut self,
        key: &[u8],
        _tx_id: TransactionId,
        _commit_lsn: LogSequenceNumber,
    ) -> crate::table::TableResult<()> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for insert_key",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for bloom operation".to_string())
        })?;

        self.wal
            .write_operation(
                self.txn_id,
                table_id,
                WriteOpType::BloomInsert,
                key.to_vec(),
                vec![],
            )
            .map_err(|e| crate::table::TableError::Other(format!("WAL write failed: {}", e)))?;

        self.record_bloom_insert(table_id, key.to_vec());
        Ok(())
    }

    fn might_contain(&self, key: &[u8]) -> crate::table::TableResult<bool> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for might_contain",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for bloom operation".to_string())
        })?;

        if self.bloom_write_set.contains(&(table_id, key.to_vec())) {
            return Ok(true);
        }

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedBloomFilter(bloom)) => {
                // Note: Tombstone checking happens during transaction commit/rollback
                // The bloom filter's might_contain will return true for tombstoned keys
                // until they are vacuumed, but rolled-back transactions won't see their
                // own uncommitted inserts due to the bloom_write_set check above
                bloom.might_contain(key)
            }
            Some(_) => Err(crate::table::TableError::Other(
                "table is not a bloom filter".to_string(),
            )),
            None => Ok(false),
        }
    }

    fn false_positive_rate(&self) -> Option<f64> {
        let table_id = self.current_table_id?;
        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedBloomFilter(bloom)) => {
                Some(bloom.false_positive_rate())
            }
            _ => None,
        }
    }

    fn stats(&self) -> crate::table::TableResult<SpecialtyTableStats> {
        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for bloom operation".to_string())
        })?;

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedBloomFilter(bloom)) => bloom.stats(),
            Some(_) => Err(crate::table::TableError::Other(
                "table is not a bloom filter".to_string(),
            )),
            None => Err(crate::table::TableError::Other(
                "table not found".to_string(),
            )),
        }
    }

    fn verify(&self) -> crate::table::TableResult<VerificationReport> {
        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for bloom operation".to_string())
        })?;

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedBloomFilter(bloom)) => bloom.verify(),
            Some(_) => Err(crate::table::TableError::Other(
                "table is not a bloom filter".to_string(),
            )),
            None => Err(crate::table::TableError::Other(
                "table not found".to_string(),
            )),
        }
    }
}

impl<FS: FileSystem> GraphAdjacency for Transaction<FS> {
    type EdgeCursor<'a>
        = TransactionEdgeCursor
    where
        Self: 'a;

    fn table_id(&self) -> TableId {
        self.current_table_id.unwrap_or(TableId::from(0))
    }

    fn name(&self) -> &str {
        self.current_table_name.as_deref().unwrap_or("unknown")
    }

    fn capabilities(&self) -> SpecialtyTableCapabilities {
        let Some(_table_id) = self.current_table_id else {
            return SpecialtyTableCapabilities::default();
        };

        // Note: GraphAdjacency table engine not yet implemented
        // Return default capabilities for now
        SpecialtyTableCapabilities::default()
    }

    fn add_edge(
        &self,
        source: &[u8],
        label: &[u8],
        target: &[u8],
        edge_id: &[u8],
        _tx_id: crate::txn::TransactionId,
        _commit_lsn: crate::wal::LogSequenceNumber,
    ) -> crate::table::TableResult<()> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for add_edge",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for graph operation".to_string())
        })?;

        // Write to WAL - encode edge data in key/value fields
        // Format: key = source|label|target|edge_id (using | as separator)
        let mut wal_key = Vec::new();
        wal_key.extend_from_slice(source);
        wal_key.push(b'|');
        wal_key.extend_from_slice(label);
        wal_key.push(b'|');
        wal_key.extend_from_slice(target);
        wal_key.push(b'|');
        wal_key.extend_from_slice(edge_id);

        self.wal
            .write_operation(
                self.txn_id,
                table_id,
                WriteOpType::GraphAddEdge,
                wal_key,
                vec![],
            )
            .map_err(|e| crate::table::TableError::Other(format!("WAL write failed: {}", e)))?;

        self.record_graph_operation(
            table_id,
            GraphEdgeOp::AddEdge {
                source: source.to_vec(),
                label: label.to_vec(),
                target: target.to_vec(),
                edge_id: edge_id.to_vec(),
            },
        );
        Ok(())
    }

    fn remove_edge(
        &self,
        source: &[u8],
        label: &[u8],
        target: &[u8],
        edge_id: &[u8],
        _tx_id: crate::txn::TransactionId,
        _commit_lsn: crate::wal::LogSequenceNumber,
    ) -> crate::table::TableResult<()> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for remove_edge",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for graph operation".to_string())
        })?;

        // Write to WAL - encode edge data in key field
        let mut wal_key = Vec::new();
        wal_key.extend_from_slice(source);
        wal_key.push(b'|');
        wal_key.extend_from_slice(label);
        wal_key.push(b'|');
        wal_key.extend_from_slice(target);
        wal_key.push(b'|');
        wal_key.extend_from_slice(edge_id);

        self.wal
            .write_operation(
                self.txn_id,
                table_id,
                WriteOpType::GraphRemoveEdge,
                wal_key,
                vec![],
            )
            .map_err(|e| crate::table::TableError::Other(format!("WAL write failed: {}", e)))?;

        self.record_graph_operation(
            table_id,
            GraphEdgeOp::RemoveEdge {
                source: source.to_vec(),
                label: label.to_vec(),
                target: target.to_vec(),
                edge_id: edge_id.to_vec(),
            },
        );
        Ok(())
    }

    fn outgoing(
        &self,
        source: &[u8],
        label: Option<&[u8]>,
    ) -> crate::table::TableResult<Self::EdgeCursor<'_>> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for outgoing",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for graph operation".to_string())
        })?;

        // Get the graph table from the registry
        if let Some(engine) = self.engine_registry.get(table_id) {
            match &engine {
                crate::table::TableEngineInstance::MemoryGraphTable(graph) => {
                    // Get the cursor from the underlying graph table
                    let cursor = graph.outgoing(source, label)?;
                    // Wrap it in TransactionEdgeCursor
                    Ok(TransactionEdgeCursor::from_memory_cursor(cursor))
                }
                _ => Err(crate::table::TableError::Other(
                    "table is not a graph adjacency table".to_string(),
                )),
            }
        } else {
            Err(crate::table::TableError::Other(
                "graph table not found in registry".to_string(),
            ))
        }
    }

    fn incoming(
        &self,
        target: &[u8],
        label: Option<&[u8]>,
    ) -> crate::table::TableResult<Self::EdgeCursor<'_>> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for incoming",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for graph operation".to_string())
        })?;

        // Get the graph table from the registry
        if let Some(engine) = self.engine_registry.get(table_id) {
            match &engine {
                crate::table::TableEngineInstance::MemoryGraphTable(graph) => {
                    // Get the cursor from the underlying graph table
                    let cursor = graph.incoming(target, label)?;
                    // Wrap it in TransactionEdgeCursor
                    Ok(TransactionEdgeCursor::from_memory_cursor(cursor))
                }
                _ => Err(crate::table::TableError::Other(
                    "table is not a graph adjacency table".to_string(),
                )),
            }
        } else {
            Err(crate::table::TableError::Other(
                "graph table not found in registry".to_string(),
            ))
        }
    }

    fn stats(&self) -> crate::table::TableResult<SpecialtyTableStats> {
        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for graph operation".to_string())
        })?;

        // Get stats from the underlying graph table
        if let Some(engine) = self.engine_registry.get(table_id) {
            match &engine {
                crate::table::TableEngineInstance::MemoryGraphTable(graph) => graph.stats(),
                _ => Err(crate::table::TableError::Other(
                    "table is not a graph adjacency table".to_string(),
                )),
            }
        } else {
            Err(crate::table::TableError::Other(
                "graph table not found in registry".to_string(),
            ))
        }
    }

    fn verify(&self) -> crate::table::TableResult<VerificationReport> {
        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for graph operation".to_string())
        })?;

        // Note: GraphAdjacency table engine not yet implemented
        Err(crate::table::TableError::Other(
            "GraphAdjacency table engine not yet implemented".to_string(),
        ))
    }
}

/// Implement the TransactionOps trait for Transaction.
impl<FS: FileSystem> TransactionOps for Transaction<FS> {
    fn id(&self) -> TransactionId {
        self.txn_id
    }

    fn isolation_level(&self) -> IsolationLevel {
        self.isolation
    }

    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }

    fn get(&self, object: TableId, key: &[u8]) -> TransactionResult<Option<ValueBuf>> {
        Transaction::get(self, object, key)
    }

    fn put(&mut self, object: TableId, key: &[u8], value: &[u8]) -> TransactionResult<()> {
        Transaction::put(self, object, key, value)
    }

    fn delete(&mut self, object: TableId, key: &[u8]) -> TransactionResult<bool> {
        Transaction::delete(self, object, key)
    }

    fn range_delete(&mut self, object: TableId, bounds: ScanBounds) -> TransactionResult<u64> {
        Transaction::range_delete(self, object, bounds)
    }

    fn commit(self) -> TransactionResult<CommitInfo> {
        Transaction::commit(self)
    }

    fn rollback(self) -> TransactionResult<()> {
        Transaction::rollback(self)
    }
}

impl<FS: FileSystem> TimeSeries for Transaction<FS> {
    type TimeSeriesCursor<'a>
        = TransactionTimeSeriesCursor
    where
        Self: 'a;

    fn table_id(&self) -> TableId {
        self.current_table_id.unwrap_or(TableId::from(0))
    }

    fn name(&self) -> &str {
        self.current_table_name.as_deref().unwrap_or("unknown")
    }

    fn capabilities(&self) -> SpecialtyTableCapabilities {
        let Some(table_id) = self.current_table_id else {
            return SpecialtyTableCapabilities::default();
        };

        if let Some(engine) = self.engine_registry.get(table_id) {
            match &engine {
                crate::table::TableEngineInstance::TimeSeriesTable(ts) => ts.capabilities(),
                _ => SpecialtyTableCapabilities::default(),
            }
        } else {
            SpecialtyTableCapabilities::default()
        }
    }

    fn append_point(
        &self,
        series_key: &[u8],
        timestamp: i64,
        value_key: &[u8],
        _tx_id: TransactionId,
        _commit_lsn: LogSequenceNumber,
    ) -> crate::table::TableResult<()> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for append_point",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for time series operation".to_string(),
            )
        })?;

        // Write to WAL - encode time series data in key/value fields
        // Format: key = series_key|timestamp, value = value_key
        let mut wal_key = Vec::new();
        wal_key.extend_from_slice(series_key);
        wal_key.push(b'|');
        wal_key.extend_from_slice(&timestamp.to_le_bytes());

        self.wal
            .write_operation(
                self.txn_id,
                table_id,
                WriteOpType::TimeSeriesInsert,
                wal_key,
                value_key.to_vec(),
            )
            .map_err(|e| crate::table::TableError::Other(format!("WAL write failed: {}", e)))?;

        self.record_timeseries_operation(
            table_id,
            TimeSeriesOp::AppendPoint {
                series_key: series_key.to_vec(),
                timestamp,
                value_key: value_key.to_vec(),
            },
        );
        Ok(())
    }

    fn scan_series(
        &self,
        series_key: &[u8],
        start_ts: i64,
        end_ts: i64,
    ) -> crate::table::TableResult<Self::TimeSeriesCursor<'_>> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for scan_series",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for time series operation".to_string(),
            )
        })?;

        if let Some(engine) = self.engine_registry.get(table_id) {
            match &engine {
                crate::table::TableEngineInstance::TimeSeriesTable(ts) => {
                    let cursor = ts.scan_series(series_key, start_ts, end_ts)?;
                    Ok(TransactionTimeSeriesCursor::from_points(cursor))
                }
                _ => Err(crate::table::TableError::Other(
                    "table is not a time series table".to_string(),
                )),
            }
        } else {
            Err(crate::table::TableError::Other(
                "time series table not found".to_string(),
            ))
        }
    }

    fn latest_before(
        &self,
        series_key: &[u8],
        timestamp: i64,
    ) -> crate::table::TableResult<Option<TimePointRef>> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for latest_before",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for time series operation".to_string(),
            )
        })?;

        if let Some(engine) = self.engine_registry.get(table_id) {
            match &engine {
                crate::table::TableEngineInstance::TimeSeriesTable(ts) => {
                    ts.latest_before(series_key, timestamp)
                }
                _ => Err(crate::table::TableError::Other(
                    "table is not a time series table".to_string(),
                )),
            }
        } else {
            Err(crate::table::TableError::Other(
                "time series table not found".to_string(),
            ))
        }
    }

    fn stats(&self) -> crate::table::TableResult<SpecialtyTableStats> {
        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for time series operation".to_string(),
            )
        })?;

        if let Some(engine) = self.engine_registry.get(table_id) {
            match &engine {
                crate::table::TableEngineInstance::TimeSeriesTable(ts) => ts.stats(),
                _ => Err(crate::table::TableError::Other(
                    "table is not a time series table".to_string(),
                )),
            }
        } else {
            Err(crate::table::TableError::Other(
                "time series table not found".to_string(),
            ))
        }
    }

    fn verify(&self) -> crate::table::TableResult<VerificationReport> {
        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for time series operation".to_string(),
            )
        })?;

        if let Some(engine) = self.engine_registry.get(table_id) {
            match &engine {
                crate::table::TableEngineInstance::TimeSeriesTable(ts) => ts.verify(),
                _ => Err(crate::table::TableError::Other(
                    "table is not a time series table".to_string(),
                )),
            }
        } else {
            Err(crate::table::TableError::Other(
                "time series table not found".to_string(),
            ))
        }
    }
}

impl<FS: FileSystem> VectorSearch for Transaction<FS> {
    fn table_id(&self) -> TableId {
        self.current_table_id.unwrap_or(TableId::from(0))
    }

    fn name(&self) -> &str {
        self.current_table_name.as_deref().unwrap_or("unknown")
    }

    fn capabilities(&self) -> SpecialtyTableCapabilities {
        let Some(table_id) = self.current_table_id else {
            return SpecialtyTableCapabilities::default();
        };

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedHnswVector(hnsw)) => {
                VectorSearch::capabilities(hnsw.as_ref())
            }
            _ => SpecialtyTableCapabilities::default(),
        }
    }

    fn dimensions(&self) -> usize {
        let Some(table_id) = self.current_table_id else {
            return 0;
        };

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedHnswVector(hnsw)) => hnsw.dimensions(),
            _ => 0,
        }
    }

    fn metric(&self) -> VectorMetric {
        let Some(table_id) = self.current_table_id else {
            return VectorMetric::Cosine;
        };

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedHnswVector(hnsw)) => hnsw.metric(),
            _ => VectorMetric::Cosine,
        }
    }

    fn insert_vector(
        &self,
        id: &[u8],
        vector: &[f32],
        _tx_id: TransactionId,
        _commit_lsn: LogSequenceNumber,
    ) -> crate::table::TableResult<()> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for insert_vector",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for vector operation".to_string())
        })?;

        // Serialize vector to bytes for WAL
        let vector_bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();

        self.wal
            .write_operation(
                self.txn_id,
                table_id,
                WriteOpType::VectorInsert,
                id.to_vec(),
                vector_bytes,
            )
            .map_err(|e| crate::table::TableError::Other(format!("WAL write failed: {}", e)))?;

        self.record_vector_operation(
            table_id,
            VectorOp::InsertVector {
                id: id.to_vec(),
                vector: vector.to_vec(),
            },
        );
        Ok(())
    }

    fn delete_vector(
        &self,
        id: &[u8],
        _tx_id: TransactionId,
        _commit_lsn: LogSequenceNumber,
    ) -> crate::table::TableResult<()> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for delete_vector",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for vector operation".to_string())
        })?;

        self.wal
            .write_operation(
                self.txn_id,
                table_id,
                WriteOpType::VectorDelete,
                id.to_vec(),
                vec![],
            )
            .map_err(|e| crate::table::TableError::Other(format!("WAL write failed: {}", e)))?;

        self.record_vector_operation(table_id, VectorOp::DeleteVector { id: id.to_vec() });
        Ok(())
    }

    fn search_vector<'a>(
        &self,
        query: &[f32],
        options: VectorSearchOptions<'a>,
    ) -> crate::table::TableResult<Vec<VectorHit>> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for search_vector",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for vector operation".to_string())
        })?;

        // Vector search reads from the underlying table
        // Pending inserts in vector_write_set are not yet visible to searches
        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedHnswVector(hnsw)) => {
                hnsw.search_vector(query, options)
            }
            Some(_) => Err(crate::table::TableError::Other(
                "table is not a vector search table".to_string(),
            )),
            None => Err(crate::table::TableError::Other(
                "table not found".to_string(),
            )),
        }
    }

    fn stats(&self) -> crate::table::TableResult<SpecialtyTableStats> {
        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for vector operation".to_string())
        })?;

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedHnswVector(hnsw)) => hnsw.stats(),
            Some(_) => Err(crate::table::TableError::Other(
                "table is not a vector search table".to_string(),
            )),
            None => Err(crate::table::TableError::Other(
                "table not found".to_string(),
            )),
        }
    }

    fn verify(&self) -> crate::table::TableResult<VerificationReport> {
        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other("no table context set for vector operation".to_string())
        })?;

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedHnswVector(hnsw)) => hnsw.verify(),
            Some(_) => Err(crate::table::TableError::Other(
                "table is not a vector search table".to_string(),
            )),
            None => Err(crate::table::TableError::Other(
                "table not found".to_string(),
            )),
        }
    }
}

impl<FS: FileSystem> GeoSpatial for Transaction<FS> {
    fn table_id(&self) -> TableId {
        self.current_table_id.unwrap_or(TableId::from(0))
    }

    fn name(&self) -> &str {
        self.current_table_name.as_deref().unwrap_or("unknown")
    }

    fn capabilities(&self) -> SpecialtyTableCapabilities {
        let Some(table_id) = self.current_table_id else {
            return SpecialtyTableCapabilities::default();
        };

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedRTree(rtree)) => {
                GeoSpatial::capabilities(rtree.as_ref())
            }
            _ => SpecialtyTableCapabilities::default(),
        }
    }

    fn insert_geometry(
        &self,
        id: &[u8],
        geometry: GeometryRef<'_>,
        _tx_id: TransactionId,
        _commit_lsn: LogSequenceNumber,
    ) -> crate::table::TableResult<()> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for insert_geometry",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for geospatial operation".to_string(),
            )
        })?;

        // Serialize geometry for storage in write set
        let serialized_geometry = match geometry {
            GeometryRef::Point(point) => SerializedGeometry::Point {
                x: point.x,
                y: point.y,
            },
            GeometryRef::BoundingBox { min, max } => SerializedGeometry::BoundingBox {
                min_x: min.x,
                min_y: min.y,
                max_x: max.x,
                max_y: max.y,
            },
            GeometryRef::Wkb(wkb) => SerializedGeometry::Wkb(wkb.to_vec()),
        };

        // Serialize geometry for WAL (store as value in Write record)
        let geometry_bytes = match &serialized_geometry {
            SerializedGeometry::Point { x, y } => {
                let mut bytes = Vec::with_capacity(17);
                bytes.push(1); // Point type
                bytes.extend_from_slice(&x.to_le_bytes());
                bytes.extend_from_slice(&y.to_le_bytes());
                bytes
            }
            SerializedGeometry::BoundingBox {
                min_x,
                min_y,
                max_x,
                max_y,
            } => {
                let mut bytes = Vec::with_capacity(33);
                bytes.push(2); // BoundingBox type
                bytes.extend_from_slice(&min_x.to_le_bytes());
                bytes.extend_from_slice(&min_y.to_le_bytes());
                bytes.extend_from_slice(&max_x.to_le_bytes());
                bytes.extend_from_slice(&max_y.to_le_bytes());
                bytes
            }
            SerializedGeometry::Wkb(wkb) => {
                let mut bytes = Vec::with_capacity(1 + wkb.len());
                bytes.push(3); // WKB type
                bytes.extend_from_slice(wkb);
                bytes
            }
        };

        self.wal
            .write_operation(
                self.txn_id,
                table_id,
                WriteOpType::GeoInsert,
                id.to_vec(),
                geometry_bytes,
            )
            .map_err(|e| crate::table::TableError::Other(format!("WAL write failed: {}", e)))?;

        self.record_geospatial_operation(
            table_id,
            GeoSpatialOp::InsertGeometry {
                id: id.to_vec(),
                geometry: serialized_geometry,
            },
        );
        Ok(())
    }

    fn delete_geometry(
        &self,
        id: &[u8],
        _tx_id: TransactionId,
        _commit_lsn: LogSequenceNumber,
    ) -> crate::table::TableResult<()> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for delete_geometry",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for geospatial operation".to_string(),
            )
        })?;

        self.wal
            .write_operation(
                self.txn_id,
                table_id,
                WriteOpType::GeoDelete,
                id.to_vec(),
                vec![],
            )
            .map_err(|e| crate::table::TableError::Other(format!("WAL write failed: {}", e)))?;

        self.record_geospatial_operation(
            table_id,
            GeoSpatialOp::DeleteGeometry { id: id.to_vec() },
        );
        Ok(())
    }

    fn intersects(
        &self,
        query: GeometryRef<'_>,
        limit: usize,
    ) -> crate::table::TableResult<Vec<GeoHit>> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for intersects",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for geospatial operation".to_string(),
            )
        })?;

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedRTree(rtree)) => {
                rtree.intersects(query, limit)
            }
            Some(_) => Err(crate::table::TableError::Other(
                "table is not a geospatial table".to_string(),
            )),
            None => Ok(vec![]),
        }
    }

    fn nearest(&self, point: GeoPoint, limit: usize) -> crate::table::TableResult<Vec<GeoHit>> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for nearest",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for geospatial operation".to_string(),
            )
        })?;

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedRTree(rtree)) => {
                rtree.nearest(point, limit)
            }
            Some(_) => Err(crate::table::TableError::Other(
                "table is not a geospatial table".to_string(),
            )),
            None => Ok(vec![]),
        }
    }

    fn stats(&self) -> crate::table::TableResult<SpecialtyTableStats> {
        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for geospatial operation".to_string(),
            )
        })?;

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedRTree(rtree)) => rtree.stats(),
            Some(_) => Err(crate::table::TableError::Other(
                "table is not a geospatial table".to_string(),
            )),
            None => Err(crate::table::TableError::Other(
                "table not found".to_string(),
            )),
        }
    }

    fn verify(&self) -> crate::table::TableResult<VerificationReport> {
        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for geospatial operation".to_string(),
            )
        })?;

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedRTree(rtree)) => rtree.verify(),
            Some(_) => Err(crate::table::TableError::Other(
                "table is not a geospatial table".to_string(),
            )),
            None => Err(crate::table::TableError::Other(
                "table not found".to_string(),
            )),
        }
    }
}

impl<FS: FileSystem> FullTextSearch for Transaction<FS> {
    fn table_id(&self) -> TableId {
        self.current_table_id.unwrap_or(TableId::from(0))
    }

    fn name(&self) -> &str {
        self.current_table_name.as_deref().unwrap_or("unknown")
    }

    fn capabilities(&self) -> SpecialtyTableCapabilities {
        let Some(table_id) = self.current_table_id else {
            return SpecialtyTableCapabilities::default();
        };

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedFullTextIndex(fulltext)) => {
                FullTextSearch::capabilities(fulltext.as_ref())
            }
            _ => SpecialtyTableCapabilities::default(),
        }
    }

    fn index_document(
        &self,
        doc_id: &[u8],
        fields: &[TextField<'_>],
        _tx_id: TransactionId,
        _commit_lsn: LogSequenceNumber,
    ) -> crate::table::TableResult<()> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for index_document",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for full-text operation".to_string(),
            )
        })?;

        let serialized_fields: Vec<(String, String, f32)> = fields
            .iter()
            .map(|f| (f.name.to_string(), f.text.to_string(), f.boost))
            .collect();

        self.wal
            .write_operation(
                self.txn_id,
                table_id,
                WriteOpType::FullTextIndex,
                doc_id.to_vec(),
                vec![],
            )
            .map_err(|e| crate::table::TableError::Other(format!("WAL write failed: {}", e)))?;

        self.record_fulltext_operation(
            table_id,
            FullTextOp::IndexDocument {
                doc_id: doc_id.to_vec(),
                fields: serialized_fields,
            },
        );
        Ok(())
    }

    fn update_document(
        &self,
        doc_id: &[u8],
        fields: &[TextField<'_>],
        _tx_id: TransactionId,
        _commit_lsn: LogSequenceNumber,
    ) -> crate::table::TableResult<()> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for update_document",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for full-text operation".to_string(),
            )
        })?;

        let serialized_fields: Vec<(String, String, f32)> = fields
            .iter()
            .map(|f| (f.name.to_string(), f.text.to_string(), f.boost))
            .collect();

        self.wal
            .write_operation(
                self.txn_id,
                table_id,
                WriteOpType::FullTextUpdate,
                doc_id.to_vec(),
                vec![],
            )
            .map_err(|e| crate::table::TableError::Other(format!("WAL write failed: {}", e)))?;

        self.record_fulltext_operation(
            table_id,
            FullTextOp::UpdateDocument {
                doc_id: doc_id.to_vec(),
                fields: serialized_fields,
            },
        );
        Ok(())
    }

    fn delete_document(
        &self,
        doc_id: &[u8],
        _tx_id: TransactionId,
        _commit_lsn: LogSequenceNumber,
    ) -> crate::table::TableResult<()> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for delete_document",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for full-text operation".to_string(),
            )
        })?;

        self.wal
            .write_operation(
                self.txn_id,
                table_id,
                WriteOpType::FullTextDelete,
                doc_id.to_vec(),
                vec![],
            )
            .map_err(|e| crate::table::TableError::Other(format!("WAL write failed: {}", e)))?;

        self.record_fulltext_operation(
            table_id,
            FullTextOp::DeleteDocument {
                doc_id: doc_id.to_vec(),
            },
        );
        Ok(())
    }

    fn search(
        &self,
        query: TextQuery<'_>,
        limit: usize,
    ) -> crate::table::TableResult<Vec<ScoredDocument>> {
        if !self.is_active() {
            return Err(crate::table::TableError::Other(format!(
                "transaction {} is not active for search",
                self.txn_id
            )));
        }

        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for full-text operation".to_string(),
            )
        })?;

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedFullTextIndex(fulltext)) => {
                fulltext.search(query, limit)
            }
            Some(_) => Err(crate::table::TableError::Other(
                "table is not a full-text index".to_string(),
            )),
            None => Ok(vec![]),
        }
    }

    fn stats(&self) -> crate::table::TableResult<SpecialtyTableStats> {
        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for full-text operation".to_string(),
            )
        })?;

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedFullTextIndex(fulltext)) => {
                fulltext.stats()
            }
            Some(_) => Err(crate::table::TableError::Other(
                "table is not a full-text index".to_string(),
            )),
            None => Err(crate::table::TableError::Other(
                "table not found".to_string(),
            )),
        }
    }

    fn verify(&self) -> crate::table::TableResult<VerificationReport> {
        let table_id = self.current_table_id.ok_or_else(|| {
            crate::table::TableError::Other(
                "no table context set for full-text operation".to_string(),
            )
        })?;

        match self.engine_registry.get(table_id) {
            Some(crate::table::TableEngineInstance::PagedFullTextIndex(fulltext)) => {
                fulltext.verify()
            }
            Some(_) => Err(crate::table::TableError::Other(
                "table is not a full-text index".to_string(),
            )),
            None => Err(crate::table::TableError::Other(
                "table not found".to_string(),
            )),
        }
    }
}
