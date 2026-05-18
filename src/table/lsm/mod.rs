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

//! LSM tree storage engine implementation.
//!
//! This module provides a Log-Structured Merge (LSM) tree storage engine
//! optimized for write-heavy workloads. The LSM tree consists of:
//!
//! - **Memtable**: In-memory write buffer for recent writes
//! - **SSTables**: Immutable sorted string tables on disk
//! - **Bloom filters**: Probabilistic filters to reduce disk I/O
//! - **Compaction**: Background process to merge and optimize SSTables
//!
//! # Architecture
//!
//! ```text
//! Writes → Memtable → Immutable Memtable → L0 SSTable
//!                                              ↓
//!                                         Compaction
//!                                              ↓
//!                                    L1, L2, ..., Ln SSTables
//! ```
//!
//! # Features
//!
//! - Write-optimized: Sequential writes to memtable, batch flushes to disk
//! - MVCC support: Version chains for snapshot isolation
//! - Bloom filters: Reduce unnecessary disk reads
//! - Leveled compaction: Exponential level sizes, non-overlapping SSTables
//! - Compression: Optional per-block compression
//! - Encryption: Optional per-block encryption

mod compaction;
mod config;
mod iterator;
mod manifest;
mod memtable;
mod sstable;

pub use self::compaction::{
    CompactionExecutor, CompactionJob, CompactionManager, CompactionPicker, CompactionStats,
};
pub use self::config::{
    BlockCacheConfig, BloomFilterConfig, CacheEvictionPolicy, CompactionConfig, CompactionStrategy,
    LevelConfig, LsmConfig, MemtableConfig, MemtableType, SStableConfig,
};
pub use self::iterator::{
    Direction, LsmEntry, LsmIterator, MemtableIterator, MergeIterator, SStableIterator,
};
pub use self::manifest::{FileMetadata, Manifest, Version, VersionEdit};
pub use self::memtable::Memtable;
pub use self::sstable::{
    DataBlock, SStableFooter, SStableId, SStableMetadata, SStableReader, SStableWriter,
};
use crate::table::SearchableTable;
pub use crate::table::bloom::{BloomFilter, BloomFilterBuilder};

// Made with Bob

// =============================================================================
// LSM Tree Table Implementation
// =============================================================================

use crate::pager::{PageId, Pager};
use crate::table::error::{TableError, TableResult};
use crate::table::{
    BatchOps, BatchReport, Flushable, MutableTable, OrderedScan, PointLookup, Table,
    TableCapabilities, TableCursor, TableEngineKind, TableReader, TableStatistics, TableWriter,
    ValueStream, WriteBatch,
};
use crate::txn::TransactionId;
use crate::types::{Bound, ScanBounds, TableId, ValueBuf};
use crate::vfs::FileSystem;
use crate::wal::LogSequenceNumber;
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tracing::{debug, instrument};

/// LSM Tree storage engine.
///
/// Provides a write-optimized storage engine using log-structured merge trees.
/// Writes go to an in-memory memtable, which is periodically flushed to disk
/// as immutable SSTables. Background compaction merges SSTables to maintain
/// read performance.
pub struct LsmTree<FS: FileSystem> {
    /// Table identifier
    table_id: TableId,

    /// Table name
    name: String,

    /// Pager for disk I/O
    pager: Arc<Pager<FS>>,

    /// Configuration
    config: LsmConfig,

    /// Active memtable (accepts writes)
    active_memtable: Arc<RwLock<Memtable>>,

    /// Immutable memtables (being flushed)
    immutable_memtables: Arc<RwLock<Vec<Memtable>>>,

    /// Manifest for version management
    manifest: Arc<Manifest<FS>>,

    /// Compaction manager
    compaction_manager: Arc<CompactionManager<FS>>,
}

impl<FS: FileSystem> LsmTree<FS> {
    /// Create a new LSM tree.
    pub fn new(
        table_id: TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        root_page_id: PageId,
        config: LsmConfig,
    ) -> TableResult<Self> {
        let num_levels = config.compaction.levels.len();
        let manifest = Arc::new(Manifest::new(pager.clone(), root_page_id, num_levels)?);

        let active_memtable = Arc::new(RwLock::new(Memtable::new(config.memtable.max_size)));

        let immutable_memtables = Arc::new(RwLock::new(Vec::new()));

        let compaction_manager = Arc::new(CompactionManager::new(
            pager.clone(),
            manifest.clone(),
            config.compaction.clone(),
        ));

        Ok(Self {
            table_id,
            name,
            pager,
            config,
            active_memtable,
            immutable_memtables,
            manifest,
            compaction_manager,
        })
    }

    /// Open an existing LSM tree.
    pub fn open(
        table_id: TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        root_page_id: PageId,
        config: LsmConfig,
    ) -> TableResult<Self> {
        let num_levels = config.compaction.levels.len();
        let manifest = Arc::new(Manifest::open(pager.clone(), root_page_id, num_levels)?);

        let active_memtable = Arc::new(RwLock::new(Memtable::new(config.memtable.max_size)));

        let immutable_memtables = Arc::new(RwLock::new(Vec::new()));

        let compaction_manager = Arc::new(CompactionManager::new(
            pager.clone(),
            manifest.clone(),
            config.compaction.clone(),
        ));

        Ok(Self {
            table_id,
            name,
            pager,
            config,
            active_memtable,
            immutable_memtables,
            manifest,
            compaction_manager,
        })
    }

    /// Get a value from the LSM tree at a specific snapshot.
    #[instrument(skip(self, key), fields(key_len = key.len()))]
    fn get_internal(
        &self,
        key: &[u8],
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Option<Vec<u8>>> {
        let start = Instant::now();
        debug!("LSM get operation");

        // 1. Check active memtable
        {
            let memtable = self.active_memtable.read().unwrap();
            if let Some(value) = memtable.get(key, snapshot_lsn)? {
                return Ok(Some(value));
            }
        }

        // 2. Check immutable memtables (newest to oldest)
        {
            let immutable = self.immutable_memtables.read().unwrap();
            for memtable in immutable.iter().rev() {
                if let Some(value) = memtable.get(key, snapshot_lsn)? {
                    return Ok(Some(value));
                }
            }
        }

        // 3. Check SSTables (L0 to Ln)
        let version = self.manifest.current();

        // Check each level
        for level in 0..version.num_levels() {
            let files = version.level_files(level as u32);

            if level == 0 {
                // L0 files may overlap, check all in reverse order (newest first)
                for file in files.iter().rev() {
                    if !file.contains_key(key) {
                        continue;
                    }

                    let reader = SStableReader::open(
                        self.pager.clone(),
                        file.first_page_id,
                        self.config.sstable.clone(),
                    )?;

                    // Check bloom filter first
                    if !reader.may_contain(key) {
                        continue;
                    }

                    if let Some(value) = reader.get(key, snapshot_lsn)? {
                        return Ok(Some(value));
                    }
                }
            } else {
                // L1+ files don't overlap, binary search
                let file_idx = files.binary_search_by(|f| {
                    if key < f.min_key.as_slice() {
                        std::cmp::Ordering::Greater
                    } else if key > f.max_key.as_slice() {
                        std::cmp::Ordering::Less
                    } else {
                        std::cmp::Ordering::Equal
                    }
                });

                if let Ok(idx) = file_idx {
                    let file = &files[idx];
                    let reader = SStableReader::open(
                        self.pager.clone(),
                        file.first_page_id,
                        self.config.sstable.clone(),
                    )?;

                    // Check bloom filter first
                    if !reader.may_contain(key) {
                        continue;
                    }

                    if let Some(value) = reader.get(key, snapshot_lsn)? {
                        return Ok(Some(value));
                    }
                }
            }
        }

        crate::table::metrics::record_get_duration("lsm", start);
        crate::table::metrics::lsm::record_sstable_read();
        Ok(None)
    }

    /// Get a streaming reader for a value at a specific snapshot.
    ///
    /// For now, this uses the same implementation as get_internal and wraps
    /// the result in a SliceValueStream. Future optimization could use ValueRef
    /// to stream directly from overflow pages without loading into memory.
    #[instrument(skip(self, key), fields(key_len = key.len()))]
    fn get_stream_internal(
        &self,
        key: &[u8],
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Option<Box<dyn ValueStream + '_>>> {
        // For now, use get_internal and wrap the result
        // This avoids lifetime issues with the SSTable reader
        match self.get_internal(key, snapshot_lsn)? {
            Some(value) => Ok(Some(Box::new(crate::table::SliceValueStream::new(value)))),
            None => Ok(None),
        }
    }

    /// Insert a key-value pair into the active memtable.
    #[instrument(skip(self, key, value), fields(key_len = key.len(), value_len = value.len()))]
    fn insert_internal(
        &self,
        key: Vec<u8>,
        value: Vec<u8>,
        tx_id: TransactionId,
        commit_lsn: Option<LogSequenceNumber>,
    ) -> TableResult<()> {
        let start = Instant::now();
        debug!("LSM insert operation");

        let memtable = self.active_memtable.write().unwrap();

        // Try to insert
        let result = match memtable.insert(key.clone(), value.clone(), tx_id, commit_lsn) {
            Ok(()) => Ok(()),
            Err(TableError::MemtableFull) => {
                // Memtable is full, need to rotate
                drop(memtable);
                self.rotate_memtable()?;

                // Retry insert on new memtable
                let new_memtable = self.active_memtable.write().unwrap();
                new_memtable.insert(key, value, tx_id, commit_lsn)
            }
            Err(e) => Err(e),
        };

        if result.is_ok() {
            crate::table::metrics::lsm::record_memtable_write();
            crate::table::metrics::record_put_duration("lsm", start);
        }

        result
    }

    /// Delete a key from the active memtable (inserts tombstone).
    fn delete_internal(
        &self,
        key: Vec<u8>,
        tx_id: TransactionId,
        commit_lsn: Option<LogSequenceNumber>,
    ) -> TableResult<()> {
        let memtable = self.active_memtable.write().unwrap();

        // Try to delete (insert tombstone)
        match memtable.delete(key.clone(), tx_id, commit_lsn) {
            Ok(()) => Ok(()),
            Err(TableError::MemtableFull) => {
                // Memtable is full, need to rotate
                drop(memtable);
                self.rotate_memtable()?;

                // Retry delete on new memtable
                let new_memtable = self.active_memtable.write().unwrap();
                new_memtable.delete(key, tx_id, commit_lsn)
            }
            Err(e) => Err(e),
        }
    }

    /// Rotate the active memtable to immutable state.
    #[instrument(skip(self))]
    fn rotate_memtable(&self) -> TableResult<()> {
        let start = Instant::now();
        debug!("Rotating memtable");

        let mut active = self.active_memtable.write().unwrap();
        let mut immutable = self.immutable_memtables.write().unwrap();

        // Make current memtable immutable
        active.make_immutable();

        // Move to immutable list
        let old_memtable =
            std::mem::replace(&mut *active, Memtable::new(self.config.memtable.max_size));
        immutable.push(old_memtable);

        crate::table::metrics::lsm::record_memtable_flush();
        crate::table::metrics::lsm::record_flush_duration(start);
        // Note: memtable size tracking would require adding a size() method to Memtable

        // Background flush: The compaction manager's background thread will automatically
        // detect the immutable memtable and flush it to L0 SSTables. The compaction loop
        // continuously monitors for work and will pick up this memtable on its next iteration.
        // No explicit trigger is needed as the background thread is always running.

        Ok(())
    }

    /// Flush the active memtable to an SSTable.
    ///
    /// This method:
    /// 1. Makes the active memtable immutable
    /// 2. Creates a new empty active memtable
    /// 3. Flushes the immutable memtable to an SSTable
    ///
    /// This is called automatically on Drop, but can also be called explicitly
    /// for controlled shutdown or to free memory.
    #[instrument(skip(self))]
    pub fn flush_memtable(&self) -> TableResult<()> {
        let start = Instant::now();
        debug!("Flushing active memtable to SSTable");

        // Check if active memtable has any data
        let has_data = {
            let memtable = self.active_memtable.read().unwrap();
            !memtable.is_empty()
        };

        if !has_data {
            debug!("Active memtable is empty, skipping flush");
            return Ok(());
        }

        // Rotate memtable to make it immutable
        self.rotate_memtable()?;

        // Now flush all immutable memtables
        self.flush_immutable_memtables()?;

        crate::table::metrics::lsm::record_flush_duration(start);
        Ok(())
    }

    /// Flush all immutable memtables to SSTables.
    ///
    /// This is typically called by the background compaction thread,
    /// but can also be called explicitly during shutdown.
    fn flush_immutable_memtables(&self) -> TableResult<()> {
        let immutable = self.immutable_memtables.read().unwrap();

        if immutable.is_empty() {
            return Ok(());
        }

        // We need to flush each immutable memtable
        // For now, we'll flush them one at a time
        // In a production system, this could be parallelized
        let memtables_to_flush: Vec<_> = immutable.iter().cloned().collect();
        drop(immutable);

        for memtable in memtables_to_flush {
            self.flush_single_memtable(&memtable)?;
        }

        // Clear the immutable memtables list
        let mut immutable = self.immutable_memtables.write().unwrap();
        immutable.clear();

        Ok(())
    }

    /// Flush a single memtable to an SSTable.
    fn flush_single_memtable(&self, memtable: &Memtable) -> TableResult<()> {
        if !memtable.is_immutable() {
            return Err(TableError::MemtableNotImmutable);
        }

        // Get all entries from the memtable
        let entries = memtable.entries()?;

        if entries.is_empty() {
            return Ok(());
        }

        // Allocate a new SSTable ID
        let sstable_id = SStableId::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
        );

        // Create SSTable writer
        let mut writer = SStableWriter::new(
            self.pager.clone(),
            sstable_id,
            0, // L0
            self.config.sstable.clone(),
            entries.len(), // estimated_entries
        );

        // Write all entries to the SSTable
        for (key, chain) in entries {
            // Write the full version chain to preserve MVCC history
            writer.add(key, chain)?;
        }

        // Finalize the SSTable with the memtable's max LSN
        let current_lsn = memtable.max_lsn().unwrap_or(LogSequenceNumber::from(0));
        let sstable_metadata = writer.finish(current_lsn)?;

        // Convert SStableMetadata to FileMetadata for the manifest
        let file_metadata = FileMetadata {
            id: sstable_metadata.id,
            level: sstable_metadata.level,
            min_key: sstable_metadata.min_key,
            max_key: sstable_metadata.max_key,
            num_entries: sstable_metadata.num_entries,
            total_size: sstable_metadata.total_size,
            created_lsn: sstable_metadata.created_lsn,
            first_page_id: sstable_metadata.first_page_id,
            num_pages: sstable_metadata.num_pages,
        };

        // Add the new SSTable to the manifest
        let version_edit = VersionEdit::add_sstable(file_metadata);

        self.manifest.apply_edit(version_edit)?;

        Ok(())
    }

    /// Create iterators for all data sources.
    fn create_iterators(
        &self,
        direction: Direction,
        _snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Vec<Box<dyn LsmIterator>>> {
        let mut iterators: Vec<Box<dyn LsmIterator>> = Vec::new();
        let mut priority = 0;

        // Active memtable (highest priority)
        {
            let memtable = self.active_memtable.read().unwrap();
            let iter = MemtableIterator::new(&memtable, direction, priority)?;
            iterators.push(Box::new(iter));
            priority += 1;
        }

        // Immutable memtables (reverse order = newest first)
        {
            let immutable = self.immutable_memtables.read().unwrap();
            for memtable in immutable.iter().rev() {
                let iter = MemtableIterator::new(memtable, direction, priority)?;
                iterators.push(Box::new(iter));
                priority += 1;
            }
        }

        // SSTables (L0 to Ln)
        let version = self.manifest.current();
        for level in 0..version.num_levels() {
            let files = version.level_files(level as u32);

            for file in files.iter().rev() {
                let reader = Arc::new(SStableReader::open(
                    self.pager.clone(),
                    file.first_page_id,
                    self.config.sstable.clone(),
                )?);

                let iter = SStableIterator::new(reader, direction, priority)?;
                iterators.push(Box::new(iter));
                priority += 1;
            }
        }

        Ok(iterators)
    }

    /// Vacuum obsolete versions from the active and immutable memtables.
    ///
    /// Note: This only vacuums memtables. SSTables are immutable and cleaned up
    /// during compaction. For a full vacuum, trigger compaction after calling this.
    ///
    /// Returns the total count of removed versions.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        let mut total_removed = 0;

        // Vacuum active memtable
        total_removed += self
            .active_memtable
            .read()
            .unwrap()
            .vacuum(min_visible_lsn)?;

        // Vacuum immutable memtables
        let immutable = self.immutable_memtables.read().unwrap();
        for memtable in immutable.iter() {
            total_removed += memtable.vacuum(min_visible_lsn)?;
        }

        Ok(total_removed)
    }
}

impl<FS: FileSystem> Table for LsmTree<FS> {
    fn table_id(&self) -> TableId {
        self.table_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> TableEngineKind {
        TableEngineKind::LsmTree
    }

    fn capabilities(&self) -> TableCapabilities {
        TableCapabilities {
            ordered: true,
            point_lookup: true,
            prefix_scan: false, // Could be enabled with proper index
            reverse_scan: true,
            range_delete: true,
            merge_operator: false,
            mvcc_native: true,
            append_optimized: true,
            memory_resident: false,
            disk_resident: true,
            supports_compression: self.config.sstable.compression.is_some(),
            supports_encryption: self.config.sstable.encryption.is_some(),
        }
    }

    fn stats(&self) -> TableResult<TableStatistics> {
        let version = self.manifest.current();
        let mut total_entries = 0u64;
        let mut total_size = 0u64;

        for level in 0..version.num_levels() {
            let files = version.level_files(level as u32);
            for file in files {
                total_entries += file.num_entries;
                total_size += file.total_size;
            }
        }

        Ok(TableStatistics {
            row_count: Some(total_entries),
            total_size_bytes: Some(total_size),
            key_stats: None,
            value_stats: None,
            histogram: None,
            last_updated_lsn: None,
        })
    }
}

impl<FS: FileSystem> SearchableTable for LsmTree<FS> {
    type Reader<'a>
        = LsmReader<'a, FS>
    where
        Self: 'a;
    type Writer<'a>
        = LsmWriter<'a, FS>
    where
        Self: 'a;

    fn reader(&self, snapshot_lsn: LogSequenceNumber) -> TableResult<Self::Reader<'_>> {
        Ok(LsmReader {
            tree: self,
            snapshot_lsn,
        })
    }

    fn writer(
        &self,
        tx_id: TransactionId,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self::Writer<'_>> {
        Ok(LsmWriter {
            tree: self,
            tx_id,
            snapshot_lsn,
            pending_changes: Vec::new(),
        })
    }
}

// =============================================================================
// Reader and Writer
// =============================================================================

/// Read-only view of the LSM tree at a specific snapshot.
pub struct LsmReader<'a, FS: FileSystem> {
    tree: &'a LsmTree<FS>,
    snapshot_lsn: LogSequenceNumber,
}

impl<'a, FS: FileSystem> PointLookup for LsmReader<'a, FS> {
    fn get(&self, key: &[u8], snapshot_lsn: LogSequenceNumber) -> TableResult<Option<ValueBuf>> {
        self.tree
            .get_internal(key, snapshot_lsn)
            .map(|opt| opt.map(ValueBuf))
    }

    fn get_stream(
        &self,
        key: &[u8],
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Option<Box<dyn ValueStream + '_>>> {
        self.tree.get_stream_internal(key, snapshot_lsn)
    }
}

impl<'a, FS: FileSystem> OrderedScan for LsmReader<'a, FS> {
    type Cursor<'b>
        = LsmCursor<'b, FS>
    where
        Self: 'b;

    fn scan(
        &self,
        bounds: ScanBounds,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self::Cursor<'_>> {
        LsmCursor::new(self.tree, bounds, snapshot_lsn)
    }
}

impl<'a, FS: FileSystem> TableReader for LsmReader<'a, FS> {
    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }

    fn approximate_len(&self) -> TableResult<Option<u64>> {
        let stats = self.tree.stats()?;
        Ok(stats.row_count)
    }
}

/// Write view of the LSM tree for a specific transaction.
pub struct LsmWriter<'a, FS: FileSystem> {
    tree: &'a LsmTree<FS>,
    tx_id: TransactionId,
    snapshot_lsn: LogSequenceNumber,
    pending_changes: Vec<(Vec<u8>, Option<Vec<u8>>)>,
}

impl<'a, FS: FileSystem> MutableTable for LsmWriter<'a, FS> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> TableResult<u64> {
        self.pending_changes
            .push((key.to_vec(), Some(value.to_vec())));
        // Return approximate size: key + value + overhead
        Ok((key.len() + value.len() + 16) as u64)
    }

    fn put_stream(&mut self, key: &[u8], stream: &mut dyn ValueStream) -> TableResult<u64> {
        // For LSM tree, we need to buffer the stream into memory for the memtable
        // The memtable is in-memory anyway, so we can't avoid this
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

    fn delete(&mut self, key: &[u8]) -> TableResult<bool> {
        // Check if key exists
        let exists = self.tree.get_internal(key, self.snapshot_lsn)?.is_some();
        if exists {
            self.pending_changes.push((key.to_vec(), None));
        }
        Ok(exists)
    }

    fn range_delete(&mut self, bounds: ScanBounds) -> TableResult<u64> {
        // Range delete in LSM tree: insert tombstones for all keys in range
        let mut deleted_count = 0u64;

        // Create a cursor to scan the range
        let reader = self.tree.reader(self.snapshot_lsn)?;
        let mut cursor = reader.scan(bounds.clone(), self.snapshot_lsn)?;

        // Collect keys to delete (can't delete while iterating)
        let mut keys_to_delete = Vec::new();
        while cursor.valid() {
            if let Some(key) = cursor.key() {
                keys_to_delete.push(key.to_vec());
            }
            cursor.next()?;
        }

        // Insert tombstones for each key
        for key in keys_to_delete {
            self.pending_changes.push((key, None));
            deleted_count += 1;
        }

        Ok(deleted_count)
    }
}

impl<'a, FS: FileSystem> BatchOps for LsmWriter<'a, FS> {
    fn batch_get(&self, keys: &[&[u8]]) -> TableResult<Vec<Option<ValueBuf>>> {
        let mut results = Vec::with_capacity(keys.len());
        for key in keys {
            results.push(
                self.tree
                    .get_internal(key, self.snapshot_lsn)?
                    .map(ValueBuf),
            );
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

impl<'a, FS: FileSystem> Flushable for LsmWriter<'a, FS> {
    fn flush(&mut self) -> TableResult<()> {
        if self.pending_changes.is_empty() {
            return Ok(());
        }

        // Apply all pending changes with uncommitted versions
        // Versions will be committed by calling commit_versions() after transaction commit
        for (key, value_opt) in self.pending_changes.drain(..) {
            match value_opt {
                Some(value) => {
                    // Insert or update - leave uncommitted
                    self.tree.insert_internal(key, value, self.tx_id, None)?;
                }
                None => {
                    // Delete (insert tombstone) - leave uncommitted
                    self.tree.delete_internal(key, self.tx_id, None)?;
                }
            }
        }

        Ok(())
    }
}

impl<'a, FS: FileSystem> TableWriter for LsmWriter<'a, FS> {
    fn tx_id(&self) -> TransactionId {
        self.tx_id
    }

    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }
}

impl<'a, FS: FileSystem> LsmWriter<'a, FS> {
    /// Mark all versions created by this transaction as committed.
    ///
    /// This must be called after flush() to make the changes visible to readers.
    /// The commit_lsn is obtained from the WAL after writing the COMMIT record.
    pub fn commit_versions(&self, commit_lsn: LogSequenceNumber) -> TableResult<()> {
        // Commit versions in the active memtable
        let active_memtable = self.tree.active_memtable.read().unwrap();
        active_memtable.commit_versions(self.tx_id, commit_lsn)?;

        // Note: We don't need to commit in immutable memtables because those are
        // already being flushed to SSTables and won't have uncommitted data from
        // this transaction. The flush() method only writes to the active memtable.

        Ok(())
    }
}

// =============================================================================
// Cursor
// =============================================================================

/// Cursor for iterating over the LSM tree.
pub struct LsmCursor<'a, FS: FileSystem> {
    tree: &'a LsmTree<FS>,
    snapshot_lsn: LogSequenceNumber,
    bounds: ScanBounds,
    merge_iterator: Option<MergeIterator>,
    current_key: Option<Vec<u8>>,
    current_value: Option<Vec<u8>>,
    exhausted: bool,
    initialized: bool,
}

impl<'a, FS: FileSystem> LsmCursor<'a, FS> {
    fn new(
        tree: &'a LsmTree<FS>,
        bounds: ScanBounds,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self> {
        Ok(Self {
            tree,
            snapshot_lsn,
            bounds,
            merge_iterator: None,
            current_key: None,
            current_value: None,
            exhausted: false,
            initialized: false,
        })
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

    fn ensure_initialized(&mut self) -> TableResult<()> {
        if self.initialized {
            return Ok(());
        }

        // Create merge iterator
        let iterators = self
            .tree
            .create_iterators(Direction::Forward, self.snapshot_lsn)?;
        let mut merge_iter = MergeIterator::new(iterators, Direction::Forward, self.snapshot_lsn)?;

        // Position at start of bounds
        match &self.bounds {
            ScanBounds::All => {
                merge_iter.seek_to_first()?;
            }
            ScanBounds::Prefix(prefix) => {
                merge_iter.seek(&prefix.0)?;
            }
            ScanBounds::Range { start, .. } => match start {
                Bound::Included(k) | Bound::Excluded(k) => {
                    merge_iter.seek(&k.0)?;
                }
                Bound::Unbounded => {
                    merge_iter.seek_to_first()?;
                }
            },
        }

        self.merge_iterator = Some(merge_iter);
        self.initialized = true;
        self.load_current()
    }

    fn load_current(&mut self) -> TableResult<()> {
        let merge_iter = self.merge_iterator.as_mut().unwrap();

        if let Some((key, value)) = merge_iter.current() {
            let key_vec = key.to_vec();
            let value_vec = value.to_vec();

            if self.is_in_bounds(&key_vec) {
                self.current_key = Some(key_vec);
                self.current_value = Some(value_vec);
                self.exhausted = false;
            } else {
                self.current_key = None;
                self.current_value = None;
                self.exhausted = true;
            }
        } else {
            self.current_key = None;
            self.current_value = None;
            self.exhausted = true;
        }

        Ok(())
    }
}

impl<FS: FileSystem> Drop for LsmTree<FS> {
    /// Flush memtables on drop to ensure data durability.
    ///
    /// This Drop implementation ensures that any unflushed data in the active
    /// memtable is written to disk before the LSM tree is destroyed. This is
    /// critical for data durability when the database is closed.
    ///
    /// Note: Errors during drop are logged but not propagated since Drop cannot
    /// return errors. For controlled shutdown with error handling, use the
    /// explicit flush_memtable() method before dropping.
    fn drop(&mut self) {
        // Attempt to flush the active memtable
        if let Err(e) = self.flush_memtable() {
            eprintln!(
                "Warning: Failed to flush LSM memtable during drop for table '{}': {}",
                self.name, e
            );
        }
    }
}

impl<'a, FS: FileSystem> TableCursor for LsmCursor<'a, FS> {
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
        self.ensure_initialized()?;

        if self.exhausted {
            return Ok(());
        }

        let merge_iter = self.merge_iterator.as_mut().unwrap();
        merge_iter.step_forward()?;
        self.load_current()
    }

    fn prev(&mut self) -> TableResult<()> {
        self.ensure_initialized()?;

        if self.exhausted {
            return Ok(());
        }

        // For prev(), we need to switch to backward iteration if not already
        // This is a simplified implementation - ideally we'd maintain both directions
        // For now, we'll recreate the iterator in backward mode
        let iterators = self
            .tree
            .create_iterators(Direction::Backward, self.snapshot_lsn)?;
        let mut merge_iter = MergeIterator::new(iterators, Direction::Backward, self.snapshot_lsn)?;

        // If we have a current key, seek to it and move backward
        if let Some(ref current_key) = self.current_key {
            merge_iter.seek(current_key)?;
            // Move to previous entry
            merge_iter.step_forward()?;
        } else {
            // Start from last
            merge_iter.seek_to_last()?;
        }

        self.merge_iterator = Some(merge_iter);
        self.load_current()
    }

    fn seek(&mut self, key: &[u8]) -> TableResult<()> {
        self.ensure_initialized()?;

        let merge_iter = self.merge_iterator.as_mut().unwrap();
        merge_iter.seek(key)?;
        self.load_current()
    }

    fn seek_for_prev(&mut self, key: &[u8]) -> TableResult<()> {
        self.ensure_initialized()?;

        // Create backward iterator
        let iterators = self
            .tree
            .create_iterators(Direction::Backward, self.snapshot_lsn)?;
        let mut merge_iter = MergeIterator::new(iterators, Direction::Backward, self.snapshot_lsn)?;

        // Seek to the key (finds first entry <= key in backward mode)
        merge_iter.seek(key)?;

        self.merge_iterator = Some(merge_iter);
        self.load_current()
    }

    fn first(&mut self) -> TableResult<()> {
        self.ensure_initialized()?;

        let merge_iter = self.merge_iterator.as_mut().unwrap();
        merge_iter.seek_to_first()?;
        self.load_current()
    }

    fn last(&mut self) -> TableResult<()> {
        self.ensure_initialized()?;

        // Create backward iterator and seek to last
        let iterators = self
            .tree
            .create_iterators(Direction::Backward, self.snapshot_lsn)?;
        let mut merge_iter = MergeIterator::new(iterators, Direction::Backward, self.snapshot_lsn)?;
        merge_iter.seek_to_last()?;

        self.merge_iterator = Some(merge_iter);
        self.load_current()
    }

    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }
}
