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

//! Benchmarks for WAL (Write-Ahead Log) implementation

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use nanostore::pager::{CompressionType, EncryptionType};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::{LocalFileSystem, MemoryFileSystem};
use nanostore::wal::{
    GroupCommitConfig, WalReader, WalRecordIterator, WalRecovery, WalWriter, WalWriterConfig,
    WriteOpType,
};
use std::hint::black_box;
use std::sync::Arc;
use std::thread;
// ============================================================================
// WAL Writer Creation Benchmarks
// ============================================================================

fn bench_wal_writer_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_writer_creation");

    group.bench_function("memory_fs", |b| {
        let fs = MemoryFileSystem::new();
        let config = WalWriterConfig::default();
        let mut counter = 0;
        b.iter(|| {
            let path = format!("/bench_{}.wal", counter);
            counter += 1;
            let writer = WalWriter::create(&fs, &path, config.clone()).unwrap();
            black_box(writer);
        });
    });

    group.bench_function("local_fs", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let config = WalWriterConfig::default();
        let mut counter = 0;
        b.iter(|| {
            let path = format!("/bench_{}.wal", counter);
            counter += 1;
            let writer = WalWriter::create(&fs, &path, config.clone()).unwrap();
            black_box(writer);
        });
    });

    group.finish();
}

// ============================================================================
// Transaction Benchmarks
// ============================================================================

fn bench_transactions(c: &mut Criterion) {
    let mut group = c.benchmark_group("transactions");

    group.bench_function("memory_fs_begin_commit", |b| {
        let fs = MemoryFileSystem::new();
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
        let mut txn_id = TransactionId::from(1);

        b.iter(|| {
            writer.write_begin(txn_id).unwrap();
            writer.write_commit(txn_id).unwrap();
            txn_id = TransactionId::from(txn_id.as_u64() + 1);
        });
    });

    group.bench_function("local_fs_begin_commit", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
        let mut txn_id = TransactionId::from(1);

        b.iter(|| {
            writer.write_begin(txn_id).unwrap();
            writer.write_commit(txn_id).unwrap();
            txn_id = TransactionId::from(txn_id.as_u64() + 1);
        });
    });

    group.bench_function("memory_fs_begin_rollback", |b| {
        let fs = MemoryFileSystem::new();
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
        let mut txn_id = TransactionId::from(1);

        b.iter(|| {
            writer.write_begin(txn_id).unwrap();
            writer.write_rollback(txn_id).unwrap();
            txn_id = TransactionId::from(txn_id.as_u64() + 1);
        });
    });

    group.finish();
}

// ============================================================================
// Write Operation Benchmarks
// ============================================================================

fn bench_write_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_operations");

    for value_size in [64, 256, 1024, 4096].iter() {
        group.throughput(Throughput::Bytes(*value_size as u64));

        group.bench_with_input(
            BenchmarkId::new("memory_fs_put", value_size),
            value_size,
            |b, &value_size| {
                let fs = MemoryFileSystem::new();
                let config = WalWriterConfig::default();
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
                let value = vec![0xAB; value_size];
                let mut counter = 0;

                // Begin a transaction for the benchmark
                writer.write_begin(TransactionId::from(1)).unwrap();

                b.iter(|| {
                    let key = format!("key_{}", counter).into_bytes();
                    counter += 1;
                    writer
                        .write_operation(
                            TransactionId::from(1),
                            TableId::from(1),
                            WriteOpType::Put,
                            key,
                            value.clone(),
                        )
                        .unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("local_fs_put", value_size),
            value_size,
            |b, &value_size| {
                let temp_dir = tempfile::tempdir().unwrap();
                let fs = LocalFileSystem::new(temp_dir.path());
                let config = WalWriterConfig::default();
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
                let value = vec![0xAB; value_size];
                let mut counter = 0;

                // Begin a transaction for the benchmark
                writer.write_begin(TransactionId::from(1)).unwrap();

                b.iter(|| {
                    let key = format!("key_{}", counter).into_bytes();
                    counter += 1;
                    writer
                        .write_operation(
                            TransactionId::from(1),
                            TableId::from(1),
                            WriteOpType::Put,
                            key,
                            value.clone(),
                        )
                        .unwrap();
                });
            },
        );
    }

    group.bench_function("memory_fs_delete", |b| {
        let fs = MemoryFileSystem::new();
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
        let mut counter = 0;

        // Begin a transaction for the benchmark
        writer.write_begin(TransactionId::from(1)).unwrap();

        b.iter(|| {
            let key = format!("key_{}", counter).into_bytes();
            counter += 1;
            writer
                .write_operation(
                    TransactionId::from(1),
                    TableId::from(1),
                    WriteOpType::Delete,
                    key,
                    vec![],
                )
                .unwrap();
        });
    });

    group.finish();
}

// ============================================================================
// Flush Benchmarks
// ============================================================================

fn bench_flush_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("flush_operations");

    for num_writes in [10, 50, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("memory_fs_flush_after_writes", num_writes),
            num_writes,
            |b, &num_writes| {
                let fs = MemoryFileSystem::new();
                let config = WalWriterConfig::default();
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

                b.iter(|| {
                    writer.write_begin(TransactionId::from(1)).unwrap();
                    for i in 0..num_writes {
                        writer
                            .write_operation(
                                TransactionId::from(1),
                                TableId::from(1),
                                WriteOpType::Put,
                                format!("key_{}", i).into_bytes(),
                                b"value".to_vec(),
                            )
                            .unwrap();
                    }
                    writer.write_commit(TransactionId::from(1)).unwrap();
                    writer.flush().unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("local_fs_flush_after_writes", num_writes),
            num_writes,
            |b, &num_writes| {
                let temp_dir = tempfile::tempdir().unwrap();
                let fs = LocalFileSystem::new(temp_dir.path());
                let config = WalWriterConfig::default();
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

                b.iter(|| {
                    writer.write_begin(TransactionId::from(1)).unwrap();
                    for i in 0..num_writes {
                        writer
                            .write_operation(
                                TransactionId::from(1),
                                TableId::from(1),
                                WriteOpType::Put,
                                format!("key_{}", i).into_bytes(),
                                b"value".to_vec(),
                            )
                            .unwrap();
                    }
                    writer.write_commit(TransactionId::from(1)).unwrap();
                    writer.flush().unwrap();
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// WAL Reader Benchmarks
// ============================================================================

fn bench_wal_reader(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_reader");

    for num_records in [10, 50, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("memory_fs_read_all", num_records),
            num_records,
            |b, &num_records| {
                let fs = MemoryFileSystem::new();
                let config = WalWriterConfig::default();
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

                // Write test data
                for i in 0..num_records {
                    writer.write_begin(TransactionId::from(i)).unwrap();
                    writer
                        .write_operation(
                            TransactionId::from(i),
                            TableId::from(1),
                            WriteOpType::Put,
                            format!("key_{}", i).into_bytes(),
                            b"value".to_vec(),
                        )
                        .unwrap();
                    writer.write_commit(TransactionId::from(i)).unwrap();
                }
                writer.flush().unwrap();
                drop(writer);

                b.iter(|| {
                    let reader = WalReader::open(&fs, "/bench.wal", None).unwrap();
                    let iter = WalRecordIterator::new(reader);
                    let records: Vec<_> = iter.collect::<Result<Vec<_>, _>>().unwrap();
                    black_box(records);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("local_fs_read_all", num_records),
            num_records,
            |b, &num_records| {
                let temp_dir = tempfile::tempdir().unwrap();
                let fs = LocalFileSystem::new(temp_dir.path());
                let config = WalWriterConfig::default();
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

                // Write test data
                for i in 0..num_records {
                    writer.write_begin(TransactionId::from(i)).unwrap();
                    writer
                        .write_operation(
                            TransactionId::from(i),
                            TableId::from(1),
                            WriteOpType::Put,
                            format!("key_{}", i).into_bytes(),
                            b"value".to_vec(),
                        )
                        .unwrap();
                    writer.write_commit(TransactionId::from(i)).unwrap();
                }
                writer.flush().unwrap();
                drop(writer);

                b.iter(|| {
                    let reader = WalReader::open(&fs, "/bench.wal", None).unwrap();
                    let iter = WalRecordIterator::new(reader);
                    let records: Vec<_> = iter.collect::<Result<Vec<_>, _>>().unwrap();
                    black_box(records);
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Recovery Benchmarks
// ============================================================================

fn bench_recovery(c: &mut Criterion) {
    let mut group = c.benchmark_group("recovery");

    for num_transactions in [10, 50, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("memory_fs_recover", num_transactions),
            num_transactions,
            |b, &num_transactions| {
                let fs = MemoryFileSystem::new();
                let config = WalWriterConfig::default();
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

                // Write test data
                for i in 0..num_transactions {
                    writer.write_begin(TransactionId::from(i)).unwrap();
                    writer
                        .write_operation(
                            TransactionId::from(i),
                            TableId::from(1),
                            WriteOpType::Put,
                            format!("key_{}", i).into_bytes(),
                            b"value".to_vec(),
                        )
                        .unwrap();
                    writer.write_commit(TransactionId::from(i)).unwrap();
                }
                writer.flush().unwrap();
                drop(writer);

                b.iter(|| {
                    let result = WalRecovery::recover(&fs, "/bench.wal").unwrap();
                    black_box(result);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("local_fs_recover", num_transactions),
            num_transactions,
            |b, &num_transactions| {
                let temp_dir = tempfile::tempdir().unwrap();
                let fs = LocalFileSystem::new(temp_dir.path());
                let config = WalWriterConfig::default();
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

                // Write test data
                for i in 0..num_transactions {
                    writer.write_begin(TransactionId::from(i)).unwrap();
                    writer
                        .write_operation(
                            TransactionId::from(i),
                            TableId::from(1),
                            WriteOpType::Put,
                            format!("key_{}", i).into_bytes(),
                            b"value".to_vec(),
                        )
                        .unwrap();
                    writer.write_commit(TransactionId::from(i)).unwrap();
                }
                writer.flush().unwrap();
                drop(writer);

                b.iter(|| {
                    let result = WalRecovery::recover(&fs, "/bench.wal").unwrap();
                    black_box(result);
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Checkpoint Benchmarks
// ============================================================================

fn bench_checkpoint(c: &mut Criterion) {
    let mut group = c.benchmark_group("checkpoint");

    group.bench_function("memory_fs_checkpoint", |b| {
        let fs = MemoryFileSystem::new();
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

        b.iter(|| {
            let lsn = writer.write_checkpoint().unwrap();
            black_box(lsn);
        });
    });

    group.bench_function("local_fs_checkpoint", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let config = WalWriterConfig::default();
        let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

        b.iter(|| {
            let lsn = writer.write_checkpoint().unwrap();
            black_box(lsn);
        });
    });

    group.finish();
}

// ============================================================================
// Complete Transaction Benchmarks
// ============================================================================

fn bench_complete_transactions(c: &mut Criterion) {
    let mut group = c.benchmark_group("complete_transactions");

    for num_writes in [1, 5, 10, 20].iter() {
        group.bench_with_input(
            BenchmarkId::new("memory_fs_txn_with_writes", num_writes),
            num_writes,
            |b, &num_writes| {
                let fs = MemoryFileSystem::new();
                let config = WalWriterConfig::default();
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
                let mut txn_id = TransactionId::from(1);

                b.iter(|| {
                    writer.write_begin(txn_id).unwrap();
                    for i in 0..num_writes {
                        writer
                            .write_operation(
                                txn_id,
                                TableId::from(1),
                                WriteOpType::Put,
                                format!("key_{}", i).into_bytes(),
                                b"value".to_vec(),
                            )
                            .unwrap();
                    }
                    writer.write_commit(txn_id).unwrap();
                    txn_id = TransactionId::from(txn_id.as_u64() + 1);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("local_fs_txn_with_writes", num_writes),
            num_writes,
            |b, &num_writes| {
                let temp_dir = tempfile::tempdir().unwrap();
                let fs = LocalFileSystem::new(temp_dir.path());
                let config = WalWriterConfig::default();
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
                let mut txn_id = TransactionId::from(1);

                b.iter(|| {
                    writer.write_begin(txn_id).unwrap();
                    for i in 0..num_writes {
                        writer
                            .write_operation(
                                txn_id,
                                TableId::from(1),
                                WriteOpType::Put,
                                format!("key_{}", i).into_bytes(),
                                b"value".to_vec(),
                            )
                            .unwrap();
                    }
                    writer.write_commit(txn_id).unwrap();
                    txn_id = TransactionId::from(txn_id.as_u64() + 1);
                });
            },
        );
    }

    group.finish();
}
// ============================================================================
// Compression Benchmarks
// ============================================================================

fn bench_compression(c: &mut Criterion) {
    let mut group = c.benchmark_group("compression");

    for compression in [
        CompressionType::None,
        CompressionType::Lz4,
        CompressionType::Zstd,
    ]
    .iter()
    {
        let compression_name = match compression {
            CompressionType::None => "none",
            CompressionType::Lz4 => "lz4",
            CompressionType::Zstd => "zstd",
        };

        for value_size in [256, 1024, 4096].iter() {
            group.throughput(Throughput::Bytes(*value_size as u64));

            group.bench_with_input(
                BenchmarkId::new(format!("memory_fs_write_{}", compression_name), value_size),
                &(*compression, *value_size),
                |b, &(compression, value_size)| {
                    let fs = MemoryFileSystem::new();
                    let mut config = WalWriterConfig::default();
                    config.compression = compression;
                    let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

                    // Create compressible data (repeated pattern)
                    let pattern = b"The quick brown fox jumps over the lazy dog. ";
                    let mut value = Vec::new();
                    for _ in 0..(value_size / pattern.len() + 1) {
                        value.extend_from_slice(pattern);
                    }
                    value.truncate(value_size);

                    let mut counter = 0;
                    writer.write_begin(TransactionId::from(1)).unwrap();

                    b.iter(|| {
                        let key = format!("key_{}", counter).into_bytes();
                        counter += 1;
                        writer
                            .write_operation(
                                TransactionId::from(1),
                                TableId::from(1),
                                WriteOpType::Put,
                                key,
                                value.clone(),
                            )
                            .unwrap();
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("memory_fs_read_{}", compression_name), value_size),
                &(*compression, *value_size),
                |b, &(compression, value_size)| {
                    let fs = MemoryFileSystem::new();
                    let mut config = WalWriterConfig::default();
                    config.compression = compression;
                    let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

                    // Create compressible data
                    let pattern = b"The quick brown fox jumps over the lazy dog. ";
                    let mut value = Vec::new();
                    for _ in 0..(value_size / pattern.len() + 1) {
                        value.extend_from_slice(pattern);
                    }
                    value.truncate(value_size);

                    // Write test data
                    for i in 0..10 {
                        writer.write_begin(TransactionId::from(i)).unwrap();
                        writer
                            .write_operation(
                                TransactionId::from(i),
                                TableId::from(1),
                                WriteOpType::Put,
                                format!("key_{}", i).into_bytes(),
                                value.clone(),
                            )
                            .unwrap();
                        writer.write_commit(TransactionId::from(i)).unwrap();
                    }
                    writer.flush().unwrap();
                    drop(writer);

                    b.iter(|| {
                        let reader = WalReader::open(&fs, "/bench.wal", None).unwrap();
                        let iter = WalRecordIterator::new(reader);
                        let records: Vec<_> = iter.collect::<Result<Vec<_>, _>>().unwrap();
                        black_box(records);
                    });
                },
            );
        }
    }

    group.finish();
}

// ============================================================================
// Encryption Benchmarks
// ============================================================================

fn bench_encryption(c: &mut Criterion) {
    let mut group = c.benchmark_group("encryption");

    // Generate a test encryption key
    let encryption_key = [0x42u8; 32];

    for encrypted in [false, true].iter() {
        let encryption_name = if *encrypted {
            "encrypted"
        } else {
            "unencrypted"
        };

        for value_size in [256, 1024, 4096].iter() {
            group.throughput(Throughput::Bytes(*value_size as u64));

            group.bench_with_input(
                BenchmarkId::new(format!("memory_fs_write_{}", encryption_name), value_size),
                &(*encrypted, *value_size),
                |b, &(encrypted, value_size)| {
                    let fs = MemoryFileSystem::new();
                    let mut config = WalWriterConfig::default();
                    if encrypted {
                        config.encryption = EncryptionType::Aes256Gcm;
                        config.encryption_key = Some(encryption_key);
                    }
                    let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

                    let value = vec![0xAB; value_size];
                    let mut counter = 0;
                    writer.write_begin(TransactionId::from(1)).unwrap();

                    b.iter(|| {
                        let key = format!("key_{}", counter).into_bytes();
                        counter += 1;
                        writer
                            .write_operation(
                                TransactionId::from(1),
                                TableId::from(1),
                                WriteOpType::Put,
                                key,
                                value.clone(),
                            )
                            .unwrap();
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("memory_fs_read_{}", encryption_name), value_size),
                &(*encrypted, *value_size),
                |b, &(encrypted, value_size)| {
                    let fs = MemoryFileSystem::new();
                    let mut config = WalWriterConfig::default();
                    let key = if encrypted {
                        config.encryption = EncryptionType::Aes256Gcm;
                        config.encryption_key = Some(encryption_key);
                        Some(encryption_key)
                    } else {
                        None
                    };
                    let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

                    let value = vec![0xAB; value_size];

                    // Write test data
                    for i in 0..10 {
                        writer.write_begin(TransactionId::from(i)).unwrap();
                        writer
                            .write_operation(
                                TransactionId::from(i),
                                TableId::from(1),
                                WriteOpType::Put,
                                format!("key_{}", i).into_bytes(),
                                value.clone(),
                            )
                            .unwrap();
                        writer.write_commit(TransactionId::from(i)).unwrap();
                    }
                    writer.flush().unwrap();
                    drop(writer);

                    b.iter(|| {
                        let reader = WalReader::open(&fs, "/bench.wal", key).unwrap();
                        let iter = WalRecordIterator::new(reader);
                        let records: Vec<_> = iter.collect::<Result<Vec<_>, _>>().unwrap();
                        black_box(records);
                    });
                },
            );
        }
    }

    group.finish();
}

// ============================================================================
// Combined Compression + Encryption Benchmarks
// ============================================================================

fn bench_compression_and_encryption(c: &mut Criterion) {
    let mut group = c.benchmark_group("compression_and_encryption");

    let encryption_key = [0x42u8; 32];
    let value_size = 4096;

    group.throughput(Throughput::Bytes(value_size as u64));

    for compression in [
        CompressionType::None,
        CompressionType::Lz4,
        CompressionType::Zstd,
    ]
    .iter()
    {
        let compression_name = match compression {
            CompressionType::None => "none",
            CompressionType::Lz4 => "lz4",
            CompressionType::Zstd => "zstd",
        };

        group.bench_function(
            format!("memory_fs_write_{}_encrypted", compression_name),
            |b| {
                let fs = MemoryFileSystem::new();
                let mut config = WalWriterConfig::default();
                config.compression = *compression;
                config.encryption = EncryptionType::Aes256Gcm;
                config.encryption_key = Some(encryption_key);
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

                // Create compressible data
                let pattern = b"The quick brown fox jumps over the lazy dog. ";
                let mut value = Vec::new();
                for _ in 0..(value_size / pattern.len() + 1) {
                    value.extend_from_slice(pattern);
                }
                value.truncate(value_size);

                let mut counter = 0;
                writer.write_begin(TransactionId::from(1)).unwrap();

                b.iter(|| {
                    let key = format!("key_{}", counter).into_bytes();
                    counter += 1;
                    writer
                        .write_operation(
                            TransactionId::from(1),
                            TableId::from(1),
                            WriteOpType::Put,
                            key,
                            value.clone(),
                        )
                        .unwrap();
                });
            },
        );

        group.bench_function(
            format!("memory_fs_read_{}_encrypted", compression_name),
            |b| {
                let fs = MemoryFileSystem::new();
                let mut config = WalWriterConfig::default();
                config.compression = *compression;
                config.encryption = EncryptionType::Aes256Gcm;
                config.encryption_key = Some(encryption_key);
                let key = Some(encryption_key);
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

                // Create compressible data
                let pattern = b"The quick brown fox jumps over the lazy dog. ";
                let mut value = Vec::new();
                for _ in 0..(value_size / pattern.len() + 1) {
                    value.extend_from_slice(pattern);
                }
                value.truncate(value_size);

                // Write test data
                for i in 0..10 {
                    writer.write_begin(TransactionId::from(i)).unwrap();
                    writer
                        .write_operation(
                            TransactionId::from(i),
                            TableId::from(1),
                            WriteOpType::Put,
                            format!("key_{}", i).into_bytes(),
                            value.clone(),
                        )
                        .unwrap();
                    writer.write_commit(TransactionId::from(i)).unwrap();
                }
                writer.flush().unwrap();
                drop(writer);

                b.iter(|| {
                    let reader = WalReader::open(&fs, "/bench.wal", key).unwrap();
                    let iter = WalRecordIterator::new(reader);
                    let records: Vec<_> = iter.collect::<Result<Vec<_>, _>>().unwrap();
                    black_box(records);
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Group Commit Benchmarks
// ============================================================================

fn bench_group_commit_single_thread(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_single_thread");

    // Benchmark without group commit
    group.bench_function("memory_fs_no_group_commit", |b| {
        let fs = MemoryFileSystem::new();
        let mut config = WalWriterConfig::default();
        config.group_commit.enabled = false;
        let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
        let mut txn_id = TransactionId::from(1);

        b.iter(|| {
            writer.write_begin(txn_id).unwrap();
            writer
                .write_operation(
                    txn_id,
                    TableId::from(1),
                    WriteOpType::Put,
                    b"key".to_vec(),
                    b"value".to_vec(),
                )
                .unwrap();
            writer.write_commit(txn_id).unwrap();
            txn_id = TransactionId::from(txn_id.as_u64() + 1);
        });
    });

    // Benchmark with group commit
    group.bench_function("memory_fs_with_group_commit", |b| {
        let fs = MemoryFileSystem::new();
        let mut config = WalWriterConfig::default();
        config.group_commit = GroupCommitConfig::high_throughput();
        let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
        let mut txn_id = TransactionId::from(1);

        b.iter(|| {
            writer.write_begin(txn_id).unwrap();
            writer
                .write_operation(
                    txn_id,
                    TableId::from(1),
                    WriteOpType::Put,
                    b"key".to_vec(),
                    b"value".to_vec(),
                )
                .unwrap();
            writer.write_commit(txn_id).unwrap();
            txn_id = TransactionId::from(txn_id.as_u64() + 1);
        });
    });

    group.finish();
}

fn bench_group_commit_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_concurrent");

    for num_threads in [2, 4, 8, 16].iter() {
        // Benchmark without group commit
        group.bench_with_input(
            BenchmarkId::new("memory_fs_no_group_commit", num_threads),
            num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    let fs = Arc::new(MemoryFileSystem::new());
                    let mut config = WalWriterConfig::default();
                    config.group_commit.enabled = false;
                    let writer = Arc::new(WalWriter::create(&*fs, "/bench.wal", config).unwrap());

                    let mut handles = vec![];
                    for thread_id in 0..num_threads {
                        let writer_clone = writer.clone();
                        let handle = thread::spawn(move || {
                            for i in 0..10 {
                                let txn_id = TransactionId::from((thread_id * 10 + i) as u64 + 1);
                                writer_clone.write_begin(txn_id).unwrap();
                                writer_clone
                                    .write_operation(
                                        txn_id,
                                        TableId::from(1),
                                        WriteOpType::Put,
                                        format!("key{}", txn_id).into_bytes(),
                                        b"value".to_vec(),
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
                });
            },
        );

        // Benchmark with group commit
        group.bench_with_input(
            BenchmarkId::new("memory_fs_with_group_commit", num_threads),
            num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    let fs = Arc::new(MemoryFileSystem::new());
                    let mut config = WalWriterConfig::default();
                    config.group_commit = GroupCommitConfig::high_throughput();
                    let writer = Arc::new(WalWriter::create(&*fs, "/bench.wal", config).unwrap());

                    let mut handles = vec![];
                    for thread_id in 0..num_threads {
                        let writer_clone = writer.clone();
                        let handle = thread::spawn(move || {
                            for i in 0..10 {
                                let txn_id = TransactionId::from((thread_id * 10 + i) as u64 + 1);
                                writer_clone.write_begin(txn_id).unwrap();
                                writer_clone
                                    .write_operation(
                                        txn_id,
                                        TableId::from(1),
                                        WriteOpType::Put,
                                        format!("key{}", txn_id).into_bytes(),
                                        b"value".to_vec(),
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
                });
            },
        );
    }

    group.finish();
}

fn bench_group_commit_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_throughput");
    group.throughput(Throughput::Elements(100));

    // Benchmark without group commit
    group.bench_function("memory_fs_no_group_commit_100_txns", |b| {
        b.iter(|| {
            let fs = MemoryFileSystem::new();
            let mut config = WalWriterConfig::default();
            config.group_commit.enabled = false;
            let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

            for txn_id in 1..=100 {
                let txn_id = TransactionId::from(txn_id);
                writer.write_begin(txn_id).unwrap();
                writer
                    .write_operation(
                        txn_id,
                        TableId::from(1),
                        WriteOpType::Put,
                        format!("key{}", txn_id).into_bytes(),
                        b"value".to_vec(),
                    )
                    .unwrap();
                writer.write_commit(txn_id).unwrap();
            }
        });
    });

    // Benchmark with group commit
    group.bench_function("memory_fs_with_group_commit_100_txns", |b| {
        b.iter(|| {
            let fs = MemoryFileSystem::new();
            let mut config = WalWriterConfig::default();
            config.group_commit = GroupCommitConfig::high_throughput();
            let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();

            for txn_id in 1..=100 {
                let txn_id = TransactionId::from(txn_id);
                writer.write_begin(txn_id).unwrap();
                writer
                    .write_operation(
                        txn_id,
                        TableId::from(1),
                        WriteOpType::Put,
                        format!("key{}", txn_id).into_bytes(),
                        b"value".to_vec(),
                    )
                    .unwrap();
                writer.write_commit(txn_id).unwrap();
            }
        });
    });

    group.finish();
}

fn bench_group_commit_configs(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_configs");

    let configs = vec![
        ("low_latency", GroupCommitConfig::low_latency()),
        ("balanced", GroupCommitConfig::balanced()),
        ("high_throughput", GroupCommitConfig::high_throughput()),
    ];

    for (name, config) in configs {
        group.bench_function(format!("memory_fs_{}", name), |b| {
            let fs = Arc::new(MemoryFileSystem::new());
            let mut wal_config = WalWriterConfig::default();
            wal_config.group_commit = config.clone();
            let writer = Arc::new(WalWriter::create(&*fs, "/bench.wal", wal_config).unwrap());

            b.iter(|| {
                let mut handles = vec![];
                for thread_id in 0..4 {
                    let writer_clone = writer.clone();
                    let handle = thread::spawn(move || {
                        for i in 0..25 {
                            let txn_id = TransactionId::from((thread_id * 25 + i) as u64 + 1);
                            writer_clone.write_begin(txn_id).unwrap();
                            writer_clone
                                .write_operation(
                                    txn_id,
                                    TableId::from(1),
                                    WriteOpType::Put,
                                    format!("key{}", txn_id).into_bytes(),
                                    b"value".to_vec(),
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
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_wal_writer_creation,
    bench_transactions,
    bench_write_operations,
    bench_flush_operations,
    bench_wal_reader,
    bench_recovery,
    bench_checkpoint,
    bench_complete_transactions,
    bench_compression,
    bench_encryption,
    bench_compression_and_encryption,
    bench_group_commit_single_thread,
    bench_group_commit_concurrent,
    bench_group_commit_throughput,
    bench_group_commit_configs,
);

criterion_main!(benches);
