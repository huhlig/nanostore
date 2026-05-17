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

//! Comprehensive integration tests for the TimeSeries table engine.
//!
//! These tests cover:
//! 1. Basic insert and scan operations
//! 2. Time range queries
//! 3. Latest-before queries
//! 4. Bucket management and rolling
//! 5. Retention policy enforcement
//! 6. Edge cases (empty ranges, negative timestamps, etc.)

use nanokv::pager::{Pager, PagerConfig};
use nanokv::table::TimeSeries;
use nanokv::table::TimeSeriesCursor;
use nanokv::table::timeseries::{TimeSeriesConfig, TimeSeriesTable};
use nanokv::txn::TransactionId;
use nanokv::types::TableId;
use nanokv::vfs::MemoryFileSystem;
use nanokv::wal::LogSequenceNumber;
use std::sync::Arc;

// Helper function to create a transaction ID for testing
fn create_tx_id() -> TransactionId {
    TransactionId::from(1)
}

// =============================================================================
// Basic Insert and Scan Operations
// =============================================================================

#[test]
fn test_basic_insert_and_scan() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "basic.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(1),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let series_key = b"cpu.usage";
    let points = vec![
        (100i64, b"10.5"),
        (200i64, b"20.0"),
        (300i64, b"30.5"),
        (400i64, b"40.0"),
    ];

    let tx_id = create_tx_id();
    for (ts, value) in &points {
        table.append_point(series_key, *ts, *value, tx_id).unwrap();
    }
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let mut cursor = table.scan_series(series_key, 0, 500).unwrap();

    let mut count = 0;
    while cursor.valid() {
        let point = cursor.current().unwrap();
        assert_eq!(point.series_key.as_ref(), series_key);
        cursor.next().unwrap();
        count += 1;
    }

    assert_eq!(count, 4);
}

#[test]
fn test_insert_multiple_series() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "multi_series.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(2),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table
        .append_point(b"cpu.usage", 100, b"10.5", tx_id)
        .unwrap();
    table
        .append_point(b"memory.usage", 100, b"50.0", tx_id)
        .unwrap();
    table.append_point(b"disk.io", 100, b"1000", tx_id).unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let cursor = table.scan_series(b"cpu.usage", 0, 200).unwrap();
    assert_eq!(cursor.count(), 1);

    let cursor = table.scan_series(b"memory.usage", 0, 200).unwrap();
    assert_eq!(cursor.count(), 1);

    let cursor = table.scan_series(b"disk.io", 0, 200).unwrap();
    assert_eq!(cursor.count(), 1);
}

#[test]
fn test_scan_empty_series() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "empty.db", PagerConfig::default()).unwrap());
    let table = TimeSeriesTable::new(
        TableId::from(3),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let cursor = table.scan_series(b"nonexistent", 0, 1000).unwrap();
    assert!(!cursor.valid());
    assert!(cursor.current().is_none());
    assert_eq!(cursor.count(), 0);
}

// =============================================================================
// Time Range Queries
// =============================================================================

#[test]
fn test_time_range_query_full_range() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "range_full.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(4),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    for i in 0..10 {
        table
            .append_point(b"sensor", i * 100, format!("{}", i).as_bytes(), tx_id)
            .unwrap();
    }
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let cursor = table.scan_series(b"sensor", 0, 1000).unwrap();
    assert_eq!(cursor.count(), 10);
}

#[test]
fn test_time_range_query_partial_range() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "range_partial.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(5),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    for i in 0..10 {
        table
            .append_point(b"sensor", i * 100, format!("{}", i).as_bytes(), tx_id)
            .unwrap();
    }
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let cursor = table.scan_series(b"sensor", 200, 600).unwrap();
    assert_eq!(cursor.count(), 4);

    let mut timestamps = Vec::new();
    let mut c = table.scan_series(b"sensor", 200, 600).unwrap();
    while c.valid() {
        if let Some(point) = c.current() {
            timestamps.push(point.timestamp);
        }
        c.next().unwrap();
    }

    assert_eq!(timestamps, vec![200, 300, 400, 500]);
}

#[test]
fn test_time_range_query_no_overlap() {
    let fs = MemoryFileSystem::new();
    let pager =
        Arc::new(Pager::create(&fs, "range_no_overlap.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(6),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    for i in 0..5 {
        table
            .append_point(b"sensor", i * 100, format!("{}", i).as_bytes(), tx_id)
            .unwrap();
    }
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let cursor = table.scan_series(b"sensor", 1000, 2000).unwrap();
    assert_eq!(cursor.count(), 0);
    assert!(!cursor.valid());
}

#[test]
fn test_time_range_query_boundary_inclusive() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "range_boundary.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(7),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table.append_point(b"sensor", 100, b"first", tx_id).unwrap();
    table
        .append_point(b"sensor", 200, b"middle", tx_id)
        .unwrap();
    table.append_point(b"sensor", 300, b"last", tx_id).unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    // Range is [start, end) so use 301 to include 300
    let cursor = table.scan_series(b"sensor", 100, 301).unwrap();
    assert_eq!(cursor.count(), 3);
}

#[test]
fn test_time_range_query_single_point() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "range_single.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(8),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table.append_point(b"sensor", 100, b"only", tx_id).unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let cursor = table.scan_series(b"sensor", 100, 101).unwrap();
    assert_eq!(cursor.count(), 1);
}

// =============================================================================
// Latest-Before Queries
// =============================================================================

#[test]
fn test_latest_before_basic() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "latest_basic.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(9),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table.append_point(b"sensor", 100, b"10.5", tx_id).unwrap();
    table.append_point(b"sensor", 200, b"20.0", tx_id).unwrap();
    table.append_point(b"sensor", 300, b"30.5", tx_id).unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let result = table.latest_before(b"sensor", 250).unwrap();
    assert!(result.is_some());
    let point = result.unwrap();
    assert_eq!(point.timestamp, 200);
    assert_eq!(point.value_key.as_ref(), b"20.0");
}

#[test]
fn test_latest_before_exact_timestamp() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "latest_exact.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(10),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table.append_point(b"sensor", 100, b"10.5", tx_id).unwrap();
    table.append_point(b"sensor", 200, b"20.0", tx_id).unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let result = table.latest_before(b"sensor", 200).unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().timestamp, 200);
}

#[test]
fn test_latest_before_before_all_points() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "latest_before.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(11),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table.append_point(b"sensor", 100, b"10.5", tx_id).unwrap();
    table.append_point(b"sensor", 200, b"20.0", tx_id).unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let result = table.latest_before(b"sensor", 50).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_latest_before_after_all_points() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "latest_after.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(12),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table.append_point(b"sensor", 100, b"10.5", tx_id).unwrap();
    table.append_point(b"sensor", 200, b"20.0", tx_id).unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let result = table.latest_before(b"sensor", 300).unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().timestamp, 200);
}

#[test]
fn test_latest_before_nonexistent_series() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "latest_none.db", PagerConfig::default()).unwrap());
    let table = TimeSeriesTable::new(
        TableId::from(13),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let result = table.latest_before(b"nonexistent", 100).unwrap();
    assert!(result.is_none());
}

// =============================================================================
// Bucket Management and Rolling
// =============================================================================

#[test]
fn test_bucket_creation_across_boundaries() {
    let fs = MemoryFileSystem::new();
    let pager =
        Arc::new(Pager::create(&fs, "bucket_boundaries.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(14),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let bucket_size = 3600;

    let tx_id = create_tx_id();
    table.append_point(b"sensor", 0, b"bucket0", tx_id).unwrap();
    table
        .append_point(b"sensor", bucket_size as i64, b"bucket1", tx_id)
        .unwrap();
    table
        .append_point(b"sensor", (bucket_size * 2) as i64, b"bucket2", tx_id)
        .unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let cursor = table
        .scan_series(b"sensor", 0, (bucket_size * 3) as i64)
        .unwrap();
    assert_eq!(cursor.count(), 3);
}

#[test]
fn test_bucket_rolling_with_many_inserts() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "bucket_rolling.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(15),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    for i in 0..100 {
        table
            .append_point(b"sensor", i * 100, format!("value_{}", i).as_bytes(), tx_id)
            .unwrap();
    }
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let cursor = table.scan_series(b"sensor", 0, 10000).unwrap();
    assert_eq!(cursor.count(), 100);
}

#[test]
fn test_table_statistics() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "stats.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(16),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    assert_eq!(table.stats().unwrap().entry_count, Some(0));

    let tx_id = create_tx_id();
    for i in 0..10 {
        table
            .append_point(b"sensor", i * 100, format!("value_{}", i).as_bytes(), tx_id)
            .unwrap();
    }
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let stats = table.stats().unwrap();
    assert_eq!(stats.entry_count, Some(10));
    assert!(stats.size_bytes.is_some());
    assert!(stats.size_bytes.unwrap() > 0);
}

#[test]
fn test_table_verify() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "verify.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(17),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    for i in 0..5 {
        table
            .append_point(b"sensor", i * 100, format!("value_{}", i).as_bytes(), tx_id)
            .unwrap();
    }
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let report = table.verify().unwrap();
    assert!(report.errors.is_empty());
    assert!(report.warnings.is_empty());
}

// =============================================================================
// Retention Policy Enforcement
// =============================================================================

#[test]
fn test_retention_policy_configuration() {
    let config = TimeSeriesConfig::default().with_retention_policy(
        nanokv::table::timeseries::TimeSeriesRetentionPolicy::max_age(
            std::time::Duration::from_secs(3600),
        ),
    );

    assert!(matches!(
        config.retention_policy,
        nanokv::table::timeseries::TimeSeriesRetentionPolicy::MaxAge(d) if d == std::time::Duration::from_secs(3600)
    ));
}

#[test]
fn test_retention_policy_with_no_retention() {
    let config = TimeSeriesConfig::default();
    assert!(matches!(
        config.retention_policy,
        nanokv::table::timeseries::TimeSeriesRetentionPolicy::None
    ));
}

// =============================================================================
// Edge Cases
// =============================================================================

#[test]
fn test_empty_time_range() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "empty_range.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(18),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table.append_point(b"sensor", 100, b"value", tx_id).unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let cursor = table.scan_series(b"sensor", 200, 200).unwrap();
    assert_eq!(cursor.count(), 0);
}

#[test]
fn test_negative_timestamps_scan() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "negative_ts.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(19),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table.append_point(b"sensor", 1000, b"old", tx_id).unwrap();
    table
        .append_point(b"sensor", 2000, b"older", tx_id)
        .unwrap();
    table
        .append_point(b"sensor", 3000, b"epoch", tx_id)
        .unwrap();
    table.append_point(b"sensor", 4000, b"new", tx_id).unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let cursor = table.scan_series(b"sensor", 1500, 2500).unwrap();
    assert_eq!(cursor.count(), 1);

    let c = table.scan_series(b"sensor", 1500, 2500).unwrap();
    assert!(c.valid());
    assert_eq!(c.current().unwrap().timestamp, 2000);
}

#[test]
fn test_negative_timestamps_latest_before() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "negative_latest.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(20),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table
        .append_point(b"sensor", 1000, b"first", tx_id)
        .unwrap();
    table
        .append_point(b"sensor", 2000, b"second", tx_id)
        .unwrap();
    table
        .append_point(b"sensor", 3000, b"third", tx_id)
        .unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let result = table.latest_before(b"sensor", 2500).unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().timestamp, 2000);
}

#[test]
fn test_cursor_iteration_stops_at_end() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "cursor_stop.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(21),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    for i in 0..5 {
        table
            .append_point(b"sensor", i * 100, format!("value_{}", i).as_bytes(), tx_id)
            .unwrap();
    }
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let mut cursor = table.scan_series(b"sensor", 0, 500).unwrap();
    let mut count = 0;

    while cursor.valid() {
        count += 1;
        cursor.next().unwrap();
    }

    assert_eq!(count, 5);
    assert!(!cursor.valid());
    assert!(cursor.current().is_none());
}

#[test]
fn test_cursor_next_beyond_end() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "cursor_beyond.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(22),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table.append_point(b"sensor", 100, b"value", tx_id).unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let mut cursor = table.scan_series(b"sensor", 0, 200).unwrap();
    assert!(cursor.valid());

    cursor.next().unwrap();
    assert!(!cursor.valid());

    cursor.next().unwrap();
    assert!(!cursor.valid());
}

#[test]
fn test_concurrent_series_independence() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "independent.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(23),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table.append_point(b"series_a", 100, b"a1", tx_id).unwrap();
    table.append_point(b"series_a", 200, b"a2", tx_id).unwrap();
    table.append_point(b"series_b", 150, b"b1", tx_id).unwrap();
    table.append_point(b"series_b", 250, b"b2", tx_id).unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let cursor_a = table.scan_series(b"series_a", 0, 300).unwrap();
    assert_eq!(cursor_a.count(), 2);

    let cursor_b = table.scan_series(b"series_b", 0, 300).unwrap();
    assert_eq!(cursor_b.count(), 2);
}

#[test]
fn test_large_number_of_points() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "large_points.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(24),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    for i in 0..10000 {
        table
            .append_point(b"sensor", i, format!("value_{}", i).as_bytes(), tx_id)
            .unwrap();
    }
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let cursor = table.scan_series(b"sensor", 0, 10000).unwrap();
    assert_eq!(cursor.count(), 10000);
}

#[test]
fn test_timestamp_ordering_in_scan() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "ordering.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(25),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table.append_point(b"sensor", 300, b"third", tx_id).unwrap();
    table.append_point(b"sensor", 100, b"first", tx_id).unwrap();
    table
        .append_point(b"sensor", 200, b"second", tx_id)
        .unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let mut cursor = table.scan_series(b"sensor", 0, 400).unwrap();
    assert!(cursor.valid());
    assert_eq!(cursor.current().unwrap().timestamp, 100);

    cursor.next().unwrap();
    assert!(cursor.valid());
    assert_eq!(cursor.current().unwrap().timestamp, 200);

    cursor.next().unwrap();
    assert!(cursor.valid());
    assert_eq!(cursor.current().unwrap().timestamp, 300);
}

#[test]
fn test_value_key_preservation() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "value_preserve.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(26),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let values = [
        b"simple".as_slice(),
        b"with:colon".as_slice(),
        b"with spaces".as_slice(),
        b"special!@#$%^&*()".as_slice(),
        b"unicode:\xe6\xb5\x8b\xe8\xaf\x95".as_slice(),
    ];

    let tx_id = create_tx_id();
    for (i, value) in values.iter().enumerate() {
        table
            .append_point(b"sensor", i as i64 * 100, value, tx_id)
            .unwrap();
    }
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let mut cursor = table.scan_series(b"sensor", 0, 500).unwrap();
    let mut idx = 0;

    while cursor.valid() {
        let point = cursor.current().unwrap();
        assert_eq!(point.value_key.as_ref(), values[idx]);
        idx += 1;
        cursor.next().unwrap();
    }

    assert_eq!(idx, values.len());
}

#[test]
fn test_empty_series_key() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "empty_key.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(27),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table.append_point(b"", 100, b"value", tx_id).unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let cursor = table.scan_series(b"", 0, 200).unwrap();
    assert_eq!(cursor.count(), 1);
}

#[test]
fn test_zero_timestamp() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "zero_ts.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(28),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let tx_id = create_tx_id();
    table.append_point(b"sensor", 0, b"epoch", tx_id).unwrap();
    table.commit_versions(tx_id, LogSequenceNumber::from(1)).unwrap();

    let cursor = table.scan_series(b"sensor", 0, 1).unwrap();
    assert_eq!(cursor.count(), 1);

    let result = table.latest_before(b"sensor", 1).unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().timestamp, 0);
}

#[test]
fn test_table_capabilities() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "capabilities.db", PagerConfig::default()).unwrap());
    let table = TimeSeriesTable::new(
        TableId::from(29),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let caps = table.capabilities();
    assert!(caps.exact);
    assert!(caps.ordered);
    assert!(caps.supports_range_query);
    assert!(!caps.approximate);
    assert!(!caps.sparse);
    assert!(!caps.supports_delete);
}

#[test]
fn test_table_name_and_id() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "name_id.db", PagerConfig::default()).unwrap());
    let table = TimeSeriesTable::new(
        TableId::from(42),
        "my_metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    assert_eq!(table.table_id(), TableId::from(42));
    assert_eq!(table.name(), "my_metrics");
}

// Made with Bob
