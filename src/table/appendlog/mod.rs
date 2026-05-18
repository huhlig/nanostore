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

//! `AppendLog` table engine implementation.
//!
//! This module provides an append-only log storage engine optimized for
//! write-heavy workloads where data is only appended, never updated in place.
//!
//! # Architecture
//!
//! ```text
//! Writes → Active Segment → Roll to Immutable Segment
//!                              ↓
//!                         Compaction (optional)
//!                              ↓
//!                         Archived Segments
//! ```
//!
//! # Features
//!
//! - **Append-only**: Sequential writes for maximum throughput
//! - **Segment rolling**: Automatic segment creation when size threshold reached
//! - **In-memory index**: Fast point lookups via offset index with version chains
//! - **Sequential scans**: Efficient range queries over time-ordered data
//! - **MVCC support**: Full multi-version concurrency control with snapshot isolation
//! - **Transaction support**: Transactional puts and deletes with commit/rollback
//! - **Version vacuuming**: Automatic cleanup of old versions
//! - **Optional compaction**: Merge old segments to reclaim space
//! - **Retention policies**: Automatic cleanup of old data
//! - **Compression**: Optional per-segment compression
//!
//! # Use Cases
//!
//! - Event logs and audit trails
//! - Time-series data (simpler alternative to `TimeSeries` engine)
//! - Write-ahead logs
//! - Message queues
//! - Append-only databases
//!
//! # MVCC Implementation
//!
//! The `AppendLog` uses version chains to track multiple versions of each key:
//! - Each key maps to a `(segment_id, offset, VersionChain)` tuple
//! - New versions are prepended to the chain on updates
//! - Deletes create tombstone versions (empty values)
//! - Snapshot isolation ensures consistent reads
//! - Old versions are removed during vacuum operations

mod config;
mod segment;

pub use self::config::{AppendLogConfig, CompressionType, RetentionPolicy};
pub use self::segment::{PersistedSegment, Segment, SegmentId, SegmentMetadata};

use crate::pager::{PageId, Pager};
use crate::snap::Snapshot;
use crate::table::{
    Flushable, MutableTable, OrderedScan, PointLookup, Table, TableCapabilities, TableCursor,
    TableEngineKind, TableResult, TableStatistics,
};
use crate::txn::{TransactionId, VersionChain};
use crate::types::{Bound, ScanBounds, ValueBuf};
use crate::vfs::FileSystem;
use crate::wal::LogSequenceNumber;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

// =============================================================================
// AppendLog Table Implementation
// =============================================================================

/// AppendLog storage engine.
///
/// Provides an append-only log storage engine optimized for write-heavy
/// workloads. Data is written sequentially to segments, with an in-memory
/// index for fast lookups.
pub struct AppendLog<FS: FileSystem> {
    /// Table identifier
    table_id: crate::types::TableId,

    /// Table name
    name: String,

    /// Configuration
    config: AppendLogConfig,

    /// Pager for persistent storage
    pager: Arc<Pager<FS>>,

    /// Root page ID (stores metadata)
    root_page_id: PageId,

    /// Internal state
    state: RwLock<AppendLogState>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PersistedAppendLogEntry {
    key: Vec<u8>,
    segment_id: SegmentId,
    offset: u64,
    chain: VersionChain,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PersistedAppendLogState {
    active_segment: PersistedSegment,
    immutable_segments: Vec<PersistedSegment>,
    index: Vec<PersistedAppendLogEntry>,
    next_segment_id: SegmentId,
    entry_count: u64,
    total_size: u64,
}

/// Internal mutable state of the AppendLog.
struct AppendLogState {
    /// Active segment being written to
    active_segment: Segment,

    /// Immutable segments (segment_id -> segment)
    immutable_segments: BTreeMap<SegmentId, Segment>,

    /// In-memory index: key -> (segment_id, offset, version_chain)
    /// The version chain tracks all versions of this key for MVCC
    index: BTreeMap<Vec<u8>, (SegmentId, u64, VersionChain)>,

    /// Next segment ID to allocate
    next_segment_id: SegmentId,

    /// Total number of entries
    entry_count: u64,

    /// Total size in bytes
    total_size: u64,
}

impl<FS: FileSystem> AppendLog<FS> {
    /// Create a new AppendLog table.
    pub fn new(
        table_id: crate::types::TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        config: AppendLogConfig,
    ) -> TableResult<Self> {
        // Allocate root page for metadata - use LsmMeta as placeholder
        let root_page_id = pager
            .allocate_page(crate::pager::PageType::LsmMeta)
            .map_err(|e| {
                crate::table::TableError::Other(format!("Failed to allocate root page: {}", e))
            })?;

        // Create initial active segment
        let active_segment = Segment::new(SegmentId(0), pager.clone())?;

        let state = AppendLogState {
            active_segment,
            immutable_segments: BTreeMap::new(),
            index: BTreeMap::new(),
            next_segment_id: SegmentId(1),
            entry_count: 0,
            total_size: 0,
        };

        let log = Self {
            table_id,
            name,
            config,
            pager,
            root_page_id,
            state: RwLock::new(state),
        };
        log.persist_metadata()?;
        Ok(log)
    }

    /// Open an existing AppendLog table.
    pub fn open(
        table_id: crate::types::TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        root_page_id: PageId,
        config: AppendLogConfig,
    ) -> TableResult<Self> {
        let page = pager.read_page(root_page_id).map_err(|e| {
            crate::table::TableError::Other(format!(
                "Failed to read AppendLog metadata page {}: {}",
                root_page_id, e
            ))
        })?;

        let persisted: PersistedAppendLogState = serde_json::from_slice(&page.data).map_err(|e| {
            crate::table::TableError::Other(format!(
                "Failed to deserialize AppendLog metadata from page {}: {}",
                root_page_id, e
            ))
        })?;

        let active_segment = Segment::from_persisted(persisted.active_segment, pager.clone())?;
        let mut immutable_segments = BTreeMap::new();
        for segment in persisted.immutable_segments {
            let restored = Segment::from_persisted(segment, pager.clone())?;
            immutable_segments.insert(restored.id(), restored);
        }

        let index = persisted
            .index
            .into_iter()
            .map(|entry| (entry.key, (entry.segment_id, entry.offset, entry.chain)))
            .collect();

        let state = AppendLogState {
            active_segment,
            immutable_segments,
            index,
            next_segment_id: persisted.next_segment_id,
            entry_count: persisted.entry_count,
            total_size: persisted.total_size,
        };

        Ok(Self {
            table_id,
            name,
            config,
            pager,
            root_page_id,
            state: RwLock::new(state),
        })
    }

    /// Get the root page ID.
    pub fn root_page_id(&self) -> PageId {
        self.root_page_id
    }

    /// Check if active segment should be rolled.
    fn should_roll_segment(state: &AppendLogState, config: &AppendLogConfig) -> bool {
        state.active_segment.size() >= config.segment_size
    }

    /// Roll the active segment to immutable.
    fn roll_segment(state: &mut AppendLogState, pager: Arc<Pager<FS>>) -> TableResult<()> {
        // Move active segment to immutable
        let old_segment_id = state.active_segment.id();
        let old_segment = std::mem::replace(
            &mut state.active_segment,
            Segment::new(state.next_segment_id, pager)?,
        );

        state.immutable_segments.insert(old_segment_id, old_segment);
        state.next_segment_id = SegmentId(state.next_segment_id.0 + 1);

        debug!("Rolled segment {} to immutable", old_segment_id.0);

        Ok(())
    }

    fn persist_metadata(&self) -> TableResult<()> {
        let state = self.state.read().unwrap();

        let active_segment = state.active_segment.persist()?;
        let immutable_segments = state
            .immutable_segments
            .values()
            .map(|segment| segment.persist())
            .collect::<TableResult<Vec<_>>>()?;
        let index = state
            .index
            .iter()
            .map(|(key, (segment_id, offset, chain))| PersistedAppendLogEntry {
                key: key.clone(),
                segment_id: *segment_id,
                offset: *offset,
                chain: chain.clone(),
            })
            .collect();

        let persisted = PersistedAppendLogState {
            active_segment,
            immutable_segments,
            index,
            next_segment_id: state.next_segment_id,
            entry_count: state.entry_count,
            total_size: state.total_size,
        };

        let metadata = serde_json::to_vec(&persisted).map_err(|e| {
            crate::table::TableError::Other(format!(
                "Failed to serialize AppendLog metadata: {}",
                e
            ))
        })?;

        let max_metadata = crate::pager::PageSize::default().data_size();
        if metadata.len() > max_metadata {
            return Err(crate::table::TableError::Other(format!(
                "AppendLog metadata too large for root page: {} > {}",
                metadata.len(),
                max_metadata
            )));
        }

        let mut page = crate::pager::Page::new(
            self.root_page_id,
            crate::pager::PageType::LsmMeta,
            metadata.len(),
        );
        page.data = metadata;
        self.pager.write_page(&page).map_err(|e| {
            crate::table::TableError::Other(format!(
                "Failed to write AppendLog metadata page {}: {}",
                self.root_page_id, e
            ))
        })?;

        Ok(())
    }

    /// Apply retention policy to remove old segments.
    fn apply_retention(state: &mut AppendLogState, policy: &RetentionPolicy) -> TableResult<()> {
        match policy {
            RetentionPolicy::None => Ok(()),
            RetentionPolicy::MaxSegments(max) => {
                // Remove oldest segments if we exceed the limit
                while state.immutable_segments.len() > *max {
                    if let Some((segment_id, _)) = state.immutable_segments.iter().next() {
                        let segment_id = *segment_id;
                        state.immutable_segments.remove(&segment_id);
                        // Remove index entries for this segment
                        state
                            .index
                            .retain(|_, (seg_id, _, _)| *seg_id != segment_id);
                        debug!("Removed segment {} due to retention policy", segment_id.0);
                    } else {
                        break;
                    }
                }
                Ok(())
            }
            RetentionPolicy::MaxAge(duration) => {
                // Remove segments older than the specified duration
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let cutoff = now.saturating_sub(duration.as_secs());

                let to_remove: Vec<SegmentId> = state
                    .immutable_segments
                    .iter()
                    .filter(|(_, seg)| seg.created_at() < cutoff)
                    .map(|(id, _)| *id)
                    .collect();

                for segment_id in to_remove {
                    state.immutable_segments.remove(&segment_id);
                    state
                        .index
                        .retain(|_, (seg_id, _, _)| *seg_id != segment_id);
                    debug!("Removed segment {} due to age retention", segment_id.0);
                }
                Ok(())
            }
        }
    }

    /// Insert or update a key-value pair with transaction tracking (MVCC-aware).
    pub fn put_tx(&self, key: &[u8], value: &[u8], tx_id: TransactionId) -> TableResult<u64> {
        let mut state = self.state.write().unwrap();

        // Check if we need to roll the segment
        if state.active_segment.size() >= self.config.segment_size {
            Self::roll_segment(&mut state, self.pager.clone())?;
            Self::apply_retention(&mut state, &self.config.retention_policy)?;
        }

        // Append to active segment
        let offset = state.active_segment.append(key, value)?;
        let segment_id = state.active_segment.id();

        // Update index with version chain
        let new_chain = if let Some((_, _, existing_chain)) = state.index.get(key) {
            // Prepend new version to existing chain
            existing_chain.clone().prepend(value.to_vec(), tx_id)
        } else {
            // Create new version chain
            VersionChain::new(value.to_vec(), tx_id)
        };

        state
            .index
            .insert(key.to_vec(), (segment_id, offset, new_chain));

        // Update statistics
        state.entry_count += 1;
        let bytes_written = (key.len() + value.len()) as u64;
        state.total_size += bytes_written;

        drop(state);
        self.persist_metadata()?;

        Ok(bytes_written)
    }

    /// Delete a key with transaction tracking (MVCC-aware).
    pub fn delete_tx(&self, key: &[u8], tx_id: TransactionId) -> TableResult<bool> {
        let mut state = self.state.write().unwrap();

        // Check if key exists and clone the data we need
        let entry_data = state
            .index
            .get(key)
            .map(|(seg_id, off, chain)| (*seg_id, *off, chain.clone()));

        let existed = entry_data.is_some();

        if let Some((segment_id, offset, existing_chain)) = entry_data {
            // Create a tombstone version (empty value)
            let tombstone_chain = existing_chain.prepend(Vec::new(), tx_id);
            state
                .index
                .insert(key.to_vec(), (segment_id, offset, tombstone_chain));
        }

        if existed {
            state.entry_count = state.entry_count.saturating_sub(1);
        }

        drop(state);
        self.persist_metadata()?;

        Ok(existed)
    }

    /// Get a value with snapshot visibility (MVCC-aware).
    pub fn get_snapshot(&self, key: &[u8], snapshot: &Snapshot) -> TableResult<Option<ValueBuf>> {
        let state = self.state.read().unwrap();

        // Look up key in index
        if let Some((_segment_id, _offset, chain)) = state.index.get(key) {
            // Find visible version
            if let Some(value) = chain.find_visible_version(snapshot) {
                // Empty value means tombstone (deleted)
                if value.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(ValueBuf(value.to_vec())));
            }
        }

        Ok(None)
    }

    /// Commit all uncommitted versions for a transaction.
    pub fn commit_versions(
        &self,
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        let mut state = self.state.write().unwrap();

        // Commit all versions created by this transaction
        for (_, _, chain) in state.index.values_mut() {
            Self::commit_chain(chain, tx_id, commit_lsn);
        }

        drop(state);
        self.persist_metadata()?;

        Ok(())
    }

    /// Helper to commit a version chain recursively.
    fn commit_chain(chain: &mut VersionChain, tx_id: TransactionId, commit_lsn: LogSequenceNumber) {
        if chain.created_by == tx_id && chain.commit_lsn.is_none() {
            chain.commit(commit_lsn);
        }
        if let Some(prev) = &mut chain.prev_version {
            Self::commit_chain(prev, tx_id, commit_lsn);
        }
    }

    /// Vacuum old versions that are no longer visible.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        let mut state = self.state.write().unwrap();
        let mut total_removed = 0usize;

        for (_, _, chain) in state.index.values_mut() {
            total_removed += Self::vacuum_chain(chain, min_visible_lsn);
        }

        drop(state);
        self.persist_metadata()?;

        Ok(total_removed)
    }

    fn latest_chain_lsn(chain: &VersionChain) -> Option<LogSequenceNumber> {
        let mut current = Some(chain);
        let mut latest: Option<LogSequenceNumber> = None;

        while let Some(version) = current {
            if let Some(commit_lsn) = version.commit_lsn {
                latest = Some(match latest {
                    Some(current_max) => current_max.max(commit_lsn),
                    None => commit_lsn,
                });
            }
            current = version.prev_version.as_deref();
        }

        latest
    }

    /// Helper to vacuum a version chain.
    fn vacuum_chain(chain: &mut VersionChain, min_visible_lsn: LogSequenceNumber) -> usize {
        let mut removed = 0usize;

        // Traverse and remove old versions
        let mut current = chain;
        loop {
            // Check if we should remove the next version
            let should_remove = if let Some(prev) = &current.prev_version {
                if let Some(commit_lsn) = prev.commit_lsn {
                    commit_lsn < min_visible_lsn
                } else {
                    false
                }
            } else {
                false
            };

            if should_remove {
                // Remove this version and all older ones
                current.prev_version = None;
                removed += 1;
                break;
            }

            // Move to next version
            if let Some(prev) = &mut current.prev_version {
                current = prev;
            } else {
                break;
            }
        }

        removed
    }
}

// =============================================================================
// Table Trait Implementation
// =============================================================================

impl<FS: FileSystem> Table for AppendLog<FS> {
    fn table_id(&self) -> crate::types::TableId {
        self.table_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> TableEngineKind {
        TableEngineKind::AppendLog
    }

    fn capabilities(&self) -> TableCapabilities {
        TableCapabilities {
            ordered: true,
            point_lookup: true,
            prefix_scan: false,
            reverse_scan: false,
            range_delete: false,
            merge_operator: false,
            mvcc_native: false,
            append_optimized: true,
            memory_resident: false,
            disk_resident: true,
            supports_compression: true,
            supports_encryption: false,
        }
    }

    fn stats(&self) -> TableResult<TableStatistics> {
        let state = self.state.read().unwrap();
        Ok(TableStatistics {
            row_count: Some(state.entry_count),
            total_size_bytes: Some(state.total_size),
            key_stats: None,
            value_stats: None,
            histogram: None,
            last_updated_lsn: state
                .index
                .values()
                .filter_map(|(_, _, chain)| Self::latest_chain_lsn(chain))
                .max()
                .or_else(|| {
                    let active_lsn = state.active_segment.latest_lsn();
                    (active_lsn != LogSequenceNumber::from(0)).then_some(active_lsn)
                }),
        })
    }
}

// =============================================================================
// PointLookup Trait Implementation
// =============================================================================

impl<FS: FileSystem> PointLookup for AppendLog<FS> {
    fn get(&self, key: &[u8], snapshot_lsn: LogSequenceNumber) -> TableResult<Option<ValueBuf>> {
        let state = self.state.read().unwrap();

        // Look up key in index
        if let Some((_segment_id, _offset, chain)) = state.index.get(key) {
            // Create a snapshot for visibility checking
            let snapshot = Snapshot::new(
                crate::snap::SnapshotId::from(0),
                String::new(),
                snapshot_lsn,
                0,
                0,
                Vec::new(),
            );

            // Find visible version
            if let Some(value) = chain.find_visible_version(&snapshot) {
                // Empty value means tombstone (deleted)
                if value.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(ValueBuf(value.to_vec())));
            }
        }

        Ok(None)
    }
}

// =============================================================================
// MutableTable Trait Implementation
// =============================================================================

impl<FS: FileSystem> MutableTable for &AppendLog<FS> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> TableResult<u64> {
        // Use a default transaction ID for non-transactional puts
        // This maintains backward compatibility
        let tx_id = TransactionId::from(0);
        self.put_tx(key, value, tx_id)
    }

    fn delete(&mut self, key: &[u8]) -> TableResult<bool> {
        // Use a default transaction ID for non-transactional deletes
        let tx_id = TransactionId::from(0);
        self.delete_tx(key, tx_id)
    }

    fn range_delete(&mut self, _bounds: ScanBounds) -> TableResult<u64> {
        // Range delete not supported for append-only logs
        Err(crate::table::TableError::Other(
            "Range delete not supported for AppendLog".to_string(),
        ))
    }
}

// =============================================================================
// OrderedScan Trait Implementation
// =============================================================================

impl<FS: FileSystem> OrderedScan for AppendLog<FS> {
    type Cursor<'a>
        = AppendLogCursor<'a, FS>
    where
        FS: 'a;

    fn scan(
        &self,
        bounds: ScanBounds,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self::Cursor<'_>> {
        AppendLogCursor::new(self, bounds, snapshot_lsn)
    }
}

// =============================================================================
// Flushable Trait Implementation
// =============================================================================

impl<FS: FileSystem> Flushable for &AppendLog<FS> {
    fn flush(&mut self) -> TableResult<()> {
        let state = self.state.read().unwrap();
        state.active_segment.flush()?;
        Ok(())
    }
}

// =============================================================================
// Cursor Implementation
// =============================================================================

/// Cursor for scanning AppendLog entries.
pub struct AppendLogCursor<'a, FS: FileSystem> {
    /// Reference to the AppendLog
    log: &'a AppendLog<FS>,

    /// Current position in the index
    current: Option<(Vec<u8>, (SegmentId, u64, VersionChain))>,

    /// Iterator over index entries
    iter: std::vec::IntoIter<(Vec<u8>, (SegmentId, u64, VersionChain))>,

    /// Snapshot for visibility checking
    snapshot: Snapshot,
}

impl<'a, FS: FileSystem> AppendLogCursor<'a, FS> {
    fn new(
        log: &'a AppendLog<FS>,
        bounds: ScanBounds,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self> {
        let state = log.state.read().unwrap();
        let snapshot = Snapshot::new(
            crate::snap::SnapshotId::from(0),
            String::new(),
            snapshot_lsn,
            0,
            0,
            Vec::new(),
        );

        // Collect entries within bounds
        let entries: Vec<_> = match bounds {
            ScanBounds::All => state
                .index
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            ScanBounds::Prefix(prefix) => state
                .index
                .range(prefix.0.clone()..)
                .take_while(|(k, _)| k.starts_with(&prefix.0))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            ScanBounds::Range { start, end } => {
                let start_bound = match start {
                    Bound::Unbounded => std::ops::Bound::Unbounded,
                    Bound::Included(k) => std::ops::Bound::Included(k.0),
                    Bound::Excluded(k) => std::ops::Bound::Excluded(k.0),
                };
                let end_bound = match end {
                    Bound::Unbounded => std::ops::Bound::Unbounded,
                    Bound::Included(k) => std::ops::Bound::Included(k.0),
                    Bound::Excluded(k) => std::ops::Bound::Excluded(k.0),
                };
                state
                    .index
                    .range((start_bound, end_bound))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            }
        };

        let mut iter = entries.into_iter();
        let current = iter.next();

        Ok(Self {
            log,
            current,
            iter,
            snapshot,
        })
    }
}

impl<'a, FS: FileSystem> TableCursor for AppendLogCursor<'a, FS> {
    fn valid(&self) -> bool {
        self.current.is_some()
    }

    fn key(&self) -> Option<&[u8]> {
        self.current.as_ref().map(|(k, _)| k.as_slice())
    }

    fn value(&self) -> Option<&[u8]> {
        // AppendLog cursor doesn't support direct value access
        // Users should use get() with the key instead
        None
    }

    fn next(&mut self) -> TableResult<()> {
        self.current = self.iter.next();
        Ok(())
    }

    fn prev(&mut self) -> TableResult<()> {
        // Reverse scan not supported
        Err(crate::table::TableError::Other(
            "Reverse scan not supported for AppendLog".to_string(),
        ))
    }

    fn seek(&mut self, _key: &[u8]) -> TableResult<()> {
        // Seek not efficiently supported without rebuilding iterator
        Err(crate::table::TableError::Other(
            "Seek not supported for AppendLog cursor".to_string(),
        ))
    }

    fn seek_for_prev(&mut self, _key: &[u8]) -> TableResult<()> {
        Err(crate::table::TableError::Other(
            "Seek for prev not supported for AppendLog".to_string(),
        ))
    }

    fn first(&mut self) -> TableResult<()> {
        // Reset to beginning - would need to rebuild iterator
        Err(crate::table::TableError::Other(
            "First not supported for AppendLog cursor".to_string(),
        ))
    }

    fn last(&mut self) -> TableResult<()> {
        Err(crate::table::TableError::Other(
            "Last not supported for AppendLog cursor".to_string(),
        ))
    }

    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot.lsn
    }
}

// Made with Bob
