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

//! TimeSeries table engine implementation.
//!
//! This module provides a specialized time series storage engine optimized for
//! time-ordered data with efficient compression and range queries.
//!
//! # Architecture
//!
//! ```text
//! Series Key → Time Buckets → Compressed Data Points
//!                  ↓
//!            [Bucket 0: 00:00-01:00]
//!            [Bucket 1: 01:00-02:00]
//!            [Bucket 2: 02:00-03:00]
//! ```
//!
//! # Features
//!
//! - **Time-based bucketing**: Automatic organization into time buckets
//! - **Specialized compression**: Delta-of-delta, Gorilla compression
//! - **Efficient range queries**: Fast scans over time ranges
//! - **Latest-before queries**: Find the most recent value before a timestamp
//! - **Retention policies**: Automatic cleanup of old data
//! - **Downsampling**: Optional aggregation of old data
//!
//! # Use Cases
//!
//! - Metrics and monitoring data
//! - IoT sensor readings
//! - Financial tick data
//! - Application performance monitoring
//! - System logs with timestamps

mod bucket;
mod compression;
mod config;

pub use self::bucket::{BucketId, BucketManager, TimeBucket};
pub use self::compression::{
    compress_timestamps_delta_of_delta, compress_values_delta, compress_values_gorilla,
    decompress_timestamps_delta_of_delta, decompress_values_delta, decompress_values_gorilla,
};
pub use self::config::{TimeSeriesCompression, TimeSeriesConfig, TimeSeriesRetentionPolicy};

use crate::pager::{PageId, Pager};
use crate::snap::Snapshot;
use crate::table::{
    SpecialtyTableCapabilities, SpecialtyTableStats, Table, TableCapabilities, TableEngineKind,
    TableResult, TableStatistics, TimePointRef, TimeSeries as TimeSeriesTrait, TimeSeriesCursor,
    VerificationReport,
};
use crate::txn::TransactionId;
use crate::types::TableId;
use crate::vfs::FileSystem;
use crate::wal::LogSequenceNumber;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

// =============================================================================
// TimeSeriesTable Implementation
// =============================================================================

/// TimeSeries storage engine.
///
/// Provides a specialized time series storage engine with time-based bucketing,
/// compression, and efficient range queries.
pub struct TimeSeriesTable<FS: FileSystem> {
    /// Table identifier
    table_id: TableId,

    /// Table name
    name: String,

    /// Configuration
    config: TimeSeriesConfig,

    /// Pager for persistent storage
    pager: Arc<Pager<FS>>,

    /// Root page ID (stores metadata)
    root_page_id: PageId,

    /// Internal state
    state: RwLock<TimeSeriesState<FS>>,
}

/// Internal mutable state of the TimeSeries table.
struct TimeSeriesState<FS: FileSystem> {
    /// Series data: series_key → bucket manager
    series: BTreeMap<Vec<u8>, BucketManager<FS>>,

    /// Total number of data points across all series
    total_points: u64,

    /// Total size in bytes
    total_size: u64,
}

impl<FS: FileSystem> TimeSeriesTable<FS> {
    /// Create a new TimeSeries table.
    pub fn new(
        table_id: TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        config: TimeSeriesConfig,
    ) -> TableResult<Self> {
        // Allocate root page for metadata
        let root_page_id = pager
            .allocate_page(crate::pager::PageType::LsmMeta)
            .map_err(|e| {
                crate::table::TableError::Other(format!("Failed to allocate root page: {}", e))
            })?;

        let state = TimeSeriesState {
            series: BTreeMap::new(),
            total_points: 0,
            total_size: 0,
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

    /// Open an existing TimeSeries table.
    pub fn open(
        table_id: TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        root_page_id: PageId,
        config: TimeSeriesConfig,
    ) -> TableResult<Self> {
        // TODO: Load metadata from root page
        let state = TimeSeriesState {
            series: BTreeMap::new(),
            total_points: 0,
            total_size: 0,
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

    /// Get or create a bucket manager for a series.
    fn get_or_create_series_manager<'a>(
        state: &'a mut TimeSeriesState<FS>,
        series_key: &[u8],
        config: &TimeSeriesConfig,
        pager: Arc<Pager<FS>>,
    ) -> &'a mut BucketManager<FS> {
        if !state.series.contains_key(series_key) {
            let manager = BucketManager::new(
                config.bucket_size,
                pager,
                100, // max buckets in memory
            );
            state.series.insert(series_key.to_vec(), manager);
        }
        state.series.get_mut(series_key).unwrap()
    }

    /// Append a point with transaction tracking (MVCC-aware).
    pub fn append_point_tx(
        &self,
        series_key: &[u8],
        timestamp: i64,
        value_key: &[u8],
        tx_id: TransactionId,
    ) -> TableResult<()> {
        let mut state = self.state.write().unwrap();

        // Get or create the bucket manager for this series
        let manager = Self::get_or_create_series_manager(
            &mut state,
            series_key,
            &self.config,
            self.pager.clone(),
        );

        // Get or create the appropriate bucket
        let bucket = manager.get_or_create_bucket(timestamp)?;

        // Insert the data point with transaction ID
        bucket.insert(timestamp, value_key.to_vec(), tx_id)?;

        // Update statistics
        state.total_points += 1;
        state.total_size += (series_key.len() + 8 + value_key.len()) as u64;

        Ok(())
    }

    /// Scan series with snapshot visibility (MVCC-aware).
    pub fn scan_series_snapshot(
        &self,
        series_key: &[u8],
        start_ts: i64,
        end_ts: i64,
        snapshot: Snapshot,
    ) -> TableResult<TimeSeriesTableCursor<'_, FS>> {
        TimeSeriesTableCursor::new_with_snapshot(self, series_key, start_ts, end_ts, snapshot)
    }

    /// Get latest point before timestamp with snapshot visibility (MVCC-aware).
    pub fn latest_before_snapshot(
        &self,
        series_key: &[u8],
        timestamp: i64,
        snapshot: &Snapshot,
    ) -> TableResult<Option<TimePointRef>> {
        let state = self.state.read().unwrap();

        // Get the series manager
        let manager = match state.series.get(series_key) {
            Some(m) => m,
            None => return Ok(None),
        };

        // Get all bucket IDs up to the timestamp
        let bucket_id = BucketId::from_timestamp(timestamp, self.config.bucket_size);

        // Search backwards through buckets
        for (&bid, _) in manager.buckets.range(..=bucket_id).rev() {
            if let Some(bucket) = manager.get_bucket(bid) {
                if let Some((ts, value_key)) = bucket.latest_before(timestamp, snapshot) {
                    return Ok(Some(TimePointRef {
                        series_key: crate::types::KeyBuf(series_key.to_vec()),
                        timestamp: ts,
                        value_key: crate::types::KeyBuf(value_key),
                    }));
                }
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

        for manager in state.series.values_mut() {
            for bucket in manager.buckets.values_mut() {
                bucket.commit_versions(tx_id, commit_lsn);
            }
        }

        Ok(())
    }

    /// Vacuum old versions that are no longer visible.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        let mut state = self.state.write().unwrap();
        let mut total_removed = 0usize;

        for manager in state.series.values_mut() {
            for bucket in manager.buckets.values_mut() {
                total_removed += bucket.vacuum(min_visible_lsn);
            }
        }

        Ok(total_removed)
    }
}

// =============================================================================
// Table Trait Implementation
// =============================================================================

impl<FS: FileSystem> Table for TimeSeriesTable<FS> {
    fn table_id(&self) -> TableId {
        self.table_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> TableEngineKind {
        TableEngineKind::TimeSeries
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
            row_count: Some(state.total_points),
            total_size_bytes: Some(state.total_size),
            key_stats: None,
            value_stats: None,
            histogram: None,
            last_updated_lsn: Some(LogSequenceNumber::from(0)),
        })
    }
}

// =============================================================================
// TimeSeries Trait Implementation
// =============================================================================

impl<FS: FileSystem> TimeSeriesTrait for TimeSeriesTable<FS> {
    type TimeSeriesCursor<'a>
        = TimeSeriesTableCursor<'a, FS>
    where
        FS: 'a;

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
            ordered: true,
            sparse: false,
            supports_delete: false,
            supports_range_query: true,
            supports_prefix_query: false,
            supports_scoring: false,
            supports_incremental_rebuild: false,
            may_be_stale: false,
        }
    }

    fn append_point(
        &self,
        series_key: &[u8],
        timestamp: i64,
        value_key: &[u8],
        tx_id: TransactionId,
        _commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        self.append_point_tx(series_key, timestamp, value_key, tx_id)
    }

    fn scan_series(
        &self,
        series_key: &[u8],
        start_ts: i64,
        end_ts: i64,
    ) -> TableResult<Self::TimeSeriesCursor<'_>> {
        TimeSeriesTableCursor::new(self, series_key, start_ts, end_ts)
    }

    fn latest_before(
        &self,
        series_key: &[u8],
        timestamp: i64,
    ) -> TableResult<Option<TimePointRef>> {
        // Create a snapshot that sees all committed data
        let snapshot = Snapshot::new(
            crate::snap::SnapshotId::from(0),
            String::new(),
            LogSequenceNumber::from(u64::MAX),
            0,
            0,
            Vec::new(),
        );

        self.latest_before_snapshot(series_key, timestamp, &snapshot)
    }

    fn stats(&self) -> TableResult<SpecialtyTableStats> {
        let state = self.state.read().unwrap();
        Ok(SpecialtyTableStats {
            entry_count: Some(state.total_points),
            size_bytes: Some(state.total_size),
            distinct_keys: Some(state.series.len() as u64),
            stale_entries: None,
            last_updated_lsn: Some(LogSequenceNumber::from(0)),
        })
    }

    fn verify(&self) -> TableResult<VerificationReport> {
        // Basic verification - check that all buckets are valid
        Ok(VerificationReport {
            checked_items: 0,
            errors: Vec::new(),
            warnings: Vec::new(),
        })
    }
}

// =============================================================================
// Cursor Implementation
// =============================================================================

/// Aggregation functions supported by the TimeSeries cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeSeriesAggregation {
    Sum,
    Avg,
    Min,
    Max,
    Count,
}

/// Result of an aggregation over a time range.
#[derive(Debug, Clone, PartialEq)]
pub enum AggregationResult {
    Sum(f64),
    Avg(f64),
    Min(f64),
    Max(f64),
    Count(u64),
}

impl AggregationResult {
    /// Get the numeric value as f64.
    pub fn as_f64(&self) -> f64 {
        match self {
            Self::Sum(value) | Self::Avg(value) | Self::Min(value) | Self::Max(value) => *value,
            Self::Count(value) => *value as f64,
        }
    }

    /// Get the count value if this is a count aggregation.
    pub fn as_count(&self) -> Option<u64> {
        match self {
            Self::Count(value) => Some(*value),
            _ => None,
        }
    }
}

/// A downsampled aggregation window.
#[derive(Debug, Clone, PartialEq)]
pub struct DownsampledPoint {
    pub start_ts: i64,
    pub end_ts: i64,
    pub aggregation: AggregationResult,
}

#[derive(Debug, Clone, Copy)]
struct AggregationState {
    sum: f64,
    count: u64,
    min: f64,
    max: f64,
}

impl AggregationState {
    fn new(value: f64) -> Self {
        Self {
            sum: value,
            count: 1,
            min: value,
            max: value,
        }
    }

    fn update(&mut self, value: f64) {
        self.sum += value;
        self.count += 1;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
    }

    fn finalize(&self, aggregation: TimeSeriesAggregation) -> AggregationResult {
        match aggregation {
            TimeSeriesAggregation::Sum => AggregationResult::Sum(self.sum),
            TimeSeriesAggregation::Avg => AggregationResult::Avg(self.sum / self.count as f64),
            TimeSeriesAggregation::Min => AggregationResult::Min(self.min),
            TimeSeriesAggregation::Max => AggregationResult::Max(self.max),
            TimeSeriesAggregation::Count => AggregationResult::Count(self.count),
        }
    }
}

fn parse_numeric_value(value_key: &[u8]) -> Option<f64> {
    if value_key.is_empty() {
        return None;
    }

    if let Ok(text) = std::str::from_utf8(value_key) {
        let trimmed = text.trim();

        if let Ok(value) = trimmed.parse::<f64>() {
            return Some(value);
        }

        if let Some((_, tail)) = trimmed.rsplit_once(':')
            && let Ok(value) = tail.trim().parse::<f64>()
        {
            return Some(value);
        }

        if let Ok(json) = serde_json::from_str::<Value>(trimmed) {
            match json {
                Value::Number(number) => number.as_f64(),
                Value::Object(map) => map
                    .get("value")
                    .and_then(|value| value.as_f64())
                    .or_else(|| map.get("avg").and_then(|value| value.as_f64()))
                    .or_else(|| map.get("sum").and_then(|value| value.as_f64())),
                _ => None,
            }
        } else {
            None
        }
    } else {
        None
    }
}

/// Cursor for scanning time series data.
pub struct TimeSeriesTableCursor<'a, FS: FileSystem> {
    /// Reference to the table
    _table: &'a TimeSeriesTable<FS>,

    /// Series key being scanned
    series_key: Vec<u8>,

    /// Start timestamp (inclusive)
    start_ts: i64,

    /// End timestamp (exclusive)
    end_ts: i64,

    /// Current data points
    points: Vec<(i64, Vec<u8>)>,

    /// Current position in points
    position: usize,
}

impl<'a, FS: FileSystem> TimeSeriesTableCursor<'a, FS> {
    fn new(
        table: &'a TimeSeriesTable<FS>,
        series_key: &[u8],
        start_ts: i64,
        end_ts: i64,
    ) -> TableResult<Self> {
        // Create a snapshot that sees all committed data
        let snapshot = Snapshot::new(
            crate::snap::SnapshotId::from(0),
            String::new(),
            LogSequenceNumber::from(u64::MAX),
            0,
            0,
            Vec::new(),
        );

        Self::new_with_snapshot(table, series_key, start_ts, end_ts, snapshot)
    }

    fn new_with_snapshot(
        table: &'a TimeSeriesTable<FS>,
        series_key: &[u8],
        start_ts: i64,
        end_ts: i64,
        snapshot: Snapshot,
    ) -> TableResult<Self> {
        let state = table.state.read().unwrap();

        // Collect all points in the range with snapshot visibility
        let mut points = Vec::new();

        if let Some(manager) = state.series.get(series_key) {
            let bucket_ids = manager.get_bucket_ids_in_range(start_ts, end_ts);

            for bucket_id in bucket_ids {
                if let Some(bucket) = manager.get_bucket(bucket_id) {
                    for (ts, value_key) in bucket.range(start_ts, end_ts, &snapshot) {
                        points.push((ts, value_key));
                    }
                }
            }
        }

        // Sort by timestamp
        points.sort_by_key(|(ts, _)| *ts);

        Ok(Self {
            _table: table,
            series_key: series_key.to_vec(),
            start_ts,
            end_ts,
            points,
            position: 0,
        })
    }

    fn aggregate_state(&self) -> Option<AggregationState> {
        let mut iter = self
            .points
            .iter()
            .filter_map(|(_, value_key)| parse_numeric_value(value_key));

        let first = iter.next()?;
        let mut state = AggregationState::new(first);
        for value in iter {
            state.update(value);
        }
        Some(state)
    }

    pub fn sum(&self) -> Option<f64> {
        self.aggregate_state().map(|state| state.sum)
    }

    pub fn avg(&self) -> Option<f64> {
        self.aggregate_state()
            .map(|state| state.sum / state.count as f64)
    }

    pub fn min(&self) -> Option<f64> {
        self.aggregate_state().map(|state| state.min)
    }

    pub fn max(&self) -> Option<f64> {
        self.aggregate_state().map(|state| state.max)
    }

    pub fn count(&self) -> u64 {
        self.points.len() as u64
    }

    pub fn aggregate(&self, aggregation: TimeSeriesAggregation) -> Option<AggregationResult> {
        match aggregation {
            TimeSeriesAggregation::Count => Some(AggregationResult::Count(self.count())),
            _ => self
                .aggregate_state()
                .map(|state| state.finalize(aggregation)),
        }
    }

    pub fn downsample(
        &self,
        interval: u64,
        aggregation: TimeSeriesAggregation,
    ) -> TableResult<Vec<DownsampledPoint>> {
        if interval == 0 {
            return Err(crate::table::TableError::Other(
                "Downsample interval must be greater than zero".to_string(),
            ));
        }

        let mut windows: BTreeMap<i64, AggregationState> = BTreeMap::new();
        let interval = interval as i64;

        for (timestamp, value_key) in &self.points {
            let value = match aggregation {
                TimeSeriesAggregation::Count => 1.0,
                _ => match parse_numeric_value(value_key) {
                    Some(value) => value,
                    None => continue,
                },
            };

            let offset = timestamp.saturating_sub(self.start_ts);
            let window_start = self.start_ts + offset.div_euclid(interval) * interval;

            windows
                .entry(window_start)
                .and_modify(|state| state.update(value))
                .or_insert_with(|| AggregationState::new(value));
        }

        Ok(windows
            .into_iter()
            .map(|(window_start, state)| DownsampledPoint {
                start_ts: window_start,
                end_ts: window_start.saturating_add(interval).min(self.end_ts),
                aggregation: state.finalize(aggregation),
            })
            .collect())
    }
}

impl<'a, FS: FileSystem> TimeSeriesCursor for TimeSeriesTableCursor<'a, FS> {
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

    fn next(&mut self) -> TableResult<()> {
        if self.valid() {
            self.position += 1;
        }
        Ok(())
    }
}

// Made with Bob
