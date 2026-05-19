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

//! Table engine metrics and observability.
//!
//! This module provides comprehensive metrics instrumentation for all table engines.
//! It defines common metrics that all engines should track, as well as engine-specific
//! metrics for specialized operations.
//!
//! # Metric Naming Convention
//!
//! All metrics follow the pattern: `nanokv.table.<engine>.<metric_name>`
//!
//! Common metrics (all engines):
//! - `nanokv.table.<engine>.get` - Point lookup operations
//! - `nanokv.table.<engine>.put` - Write operations
//! - `nanokv.table.<engine>.delete` - Delete operations
//! - `nanokv.table.<engine>.scan` - Scan operations
//! - `nanokv.table.<engine>.get_duration` - Get latency histogram
//! - `nanokv.table.<engine>.put_duration` - Put latency histogram
//! - `nanokv.table.<engine>.delete_duration` - Delete latency histogram
//! - `nanokv.table.<engine>.scan_duration` - Scan latency histogram
//!
//! Engine-specific metrics are documented in their respective sections below.

use metrics::{counter, gauge, histogram};
use std::time::Instant;

/// Helper macro to record operation latency.
///
/// Usage: `record_latency!("nanokv.table.btree.get_duration", start_time);`
#[macro_export]
macro_rules! record_latency {
    ($metric:expr, $start:expr) => {
        histogram!($metric).record($start.elapsed().as_secs_f64());
    };
}

// =============================================================================
// Common Table Metrics
// =============================================================================

/// Record a get operation for any table engine.
#[inline]
pub fn record_get(engine: &str) {
    counter!(format!("nanokv.table.{}.get", engine)).increment(1);
}

/// Record a put operation for any table engine.
#[inline]
pub fn record_put(engine: &str) {
    counter!(format!("nanokv.table.{}.put", engine)).increment(1);
}

/// Record a delete operation for any table engine.
#[inline]
pub fn record_delete(engine: &str) {
    counter!(format!("nanokv.table.{}.delete", engine)).increment(1);
}

/// Record a scan operation for any table engine.
#[inline]
pub fn record_scan(engine: &str) {
    counter!(format!("nanokv.table.{}.scan", engine)).increment(1);
}

/// Record get operation latency.
#[inline]
pub fn record_get_duration(engine: &str, start: Instant) {
    histogram!(format!("nanokv.table.{}.get_duration", engine))
        .record(start.elapsed().as_secs_f64());
}

/// Record put operation latency.
#[inline]
pub fn record_put_duration(engine: &str, start: Instant) {
    histogram!(format!("nanokv.table.{}.put_duration", engine))
        .record(start.elapsed().as_secs_f64());
}

/// Record delete operation latency.
#[inline]
pub fn record_delete_duration(engine: &str, start: Instant) {
    histogram!(format!("nanokv.table.{}.delete_duration", engine))
        .record(start.elapsed().as_secs_f64());
}

/// Record scan operation latency.
#[inline]
pub fn record_scan_duration(engine: &str, start: Instant) {
    histogram!(format!("nanokv.table.{}.scan_duration", engine))
        .record(start.elapsed().as_secs_f64());
}

// =============================================================================
// BTree Metrics
// =============================================================================

/// BTree-specific metrics for node operations and tree structure.
pub mod btree {
    use super::*;

    const ENGINE: &str = "btree";

    /// Record a node split operation.
    #[inline]
    pub fn record_split() {
        counter!("nanokv.table.btree.split").increment(1);
    }

    /// Record a node merge operation.
    #[inline]
    pub fn record_merge() {
        counter!("nanokv.table.btree.merge").increment(1);
    }

    /// Record a node read operation.
    #[inline]
    pub fn record_node_read() {
        counter!("nanokv.table.btree.node_read").increment(1);
    }

    /// Record a node write operation.
    #[inline]
    pub fn record_node_write() {
        counter!("nanokv.table.btree.node_write").increment(1);
    }

    /// Update the current tree height.
    #[inline]
    pub fn set_tree_height(height: usize) {
        gauge!("nanokv.table.btree.tree_height").set(height as f64);
    }

    /// Update the number of internal nodes.
    #[inline]
    pub fn set_internal_nodes(count: u64) {
        gauge!("nanokv.table.btree.internal_nodes").set(count as f64);
    }

    /// Update the number of leaf nodes.
    #[inline]
    pub fn set_leaf_nodes(count: u64) {
        gauge!("nanokv.table.btree.leaf_nodes").set(count as f64);
    }

    /// Record node read latency.
    #[inline]
    pub fn record_node_read_duration(start: Instant) {
        histogram!("nanokv.table.btree.node_read_duration").record(start.elapsed().as_secs_f64());
    }

    /// Record node write latency.
    #[inline]
    pub fn record_node_write_duration(start: Instant) {
        histogram!("nanokv.table.btree.node_write_duration").record(start.elapsed().as_secs_f64());
    }

    /// Record search operation latency.
    #[inline]
    pub fn record_search_duration(start: Instant) {
        histogram!("nanokv.table.btree.search_duration").record(start.elapsed().as_secs_f64());
    }
}

// =============================================================================
// LSM Tree Metrics
// =============================================================================

/// LSM tree-specific metrics for memtable, SSTable, and compaction operations.
pub mod lsm {
    use super::*;

    const ENGINE: &str = "lsm";

    /// Record a memtable write operation.
    #[inline]
    pub fn record_memtable_write() {
        counter!("nanokv.table.lsm.memtable_write").increment(1);
    }

    /// Record a memtable flush operation.
    #[inline]
    pub fn record_memtable_flush() {
        counter!("nanokv.table.lsm.memtable_flush").increment(1);
    }

    /// Record an SSTable read operation.
    #[inline]
    pub fn record_sstable_read() {
        counter!("nanokv.table.lsm.sstable_read").increment(1);
    }

    /// Record a compaction operation.
    #[inline]
    pub fn record_compaction() {
        counter!("nanokv.table.lsm.compaction").increment(1);
    }

    /// Record bytes written during compaction.
    #[inline]
    pub fn record_compaction_bytes_written(bytes: u64) {
        counter!("nanokv.table.lsm.compaction_bytes_written").increment(bytes);
    }

    /// Record bytes read during compaction.
    #[inline]
    pub fn record_compaction_bytes_read(bytes: u64) {
        counter!("nanokv.table.lsm.compaction_bytes_read").increment(bytes);
    }

    /// Update the current number of SSTables.
    #[inline]
    pub fn set_sstable_count(count: usize) {
        gauge!("nanokv.table.lsm.sstable_count").set(count as f64);
    }

    /// Update the current memtable size in bytes.
    #[inline]
    pub fn set_memtable_size(bytes: usize) {
        gauge!("nanokv.table.lsm.memtable_size_bytes").set(bytes as f64);
    }

    /// Update the read amplification factor.
    #[inline]
    pub fn set_read_amplification(factor: f64) {
        gauge!("nanokv.table.lsm.read_amplification").set(factor);
    }

    /// Update the write amplification factor.
    #[inline]
    pub fn set_write_amplification(factor: f64) {
        gauge!("nanokv.table.lsm.write_amplification").set(factor);
    }

    /// Record memtable flush latency.
    #[inline]
    pub fn record_flush_duration(start: Instant) {
        histogram!("nanokv.table.lsm.flush_duration").record(start.elapsed().as_secs_f64());
    }

    /// Record compaction latency.
    #[inline]
    pub fn record_compaction_duration(start: Instant) {
        histogram!("nanokv.table.lsm.compaction_duration").record(start.elapsed().as_secs_f64());
    }
}

// =============================================================================
// Bloom Filter Metrics
// =============================================================================

/// Bloom filter-specific metrics for insert, query, and false positive tracking.
pub mod bloom {
    use super::*;

    const ENGINE: &str = "bloom";

    /// Record a bloom filter insert operation.
    #[inline]
    pub fn record_insert() {
        counter!("nanokv.table.bloom.insert").increment(1);
    }

    /// Record a bloom filter query operation.
    #[inline]
    pub fn record_query() {
        counter!("nanokv.table.bloom.query").increment(1);
    }

    /// Record a positive query result (may contain).
    #[inline]
    pub fn record_positive() {
        counter!("nanokv.table.bloom.positive").increment(1);
    }

    /// Record a negative query result (definitely not present).
    #[inline]
    pub fn record_negative() {
        counter!("nanokv.table.bloom.negative").increment(1);
    }

    /// Record a false positive (bloom said yes, but key not found).
    #[inline]
    pub fn record_false_positive() {
        counter!("nanokv.table.bloom.false_positive").increment(1);
    }

    /// Update the current saturation level (0.0 to 1.0).
    #[inline]
    pub fn set_saturation(saturation: f64) {
        gauge!("nanokv.table.bloom.saturation").set(saturation);
    }

    /// Update the estimated false positive rate.
    #[inline]
    pub fn set_false_positive_rate(rate: f64) {
        gauge!("nanokv.table.bloom.false_positive_rate").set(rate);
    }

    /// Update the number of bits set in the filter.
    #[inline]
    pub fn set_bits_set(count: usize) {
        gauge!("nanokv.table.bloom.bits_set").set(count as f64);
    }

    /// Update the total number of bits in the filter.
    #[inline]
    pub fn set_total_bits(count: usize) {
        gauge!("nanokv.table.bloom.total_bits").set(count as f64);
    }
}

// =============================================================================
// RTree Metrics
// =============================================================================

/// RTree-specific metrics for spatial operations and tree structure.
pub mod rtree {
    use super::*;

    const ENGINE: &str = "rtree";

    /// Record a node split operation.
    #[inline]
    pub fn record_split() {
        counter!("nanokv.table.rtree.split").increment(1);
    }

    /// Record a spatial query operation.
    #[inline]
    pub fn record_query() {
        counter!("nanokv.table.rtree.query").increment(1);
    }

    /// Record the number of candidate nodes examined during a query.
    #[inline]
    pub fn record_query_candidates(count: usize) {
        histogram!("nanokv.table.rtree.query_candidates").record(count as f64);
    }

    /// Record the number of results returned from a query.
    #[inline]
    pub fn record_query_results(count: usize) {
        histogram!("nanokv.table.rtree.query_results").record(count as f64);
    }

    /// Update the current tree height.
    #[inline]
    pub fn set_tree_height(height: u32) {
        gauge!("nanokv.table.rtree.tree_height").set(height as f64);
    }

    /// Update the number of objects indexed.
    #[inline]
    pub fn set_object_count(count: usize) {
        gauge!("nanokv.table.rtree.object_count").set(count as f64);
    }

    /// Record spatial query latency.
    #[inline]
    pub fn record_query_duration(start: Instant) {
        histogram!("nanokv.table.rtree.query_duration").record(start.elapsed().as_secs_f64());
    }
}

// =============================================================================
// TimeSeries Metrics
// =============================================================================

/// TimeSeries-specific metrics for append, bucket, and aggregation operations.
pub mod timeseries {
    use super::*;

    const ENGINE: &str = "timeseries";

    /// Record a data point append operation.
    #[inline]
    pub fn record_append() {
        counter!("nanokv.table.timeseries.append").increment(1);
    }

    /// Record a bucket creation operation.
    #[inline]
    pub fn record_bucket_created() {
        counter!("nanokv.table.timeseries.bucket_created").increment(1);
    }

    /// Record a bucket flush operation.
    #[inline]
    pub fn record_bucket_flushed() {
        counter!("nanokv.table.timeseries.bucket_flushed").increment(1);
    }

    /// Record an aggregation query operation.
    #[inline]
    pub fn record_aggregation() {
        counter!("nanokv.table.timeseries.aggregation").increment(1);
    }

    /// Record the number of data points in an aggregation.
    #[inline]
    pub fn record_aggregation_points(count: usize) {
        histogram!("nanokv.table.timeseries.aggregation_points").record(count as f64);
    }

    /// Update the number of active buckets.
    #[inline]
    pub fn set_active_buckets(count: usize) {
        gauge!("nanokv.table.timeseries.active_buckets").set(count as f64);
    }

    /// Update the total number of data points stored.
    #[inline]
    pub fn set_total_points(count: u64) {
        gauge!("nanokv.table.timeseries.total_points").set(count as f64);
    }

    /// Update the compression ratio achieved.
    #[inline]
    pub fn set_compression_ratio(ratio: f64) {
        gauge!("nanokv.table.timeseries.compression_ratio").set(ratio);
    }

    /// Record append operation latency.
    #[inline]
    pub fn record_append_duration(start: Instant) {
        histogram!("nanokv.table.timeseries.append_duration").record(start.elapsed().as_secs_f64());
    }

    /// Record aggregation query latency.
    #[inline]
    pub fn record_aggregation_duration(start: Instant) {
        histogram!("nanokv.table.timeseries.aggregation_duration")
            .record(start.elapsed().as_secs_f64());
    }
}

// =============================================================================
// HNSW Vector Index Metrics
// =============================================================================

/// HNSW vector index metrics for search and graph operations.
pub mod hnsw {
    use super::*;

    const ENGINE: &str = "hnsw";

    /// Record a vector insert operation.
    #[inline]
    pub fn record_insert() {
        counter!("nanokv.table.hnsw.insert").increment(1);
    }

    /// Record a vector search operation.
    #[inline]
    pub fn record_search() {
        counter!("nanokv.table.hnsw.search").increment(1);
    }

    /// Record the number of distance calculations during search.
    #[inline]
    pub fn record_distance_calculations(count: usize) {
        histogram!("nanokv.table.hnsw.distance_calculations").record(count as f64);
    }

    /// Record the number of hops during search.
    #[inline]
    pub fn record_search_hops(count: usize) {
        histogram!("nanokv.table.hnsw.search_hops").record(count as f64);
    }

    /// Update the number of vectors indexed.
    #[inline]
    pub fn set_vector_count(count: usize) {
        gauge!("nanokv.table.hnsw.vector_count").set(count as f64);
    }

    /// Update the maximum layer in the graph.
    #[inline]
    pub fn set_max_layer(layer: usize) {
        gauge!("nanokv.table.hnsw.max_layer").set(layer as f64);
    }

    /// Record vector search latency.
    #[inline]
    pub fn record_search_duration(start: Instant) {
        histogram!("nanokv.table.hnsw.search_duration").record(start.elapsed().as_secs_f64());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_common_metrics() {
        // Just ensure the functions compile and don't panic
        record_get("btree");
        record_put("lsm");
        record_delete("bloom");
        record_scan("rtree");
    }

    #[test]
    fn test_btree_metrics() {
        btree::record_split();
        btree::record_merge();
        btree::set_tree_height(5);
        btree::set_internal_nodes(100);
        btree::set_leaf_nodes(500);
    }

    #[test]
    fn test_lsm_metrics() {
        lsm::record_memtable_write();
        lsm::record_memtable_flush();
        lsm::record_sstable_read();
        lsm::record_compaction();
        lsm::set_sstable_count(10);
        lsm::set_read_amplification(2.5);
        lsm::set_write_amplification(3.0);
    }

    #[test]
    fn test_bloom_metrics() {
        bloom::record_insert();
        bloom::record_query();
        bloom::record_positive();
        bloom::record_false_positive();
        bloom::set_saturation(0.75);
        bloom::set_false_positive_rate(0.01);
    }

    #[test]
    fn test_rtree_metrics() {
        rtree::record_split();
        rtree::record_query();
        rtree::record_query_candidates(50);
        rtree::record_query_results(10);
        rtree::set_tree_height(4);
        rtree::set_object_count(1000);
    }

    #[test]
    fn test_timeseries_metrics() {
        timeseries::record_append();
        timeseries::record_bucket_created();
        timeseries::record_aggregation();
        timeseries::set_active_buckets(5);
        timeseries::set_total_points(10000);
        timeseries::set_compression_ratio(4.5);
    }
}

// Made with Bob
