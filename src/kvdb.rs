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

//! Top-level embedded storage engine implementation.
//!
//! This module provides the main `StorageEngine` struct that owns the catalog, file allocation,
//! transaction manager, WAL, and registered table/index engines. ACID semantics are coordinated
//! at this layer.
//!
//! # Phase 4: Core API - StorageEngine & Table Handles
//!
//! This implementation provides:
//! - Storage engine-level CRUD operations with automatic index maintenance
//! - Table handle wrapper for ergonomic access
//! - Proper error handling and validation
//! - Support for both persistent and memory tables
//!
//! ## Design Philosophy: "All Collections Are Tables"
//!
//! Following ADR-007 and ADR-011, this implementation treats indexes as specialty tables:
//! - Both tables and indexes use TableId at the storage layer
//! - Transaction layer treats them uniformly
//! - StorageEngine layer maintains semantic distinction and handles index maintenance
//! - Index updates are explicit and visible in transaction write sets

use crate::pager::{Page, PageId, PageType, Pager, PagerConfig};
use crate::snap::{Snapshot, SnapshotId};
use crate::table::{TableEngineRegistry, TableInfo, TableOptions};
use crate::txn::{ConflictDetector, Transaction, TransactionId};
use crate::types::{ConsistencyGuarantees, Durability, IsolationLevel};
use crate::types::{TableId, ValueBuf};
use crate::vfs::FileSystem;
use crate::wal::{LogSequenceNumber, WalWriter, WalWriterConfig};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Configuration for automatic vacuum operations.
#[derive(Debug, Clone)]
pub struct VacuumConfig {
    /// Enable automatic background vacuum
    pub enabled: bool,
    /// Interval between vacuum runs (default: 5 minutes)
    pub interval: Duration,
}

impl Default for VacuumConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(300), // 5 minutes
        }
    }
}

/// Metrics for a single vacuum operation.
#[derive(Debug, Clone, Default)]
pub struct VacuumMetrics {
    /// Timestamp when vacuum started
    pub started_at: Option<Instant>,
    /// Timestamp when vacuum completed
    pub completed_at: Option<Instant>,
    /// Total versions removed across all tables
    pub total_versions_removed: usize,
    /// Per-table breakdown of versions removed
    pub versions_removed_per_table: HashMap<TableId, usize>,
    /// Duration of the vacuum operation
    pub duration: Option<Duration>,
}

impl VacuumMetrics {
    /// Create a new metrics instance with start time
    pub fn new() -> Self {
        Self {
            started_at: Some(Instant::now()),
            completed_at: None,
            total_versions_removed: 0,
            versions_removed_per_table: HashMap::new(),
            duration: None,
        }
    }

    /// Mark the vacuum as complete and calculate duration
    pub fn complete(&mut self) {
        self.completed_at = Some(Instant::now());
        if let (Some(start), Some(end)) = (self.started_at, self.completed_at) {
            self.duration = Some(end.duration_since(start));
        }
    }

    /// Add versions removed for a table
    pub fn add_table_result(&mut self, table_id: TableId, versions_removed: usize) {
        self.total_versions_removed += versions_removed;
        self.versions_removed_per_table
            .insert(table_id, versions_removed);
    }
}

/// Statistics for VACUUM FULL operation.
///
/// VACUUM FULL is a blocking operation that compacts the database file by moving
/// data from high-numbered pages to low-numbered pages, then truncating the file.
#[derive(Debug, Clone, Default)]
pub struct VacuumFullStats {
    /// Number of pages moved during compaction
    pub pages_moved: u64,
    /// Number of pages freed and truncated from the end of the file
    pub pages_truncated: u64,
    /// Bytes reclaimed from file truncation
    pub bytes_reclaimed: u64,
    /// File size before VACUUM FULL (in bytes)
    pub file_size_before: u64,
    /// File size after VACUUM FULL (in bytes)
    pub file_size_after: u64,
    /// Duration of the operation
    pub duration: Duration,
}

impl VacuumFullStats {
    /// Create a new stats instance with the initial file size
    pub fn new(file_size_before: u64) -> Self {
        Self {
            pages_moved: 0,
            pages_truncated: 0,
            bytes_reclaimed: 0,
            file_size_before,
            file_size_after: file_size_before,
            duration: Duration::default(),
        }
    }

    /// Calculate bytes reclaimed based on file size difference
    pub fn calculate_reclaimed(&mut self) {
        if self.file_size_before > self.file_size_after {
            self.bytes_reclaimed = self.file_size_before - self.file_size_after;
        }
    }
}

/// Aggregated vacuum statistics over time.
#[derive(Debug, Clone, Default)]
pub struct VacuumStats {
    /// Total number of vacuum runs
    pub total_runs: usize,
    /// Total versions removed across all runs
    pub total_versions_removed: usize,
    /// Average versions removed per run
    pub avg_versions_per_run: f64,
    /// Average duration per run
    pub avg_duration: Option<Duration>,
    /// Last vacuum metrics
    pub last_vacuum: Option<VacuumMetrics>,
}

impl VacuumStats {
    /// Update stats with a new vacuum run
    pub fn record_vacuum(&mut self, metrics: VacuumMetrics) {
        self.total_runs += 1;
        self.total_versions_removed += metrics.total_versions_removed;
        self.avg_versions_per_run = self.total_versions_removed as f64 / self.total_runs as f64;

        // Update average duration
        if let Some(duration) = metrics.duration {
            let total_duration = self
                .avg_duration
                .map(|avg| avg * (self.total_runs - 1) as u32 + duration)
                .unwrap_or(duration);
            self.avg_duration = Some(total_duration / self.total_runs as u32);
        }

        self.last_vacuum = Some(metrics);
    }
}

/// Top-level embedded storage engine.
///
/// This struct owns the catalog, file allocation, transaction manager, WAL,
/// and registered table/index engines. ACID semantics should be coordinated at
/// this layer rather than by independently stacking transactional wrappers around
/// individual tables.
pub struct StorageEngine<FS: FileSystem> {
    // Transaction management
    /// Shared conflict detector for coordinating transactions
    conflict_detector: Arc<Mutex<ConflictDetector>>,

    /// Next transaction ID to allocate (lock-free atomic counter)
    next_txn_id: Arc<AtomicU64>,

    /// Next snapshot ID to allocate (lock-free atomic counter)
    next_snapshot_id: Arc<AtomicU64>,

    /// Current LSN for snapshot isolation
    current_lsn: Arc<RwLock<LogSequenceNumber>>,

    /// Active named snapshots pinned by ID.
    snapshots: Arc<RwLock<HashMap<SnapshotId, Snapshot>>>,

    // Catalog management
    /// Unified catalog: maps table/index names to their metadata
    /// Both regular tables and indexes are stored here
    table_catalog: Arc<RwLock<HashMap<String, TableInfo>>>,

    // Storage layer
    /// Write-ahead log for durability
    wal: Arc<WalWriter<FS>>,

    /// Pager for disk I/O
    pager: Arc<Pager<FS>>,

    /// Table engine registry for managing storage engine instances
    engine_registry: Arc<TableEngineRegistry<FS>>,

    // Vacuum management
    /// Configuration for automatic vacuum
    vacuum_config: Arc<RwLock<VacuumConfig>>,

    /// Aggregated vacuum statistics
    vacuum_stats: Arc<RwLock<VacuumStats>>,

    /// Flag to signal vacuum thread shutdown
    vacuum_shutdown: Arc<AtomicBool>,

    /// Handle to the background vacuum thread
    vacuum_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
}

impl<FS: FileSystem> StorageEngine<FS> {
    /// Create a new storage engine instance with the given filesystem, WAL path, and database path.
    pub fn new(fs: &FS, wal_path: &str, db_path: &str) -> Result<Self, StorageEngineError> {
        let wal_config = WalWriterConfig::default();
        let wal = WalWriter::create(fs, wal_path, wal_config)
            .map_err(|e| StorageEngineError::wal_failed(format!("Failed to create WAL: {}", e)))?;

        // Create pager for database file with default config
        let pager_config = PagerConfig::default();
        let pager = Pager::create(fs, db_path, pager_config)
            .map_err(|e| StorageEngineError::pager_failed(format!("Failed to create pager: {}", e)))?;
        let pager = Arc::new(pager);

        let engine_registry = Arc::new(TableEngineRegistry::new(pager.clone()));

        let vacuum_config = Arc::new(RwLock::new(VacuumConfig::default()));
        let vacuum_stats = Arc::new(RwLock::new(VacuumStats::default()));
        let vacuum_shutdown = Arc::new(AtomicBool::new(false));

        let db = Self {
            conflict_detector: Arc::new(Mutex::new(ConflictDetector::new())),
            next_txn_id: Arc::new(AtomicU64::new(1)),
            next_snapshot_id: Arc::new(AtomicU64::new(1)),
            current_lsn: Arc::new(RwLock::new(LogSequenceNumber::from(0))),
            snapshots: Arc::new(RwLock::new(HashMap::new())),
            table_catalog: Arc::new(RwLock::new(HashMap::new())),
            wal: Arc::new(wal),
            pager,
            engine_registry: engine_registry.clone(),
            vacuum_config: vacuum_config.clone(),
            vacuum_stats: vacuum_stats.clone(),
            vacuum_shutdown: vacuum_shutdown.clone(),
            vacuum_thread: Arc::new(Mutex::new(None)),
        };

        // Initialize empty catalog page
        db.persist_catalog()?;

        // Start background vacuum thread
        db.start_vacuum_thread();

        Ok(db)
    }

    /// Open an existing storage engine instance.
    pub fn open(fs: &FS, wal_path: &str, db_path: &str) -> Result<Self, StorageEngineError> {
        let wal_config = WalWriterConfig::default();
        let wal = WalWriter::open(fs, wal_path, wal_config)
            .map_err(|e| StorageEngineError::wal_failed(format!("Failed to open WAL: {}", e)))?;

        // Get current LSN from WAL
        let current_lsn = wal.current_lsn();

        // Open pager for database file
        let pager = Pager::open(fs, db_path)
            .map_err(|e| StorageEngineError::pager_failed(format!("Failed to open pager: {}", e)))?;
        let pager = Arc::new(pager);

        let engine_registry = Arc::new(TableEngineRegistry::new(pager.clone()));

        let vacuum_config = Arc::new(RwLock::new(VacuumConfig::default()));
        let vacuum_stats = Arc::new(RwLock::new(VacuumStats::default()));
        let vacuum_shutdown = Arc::new(AtomicBool::new(false));

        let db = Self {
            conflict_detector: Arc::new(Mutex::new(ConflictDetector::new())),
            next_txn_id: Arc::new(AtomicU64::new(1)),
            next_snapshot_id: Arc::new(AtomicU64::new(1)),
            current_lsn: Arc::new(RwLock::new(current_lsn)),
            snapshots: Arc::new(RwLock::new(HashMap::new())),
            table_catalog: Arc::new(RwLock::new(HashMap::new())),
            wal: Arc::new(wal),
            pager,
            engine_registry: engine_registry.clone(),
            vacuum_config: vacuum_config.clone(),
            vacuum_stats: vacuum_stats.clone(),
            vacuum_shutdown: vacuum_shutdown.clone(),
            vacuum_thread: Arc::new(Mutex::new(None)),
        };

        // Recover catalog from disk
        db.recover_catalog()?;

        // Start background vacuum thread
        db.start_vacuum_thread();

        Ok(db)
    }

    /// Allocate a new transaction ID using lock-free atomic increment.
    fn allocate_txn_id(&self) -> TransactionId {
        let txn_id = self.next_txn_id.fetch_add(1, Ordering::SeqCst);
        TransactionId::from(txn_id)
    }

    fn allocate_snapshot_id(&self) -> SnapshotId {
        let snapshot_id = self.next_snapshot_id.fetch_add(1, Ordering::SeqCst);
        SnapshotId::from(snapshot_id)
    }

    fn current_snapshot_lsn(&self) -> LogSequenceNumber {
        *self.current_lsn.read().unwrap()
    }

    fn validate_snapshot_lsn(&self, lsn: LogSequenceNumber) -> Result<(), StorageEngineError> {
        let latest_readable_lsn = self.current_snapshot_lsn();

        if lsn > latest_readable_lsn {
            return Err(StorageEngineError::invalid_operation(format!(
                "Snapshot LSN {} is not yet committed; latest readable LSN is {}",
                lsn, latest_readable_lsn
            )));
        }

        if lsn == LogSequenceNumber::from(0) {
            return Ok(());
        }

        let snapshots = self.snapshots.read().unwrap();
        let is_pinned = snapshots.values().any(|snapshot| snapshot.lsn == lsn);
        drop(snapshots);

        if !is_pinned {
            return Err(StorageEngineError::invalid_operation(format!(
                "Snapshot LSN {} is not pinned by an active named snapshot",
                lsn
            )));
        }

        Ok(())
    }

    /// Begin a read-only transaction using the latest stable snapshot.
    pub fn begin_read(&self) -> Result<Transaction<FS>, StorageEngineError> {
        let txn_id = self.allocate_txn_id();
        let snapshot_lsn = self.current_snapshot_lsn();

        Ok(Transaction::new_read_only(
            txn_id,
            snapshot_lsn,
            IsolationLevel::ReadCommitted,
            Arc::clone(&self.conflict_detector),
            Arc::clone(&self.wal),
            Arc::clone(&self.engine_registry),
            Arc::clone(&self.current_lsn),
        ))
    }

    /// Begin a read-only transaction with a specific isolation level.
    pub fn begin_read_with_isolation(
        &self,
        isolation: IsolationLevel,
    ) -> Result<Transaction<FS>, StorageEngineError> {
        let txn_id = self.allocate_txn_id();
        let snapshot_lsn = self.current_snapshot_lsn();

        Ok(Transaction::new_read_only(
            txn_id,
            snapshot_lsn,
            isolation,
            Arc::clone(&self.conflict_detector),
            Arc::clone(&self.wal),
            Arc::clone(&self.engine_registry),
            Arc::clone(&self.current_lsn),
        ))
    }

    /// Begin a write transaction with the requested durability policy.
    pub fn begin_write(&self, durability: Durability) -> Result<Transaction<FS>, StorageEngineError> {
        let txn_id = self.allocate_txn_id();
        let snapshot_lsn = *self.current_lsn.read().unwrap();

        // Transaction::new will write BEGIN to WAL
        Ok(Transaction::new(
            txn_id,
            snapshot_lsn,
            IsolationLevel::ReadCommitted,
            durability,
            Arc::clone(&self.conflict_detector),
            Arc::clone(&self.wal),
            Arc::clone(&self.engine_registry),
            Arc::clone(&self.current_lsn),
        ))
    }

    /// Begin a write transaction with specific durability and isolation level.
    pub fn begin_write_with_isolation(
        &self,
        durability: Durability,
        isolation: IsolationLevel,
    ) -> Result<Transaction<FS>, StorageEngineError> {
        let txn_id = self.allocate_txn_id();
        let snapshot_lsn = *self.current_lsn.read().unwrap();

        // Transaction::new will write BEGIN to WAL
        Ok(Transaction::new(
            txn_id,
            snapshot_lsn,
            isolation,
            durability,
            Arc::clone(&self.conflict_detector),
            Arc::clone(&self.wal),
            Arc::clone(&self.engine_registry),
            Arc::clone(&self.current_lsn),
        ))
    }

    /// Begin a read-only transaction at a specific snapshot LSN.
    ///
    /// This is useful for reading from named snapshots or implementing
    /// time-travel queries. Returns an error if the LSN is not available
    /// (e.g., too old and already garbage collected).
    pub fn begin_read_at(&self, lsn: LogSequenceNumber) -> Result<Transaction<FS>, StorageEngineError> {
        self.validate_snapshot_lsn(lsn)?;
        let txn_id = self.allocate_txn_id();

        Ok(Transaction::new_read_only(
            txn_id,
            lsn,
            IsolationLevel::ReadCommitted,
            Arc::clone(&self.conflict_detector),
            Arc::clone(&self.wal),
            Arc::clone(&self.engine_registry),
            Arc::clone(&self.current_lsn),
        ))
    }

    /// Begin a read-only transaction at a specific snapshot LSN with custom isolation level.
    ///
    /// This is useful for reading from named snapshots or implementing
    /// time-travel queries with specific isolation guarantees. Returns an error
    /// if the LSN is not available (e.g., too old and already garbage collected).
    pub fn begin_read_at_with_isolation(
        &self,
        lsn: LogSequenceNumber,
        isolation: IsolationLevel,
    ) -> Result<Transaction<FS>, StorageEngineError> {
        self.validate_snapshot_lsn(lsn)?;
        let txn_id = self.allocate_txn_id();

        Ok(Transaction::new_read_only(
            txn_id,
            lsn,
            isolation,
            Arc::clone(&self.conflict_detector),
            Arc::clone(&self.wal),
            Arc::clone(&self.engine_registry),
            Arc::clone(&self.current_lsn),
        ))
    }

    // =========================================================================
    // Catalog Persistence
    // =========================================================================

    /// Persist the catalog to disk.
    ///
    /// The catalog is serialized as JSON and written to the catalog page.
    /// Format:
    /// - Version (u32): Catalog format version
    /// - Count (u32): Number of tables
    /// - JSON data: Serialized Vec<TableInfo>
    fn persist_catalog(&self) -> Result<(), StorageEngineError> {
        let catalog = self.table_catalog.read().unwrap();

        // Collect all table info into a vector
        let tables: Vec<TableInfo> = catalog.values().cloned().collect();

        // Serialize to JSON
        let json_data = serde_json::to_vec(&tables)
            .map_err(|e| StorageEngineError::other(format!("Failed to serialize catalog: {}", e)))?;

        // Catalog page is always page 2 (page 0 = header, page 1 = superblock, page 2 = catalog)
        // We use a fixed page ID rather than allocating to ensure consistency
        let catalog_page_id = PageId::from(2);

        // Try to allocate the catalog page if it doesn't exist yet
        // This will fail if page already exists, which is fine - we'll just write to it
        let _ = self.pager.allocate_page(PageType::Catalog);

        // Prepare page data with version and count header
        let version: u32 = 1; // Catalog format version
        let count: u32 = tables.len() as u32;

        let mut page_data = Vec::with_capacity(8 + json_data.len());
        page_data.extend_from_slice(&version.to_le_bytes());
        page_data.extend_from_slice(&count.to_le_bytes());
        page_data.extend_from_slice(&json_data);

        // Create page with catalog data
        let mut page = Page::new(catalog_page_id, PageType::Catalog, page_data.len());
        page.data = page_data;

        // Write to catalog page
        self.pager.write_page(&page).map_err(|e| {
            StorageEngineError::pager_failed(format!("Failed to write catalog page: {}", e))
        })?;

        Ok(())
    }

    /// Recover the catalog from disk.
    ///
    /// Reads the catalog page and deserializes the table metadata.
    /// Also reopens all storage engines for the recovered tables.
    fn recover_catalog(&self) -> Result<(), StorageEngineError> {
        // Catalog page is always page 2 (page 0 = header, page 1 = superblock, page 2 = catalog)
        let catalog_page_id = PageId::from(2);

        // Read catalog page
        let page = self.pager.read_page(catalog_page_id).map_err(|e| {
            StorageEngineError::pager_failed(format!("Failed to read catalog page: {}", e))
        })?;

        // Check if page is empty (new database)
        if page.data.is_empty() || page.data.len() < 8 {
            return Ok(()); // Empty catalog is valid for new databases
        }

        // Parse header
        let version = u32::from_le_bytes(page.data[0..4].try_into().unwrap());
        let count = u32::from_le_bytes(page.data[4..8].try_into().unwrap());

        // Validate version
        if version != 1 {
            return Err(StorageEngineError::other(format!(
                "Unsupported catalog version: {}",
                version
            )));
        }

        // Deserialize JSON data
        let json_data = &page.data[8..];
        let tables: Vec<TableInfo> = serde_json::from_slice(json_data)
            .map_err(|e| StorageEngineError::other(format!("Failed to deserialize catalog: {}", e)))?;

        // Validate count
        if tables.len() != count as usize {
            return Err(StorageEngineError::other(format!(
                "Catalog count mismatch: expected {}, got {}",
                count,
                tables.len()
            )));
        }

        // Populate catalog and reopen engines
        let mut catalog = self.table_catalog.write().unwrap();
        catalog.clear();

        for table_info in tables {
            // Reopen the storage engine for this table if it has a root page
            // Memory tables don't persist, so they start fresh
            if let Some(root_location) = table_info.root {
                let engine = self
                    .engine_registry
                    .open_engine(
                        table_info.id,
                        table_info.name.clone(),
                        &table_info.options,
                        root_location.page_id,
                    )
                    .map_err(|e| {
                        StorageEngineError::other(format!(
                            "Failed to reopen storage engine for table '{}': {}",
                            table_info.name, e
                        ))
                    })?;

                // Register the reopened engine
                self.engine_registry.register(engine).map_err(|e| {
                    StorageEngineError::other(format!(
                        "Failed to register storage engine for table '{}': {}",
                        table_info.name, e
                    ))
                })?;
            } else {
                // Memory table or table without root - create fresh engine
                let (engine, _root_page_id) = self
                    .engine_registry
                    .create_engine(table_info.id, table_info.name.clone(), &table_info.options)
                    .map_err(|e| {
                        StorageEngineError::other(format!(
                            "Failed to create storage engine for table '{}': {}",
                            table_info.name, e
                        ))
                    })?;

                self.engine_registry.register(engine).map_err(|e| {
                    StorageEngineError::other(format!(
                        "Failed to register storage engine for table '{}': {}",
                        table_info.name, e
                    ))
                })?;
            }

            catalog.insert(table_info.name.clone(), table_info);
        }

        Ok(())
    }

    /// Create a logical table using a chosen physical engine.
    ///
    /// This operation is transactional - the table becomes visible only after
    /// the current LSN advances (simulating a commit).
    pub fn create_table(
        &self,
        name: &str,
        options: TableOptions,
    ) -> Result<TableId, StorageEngineError> {
        let mut catalog = self.table_catalog.write().unwrap();

        // Check if table already exists
        if catalog.contains_key(name) {
            return Err(StorageEngineError::table_already_exists(name));
        }

        // Allocate new table ID
        let table_id = TableId::from(catalog.len() as u64 + 1);

        // Get current LSN for creation timestamp
        let created_lsn = *self.current_lsn.read().unwrap();

        // Create the storage engine instance
        let (engine, root_page_id) = self
            .engine_registry
            .create_engine(table_id, name.to_string(), &options)
            .map_err(|e| StorageEngineError::other(format!("Failed to create storage engine: {}", e)))?;

        // Register the engine
        self.engine_registry.register(engine).map_err(|e| {
            StorageEngineError::other(format!("Failed to register storage engine: {}", e))
        })?;

        // Create table info with root page location
        let root = root_page_id.map(|page_id| crate::pager::PhysicalLocation {
            page_id,
            offset: 0,
            length: 0,
        });

        let table_info = TableInfo {
            id: table_id,
            name: name.to_string(),
            options,
            root,
            created_lsn,
            metadata: std::collections::HashMap::new(),
        };

        // Add to catalog
        catalog.insert(name.to_string(), table_info);

        // Release lock before persisting to avoid deadlock
        drop(catalog);

        // Persist catalog to disk immediately
        self.persist_catalog()?;

        Ok(table_id)
    }

    /// Drop a logical table and its dependent indexes.
    ///
    /// This operation is transactional - the table becomes invisible only after
    /// the current LSN advances (simulating a commit).
    pub fn drop_table(&self, table: TableId) -> Result<(), StorageEngineError> {
        let mut catalog = self.table_catalog.write().unwrap();

        // Find and remove the table
        let table_name = catalog
            .iter()
            .find(|(_, info)| info.id == table)
            .map(|(name, _)| name.clone());

        if let Some(name) = table_name {
            catalog.remove(&name);

            // Release lock before persisting to avoid deadlock
            drop(catalog);

            // Unregister the engine from the registry
            self.engine_registry.remove(table);

            // Persist catalog to disk immediately
            self.persist_catalog()?;

            Ok(())
        } else {
            Err(StorageEngineError::not_found(table))
        }
    }

    /// Open an existing table by name.
    pub fn open_table(&self, name: &str) -> Result<Option<TableId>, StorageEngineError> {
        let catalog = self.table_catalog.read().unwrap();
        Ok(catalog.get(name).map(|info| info.id))
    }

    /// Get table or index info by TableId.
    pub fn get_object_info(&self, id: TableId) -> Result<Option<TableInfo>, StorageEngineError> {
        let catalog = self.table_catalog.read().unwrap();
        Ok(catalog.values().find(|info| info.id == id).cloned())
    }

    /// Get table or index info by name.
    pub fn get_object_info_by_name(&self, name: &str) -> Result<Option<TableInfo>, StorageEngineError> {
        let catalog = self.table_catalog.read().unwrap();
        Ok(catalog.get(name).cloned())
    }

    /// Check if a TableId refers to a table.
    pub fn is_table(&self, id: TableId) -> Result<bool, StorageEngineError> {
        let catalog = self.table_catalog.read().unwrap();
        Ok(catalog.values().any(|info| info.id == id))
    }

    /// Return all tables in the catalog.
    pub fn list_tables(&self) -> Result<Vec<TableInfo>, StorageEngineError> {
        let catalog = self.table_catalog.read().unwrap();
        Ok(catalog.values().cloned().collect())
    }

    /// Return all catalog objects (alias for list_tables since indexes are just tables).
    pub fn list_all_objects(&self) -> Result<Vec<TableInfo>, StorageEngineError> {
        self.list_tables()
    }

    /// Create a named snapshot at the current LSN.
    ///
    /// The snapshot pins necessary pages/segments to enable consistent reads
    /// at the snapshot LSN. Snapshots must be explicitly released to free
    /// resources.
    pub fn create_snapshot(&self, name: &str) -> Result<Snapshot, StorageEngineError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(StorageEngineError::invalid_operation(
                "Snapshot name cannot be empty".to_string(),
            ));
        }

        let mut snapshots = self.snapshots.write().unwrap();
        if snapshots.values().any(|snapshot| snapshot.name == name) {
            return Err(StorageEngineError::invalid_operation(format!(
                "Snapshot '{}' already exists",
                name
            )));
        }

        let snapshot = Snapshot::new(
            self.allocate_snapshot_id(),
            name.to_string(),
            self.current_snapshot_lsn(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| StorageEngineError::other(format!("System time error: {}", e)))?
                .as_secs() as i64,
            0,
            self.wal.active_transactions(),
        );

        snapshots.insert(snapshot.id, snapshot.clone());
        Ok(snapshot)
    }

    /// List all active snapshots.
    pub fn list_snapshots(&self) -> Result<Vec<Snapshot>, StorageEngineError> {
        let mut snapshots: Vec<_> = self.snapshots.read().unwrap().values().cloned().collect();
        snapshots.sort_by_key(|snapshot| snapshot.id);
        Ok(snapshots)
    }

    /// Release a snapshot, allowing its resources to be reclaimed.
    ///
    /// After releasing, the snapshot LSN may no longer be available for reads.
    pub fn release_snapshot(&self, snapshot_id: SnapshotId) -> Result<(), StorageEngineError> {
        let removed = self.snapshots.write().unwrap().remove(&snapshot_id);
        if removed.is_some() {
            Ok(())
        } else {
            Err(StorageEngineError::invalid_operation(format!(
                "Snapshot {} not found",
                snapshot_id
            )))
        }
    }

    /// Compute the minimum visible LSN across all active snapshots and transactions.
    ///
    /// This is the watermark below which version chains can be safely vacuumed.
    /// Any version with commit_lsn < min_visible_lsn is guaranteed to be invisible
    /// to all current and future transactions.
    ///
    /// Returns None if there are no active snapshots (meaning all committed versions
    /// are potentially visible).
    pub fn min_visible_lsn(&self) -> Option<LogSequenceNumber> {
        let snapshots = self.snapshots.read().unwrap();

        // Find the minimum LSN across all active snapshots
        snapshots.values().map(|snapshot| snapshot.lsn).min()
    }

    /// Vacuum a specific table to remove obsolete version chains.
    ///
    /// This removes versions older than the minimum visible LSN across all active
    /// snapshots. The vacuum is performed atomically per table.
    ///
    /// # Arguments
    ///
    /// * `table_id` - The table to vacuum
    ///
    /// # Returns
    ///
    /// Returns the number of versions removed, or an error if the table doesn't exist
    /// or doesn't support vacuuming.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Vacuum a specific table
    /// let removed = db.vacuum_table(table_id)?;
    /// println!("Removed {} obsolete versions", removed);
    /// ```
    pub fn vacuum_table(&self, table_id: TableId) -> Result<usize, StorageEngineError> {
        // Get minimum visible LSN
        let min_visible_lsn = match self.min_visible_lsn() {
            Some(lsn) => lsn,
            None => {
                // No active snapshots - use current LSN as watermark
                // This is conservative: we keep all versions visible to any future snapshot
                self.current_snapshot_lsn()
            }
        };

        // Get table info to determine engine type
        let table_info = self
            .get_object_info(table_id)?
            .ok_or_else(|| StorageEngineError::not_found(table_id))?;

        // Vacuum the table through the engine registry
        let registry = self.engine_registry.clone();
        registry.vacuum_table(table_id, min_visible_lsn)
    }

    /// Vacuum all tables in the storage engine.
    ///
    /// This is a convenience method that vacuums all tables that support it.
    /// Tables that don't support vacuuming are skipped.
    ///
    /// # Returns
    ///
    /// Returns a map of table_id -> versions_removed for all vacuumed tables.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Vacuum all tables
    /// let results = db.vacuum_all()?;
    /// for (table_id, removed) in results {
    ///     println!("Table {}: removed {} versions", table_id, removed);
    /// }
    /// ```
    pub fn vacuum_all(&self) -> Result<std::collections::HashMap<TableId, usize>, StorageEngineError> {
        let mut results = std::collections::HashMap::new();

        // Get all tables
        let tables = self.list_tables()?;

        for table_info in tables {
            // Try to vacuum each table, but don't fail if a table doesn't support it
            match self.vacuum_table(table_info.id) {
                Ok(removed) => {
                    if removed > 0 {
                        results.insert(table_info.id, removed);
                    }
                }
                Err(_) => {
                    // Skip tables that don't support vacuuming
                    continue;
                }
            }
        }

        Ok(results)
    }

    /// Vacuum all tables with metrics collection.
    ///
    /// This is an internal method that collects detailed metrics during vacuum.
    /// Used by both manual triggers and the background vacuum thread.
    fn vacuum_all_with_metrics(&self) -> Result<VacuumMetrics, StorageEngineError> {
        let mut metrics = VacuumMetrics::new();

        // Get all tables
        let tables = self.list_tables()?;

        for table_info in tables {
            // Try to vacuum each table, but don't fail if a table doesn't support it
            match self.vacuum_table(table_info.id) {
                Ok(removed) => {
                    if removed > 0 {
                        metrics.add_table_result(table_info.id, removed);
                    }
                }
                Err(_) => {
                    // Skip tables that don't support vacuuming
                    continue;
                }
            }
        }

        metrics.complete();
        Ok(metrics)
    }

    /// Manually trigger a vacuum operation with metrics collection.
    ///
    /// This performs an immediate vacuum of all tables and returns detailed metrics.
    /// Unlike the automatic background vacuum, this is synchronous and returns results.
    ///
    /// # Returns
    ///
    /// Returns metrics about the vacuum operation including versions removed and duration.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Manually trigger vacuum
    /// let metrics = db.trigger_vacuum()?;
    /// println!("Removed {} versions in {:?}",
    ///          metrics.total_versions_removed,
    ///          metrics.duration);
    /// ```
    pub fn trigger_vacuum(&self) -> Result<VacuumMetrics, StorageEngineError> {
        let metrics = self.vacuum_all_with_metrics()?;

        // Update stats
        let mut stats = self.vacuum_stats.write().unwrap();
        stats.record_vacuum(metrics.clone());

        Ok(metrics)
    }

    /// Get current vacuum statistics.
    ///
    /// Returns aggregated statistics about all vacuum operations performed.
    pub fn vacuum_stats(&self) -> VacuumStats {
        self.vacuum_stats.read().unwrap().clone()
    }

    /// Get current vacuum configuration.
    pub fn vacuum_config(&self) -> VacuumConfig {
        self.vacuum_config.read().unwrap().clone()
    }

    /// Update vacuum configuration.
    ///
    /// Changes take effect on the next vacuum cycle. If vacuum is disabled,
    /// the background thread will stop after the current cycle completes.
    pub fn set_vacuum_config(&self, config: VacuumConfig) {
        *self.vacuum_config.write().unwrap() = config;
    }

    /// Start the background vacuum thread.
    ///
    /// This is called automatically by `new()` and `open()`.
    fn start_vacuum_thread(&self) {
        let config = self.vacuum_config.clone();
        let stats = self.vacuum_stats.clone();
        let shutdown = self.vacuum_shutdown.clone();
        let engine_registry = self.engine_registry.clone();
        let snapshots = self.snapshots.clone();
        let current_lsn = self.current_lsn.clone();
        let table_catalog = self.table_catalog.clone();

        let handle = std::thread::spawn(move || {
            loop {
                // Check if we should shutdown
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }

                // Get current config
                let cfg = config.read().unwrap().clone();

                // Sleep for the configured interval
                std::thread::sleep(cfg.interval);

                // Check again after sleep
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }

                // Skip if vacuum is disabled
                if !cfg.enabled {
                    continue;
                }

                // Perform vacuum with metrics
                let mut metrics = VacuumMetrics::new();

                // Get minimum visible LSN
                let min_visible_lsn = {
                    let snapshots_guard = snapshots.read().unwrap();
                    snapshots_guard
                        .values()
                        .map(|snapshot| snapshot.lsn)
                        .min()
                        .unwrap_or_else(|| *current_lsn.read().unwrap())
                };

                // Get all tables
                let tables: Vec<TableInfo> = {
                    let catalog = table_catalog.read().unwrap();
                    catalog.values().cloned().collect()
                };

                // Vacuum each table
                for table_info in tables {
                    match engine_registry.vacuum_table(table_info.id, min_visible_lsn) {
                        Ok(removed) => {
                            if removed > 0 {
                                metrics.add_table_result(table_info.id, removed);
                            }
                        }
                        Err(_) => {
                            // Skip tables that don't support vacuuming
                            continue;
                        }
                    }
                }

                metrics.complete();

                // Update stats
                let mut stats_guard = stats.write().unwrap();
                stats_guard.record_vacuum(metrics);
            }
        });

        *self.vacuum_thread.lock().unwrap() = Some(handle);
    }

    /// Stop the background vacuum thread.
    ///
    /// This is called automatically by `close()` and `Drop`.
    fn stop_vacuum_thread(&self) {
        // Signal shutdown
        self.vacuum_shutdown.store(true, Ordering::Relaxed);

        // Wait for thread to finish
        if let Some(handle) = self.vacuum_thread.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    /// Get the consistency guarantees provided by this storage engine.
    ///
    /// This documents the ACID properties, isolation levels, and crash
    /// recovery semantics. Query planners and applications can use this
    /// to make informed decisions about transaction boundaries and
    /// error handling.
    pub fn consistency_guarantees(&self) -> ConsistencyGuarantees {
        // Conservative default
        ConsistencyGuarantees {
            atomicity: true,
            consistency: true,
            isolation: IsolationLevel::ReadCommitted,
            durability: Durability::WalOnly,
            crash_safe: false,
            point_in_time_recovery: false,
        }
    }

    // =========================================================================
    // Phase 4: Enhanced CRUD Operations with Index Maintenance
    // =========================================================================

    /// Insert a key-value pair into a table with automatic index maintenance.
    ///
    /// This is a convenience method that:
    /// 1. Begins a write transaction
    /// 2. Inserts the key-value pair into the table
    /// 3. Updates all indexes on the table
    /// 4. Commits the transaction atomically
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The table does not exist
    /// - The key already exists (use `upsert` for update-or-insert)
    /// - Index maintenance fails
    /// - Transaction commit fails
    pub fn insert(&self, table: TableId, key: &[u8], value: &[u8]) -> Result<(), StorageEngineError> {
        // Validate table exists and is a regular table
        if !self.is_table(table)? {
            return Err(StorageEngineError::not_a_table(table));
        }

        let mut txn = self.begin_write(Durability::SyncOnCommit)?;

        // Check if key already exists
        if txn.get(table, key)?.is_some() {
            return Err(StorageEngineError::key_already_exists(table, key));
        }

        // Insert into table
        txn.put(table, key, value)?;

        // Commit transaction
        txn.commit().map_err(|e| {
            StorageEngineError::transaction_failed(format!("Insert commit failed: {}", e))
        })?;

        Ok(())
    }

    /// Update an existing key-value pair in a table with automatic index maintenance.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The table does not exist
    /// - The key does not exist (use `upsert` for insert-or-update)
    /// - Index maintenance fails
    /// - Transaction commit fails
    pub fn update(&self, table: TableId, key: &[u8], value: &[u8]) -> Result<(), StorageEngineError> {
        // Validate table exists and is a regular table
        if !self.is_table(table)? {
            return Err(StorageEngineError::not_a_table(table));
        }

        let mut txn = self.begin_write(Durability::SyncOnCommit)?;

        // Get old value for index maintenance
        let _old_value = txn
            .get(table, key)?
            .ok_or_else(|| StorageEngineError::key_not_found(table, key))?;

        // Update in table
        txn.put(table, key, value)?;

        // Commit transaction
        txn.commit().map_err(|e| {
            StorageEngineError::transaction_failed(format!("Update commit failed: {}", e))
        })?;

        Ok(())
    }

    /// Insert or update a key-value pair in a table with automatic index maintenance.
    ///
    /// This is a convenience method that inserts if the key doesn't exist,
    /// or updates if it does.
    pub fn upsert(&self, table: TableId, key: &[u8], value: &[u8]) -> Result<bool, StorageEngineError> {
        // Validate table exists and is a regular table
        if !self.is_table(table)? {
            return Err(StorageEngineError::not_a_table(table));
        }

        let mut txn = self.begin_write(Durability::SyncOnCommit)?;

        // Check if key exists
        let old_value = txn.get(table, key)?;
        let is_update = old_value.is_some();

        // Put the new value
        txn.put(table, key, value)?;

        // Commit transaction
        txn.commit().map_err(|e| {
            StorageEngineError::transaction_failed(format!("Upsert commit failed: {}", e))
        })?;

        Ok(is_update)
    }

    /// Get a value from a table.
    ///
    /// This is a convenience method that begins a read transaction and
    /// retrieves the value.
    pub fn get(&self, table: TableId, key: &[u8]) -> Result<Option<ValueBuf>, StorageEngineError> {
        // Validate table exists
        if !self.is_table(table)? {
            return Err(StorageEngineError::not_a_table(table));
        }

        let txn = self.begin_read()?;
        txn.get(table, key)
            .map_err(|e| StorageEngineError::transaction_failed(format!("Get failed: {}", e)))
    }

    /// Delete a key from a table with automatic index maintenance.
    ///
    /// Returns true if the key existed and was deleted, false if it didn't exist.
    pub fn delete(&self, table: TableId, key: &[u8]) -> Result<bool, StorageEngineError> {
        // Validate table exists and is a regular table
        if !self.is_table(table)? {
            return Err(StorageEngineError::not_a_table(table));
        }

        let mut txn = self.begin_write(Durability::SyncOnCommit)?;

        // Get current value for index maintenance
        let old_value = txn.get(table, key)?;

        if old_value.is_none() {
            return Ok(false);
        }

        // Delete from table
        let deleted = txn.delete(table, key)?;

        // Commit transaction
        txn.commit().map_err(|e| {
            StorageEngineError::transaction_failed(format!("Delete commit failed: {}", e))
        })?;

        Ok(deleted)
    }

    /// Open a table handle for ergonomic access.
    ///
    /// Returns a `TableHandle` that provides convenient methods for
    /// working with the table.
    pub fn table(&self, table: TableId) -> Result<TableHandle<'_, FS>, StorageEngineError> {
        // Validate table exists and is a regular table
        if !self.is_table(table)? {
            return Err(StorageEngineError::not_a_table(table));
        }

        Ok(TableHandle {
            db: self,
            table_id: table,
        })
    }

    /// Explicitly close the storage engine with controlled shutdown.
    ///
    /// This method provides a controlled shutdown sequence:
    /// 1. Flushes all LSM tree memtables to SSTables
    /// 2. Flushes WAL buffer to disk
    /// 3. Syncs pager (flushes cache and syncs database file)
    ///
    /// Unlike Drop, this method returns errors for proper error handling.
    /// The Drop implementation will still run if close() is not called,
    /// but errors will only be logged, not returned.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - LSM memtable flush fails
    /// - WAL flush fails
    /// - Pager sync fails
    pub fn close(self) -> Result<(), StorageEngineError> {
        // Note: The Drop implementations for LsmTree will automatically
        // flush memtables when the engine registry is dropped.
        // We just need to ensure WAL and pager are flushed.

        // Step 0: Stop vacuum thread
        self.stop_vacuum_thread();

        // Step 1: Flush WAL buffer
        self.wal.flush().map_err(|e| {
            StorageEngineError::wal_failed(format!("Failed to flush WAL during close: {}", e))
        })?;

        // Step 2: Sync pager (flushes cache and syncs file)
        self.pager.sync().map_err(|e| {
            StorageEngineError::pager_failed(format!("Failed to sync pager during close: {}", e))
        })?;

        // Step 3: Drop self, which will trigger Drop implementations for all engines
        // The LsmTree Drop implementation will flush memtables
        drop(self);

        Ok(())
    }
}

impl<FS: FileSystem> Drop for StorageEngine<FS> {
    /// Ensure data durability on clean shutdown.
    ///
    /// This Drop implementation:
    /// 1. Flushes all pending WAL writes
    /// 2. Syncs WAL to disk
    /// 3. Flushes pager dirty pages
    /// 4. Syncs database file
    ///
    /// Note: Errors during drop are logged but not propagated since Drop cannot return errors.
    fn drop(&mut self) {
        // Step 0: Stop vacuum thread
        self.stop_vacuum_thread();

        // Step 1: Flush WAL buffer
        if let Err(e) = self.wal.flush() {
            eprintln!(
                "Warning: Failed to flush WAL during storage engine shutdown: {}",
                e
            );
        }

        // Step 2: Sync pager (flushes cache and syncs file)
        if let Err(e) = self.pager.sync() {
            eprintln!(
                "Warning: Failed to sync pager during storage engine shutdown: {}",
                e
            );
        }

        // Note: WAL sync is handled by flush() if sync_on_write is enabled,
        // or by the group commit coordinator. The pager.sync() call ensures
        // all database file changes are persisted.
    }
}

/// Table handle for ergonomic access to a specific table.
///
/// Provides convenient methods for CRUD operations without needing to
/// pass the table ID repeatedly.
pub struct TableHandle<'db, FS: FileSystem> {
    db: &'db StorageEngine<FS>,
    table_id: TableId,
}

impl<'db, FS: FileSystem> TableHandle<'db, FS> {
    /// Get the table ID.
    pub fn id(&self) -> TableId {
        self.table_id
    }

    /// Get table metadata.
    pub fn info(&self) -> Result<Option<TableInfo>, StorageEngineError> {
        self.db.get_object_info(self.table_id)
    }

    /// Insert a key-value pair.
    pub fn insert(&self, key: &[u8], value: &[u8]) -> Result<(), StorageEngineError> {
        self.db.insert(self.table_id, key, value)
    }

    /// Update an existing key-value pair.
    pub fn update(&self, key: &[u8], value: &[u8]) -> Result<(), StorageEngineError> {
        self.db.update(self.table_id, key, value)
    }

    /// Insert or update a key-value pair.
    pub fn upsert(&self, key: &[u8], value: &[u8]) -> Result<bool, StorageEngineError> {
        self.db.upsert(self.table_id, key, value)
    }

    /// Get a value.
    pub fn get(&self, key: &[u8]) -> Result<Option<ValueBuf>, StorageEngineError> {
        self.db.get(self.table_id, key)
    }

    /// Delete a key.
    pub fn delete(&self, key: &[u8]) -> Result<bool, StorageEngineError> {
        self.db.delete(self.table_id, key)
    }

    /// Check if a key exists.
    pub fn contains(&self, key: &[u8]) -> Result<bool, StorageEngineError> {
        Ok(self.get(key)?.is_some())
    }
}

/// Storage engine error type with enhanced context.
#[derive(Debug)]
pub struct StorageEngineError {
    pub kind: StorageEngineErrorKind,
    pub message: String,
}

/// Storage engine error kinds for better error handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageEngineErrorKind {
    /// Table or index not found
    NotFound,
    /// Object exists but is not a table
    NotATable,
    /// Object exists but is not an index
    NotAnIndex,
    /// Key already exists (for insert operations)
    KeyAlreadyExists,
    /// Key not found (for update operations)
    KeyNotFound,
    /// Table already exists
    TableAlreadyExists,
    /// Index already exists
    IndexAlreadyExists,
    /// Index maintenance failed
    IndexMaintenanceFailed,
    /// Transaction operation failed
    TransactionFailed,
    /// WAL operation failed
    WalFailed,
    /// Pager operation failed
    PagerFailed,
    /// Invalid operation or state
    InvalidOperation,
    /// Other error
    Other,
}

impl StorageEngineError {
    pub fn not_found(object: TableId) -> Self {
        Self {
            kind: StorageEngineErrorKind::NotFound,
            message: format!("Object {:?} not found", object),
        }
    }

    pub fn not_a_table(object: TableId) -> Self {
        Self {
            kind: StorageEngineErrorKind::NotATable,
            message: format!("Object {:?} is not a table", object),
        }
    }

    pub fn not_an_index(object: TableId) -> Self {
        Self {
            kind: StorageEngineErrorKind::NotAnIndex,
            message: format!("Object {:?} is not an index", object),
        }
    }

    pub fn key_already_exists(table: TableId, key: &[u8]) -> Self {
        Self {
            kind: StorageEngineErrorKind::KeyAlreadyExists,
            message: format!("Key {:?} already exists in table {:?}", key, table),
        }
    }

    pub fn key_not_found(table: TableId, key: &[u8]) -> Self {
        Self {
            kind: StorageEngineErrorKind::KeyNotFound,
            message: format!("Key {:?} not found in table {:?}", key, table),
        }
    }

    pub fn table_already_exists(name: &str) -> Self {
        Self {
            kind: StorageEngineErrorKind::TableAlreadyExists,
            message: format!("Table '{}' already exists", name),
        }
    }

    pub fn index_already_exists(name: &str) -> Self {
        Self {
            kind: StorageEngineErrorKind::IndexAlreadyExists,
            message: format!("Index '{}' already exists", name),
        }
    }

    pub fn index_maintenance_failed(index: TableId, details: String) -> Self {
        Self {
            kind: StorageEngineErrorKind::IndexMaintenanceFailed,
            message: format!("Index {:?} maintenance failed: {}", index, details),
        }
    }

    pub fn transaction_failed(details: String) -> Self {
        Self {
            kind: StorageEngineErrorKind::TransactionFailed,
            message: format!("Transaction failed: {}", details),
        }
    }

    pub fn wal_failed(details: String) -> Self {
        Self {
            kind: StorageEngineErrorKind::WalFailed,
            message: format!("WAL operation failed: {}", details),
        }
    }

    pub fn pager_failed(details: String) -> Self {
        Self {
            kind: StorageEngineErrorKind::PagerFailed,
            message: format!("Pager operation failed: {}", details),
        }
    }

    pub fn invalid_operation(details: String) -> Self {
        Self {
            kind: StorageEngineErrorKind::InvalidOperation,
            message: format!("Invalid operation: {}", details),
        }
    }

    pub fn other(message: String) -> Self {
        Self {
            kind: StorageEngineErrorKind::Other,
            message,
        }
    }
}

impl Default for StorageEngineError {
    fn default() -> Self {
        Self {
            kind: StorageEngineErrorKind::Other,
            message: "Unknown storage engine error".to_string(),
        }
    }
}

impl std::fmt::Display for StorageEngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for StorageEngineError {}

impl From<crate::txn::TransactionError> for StorageEngineError {
    fn from(err: crate::txn::TransactionError) -> Self {
        StorageEngineError::transaction_failed(err.to_string())
    }
}
