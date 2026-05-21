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
pub mod appendlog;
pub mod art;
pub mod blob;
pub mod bloom;
pub mod btree;
mod composite;
mod error;
pub mod fulltext;
pub mod graph;
pub mod hash;
pub mod hnsw;
pub mod lsm;
pub mod metrics;
pub mod rtree;
pub mod timeseries;
mod traits;

pub use self::appendlog::{AppendLog, AppendLogConfig, CompressionType, RetentionPolicy};
pub use self::art::{MemoryART, MemoryARTReader, MemoryARTWriter};
pub use self::blob::{FileBlob, MemoryBlob, PagedBlob};
pub use self::bloom::{BloomFilter, BloomFilterBuilder, PagedBloomFilter};
pub use self::btree::{MemoryBTree, PagedBTree};
pub use self::composite::{
    CompositeIndexBuilder, CompositeIndexConfig, CompositeQueryExecutor, CompositeQueryResult,
    CompositionStrategy, SecondaryRole, SecondaryTableConfig, TableConsulted,
};
pub use self::error::{TableError, TableResult};
pub use self::fulltext::{
    FullTextConfig, PagedFullTextIndex, Tokenizer, TokenizerConfig, TokenizerKind,
};
pub use self::graph::{GraphConfig, GraphStorageBackend, MemoryGraphTable};
pub use self::hash::{MemoryHashTable, MemoryHashTableReader, MemoryHashTableWriter};
pub use self::hnsw::{HnswConfig, PagedHnswVector};
pub use self::lsm::{
    CompactionConfig, CompactionStrategy, LevelConfig, LsmConfig, Memtable, MemtableConfig,
    MemtableType, SStableConfig,
};
pub use self::rtree::{PagedRTree, SpatialConfig, SplitStrategy};
pub use self::timeseries::{
    TimeSeriesCompression, TimeSeriesConfig, TimeSeriesRetentionPolicy, TimeSeriesTable,
};
pub use self::traits::{
    // Specialty table traits (formerly index traits)
    ApproximateMembership,
    // Core table traits
    BatchOps,
    BatchReport,
    CandidateSet,
    CheckpointInfo,
    CompactionOptions,
    CompactionReport,
    ConsistencyError,
    ConsistencyErrorType,
    ConsistencyVerifier,
    ConsistencyWarning,
    CostEstimate,
    DenseOrdered,
    EdgeCursor,
    EdgeRef,
    EvictableCache,
    Flushable,
    FullTextSearch,
    GeoHit,
    GeoPoint,
    GeoSpatial,
    GeometryRef,
    GraphAdjacency,
    Histogram,
    HistogramBucket,
    HnswVector,
    IvfVector,
    KeyStatistics,
    Maintainable,
    MemoryAware,
    Migratable,
    MutableTable,
    Mutation,
    OrderedKvTable,
    OrderedScan,
    PhysicalRange,
    PointLookup,
    Predicate,
    PrefixScan,
    QueryBudget,
    QueryablePredicate,
    RebuildBudget,
    RebuildProgress,
    Rebuildable,
    RepairAction,
    RepairPlan,
    RepairReport,
    ScoredDocument,
    SearchableTable,
    Severity,
    SliceValueStream,
    SparseOrdered,
    SparseQuery,
    SpecialtyTableCapabilities,
    SpecialtyTableCursor,
    SpecialtyTableSource,
    SpecialtyTableSourceError,
    SpecialtyTableStats,
    StatisticsProvider,
    Table,
    TableCapabilities,
    TableCursor,
    TableEngineKind,
    TableInfo,
    TableOptions,
    TableReader,
    TableStatistics,
    TableWriter,
    TextField,
    TextQuery,
    TimePointRef,
    TimeSeries,
    TimeSeriesCursor,
    VacuumOptions,
    VacuumReport,
    ValueStatistics,
    ValueStream,
    VectorHit,
    VectorMetric,
    VectorSearch,
    VectorSearchOptions,
    VerificationReport,
    VerifyScope,
    WorkBudget,
    WriteBatch,
};
use crate::pager::{PageId, Pager};
use crate::table::lsm::LsmTree;
use crate::types::TableId;
use crate::vfs::FileSystem;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock};

// =============================================================================
// Table Engine Wrapper
// =============================================================================

/// Wrapper for table engine instances that allows storing different engine
/// types in the same collection.
pub enum TableEngineInstance<FS: FileSystem> {
    AppendLog(Arc<AppendLog<FS>>),
    PagedBTree(Arc<PagedBTree<FS>>),
    LsmTree(Arc<LsmTree<FS>>),
    PagedBloomFilter(Arc<PagedBloomFilter<FS>>),
    PagedHnswVector(Arc<PagedHnswVector<FS>>),
    PagedRTree(Arc<PagedRTree<FS>>),
    TimeSeriesTable(Arc<TimeSeriesTable<FS>>),
    PagedFullTextIndex(Arc<PagedFullTextIndex<FS>>),
    PagedBlob(Arc<PagedBlob<FS>>),
    MemoryBTree(Arc<MemoryBTree>),
    MemoryHashTable(Arc<MemoryHashTable>),
    MemoryGraphTable(Arc<MemoryGraphTable>),
    MemoryBlob(Arc<MemoryBlob>),
    MemoryART(Arc<MemoryART>),
}

impl<FS: FileSystem> TableEngineInstance<FS> {
    /// Get the table ID.
    #[must_use]
    pub fn table_id(&self) -> TableId {
        match self {
            Self::AppendLog(engine) => crate::table::Table::table_id(engine.as_ref()),
            Self::PagedBTree(engine) => crate::table::Table::table_id(engine.as_ref()),
            Self::LsmTree(engine) => crate::table::Table::table_id(engine.as_ref()),
            Self::PagedBloomFilter(engine) => crate::table::Table::table_id(engine.as_ref()),
            Self::PagedHnswVector(engine) => crate::table::Table::table_id(engine.as_ref()),
            Self::PagedRTree(engine) => crate::table::Table::table_id(engine.as_ref()),
            Self::TimeSeriesTable(engine) => crate::table::Table::table_id(engine.as_ref()),
            Self::PagedFullTextIndex(engine) => crate::table::Table::table_id(engine.as_ref()),
            Self::PagedBlob(engine) => crate::table::Table::table_id(engine.as_ref()),
            Self::MemoryBTree(engine) => crate::table::Table::table_id(engine.as_ref()),
            Self::MemoryHashTable(engine) => crate::table::Table::table_id(engine.as_ref()),
            Self::MemoryGraphTable(engine) => crate::table::Table::table_id(engine.as_ref()),
            Self::MemoryBlob(engine) => crate::table::Table::table_id(engine.as_ref()),
            Self::MemoryART(engine) => crate::table::Table::table_id(engine.as_ref()),
        }
    }

    /// Get the table name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::AppendLog(engine) => crate::table::Table::name(engine.as_ref()),
            Self::PagedBTree(engine) => crate::table::Table::name(engine.as_ref()),
            Self::LsmTree(engine) => crate::table::Table::name(engine.as_ref()),
            Self::PagedBloomFilter(engine) => crate::table::Table::name(engine.as_ref()),
            Self::PagedHnswVector(engine) => crate::table::Table::name(engine.as_ref()),
            Self::PagedRTree(engine) => crate::table::Table::name(engine.as_ref()),
            Self::TimeSeriesTable(engine) => crate::table::Table::name(engine.as_ref()),
            Self::PagedFullTextIndex(engine) => crate::table::Table::name(engine.as_ref()),
            Self::PagedBlob(engine) => crate::table::Table::name(engine.as_ref()),
            Self::MemoryBTree(engine) => crate::table::Table::name(engine.as_ref()),
            Self::MemoryHashTable(engine) => crate::table::Table::name(engine.as_ref()),
            Self::MemoryGraphTable(engine) => crate::table::Table::name(engine.as_ref()),
            Self::MemoryBlob(engine) => crate::table::Table::name(engine.as_ref()),
            Self::MemoryART(engine) => crate::table::Table::name(engine.as_ref()),
        }
    }

    /// Get the engine kind.
    #[must_use]
    pub fn kind(&self) -> TableEngineKind {
        match self {
            Self::AppendLog(engine) => engine.kind(),
            Self::PagedBTree(engine) => engine.kind(),
            Self::LsmTree(engine) => engine.kind(),
            Self::PagedBloomFilter(engine) => engine.kind(),
            Self::PagedHnswVector(engine) => engine.kind(),
            Self::PagedRTree(engine) => engine.kind(),
            Self::TimeSeriesTable(engine) => engine.kind(),
            Self::PagedFullTextIndex(engine) => engine.kind(),
            Self::PagedBlob(engine) => engine.kind(),
            Self::MemoryBTree(engine) => engine.kind(),
            Self::MemoryHashTable(engine) => engine.kind(),
            Self::MemoryGraphTable(engine) => engine.kind(),
            Self::MemoryBlob(engine) => engine.kind(),
            Self::MemoryART(engine) => engine.kind(),
        }
    }

    /// Get the root page ID for persistent engines.
    #[must_use]
    pub fn root_page_id(&self) -> Option<PageId> {
        match self {
            Self::AppendLog(engine) => Some(engine.root_page_id()),
            Self::PagedBTree(engine) => Some(engine.get_root_page_id()),
            Self::LsmTree(_) => None, // LSM has manifest, not single root
            Self::PagedBloomFilter(engine) => Some(engine.root_page_id()),
            Self::PagedHnswVector(engine) => Some(engine.root_page_id()),
            Self::PagedRTree(engine) => Some(engine.root_page_id()),
            Self::TimeSeriesTable(engine) => Some(engine.root_page_id()),
            Self::PagedFullTextIndex(engine) => Some(engine.root_page_id()),
            Self::PagedBlob(engine) => Some(engine.root_page_id()),
            Self::MemoryBTree(_) => None,
            Self::MemoryHashTable(_) => None,
            Self::MemoryGraphTable(_) => None,
            Self::MemoryBlob(_) => None,
            Self::MemoryART(_) => None,
        }
    }
}

impl<FS: FileSystem> Clone for TableEngineInstance<FS> {
    fn clone(&self) -> Self {
        match self {
            Self::AppendLog(engine) => Self::AppendLog(Arc::clone(engine)),
            Self::PagedBTree(engine) => Self::PagedBTree(Arc::clone(engine)),
            Self::LsmTree(engine) => Self::LsmTree(Arc::clone(engine)),
            Self::PagedBloomFilter(engine) => Self::PagedBloomFilter(Arc::clone(engine)),
            Self::PagedHnswVector(engine) => Self::PagedHnswVector(Arc::clone(engine)),
            Self::PagedRTree(engine) => Self::PagedRTree(Arc::clone(engine)),
            Self::TimeSeriesTable(engine) => Self::TimeSeriesTable(Arc::clone(engine)),
            Self::PagedFullTextIndex(engine) => Self::PagedFullTextIndex(Arc::clone(engine)),
            Self::PagedBlob(engine) => Self::PagedBlob(Arc::clone(engine)),
            Self::MemoryBTree(engine) => Self::MemoryBTree(Arc::clone(engine)),
            Self::MemoryHashTable(engine) => Self::MemoryHashTable(Arc::clone(engine)),
            Self::MemoryGraphTable(engine) => Self::MemoryGraphTable(Arc::clone(engine)),
            Self::MemoryBlob(engine) => Self::MemoryBlob(Arc::clone(engine)),
            Self::MemoryART(engine) => Self::MemoryART(Arc::clone(engine)),
        }
    }
}

// =============================================================================
// Table Engine Registry
// =============================================================================

/// Registry for managing table engine instances.
///
/// The registry maintains a mapping from ObjectId to table engine instances,
/// providing factory methods for creating different engine types and managing
/// their lifecycle.
pub struct TableEngineRegistry<FS: FileSystem> {
    /// Map of table/index ID to engine instance
    engines: RwLock<HashMap<TableId, TableEngineInstance<FS>>>,

    /// Pager for persistent engines
    pager: Arc<Pager<FS>>,
}

impl<FS: FileSystem> TableEngineRegistry<FS> {
    /// Create a new table engine registry.
    #[must_use]
    pub fn new(pager: Arc<Pager<FS>>) -> Self {
        Self {
            engines: RwLock::new(HashMap::new()),
            pager,
        }
    }

    /// Create a new table engine instance.
    /// Returns the engine instance and its root page ID (if applicable).
    pub fn create_engine(
        &self,
        table_id: TableId,
        name: String,
        options: &TableOptions,
    ) -> Result<(TableEngineInstance<FS>, Option<PageId>), RegistryError> {
        match options.engine {
            TableEngineKind::AppendLog => {
                // Extract config from options or use default
                let config = options.appendlog_config.clone().unwrap_or_default();

                let appendlog = AppendLog::new(table_id, name, self.pager.clone(), config)
                    .map_err(|e| RegistryError::EngineCreationFailed {
                        engine: options.engine,
                        details: format!("Failed to create AppendLog: {}", e),
                    })?;
                let root_page_id = appendlog.root_page_id();
                Ok((
                    TableEngineInstance::AppendLog(Arc::new(appendlog)),
                    Some(root_page_id),
                ))
            }
            TableEngineKind::BTree => {
                // Create new BTree with allocated root page
                let btree = PagedBTree::new(table_id, name, self.pager.clone()).map_err(|e| {
                    RegistryError::EngineCreationFailed {
                        engine: options.engine,
                        details: format!("Failed to create BTree: {}", e),
                    }
                })?;
                let root_page_id = btree.get_root_page_id();
                Ok((
                    TableEngineInstance::PagedBTree(Arc::new(btree)),
                    Some(root_page_id),
                ))
            }
            TableEngineKind::LsmTree => {
                // Allocate root page for LSM manifest
                let root_page_id = self
                    .pager
                    .allocate_page(crate::pager::PageType::LsmMeta)
                    .map_err(|e| RegistryError::EngineCreationFailed {
                        engine: options.engine,
                        details: format!("Failed to allocate manifest page: {}", e),
                    })?;

                // Extract LSM config from options or use default
                let lsm_config = options.lsm_config.clone().unwrap_or_default();

                let lsm = crate::table::lsm::LsmTree::new(
                    table_id,
                    name,
                    self.pager.clone(),
                    root_page_id,
                    lsm_config,
                )
                .map_err(|e| RegistryError::EngineCreationFailed {
                    engine: options.engine,
                    details: format!("Failed to create LSM tree: {}", e),
                })?;
                Ok((
                    TableEngineInstance::LsmTree(Arc::new(lsm)),
                    Some(root_page_id),
                ))
            }
            TableEngineKind::Bloom => {
                // Extract Bloom filter parameters from options or use defaults
                let (num_items, bits_per_key) = options.bloom_config.unwrap_or((10000, 10));

                let bloom = PagedBloomFilter::new(
                    table_id,
                    name,
                    self.pager.clone(),
                    num_items,
                    bits_per_key,
                    None,
                )
                .map_err(|e| RegistryError::EngineCreationFailed {
                    engine: options.engine,
                    details: format!("Failed to create Bloom filter: {}", e),
                })?;
                let root_page_id = bloom.root_page_id();
                Ok((
                    TableEngineInstance::PagedBloomFilter(Arc::new(bloom)),
                    Some(root_page_id),
                ))
            }
            TableEngineKind::Memory => {
                // Create in-memory BTree - no root page
                let memory_btree = MemoryBTree::new(table_id, name);
                Ok((
                    TableEngineInstance::MemoryBTree(Arc::new(memory_btree)),
                    None,
                ))
            }
            TableEngineKind::Hash => {
                // Create in-memory hash table - no root page
                let hash_table = MemoryHashTable::new(table_id, name);
                Ok((
                    TableEngineInstance::MemoryHashTable(Arc::new(hash_table)),
                    None,
                ))
            }
            TableEngineKind::Art => {
                // Create in-memory ART - no root page
                let art = MemoryART::new(table_id, name);
                Ok((TableEngineInstance::MemoryART(Arc::new(art)), None))
            }
            TableEngineKind::GeoSpatial => {
                // Extract spatial config from options or use default
                let spatial_config = options.spatial_config.clone().unwrap_or_default();

                let rtree = PagedRTree::new(table_id, name, self.pager.clone(), spatial_config)
                    .map_err(|e| RegistryError::EngineCreationFailed {
                        engine: options.engine,
                        details: format!("Failed to create R-Tree: {}", e),
                    })?;
                let root_page_id = rtree.root_page_id();
                Ok((
                    TableEngineInstance::PagedRTree(Arc::new(rtree)),
                    Some(root_page_id),
                ))
            }
            TableEngineKind::TimeSeries => {
                // Extract TimeSeries config from options or use default
                let config = options.timeseries_config.clone().unwrap_or_default();

                let timeseries = TimeSeriesTable::new(table_id, name, self.pager.clone(), config)
                    .map_err(|e| RegistryError::EngineCreationFailed {
                    engine: options.engine,
                    details: format!("Failed to create TimeSeries: {}", e),
                })?;
                let root_page_id = timeseries.root_page_id();
                Ok((
                    TableEngineInstance::TimeSeriesTable(Arc::new(timeseries)),
                    Some(root_page_id),
                ))
            }
            TableEngineKind::FullText => {
                // Extract FullText config from options or use default
                let config = options.fulltext_config.clone().unwrap_or_default();

                let fulltext = PagedFullTextIndex::new(table_id, name, self.pager.clone(), config)
                    .map_err(|e| RegistryError::EngineCreationFailed {
                        engine: options.engine,
                        details: format!("Failed to create FullText index: {}", e),
                    })?;
                let root_page_id = fulltext.root_page_id();
                Ok((
                    TableEngineInstance::PagedFullTextIndex(Arc::new(fulltext)),
                    Some(root_page_id),
                ))
            }
            TableEngineKind::Blob => {
                // Create PagedBlob
                let blob = PagedBlob::new(table_id, name, self.pager.clone()).map_err(|e| {
                    RegistryError::EngineCreationFailed {
                        engine: options.engine,
                        details: format!("Failed to create PagedBlob: {}", e),
                    }
                })?;
                let root_page_id = blob.root_page_id();
                Ok((
                    TableEngineInstance::PagedBlob(Arc::new(blob)),
                    Some(root_page_id),
                ))
            }
            _ => Err(RegistryError::UnsupportedEngine(options.engine)),
        }
    }

    /// Open an existing table engine instance.
    pub fn open_engine(
        &self,
        table_id: TableId,
        name: String,
        options: &TableOptions,
        root_page_id: PageId,
    ) -> Result<TableEngineInstance<FS>, RegistryError> {
        match options.engine {
            TableEngineKind::AppendLog => {
                // Extract config from options or use default
                let config = options.appendlog_config.clone().unwrap_or_default();
                let appendlog =
                    AppendLog::open(table_id, name, self.pager.clone(), root_page_id, config)
                        .map_err(|e| RegistryError::EngineOpenFailed {
                            engine: options.engine,
                            details: format!("Failed to open AppendLog: {}", e),
                        })?;
                Ok(TableEngineInstance::AppendLog(Arc::new(appendlog)))
            }
            TableEngineKind::BTree => {
                let btree = PagedBTree::open(table_id, name, self.pager.clone(), root_page_id);
                Ok(TableEngineInstance::PagedBTree(Arc::new(btree)))
            }
            TableEngineKind::LsmTree => {
                // Extract LSM config from options or use default
                let lsm_config = options.lsm_config.clone().unwrap_or_default();
                let lsm = crate::table::lsm::LsmTree::open(
                    table_id,
                    name,
                    self.pager.clone(),
                    root_page_id,
                    lsm_config,
                )
                .map_err(|e| RegistryError::EngineOpenFailed {
                    engine: options.engine,
                    details: format!("Failed to open LSM tree: {}", e),
                })?;
                Ok(TableEngineInstance::LsmTree(Arc::new(lsm)))
            }
            TableEngineKind::Bloom => {
                let bloom =
                    PagedBloomFilter::open(table_id, name, self.pager.clone(), root_page_id)
                        .map_err(|e| RegistryError::EngineOpenFailed {
                            engine: options.engine,
                            details: format!("Failed to open Bloom filter: {}", e),
                        })?;
                Ok(TableEngineInstance::PagedBloomFilter(Arc::new(bloom)))
            }
            TableEngineKind::GeoSpatial => {
                // Extract spatial config from options or use default
                let spatial_config = options.spatial_config.clone().unwrap_or_default();
                let rtree = PagedRTree::open(
                    table_id,
                    name,
                    self.pager.clone(),
                    root_page_id,
                    spatial_config,
                )
                .map_err(|e| RegistryError::EngineOpenFailed {
                    engine: options.engine,
                    details: format!("Failed to open R-Tree: {}", e),
                })?;
                Ok(TableEngineInstance::PagedRTree(Arc::new(rtree)))
            }
            TableEngineKind::TimeSeries => {
                // Extract TimeSeries config from options or use default
                let config = options.timeseries_config.clone().unwrap_or_default();
                let timeseries =
                    TimeSeriesTable::open(table_id, name, self.pager.clone(), root_page_id, config)
                        .map_err(|e| RegistryError::EngineOpenFailed {
                            engine: options.engine,
                            details: format!("Failed to open TimeSeries: {}", e),
                        })?;
                Ok(TableEngineInstance::TimeSeriesTable(Arc::new(timeseries)))
            }
            TableEngineKind::FullText => {
                let fulltext =
                    PagedFullTextIndex::open(table_id, name, self.pager.clone(), root_page_id)
                        .map_err(|e| RegistryError::EngineOpenFailed {
                            engine: options.engine,
                            details: format!("Failed to open FullText index: {}", e),
                        })?;
                Ok(TableEngineInstance::PagedFullTextIndex(Arc::new(fulltext)))
            }
            TableEngineKind::Blob => {
                let blob = PagedBlob::open(table_id, name, self.pager.clone(), root_page_id);
                Ok(TableEngineInstance::PagedBlob(Arc::new(blob)))
            }
            TableEngineKind::Memory => {
                // Memory tables don't persist, create new
                let memory_btree = MemoryBTree::new(table_id, name);
                Ok(TableEngineInstance::MemoryBTree(Arc::new(memory_btree)))
            }
            TableEngineKind::Hash => {
                // Hash tables don't persist, create new
                let hash_table = MemoryHashTable::new(table_id, name);
                Ok(TableEngineInstance::MemoryHashTable(Arc::new(hash_table)))
            }
            TableEngineKind::Art => {
                // ART tables don't persist, create new
                let art = MemoryART::new(table_id, name);
                Ok(TableEngineInstance::MemoryART(Arc::new(art)))
            }
            _ => Err(RegistryError::UnsupportedEngine(options.engine)),
        }
    }

    /// Register a table engine instance.
    pub fn register(&self, engine: TableEngineInstance<FS>) -> Result<(), RegistryError> {
        let mut engines = self.engines.write().unwrap();
        let table_id = engine.table_id();

        if engines.contains_key(&table_id) {
            return Err(RegistryError::EngineAlreadyRegistered(table_id));
        }

        engines.insert(table_id, engine);
        Ok(())
    }

    /// Get a table engine instance.
    pub fn get(&self, table_id: TableId) -> Option<TableEngineInstance<FS>> {
        let engines = self.engines.read().unwrap();
        engines.get(&table_id).cloned()
    }

    /// Remove a table engine instance.
    pub fn remove(&self, table_id: TableId) -> Option<TableEngineInstance<FS>> {
        let mut engines = self.engines.write().unwrap();
        engines.remove(&table_id)
    }

    /// List all registered engines.
    pub fn list(&self) -> Vec<TableEngineInstance<FS>> {
        let engines = self.engines.read().unwrap();
        engines.values().cloned().collect()
    }

    /// Get the number of registered engines.
    pub fn count(&self) -> usize {
        let engines = self.engines.read().unwrap();
        engines.len()
    }

    /// Check if an engine is registered.
    pub fn contains(&self, table_id: TableId) -> bool {
        let engines = self.engines.read().unwrap();
        engines.contains_key(&table_id)
    }

    /// Vacuum a table to remove obsolete version chains.
    ///
    /// This method delegates to the appropriate engine's vacuum implementation.
    /// Not all engines support vacuuming (e.g., `AppendLog`, Bloom filters).
    ///
    /// # Arguments
    ///
    /// * `table_id` - The table to vacuum
    /// * `min_visible_lsn` - The minimum LSN that must remain visible
    ///
    /// # Returns
    ///
    /// Returns the number of versions removed, or an error if the table doesn't
    /// exist or doesn't support vacuuming.
    pub fn vacuum_table(
        &self,
        table_id: TableId,
        min_visible_lsn: crate::wal::LogSequenceNumber,
    ) -> Result<usize, crate::engine::StorageEngineError> {
        let engine = self
            .get(table_id)
            .ok_or_else(|| crate::engine::StorageEngineError::not_found(table_id))?;

        match engine {
            TableEngineInstance::PagedBTree(btree) => {
                // PagedBTree supports vacuum
                btree.vacuum(min_visible_lsn).map_err(|e| {
                    crate::engine::StorageEngineError::other(format!("Vacuum failed: {}", e))
                })
            }
            TableEngineInstance::MemoryBTree(btree) => {
                // MemoryBTree supports vacuum
                btree.vacuum(min_visible_lsn).map_err(|e| {
                    crate::engine::StorageEngineError::other(format!("Vacuum failed: {}", e))
                })
            }
            TableEngineInstance::MemoryHashTable(hash) => {
                // MemoryHashTable supports vacuum
                hash.vacuum(min_visible_lsn).map_err(|e| {
                    crate::engine::StorageEngineError::other(format!("Vacuum failed: {}", e))
                })
            }
            TableEngineInstance::MemoryART(art) => {
                // MemoryART supports vacuum
                art.vacuum(min_visible_lsn).map_err(|e| {
                    crate::engine::StorageEngineError::other(format!("Vacuum failed: {}", e))
                })
            }
            TableEngineInstance::LsmTree(lsm) => {
                // LsmTree supports vacuum
                lsm.vacuum(min_visible_lsn).map_err(|e| {
                    crate::engine::StorageEngineError::other(format!("Vacuum failed: {}", e))
                })
            }
            TableEngineInstance::MemoryGraphTable(graph) => {
                // MemoryGraphTable supports vacuum
                graph.vacuum(min_visible_lsn).map_err(|e| {
                    crate::engine::StorageEngineError::other(format!("Vacuum failed: {}", e))
                })
            }
            TableEngineInstance::TimeSeriesTable(ts) => {
                // TimeSeriesTable supports vacuum
                ts.vacuum(min_visible_lsn).map_err(|e| {
                    crate::engine::StorageEngineError::other(format!("Vacuum failed: {}", e))
                })
            }
            TableEngineInstance::PagedBlob(blob) => {
                // PagedBlob supports vacuum
                blob.vacuum(min_visible_lsn).map_err(|e| {
                    crate::engine::StorageEngineError::other(format!("Vacuum failed: {}", e))
                })
            }
            // Engines that don't support vacuum
            TableEngineInstance::AppendLog(_) => {
                Err(crate::engine::StorageEngineError::invalid_operation(
                    "AppendLog does not support vacuum".to_string(),
                ))
            }
            TableEngineInstance::PagedBloomFilter(_) => {
                Err(crate::engine::StorageEngineError::invalid_operation(
                    "Bloom filters do not support vacuum".to_string(),
                ))
            }
            TableEngineInstance::PagedHnswVector(_) => {
                Err(crate::engine::StorageEngineError::invalid_operation(
                    "HNSW vectors do not support vacuum yet".to_string(),
                ))
            }
            TableEngineInstance::PagedRTree(_) => {
                Err(crate::engine::StorageEngineError::invalid_operation(
                    "R-Tree does not support vacuum yet".to_string(),
                ))
            }
            TableEngineInstance::PagedFullTextIndex(_) => {
                Err(crate::engine::StorageEngineError::invalid_operation(
                    "Full-text index does not support vacuum yet".to_string(),
                ))
            }
            TableEngineInstance::MemoryBlob(_) => {
                Err(crate::engine::StorageEngineError::invalid_operation(
                    "Blob storage does not support vacuum".to_string(),
                ))
            }
        }
    }

    /// Close all engines and clean up resources.
    pub fn close_all(&self) {
        let mut engines = self.engines.write().unwrap();
        engines.clear();
    }
}

// =============================================================================
// Registry Errors
// =============================================================================

/// Errors that can occur during table engine registry operations.
#[derive(Debug)]
pub enum RegistryError {
    /// Engine creation failed
    EngineCreationFailed {
        engine: TableEngineKind,
        details: String,
    },
    /// Engine open failed
    EngineOpenFailed {
        engine: TableEngineKind,
        details: String,
    },
    /// Engine already registered
    EngineAlreadyRegistered(TableId),
    /// Engine not found
    EngineNotFound(TableId),
    /// Unsupported engine type
    UnsupportedEngine(TableEngineKind),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EngineCreationFailed { engine, details } => {
                write!(f, "Failed to create {:?} engine: {}", engine, details)
            }
            Self::EngineOpenFailed { engine, details } => {
                write!(f, "Failed to open {:?} engine: {}", engine, details)
            }
            Self::EngineAlreadyRegistered(id) => {
                write!(f, "Engine {:?} is already registered", id)
            }
            Self::EngineNotFound(id) => {
                write!(f, "Engine {:?} not found in registry", id)
            }
            Self::UnsupportedEngine(engine) => {
                write!(f, "Unsupported engine type: {:?}", engine)
            }
        }
    }
}

impl std::error::Error for RegistryError {}

// Made with Bob
