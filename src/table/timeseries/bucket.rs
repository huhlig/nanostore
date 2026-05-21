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

//! Time-based bucketing for time series data.
//!
//! This module implements time-based bucketing to organize time series data
//! into manageable chunks. Each bucket contains data points within a specific
//! time range, enabling efficient range queries and data management.

use crate::pager::{Page, PageId, PageType, Pager};
use crate::snap::Snapshot;
use crate::table::TableResult;
use crate::txn::{TransactionId, VersionChain};
use crate::vfs::FileSystem;
use crate::wal::LogSequenceNumber;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Tombstone for a rolled-back time series point.
///
/// When a transaction is rolled back, we mark the appended points as tombstoned
/// so they are filtered out during scans. This allows us to support rollback
/// for append-only time series data.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct TimeSeriesTombstone {
    /// Series key
    pub series_key: Vec<u8>,
    /// Timestamp of the tombstoned point
    pub timestamp: i64,
    /// Transaction ID that created this tombstone
    pub tombstone_tx_id: TransactionId,
    /// LSN when the tombstone was created
    pub tombstone_lsn: Option<LogSequenceNumber>,
}

impl TimeSeriesTombstone {
    /// Create a new tombstone for a time series point.
    pub fn new(series_key: Vec<u8>, timestamp: i64, tx_id: TransactionId) -> Self {
        Self {
            series_key,
            timestamp,
            tombstone_tx_id: tx_id,
            tombstone_lsn: None,
        }
    }

    /// Mark the tombstone as committed.
    pub fn commit(&mut self, lsn: LogSequenceNumber) {
        self.tombstone_lsn = Some(lsn);
    }

    /// Check if this tombstone is visible to a snapshot.
    pub fn is_visible(&self, snapshot: &Snapshot) -> bool {
        if let Some(lsn) = self.tombstone_lsn {
            lsn <= snapshot.lsn
        } else {
            false
        }
    }
}

/// Unique identifier for a time bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BucketId(pub u64);

impl BucketId {
    /// Create a new bucket ID from a timestamp and bucket size.
    #[must_use]
    pub fn from_timestamp(timestamp: i64, bucket_size: u64) -> Self {
        // Ensure we handle negative timestamps correctly
        let bucket_num = if timestamp >= 0 {
            (timestamp as u64) / bucket_size
        } else {
            // For negative timestamps, round down to the bucket
            let abs_ts = (-timestamp) as u64;
            let bucket_offset = abs_ts.div_ceil(bucket_size);
            u64::MAX - bucket_offset + 1
        };
        BucketId(bucket_num)
    }

    /// Get the start timestamp for this bucket.
    #[must_use]
    pub fn start_timestamp(&self, bucket_size: u64) -> i64 {
        if self.0 > (i64::MAX as u64) / bucket_size {
            // Handle overflow for very large bucket IDs
            i64::MAX
        } else {
            (self.0 * bucket_size) as i64
        }
    }

    /// Get the end timestamp for this bucket (exclusive).
    #[must_use]
    pub fn end_timestamp(&self, bucket_size: u64) -> i64 {
        self.start_timestamp(bucket_size)
            .saturating_add(bucket_size as i64)
    }
}

/// A time bucket containing data points for a specific time range.
pub struct TimeBucket<FS: FileSystem> {
    /// Bucket identifier
    id: BucketId,

    /// Start timestamp (inclusive)
    start_ts: i64,

    /// End timestamp (exclusive)
    end_ts: i64,

    /// Data points in this bucket: (timestamp, `version_chain`)
    /// Each timestamp can have multiple versions for MVCC support
    points: BTreeMap<i64, VersionChain>,

    /// Tombstones for rolled-back points: (timestamp, tombstone)
    /// These mark points that should be filtered out during scans
    tombstones: BTreeMap<i64, TimeSeriesTombstone>,

    /// Page ID where this bucket is stored (if persisted)
    page_id: Option<PageId>,

    /// Pager for persistent storage
    pager: std::sync::Arc<Pager<FS>>,

    /// Whether the bucket has been modified since last flush
    dirty: bool,

    /// Last access time (for LRU eviction)
    last_access_time: u64,
}

impl<FS: FileSystem> TimeBucket<FS> {
    /// Create a new time bucket.
    #[must_use]
    pub fn new(id: BucketId, bucket_size: u64, pager: std::sync::Arc<Pager<FS>>) -> Self {
        let start_ts = id.start_timestamp(bucket_size);
        let end_ts = id.end_timestamp(bucket_size);

        Self {
            id,
            start_ts,
            end_ts,
            points: BTreeMap::new(),
            tombstones: BTreeMap::new(),
            page_id: None,
            pager,
            dirty: false,
            last_access_time: Self::current_timestamp(),
        }
    }

    /// Get current timestamp in seconds since UNIX epoch.
    fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    /// Update the last access time.
    fn touch(&mut self) {
        self.last_access_time = Self::current_timestamp();
    }

    /// Get the bucket ID.
    #[must_use]
    pub fn id(&self) -> BucketId {
        self.id
    }

    /// Get the start timestamp.
    #[must_use]
    pub fn start_timestamp(&self) -> i64 {
        self.start_ts
    }

    /// Get the end timestamp.
    #[must_use]
    pub fn end_timestamp(&self) -> i64 {
        self.end_ts
    }

    /// Check if a timestamp falls within this bucket.
    #[must_use]
    pub fn contains_timestamp(&self, timestamp: i64) -> bool {
        timestamp >= self.start_ts && timestamp < self.end_ts
    }

    /// Insert a data point into the bucket with transaction tracking.
    pub fn insert(
        &mut self,
        timestamp: i64,
        value_key: Vec<u8>,
        tx_id: TransactionId,
    ) -> TableResult<()> {
        if !self.contains_timestamp(timestamp) {
            return Err(crate::table::TableError::Other(format!(
                "Timestamp {} is outside bucket range [{}, {})",
                timestamp, self.start_ts, self.end_ts
            )));
        }

        // Create or update version chain
        let new_chain = if let Some(existing_chain) = self.points.remove(&timestamp) {
            // Prepend new version to existing chain
            existing_chain.prepend(value_key, tx_id)
        } else {
            // Create new version chain
            VersionChain::new(value_key, tx_id)
        };

        self.points.insert(timestamp, new_chain);
        self.dirty = true;
        Ok(())
    }

    /// Add a tombstone for a rolled-back point.
    ///
    /// This marks a point as deleted due to transaction rollback.
    /// The point will be filtered out during scans.
    pub fn add_tombstone(
        &mut self,
        series_key: Vec<u8>,
        timestamp: i64,
        tx_id: TransactionId,
    ) -> TableResult<()> {
        if !self.contains_timestamp(timestamp) {
            return Err(crate::table::TableError::Other(format!(
                "Timestamp {} is outside bucket range [{}, {})",
                timestamp, self.start_ts, self.end_ts
            )));
        }

        let tombstone = TimeSeriesTombstone::new(series_key, timestamp, tx_id);
        self.tombstones.insert(timestamp, tombstone);
        self.dirty = true;
        Ok(())
    }

    /// Commit all tombstones created by the given transaction.
    pub fn commit_tombstones(&mut self, tx_id: TransactionId, commit_lsn: LogSequenceNumber) {
        for tombstone in self.tombstones.values_mut() {
            if tombstone.tombstone_tx_id == tx_id && tombstone.tombstone_lsn.is_none() {
                tombstone.commit(commit_lsn);
            }
        }
        self.dirty = true;
    }

    /// Check if a timestamp is tombstoned for a given snapshot.
    fn is_tombstoned(&self, timestamp: i64, snapshot: &Snapshot) -> bool {
        if let Some(tombstone) = self.tombstones.get(&timestamp) {
            tombstone.is_visible(snapshot)
        } else {
            false
        }
    }

    /// Get a data point by timestamp with snapshot visibility.
    pub fn get(&mut self, timestamp: i64, snapshot: &Snapshot) -> Option<Vec<u8>> {
        self.touch();

        // Check if tombstoned first
        if self.is_tombstoned(timestamp, snapshot) {
            return None;
        }

        if let Some(chain) = self.points.get(&timestamp) {
            chain.find_visible_inline(snapshot).map(|v| v.to_vec())
        } else {
            None
        }
    }

    /// Get the number of points in this bucket.
    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Check if the bucket is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Check if the bucket is dirty (modified since last flush).
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Get an iterator over points in the specified time range with snapshot visibility.
    pub fn range<'a>(
        &'a self,
        start: i64,
        end: i64,
        snapshot: &'a Snapshot,
    ) -> impl Iterator<Item = (i64, Vec<u8>)> + 'a {
        self.points
            .range(start..end)
            .filter_map(move |(ts, chain)| {
                // Filter out tombstoned points
                if self.is_tombstoned(*ts, snapshot) {
                    return None;
                }
                chain
                    .find_visible_inline(snapshot)
                    .map(|v| (*ts, v.to_vec()))
            })
    }

    /// Get all points in the bucket with snapshot visibility.
    pub fn iter<'a>(&'a self, snapshot: &'a Snapshot) -> impl Iterator<Item = (i64, Vec<u8>)> + 'a {
        self.points.iter().filter_map(move |(ts, chain)| {
            // Filter out tombstoned points
            if self.is_tombstoned(*ts, snapshot) {
                return None;
            }
            chain
                .find_visible_inline(snapshot)
                .map(|v| (*ts, v.to_vec()))
        })
    }

    /// Find the latest point before or at the given timestamp with snapshot visibility.
    #[must_use]
    pub fn latest_before(&self, timestamp: i64, snapshot: &Snapshot) -> Option<(i64, Vec<u8>)> {
        self.points
            .range(..=timestamp)
            .rev()
            .find_map(|(ts, chain)| {
                // Filter out tombstoned points
                if self.is_tombstoned(*ts, snapshot) {
                    return None;
                }
                chain
                    .find_visible_inline(snapshot)
                    .map(|v| (*ts, v.to_vec()))
            })
    }

    /// Serialize the bucket to bytes.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Write bucket ID (8 bytes)
        bytes.extend_from_slice(&self.id.0.to_le_bytes());

        // Write start timestamp (8 bytes)
        bytes.extend_from_slice(&self.start_ts.to_le_bytes());

        // Write end timestamp (8 bytes)
        bytes.extend_from_slice(&self.end_ts.to_le_bytes());

        // Write last access time (8 bytes)
        bytes.extend_from_slice(&self.last_access_time.to_le_bytes());

        // Write number of points (4 bytes)
        bytes.extend_from_slice(&(self.points.len() as u32).to_le_bytes());

        // Write each point: timestamp (8 bytes) + chain_len (4 bytes) + serialized_chain
        for (timestamp, chain) in &self.points {
            bytes.extend_from_slice(&timestamp.to_le_bytes());

            // Serialize the version chain using postcard
            let chain_bytes = postcard::to_allocvec(chain).unwrap_or_default();
            bytes.extend_from_slice(&(chain_bytes.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&chain_bytes);
        }

        // Write number of tombstones (4 bytes)
        bytes.extend_from_slice(&(self.tombstones.len() as u32).to_le_bytes());

        // Write each tombstone: serialized_tombstone_len (4 bytes) + serialized_tombstone
        for tombstone in self.tombstones.values() {
            let tombstone_bytes = postcard::to_allocvec(tombstone).unwrap_or_default();
            bytes.extend_from_slice(&(tombstone_bytes.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&tombstone_bytes);
        }

        bytes
    }

    /// Deserialize a bucket from bytes.
    pub(crate) fn from_bytes(
        data: &[u8],
        _bucket_size: u64,
        pager: std::sync::Arc<Pager<FS>>,
    ) -> TableResult<Self> {
        let mut pos = 0;

        // Read bucket ID
        if data.len() < pos + 8 {
            return Err(crate::table::TableError::Other(
                "Insufficient data for bucket ID".to_string(),
            ));
        }
        let bucket_id = BucketId(u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap()));
        pos += 8;

        // Read start timestamp
        if data.len() < pos + 8 {
            return Err(crate::table::TableError::Other(
                "Insufficient data for start timestamp".to_string(),
            ));
        }
        let start_ts = i64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;

        // Read end timestamp
        if data.len() < pos + 8 {
            return Err(crate::table::TableError::Other(
                "Insufficient data for end timestamp".to_string(),
            ));
        }
        let end_ts = i64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;

        // Read last access time
        if data.len() < pos + 8 {
            return Err(crate::table::TableError::Other(
                "Insufficient data for last access time".to_string(),
            ));
        }
        let last_access_time = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;

        // Read number of points
        if data.len() < pos + 4 {
            return Err(crate::table::TableError::Other(
                "Insufficient data for point count".to_string(),
            ));
        }
        let point_count = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        pos += 4;

        // Read points
        let mut points = BTreeMap::new();
        for _ in 0..point_count {
            // Read timestamp
            if data.len() < pos + 8 {
                return Err(crate::table::TableError::Other(
                    "Insufficient data for point timestamp".to_string(),
                ));
            }
            let timestamp = i64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
            pos += 8;

            // Read chain length
            if data.len() < pos + 4 {
                return Err(crate::table::TableError::Other(
                    "Insufficient data for chain length".to_string(),
                ));
            }
            let chain_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;

            // Read and deserialize version chain
            if data.len() < pos + chain_len {
                return Err(crate::table::TableError::Other(
                    "Insufficient data for version chain".to_string(),
                ));
            }
            let chain: VersionChain =
                postcard::from_bytes(&data[pos..pos + chain_len]).map_err(|e| {
                    crate::table::TableError::Other(format!(
                        "Failed to deserialize version chain: {}",
                        e
                    ))
                })?;
            pos += chain_len;

            points.insert(timestamp, chain);
        }

        // Read tombstones (if present in newer format)
        let mut tombstones = BTreeMap::new();
        if pos < data.len() {
            // Read number of tombstones
            if data.len() >= pos + 4 {
                let tombstone_count = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
                pos += 4;

                // Read tombstones
                for _ in 0..tombstone_count {
                    // Read tombstone length
                    if data.len() < pos + 4 {
                        break; // Gracefully handle incomplete data
                    }
                    let tombstone_len =
                        u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
                    pos += 4;

                    // Read and deserialize tombstone
                    if data.len() < pos + tombstone_len {
                        break; // Gracefully handle incomplete data
                    }
                    if let Ok(tombstone) =
                        postcard::from_bytes::<TimeSeriesTombstone>(&data[pos..pos + tombstone_len])
                    {
                        tombstones.insert(tombstone.timestamp, tombstone);
                    }
                    pos += tombstone_len;
                }
            }
        }

        Ok(Self {
            id: bucket_id,
            start_ts,
            end_ts,
            points,
            tombstones,
            page_id: None,
            pager,
            dirty: false,
            last_access_time,
        })
    }

    /// Flush the bucket to disk.
    pub fn flush(&mut self) -> TableResult<()> {
        // Always flush to ensure we have a page ID, even for empty buckets
        // Serialize the bucket
        let bucket_data = self.to_bytes();

        // Allocate or reuse a page
        let page_id = if let Some(existing_page_id) = self.page_id {
            existing_page_id
        } else {
            // Allocate a new page
            let new_page_id = self.pager.allocate_page(PageType::BTreeLeaf).map_err(|e| {
                crate::table::TableError::Other(format!("Failed to allocate page: {}", e))
            })?;
            self.page_id = Some(new_page_id);
            new_page_id
        };

        // Create a page with the bucket data
        let page_size = self.pager.page_size();
        let mut page = Page::new(page_id, PageType::BTreeLeaf, page_size.data_size());
        page.data_mut().extend_from_slice(&bucket_data);

        // Write the page to disk
        self.pager
            .write_page(&page)
            .map_err(|e| crate::table::TableError::Other(format!("Failed to write page: {}", e)))?;

        self.dirty = false;
        Ok(())
    }

    /// Load a bucket from disk.
    pub fn load(
        page_id: PageId,
        bucket_size: u64,
        pager: std::sync::Arc<Pager<FS>>,
    ) -> TableResult<Self> {
        // Read the page from disk
        let page = pager
            .read_page(page_id)
            .map_err(|e| crate::table::TableError::Other(format!("Failed to read page: {}", e)))?;

        // Deserialize the bucket
        let mut bucket = Self::from_bytes(page.data(), bucket_size, pager)?;
        bucket.page_id = Some(page_id);
        bucket.touch();

        Ok(bucket)
    }

    /// Get the page ID where this bucket is stored.
    #[must_use]
    pub fn page_id(&self) -> Option<PageId> {
        self.page_id
    }

    /// Set the page ID where this bucket is stored.
    #[must_use]
    pub fn set_page_id(&mut self, page_id: PageId) {
        self.page_id = Some(page_id);
    }

    /// Get the last access time.
    #[must_use]
    pub fn last_access_time(&self) -> u64 {
        self.last_access_time
    }

    /// Get the estimated size in bytes.
    #[must_use]
    pub fn estimated_size(&self) -> usize {
        let mut size = 32; // Fixed overhead (id, timestamps, metadata)
        for (_, chain) in &self.points {
            size += 8; // timestamp
            let mut current = Some(chain);
            while let Some(version) = current {
                size += 4 + version.value.len(); // length + value
                size += std::mem::size_of::<VersionChain>();
                current = version.prev_version.as_deref();
            }
        }
        size
    }

    /// Commit all uncommitted versions created by the given transaction.
    pub fn commit_versions(&mut self, tx_id: TransactionId, commit_lsn: LogSequenceNumber) {
        for chain in self.points.values_mut() {
            Self::commit_chain_recursive(chain, tx_id, commit_lsn);
        }
        self.dirty = true;
    }

    /// Recursively commit versions in a chain.
    fn commit_chain_recursive(
        chain: &mut VersionChain,
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) {
        if chain.created_by == tx_id && chain.commit_lsn.is_none() {
            chain.commit(commit_lsn);
        }
        if let Some(prev) = &mut chain.prev_version {
            Self::commit_chain_recursive(prev, tx_id, commit_lsn);
        }
    }

    /// Vacuum old versions that are no longer visible.
    pub fn vacuum(&mut self, min_visible_lsn: LogSequenceNumber) -> usize {
        let mut removed = 0;

        // Vacuum version chains
        for chain in self.points.values_mut() {
            let (count, _freed_refs) = chain.vacuum(min_visible_lsn);
            removed += count;
            // TODO: Free overflow pages for external values in _freed_refs
        }

        // Remove tombstones that are no longer needed
        // (tombstones older than min_visible_lsn can be safely removed)
        let tombstones_to_remove: Vec<i64> = self
            .tombstones
            .iter()
            .filter(|(_, tombstone)| {
                if let Some(lsn) = tombstone.tombstone_lsn {
                    lsn < min_visible_lsn
                } else {
                    false
                }
            })
            .map(|(ts, _)| *ts)
            .collect();

        for ts in tombstones_to_remove {
            self.tombstones.remove(&ts);
            removed += 1;
        }

        if removed > 0 {
            self.dirty = true;
        }
        removed
    }
}

/// Manager for time buckets.
pub struct BucketManager<FS: FileSystem> {
    /// Bucket size in seconds
    bucket_size: u64,

    /// Active buckets in memory
    pub(crate) buckets: BTreeMap<BucketId, TimeBucket<FS>>,

    /// Pager for persistent storage
    pager: std::sync::Arc<Pager<FS>>,

    /// Maximum number of buckets to keep in memory
    max_buckets_in_memory: usize,

    /// Mapping of bucket IDs to their page IDs (for lazy loading)
    bucket_page_map: BTreeMap<BucketId, PageId>,
}

impl<FS: FileSystem> BucketManager<FS> {
    /// Create a new bucket manager.
    #[must_use]
    pub fn new(
        bucket_size: u64,
        pager: std::sync::Arc<Pager<FS>>,
        max_buckets_in_memory: usize,
    ) -> Self {
        Self {
            bucket_size,
            buckets: BTreeMap::new(),
            pager,
            max_buckets_in_memory,
            bucket_page_map: BTreeMap::new(),
        }
    }

    /// Get or create a bucket for the given timestamp.
    pub fn get_or_create_bucket(&mut self, timestamp: i64) -> TableResult<&mut TimeBucket<FS>> {
        let bucket_id = BucketId::from_timestamp(timestamp, self.bucket_size);

        // Check if bucket is already in memory
        if self.buckets.contains_key(&bucket_id) {
            let bucket = self.buckets.get_mut(&bucket_id).unwrap();
            bucket.touch();
            return Ok(bucket);
        }

        // Check if we need to evict old buckets before loading/creating
        if self.buckets.len() >= self.max_buckets_in_memory {
            self.evict_lru_bucket()?;
        }

        // Try to load from disk if we have a page ID for this bucket
        if let Some(&page_id) = self.bucket_page_map.get(&bucket_id) {
            let bucket = TimeBucket::load(page_id, self.bucket_size, self.pager.clone())?;
            self.buckets.insert(bucket_id, bucket);
        } else {
            // Create a new bucket
            let bucket = TimeBucket::new(bucket_id, self.bucket_size, self.pager.clone());
            self.buckets.insert(bucket_id, bucket);
        }

        Ok(self.buckets.get_mut(&bucket_id).unwrap())
    }

    /// Get a bucket by ID.
    #[must_use]
    pub fn get_bucket(&self, bucket_id: BucketId) -> Option<&TimeBucket<FS>> {
        self.buckets.get(&bucket_id)
    }

    /// Get a mutable bucket by ID.
    pub fn get_bucket_mut(&mut self, bucket_id: BucketId) -> Option<&mut TimeBucket<FS>> {
        self.buckets.get_mut(&bucket_id)
    }

    /// Get all bucket IDs that overlap with the given time range.
    #[must_use]
    pub fn get_bucket_ids_in_range(&self, start_ts: i64, end_ts: i64) -> Vec<BucketId> {
        let start_bucket = BucketId::from_timestamp(start_ts, self.bucket_size);
        let end_bucket = BucketId::from_timestamp(end_ts, self.bucket_size);

        self.buckets
            .range(start_bucket..=end_bucket)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Evict the least recently used bucket from memory.
    fn evict_lru_bucket(&mut self) -> TableResult<()> {
        // Find the bucket with the oldest access time
        let lru_bucket_id = self
            .buckets
            .iter()
            .min_by_key(|(_, bucket)| bucket.last_access_time())
            .map(|(id, _)| *id);

        if let Some(bucket_id) = lru_bucket_id
            && let Some(mut bucket) = self.buckets.remove(&bucket_id)
        {
            // Flush if dirty
            if bucket.is_dirty() {
                bucket.flush()?;
            }

            // Store the page ID mapping for lazy loading
            if let Some(page_id) = bucket.page_id() {
                self.bucket_page_map.insert(bucket_id, page_id);
            }
        }
        Ok(())
    }

    /// Flush all dirty buckets to disk.
    pub fn flush_all(&mut self) -> TableResult<()> {
        for (bucket_id, bucket) in self.buckets.iter_mut() {
            if bucket.is_dirty() {
                bucket.flush()?;
                // Update page mapping
                if let Some(page_id) = bucket.page_id() {
                    self.bucket_page_map.insert(*bucket_id, page_id);
                }
            }
        }
        Ok(())
    }

    /// Get the number of buckets in memory.
    #[must_use]
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    /// Get the total number of tracked buckets (in memory + on disk).
    #[must_use]
    pub fn total_bucket_count(&self) -> usize {
        self.bucket_page_map.len()
    }

    /// Register a bucket's page ID for lazy loading.
    pub fn register_bucket_page(&mut self, bucket_id: BucketId, page_id: PageId) {
        self.bucket_page_map.insert(bucket_id, page_id);
    }

    /// Get the page ID for a bucket if it exists.
    #[must_use]
    pub fn get_bucket_page_id(&self, bucket_id: BucketId) -> Option<PageId> {
        self.bucket_page_map.get(&bucket_id).copied()
    }

    /// Get all bucket page mappings for persistence.
    #[must_use]
    pub fn get_bucket_page_mappings(&self) -> Vec<(u64, PageId)> {
        self.bucket_page_map
            .iter()
            .map(|(bucket_id, page_id)| (bucket_id.0, *page_id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_id_from_timestamp() {
        let bucket_size = 3600; // 1 hour

        // Test positive timestamps
        let id1 = BucketId::from_timestamp(0, bucket_size);
        assert_eq!(id1.0, 0);

        let id2 = BucketId::from_timestamp(3600, bucket_size);
        assert_eq!(id2.0, 1);

        let id3 = BucketId::from_timestamp(7200, bucket_size);
        assert_eq!(id3.0, 2);

        // Test timestamp within bucket
        let id4 = BucketId::from_timestamp(1800, bucket_size);
        assert_eq!(id4.0, 0);
    }

    #[test]
    fn test_bucket_timestamps() {
        let bucket_size = 3600;
        let id = BucketId(5);

        let start = id.start_timestamp(bucket_size);
        let end = id.end_timestamp(bucket_size);

        assert_eq!(start, 18000);
        assert_eq!(end, 21600);
    }

    #[test]
    fn test_bucket_contains_timestamp() {
        let bucket_size = 3600;
        let fs = crate::vfs::MemoryFileSystem::new();
        let pager = std::sync::Arc::new(
            Pager::create(&fs, "test.db", crate::pager::PagerConfig::default()).unwrap(),
        );

        let bucket = TimeBucket::new(BucketId(0), bucket_size, pager);

        assert!(bucket.contains_timestamp(0));
        assert!(bucket.contains_timestamp(1800));
        assert!(bucket.contains_timestamp(3599));
        assert!(!bucket.contains_timestamp(3600));
        assert!(!bucket.contains_timestamp(-1));
    }
}

// Made with Bob
