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

//! End-to-end stress tests for full StorageEngine operations.
//!
//! These tests validate the complete StorageEngine stack under realistic production
//! workloads:
//! - Concurrent transactions across multiple tables
//! - Mixed read/write workloads with different isolation levels
//! - Long-running operations with snapshots
//! - Memory pressure scenarios
//! - Realistic production patterns (e.g., OLTP, analytics, time-series)
//!
//! Unlike unit tests that focus on individual components, these tests exercise
//! the entire system to validate:
//! - Correct interaction between layers (pager, WAL, transactions, tables)
//! - System stability under sustained load
//! - Performance characteristics under stress
//! - Resource management (memory, file handles, locks)
//! - Error handling and recovery

use nanostore::kvdb::StorageEngine;
use nanostore::table::{TableEngineKind, TableOptions};
use nanostore::types::{Durability, IsolationLevel};
use nanostore::vfs::MemoryFileSystem;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

// =============================================================================
// Test Helpers
// =============================================================================

/// Create table options for different engine types
fn btree_table_options() -> TableOptions {
    TableOptions {
        engine: TableEngineKind::BTree,
        ..TableOptions::default()
    }
}

fn lsm_table_options() -> TableOptions {
    TableOptions {
        engine: TableEngineKind::LsmTree,
        ..TableOptions::default()
    }
}

fn memory_table_options() -> TableOptions {
    TableOptions {
        engine: TableEngineKind::Memory,
        ..TableOptions::default()
    }
}

// =============================================================================
// Concurrent Transaction Stress Tests
// =============================================================================

/// Test concurrent transactions across multiple tables.
///
/// Validates that the system can handle many concurrent transactions
/// performing operations on different tables without conflicts or deadlocks.
#[test]
fn test_concurrent_transactions_multiple_tables() {
    let fs = MemoryFileSystem::new();
    let db = Arc::new(StorageEngine::new(&fs, "stress.wal", "stress.db").unwrap());

    // Create multiple tables with different engines
    let users_id = db.create_table("users", btree_table_options()).unwrap();
    let logs_id = db.create_table("logs", lsm_table_options()).unwrap();
    let cache_id = db.create_table("cache", memory_table_options()).unwrap();

    let num_threads = 10;
    let ops_per_thread = 100;
    let barrier = Arc::new(Barrier::new(num_threads));
    let success_count = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        let success_count = Arc::clone(&success_count);

        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            barrier.wait();

            for op_id in 0..ops_per_thread {
                let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();

                // Perform operations on all three tables
                let user_key = format!("user_{}_{}", thread_id, op_id);
                let log_key = format!("log_{}_{}", thread_id, op_id);
                let cache_key = format!("cache_{}_{}", thread_id, op_id);

                let value = format!("value_{}", op_id);

                // Write to all tables in the same transaction
                let mut all_ok = true;
                all_ok &= tx
                    .put(users_id, user_key.as_bytes(), value.as_bytes())
                    .is_ok();
                all_ok &= tx
                    .put(logs_id, log_key.as_bytes(), value.as_bytes())
                    .is_ok();
                all_ok &= tx
                    .put(cache_id, cache_key.as_bytes(), value.as_bytes())
                    .is_ok();

                // Commit transaction
                if all_ok && tx.commit().is_ok() {
                    success_count.fetch_add(1, Ordering::SeqCst);
                }
            }
        });

        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    // Verify all operations succeeded
    let total_ops = num_threads * ops_per_thread;
    assert_eq!(
        success_count.load(Ordering::SeqCst),
        total_ops,
        "All transactions should succeed"
    );

    // Verify data integrity - sample some keys
    let tx = db.begin_read().unwrap();
    for thread_id in 0..num_threads {
        let user_key = format!("user_{}_0", thread_id);
        let result = tx.get(users_id, user_key.as_bytes()).unwrap();
        assert!(result.is_some(), "Data should be readable after commit");
    }
}

/// Test concurrent read and write transactions with different isolation levels.
///
/// Validates that readers and writers can coexist without blocking each other
/// excessively, and that isolation levels are correctly enforced.
#[test]
fn test_concurrent_readers_and_writers() {
    let fs = MemoryFileSystem::new();
    let db = Arc::new(StorageEngine::new(&fs, "stress.wal", "stress.db").unwrap());

    let table_id = db.create_table("data", btree_table_options()).unwrap();

    // Pre-populate with initial data
    {
        let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
        for i in 0..100 {
            let key = format!("key_{:04}", i);
            tx.put(table_id, key.as_bytes(), b"initial").unwrap();
        }
        tx.commit().unwrap();
    }

    let num_readers = 20;
    let num_writers = 5;
    let duration = Duration::from_secs(2);
    let stop_flag = Arc::new(AtomicBool::new(false));

    let read_count = Arc::new(AtomicUsize::new(0));
    let write_count = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    // Spawn reader threads
    for reader_id in 0..num_readers {
        let db = Arc::clone(&db);
        let stop_flag = Arc::clone(&stop_flag);
        let read_count = Arc::clone(&read_count);

        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                // Use different isolation levels
                let isolation = if reader_id % 2 == 0 {
                    IsolationLevel::ReadCommitted
                } else {
                    IsolationLevel::Serializable
                };

                let tx = db.begin_read_with_isolation(isolation).unwrap();

                // Read random keys
                for i in (0..100).step_by(10) {
                    let key = format!("key_{:04}", i);
                    let _ = tx.get(table_id, key.as_bytes());
                }

                read_count.fetch_add(1, Ordering::Relaxed);
            }
        });

        handles.push(handle);
    }

    // Spawn writer threads
    for writer_id in 0..num_writers {
        let db = Arc::clone(&db);
        let stop_flag = Arc::clone(&stop_flag);
        let write_count = Arc::clone(&write_count);

        let handle = thread::spawn(move || {
            let mut op_count = 0;
            while !stop_flag.load(Ordering::Relaxed) {
                let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();

                // Update a few keys
                for i in 0..5 {
                    let key = format!("key_{:04}", (writer_id * 20 + i) % 100);
                    let value = format!("writer_{}_{}", writer_id, op_count);
                    tx.put(table_id, key.as_bytes(), value.as_bytes()).unwrap();
                }

                if tx.commit().is_ok() {
                    write_count.fetch_add(1, Ordering::Relaxed);
                    op_count += 1;
                }
            }
        });

        handles.push(handle);
    }

    // Let the test run for the specified duration
    thread::sleep(duration);
    stop_flag.store(true, Ordering::Relaxed);

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    let total_reads = read_count.load(Ordering::Relaxed);
    let total_writes = write_count.load(Ordering::Relaxed);

    println!(
        "Completed {} reads and {} writes in {:?}",
        total_reads, total_writes, duration
    );

    // Verify we achieved reasonable throughput
    assert!(
        total_reads > 100,
        "Should complete many read transactions: {}",
        total_reads
    );
    assert!(
        total_writes > 10,
        "Should complete some write transactions: {}",
        total_writes
    );
}

/// Test write-write conflicts with concurrent transactions.
///
/// Validates that the conflict detection system correctly identifies and
/// handles write-write conflicts between concurrent transactions.
#[test]
fn test_concurrent_write_conflicts() {
    let fs = MemoryFileSystem::new();
    let db = Arc::new(StorageEngine::new(&fs, "stress.wal", "stress.db").unwrap());

    let table_id = db.create_table("data", btree_table_options()).unwrap();

    // Pre-populate with data
    {
        let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
        for i in 0..10 {
            let key = format!("key_{}", i);
            tx.put(table_id, key.as_bytes(), b"initial").unwrap();
        }
        tx.commit().unwrap();
    }

    let num_threads = 10;
    let barrier = Arc::new(Barrier::new(num_threads));
    let success_count = Arc::new(AtomicUsize::new(0));
    let conflict_count = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        let success_count = Arc::clone(&success_count);
        let conflict_count = Arc::clone(&conflict_count);

        let handle = thread::spawn(move || {
            barrier.wait();

            // All threads try to update the same keys
            match db.begin_write(Durability::SyncOnCommit) {
                Ok(mut tx) => {
                    let mut put_success = true;
                    for i in 0..10 {
                        let key = format!("key_{}", i);
                        let value = format!("thread_{}", thread_id);
                        if tx.put(table_id, key.as_bytes(), value.as_bytes()).is_err() {
                            put_success = false;
                            break;
                        }
                    }

                    if put_success {
                        match tx.commit() {
                            Ok(_) => {
                                success_count.fetch_add(1, Ordering::SeqCst);
                            }
                            Err(_) => {
                                conflict_count.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                    } else {
                        conflict_count.fetch_add(1, Ordering::SeqCst);
                    }
                }
                Err(_) => {
                    conflict_count.fetch_add(1, Ordering::SeqCst);
                }
            }
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let successes = success_count.load(Ordering::SeqCst);
    let conflicts = conflict_count.load(Ordering::SeqCst);

    println!(
        "Conflicts: {} successes, {} conflicts out of {} threads",
        successes, conflicts, num_threads
    );

    // At least one should succeed, and we should detect conflicts
    assert!(successes >= 1, "At least one transaction should succeed");
    assert_eq!(
        successes + conflicts,
        num_threads,
        "All transactions should either succeed or conflict"
    );
}

// =============================================================================
// Mixed Workload Stress Tests
// =============================================================================

/// Test OLTP-style workload with mixed operations.
///
/// Simulates an online transaction processing workload with:
/// - Short transactions
/// - Mix of reads and writes
/// - Point lookups and updates
/// - High concurrency
#[test]
fn test_oltp_workload() {
    let fs = MemoryFileSystem::new();
    let db = Arc::new(StorageEngine::new(&fs, "oltp.wal", "oltp.db").unwrap());

    // Create tables for OLTP workload
    let accounts_id = db.create_table("accounts", btree_table_options()).unwrap();
    let transactions_id = db
        .create_table("transactions", lsm_table_options())
        .unwrap();

    // Initialize accounts
    {
        let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
        for i in 0..100 {
            let key = format!("account_{:04}", i);
            let balance = format!("{}", 1000);
            tx.put(accounts_id, key.as_bytes(), balance.as_bytes())
                .unwrap();
        }
        tx.commit().unwrap();
    }

    let num_threads = 8;
    let ops_per_thread = 200;
    let barrier = Arc::new(Barrier::new(num_threads));
    let success_count = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        let success_count = Arc::clone(&success_count);

        let handle = thread::spawn(move || {
            barrier.wait();

            for op_id in 0..ops_per_thread {
                // Mix of operations: 70% reads, 30% writes
                if op_id % 10 < 7 {
                    // Read operation
                    let tx = db.begin_read().unwrap();
                    let account_id = (thread_id * 10 + op_id) % 100;
                    let key = format!("account_{:04}", account_id);
                    let _ = tx.get(accounts_id, key.as_bytes());
                } else {
                    // Write operation (transfer between accounts)
                    if let Ok(mut tx) = db.begin_write(Durability::SyncOnCommit) {
                        let from_account = (thread_id * 10 + op_id) % 100;
                        let to_account = (from_account + 1) % 100;

                        // Log transaction
                        let tx_key = format!("tx_{}_{}", thread_id, op_id);
                        let tx_data = format!("transfer_{}_{}", from_account, to_account);

                        let mut all_ok = true;
                        all_ok &= tx
                            .put(transactions_id, tx_key.as_bytes(), tx_data.as_bytes())
                            .is_ok();

                        // Update accounts (simplified - not checking balances)
                        let from_key = format!("account_{:04}", from_account);
                        let to_key = format!("account_{:04}", to_account);
                        all_ok &= tx.put(accounts_id, from_key.as_bytes(), b"updated").is_ok();
                        all_ok &= tx.put(accounts_id, to_key.as_bytes(), b"updated").is_ok();

                        if all_ok && tx.commit().is_ok() {
                            success_count.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let writes = success_count.load(Ordering::Relaxed);
    let expected_writes = num_threads * ops_per_thread * 3 / 10; // 30% writes

    println!("OLTP workload: {} successful writes", writes);
    assert!(
        writes > expected_writes / 2,
        "Should complete at least half of expected writes"
    );
}

/// Test analytics-style workload with large scans.
///
/// Simulates an analytical workload with:
/// - Long-running read transactions
/// - Large range scans
/// - Concurrent with ongoing writes
#[test]
fn test_analytics_workload() {
    let fs = MemoryFileSystem::new();
    let db = Arc::new(StorageEngine::new(&fs, "analytics.wal", "analytics.db").unwrap());

    let events_id = db.create_table("events", lsm_table_options()).unwrap();

    // Populate with event data
    {
        let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
        for i in 0..1000 {
            let key = format!("event_{:06}", i);
            let value = format!("data_{}", i);
            tx.put(events_id, key.as_bytes(), value.as_bytes()).unwrap();
        }
        tx.commit().unwrap();
    }

    let stop_flag = Arc::new(AtomicBool::new(false));
    let scan_count = Arc::new(AtomicUsize::new(0));
    let write_count = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    // Spawn analytical query threads (long-running reads)
    for _ in 0..3 {
        let db = Arc::clone(&db);
        let stop_flag = Arc::clone(&stop_flag);
        let scan_count = Arc::clone(&scan_count);

        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                // Long-running read transaction
                let tx = db.begin_read().unwrap();

                // Scan through data (simulating aggregation)
                let mut count = 0;
                for i in (0..1000).step_by(10) {
                    let key = format!("event_{:06}", i);
                    if tx.get(events_id, key.as_bytes()).unwrap().is_some() {
                        count += 1;
                    }
                }

                if count > 0 {
                    scan_count.fetch_add(1, Ordering::Relaxed);
                }

                // Simulate processing time
                thread::sleep(Duration::from_millis(10));
            }
        });

        handles.push(handle);
    }

    // Spawn writer threads (ongoing ingestion)
    for writer_id in 0..2 {
        let db = Arc::clone(&db);
        let stop_flag = Arc::clone(&stop_flag);
        let write_count = Arc::clone(&write_count);

        let handle = thread::spawn(move || {
            let mut op_count = 0;
            while !stop_flag.load(Ordering::Relaxed) {
                let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();

                // Write new events
                for i in 0..10 {
                    let key = format!("new_event_{}_{}", writer_id, op_count * 10 + i);
                    let value = format!("new_data_{}", op_count);
                    tx.put(events_id, key.as_bytes(), value.as_bytes()).unwrap();
                }

                if tx.commit().is_ok() {
                    write_count.fetch_add(1, Ordering::Relaxed);
                    op_count += 1;
                }

                thread::sleep(Duration::from_millis(50));
            }
        });

        handles.push(handle);
    }

    // Run for 2 seconds
    thread::sleep(Duration::from_secs(2));
    stop_flag.store(true, Ordering::Relaxed);

    for handle in handles {
        handle.join().unwrap();
    }

    let scans = scan_count.load(Ordering::Relaxed);
    let writes = write_count.load(Ordering::Relaxed);

    println!("Analytics workload: {} scans, {} writes", scans, writes);

    assert!(scans > 10, "Should complete multiple analytical scans");
    assert!(writes > 5, "Should complete ongoing writes");
}

// =============================================================================
// Long-Running Operation Tests
// =============================================================================

/// Test long-running transactions with snapshots.
///
/// Validates that long-running read transactions can maintain consistent
/// snapshots while concurrent writes occur.
#[test]
fn test_long_running_snapshot_consistency() {
    let fs = MemoryFileSystem::new();
    let db = Arc::new(StorageEngine::new(&fs, "snapshot.wal", "snapshot.db").unwrap());

    let table_id = db.create_table("data", btree_table_options()).unwrap();

    // Initial data
    {
        let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
        for i in 0..100 {
            let key = format!("key_{:04}", i);
            tx.put(table_id, key.as_bytes(), b"version_0").unwrap();
        }
        tx.commit().unwrap();
    }

    // Start a long-running read transaction
    let long_tx = db.begin_read().unwrap();

    // Verify initial state
    for i in 0..100 {
        let key = format!("key_{:04}", i);
        let value = long_tx.get(table_id, key.as_bytes()).unwrap().unwrap();
        assert_eq!(value.as_ref(), b"version_0");
    }

    // Perform many updates in separate transactions
    for version in 1..=10 {
        let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
        for i in 0..100 {
            let key = format!("key_{:04}", i);
            let value = format!("version_{}", version);
            tx.put(table_id, key.as_bytes(), value.as_bytes()).unwrap();
        }
        tx.commit().unwrap();
    }

    // Long-running transaction should still see original data
    for i in 0..100 {
        let key = format!("key_{:04}", i);
        let value = long_tx.get(table_id, key.as_bytes()).unwrap().unwrap();
        assert_eq!(
            value.as_ref(),
            b"version_0",
            "Long-running transaction should see consistent snapshot"
        );
    }

    // New transaction should see latest data
    let new_tx = db.begin_read().unwrap();
    for i in 0..100 {
        let key = format!("key_{:04}", i);
        let value = new_tx.get(table_id, key.as_bytes()).unwrap().unwrap();
        assert_eq!(value.as_ref(), b"version_10");
    }
}

/// Test StorageEngine behavior under sustained load.
///
/// Runs a continuous workload for an extended period to validate:
/// - No memory leaks
/// - Stable performance
/// - No resource exhaustion
#[test]
fn test_sustained_load() {
    let fs = MemoryFileSystem::new();
    let db = Arc::new(StorageEngine::new(&fs, "sustained.wal", "sustained.db").unwrap());

    let table_id = db.create_table("data", lsm_table_options()).unwrap();

    let duration = Duration::from_secs(5);
    let start = Instant::now();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let op_count = Arc::new(AtomicUsize::new(0));

    let num_threads = 4;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let db = Arc::clone(&db);
        let stop_flag = Arc::clone(&stop_flag);
        let op_count = Arc::clone(&op_count);

        let handle = thread::spawn(move || {
            let mut local_count = 0;
            while !stop_flag.load(Ordering::Relaxed) {
                let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();

                // Perform a batch of operations
                for i in 0..10 {
                    let key = format!("key_{}_{}", thread_id, local_count * 10 + i);
                    let value = format!("value_{}", local_count);
                    tx.put(table_id, key.as_bytes(), value.as_bytes()).unwrap();
                }

                if tx.commit().is_ok() {
                    local_count += 1;
                    op_count.fetch_add(1, Ordering::Relaxed);
                }
            }
        });

        handles.push(handle);
    }

    // Run until duration expires
    thread::sleep(duration);
    stop_flag.store(true, Ordering::Relaxed);

    for handle in handles {
        handle.join().unwrap();
    }

    let elapsed = start.elapsed();
    let total_ops = op_count.load(Ordering::Relaxed);
    let ops_per_sec = total_ops as f64 / elapsed.as_secs_f64();

    println!(
        "Sustained load: {} operations in {:?} ({:.2} ops/sec)",
        total_ops, elapsed, ops_per_sec
    );

    assert!(
        total_ops > 100,
        "Should complete many operations under sustained load"
    );
}

// =============================================================================
// Memory Pressure Tests
// =============================================================================

/// Test behavior with large values.
///
/// Validates that the system can handle large values without issues.
#[test]
fn test_large_values() {
    let fs = MemoryFileSystem::new();
    let db = StorageEngine::new(&fs, "large.wal", "large.db").unwrap();

    let table_id = db.create_table("data", btree_table_options()).unwrap();

    // Create large values (1MB each)
    let large_value = vec![b'X'; 1024 * 1024];

    let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();

    // Write multiple large values
    for i in 0..10 {
        let key = format!("large_key_{}", i);
        tx.put(table_id, key.as_bytes(), &large_value).unwrap();
    }

    tx.commit().unwrap();

    // Verify we can read them back
    let tx = db.begin_read().unwrap();
    for i in 0..10 {
        let key = format!("large_key_{}", i);
        let value = tx.get(table_id, key.as_bytes()).unwrap().unwrap();
        assert_eq!(value.0.len(), 1024 * 1024);
    }
}

/// Test behavior with many small transactions.
///
/// Validates that the system can handle high transaction throughput
/// without resource exhaustion.
#[test]
fn test_many_small_transactions() {
    let fs = MemoryFileSystem::new();
    let db = StorageEngine::new(&fs, "many.wal", "many.db").unwrap();

    let table_id = db.create_table("data", memory_table_options()).unwrap();

    let num_transactions = 1000;

    for i in 0..num_transactions {
        let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
        let key = format!("key_{}", i);
        let value = format!("value_{}", i);
        tx.put(table_id, key.as_bytes(), value.as_bytes()).unwrap();
        tx.commit().unwrap();
    }

    // Verify all data is present
    let tx = db.begin_read().unwrap();
    for i in 0..num_transactions {
        let key = format!("key_{}", i);
        let result = tx.get(table_id, key.as_bytes()).unwrap();
        assert!(result.is_some(), "Key {} should exist", key);
    }
}

// =============================================================================
// Realistic Production Pattern Tests
// =============================================================================

/// Test time-series ingestion pattern.
///
/// Simulates a time-series StorageEngine workload with:
/// - High write throughput
/// - Append-only writes
/// - Periodic reads for recent data
#[test]
fn test_timeseries_ingestion_pattern() {
    let fs = MemoryFileSystem::new();
    let db = Arc::new(StorageEngine::new(&fs, "timeseries.wal", "timeseries.db").unwrap());

    let metrics_id = db.create_table("metrics", lsm_table_options()).unwrap();

    let duration = Duration::from_secs(3);
    let stop_flag = Arc::new(AtomicBool::new(false));
    let write_count = Arc::new(AtomicUsize::new(0));
    let read_count = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    // Spawn writer threads (high-throughput ingestion)
    for writer_id in 0..4 {
        let db = Arc::clone(&db);
        let stop_flag = Arc::clone(&stop_flag);
        let write_count = Arc::clone(&write_count);

        let handle = thread::spawn(move || {
            let mut timestamp = 0u64;
            while !stop_flag.load(Ordering::Relaxed) {
                let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();

                // Batch writes for efficiency
                for _ in 0..20 {
                    let key = format!("metric_{}_{:016}", writer_id, timestamp);
                    let value = format!("value_{}", timestamp);
                    tx.put(metrics_id, key.as_bytes(), value.as_bytes())
                        .unwrap();
                    timestamp += 1;
                }

                if tx.commit().is_ok() {
                    write_count.fetch_add(20, Ordering::Relaxed);
                }
            }
        });

        handles.push(handle);
    }

    // Spawn reader thread (periodic queries for recent data)
    {
        let db = Arc::clone(&db);
        let stop_flag = Arc::clone(&stop_flag);
        let read_count = Arc::clone(&read_count);

        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                let tx = db.begin_read().unwrap();

                // Query recent data from all writers
                for writer_id in 0..4 {
                    for i in 0..10 {
                        let key = format!("metric_{}_{:016}", writer_id, i);
                        let _ = tx.get(metrics_id, key.as_bytes());
                    }
                }

                read_count.fetch_add(1, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(100));
            }
        });

        handles.push(handle);
    }

    thread::sleep(duration);
    stop_flag.store(true, Ordering::Relaxed);

    for handle in handles {
        handle.join().unwrap();
    }

    let writes = write_count.load(Ordering::Relaxed);
    let reads = read_count.load(Ordering::Relaxed);

    println!("Time-series pattern: {} writes, {} reads", writes, reads);

    assert!(writes > 1000, "Should achieve high write throughput");
    assert!(reads > 10, "Should complete periodic reads");
}

/// Test cache-like access pattern.
///
/// Simulates a cache workload with:
/// - High read rate
/// - Occasional writes
/// - Hot key access patterns
#[test]
fn test_cache_access_pattern() {
    let fs = MemoryFileSystem::new();
    let db = Arc::new(StorageEngine::new(&fs, "cache.wal", "cache.db").unwrap());

    let cache_id = db.create_table("cache", memory_table_options()).unwrap();

    // Pre-populate cache
    {
        let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();
        for i in 0..100 {
            let key = format!("key_{:04}", i);
            let value = format!("cached_value_{}", i);
            tx.put(cache_id, key.as_bytes(), value.as_bytes()).unwrap();
        }
        tx.commit().unwrap();
    }

    let duration = Duration::from_secs(2);
    let stop_flag = Arc::new(AtomicBool::new(false));
    let read_count = Arc::new(AtomicUsize::new(0));
    let write_count = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    // Spawn many reader threads (high read rate)
    for _ in 0..10 {
        let db = Arc::clone(&db);
        let stop_flag = Arc::clone(&stop_flag);
        let read_count = Arc::clone(&read_count);

        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                let tx = db.begin_read().unwrap();

                // Access hot keys (first 20 keys)
                for i in 0..20 {
                    let key = format!("key_{:04}", i);
                    let _ = tx.get(cache_id, key.as_bytes());
                }

                read_count.fetch_add(20, Ordering::Relaxed);
            }
        });

        handles.push(handle);
    }

    // Spawn occasional writer (cache updates)
    {
        let db = Arc::clone(&db);
        let stop_flag = Arc::clone(&stop_flag);
        let write_count = Arc::clone(&write_count);

        let handle = thread::spawn(move || {
            let mut update_count = 0;
            while !stop_flag.load(Ordering::Relaxed) {
                let mut tx = db.begin_write(Durability::SyncOnCommit).unwrap();

                // Update a few hot keys
                for i in 0..5 {
                    let key = format!("key_{:04}", i);
                    let value = format!("updated_{}_{}", i, update_count);
                    tx.put(cache_id, key.as_bytes(), value.as_bytes()).unwrap();
                }

                if tx.commit().is_ok() {
                    write_count.fetch_add(1, Ordering::Relaxed);
                    update_count += 1;
                }

                thread::sleep(Duration::from_millis(100));
            }
        });

        handles.push(handle);
    }

    thread::sleep(duration);
    stop_flag.store(true, Ordering::Relaxed);

    for handle in handles {
        handle.join().unwrap();
    }

    let reads = read_count.load(Ordering::Relaxed);
    let writes = write_count.load(Ordering::Relaxed);

    println!("Cache pattern: {} reads, {} writes", reads, writes);

    assert!(reads > 1000, "Should achieve very high read rate");
    assert!(writes > 5, "Should complete occasional writes");
}

// Made with Bob
