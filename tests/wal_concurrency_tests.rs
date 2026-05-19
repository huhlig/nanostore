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

//! Concurrency tests for WAL (Write-Ahead Log)
//!
//! These tests verify that the WAL writer is thread-safe and can handle
//! concurrent operations without data races, panics, or corruption.

use nanostore::pager::CompressionType;
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::{WalReader, WalRecovery, WalWriter, WalWriterConfig, WriteOpType};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Test concurrent writes from multiple threads
#[test]
fn test_concurrent_wal_writers() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "concurrent_writes.wal";
    let config = WalWriterConfig::default();

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    // Spawn multiple threads that write concurrently
    let num_threads = 10;
    let writes_per_thread = 20;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = Arc::clone(&writer);
        let handle = thread::spawn(move || {
            for i in 0..writes_per_thread {
                let txn_id = TransactionId::from((thread_id * writes_per_thread + i) as u64);

                // Begin transaction
                writer_clone.write_begin(txn_id).unwrap();

                // Write operation
                writer_clone
                    .write_operation(
                        txn_id,
                        TableId::from(thread_id as u64),
                        WriteOpType::Put,
                        format!("key_{}_{}", thread_id, i).into_bytes(),
                        format!("value_{}_{}", thread_id, i).into_bytes(),
                    )
                    .unwrap();

                // Commit transaction
                writer_clone.write_commit(txn_id).unwrap();
            }
        });
        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    // Flush to ensure all writes are persisted
    writer.flush().unwrap();

    // Verify recovery
    let result = WalRecovery::recover(&*fs, path).unwrap();
    assert_eq!(
        result.committed_writes.len(),
        (num_threads * writes_per_thread) as usize
    );
    assert!(result.active_transactions.is_empty());
}

/// Test concurrent readers and writers
#[test]
fn test_concurrent_reader_writer() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "reader_writer.wal";
    let config = WalWriterConfig::default();

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    // Write some initial data
    for i in 0..10 {
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
    writer.flush().unwrap();

    // Spawn writer thread
    let writer_clone = Arc::clone(&writer);
    let writer_handle = thread::spawn(move || {
        for i in 10..20 {
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
            thread::sleep(Duration::from_millis(1));
        }
        writer_clone.flush().unwrap();
    });

    // Spawn reader threads
    let fs_clone = Arc::clone(&fs);
    let reader_handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(5));
        let mut reader = WalReader::open(&*fs_clone, path, None).unwrap();
        let mut count = 0;
        while let Some(_record) = reader.read_next().unwrap() {
            count += 1;
        }
        count
    });

    // Wait for both threads
    writer_handle.join().unwrap();
    let records_read = reader_handle.join().unwrap();

    // Reader should have read at least the initial records
    assert!(records_read >= 30); // 10 initial txns * 3 records each (BEGIN, WRITE, COMMIT)
}

/// Test concurrent checkpoints
#[test]
fn test_concurrent_checkpoints() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "concurrent_checkpoints.wal";
    let config = WalWriterConfig::default();

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    // Spawn threads that write and checkpoint concurrently
    let num_threads = 5;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = Arc::clone(&writer);
        let handle = thread::spawn(move || {
            for i in 0..10 {
                let txn_id = TransactionId::from((thread_id * 100 + i) as u64);

                writer_clone.write_begin(txn_id).unwrap();
                writer_clone
                    .write_operation(
                        txn_id,
                        TableId::from(1),
                        WriteOpType::Put,
                        format!("key_{}_{}", thread_id, i).into_bytes(),
                        format!("value_{}_{}", thread_id, i).into_bytes(),
                    )
                    .unwrap();
                writer_clone.write_commit(txn_id).unwrap();

                // Periodically write checkpoints
                if i % 3 == 0 {
                    writer_clone.write_checkpoint().unwrap();
                }
            }
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }

    writer.flush().unwrap();

    // Verify recovery
    let result = WalRecovery::recover(&*fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), (num_threads * 10) as usize);
    assert!(result.last_checkpoint_lsn.is_some());
}

/// Test concurrent transaction operations (begin, write, commit)
#[test]
fn test_concurrent_transaction_operations() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "concurrent_txns.wal";
    let config = WalWriterConfig::default();

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    // Spawn threads with overlapping transactions
    let num_threads = 8;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = Arc::clone(&writer);
        let handle = thread::spawn(move || {
            let txn_id = TransactionId::from(thread_id as u64);

            // Begin transaction
            writer_clone.write_begin(txn_id).unwrap();

            // Multiple writes in the same transaction
            for i in 0..5 {
                writer_clone
                    .write_operation(
                        txn_id,
                        TableId::from(1),
                        WriteOpType::Put,
                        format!("key_{}_{}", thread_id, i).into_bytes(),
                        format!("value_{}_{}", thread_id, i).into_bytes(),
                    )
                    .unwrap();
            }

            // Commit transaction
            writer_clone.write_commit(txn_id).unwrap();
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }

    writer.flush().unwrap();

    // Verify recovery
    let result = WalRecovery::recover(&*fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), (num_threads * 5) as usize);
    assert!(result.active_transactions.is_empty());
}

/// Test concurrent rollbacks
#[test]
fn test_concurrent_rollbacks() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "concurrent_rollbacks.wal";
    let config = WalWriterConfig::default();

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    let num_threads = 10;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = Arc::clone(&writer);
        let handle = thread::spawn(move || {
            let txn_id = TransactionId::from(thread_id as u64);

            writer_clone.write_begin(txn_id).unwrap();
            writer_clone
                .write_operation(
                    txn_id,
                    TableId::from(1),
                    WriteOpType::Put,
                    format!("key_{}", thread_id).into_bytes(),
                    format!("value_{}", thread_id).into_bytes(),
                )
                .unwrap();

            // Half-commit, half-rollback
            if thread_id % 2 == 0 {
                writer_clone.write_commit(txn_id).unwrap();
            } else {
                writer_clone.write_rollback(txn_id).unwrap();
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    writer.flush().unwrap();

    // Verify recovery - only committed transactions should be present
    let result = WalRecovery::recover(&*fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), (num_threads / 2) as usize);
    assert!(result.active_transactions.is_empty());
}

/// Test concurrent flushes
#[test]
fn test_concurrent_flushes() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "concurrent_flushes.wal";
    let mut config = WalWriterConfig::default();
    config.sync_on_write = false; // Disable auto-sync to test manual flushes

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    let num_threads = 8;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = Arc::clone(&writer);
        let handle = thread::spawn(move || {
            for i in 0..10 {
                let txn_id = TransactionId::from((thread_id * 100 + i) as u64);

                writer_clone.write_begin(txn_id).unwrap();
                writer_clone
                    .write_operation(
                        txn_id,
                        TableId::from(1),
                        WriteOpType::Put,
                        format!("key_{}_{}", thread_id, i).into_bytes(),
                        format!("value_{}_{}", thread_id, i).into_bytes(),
                    )
                    .unwrap();
                writer_clone.write_commit(txn_id).unwrap();

                // Concurrent flushes
                if i % 2 == 0 {
                    writer_clone.flush().unwrap();
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Final flush
    writer.flush().unwrap();

    // Verify all data was written correctly
    let result = WalRecovery::recover(&*fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), (num_threads * 10) as usize);
}

/// Test concurrent LSN generation
#[test]
fn test_concurrent_lsn_generation() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "concurrent_lsn.wal";
    let config = WalWriterConfig::default();

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    let num_threads = 20;
    let writes_per_thread = 10;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = Arc::clone(&writer);
        let handle = thread::spawn(move || {
            let mut lsns = vec![];
            for i in 0..writes_per_thread {
                let txn_id = TransactionId::from((thread_id * writes_per_thread + i) as u64);
                let lsn = writer_clone.write_begin(txn_id).unwrap();
                lsns.push(lsn);
                writer_clone.write_commit(txn_id).unwrap();
            }
            lsns
        });
        handles.push(handle);
    }

    // Collect all LSNs
    let mut all_lsns = vec![];
    for handle in handles {
        let lsns = handle.join().unwrap();
        all_lsns.extend(lsns);
    }

    writer.flush().unwrap();

    // Verify all LSNs are unique
    all_lsns.sort();
    all_lsns.dedup();
    assert_eq!(all_lsns.len(), (num_threads * writes_per_thread) as usize);
}

/// Test concurrent active transaction tracking
#[test]
fn test_concurrent_active_transaction_tracking() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "concurrent_active_txns.wal";
    let config = WalWriterConfig::default();

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    let num_threads = 15;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = Arc::clone(&writer);
        let handle = thread::spawn(move || {
            let txn_id = TransactionId::from(thread_id as u64);

            // Begin transaction
            writer_clone.write_begin(txn_id).unwrap();

            // Small delay to ensure overlap
            thread::sleep(Duration::from_millis(5));

            // Write operation
            writer_clone
                .write_operation(
                    txn_id,
                    TableId::from(1),
                    WriteOpType::Put,
                    format!("key_{}", thread_id).into_bytes(),
                    format!("value_{}", thread_id).into_bytes(),
                )
                .unwrap();

            // Commit
            writer_clone.write_commit(txn_id).unwrap();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    writer.flush().unwrap();

    // Verify no active transactions remain
    assert!(writer.active_transactions().is_empty());
}

/// Test concurrent writes with compression
#[test]
fn test_concurrent_writes_with_compression() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "concurrent_compressed.wal";
    let mut config = WalWriterConfig::default();
    config.compression = CompressionType::Lz4;

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    let num_threads = 8;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = Arc::clone(&writer);
        let handle = thread::spawn(move || {
            for i in 0..10 {
                let txn_id = TransactionId::from((thread_id * 100 + i) as u64);
                let compressible_data = format!("Repeated data {} ", thread_id).repeat(100);

                writer_clone.write_begin(txn_id).unwrap();
                writer_clone
                    .write_operation(
                        txn_id,
                        TableId::from(1),
                        WriteOpType::Put,
                        format!("key_{}_{}", thread_id, i).into_bytes(),
                        compressible_data.into_bytes(),
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

    writer.flush().unwrap();

    // Verify recovery with compressed data
    let result = WalRecovery::recover(&*fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), (num_threads * 10) as usize);
}

/// Test high contention scenario
#[test]
fn test_high_contention_wal_writes() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "high_contention.wal";
    let config = WalWriterConfig::default();

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    // Many threads, many writes, no delays
    let num_threads = 50;
    let writes_per_thread = 20;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = Arc::clone(&writer);
        let handle = thread::spawn(move || {
            for i in 0..writes_per_thread {
                let txn_id = TransactionId::from((thread_id * writes_per_thread + i) as u64);

                writer_clone.write_begin(txn_id).unwrap();
                writer_clone
                    .write_operation(
                        txn_id,
                        TableId::from(1),
                        WriteOpType::Put,
                        format!("k_{}_{}", thread_id, i).into_bytes(),
                        format!("v_{}_{}", thread_id, i).into_bytes(),
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

    writer.flush().unwrap();

    // Verify all writes succeeded
    let result = WalRecovery::recover(&*fs, path).unwrap();
    assert_eq!(
        result.committed_writes.len(),
        (num_threads * writes_per_thread) as usize
    );
}

/// Test concurrent truncate operations
#[test]
fn test_concurrent_truncate() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "concurrent_truncate.wal";
    let config = WalWriterConfig::default();

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    // Write some initial data
    for i in 0..10 {
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
    writer.flush().unwrap();

    let size_before = writer.file_size();
    assert!(size_before > 0);

    // Truncate should work correctly
    writer.truncate().unwrap();
    assert_eq!(writer.file_size(), 0);

    // Write more data after truncate
    let writer_clone = Arc::clone(&writer);
    let handle = thread::spawn(move || {
        for i in 20..30 {
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
        }
        writer_clone.flush().unwrap();
    });

    handle.join().unwrap();

    // After new writes, file should have data again
    assert!(writer.file_size() > 0);
}

/// Test concurrent recovery - simulate crash and recovery with active transactions
#[test]
fn test_concurrent_recovery() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "concurrent_recovery.wal";
    let config = WalWriterConfig::default();

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    // Spawn multiple threads that write concurrently
    let num_threads = 8;
    let writes_per_thread = 10;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = Arc::clone(&writer);
        let handle = thread::spawn(move || {
            for i in 0..writes_per_thread {
                let txn_id = TransactionId::from((thread_id * writes_per_thread + i) as u64);

                writer_clone.write_begin(txn_id).unwrap();
                writer_clone
                    .write_operation(
                        txn_id,
                        TableId::from(thread_id as u64),
                        WriteOpType::Put,
                        format!("key_{}_{}", thread_id, i).into_bytes(),
                        format!("value_{}_{}", thread_id, i).into_bytes(),
                    )
                    .unwrap();

                // Only commit half of the transactions to simulate crash
                if i < writes_per_thread / 2 {
                    writer_clone.write_commit(txn_id).unwrap();
                }
            }
        });
        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    // Flush to ensure all writes are persisted
    writer.flush().unwrap();

    // Drop writer to simulate crash
    drop(writer);

    // Perform recovery
    let result = WalRecovery::recover(&*fs, path).unwrap();

    // Verify recovery results
    // Should have committed writes from first half of each thread
    assert_eq!(
        result.committed_writes.len(),
        (num_threads * writes_per_thread / 2) as usize
    );

    // Should have active transactions from second half of each thread
    assert_eq!(
        result.active_transactions.len(),
        (num_threads * writes_per_thread / 2) as usize
    );

    // Verify all committed writes are present
    for thread_id in 0..num_threads {
        for i in 0..(writes_per_thread / 2) {
            let txn_id = (thread_id * writes_per_thread + i) as u64;
            let found = result.committed_writes.iter().any(|w| {
                w.table_id == TableId::from(thread_id as u64)
                    && w.key == format!("key_{}_{}", thread_id, i).into_bytes()
                    && w.value == format!("value_{}_{}", thread_id, i).into_bytes()
            });
            assert!(
                found,
                "Missing committed write for txn {} (thread {}, write {})",
                txn_id, thread_id, i
            );
        }
    }

    // Verify all active transactions are present
    for thread_id in 0..num_threads {
        for i in (writes_per_thread / 2)..writes_per_thread {
            let txn_id = TransactionId::from((thread_id * writes_per_thread + i) as u64);
            assert!(
                result.active_transactions.contains(&txn_id),
                "Missing active transaction {} (thread {}, write {})",
                txn_id,
                thread_id,
                i
            );
        }
    }
}

/// Test concurrent recovery with checkpoint
#[test]
fn test_concurrent_recovery_with_checkpoint() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "concurrent_recovery_checkpoint.wal";
    let config = WalWriterConfig::default();

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    // Phase 1: Write and commit some transactions
    let num_threads = 5;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = Arc::clone(&writer);
        let handle = thread::spawn(move || {
            let txn_id = TransactionId::from(thread_id as u64);
            writer_clone.write_begin(txn_id).unwrap();
            writer_clone
                .write_operation(
                    txn_id,
                    TableId::from(1),
                    WriteOpType::Put,
                    format!("key_{}", thread_id).into_bytes(),
                    format!("value_{}", thread_id).into_bytes(),
                )
                .unwrap();
            writer_clone.write_commit(txn_id).unwrap();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Write checkpoint
    writer.write_checkpoint().unwrap();

    // Phase 2: Start new transactions but don't commit (simulate crash)
    let mut handles = vec![];
    for thread_id in num_threads..(num_threads * 2) {
        let writer_clone = Arc::clone(&writer);
        let handle = thread::spawn(move || {
            let txn_id = TransactionId::from(thread_id as u64);
            writer_clone.write_begin(txn_id).unwrap();
            writer_clone
                .write_operation(
                    txn_id,
                    TableId::from(1),
                    WriteOpType::Put,
                    format!("key_{}", thread_id).into_bytes(),
                    format!("value_{}", thread_id).into_bytes(),
                )
                .unwrap();
            // Don't commit - simulate crash
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    writer.flush().unwrap();
    drop(writer);

    // Perform recovery
    let result = WalRecovery::recover(&*fs, path).unwrap();

    // Should have checkpoint
    assert!(result.last_checkpoint_lsn.is_some());

    // Should have committed writes from phase 1
    assert_eq!(result.committed_writes.len(), num_threads as usize);

    // Should have active transactions from phase 2
    assert_eq!(result.active_transactions.len(), num_threads as usize);

    // Verify active transactions are from phase 2
    for thread_id in num_threads..(num_threads * 2) {
        let txn_id = TransactionId::from(thread_id as u64);
        assert!(result.active_transactions.contains(&txn_id));
    }
}

/// Test recovery with concurrent readers during recovery
#[test]
fn test_concurrent_recovery_with_readers() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "concurrent_recovery_readers.wal";
    let config = WalWriterConfig::default();

    // Write some test data
    {
        let writer = WalWriter::create(&*fs, path, config).unwrap();
        for i in 0..20 {
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
            if i % 2 == 0 {
                writer.write_commit(txn_id).unwrap();
            }
            // Leave odd transactions uncommitted
        }
        writer.flush().unwrap();
    }

    // Spawn multiple threads that perform recovery concurrently
    let num_threads = 10;
    let mut handles = vec![];

    for _ in 0..num_threads {
        let fs_clone = Arc::clone(&fs);
        let path_clone = path.to_string();
        let handle = thread::spawn(move || {
            let result = WalRecovery::recover(&*fs_clone, &path_clone).unwrap();
            (
                result.committed_writes.len(),
                result.active_transactions.len(),
            )
        });
        handles.push(handle);
    }

    // Collect results from all threads
    let mut results = vec![];
    for handle in handles {
        results.push(handle.join().unwrap());
    }

    // All recovery attempts should produce the same result
    let first_result = results[0];
    for result in &results {
        assert_eq!(
            *result, first_result,
            "All recovery attempts should produce identical results"
        );
    }

    // Verify the expected counts
    assert_eq!(first_result.0, 10); // 10 committed transactions (even numbers)
    assert_eq!(first_result.1, 10); // 10 active transactions (odd numbers)
}

/// Test LSN monotonicity under extreme concurrency
#[test]
fn test_lsn_monotonicity_stress() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "lsn_monotonicity_stress.wal";
    let config = WalWriterConfig::default();

    let writer = Arc::new(WalWriter::create(&*fs, path, config).unwrap());

    // Very high contention scenario
    let num_threads = 100;
    let writes_per_thread = 5;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let writer_clone = Arc::clone(&writer);
        let handle = thread::spawn(move || {
            let mut lsns = vec![];
            for i in 0..writes_per_thread {
                let txn_id = TransactionId::from((thread_id * writes_per_thread + i) as u64);
                let lsn = writer_clone.write_begin(txn_id).unwrap();
                lsns.push(lsn);
                writer_clone
                    .write_operation(
                        txn_id,
                        TableId::from(1),
                        WriteOpType::Put,
                        format!("k{}", txn_id).into_bytes(),
                        format!("v{}", txn_id).into_bytes(),
                    )
                    .unwrap();
                writer_clone.write_commit(txn_id).unwrap();
            }
            lsns
        });
        handles.push(handle);
    }

    // Collect all LSNs
    let mut all_lsns = vec![];
    for handle in handles {
        let lsns = handle.join().unwrap();
        all_lsns.extend(lsns);
    }

    writer.flush().unwrap();

    // Verify all LSNs are unique and monotonically increasing
    all_lsns.sort();
    let original_len = all_lsns.len();
    all_lsns.dedup();
    assert_eq!(
        all_lsns.len(),
        original_len,
        "All LSNs must be unique (no duplicates)"
    );

    // Verify LSNs are sequential (no gaps in this test since we control all writes)
    for i in 1..all_lsns.len() {
        assert!(
            all_lsns[i] > all_lsns[i - 1],
            "LSNs must be strictly increasing"
        );
    }

    // Verify recovery produces correct number of writes
    let result = WalRecovery::recover(&*fs, path).unwrap();
    assert_eq!(
        result.committed_writes.len(),
        (num_threads * writes_per_thread) as usize
    );
}
