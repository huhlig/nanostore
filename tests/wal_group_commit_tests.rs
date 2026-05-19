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

//! Tests for WAL group commit functionality

use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::{GroupCommitConfig, WalWriter, WalWriterConfig, WriteOpType};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_group_commit_disabled() {
    let fs = MemoryFileSystem::new();
    let mut config = WalWriterConfig::default();
    config.group_commit.enabled = false;

    let writer = WalWriter::create(&fs, "test.wal", config).unwrap();
    assert!(!writer.is_group_commit_enabled());

    // Should work normally without group commit
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key".to_vec(),
            b"value".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();
}

#[test]
fn test_group_commit_enabled() {
    let fs = MemoryFileSystem::new();
    let mut config = WalWriterConfig::default();
    config.group_commit.enabled = true;
    config.group_commit.max_batch_size = 10;
    config.group_commit.max_wait_micros = 1000;

    let writer = WalWriter::create(&fs, "test.wal", config).unwrap();
    assert!(writer.is_group_commit_enabled());

    // Should work with group commit
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key".to_vec(),
            b"value".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();

    // Check metrics
    if let Some(metrics) = writer.group_commit_metrics() {
        assert!(
            metrics
                .total_commits
                .load(std::sync::atomic::Ordering::Relaxed)
                > 0
        );
    }
}

#[test]
fn test_group_commit_single_transaction() {
    let fs = MemoryFileSystem::new();
    let mut config = WalWriterConfig::default();
    config.group_commit = GroupCommitConfig::high_throughput();

    let writer = WalWriter::create(&fs, "test.wal", config).unwrap();

    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"user:1".to_vec(),
            b"Alice".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();

    // Verify metrics
    if let Some(metrics) = writer.group_commit_metrics() {
        let commits = metrics
            .total_commits
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(commits, 1);
    }
}

#[test]
fn test_group_commit_multiple_transactions_sequential() {
    let fs = MemoryFileSystem::new();
    let mut config = WalWriterConfig::default();
    config.group_commit = GroupCommitConfig::high_throughput();

    let writer = WalWriter::create(&fs, "test.wal", config).unwrap();

    // Write multiple transactions sequentially
    for i in 1..=5 {
        let txn_id = TransactionId::from(i);
        writer.write_begin(txn_id).unwrap();
        writer
            .write_operation(
                txn_id,
                TableId::from(1),
                WriteOpType::Put,
                format!("key{}", i).into_bytes(),
                format!("value{}", i).into_bytes(),
            )
            .unwrap();
        writer.write_commit(txn_id).unwrap();
    }

    // Verify all commits were processed
    if let Some(metrics) = writer.group_commit_metrics() {
        let commits = metrics
            .total_commits
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(commits, 5);
    }
}

#[test]
fn test_group_commit_concurrent_transactions() {
    let fs = Arc::new(MemoryFileSystem::new());
    let mut config = WalWriterConfig::default();
    config.group_commit = GroupCommitConfig::high_throughput();

    let writer = Arc::new(WalWriter::create(&*fs, "test.wal", config).unwrap());

    let num_threads = 10;
    let transactions_per_thread = 5;

    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = writer.clone();
        let handle = thread::spawn(move || {
            for i in 0..transactions_per_thread {
                let txn_id =
                    TransactionId::from((thread_id * transactions_per_thread + i) as u64 + 1);
                writer_clone.write_begin(txn_id).unwrap();
                writer_clone
                    .write_operation(
                        txn_id,
                        TableId::from(1),
                        WriteOpType::Put,
                        format!("key{}", txn_id).into_bytes(),
                        format!("value{}", txn_id).into_bytes(),
                    )
                    .unwrap();
                writer_clone.write_commit(txn_id).unwrap();
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Verify all commits were processed
    if let Some(metrics) = writer.group_commit_metrics() {
        let commits = metrics
            .total_commits
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(commits, (num_threads * transactions_per_thread) as u64);

        // Check that batching occurred (should have fewer fsyncs than commits)
        let fsyncs = metrics
            .total_fsyncs
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            fsyncs < commits,
            "Expected batching: {} fsyncs for {} commits",
            fsyncs,
            commits
        );

        println!("Group commit effectiveness:");
        println!("  Total commits: {}", commits);
        println!("  Total fsyncs: {}", fsyncs);
        println!("  Avg batch size: {:.2}", metrics.avg_batch_size());
        println!("  Fsync reduction: {:.2}x", metrics.fsync_reduction_ratio());
    }
}

#[test]
fn test_group_commit_batching() {
    let fs = Arc::new(MemoryFileSystem::new());
    let mut config = WalWriterConfig::default();
    config.group_commit = GroupCommitConfig {
        enabled: true,
        max_batch_size: 5,
        max_wait_micros: 10000, // 10ms - long enough to accumulate
        min_batch_size: 2,
    };

    let writer = Arc::new(WalWriter::create(&*fs, "test.wal", config).unwrap());

    // Start multiple transactions concurrently to trigger batching
    let mut handles = vec![];
    for i in 1..=10 {
        let writer_clone = writer.clone();
        let handle = thread::spawn(move || {
            let txn_id = TransactionId::from(i);
            writer_clone.write_begin(txn_id).unwrap();
            writer_clone
                .write_operation(
                    txn_id,
                    TableId::from(1),
                    WriteOpType::Put,
                    format!("key{}", i).into_bytes(),
                    format!("value{}", i).into_bytes(),
                )
                .unwrap();
            writer_clone.write_commit(txn_id).unwrap();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Give coordinator time to process
    thread::sleep(Duration::from_millis(50));

    // Verify batching occurred
    if let Some(metrics) = writer.group_commit_metrics() {
        let commits = metrics
            .total_commits
            .load(std::sync::atomic::Ordering::Relaxed);
        let fsyncs = metrics
            .total_fsyncs
            .load(std::sync::atomic::Ordering::Relaxed);

        assert_eq!(commits, 10);
        assert!(fsyncs < commits, "Expected batching to reduce fsyncs");
        assert!(
            metrics.avg_batch_size() > 1.0,
            "Expected average batch size > 1"
        );
    }
}

#[test]
fn test_group_commit_config_presets() {
    let fs = MemoryFileSystem::new();

    // Test high-throughput config
    let mut config = WalWriterConfig::default();
    config.group_commit = GroupCommitConfig::high_throughput();
    let writer = WalWriter::create(&fs, "test1.wal", config).unwrap();
    assert!(writer.is_group_commit_enabled());

    // Test low-latency config
    let mut config = WalWriterConfig::default();
    config.group_commit = GroupCommitConfig::low_latency();
    let writer = WalWriter::create(&fs, "test2.wal", config).unwrap();
    assert!(!writer.is_group_commit_enabled());

    // Test balanced config
    let mut config = WalWriterConfig::default();
    config.group_commit = GroupCommitConfig::balanced();
    let writer = WalWriter::create(&fs, "test3.wal", config).unwrap();
    assert!(writer.is_group_commit_enabled());
}

#[test]
fn test_group_commit_metrics() {
    let fs = MemoryFileSystem::new();
    let mut config = WalWriterConfig::default();
    config.group_commit = GroupCommitConfig::high_throughput();

    let writer = WalWriter::create(&fs, "test.wal", config).unwrap();

    // Write some transactions
    for i in 1..=3 {
        let txn_id = TransactionId::from(i);
        writer.write_begin(txn_id).unwrap();
        writer
            .write_operation(
                txn_id,
                TableId::from(1),
                WriteOpType::Put,
                format!("key{}", i).into_bytes(),
                format!("value{}", i).into_bytes(),
            )
            .unwrap();
        writer.write_commit(txn_id).unwrap();
    }

    // Check metrics exist and are reasonable
    if let Some(metrics) = writer.group_commit_metrics() {
        let commits = metrics
            .total_commits
            .load(std::sync::atomic::Ordering::Relaxed);
        let batches = metrics
            .total_batches
            .load(std::sync::atomic::Ordering::Relaxed);
        let fsyncs = metrics
            .total_fsyncs
            .load(std::sync::atomic::Ordering::Relaxed);

        assert!(commits >= 3);
        assert!(batches > 0);
        assert_eq!(batches, fsyncs); // Each batch triggers one fsync
        assert!(metrics.avg_batch_size() > 0.0);
        assert!(metrics.fsync_reduction_ratio() >= 1.0);
    }
}

#[test]
fn test_group_commit_with_rollback() {
    let fs = MemoryFileSystem::new();
    let mut config = WalWriterConfig::default();
    config.group_commit = GroupCommitConfig::high_throughput();

    let writer = WalWriter::create(&fs, "test.wal", config).unwrap();

    // Transaction 1: commit
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key1".to_vec(),
            b"value1".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();

    // Transaction 2: rollback (should not use group commit)
    writer.write_begin(TransactionId::from(2)).unwrap();
    writer
        .write_operation(
            TransactionId::from(2),
            TableId::from(1),
            WriteOpType::Put,
            b"key2".to_vec(),
            b"value2".to_vec(),
        )
        .unwrap();
    writer.write_rollback(TransactionId::from(2)).unwrap();

    // Transaction 3: commit
    writer.write_begin(TransactionId::from(3)).unwrap();
    writer
        .write_operation(
            TransactionId::from(3),
            TableId::from(1),
            WriteOpType::Put,
            b"key3".to_vec(),
            b"value3".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(3)).unwrap();

    // Only commits should be counted in group commit metrics
    if let Some(metrics) = writer.group_commit_metrics() {
        let commits = metrics
            .total_commits
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(commits, 2); // Only transactions 1 and 3
    }
}

#[test]
fn test_group_commit_stress() {
    let fs = Arc::new(MemoryFileSystem::new());
    let mut config = WalWriterConfig::default();
    config.group_commit = GroupCommitConfig::high_throughput();

    let writer = Arc::new(WalWriter::create(&*fs, "test.wal", config).unwrap());

    let num_threads = 20;
    let transactions_per_thread = 50;

    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = writer.clone();
        let handle = thread::spawn(move || {
            for i in 0..transactions_per_thread {
                let txn_id =
                    TransactionId::from((thread_id * transactions_per_thread + i) as u64 + 1);
                writer_clone.write_begin(txn_id).unwrap();

                // Multiple operations per transaction
                for j in 0..3 {
                    writer_clone
                        .write_operation(
                            txn_id,
                            TableId::from(1),
                            WriteOpType::Put,
                            format!("key{}_{}", txn_id, j).into_bytes(),
                            format!("value{}_{}", txn_id, j).into_bytes(),
                        )
                        .unwrap();
                }

                writer_clone.write_commit(txn_id).unwrap();
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Verify all commits were processed
    if let Some(metrics) = writer.group_commit_metrics() {
        let commits = metrics
            .total_commits
            .load(std::sync::atomic::Ordering::Relaxed);
        let expected_commits = (num_threads * transactions_per_thread) as u64;
        assert_eq!(commits, expected_commits);

        let fsyncs = metrics
            .total_fsyncs
            .load(std::sync::atomic::Ordering::Relaxed);
        let reduction = commits as f64 / fsyncs as f64;

        println!("Stress test results:");
        println!("  Threads: {}", num_threads);
        println!("  Transactions per thread: {}", transactions_per_thread);
        println!("  Total commits: {}", commits);
        println!("  Total fsyncs: {}", fsyncs);
        println!("  Fsync reduction: {:.2}x", reduction);
        println!("  Avg batch size: {:.2}", metrics.avg_batch_size());

        // Under high load, we should see significant batching
        assert!(
            reduction > 2.0,
            "Expected at least 2x fsync reduction under load"
        );
    }
}
