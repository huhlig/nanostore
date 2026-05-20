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

//! Stress tests for TimeSeries table engine.
//!
//! These tests validate TimeSeries behavior under heavy load:
//! - High ingestion rates (10K+ points/second)
//! - Large time ranges
//! - Many concurrent series
//! - Dense time series data
//! - Complex time range queries

use nanostore::pager::{Pager, PagerConfig};
use nanostore::table::TimeSeries;
use nanostore::table::TimeSeriesCursor;
use nanostore::table::timeseries::{TimeSeriesConfig, TimeSeriesTable};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
use std::sync::Arc;

/// Helper to create a test TimeSeries table
fn create_test_timeseries() -> TimeSeriesTable<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "ts_stress.db", PagerConfig::default()).unwrap());

    TimeSeriesTable::new(
        TableId::from(1),
        "stress_timeseries".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap()
}

/// Test high ingestion rate: 50,000 points in a single series
///
/// Validates that TimeSeries can handle high-volume data ingestion.
#[test]
fn test_high_ingestion_50k_points() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    let series_key = b"high_volume_sensor";

    // Insert 50,000 points
    for i in 0..50_000 {
        let timestamp = i * 1000; // 1 second intervals
        let value = format!("value_{}", i);
        table
            .append_point(series_key, timestamp, value.as_bytes(), tx_id)
            .unwrap();
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(1))
        .unwrap();

    // Verify data is accessible
    let cursor = table.scan_series(series_key, 0, 50_000_000).unwrap();
    assert_eq!(cursor.count(), 50_000);
}

/// Test 100,000 points across multiple series
///
/// Validates that TimeSeries can handle many concurrent series.
#[test]
fn test_multiple_series_100k_points() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    // 10 series with 10,000 points each
    for series_idx in 0..10 {
        let series_key = format!("sensor_{}", series_idx);
        for i in 0..10_000 {
            let timestamp = i * 1000;
            let value = format!("s{}_v{}", series_idx, i);
            table
                .append_point(series_key.as_bytes(), timestamp, value.as_bytes(), tx_id)
                .unwrap();
        }
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(1))
        .unwrap();

    // Verify each series
    for series_idx in 0..10 {
        let series_key = format!("sensor_{}", series_idx);
        let cursor = table
            .scan_series(series_key.as_bytes(), 0, 10_000_000)
            .unwrap();
        assert_eq!(cursor.count(), 10_000, "Series {} failed", series_idx);
    }
}

/// Test dense time series: 1 million points with millisecond precision
///
/// Validates that TimeSeries can handle very dense data.
#[test]
fn test_dense_timeseries_1m_points() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    let series_key = b"dense_sensor";

    // Insert 1 million points with millisecond timestamps
    for i in 0..1_000_000i64 {
        let timestamp = i; // 1ms intervals
        let value = format!("{}", i % 1000);
        table
            .append_point(series_key, timestamp, value.as_bytes(), tx_id)
            .unwrap();

        // Commit periodically to avoid memory issues
        if i % 50_000 == 0 && i > 0 {
            table
                .commit_versions(tx_id, LogSequenceNumber::from((i / 50_000) as u64))
                .unwrap();
        }
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(20))
        .unwrap();

    // Verify total count
    let cursor = table.scan_series(series_key, 0, 1_000_000).unwrap();
    assert_eq!(cursor.count(), 1_000_000);
}

/// Test large time range queries
///
/// Validates that queries over large time ranges work correctly.
#[test]
fn test_large_time_range_queries_100k_points() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    let series_key = b"range_sensor";

    // Insert 100,000 points over a year (365 days)
    let year_in_seconds = 365 * 24 * 60 * 60;
    let interval = year_in_seconds / 100_000;

    for i in 0..100_000 {
        let timestamp = i * interval;
        let value = format!("value_{}", i);
        table
            .append_point(series_key, timestamp, value.as_bytes(), tx_id)
            .unwrap();
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(1))
        .unwrap();

    // Query different time ranges
    let ranges = vec![
        (0, year_in_seconds / 4),           // First quarter
        (year_in_seconds / 4, year_in_seconds / 2), // Second quarter
        (0, year_in_seconds),               // Full year
    ];

    for (start, end) in ranges {
        let cursor = table.scan_series(series_key, start, end).unwrap();
        let count = cursor.count();
        assert!(count > 0, "Range [{}, {}) should have data", start, end);
    }
}

/// Test out-of-order insertions
///
/// Validates that TimeSeries handles out-of-order data correctly.
#[test]
fn test_out_of_order_insertions_20k_points() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    let series_key = b"ooo_sensor";

    // Insert 20,000 points in pseudo-random order
    for i in 0..20_000 {
        let timestamp = ((i * 7919) % 20_000) * 1000;
        let value = format!("value_{}", timestamp);
        table
            .append_point(series_key, timestamp, value.as_bytes(), tx_id)
            .unwrap();
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(1))
        .unwrap();

    // Verify data is sorted when scanned
    let mut cursor = table.scan_series(series_key, 0, 20_000_000).unwrap();
    let mut prev_timestamp = -1i64;

    while cursor.valid() {
        if let Some(point) = cursor.current() {
            assert!(
                point.timestamp > prev_timestamp,
                "Points should be sorted by timestamp"
            );
            prev_timestamp = point.timestamp;
        }
        cursor.next().unwrap();
    }
}

/// Test latest_before queries with large dataset
///
/// Validates that latest_before works efficiently with large datasets.
#[test]
fn test_latest_before_queries_50k_points() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    let series_key = b"latest_sensor";

    // Insert 50,000 points
    for i in 0..50_000 {
        let timestamp = i * 1000;
        let value = format!("value_{}", i);
        table
            .append_point(series_key, timestamp, value.as_bytes(), tx_id)
            .unwrap();
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(1))
        .unwrap();

    // Perform multiple latest_before queries
    for query_ts in [5_000_000, 15_000_000, 25_000_000, 35_000_000, 45_000_000] {
        let result = table.latest_before(series_key, query_ts).unwrap();
        assert!(result.is_some(), "Should find point before {}", query_ts);

        let point = result.unwrap();
        assert!(
            point.timestamp <= query_ts,
            "Point timestamp should be <= query timestamp"
        );
    }
}

/// Test many small series
///
/// Validates that TimeSeries can handle many small series efficiently.
#[test]
fn test_many_small_series_1000_series() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    // 1,000 series with 100 points each
    for series_idx in 0..1_000 {
        let series_key = format!("sensor_{:04}", series_idx);
        for i in 0..100 {
            let timestamp = i * 1000;
            let value = format!("v{}", i);
            table
                .append_point(series_key.as_bytes(), timestamp, value.as_bytes(), tx_id)
                .unwrap();
        }
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(1))
        .unwrap();

    // Verify random sample of series
    for series_idx in [0, 100, 500, 750, 999] {
        let series_key = format!("sensor_{:04}", series_idx);
        let cursor = table
            .scan_series(series_key.as_bytes(), 0, 100_000)
            .unwrap();
        assert_eq!(cursor.count(), 100, "Series {} failed", series_idx);
    }
}

/// Test bucket boundaries with high volume
///
/// Validates that bucket management works correctly under load.
#[test]
fn test_bucket_boundaries_30k_points() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    let series_key = b"bucket_sensor";
    let bucket_size = 3600; // 1 hour buckets

    // Insert 30,000 points spanning multiple buckets
    for i in 0..30_000 {
        let timestamp = i * 10; // 10 second intervals
        let value = format!("value_{}", i);
        table
            .append_point(series_key, timestamp, value.as_bytes(), tx_id)
            .unwrap();
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(1))
        .unwrap();

    // Query across bucket boundaries
    let num_buckets = (30_000 * 10) / bucket_size;
    for bucket_idx in 0..num_buckets {
        let start = bucket_idx * bucket_size;
        let end = (bucket_idx + 1) * bucket_size;
        let cursor = table.scan_series(series_key, start, end).unwrap();
        assert!(cursor.count() > 0, "Bucket {} should have data", bucket_idx);
    }
}

/// Test concurrent updates to same series
///
/// Validates that multiple transactions can update the same series.
#[test]
fn test_concurrent_updates_10k_points() {
    let mut table = create_test_timeseries();
    let series_key = b"concurrent_sensor";

    // Simulate 10 transactions, each adding 1,000 points
    for tx_num in 0..10i64 {
        let tx_id = TransactionId::from((tx_num + 1) as u64);
        let base_timestamp = tx_num * 1_000_000;

        for i in 0..1_000i64 {
            let timestamp = base_timestamp + i * 1000;
            let value = format!("tx{}_v{}", tx_num, i);
            table
                .append_point(series_key, timestamp, value.as_bytes(), tx_id)
                .unwrap();
        }

        table
            .commit_versions(tx_id, LogSequenceNumber::from((tx_num + 1) as u64))
            .unwrap();
    }

    // Verify all points are accessible
    let cursor = table.scan_series(series_key, 0, 10_000_000).unwrap();
    assert_eq!(cursor.count(), 10_000);
}

/// Test large value sizes
///
/// Validates that TimeSeries can handle large values (1KB each).
#[test]
fn test_large_values_5k_points() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    let series_key = b"large_value_sensor";
    let large_value = vec![b'X'; 1024]; // 1KB value

    // Insert 5,000 points with large values
    for i in 0..5_000 {
        let timestamp = i * 1000;
        table
            .append_point(series_key, timestamp, &large_value, tx_id)
            .unwrap();
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(1))
        .unwrap();

    // Verify values
    let mut cursor = table.scan_series(series_key, 0, 5_000_000).unwrap();
    let mut count = 0;

    while cursor.valid() {
        if let Some(point) = cursor.current() {
            assert_eq!(point.value_key.as_ref().len(), 1024);
            count += 1;
        }
        cursor.next().unwrap();
    }

    assert_eq!(count, 5_000);
}

/// Test time series with gaps
///
/// Validates that TimeSeries handles sparse data correctly.
#[test]
fn test_sparse_timeseries_10k_points() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    let series_key = b"sparse_sensor";

    // Insert 10,000 points with large gaps
    for i in 0..10_000 {
        let timestamp = i * 10_000; // 10 second gaps
        let value = format!("value_{}", i);
        table
            .append_point(series_key, timestamp, value.as_bytes(), tx_id)
            .unwrap();
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(1))
        .unwrap();

    // Query specific ranges
    let cursor = table.scan_series(series_key, 0, 100_000_000).unwrap();
    assert_eq!(cursor.count(), 10_000);

    // Verify gaps exist
    let cursor = table.scan_series(series_key, 5_000, 6_000).unwrap();
    assert_eq!(cursor.count(), 0, "Should have no points in gap");
}

/// Test mixed series sizes
///
/// Validates that TimeSeries handles series of varying sizes.
#[test]
fn test_mixed_series_sizes_50k_total() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    // Create series of different sizes
    let series_configs = vec![
        ("tiny", 10),
        ("small", 100),
        ("medium", 1_000),
        ("large", 10_000),
        ("huge", 38_890), // Total: 50,000
    ];

    for (name, count) in series_configs {
        let series_key = format!("sensor_{}", name);
        for i in 0..count {
            let timestamp = i * 1000;
            let value = format!("v{}", i);
            table
                .append_point(series_key.as_bytes(), timestamp, value.as_bytes(), tx_id)
                .unwrap();
        }
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(1))
        .unwrap();

    // Verify each series
    let cursor = table.scan_series(b"sensor_tiny", 0, 100_000).unwrap();
    assert_eq!(cursor.count(), 10);

    let cursor = table.scan_series(b"sensor_huge", 0, 40_000_000).unwrap();
    assert_eq!(cursor.count(), 38_890);
}

/// Test rapid successive queries
///
/// Validates that TimeSeries can handle many queries efficiently.
#[test]
fn test_rapid_queries_10k_points() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    let series_key = b"query_sensor";

    // Insert 10,000 points
    for i in 0..10_000 {
        let timestamp = i * 1000;
        let value = format!("value_{}", i);
        table
            .append_point(series_key, timestamp, value.as_bytes(), tx_id)
            .unwrap();
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(1))
        .unwrap();

    // Perform 1,000 queries
    for query_idx in 0..1_000 {
        let start = (query_idx * 10) * 1000;
        let end = start + 10_000;
        let cursor = table.scan_series(series_key, start, end).unwrap();
        assert!(cursor.count() > 0, "Query {} should return data", query_idx);
    }
}

/// Test overlapping time ranges
///
/// Validates that overlapping queries work correctly.
#[test]
fn test_overlapping_ranges_20k_points() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    let series_key = b"overlap_sensor";

    // Insert 20,000 points
    for i in 0..20_000 {
        let timestamp = i * 1000;
        let value = format!("value_{}", i);
        table
            .append_point(series_key, timestamp, value.as_bytes(), tx_id)
            .unwrap();
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(1))
        .unwrap();

    // Query overlapping ranges
    let ranges = vec![
        (0, 10_000_000),
        (5_000_000, 15_000_000),
        (10_000_000, 20_000_000),
        (0, 20_000_000),
    ];

    for (start, end) in ranges {
        let cursor = table.scan_series(series_key, start, end).unwrap();
        let count = cursor.count();
        assert!(count > 0, "Range [{}, {}) should have data", start, end);
    }
}

/// Test statistics with large dataset
///
/// Validates that statistics are accurate with large datasets.
#[test]
fn test_statistics_50k_points() {
    let mut table = create_test_timeseries();
    let tx_id = TransactionId::from(1);

    let series_key = b"stats_sensor";

    // Insert 50,000 points
    for i in 0..50_000 {
        let timestamp = i * 1000;
        let value = format!("value_{}", i);
        table
            .append_point(series_key, timestamp, value.as_bytes(), tx_id)
            .unwrap();
    }

    table
        .commit_versions(tx_id, LogSequenceNumber::from(1))
        .unwrap();

    // Verify statistics
    let stats = table.stats().unwrap();
    assert_eq!(stats.entry_count, Some(50_000));
    assert!(stats.size_bytes.is_some());
    assert!(stats.size_bytes.unwrap() > 0);
}

// Made with Bob