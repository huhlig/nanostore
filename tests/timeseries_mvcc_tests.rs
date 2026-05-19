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

//! MVCC tests for TimeSeriesTable.

use nanostore::pager::{Pager, PagerConfig};
use nanostore::snap::Snapshot;
use nanostore::table::TimeSeries;
use nanostore::table::timeseries::{TimeSeriesConfig, TimeSeriesTable};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
use std::sync::Arc;

#[test]
fn test_timeseries_mvcc_snapshot_isolation() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = TimeSeriesConfig::default();
    let table =
        TimeSeriesTable::new(TableId::from(1), "test_series".to_string(), pager, config).unwrap();

    let series_key = b"sensor1";
    let tx_id1 = TransactionId::from(1);
    let tx_id2 = TransactionId::from(2);

    // Transaction 1: Write initial value at timestamp 1000
    table
        .append_point_tx(series_key, 1000, b"value1", tx_id1)
        .unwrap();

    // Commit transaction 1 at LSN 10
    table
        .commit_versions(tx_id1, LogSequenceNumber::from(10))
        .unwrap();

    // Create snapshot at LSN 10 (should see value1)
    let snapshot1 = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap1".to_string(),
        LogSequenceNumber::from(10),
        0,
        0,
        Vec::new(),
    );

    // Transaction 2: Update value at timestamp 1000
    table
        .append_point_tx(series_key, 1000, b"value2", tx_id2)
        .unwrap();

    // Commit transaction 2 at LSN 20
    table
        .commit_versions(tx_id2, LogSequenceNumber::from(20))
        .unwrap();

    // Snapshot 1 should still see value1 (snapshot isolation)
    let result1 = table
        .latest_before_snapshot(series_key, 1000, &snapshot1)
        .unwrap();
    assert!(result1.is_some());
    let point1 = result1.unwrap();
    assert_eq!(point1.value_key.0, b"value1");

    // Create new snapshot at LSN 20 (should see value2)
    let snapshot2 = Snapshot::new(
        nanostore::snap::SnapshotId::from(2),
        "snap2".to_string(),
        LogSequenceNumber::from(20),
        0,
        0,
        Vec::new(),
    );

    let result2 = table
        .latest_before_snapshot(series_key, 1000, &snapshot2)
        .unwrap();
    assert!(result2.is_some());
    let point2 = result2.unwrap();
    assert_eq!(point2.value_key.0, b"value2");
}

#[test]
fn test_timeseries_mvcc_multiple_versions() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = TimeSeriesConfig::default();
    let table =
        TimeSeriesTable::new(TableId::from(1), "test_series".to_string(), pager, config).unwrap();

    let series_key = b"sensor1";

    // Create multiple versions of the same timestamp
    for i in 1..=5 {
        let tx_id = TransactionId::from(i);
        let value = format!("value{}", i);
        table
            .append_point_tx(series_key, 1000, value.as_bytes(), tx_id)
            .unwrap();
        table
            .commit_versions(tx_id, LogSequenceNumber::from(i * 10))
            .unwrap();
    }

    // Each snapshot should see its corresponding version
    for i in 1..=5 {
        let snapshot = Snapshot::new(
            nanostore::snap::SnapshotId::from(i),
            format!("snap{}", i),
            LogSequenceNumber::from(i * 10),
            0,
            0,
            Vec::new(),
        );

        let result = table
            .latest_before_snapshot(series_key, 1000, &snapshot)
            .unwrap();
        assert!(result.is_some());
        let point = result.unwrap();
        let expected_value = format!("value{}", i);
        assert_eq!(point.value_key.0, expected_value.as_bytes());
    }
}

#[test]
fn test_timeseries_mvcc_scan_with_snapshot() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = TimeSeriesConfig::default();
    let table =
        TimeSeriesTable::new(TableId::from(1), "test_series".to_string(), pager, config).unwrap();

    let series_key = b"sensor1";
    let tx_id1 = TransactionId::from(1);
    let tx_id2 = TransactionId::from(2);

    // Transaction 1: Write values at timestamps 1000, 2000, 3000
    table
        .append_point_tx(series_key, 1000, b"v1_1000", tx_id1)
        .unwrap();
    table
        .append_point_tx(series_key, 2000, b"v1_2000", tx_id1)
        .unwrap();
    table
        .append_point_tx(series_key, 3000, b"v1_3000", tx_id1)
        .unwrap();
    table
        .commit_versions(tx_id1, LogSequenceNumber::from(10))
        .unwrap();

    // Create snapshot at LSN 10
    let snapshot1 = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap1".to_string(),
        LogSequenceNumber::from(10),
        0,
        0,
        Vec::new(),
    );

    // Transaction 2: Update value at timestamp 2000
    table
        .append_point_tx(series_key, 2000, b"v2_2000", tx_id2)
        .unwrap();
    table
        .commit_versions(tx_id2, LogSequenceNumber::from(20))
        .unwrap();

    // Scan with snapshot1 should see original values
    let cursor1 = table
        .scan_series_snapshot(series_key, 1000, 4000, snapshot1)
        .unwrap();
    use nanostore::table::TimeSeriesCursor;

    let mut points1 = Vec::new();
    let mut cursor1 = cursor1;
    while cursor1.valid() {
        if let Some(point) = cursor1.current() {
            points1.push((point.timestamp, point.value_key.0.clone()));
        }
        cursor1.next().unwrap();
    }

    assert_eq!(points1.len(), 3);
    assert_eq!(points1[0], (1000, b"v1_1000".to_vec()));
    assert_eq!(points1[1], (2000, b"v1_2000".to_vec()));
    assert_eq!(points1[2], (3000, b"v1_3000".to_vec()));

    // Scan with new snapshot should see updated value
    let snapshot2 = Snapshot::new(
        nanostore::snap::SnapshotId::from(2),
        "snap2".to_string(),
        LogSequenceNumber::from(20),
        0,
        0,
        Vec::new(),
    );

    let cursor2 = table
        .scan_series_snapshot(series_key, 1000, 4000, snapshot2)
        .unwrap();

    let mut points2 = Vec::new();
    let mut cursor2 = cursor2;
    while cursor2.valid() {
        if let Some(point) = cursor2.current() {
            points2.push((point.timestamp, point.value_key.0.clone()));
        }
        cursor2.next().unwrap();
    }

    assert_eq!(points2.len(), 3);
    assert_eq!(points2[0], (1000, b"v1_1000".to_vec()));
    assert_eq!(points2[1], (2000, b"v2_2000".to_vec())); // Updated value
    assert_eq!(points2[2], (3000, b"v1_3000".to_vec()));
}

#[test]
fn test_timeseries_mvcc_vacuum() {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());

    let config = TimeSeriesConfig::default();
    let table =
        TimeSeriesTable::new(TableId::from(1), "test_series".to_string(), pager, config).unwrap();

    let series_key = b"sensor1";

    // Create multiple versions
    for i in 1..=5 {
        let tx_id = TransactionId::from(i);
        let value = format!("value{}", i);
        table
            .append_point_tx(series_key, 1000, value.as_bytes(), tx_id)
            .unwrap();
        table
            .commit_versions(tx_id, LogSequenceNumber::from(i * 10))
            .unwrap();
    }

    // Vacuum old versions (keep only versions >= LSN 40)
    let removed = table.vacuum(LogSequenceNumber::from(40)).unwrap();

    // Should have removed some old versions (keeping at least one base version)
    assert!(removed > 0);

    // Latest version should still be accessible
    let snapshot = Snapshot::new(
        nanostore::snap::SnapshotId::from(1),
        "snap".to_string(),
        LogSequenceNumber::from(50),
        0,
        0,
        Vec::new(),
    );

    let result = table
        .latest_before_snapshot(series_key, 1000, &snapshot)
        .unwrap();
    assert!(result.is_some());
    let point = result.unwrap();
    assert_eq!(point.value_key.0, b"value5");
}

// Made with Bob
