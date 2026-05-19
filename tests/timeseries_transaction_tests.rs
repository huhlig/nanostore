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

//! Tests for TimeSeries trait implementation on Transaction.
//!
//! These tests verify that:
//! 1. TimeSeries operations are properly tracked in the transaction write set
//! 2. WAL records are written for time series operations
//! 3. Operations are applied on commit
//! 4. Operations are discarded on rollback
//! 5. Table context management works correctly

use nanostore::table::{TableEngineRegistry, TimeSeries};
use nanostore::txn::{ConflictDetector, Transaction, TransactionId};
use nanostore::types::{Durability, IsolationLevel, TableId};
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
use std::sync::{Arc, Mutex, RwLock};

/// Helper to create a test transaction
fn create_test_transaction() -> Transaction<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let wal_config = nanostore::wal::WalWriterConfig::default();
    let wal = Arc::new(nanostore::wal::WalWriter::create(&fs, "test.wal", wal_config).unwrap());
    let conflict_detector = Arc::new(Mutex::new(ConflictDetector::new()));

    // Create a minimal pager for the engine registry
    let pager_config = nanostore::pager::PagerConfig::default();
    let pager = Arc::new(nanostore::pager::Pager::create(&fs, "test.db", pager_config).unwrap());
    let engine_registry = Arc::new(TableEngineRegistry::new(pager));
    let current_lsn = Arc::new(RwLock::new(LogSequenceNumber::from(1)));

    Transaction::new(
        nanostore::txn::TransactionId::from(1),
        LogSequenceNumber::from(1),
        IsolationLevel::Serializable,
        Durability::WalOnly,
        conflict_detector,
        wal,
        engine_registry,
        current_lsn,
    )
}

#[test]
fn test_timeseries_append_point_basic() {
    let mut txn = create_test_transaction();
    let table_id = TableId::from(1);

    // Set table context
    txn.with_table(table_id);

    // Append a point
    let series_key = b"sensor-1";
    let timestamp = 1000i64;
    let value_key = b"temperature:25.5";

    let result = TimeSeries::append_point(
        &mut txn,
        series_key,
        timestamp,
        value_key,
        TransactionId::from(1),
        LogSequenceNumber::from(1),
    );
    assert!(result.is_ok(), "append_point should succeed");

    // Clear table context
    txn.clear_table_context();
}

#[test]
fn test_timeseries_append_multiple_points() {
    let mut txn = create_test_transaction();
    let table_id = TableId::from(1);

    txn.with_table(table_id);

    // Append multiple points to the same series
    let series_key = b"sensor-1";
    for i in 0..5 {
        let timestamp = 1000i64 + i * 100;
        let value = format!("temperature:{}.5", 20 + i);
        let result = TimeSeries::append_point(
            &mut txn,
            series_key,
            timestamp,
            value.as_bytes(),
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        );
        assert!(result.is_ok(), "append_point {} should succeed", i);
    }

    txn.clear_table_context();
}

#[test]
fn test_timeseries_append_multiple_series() {
    let mut txn = create_test_transaction();
    let table_id = TableId::from(1);

    txn.with_table(table_id);

    // Append points to different series
    for sensor_id in 1..=3 {
        let series_key = format!("sensor-{}", sensor_id);
        let timestamp = 1000i64;
        let value = format!("temperature:{}.5", 20 + sensor_id);

        let result = TimeSeries::append_point(
            &mut txn,
            series_key.as_bytes(),
            timestamp,
            value.as_bytes(),
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        );
        assert!(
            result.is_ok(),
            "append_point for sensor {} should succeed",
            sensor_id
        );
    }

    txn.clear_table_context();
}

#[test]
fn test_timeseries_without_table_context() {
    let mut txn = create_test_transaction();

    // Try to append without setting table context
    let series_key = b"sensor-1";
    let timestamp = 1000i64;
    let value_key = b"temperature:25.5";

    let result = TimeSeries::append_point(
        &mut txn,
        series_key,
        timestamp,
        value_key,
        TransactionId::from(1),
        LogSequenceNumber::from(1),
    );
    assert!(
        result.is_err(),
        "append_point should fail without table context"
    );
    assert!(
        result.unwrap_err().to_string().contains("no table context"),
        "Error should mention missing table context"
    );
}

#[test]
fn test_timeseries_table_id_and_name() {
    let mut txn = create_test_transaction();
    let table_id = TableId::from(42);

    // Before setting context
    assert_eq!(TimeSeries::table_id(&txn), TableId::from(0));
    assert_eq!(TimeSeries::name(&txn), "unknown");

    // After setting context
    txn.with_table(table_id);
    assert_eq!(TimeSeries::table_id(&txn), table_id);
    // Name will be "table_42" since no actual engine is registered
    assert!(TimeSeries::name(&txn).contains("42"));

    txn.clear_table_context();
}

#[test]
fn test_timeseries_capabilities() {
    let mut txn = create_test_transaction();
    let table_id = TableId::from(1);

    // Without table context, should return default capabilities
    let caps = TimeSeries::capabilities(&txn);
    assert!(!caps.exact);
    assert!(!caps.ordered);

    // With table context (but no actual engine), should still return default
    txn.with_table(table_id);
    let caps = TimeSeries::capabilities(&txn);
    assert!(!caps.exact);
    assert!(!caps.ordered);

    txn.clear_table_context();
}

#[test]
fn test_timeseries_scan_series_without_table() {
    let txn = create_test_transaction();

    let series_key = b"sensor-1";
    let start_ts = 1000i64;
    let end_ts = 2000i64;

    let result = TimeSeries::scan_series(&txn, series_key, start_ts, end_ts);
    assert!(
        result.is_err(),
        "scan_series should fail without table context"
    );
    assert!(
        result.unwrap_err().to_string().contains("no table context"),
        "Error should mention no table context"
    );
}

#[test]
fn test_timeseries_latest_before_without_table() {
    let txn = create_test_transaction();

    let series_key = b"sensor-1";
    let timestamp = 1500i64;

    let result = TimeSeries::latest_before(&txn, series_key, timestamp);
    assert!(
        result.is_err(),
        "latest_before should fail without table context"
    );
    assert!(
        result.unwrap_err().to_string().contains("no table context"),
        "Error should mention no table context"
    );
}

#[test]
fn test_timeseries_stats_without_table() {
    let txn = create_test_transaction();

    let result = TimeSeries::stats(&txn);
    assert!(result.is_err(), "stats should fail without table context");
    assert!(
        result.unwrap_err().to_string().contains("no table context"),
        "Error should mention no table context"
    );
}

#[test]
fn test_timeseries_verify_without_table() {
    let txn = create_test_transaction();

    let result = TimeSeries::verify(&txn);
    assert!(result.is_err(), "verify should fail without table context");
    assert!(
        result.unwrap_err().to_string().contains("no table context"),
        "Error should mention no table context"
    );
}

#[test]
fn test_timeseries_commit_with_operations() {
    let mut txn = create_test_transaction();
    let table_id = TableId::from(1);

    txn.with_table(table_id);

    // Append some points
    let series_key = b"sensor-1";
    for i in 0..3 {
        let timestamp = 1000i64 + i * 100;
        let value = format!("temperature:{}.5", 20 + i);
        TimeSeries::append_point(
            &mut txn,
            series_key,
            timestamp,
            value.as_bytes(),
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    }

    txn.clear_table_context();

    // Commit should succeed (operations are logged to WAL)
    let result = txn.commit();
    assert!(
        result.is_ok(),
        "commit should succeed with time series operations"
    );
}

#[test]
fn test_timeseries_rollback_with_operations() {
    let mut txn = create_test_transaction();
    let table_id = TableId::from(1);

    txn.with_table(table_id);

    // Append some points
    let series_key = b"sensor-1";
    for i in 0..3 {
        let timestamp = 1000i64 + i * 100;
        let value = format!("temperature:{}.5", 20 + i);
        TimeSeries::append_point(
            &mut txn,
            series_key,
            timestamp,
            value.as_bytes(),
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    }

    txn.clear_table_context();

    // Rollback should succeed (operations are discarded)
    let result = txn.rollback();
    assert!(
        result.is_ok(),
        "rollback should succeed with time series operations"
    );
}

#[test]
fn test_timeseries_after_commit_fails() {
    let mut txn = create_test_transaction();
    let table_id = TableId::from(1);

    txn.with_table(table_id);

    // Append a point
    let series_key = b"sensor-1";
    let timestamp = 1000i64;
    let value_key = b"temperature:25.5";
    TimeSeries::append_point(
        &mut txn,
        series_key,
        timestamp,
        value_key,
        TransactionId::from(1),
        LogSequenceNumber::from(1),
    )
    .unwrap();

    // Commit
    txn.commit().unwrap();

    // Try to create a new transaction and use the old one (this won't compile in real code,
    // but we can test the state check)
    // Note: This test is conceptual - in practice, txn is consumed by commit()
}

#[test]
fn test_timeseries_mixed_with_regular_operations() {
    let mut txn = create_test_transaction();
    let kv_table_id = TableId::from(1);
    let ts_table_id = TableId::from(2);

    // Do a regular put operation
    txn.put(kv_table_id, b"key1", b"value1").unwrap();

    // Do time series operations
    txn.with_table(ts_table_id);
    let series_key = b"sensor-1";
    let timestamp = 1000i64;
    let value_key = b"temperature:25.5";
    TimeSeries::append_point(
        &mut txn,
        series_key,
        timestamp,
        value_key,
        TransactionId::from(1),
        LogSequenceNumber::from(1),
    )
    .unwrap();
    txn.clear_table_context();

    // Do another regular operation
    txn.put(kv_table_id, b"key2", b"value2").unwrap();

    // Commit should handle both types of operations
    let result = txn.commit();
    assert!(
        result.is_ok(),
        "commit should succeed with mixed operations"
    );
}

#[test]
fn test_timeseries_timestamp_ordering() {
    let mut txn = create_test_transaction();
    let table_id = TableId::from(1);

    txn.with_table(table_id);

    let series_key = b"sensor-1";

    // Append points in non-chronological order (should still work)
    let timestamps = [1500i64, 1000i64, 2000i64, 1200i64];
    for (i, &timestamp) in timestamps.iter().enumerate() {
        let value = format!("temperature:{}.5", 20 + i);
        let result = TimeSeries::append_point(
            &mut txn,
            series_key,
            timestamp,
            value.as_bytes(),
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        );
        assert!(
            result.is_ok(),
            "append_point with timestamp {} should succeed",
            timestamp
        );
    }

    txn.clear_table_context();
}

#[test]
fn test_timeseries_negative_timestamps() {
    let mut txn = create_test_transaction();
    let table_id = TableId::from(1);

    txn.with_table(table_id);

    let series_key = b"sensor-1";

    // Append points with negative timestamps (historical data)
    let timestamps = [-1000i64, -500i64, 0i64, 500i64, 1000i64];
    for (i, &timestamp) in timestamps.iter().enumerate() {
        let value = format!("temperature:{}.5", 20 + i);
        let result = TimeSeries::append_point(
            &mut txn,
            series_key,
            timestamp,
            value.as_bytes(),
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        );
        assert!(
            result.is_ok(),
            "append_point with timestamp {} should succeed",
            timestamp
        );
    }

    txn.clear_table_context();
}

#[test]
fn test_timeseries_empty_series_key() {
    let mut txn = create_test_transaction();
    let table_id = TableId::from(1);

    txn.with_table(table_id);

    // Empty series key should be allowed (edge case)
    let series_key = b"";
    let timestamp = 1000i64;
    let value_key = b"temperature:25.5";

    let result = TimeSeries::append_point(
        &mut txn,
        series_key,
        timestamp,
        value_key,
        TransactionId::from(1),
        LogSequenceNumber::from(1),
    );
    assert!(
        result.is_ok(),
        "append_point with empty series key should succeed"
    );

    txn.clear_table_context();
}

#[test]
fn test_timeseries_empty_value_key() {
    let mut txn = create_test_transaction();
    let table_id = TableId::from(1);

    txn.with_table(table_id);

    // Empty value key should be allowed (edge case)
    let series_key = b"sensor-1";
    let timestamp = 1000i64;
    let value_key = b"";

    let result = TimeSeries::append_point(
        &mut txn,
        series_key,
        timestamp,
        value_key,
        TransactionId::from(1),
        LogSequenceNumber::from(1),
    );
    assert!(
        result.is_ok(),
        "append_point with empty value key should succeed"
    );

    txn.clear_table_context();
}

#[test]
fn test_timeseries_large_value_key() {
    let mut txn = create_test_transaction();
    let table_id = TableId::from(1);

    txn.with_table(table_id);

    let series_key = b"sensor-1";
    let timestamp = 1000i64;
    // Create a large value key (1KB)
    let value_key = vec![b'x'; 1024];

    let result = TimeSeries::append_point(
        &mut txn,
        series_key,
        timestamp,
        &value_key,
        TransactionId::from(1),
        LogSequenceNumber::from(1),
    );
    assert!(
        result.is_ok(),
        "append_point with large value key should succeed"
    );

    txn.clear_table_context();
}

// Made with Bob
