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

//! Table capability traits.
//!
//! This module defines traits for different table capabilities:
//! - Point lookups
//! - Ordered scans
//! - Mutations
//! - Batch operations
//! - Memory management
//! - Maintenance operations

use crate::pager::PhysicalLocation;
use crate::table::TableResult;
use crate::txn::TransactionId;
use crate::types::{
    CompressionKind, EncryptionKind, KeyBuf, KeyEncoding, MemoryPressure, ScanBounds, TableId,
    ValueBuf,
};
use crate::wal::LogSequenceNumber;
use std::borrow::Cow;

// TableId has been removed - use ObjectId directly throughout the codebase.
// This completes the type system unification where tables and indexes share
// the same identifier type without wrapper aliases.

/// Options for creating a table.
///
/// In the unified architecture, the engine determines the table's capabilities.
/// Capabilities are discovered via trait implementations, not enums.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TableOptions {
    pub engine: TableEngineKind,
    pub key_encoding: KeyEncoding,
    pub compression: Option<CompressionKind>,
    pub encryption: Option<EncryptionKind>,
    pub page_size: Option<usize>,
    pub format_version: u32,
    /// Maximum size for inline values (stored directly in table pages).
    /// Values larger than this threshold should use external storage (ValueRef).
    pub max_inline_size: Option<usize>,
    /// Maximum value size supported by this table.
    pub max_value_size: Option<u64>,
    
    // Engine-specific configuration fields
    /// AppendLog configuration
    pub appendlog_config: Option<crate::table::appendlog::AppendLogConfig>,
    /// LSM tree configuration
    pub lsm_config: Option<crate::table::lsm::LsmConfig>,
    /// Bloom filter configuration (num_items, bits_per_key)
    pub bloom_config: Option<(usize, usize)>,
    /// R-Tree spatial configuration
    pub spatial_config: Option<crate::table::rtree::SpatialConfig>,
    /// TimeSeries configuration
    pub timeseries_config: Option<crate::table::timeseries::TimeSeriesConfig>,
    /// FullText search configuration
    pub fulltext_config: Option<crate::table::fulltext::FullTextConfig>,
}

impl Default for TableOptions {
    fn default() -> Self {
        Self {
            engine: TableEngineKind::Memory,
            key_encoding: KeyEncoding::RawBytes,
            compression: None,
            encryption: None,
            page_size: None,
            format_version: 1,
            max_inline_size: None,
            max_value_size: None,
            appendlog_config: None,
            lsm_config: None,
            bloom_config: None,
            spatial_config: None,
            timeseries_config: None,
            fulltext_config: None,
        }
    }
}

/// Table metadata from the catalog.
///
/// In the unified architecture, this represents ALL tables.
/// The `options.engine` field determines the table's implementation and capabilities.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TableInfo {
    pub id: TableId,
    pub name: String,
    pub options: TableOptions,
    pub root: Option<PhysicalLocation>,
    pub created_lsn: LogSequenceNumber,
}

/// Table engine kind - determines both implementation and capabilities.
///
/// In the unified architecture, the engine kind specifies what type of table
/// this is. Capabilities are determined by what traits the engine implements.
///
/// Dense ordered engines (BTree, LsmTree, Memory) implement DenseOrdered trait.
/// Specialty engines (Bloom, SparseOrdered, etc.) implement their specific traits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TableEngineKind {
    /// B-Tree (paged, dense ordered)
    BTree,
    /// B+ Tree (paged, dense ordered)
    BPlusTree,
    /// LSM Tree (paged, dense ordered, write-optimized)
    LsmTree,
    /// Adaptive Radix Tree (paged, dense ordered)
    Art,
    /// In-memory hash table
    Hash,
    /// In-memory dense ordered table
    Memory,
    /// Append-only log
    AppendLog,
    /// Columnar storage segment
    ColumnarSegment,
    /// Sparse ordered index with markers
    SparseOrdered,
    /// Bloom filter (approximate membership)
    Bloom,
    /// Bitmap index
    Bitmap,
    /// Full-text search index
    FullText,
    /// Vector search using HNSW
    VectorHnsw,
    /// Vector search using IVF
    VectorIvf,
    /// Geospatial index
    GeoSpatial,
    /// Time series storage
    TimeSeries,
    /// Graph adjacency list
    GraphAdjacency,
    /// Blob storage (large binary objects)
    Blob,
    /// Custom engine
    Custom(u32),
}

impl TableEngineKind {
    /// Returns true if this engine provides persistent storage.
    ///
    /// Persistent engines survive process restarts and store data on disk.
    /// Non-persistent engines are ephemeral and lose data when the process ends.
    pub fn is_persistent(&self) -> bool {
        match self {
            // Paged engines - persistent
            Self::BTree
            | Self::BPlusTree
            | Self::LsmTree
            | Self::AppendLog
            | Self::ColumnarSegment
            | Self::TimeSeries => true,

            // In-memory engines - ephemeral
            Self::Hash | Self::Memory | Self::Art => false,

            // Specialty indexes - typically ephemeral, rebuilt from base table
            Self::SparseOrdered
            | Self::Bloom
            | Self::Bitmap
            | Self::FullText
            | Self::VectorHnsw
            | Self::VectorIvf
            | Self::GeoSpatial
            | Self::GraphAdjacency => false,

            // Blob storage - can be persistent or ephemeral depending on implementation
            // The specific implementation (MemoryBlob, PagedBlob, FileBlob) determines this
            Self::Blob => true,

            // Custom engines - assume non-persistent by default
            Self::Custom(_) => false,
        }
    }

    /// Returns true if this engine is in-memory only (ephemeral).
    pub fn is_ephemeral(&self) -> bool {
        !self.is_persistent()
    }
}

// =============================================================================
// Table capability traits
// =============================================================================

/// Streaming read interface for large values.
///
/// Allows reading values in chunks without loading the entire value into memory.
/// Similar to std::io::Read but returns TableResult for consistency.
pub trait ValueStream {
    /// Read data into the provided buffer.
    ///
    /// Returns the number of bytes read. A return value of 0 indicates EOF.
    fn read(&mut self, buf: &mut [u8]) -> TableResult<usize>;

    /// Get a size hint for the total value size, if known.
    ///
    /// This is useful for pre-allocating buffers or displaying progress.
    fn size_hint(&self) -> Option<u64>;
}

/// Streaming write interface for large values.
///
/// Allows writing values in chunks without requiring the entire value in memory.
pub trait ValueSink {
    /// Write data from the provided buffer.
    ///
    /// Returns the number of bytes written.
    fn write(&mut self, buf: &[u8]) -> TableResult<usize>;

    /// Finish writing and return the total bytes written.
    ///
    /// This must be called to ensure all data is persisted.
    fn finish(self) -> TableResult<u64>;
}

/// Helper struct for streaming a slice of bytes.
///
/// This is used as a default implementation for get_stream when the value
/// is already in memory.
pub struct SliceValueStream {
    data: Vec<u8>,
    position: usize,
}

impl SliceValueStream {
    /// Create a new stream from a vector of bytes.
    pub fn new(data: Vec<u8>) -> Self {
        Self { data, position: 0 }
    }
}

impl ValueStream for SliceValueStream {
    fn read(&mut self, buf: &mut [u8]) -> TableResult<usize> {
        let remaining = self.data.len() - self.position;
        if remaining == 0 {
            return Ok(0);
        }

        let to_copy = remaining.min(buf.len());
        buf[..to_copy].copy_from_slice(&self.data[self.position..self.position + to_copy]);
        self.position += to_copy;
        Ok(to_copy)
    }

    fn size_hint(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }
}

/// Point lookup capability with MVCC snapshot support.
///
/// The `snapshot_lsn` parameter enables tables to:
/// 1. Traverse version chains to find the visible version
/// 2. Support snapshot isolation
/// 3. Enable time-travel queries
pub trait PointLookup {
    /// Get the value for a key at a specific snapshot.
    ///
    /// # Arguments
    ///
    /// * `key` - The key to look up
    /// * `snapshot_lsn` - The LSN at which to read (for MVCC visibility)
    ///
    /// # Returns
    ///
    /// The value if found and visible at the snapshot, or None otherwise
    fn get(&self, key: &[u8], snapshot_lsn: LogSequenceNumber) -> TableResult<Option<ValueBuf>>;

    /// Get a streaming reader for a value at a specific snapshot.
    ///
    /// This is more efficient for large values as it avoids loading the entire
    /// value into memory. For small inline values, implementations may wrap
    /// the value in a simple stream.
    ///
    /// # Arguments
    ///
    /// * `key` - The key to look up
    /// * `snapshot_lsn` - The LSN at which to read (for MVCC visibility)
    ///
    /// # Returns
    ///
    /// A streaming reader if the key exists and is visible, or None otherwise
    fn get_stream(
        &self,
        key: &[u8],
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Option<Box<dyn ValueStream + '_>>> {
        // Default implementation: load entire value and wrap in a stream
        self.get(key, snapshot_lsn).map(|opt| {
            opt.map(|value_buf| {
                Box::new(SliceValueStream::new(value_buf.0)) as Box<dyn ValueStream + '_>
            })
        })
    }

    /// Check if a key exists at a specific snapshot.
    fn contains(&self, key: &[u8], snapshot_lsn: LogSequenceNumber) -> TableResult<bool> {
        Ok(self.get(key, snapshot_lsn)?.is_some())
    }
}

/// Ordered scan capability with MVCC snapshot support.
pub trait OrderedScan {
    type Cursor<'a>: TableCursor
    where
        Self: 'a;

    /// Create a cursor over the specified bounds at a specific snapshot.
    ///
    /// # Arguments
    ///
    /// * `bounds` - The range of keys to scan
    /// * `snapshot_lsn` - The LSN at which to read (for MVCC visibility)
    fn scan(
        &self,
        bounds: ScanBounds,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self::Cursor<'_>>;
}

/// Prefix scan capability.
///
/// This trait should only be implemented by engines that efficiently support prefix scans.
/// The default implementation is intentionally omitted to prevent engines from accidentally
/// enabling prefix scans without proper support. Engines must explicitly implement this
/// trait and should verify TableCapabilities::prefix_scan is true.
pub trait PrefixScan: OrderedScan {
    /// Scan all keys with the given prefix at a specific snapshot.
    ///
    /// Implementations should check that the engine's capabilities include prefix_scan support.
    fn scan_prefix(
        &self,
        prefix: &[u8],
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self::Cursor<'_>>;
}

/// Mutation capability.
pub trait MutableTable {
    /// Insert or update a key-value pair.
    ///
    /// # Returns
    ///
    /// The number of bytes written (key + value + metadata).
    ///
    /// # Errors
    ///
    /// Returns an error if the value exceeds `max_value_size()`.
    fn put(&mut self, key: &[u8], value: &[u8]) -> TableResult<u64>;

    /// Insert or update a key with a streaming value.
    ///
    /// This is more efficient for large values as it avoids loading the entire
    /// value into memory. The stream is consumed during the operation.
    ///
    /// # Arguments
    ///
    /// * `key` - The key to insert or update
    /// * `stream` - A streaming reader for the value data
    ///
    /// # Returns
    ///
    /// The number of bytes written (key + value + metadata).
    ///
    /// # Errors
    ///
    /// Returns an error if the value exceeds `max_value_size()`.
    fn put_stream(&mut self, key: &[u8], stream: &mut dyn ValueStream) -> TableResult<u64> {
        // Default implementation: read entire stream into memory and call put
        let mut buffer = Vec::new();
        if let Some(size_hint) = stream.size_hint() {
            buffer.reserve(size_hint as usize);
        }

        let mut temp_buf = vec![0u8; 8192]; // 8KB chunks
        loop {
            let n = stream.read(&mut temp_buf)?;
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&temp_buf[..n]);
        }

        self.put(key, &buffer)
    }

    fn delete(&mut self, key: &[u8]) -> TableResult<bool>;

    fn range_delete(&mut self, bounds: ScanBounds) -> TableResult<u64>;

    /// Get the maximum inline size for this table.
    ///
    /// Values smaller than this threshold are stored inline in table pages.
    /// Values larger than this threshold may be stored externally (implementation-dependent).
    ///
    /// Returns `None` if there is no inline size limit (all values stored inline).
    fn max_inline_size(&self) -> Option<usize> {
        None // Default: no limit, store all values inline
    }

    /// Get the maximum value size supported by this table.
    ///
    /// Attempts to store values larger than this limit will fail.
    ///
    /// Returns `None` if there is no size limit.
    fn max_value_size(&self) -> Option<u64> {
        None // Default: no limit
    }
}

/// Batch operation capability.
pub trait BatchOps {
    fn batch_get(&self, keys: &[&[u8]]) -> TableResult<Vec<Option<ValueBuf>>>;

    fn apply_batch<'a>(&mut self, batch: WriteBatch<'a>) -> TableResult<BatchReport>;
}

/// Flush capability.
pub trait Flushable {
    fn flush(&mut self) -> TableResult<()>;
}

/// Memory awareness for adaptive resource management.
pub trait MemoryAware {
    /// Get current memory usage in bytes.
    fn memory_usage(&self) -> usize;

    /// Get the configured memory budget in bytes.
    fn memory_budget(&self) -> usize;

    /// Check if the component can evict data to free memory.
    fn can_evict(&self) -> bool {
        false
    }
}

/// Cache eviction capability for memory management.
pub trait EvictableCache: MemoryAware {
    /// Evict data to reach the target memory usage.
    fn evict(&mut self, target_bytes: usize) -> TableResult<usize>;

    /// Get the eviction priority for a specific key.
    fn eviction_priority(&self, key: &[u8]) -> Option<u64> {
        let _ = key;
        None
    }

    /// Respond to memory pressure.
    fn on_memory_pressure(&mut self, pressure: MemoryPressure) -> TableResult<()> {
        let current = self.memory_usage();
        let budget = self.memory_budget();

        // Calculate target relative to current usage, not budget
        // This prevents trying to "evict up" when current << budget
        let target = match pressure {
            MemoryPressure::None => return Ok(()),
            MemoryPressure::Low => current.min((budget as f64 * 0.90) as usize),
            MemoryPressure::Medium => current.min((budget as f64 * 0.75) as usize),
            MemoryPressure::High => current.min((budget as f64 * 0.50) as usize),
            MemoryPressure::Critical => current.min((budget as f64 * 0.25) as usize),
        };

        if current > target {
            self.evict(target)?;
        }

        Ok(())
    }
}

/// Format migration capability for schema evolution.
pub trait Migratable {
    /// Get the current format version.
    fn format_version(&self) -> u32;

    /// Check if migration from the given version is supported.
    fn can_migrate_from(&self, from_version: u32) -> bool;

    /// Estimate the cost of migration in arbitrary units.
    /// Returns 0 if no migration needed, u64::MAX if migration is impossible,
    /// or a finite cost estimate if migration is possible.
    fn migration_cost(&self, from_version: u32) -> u64 {
        if from_version == self.format_version() {
            // No migration needed
            0
        } else if self.can_migrate_from(from_version) {
            // Migration is possible - return cost based on version difference
            // Larger version gaps typically require more work
            let version_delta = self.format_version().abs_diff(from_version);
            // Base cost of 100 per version step
            100u64.saturating_mul(version_delta as u64)
        } else {
            // Migration is not possible
            u64::MAX
        }
    }

    /// Perform migration from the specified version.
    fn migrate(&mut self, from_version: u32) -> TableResult<()>;
}

/// Base table trait with just identity and metadata.
///
/// This is the minimal trait that all table implementations must provide,
/// including specialty tables (Blob, ApproximateMembership, etc.) that don't
/// support ordered scans or traditional reader/writer patterns.
pub trait Table {
    fn table_id(&self) -> TableId;

    fn name(&self) -> &str;

    fn kind(&self) -> TableEngineKind;

    fn capabilities(&self) -> TableCapabilities;

    fn stats(&self) -> TableResult<TableStatistics>;
}

/// Searchable table with reader/writer access patterns.
///
/// This trait extends the base Table trait to add reader/writer abstractions
/// for tables that support point lookups and ordered scans (BTree, LSM, etc.).
/// Specialty tables that don't fit this pattern (Blob, Bloom filters, etc.)
/// can implement just the base Table trait.
pub trait SearchableTable: Table {
    type Reader<'a>: TableReader
    where
        Self: 'a;

    type Writer<'a>: TableWriter
    where
        Self: 'a;

    fn reader(&self, snapshot_lsn: LogSequenceNumber) -> TableResult<Self::Reader<'_>>;

    /// Create a writer for the given transaction.
    ///
    /// The snapshot_lsn parameter enables MVCC operations within the writer:
    /// - Check-and-set operations need to read current values
    /// - Read-before-delete operations need visibility checks
    /// - Conditional updates need snapshot isolation
    fn writer(
        &self,
        tx_id: TransactionId,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self::Writer<'_>>;
}

/// Marker trait for a full ordered key-value table.
pub trait OrderedKvTable: PointLookup + OrderedScan + MutableTable + BatchOps + Flushable {}

impl<T> OrderedKvTable for T where T: PointLookup + OrderedScan + MutableTable + BatchOps + Flushable
{}

/// Backward compatibility alias.
#[deprecated(since = "0.1.0", note = "Use `SearchableTable` instead")]
pub trait TableEngine: SearchableTable {}

impl<T: SearchableTable> TableEngine for T {}

/// Read view over a table engine.
pub trait TableReader: PointLookup + OrderedScan {
    fn snapshot_lsn(&self) -> LogSequenceNumber;

    fn approximate_len(&self) -> TableResult<Option<u64>>;
}

/// Write view over a table engine.
pub trait TableWriter: MutableTable + BatchOps + Flushable {
    fn tx_id(&self) -> TransactionId;

    /// Get the snapshot LSN for this writer.
    ///
    /// This is the canonical location for the snapshot LSN in write operations.
    /// Writers need read snapshots for check-and-set, read-before-delete, and
    /// other conditional operations.
    fn snapshot_lsn(&self) -> LogSequenceNumber;
}

/// Declared table capabilities.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableCapabilities {
    pub ordered: bool,
    pub point_lookup: bool,
    pub prefix_scan: bool,
    pub reverse_scan: bool,
    pub range_delete: bool,
    pub merge_operator: bool,
    pub mvcc_native: bool,
    pub append_optimized: bool,
    pub memory_resident: bool,
    pub disk_resident: bool,
    pub supports_compression: bool,
    pub supports_encryption: bool,
}

// =============================================================================
// Maintenance, statistics, verification, and repair
// =============================================================================

/// Maintenance operations common to tables, indexes, and storage files.
///
/// Note: For flushing data to disk, use the `Flushable` trait instead.
/// This trait focuses on higher-level maintenance operations like compaction,
/// checkpointing, and vacuuming.
pub trait Maintainable {
    fn compact(&mut self, options: CompactionOptions) -> TableResult<CompactionReport>;

    fn checkpoint(&mut self) -> TableResult<CheckpointInfo>;

    fn vacuum(&mut self, options: VacuumOptions) -> TableResult<VacuumReport>;
}

/// Statistics provider for query planning and diagnostics.
pub trait StatisticsProvider {
    fn statistics(&self) -> TableResult<TableStatistics>;

    fn refresh_statistics(&mut self, budget: WorkBudget) -> TableResult<()>;
}

/// Consistency verification and repair.
pub trait ConsistencyVerifier {
    fn verify(&self, scope: VerifyScope) -> TableResult<VerificationReport>;

    fn repair(&mut self, plan: RepairPlan) -> TableResult<RepairReport>;
}

// =============================================================================
// Supporting types
// =============================================================================

/// Table-level cursor trait.
///
/// This trait is implemented by table engine cursors. The transaction-level
/// `Cursor` struct (in `txn::cursor`) boxes a `TableCursor` implementation.
///
/// These methods are critical for:
/// - Efficient range queries (seek)
/// - Reverse iteration (seek_for_prev, last)
/// - Boundary access (first, last)
/// - MVCC visibility (snapshot_lsn)
pub trait TableCursor {
    fn valid(&self) -> bool;
    fn key(&self) -> Option<&[u8]>;
    fn value(&self) -> Option<&[u8]>;
    fn next(&mut self) -> TableResult<()>;
    fn prev(&mut self) -> TableResult<()>;
    fn seek(&mut self, key: &[u8]) -> TableResult<()>;
    fn seek_for_prev(&mut self, key: &[u8]) -> TableResult<()>;
    fn first(&mut self) -> TableResult<()>;
    fn last(&mut self) -> TableResult<()>;
    fn snapshot_lsn(&self) -> LogSequenceNumber;
}

/// A batch of table mutations.
#[derive(Clone, Debug)]
pub struct WriteBatch<'a> {
    pub mutations: Vec<Mutation<'a>>,
}

#[derive(Clone, Debug)]
pub enum Mutation<'a> {
    Put {
        key: Cow<'a, [u8]>,
        value: Cow<'a, [u8]>,
    },
    Delete {
        key: Cow<'a, [u8]>,
    },
    RangeDelete {
        bounds: ScanBounds,
    },
    Merge {
        key: Cow<'a, [u8]>,
        operand: Cow<'a, [u8]>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatchReport {
    pub attempted: u64,
    pub applied: u64,
    pub deleted: u64,
    pub bytes_written: u64,
}

#[derive(Clone, Debug, Default)]
pub struct TableStatistics {
    pub row_count: Option<u64>,
    pub total_size_bytes: Option<u64>,
    pub key_stats: Option<KeyStatistics>,
    pub value_stats: Option<ValueStatistics>,
    pub histogram: Option<Histogram>,
    pub last_updated_lsn: Option<LogSequenceNumber>,
}

#[derive(Clone, Debug, Default)]
pub struct KeyStatistics {
    pub min_size: usize,
    pub max_size: usize,
    pub avg_size: f64,
    pub distinct_count: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct ValueStatistics {
    pub min_size: usize,
    pub max_size: usize,
    pub avg_size: f64,
    pub null_count: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Histogram {
    pub buckets: Vec<HistogramBucket>,
}

#[derive(Clone, Debug)]
pub struct HistogramBucket {
    pub lower: KeyBuf,
    pub upper: KeyBuf,
    pub count: u64,
}

#[derive(Clone, Debug, Default)]
pub struct CompactionOptions {
    pub full: bool,
    pub max_pages: Option<u64>,
    pub target_level: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct CompactionReport {
    pub pages_read: u64,
    pub pages_written: u64,
    pub bytes_reclaimed: u64,
    pub output_lsn: Option<LogSequenceNumber>,
}

#[derive(Clone, Debug, Default)]
pub struct CheckpointInfo {
    pub checkpoint_lsn: LogSequenceNumber,
    pub durable_lsn: LogSequenceNumber,
    pub pages_flushed: u64,
}

#[derive(Clone, Debug, Default)]
pub struct VacuumOptions {
    pub aggressive: bool,
    pub max_pages: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct VacuumReport {
    pub pages_freed: u64,
    pub bytes_reclaimed: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkBudget {
    pub max_pages: Option<u64>,
    pub max_millis: Option<u64>,
    pub max_items: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyScope {
    Catalog,
    Table(TableId),
    Page(crate::pager::PageId),
    FullDatabase,
}

#[derive(Clone, Debug, Default)]
pub struct VerificationReport {
    pub checked_items: u64,
    pub errors: Vec<ConsistencyError>,
    pub warnings: Vec<ConsistencyWarning>,
}

#[derive(Clone, Debug)]
pub struct ConsistencyError {
    pub error_type: ConsistencyErrorType,
    pub location: String,
    pub description: String,
    pub severity: Severity,
}

#[derive(Clone, Debug)]
pub struct ConsistencyWarning {
    pub location: String,
    pub description: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsistencyErrorType {
    InvalidPointer,
    CorruptedPage,
    CorruptedIndex,
    OrphanedPage,
    MissingPage,
    ChecksumMismatch,
    WalMismatch,
    CatalogMismatch,
    IndexTableMismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Clone, Debug)]
pub struct RepairPlan {
    pub actions: Vec<RepairAction>,
}

#[derive(Clone, Debug)]
pub enum RepairAction {
    RebuildTable(TableId),
    ReclaimOrphanedPages,
    RestoreFromWal { through: LogSequenceNumber },
    DropCorruptedTable(TableId),
}

#[derive(Clone, Debug, Default)]
pub struct RepairReport {
    pub actions_attempted: u64,
    pub actions_succeeded: u64,
    pub unrepaired_errors: Vec<ConsistencyError>,
}

// =============================================================================
// Specialty table traits (formerly index traits)
// =============================================================================

/// Cursor over ordered specialty table entries.
///
/// Uses concrete `TableResult` error type for consistency with `TableCursor`,
/// enabling uniform error handling at the transaction layer.
pub trait SpecialtyTableCursor {
    fn valid(&self) -> bool;

    fn index_key(&self) -> Option<&[u8]>;

    fn primary_key(&self) -> Option<&[u8]>;

    fn next(&mut self) -> TableResult<()>;

    fn prev(&mut self) -> TableResult<()>;

    fn seek(&mut self, index_key: &[u8]) -> TableResult<()>;
}

/// Cursor over time-series data points.
pub trait TimeSeriesCursor {
    fn valid(&self) -> bool;

    fn current(&self) -> Option<TimePointRef>;

    fn next(&mut self) -> TableResult<()>;
}

/// Cursor over graph edges.
pub trait EdgeCursor {
    fn valid(&self) -> bool;

    fn current(&self) -> Option<EdgeRef>;

    fn next(&mut self) -> TableResult<()>;
}

/// Dense ordered specialty table: one or more entries per logical record.
/// Renamed from DenseOrderedIndex to reflect unified table architecture.
pub trait DenseOrdered {
    type Cursor<'a>: SpecialtyTableCursor
    where
        Self: 'a;

    fn table_id(&self) -> TableId;

    fn name(&self) -> &str;

    fn capabilities(&self) -> SpecialtyTableCapabilities;

    fn insert_entry(
        &mut self,
        index_key: &[u8],
        primary_key: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()>;

    fn delete_entry(
        &mut self,
        index_key: &[u8],
        primary_key: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()>;

    fn scan(&self, bounds: ScanBounds) -> TableResult<Self::Cursor<'_>>;

    fn stats(&self) -> TableResult<SpecialtyTableStats>;

    fn verify(&self) -> TableResult<VerificationReport>;
}

/// Sparse specialty table: maps summarized keys/statistics to candidate physical ranges.
/// Renamed from SparseIndex to reflect unified table architecture.
pub trait SparseOrdered {
    fn table_id(&self) -> TableId;

    fn name(&self) -> &str;

    fn capabilities(&self) -> SpecialtyTableCapabilities;

    fn add_marker(&mut self, marker_key: &[u8], target: PhysicalLocation) -> TableResult<()>;

    /// Remove a marker by key and page ID.
    ///
    /// For LSM SST file removal, the caller typically knows the marker key and page ID
    /// but not the exact byte range within the page. This signature is more practical
    /// than requiring a full `PhysicalLocation`.
    fn remove_marker(
        &mut self,
        marker_key: &[u8],
        page_id: crate::pager::PageId,
    ) -> TableResult<bool>;

    fn find_candidate_ranges(&self, query: SparseQuery<'_>) -> TableResult<Vec<PhysicalRange>>;

    /// Convert physical ranges to scan bounds for ordered scan layer.
    ///
    /// This enables sparse tables to participate in query planning by translating
    /// their physical range candidates into logical key ranges that can be used
    /// with the ordered scan interface.
    fn to_scan_bounds(&self, ranges: &[PhysicalRange]) -> TableResult<Vec<ScanBounds>>;

    fn stats(&self) -> TableResult<SpecialtyTableStats>;

    fn verify(&self) -> TableResult<VerificationReport>;
}

/// Approximate membership table such as a Bloom filter.
/// Renamed from ApproximateMembershipIndex to reflect unified table architecture.
pub trait ApproximateMembership {
    fn table_id(&self) -> TableId;

    fn name(&self) -> &str;

    fn capabilities(&self) -> SpecialtyTableCapabilities;

    fn insert_key(
        &mut self,
        key: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()>;

    /// Returns false only when the key is definitely absent.
    fn might_contain(&self, key: &[u8]) -> TableResult<bool>;

    /// Returns the estimated false-positive rate when known.
    fn false_positive_rate(&self) -> Option<f64>;

    fn stats(&self) -> TableResult<SpecialtyTableStats>;

    fn verify(&self) -> TableResult<VerificationReport>;
}

/// Full-text search table with field-aware tokenization, posting lists, and scoring.
/// Renamed from FullTextIndex to reflect unified table architecture.
pub trait FullTextSearch {
    fn table_id(&self) -> TableId;

    fn name(&self) -> &str;

    fn capabilities(&self) -> SpecialtyTableCapabilities;

    fn index_document(
        &self,
        doc_id: &[u8],
        fields: &[TextField<'_>],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()>;

    /// Update an existing document, replacing its indexed content.
    ///
    /// This is more efficient than delete-then-insert for posting list updates,
    /// as it can reuse existing posting list entries where terms haven't changed.
    fn update_document(
        &self,
        doc_id: &[u8],
        fields: &[TextField<'_>],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()>;

    fn delete_document(
        &self,
        doc_id: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()>;

    fn search(&self, query: TextQuery<'_>, limit: usize) -> TableResult<Vec<ScoredDocument>>;

    fn stats(&self) -> TableResult<SpecialtyTableStats>;

    fn verify(&self) -> TableResult<VerificationReport>;
}

/// Shared vector-search interface for HNSW, IVF, flat, and hybrid vector tables.
/// Renamed from VectorIndex to reflect unified table architecture.
pub trait VectorSearch {
    fn table_id(&self) -> TableId;

    fn name(&self) -> &str;

    fn capabilities(&self) -> SpecialtyTableCapabilities;

    fn dimensions(&self) -> usize;

    fn metric(&self) -> VectorMetric;

    fn insert_vector(
        &self,
        id: &[u8],
        vector: &[f32],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()>;

    fn delete_vector(
        &self,
        id: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()>;

    fn search_vector<'a>(
        &self,
        query: &[f32],
        options: VectorSearchOptions<'a>,
    ) -> TableResult<Vec<VectorHit>>;

    fn stats(&self) -> TableResult<SpecialtyTableStats>;

    fn verify(&self) -> TableResult<VerificationReport>;
}

/// HNSW-specific controls.
pub trait HnswVector: VectorSearch {
    fn set_ef_construction(&self, ef: usize);

    fn set_max_connections(&self, m: usize);
}

/// IVF-specific controls.
pub trait IvfVector: VectorSearch {
    fn train(&mut self, samples: &[&[f32]]) -> TableResult<()>;

    fn centroid_count(&self) -> usize;
}

/// Graph adjacency table optimized for incoming/outgoing edge traversal.
/// Renamed from GraphAdjacencyIndex to reflect unified table architecture.
pub trait GraphAdjacency {
    type EdgeCursor<'a>: EdgeCursor
    where
        Self: 'a;

    fn table_id(&self) -> TableId;

    fn name(&self) -> &str;

    fn capabilities(&self) -> SpecialtyTableCapabilities;

    fn add_edge(
        &self,
        source: &[u8],
        label: &[u8],
        target: &[u8],
        edge_id: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()>;

    /// Add an edge with an optional weight.
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
        // Default implementation ignores weight
        self.add_edge(source, label, target, edge_id, tx_id, commit_lsn)
    }

    fn remove_edge(
        &self,
        source: &[u8],
        label: &[u8],
        target: &[u8],
        edge_id: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()>;

    fn outgoing(&self, source: &[u8], label: Option<&[u8]>) -> TableResult<Self::EdgeCursor<'_>>;

    fn incoming(&self, target: &[u8], label: Option<&[u8]>) -> TableResult<Self::EdgeCursor<'_>>;

    fn stats(&self) -> TableResult<SpecialtyTableStats>;

    fn verify(&self) -> TableResult<VerificationReport>;
}

/// Time-series table optimized for append, range, retention, and latest-before queries.
/// Renamed from TimeSeriesIndex to reflect unified table architecture.
pub trait TimeSeries {
    type TimeSeriesCursor<'a>: TimeSeriesCursor
    where
        Self: 'a;

    fn table_id(&self) -> TableId;

    fn name(&self) -> &str;

    fn capabilities(&self) -> SpecialtyTableCapabilities;

    fn append_point(
        &self,
        series_key: &[u8],
        timestamp: i64,
        value_key: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()>;

    fn scan_series(
        &self,
        series_key: &[u8],
        start_ts: i64,
        end_ts: i64,
    ) -> TableResult<Self::TimeSeriesCursor<'_>>;

    fn latest_before(&self, series_key: &[u8], timestamp: i64)
    -> TableResult<Option<TimePointRef>>;

    fn stats(&self) -> TableResult<SpecialtyTableStats>;

    fn verify(&self) -> TableResult<VerificationReport>;
}

/// Geospatial table abstraction for point and region queries.
/// Renamed from GeoSpatialIndex to reflect unified table architecture.
pub trait GeoSpatial {
    fn table_id(&self) -> TableId;

    fn name(&self) -> &str;

    fn capabilities(&self) -> SpecialtyTableCapabilities;

    fn insert_geometry(
        &self,
        id: &[u8],
        geometry: GeometryRef<'_>,
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()>;

    fn delete_geometry(
        &self,
        id: &[u8],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()>;

    fn intersects(&self, query: GeometryRef<'_>, limit: usize) -> TableResult<Vec<GeoHit>>;

    fn nearest(&self, point: GeoPoint, limit: usize) -> TableResult<Vec<GeoHit>>;

    fn stats(&self) -> TableResult<SpecialtyTableStats>;

    fn verify(&self) -> TableResult<VerificationReport>;
}

/// Incremental rebuild lifecycle for specialty tables that may become stale.
pub trait Rebuildable {
    fn table_id(&self) -> TableId;

    fn mark_stale(&mut self) -> TableResult<()>;

    fn is_stale(&self) -> bool;

    fn rebuild(&mut self, source: &dyn SpecialtyTableSource) -> TableResult<()>;

    fn rebuild_incremental(
        &mut self,
        source: &dyn SpecialtyTableSource,
        budget: RebuildBudget,
    ) -> TableResult<RebuildProgress>;
}

// =============================================================================
// Query planning and cost estimation for specialty tables
// =============================================================================

/// Common interface for specialty tables that can participate in query planning.
pub trait QueryablePredicate {
    fn table_id(&self) -> TableId;

    fn estimate(&self, predicate: Predicate<'_>) -> TableResult<CostEstimate>;

    fn query_candidates(
        &self,
        predicate: Predicate<'_>,
        budget: QueryBudget,
    ) -> TableResult<CandidateSet>;
}

/// Predicate understood by the generic query-planning layer.
#[derive(Clone, Debug)]
pub enum Predicate<'a> {
    Eq {
        field: std::borrow::Cow<'a, str>,
        value: std::borrow::Cow<'a, [u8]>,
    },
    Range {
        field: std::borrow::Cow<'a, str>,
        start: crate::types::Bound<std::borrow::Cow<'a, [u8]>>,
        end: crate::types::Bound<std::borrow::Cow<'a, [u8]>>,
    },
    Prefix {
        field: std::borrow::Cow<'a, str>,
        prefix: std::borrow::Cow<'a, [u8]>,
    },
    Text {
        field: Option<std::borrow::Cow<'a, str>>,
        query: std::borrow::Cow<'a, str>,
    },
    VectorKnn {
        field: std::borrow::Cow<'a, str>,
        vector: std::borrow::Cow<'a, [f32]>,
        k: usize,
    },
    GeoIntersects {
        field: std::borrow::Cow<'a, str>,
        geometry: GeometryRef<'a>,
    },
    /// Check if a field value is NULL.
    IsNull {
        field: std::borrow::Cow<'a, str>,
    },
    /// Check if a field value is NOT NULL.
    IsNotNull {
        field: std::borrow::Cow<'a, str>,
    },
    /// Check if a field value is in a set of values.
    In {
        field: std::borrow::Cow<'a, str>,
        values: Vec<std::borrow::Cow<'a, [u8]>>,
    },
    /// Check if a field value is between two bounds (inclusive).
    Between {
        field: std::borrow::Cow<'a, str>,
        low: std::borrow::Cow<'a, [u8]>,
        high: std::borrow::Cow<'a, [u8]>,
    },
    And(Vec<Predicate<'a>>),
    Or(Vec<Predicate<'a>>),
    Not(Box<Predicate<'a>>),
}

/// Cost/selectivity estimate for query planning.
#[derive(Clone, Debug, Default)]
pub struct CostEstimate {
    pub estimated_rows: Option<u64>,
    pub selectivity: Option<f64>,
    pub io_cost: Option<f64>,
    pub cpu_cost: Option<f64>,
    pub memory_cost_bytes: Option<u64>,
    pub exact: bool,
    pub ordered: bool,
}

/// Candidate primary-key set produced by a specialty table.
#[derive(Clone, Debug)]
pub enum CandidateSet {
    Exact(Vec<KeyBuf>),
    Approximate(Vec<KeyBuf>),
    PhysicalRanges(Vec<PhysicalRange>),
    Empty,
    Unknown,
}

/// Query budget for approximate or incremental specialty table queries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueryBudget {
    pub max_results: Option<usize>,
    pub max_pages: Option<u64>,
    pub max_millis: Option<u64>,
}

// =============================================================================
// Supporting types for specialty tables
// =============================================================================

/// Source abstraction used to rebuild specialty tables.
///
/// Returns an iterator over (primary_key, value) pairs, enabling pausable
/// iteration for incremental rebuilds with budget constraints.
///
/// # Error Handling
///
/// The `SpecialtyTableSourceError` enum preserves the original error type information,
/// allowing rebuild logic to distinguish between different error categories:
///
/// - `TableScan`: Errors from the underlying table scan operation
/// - `Io`: I/O errors (transient failures, disk full, etc.)
/// - `InvalidData`: Corrupt or malformed data encountered during scan
/// - `Cancelled`: Scan was interrupted or cancelled
/// - `Other`: Other source-specific errors
///
/// This enables proper error handling strategies such as:
/// - Retrying transient I/O failures
/// - Marking specialty tables as stale on corruption
/// - Distinguishing recoverable from fatal errors
pub trait SpecialtyTableSource {
    fn scan_rows(
        &self,
        bounds: ScanBounds,
    ) -> TableResult<
        Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>), SpecialtyTableSourceError>> + '_>,
    >;
}

/// Errors that can occur when scanning table data for specialty table rebuilds.
///
/// This enum preserves the original error type information, enabling rebuild
/// logic to distinguish between transient I/O failures, corruption, and other
/// error categories for proper error handling and retry strategies.
#[derive(Debug, thiserror::Error)]
pub enum SpecialtyTableSourceError {
    /// Table scan operation failed
    #[error("Table scan failed: {0}")]
    TableScan(#[from] crate::table::TableError),

    /// I/O error during scan
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Invalid data encountered during scan
    #[error("Invalid data: {0}")]
    InvalidData(String),

    /// Scan was cancelled or interrupted
    #[error("Scan cancelled: {0}")]
    Cancelled(String),

    /// Other source error
    #[error("Source error: {0}")]
    Other(String),
}

#[derive(Clone, Debug)]
pub struct SparseQuery<'a> {
    pub key_range: Option<(&'a [u8], &'a [u8])>,
    pub min_max_filter: Option<(&'a [u8], &'a [u8])>,
}

#[derive(Clone, Debug)]
pub struct TextField<'a> {
    pub name: &'a str,
    pub text: &'a str,
    pub boost: f32,
}

#[derive(Clone, Debug)]
pub struct TextQuery<'a> {
    pub query: &'a str,
    pub default_field: Option<&'a str>,
    pub require_positions: bool,
}

#[derive(Clone, Debug)]
pub struct ScoredDocument {
    pub doc_id: KeyBuf,
    pub score: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorMetric {
    Cosine,
    Dot,
    Euclidean,
    Manhattan,
}

#[derive(Clone, Debug)]
pub struct VectorSearchOptions<'a> {
    pub limit: usize,
    pub ef_search: Option<usize>,
    pub probes: Option<usize>,
    pub filter: Option<Predicate<'a>>,
}

#[derive(Clone, Debug)]
pub struct VectorHit {
    pub id: KeyBuf,
    /// Distance metric value (lower is closer for most metrics).
    /// Uses f32 for consistency with vector data representation.
    pub distance: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeRef {
    pub edge_id: KeyBuf,
    pub source: KeyBuf,
    pub label: KeyBuf,
    pub target: KeyBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimePointRef {
    pub series_key: KeyBuf,
    pub timestamp: i64,
    pub value_key: KeyBuf,
}

/// Geographic point with double-precision coordinates.
///
/// Uses f64 (not f32) for geographic coordinates because:
/// - Standard geographic coordinate systems (WGS84, etc.) are defined with double precision
/// - f32 provides only ~7 decimal digits, which is ~11m precision at the equator
/// - f64 provides ~15 decimal digits, which is ~1mm precision - essential for accurate GIS
/// - Geospatial standards (GeoJSON, WKT, etc.) use double precision
/// - The precision loss from f32 accumulates in distance calculations and transformations
///
/// In contrast, VectorSearch uses f32 because:
/// - Vector embeddings are approximate by nature (trained with noise/quantization)
/// - Memory efficiency is critical for large vector collections
/// - Distance calculations are relative, not absolute measurements
/// - Most ML frameworks produce f32 embeddings
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug)]
pub enum GeometryRef<'a> {
    Point(GeoPoint),
    BoundingBox { min: GeoPoint, max: GeoPoint },
    Wkb(&'a [u8]),
}

#[derive(Clone, Debug)]
pub struct GeoHit {
    pub id: KeyBuf,
    /// Distance in meters (or coordinate system units).
    /// Uses f32 for consistency with VectorHit and memory efficiency.
    /// Optional because some queries (intersects) don't compute distance.
    pub distance: Option<f32>,
}

/// Declared specialty table capabilities.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpecialtyTableCapabilities {
    pub exact: bool,
    pub approximate: bool,
    pub ordered: bool,
    pub sparse: bool,
    pub supports_delete: bool,
    pub supports_range_query: bool,
    pub supports_prefix_query: bool,
    pub supports_scoring: bool,
    pub supports_incremental_rebuild: bool,
    pub may_be_stale: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SpecialtyTableStats {
    pub entry_count: Option<u64>,
    pub size_bytes: Option<u64>,
    pub distinct_keys: Option<u64>,
    pub stale_entries: Option<u64>,
    pub last_updated_lsn: Option<LogSequenceNumber>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RebuildBudget {
    pub max_rows: Option<u64>,
    pub max_pages: Option<u64>,
    pub max_millis: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct RebuildProgress {
    pub complete: bool,
    pub rows_scanned: u64,
    pub rows_indexed: u64,
    pub resume_key: Option<KeyBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalRange {
    pub start: PhysicalLocation,
    pub end: PhysicalLocation,
}
