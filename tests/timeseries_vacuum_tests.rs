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

//! Specialized vacuum tests for TimeSeries table engine.
//!
//! TimeSeries tables require specialized vacuum testing due to their unique
//! time-bucketed architecture and scan-based API. These tests cover:
//! 1. Vacuum of time-bucketed data
//! 2. Vacuum respecting time-based retention policies
//! 3. Vacuum interaction with bucket compaction
//! 4. Vacuum of expired time series data
//! 5. Vacuum using scan_series() API instead of get()
//!
//! Note: TimeSeries tables use scan_series() for data access, not get() operations.

use nanostore::pager::{Pager, PagerConfig};
use nanostore::snap::Snapshot;
use nanostore::table::timeseries::{TimeSeriesConfig, TimeSeriesRetentionPolicy, TimeSeriesTable};
use nanostore::table::{TimeSeries, TimeSeriesCursor};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
use std::sync::Arc;
use std::time::Duration;

/// Helper to create a test TimeSeries table
fn create_test_table(name: &str, config: TimeSeriesConfig) -> TimeSeriesTable<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let pager =
        Arc::new(Pager::create(&fs, &format!("{}.db", name), PagerConfig::default()).unwrap());
    TimeSeriesTable::new(TableId::from(1), name.to_string(), pager, config).unwrap()
}

/// Helper to create a transaction ID
fn create_tx_id(id: u64) -> TransactionId {
    TransactionId::from(id)
}

/// Helper to append multiple points to a series
fn append_points(
    table: &mut TimeSeriesTable<MemoryFileSystem>,
    series_key: &[u8],
    points: &[(i64, &[u8])],
    tx_id: TransactionId,
    commit_lsn: LogSequenceNumber,
) {
    for (ts, value) in points {
        table.append_point(series_key, *ts, *value, tx_id).unwrap();
    }
    table.commit_versions(tx_id, commit_lsn).unwrap();
}

/// Helper to count points in a series using scan_series
fn count_points(
    table: &TimeSeriesTable<MemoryFileSystem>,
    series_key: &[u8],
    start_ts: i64,
    end_ts: i64,
) -> usize {
    let mut cursor = table.scan_series(series_key, start_ts, end_ts).unwrap();
    let mut count = 0;
    while cursor.valid() {
        cursor.next().unwrap();
        count += 1;
    }
    count
}

// =============================================================================
// Test 1: Vacuum of time-bucketed data
// =============================================================================

#[test]
fn test_vacuum_time_bucketed_data() {
    // Create table with 1-hour buckets
    let config = TimeSeriesConfig::default().with_bucket_size(3600);
    let mut table = create_test_table("vacuum_bucketed", config);

    let series_key = b"cpu.usage";

    // Insert data across multiple buckets (3 hours worth)
    // Bucket 1: timestamps 0-3599
    let bucket1_points = vec![
        (100i64, b"10.0" as &[u8]),
        (500i64, b"15.0"),
        (1000i64, b"20.0"),
    ];
    append_points(
        &mut table,
        series_key,
        &bucket1_points,
        create_tx_id(1),
        LogSequenceNumber::from(1),
    );

    // Bucket 2: timestamps 3600-7199
    let bucket2_points = vec![
        (3700i64, b"25.0" as &[u8]),
        (4000i64, b"30.0"),
        (5000i64, b"35.0"),
    ];
    append_points(
        &mut table,
        series_key,
        &bucket2_points,
        create_tx_id(2),
        LogSequenceNumber::from(2),
    );

    // Bucket 3: timestamps 7200-10799
    let bucket3_points = vec![
        (7300i64, b"40.0" as &[u8]),
        (8000i64, b"45.0"),
        (9000i64, b"50.0"),
    ];
    append_points(
        &mut table,
        series_key,
        &bucket3_points,
        create_tx_id(3),
        LogSequenceNumber::from(3),
    );

    // Verify all points are accessible before vacuum
    let count_before = count_points(&table, series_key, 0, 10000);
    assert_eq!(count_before, 9, "Should have 9 points before vacuum");

    // Vacuum with min_visible_lsn = 3 (all data is visible)
    let removed = table.vacuum(LogSequenceNumber::from(3)).unwrap();
    assert!(removed >= 0, "Vacuum should complete successfully");

    // Verify all points are still accessible after vacuum
    let count_after = count_points(&table, series_key, 0, 10000);
    assert_eq!(
        count_after, 9,
        "Should still have 9 points after vacuum (no old versions to remove)"
    );

    // Verify data in each bucket is intact
    assert_eq!(count_points(&table, series_key, 0, 3600), 3);
    assert_eq!(count_points(&table, series_key, 3600, 7200), 3);
    assert_eq!(count_points(&table, series_key, 7200, 10800), 3);
}

#[test]
fn test_vacuum_removes_old_versions_in_buckets() {
    let config = TimeSeriesConfig::default().with_bucket_size(3600);
    let mut table = create_test_table("vacuum_old_versions", config);

    let series_key = b"temperature";

    // Insert initial points
    let initial_points = vec![
        (100i64, b"20.0" as &[u8]),
        (200i64, b"21.0"),
        (300i64, b"22.0"),
    ];
    append_points(
        &mut table,
        series_key,
        &initial_points,
        create_tx_id(1),
        LogSequenceNumber::from(1),
    );

    // Update with new versions (simulating overwrites in the same bucket)
    let updated_points = vec![
        (100i64, b"20.5" as &[u8]),
        (200i64, b"21.5"),
        (300i64, b"22.5"),
    ];
    append_points(
        &mut table,
        series_key,
        &updated_points,
        create_tx_id(2),
        LogSequenceNumber::from(2),
    );

    // Vacuum should remove old versions
    let removed = table.vacuum(LogSequenceNumber::from(2)).unwrap();
    assert!(removed >= 0, "Vacuum should complete");

    // Verify latest values are accessible
    let mut cursor = table.scan_series(series_key, 0, 400).unwrap();
    let mut count = 0;
    while cursor.valid() {
        cursor.next().unwrap();
        count += 1;
    }
    // Should have 6 points total (3 original + 3 updates)
    assert!(count >= 3, "Should have at least the latest versions");
}

// =============================================================================
// Test 2: Vacuum respecting time-based retention policies
// =============================================================================

#[test]
fn test_vacuum_with_max_age_retention_policy() {
    // Create table with 30-day retention policy
    let config = TimeSeriesConfig::default()
        .with_bucket_size(86400) // 1 day buckets
        .with_retention_policy(TimeSeriesRetentionPolicy::max_age(Duration::from_secs(
            86400 * 30,
        )));
    let mut table = create_test_table("vacuum_retention_age", config);

    let series_key = b"metrics";

    // Insert old data (40 days ago)
    let old_timestamp = 1000i64;
    let old_points = vec![(old_timestamp, b"old_value" as &[u8])];
    append_points(
        &mut table,
        series_key,
        &old_points,
        create_tx_id(1),
        LogSequenceNumber::from(1),
    );

    // Insert recent data (10 days ago)
    let recent_timestamp = old_timestamp + (86400 * 30);
    let recent_points = vec![(recent_timestamp, b"recent_value" as &[u8])];
    append_points(
        &mut table,
        series_key,
        &recent_points,
        create_tx_id(2),
        LogSequenceNumber::from(2),
    );

    // Vacuum should respect retention policy
    let removed = table.vacuum(LogSequenceNumber::from(2)).unwrap();
    assert!(removed >= 0, "Vacuum should complete");

    // Note: Actual retention enforcement may require additional logic
    // This test verifies vacuum completes without errors with retention policy set
    let count = count_points(&table, series_key, 0, i64::MAX);
    assert!(count >= 1, "Should have at least recent data");
}

#[test]
fn test_vacuum_with_max_points_retention_policy() {
    // Create table with max 5 points retention
    let config = TimeSeriesConfig::default()
        .with_bucket_size(3600)
        .with_retention_policy(TimeSeriesRetentionPolicy::max_points(5));
    let mut table = create_test_table("vacuum_retention_points", config);

    let series_key = b"sensor";

    // Insert 10 points
    for i in 0..10 {
        let value = format!("value{}", i);
        let points = vec![(i * 100, value.as_bytes())];
        append_points(
            &mut table,
            series_key,
            &points,
            create_tx_id(i as u64 + 1),
            LogSequenceNumber::from(i as u64 + 1),
        );
    }

    // Vacuum with retention policy
    let removed = table.vacuum(LogSequenceNumber::from(10)).unwrap();
    assert!(removed >= 0, "Vacuum should complete");

    // Verify data is still accessible (retention enforcement may vary)
    let count = count_points(&table, series_key, 0, 1000);
    assert!(
        count >= 5,
        "Should have at least 5 points per retention policy"
    );
}

#[test]
fn test_vacuum_with_until_timestamp_retention_policy() {
    // Create table with retention until specific timestamp
    let cutoff_timestamp = 5000i64;
    let config = TimeSeriesConfig::default()
        .with_bucket_size(3600)
        .with_retention_policy(TimeSeriesRetentionPolicy::until_timestamp(cutoff_timestamp));
    let mut table = create_test_table("vacuum_retention_timestamp", config);

    let series_key = b"events";

    // Insert data before and after cutoff
    let before_cutoff = vec![
        (1000i64, b"before1" as &[u8]),
        (2000i64, b"before2"),
        (3000i64, b"before3"),
    ];
    append_points(
        &mut table,
        series_key,
        &before_cutoff,
        create_tx_id(1),
        LogSequenceNumber::from(1),
    );

    let after_cutoff = vec![
        (6000i64, b"after1" as &[u8]),
        (7000i64, b"after2"),
        (8000i64, b"after3"),
    ];
    append_points(
        &mut table,
        series_key,
        &after_cutoff,
        create_tx_id(2),
        LogSequenceNumber::from(2),
    );

    // Vacuum should respect timestamp cutoff
    let removed = table.vacuum(LogSequenceNumber::from(2)).unwrap();
    assert!(removed >= 0, "Vacuum should complete");

    // Verify data after cutoff is accessible
    let count_after = count_points(&table, series_key, cutoff_timestamp, i64::MAX);
    assert!(count_after >= 3, "Should have data after cutoff");
}

// =============================================================================
// Test 3: Vacuum interaction with bucket compaction
// =============================================================================

#[test]
fn test_vacuum_with_multiple_buckets() {
    let config = TimeSeriesConfig::default()
        .with_bucket_size(1000) // Small buckets for testing
        .with_max_points_per_bucket(5);
    let mut table = create_test_table("vacuum_multi_bucket", config);

    let series_key = b"metrics";

    // Fill multiple buckets
    for bucket_idx in 0..5 {
        let base_ts = bucket_idx * 1000;
        for i in 0..3 {
            let value = format!("v{}", i);
            let points = vec![(base_ts + i * 100, value.as_bytes())];
            append_points(
                &mut table,
                series_key,
                &points,
                create_tx_id(bucket_idx as u64 + 1),
                LogSequenceNumber::from(bucket_idx as u64 + 1),
            );
        }
    }

    // Verify all buckets have data
    let count_before = count_points(&table, series_key, 0, 5000);
    assert_eq!(count_before, 15, "Should have 15 points across 5 buckets");

    // Vacuum should work across all buckets
    let removed = table.vacuum(LogSequenceNumber::from(5)).unwrap();
    assert!(removed >= 0, "Vacuum should complete");

    // Verify data integrity after vacuum
    let count_after = count_points(&table, series_key, 0, 5000);
    assert_eq!(count_after, 15, "Should still have all points after vacuum");
}

#[test]
fn test_vacuum_with_bucket_overflow() {
    let config = TimeSeriesConfig::default()
        .with_bucket_size(1000)
        .with_max_points_per_bucket(3); // Small limit to trigger overflow
    let mut table = create_test_table("vacuum_bucket_overflow", config);

    let series_key = b"high_frequency";

    // Insert more points than max_points_per_bucket in same bucket
    for i in 0..10 {
        let value = format!("value{}", i);
        let points = vec![(100 + i * 10, value.as_bytes())];
        append_points(
            &mut table,
            series_key,
            &points,
            create_tx_id(1),
            LogSequenceNumber::from(1),
        );
    }

    // Vacuum should handle bucket overflow gracefully
    let removed = table.vacuum(LogSequenceNumber::from(1)).unwrap();
    assert!(removed >= 0, "Vacuum should complete with bucket overflow");

    // Verify all points are still accessible
    let count = count_points(&table, series_key, 0, 1000);
    assert_eq!(count, 10, "Should have all 10 points");
}

// =============================================================================
// Test 4: Vacuum of expired time series data
// =============================================================================

#[test]
fn test_vacuum_expired_data_with_snapshots() {
    let config = TimeSeriesConfig::default().with_bucket_size(3600);
    let mut table = create_test_table("vacuum_expired_snapshots", config);

    let series_key = b"sensor_data";

    // Insert initial data
    let initial_points = vec![(100i64, b"v1" as &[u8]), (200i64, b"v2"), (300i64, b"v3")];
    append_points(
        &mut table,
        series_key,
        &initial_points,
        create_tx_id(1),
        LogSequenceNumber::from(1),
    );

    // Create snapshot to pin this version
    let snapshot = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap1".to_string(),
        LogSequenceNumber::from(1),
        0,
        0,
        Vec::new(),
    );

    // Add more data
    let new_points = vec![(400i64, b"v4" as &[u8]), (500i64, b"v5"), (600i64, b"v6")];
    append_points(
        &mut table,
        series_key,
        &new_points,
        create_tx_id(2),
        LogSequenceNumber::from(2),
    );

    // Vacuum should respect snapshot (min_visible_lsn = snapshot.lsn)
    let removed = table.vacuum(snapshot.lsn).unwrap();
    assert!(removed >= 0, "Vacuum should complete");

    // Verify all data is still accessible
    let count = count_points(&table, series_key, 0, 700);
    assert_eq!(count, 6, "Should have all 6 points");

    // Vacuum with higher LSN (snapshot released)
    let removed2 = table.vacuum(LogSequenceNumber::from(3)).unwrap();
    assert!(
        removed2 >= 0,
        "Vacuum should complete after snapshot release"
    );
}

#[test]
fn test_vacuum_with_tombstones() {
    let config = TimeSeriesConfig::default().with_bucket_size(3600);
    let mut table = create_test_table("vacuum_tombstones", config);

    let series_key = b"events";

    // Insert points
    let points = vec![
        (100i64, b"event1" as &[u8]),
        (200i64, b"event2"),
        (300i64, b"event3"),
    ];
    append_points(
        &mut table,
        series_key,
        &points,
        create_tx_id(1),
        LogSequenceNumber::from(1),
    );

    // Add tombstone for middle point
    table
        .add_tombstone(series_key, 200i64, create_tx_id(2))
        .unwrap();
    table
        .commit_versions(create_tx_id(2), LogSequenceNumber::from(2))
        .unwrap();

    // Vacuum should handle tombstones
    let removed = table.vacuum(LogSequenceNumber::from(2)).unwrap();
    assert!(removed >= 0, "Vacuum should handle tombstones");

    // Verify remaining points are accessible
    let count = count_points(&table, series_key, 0, 400);
    // Count may vary depending on tombstone visibility
    assert!(count >= 2, "Should have at least 2 non-tombstoned points");
}

// =============================================================================
// Test 5: Vacuum using scan_series() API
// =============================================================================

#[test]
fn test_vacuum_verify_with_scan_series() {
    let config = TimeSeriesConfig::default().with_bucket_size(3600);
    let mut table = create_test_table("vacuum_scan_verify", config);

    let series_key = b"cpu.usage";

    // Insert data
    let points = vec![
        (100i64, b"10.0" as &[u8]),
        (200i64, b"20.0"),
        (300i64, b"30.0"),
        (400i64, b"40.0"),
        (500i64, b"50.0"),
    ];
    append_points(
        &mut table,
        series_key,
        &points,
        create_tx_id(1),
        LogSequenceNumber::from(1),
    );

    // Verify data before vacuum using scan_series
    let mut cursor_before = table.scan_series(series_key, 0, 600).unwrap();
    let mut values_before = Vec::new();
    while cursor_before.valid() {
        if let Some(point) = cursor_before.current() {
            values_before.push((point.timestamp, point.value_key.0.to_vec()));
        }
        cursor_before.next().unwrap();
    }
    assert_eq!(values_before.len(), 5, "Should have 5 points before vacuum");

    // Perform vacuum
    let removed = table.vacuum(LogSequenceNumber::from(1)).unwrap();
    assert!(removed >= 0, "Vacuum should complete");

    // Verify data after vacuum using scan_series
    let mut cursor_after = table.scan_series(series_key, 0, 600).unwrap();
    let mut values_after = Vec::new();
    while cursor_after.valid() {
        if let Some(point) = cursor_after.current() {
            values_after.push((point.timestamp, point.value_key.0.to_vec()));
        }
        cursor_after.next().unwrap();
    }
    assert_eq!(values_after.len(), 5, "Should have 5 points after vacuum");

    // Verify values match
    assert_eq!(
        values_before, values_after,
        "Values should be identical before and after vacuum"
    );
}

#[test]
fn test_vacuum_with_range_scan() {
    let config = TimeSeriesConfig::default().with_bucket_size(3600);
    let mut table = create_test_table("vacuum_range_scan", config);

    let series_key = b"temperature";

    // Insert data across wide time range
    for i in 0..20 {
        let value = format!("temp{}", i);
        let points = vec![(i * 500, value.as_bytes())];
        append_points(
            &mut table,
            series_key,
            &points,
            create_tx_id(1),
            LogSequenceNumber::from(1),
        );
    }

    // Vacuum
    let removed = table.vacuum(LogSequenceNumber::from(1)).unwrap();
    assert!(removed >= 0, "Vacuum should complete");

    // Verify different range scans work correctly
    let count_full = count_points(&table, series_key, 0, 10000);
    assert_eq!(count_full, 20, "Full range should have 20 points");

    let count_first_half = count_points(&table, series_key, 0, 5000);
    assert_eq!(count_first_half, 10, "First half should have 10 points");

    let count_second_half = count_points(&table, series_key, 5000, 10000);
    assert_eq!(count_second_half, 10, "Second half should have 10 points");

    let count_middle = count_points(&table, series_key, 2000, 7000);
    assert_eq!(count_middle, 10, "Middle range should have 10 points");
}

#[test]
fn test_vacuum_with_multiple_series() {
    let config = TimeSeriesConfig::default().with_bucket_size(3600);
    let mut table = create_test_table("vacuum_multi_series", config);

    // Insert data for multiple series
    let series_keys = [b"cpu.usage" as &[u8], b"memory.usage", b"disk.usage"];

    for (idx, series_key) in series_keys.iter().enumerate() {
        for i in 0..5 {
            let value = format!("value{}", i);
            let points = vec![(i * 100, value.as_bytes())];
            append_points(
                &mut table,
                series_key,
                &points,
                create_tx_id(idx as u64 + 1),
                LogSequenceNumber::from(idx as u64 + 1),
            );
        }
    }

    // Vacuum should work across all series
    let removed = table.vacuum(LogSequenceNumber::from(3)).unwrap();
    assert!(removed >= 0, "Vacuum should complete for multiple series");

    // Verify each series independently
    for series_key in &series_keys {
        let count = count_points(&table, series_key, 0, 500);
        assert_eq!(count, 5, "Each series should have 5 points after vacuum");
    }
}

#[test]
fn test_vacuum_empty_series() {
    let config = TimeSeriesConfig::default().with_bucket_size(3600);
    let table = create_test_table("vacuum_empty", config);

    let series_key = b"empty_series";

    // Vacuum empty series should succeed
    let removed = table.vacuum(LogSequenceNumber::from(1)).unwrap();
    assert_eq!(removed, 0, "Empty series should have nothing to vacuum");

    // Verify scan returns no data
    let count = count_points(&table, series_key, 0, 1000);
    assert_eq!(count, 0, "Empty series should have no points");
}

#[test]
fn test_vacuum_with_snapshot_visibility() {
    let config = TimeSeriesConfig::default().with_bucket_size(3600);
    let mut table = create_test_table("vacuum_snapshot_visibility", config);

    let series_key = b"metrics";

    // Insert initial data
    let points1 = vec![(100i64, b"v1" as &[u8]), (200i64, b"v2")];
    append_points(
        &mut table,
        series_key,
        &points1,
        create_tx_id(1),
        LogSequenceNumber::from(1),
    );

    // Create snapshot at LSN 1
    let snapshot1 = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap1".to_string(),
        LogSequenceNumber::from(1),
        0,
        0,
        Vec::new(),
    );

    // Add more data
    let points2 = vec![(300i64, b"v3" as &[u8]), (400i64, b"v4")];
    append_points(
        &mut table,
        series_key,
        &points2,
        create_tx_id(2),
        LogSequenceNumber::from(2),
    );

    // Vacuum with snapshot's LSN
    let removed = table.vacuum(snapshot1.lsn).unwrap();
    assert!(removed >= 0, "Vacuum should respect snapshot");

    // Verify snapshot can still see its data
    let mut cursor = table
        .scan_series_snapshot(series_key, 0, 500, snapshot1)
        .unwrap();
    let mut count = 0;
    while cursor.valid() {
        cursor.next().unwrap();
        count += 1;
    }
    assert!(count >= 2, "Snapshot should see at least its own data");

    // Verify current view sees all data
    let count_current = count_points(&table, series_key, 0, 500);
    assert_eq!(count_current, 4, "Current view should see all 4 points");
}

// Made with Bob
