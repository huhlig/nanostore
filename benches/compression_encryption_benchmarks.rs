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

//! Comprehensive benchmarks for compression and encryption features

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use nanostore::pager::{CompressionType, EncryptionType, Page, PageType, Pager, PagerConfig};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::{WalWriter, WalWriterConfig, WriteOpType};
use rand::Rng;
use std::hint::black_box;
// ============================================================================
// Helper Functions
// ============================================================================

/// Generate highly compressible data (repeated patterns)
fn generate_compressible_data(size: usize) -> Vec<u8> {
    let pattern = b"The quick brown fox jumps over the lazy dog. ";
    let mut data = Vec::with_capacity(size);
    while data.len() < size {
        data.extend_from_slice(pattern);
    }
    data.truncate(size);
    data
}

/// Generate moderately compressible data (text-like with some variation)
fn generate_moderate_data(size: usize) -> Vec<u8> {
    let patterns: Vec<&[u8]> = vec![
        b"Lorem ipsum dolor sit amet, consectetur adipiscing. ",
        b"Ut enim ad minim veniam, quis nostrud exercitation. ",
        b"Duis aute irure dolor in reprehenderit in voluptat. ",
        b"Excepteur sint occaecat cupidatat non proident sunt ",
    ];
    let mut data = Vec::with_capacity(size);
    let mut idx = 0;
    while data.len() < size {
        data.extend_from_slice(patterns[idx % patterns.len()]);
        idx += 1;
    }
    data.truncate(size);
    data
}

/// Generate incompressible data (random bytes)
fn generate_incompressible_data(size: usize) -> Vec<u8> {
    let mut data = vec![0u8; size];
    rand::rng().fill_bytes(&mut data);
    data
}

// ============================================================================
// Pager Compression Benchmarks
// ============================================================================

fn bench_pager_compression(c: &mut Criterion) {
    let mut group = c.benchmark_group("pager_compression");

    for &data_size in &[1024, 4096, 16384] {
        group.throughput(Throughput::Bytes(data_size as u64));

        // LZ4 Compression - Write
        group.bench_with_input(
            BenchmarkId::new("lz4_write", data_size),
            &data_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default().with_compression(CompressionType::Lz4);
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                page.header.compression = CompressionType::Lz4;
                page.data_mut()
                    .extend_from_slice(&generate_compressible_data(size));

                b.iter(|| {
                    pager.write_page(&page).unwrap();
                });
            },
        );

        // LZ4 Compression - Read
        group.bench_with_input(
            BenchmarkId::new("lz4_read", data_size),
            &data_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default().with_compression(CompressionType::Lz4);
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                page.header.compression = CompressionType::Lz4;
                page.data_mut()
                    .extend_from_slice(&generate_compressible_data(size));
                pager.write_page(&page).unwrap();

                b.iter(|| {
                    let read_page = pager.read_page(page_id).unwrap();
                    black_box(read_page);
                });
            },
        );

        // Zstd Compression - Write
        group.bench_with_input(
            BenchmarkId::new("zstd_write", data_size),
            &data_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default().with_compression(CompressionType::Zstd);
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                page.header.compression = CompressionType::Zstd;
                page.data_mut()
                    .extend_from_slice(&generate_compressible_data(size));

                b.iter(|| {
                    pager.write_page(&page).unwrap();
                });
            },
        );

        // Zstd Compression - Read
        group.bench_with_input(
            BenchmarkId::new("zstd_read", data_size),
            &data_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default().with_compression(CompressionType::Zstd);
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                page.header.compression = CompressionType::Zstd;
                page.data_mut()
                    .extend_from_slice(&generate_compressible_data(size));
                pager.write_page(&page).unwrap();

                b.iter(|| {
                    let read_page = pager.read_page(page_id).unwrap();
                    black_box(read_page);
                });
            },
        );

        // No Compression - Baseline Write
        group.bench_with_input(
            BenchmarkId::new("none_write", data_size),
            &data_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default();
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                page.data_mut()
                    .extend_from_slice(&generate_compressible_data(size));

                b.iter(|| {
                    pager.write_page(&page).unwrap();
                });
            },
        );

        // No Compression - Baseline Read
        group.bench_with_input(
            BenchmarkId::new("none_read", data_size),
            &data_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default();
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                page.data_mut()
                    .extend_from_slice(&generate_compressible_data(size));
                pager.write_page(&page).unwrap();

                b.iter(|| {
                    let read_page = pager.read_page(page_id).unwrap();
                    black_box(read_page);
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Pager Encryption Benchmarks
// ============================================================================

fn bench_pager_encryption(c: &mut Criterion) {
    let mut group = c.benchmark_group("pager_encryption");

    let encryption_key = [0x42u8; 32];

    for &data_size in &[1024, 4096, 16384] {
        group.throughput(Throughput::Bytes(data_size as u64));

        // AES-256-GCM Encryption - Write
        group.bench_with_input(
            BenchmarkId::new("aes256gcm_write", data_size),
            &data_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default()
                    .with_encryption(EncryptionType::Aes256Gcm, encryption_key);
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                page.header.encryption = EncryptionType::Aes256Gcm;
                page.data_mut().resize(size, 0xAB);

                b.iter(|| {
                    pager.write_page(&page).unwrap();
                });
            },
        );

        // AES-256-GCM Encryption - Read
        group.bench_with_input(
            BenchmarkId::new("aes256gcm_read", data_size),
            &data_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default()
                    .with_encryption(EncryptionType::Aes256Gcm, encryption_key);
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                page.header.encryption = EncryptionType::Aes256Gcm;
                page.data_mut().resize(size, 0xAB);
                pager.write_page(&page).unwrap();

                b.iter(|| {
                    let read_page = pager.read_page(page_id).unwrap();
                    black_box(read_page);
                });
            },
        );

        // No Encryption - Baseline Write
        group.bench_with_input(
            BenchmarkId::new("none_write", data_size),
            &data_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default();
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                page.data_mut().resize(size, 0xAB);

                b.iter(|| {
                    pager.write_page(&page).unwrap();
                });
            },
        );

        // No Encryption - Baseline Read
        group.bench_with_input(
            BenchmarkId::new("none_read", data_size),
            &data_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default();
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                page.data_mut().resize(size, 0xAB);
                pager.write_page(&page).unwrap();

                b.iter(|| {
                    let read_page = pager.read_page(page_id).unwrap();
                    black_box(read_page);
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Combined Pager Benchmarks (Compression + Encryption)
// ============================================================================

fn bench_pager_combined(c: &mut Criterion) {
    let mut group = c.benchmark_group("pager_combined");

    let encryption_key = [0x42u8; 32];
    let data_size = 4096;

    group.throughput(Throughput::Bytes(data_size as u64));

    // LZ4 + AES-256-GCM - Write
    group.bench_function("lz4_aes256gcm_write", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default()
            .with_compression(CompressionType::Lz4)
            .with_encryption(EncryptionType::Aes256Gcm, encryption_key);
        let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

        let mut page = Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
        page.header.compression = CompressionType::Lz4;
        page.header.encryption = EncryptionType::Aes256Gcm;
        page.data_mut()
            .extend_from_slice(&generate_compressible_data(data_size));

        b.iter(|| {
            pager.write_page(&page).unwrap();
        });
    });

    // LZ4 + AES-256-GCM - Read
    group.bench_function("lz4_aes256gcm_read", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default()
            .with_compression(CompressionType::Lz4)
            .with_encryption(EncryptionType::Aes256Gcm, encryption_key);
        let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

        let mut page = Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
        page.header.compression = CompressionType::Lz4;
        page.header.encryption = EncryptionType::Aes256Gcm;
        page.data_mut()
            .extend_from_slice(&generate_compressible_data(data_size));
        pager.write_page(&page).unwrap();

        b.iter(|| {
            let read_page = pager.read_page(page_id).unwrap();
            black_box(read_page);
        });
    });

    // Zstd + AES-256-GCM - Write
    group.bench_function("zstd_aes256gcm_write", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default()
            .with_compression(CompressionType::Zstd)
            .with_encryption(EncryptionType::Aes256Gcm, encryption_key);
        let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

        let mut page = Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
        page.header.compression = CompressionType::Zstd;
        page.header.encryption = EncryptionType::Aes256Gcm;
        page.data_mut()
            .extend_from_slice(&generate_compressible_data(data_size));

        b.iter(|| {
            pager.write_page(&page).unwrap();
        });
    });

    // Zstd + AES-256-GCM - Read
    group.bench_function("zstd_aes256gcm_read", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default()
            .with_compression(CompressionType::Zstd)
            .with_encryption(EncryptionType::Aes256Gcm, encryption_key);
        let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

        let mut page = Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
        page.header.compression = CompressionType::Zstd;
        page.header.encryption = EncryptionType::Aes256Gcm;
        page.data_mut()
            .extend_from_slice(&generate_compressible_data(data_size));
        pager.write_page(&page).unwrap();

        b.iter(|| {
            let read_page = pager.read_page(page_id).unwrap();
            black_box(read_page);
        });
    });

    group.finish();
}

// ============================================================================
// WAL Compression Benchmarks
// ============================================================================

fn bench_wal_compression(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_compression");

    for &value_size in &[64, 256, 1024, 4096] {
        group.throughput(Throughput::Bytes(value_size as u64));

        // LZ4 Compression - Write
        group.bench_with_input(
            BenchmarkId::new("lz4_write", value_size),
            &value_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let mut config = WalWriterConfig::default();
                config.compression = CompressionType::Lz4;
                config.max_wal_size = 16 * 1024 * 1024 * 1024;
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
                let value = generate_compressible_data(size);
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

        // Zstd Compression - Write
        group.bench_with_input(
            BenchmarkId::new("zstd_write", value_size),
            &value_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let mut config = WalWriterConfig::default();
                config.compression = CompressionType::Zstd;
                config.max_wal_size = 16 * 1024 * 1024 * 1024;
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
                let value = generate_compressible_data(size);
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

        // No Compression - Baseline
        group.bench_with_input(
            BenchmarkId::new("none_write", value_size),
            &value_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let mut config = WalWriterConfig::default();
                config.max_wal_size = 16 * 1024 * 1024 * 1024;
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
                let value = generate_compressible_data(size);
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
    }

    group.finish();
}

// ============================================================================
// WAL Encryption Benchmarks
// ============================================================================

fn bench_wal_encryption(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_encryption");

    let encryption_key = [0x42u8; 32];

    for &value_size in &[64, 256, 1024, 4096] {
        group.throughput(Throughput::Bytes(value_size as u64));

        // AES-256-GCM Encryption - Write
        group.bench_with_input(
            BenchmarkId::new("aes256gcm_write", value_size),
            &value_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let mut config = WalWriterConfig::default();
                config.encryption = EncryptionType::Aes256Gcm;
                config.encryption_key = Some(encryption_key);
                config.max_wal_size = 16 * 1024 * 1024 * 1024;
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
                let value = vec![0xAB; size];
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

        // No Encryption - Baseline
        group.bench_with_input(
            BenchmarkId::new("none_write", value_size),
            &value_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let mut config = WalWriterConfig::default();
                config.max_wal_size = 16 * 1024 * 1024 * 1024;
                let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
                let value = vec![0xAB; size];
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
    }

    group.finish();
}

// ============================================================================
// Combined WAL Benchmarks (Compression + Encryption)
// ============================================================================

fn bench_wal_combined(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_combined");

    let encryption_key = [0x42u8; 32];
    let value_size = 1024;

    group.throughput(Throughput::Bytes(value_size as u64));

    // LZ4 + AES-256-GCM
    group.bench_function("lz4_aes256gcm_write", |b| {
        let fs = MemoryFileSystem::new();
        let mut config = WalWriterConfig::default();
        config.compression = CompressionType::Lz4;
        config.max_wal_size = 16 * 1024 * 1024 * 1024;
        config.encryption = EncryptionType::Aes256Gcm;
        config.encryption_key = Some(encryption_key);
        let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
        let value = generate_compressible_data(value_size);
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
    });

    // Zstd + AES-256-GCM
    group.bench_function("zstd_aes256gcm_write", |b| {
        let fs = MemoryFileSystem::new();
        let mut config = WalWriterConfig::default();
        config.compression = CompressionType::Zstd;
        config.max_wal_size = 16 * 1024 * 1024 * 1024;
        config.encryption = EncryptionType::Aes256Gcm;
        config.encryption_key = Some(encryption_key);
        let writer = WalWriter::create(&fs, "/bench.wal", config).unwrap();
        let value = generate_compressible_data(value_size);
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
    });

    group.finish();
}

// ============================================================================
// Data Pattern Benchmarks
// ============================================================================

fn bench_data_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("data_patterns");

    let data_size = 4096;
    group.throughput(Throughput::Bytes(data_size as u64));

    // Highly compressible data with LZ4
    group.bench_function("compressible_lz4", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default().with_compression(CompressionType::Lz4);
        let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

        let mut page = Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
        page.header.compression = CompressionType::Lz4;
        page.data_mut()
            .extend_from_slice(&generate_compressible_data(data_size));

        b.iter(|| {
            pager.write_page(&page).unwrap();
        });
    });

    // Moderately compressible data with LZ4
    group.bench_function("moderate_lz4", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default().with_compression(CompressionType::Lz4);
        let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

        let mut page = Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
        page.header.compression = CompressionType::Lz4;
        page.data_mut()
            .extend_from_slice(&generate_moderate_data(data_size));

        b.iter(|| {
            pager.write_page(&page).unwrap();
        });
    });

    // Incompressible data with LZ4
    group.bench_function("incompressible_lz4", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default().with_compression(CompressionType::Lz4);
        let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

        let mut page = Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
        page.header.compression = CompressionType::Lz4;
        page.data_mut()
            .extend_from_slice(&generate_incompressible_data(data_size));

        b.iter(|| {
            pager.write_page(&page).unwrap();
        });
    });

    // Highly compressible data with Zstd
    group.bench_function("compressible_zstd", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default().with_compression(CompressionType::Zstd);
        let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

        let mut page = Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
        page.header.compression = CompressionType::Zstd;
        page.data_mut()
            .extend_from_slice(&generate_compressible_data(data_size));

        b.iter(|| {
            pager.write_page(&page).unwrap();
        });
    });

    // Moderately compressible data with Zstd
    group.bench_function("moderate_zstd", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default().with_compression(CompressionType::Zstd);
        let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

        let mut page = Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
        page.header.compression = CompressionType::Zstd;
        page.data_mut()
            .extend_from_slice(&generate_moderate_data(data_size));

        b.iter(|| {
            pager.write_page(&page).unwrap();
        });
    });

    // Incompressible data with Zstd
    group.bench_function("incompressible_zstd", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default().with_compression(CompressionType::Zstd);
        let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

        let mut page = Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
        page.header.compression = CompressionType::Zstd;
        page.data_mut()
            .extend_from_slice(&generate_incompressible_data(data_size));

        b.iter(|| {
            pager.write_page(&page).unwrap();
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_pager_compression,
    bench_pager_encryption,
    bench_pager_combined,
    bench_wal_compression,
    bench_wal_encryption,
    bench_wal_combined,
    bench_data_patterns,
);

criterion_main!(benches);
