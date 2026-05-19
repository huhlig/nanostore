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

//! Benchmarks for Pager implementation

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use nanostore::pager::{
    CompressionType, EncryptionType, Page, PageId, PageSize, PageType, Pager, PagerConfig,
};
use nanostore::vfs::{LocalFileSystem, MemoryFileSystem};
use std::hint::black_box;

// ============================================================================
// Pager Creation Benchmarks
// ============================================================================

fn bench_pager_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("pager_creation");

    for page_size in [PageSize::Size4KB, PageSize::Size8KB, PageSize::Size16KB].iter() {
        group.bench_with_input(
            BenchmarkId::new("memory_fs", format!("{:?}", page_size)),
            page_size,
            |b, &page_size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default().with_page_size(page_size);
                let mut counter = 0;
                b.iter(|| {
                    let path = format!("/bench_{}.db", counter);
                    counter += 1;
                    let pager = Pager::create(&fs, &path, config.clone()).unwrap();
                    black_box(pager);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("local_fs", format!("{:?}", page_size)),
            page_size,
            |b, &page_size| {
                let temp_dir = tempfile::tempdir().unwrap();
                let fs = LocalFileSystem::new(temp_dir.path());
                let config = PagerConfig::default().with_page_size(page_size);
                let mut counter = 0;
                b.iter(|| {
                    let path = format!("/bench_{}.db", counter);
                    counter += 1;
                    let pager = Pager::create(&fs, &path, config.clone()).unwrap();
                    black_box(pager);
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Page Allocation Benchmarks
// ============================================================================

fn bench_page_allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("page_allocation");

    group.bench_function("memory_fs_allocate_single", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default();
        let pager = Pager::create(&fs, "/bench.db", config).unwrap();

        b.iter(|| {
            let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            black_box(page_id);
        });
    });

    group.bench_function("local_fs_allocate_single", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let config = PagerConfig::default();
        let pager = Pager::create(&fs, "/bench.db", config).unwrap();

        b.iter(|| {
            let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            black_box(page_id);
        });
    });

    group.bench_function("memory_fs_allocate_and_free", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default();
        let pager = Pager::create(&fs, "/bench.db", config).unwrap();

        b.iter(|| {
            let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            pager.free_page(page_id).unwrap();
            black_box(page_id);
        });
    });

    group.bench_function("local_fs_allocate_and_free", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let config = PagerConfig::default();
        let pager = Pager::create(&fs, "/bench.db", config).unwrap();

        b.iter(|| {
            let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            pager.free_page(page_id).unwrap();
            black_box(page_id);
        });
    });

    group.finish();
}

// ============================================================================
// Page Read/Write Benchmarks
// ============================================================================

fn bench_page_read_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("page_read_write");

    for page_size in [PageSize::Size4KB, PageSize::Size8KB, PageSize::Size16KB].iter() {
        let data_size = page_size.data_size();
        group.throughput(Throughput::Bytes(data_size as u64));

        group.bench_with_input(
            BenchmarkId::new("memory_fs_write", format!("{:?}", page_size)),
            page_size,
            |b, &page_size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default().with_page_size(page_size);
                let pager = Pager::create(&fs, "/bench.db", config).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page = Page::new(page_id, PageType::BTreeLeaf, page_size.data_size());
                page.data_mut().resize(page_size.data_size(), 0xAB);

                b.iter(|| {
                    pager.write_page(&page).unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("local_fs_write", format!("{:?}", page_size)),
            page_size,
            |b, &page_size| {
                let temp_dir = tempfile::tempdir().unwrap();
                let fs = LocalFileSystem::new(temp_dir.path());
                let config = PagerConfig::default().with_page_size(page_size);
                let pager = Pager::create(&fs, "/bench.db", config).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page = Page::new(page_id, PageType::BTreeLeaf, page_size.data_size());
                page.data_mut().resize(page_size.data_size(), 0xAB);

                b.iter(|| {
                    pager.write_page(&page).unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("memory_fs_read", format!("{:?}", page_size)),
            page_size,
            |b, &page_size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default().with_page_size(page_size);
                let pager = Pager::create(&fs, "/bench.db", config).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page = Page::new(page_id, PageType::BTreeLeaf, page_size.data_size());
                page.data_mut().resize(page_size.data_size(), 0xAB);
                pager.write_page(&page).unwrap();

                b.iter(|| {
                    let read_page = pager.read_page(page_id).unwrap();
                    black_box(read_page);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("local_fs_read", format!("{:?}", page_size)),
            page_size,
            |b, &page_size| {
                let temp_dir = tempfile::tempdir().unwrap();
                let fs = LocalFileSystem::new(temp_dir.path());
                let config = PagerConfig::default().with_page_size(page_size);
                let pager = Pager::create(&fs, "/bench.db", config).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page = Page::new(page_id, PageType::BTreeLeaf, page_size.data_size());
                page.data_mut().resize(page_size.data_size(), 0xAB);
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
// Page Serialization Benchmarks
// ============================================================================

fn bench_page_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("page_serialization");

    for page_size in [PageSize::Size4KB, PageSize::Size8KB, PageSize::Size16KB].iter() {
        let data_size = page_size.data_size();
        group.throughput(Throughput::Bytes(page_size.to_u32() as u64));

        group.bench_with_input(
            BenchmarkId::new("serialize", format!("{:?}", page_size)),
            page_size,
            |b, &page_size| {
                let mut page =
                    Page::new(PageId::from(1), PageType::BTreeLeaf, page_size.data_size());
                page.data_mut().resize(data_size, 0xAB);

                b.iter(|| {
                    let bytes = page.to_bytes(page_size.to_u32() as usize, None).unwrap();
                    black_box(bytes);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("deserialize", format!("{:?}", page_size)),
            page_size,
            |b, &page_size| {
                let mut page =
                    Page::new(PageId::from(1), PageType::BTreeLeaf, page_size.data_size());
                page.data_mut().resize(data_size, 0xAB);
                let bytes = page.to_bytes(page_size.to_u32() as usize, None).unwrap();

                b.iter(|| {
                    let page = Page::from_bytes(&bytes, true, None).unwrap();
                    black_box(page);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("checksum_calculation", format!("{:?}", page_size)),
            page_size,
            |b, &page_size| {
                let mut page =
                    Page::new(PageId::from(1), PageType::BTreeLeaf, page_size.data_size());
                page.data_mut().resize(data_size, 0xAB);

                b.iter(|| {
                    let checksum = page.calculate_checksum();
                    black_box(checksum);
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Bulk Operations Benchmarks
// ============================================================================

fn bench_bulk_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_operations");

    for num_pages in [10, 50, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("memory_fs_allocate_many", num_pages),
            num_pages,
            |b, &num_pages| {
                b.iter(|| {
                    let fs = MemoryFileSystem::new();
                    let config = PagerConfig::default();
                    let pager = Pager::create(&fs, "/bench.db", config).unwrap();

                    for _ in 0..num_pages {
                        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
                        black_box(page_id);
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("memory_fs_write_many", num_pages),
            num_pages,
            |b, &num_pages| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default();
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();

                let mut pages = Vec::new();
                for _ in 0..num_pages {
                    let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
                    let mut page =
                        Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                    page.data_mut().resize(1024, 0xAB);
                    pages.push(page);
                }

                b.iter(|| {
                    for page in &pages {
                        pager.write_page(page).unwrap();
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("memory_fs_read_many", num_pages),
            num_pages,
            |b, &num_pages| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default();
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();

                let mut page_ids = Vec::new();
                for _ in 0..num_pages {
                    let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
                    let mut page =
                        Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                    page.data_mut().resize(1024, 0xAB);
                    pager.write_page(&page).unwrap();
                    page_ids.push(page_id);
                }

                b.iter(|| {
                    for &page_id in &page_ids {
                        let page = pager.read_page(page_id).unwrap();
                        black_box(page);
                    }
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Free List Benchmarks
// ============================================================================

fn bench_free_list_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("free_list_operations");

    group.bench_function("memory_fs_free_and_reuse_cycle", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::default();
        let pager = Pager::create(&fs, "/bench.db", config).unwrap();

        b.iter(|| {
            // Allocate
            let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            // Free
            pager.free_page(page_id).unwrap();
            // Reuse
            let reused_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            black_box(reused_id);
        });
    });

    group.bench_function("memory_fs_free_many_pages", |b| {
        b.iter(|| {
            let fs = MemoryFileSystem::new();
            let config = PagerConfig::default();
            let pager = Pager::create(&fs, "/bench.db", config).unwrap();

            // Allocate 100 pages
            let mut page_ids = Vec::new();
            for _ in 0..100 {
                page_ids.push(pager.allocate_page(PageType::BTreeLeaf).unwrap());
            }

            // Free all pages
            for page_id in page_ids {
                pager.free_page(page_id).unwrap();
            }
        });
    });

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

        for data_size in [1024, 2048, 4000].iter() {
            group.throughput(Throughput::Bytes(*data_size as u64));

            group.bench_with_input(
                BenchmarkId::new(format!("memory_fs_write_{}", compression_name), data_size),
                &(*compression, *data_size),
                |b, &(compression, data_size)| {
                    let fs = MemoryFileSystem::new();
                    let config = PagerConfig::default().with_compression(compression);
                    let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                    let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                    let mut page =
                        Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                    // Create compressible data (repeated pattern)
                    let pattern = b"The quick brown fox jumps over the lazy dog. ";
                    for _ in 0..(data_size / pattern.len() + 1) {
                        page.data_mut().extend_from_slice(pattern);
                    }
                    page.data_mut().truncate(data_size);

                    b.iter(|| {
                        pager.write_page(&page).unwrap();
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("memory_fs_read_{}", compression_name), data_size),
                &(*compression, *data_size),
                |b, &(compression, data_size)| {
                    let fs = MemoryFileSystem::new();
                    let config = PagerConfig::default().with_compression(compression);
                    let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                    let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                    let mut page =
                        Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                    let pattern = b"The quick brown fox jumps over the lazy dog. ";
                    for _ in 0..(data_size / pattern.len() + 1) {
                        page.data_mut().extend_from_slice(pattern);
                    }
                    page.data_mut().truncate(data_size);
                    pager.write_page(&page).unwrap();

                    b.iter(|| {
                        let read_page = pager.read_page(page_id).unwrap();
                        black_box(read_page);
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

        for data_size in [1024, 2048, 4000].iter() {
            group.throughput(Throughput::Bytes(*data_size as u64));

            group.bench_with_input(
                BenchmarkId::new(format!("memory_fs_write_{}", encryption_name), data_size),
                &(*encrypted, *data_size),
                |b, &(encrypted, data_size)| {
                    let fs = MemoryFileSystem::new();
                    let mut config = PagerConfig::default();
                    if encrypted {
                        config = config.with_encryption(EncryptionType::Aes256Gcm, encryption_key);
                    }
                    let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                    let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                    let mut page =
                        Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                    page.data_mut().resize(data_size, 0xAB);

                    b.iter(|| {
                        pager.write_page(&page).unwrap();
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("memory_fs_read_{}", encryption_name), data_size),
                &(*encrypted, *data_size),
                |b, &(encrypted, data_size)| {
                    let fs = MemoryFileSystem::new();
                    let mut config = PagerConfig::default();
                    if encrypted {
                        config = config.with_encryption(EncryptionType::Aes256Gcm, encryption_key);
                    }
                    let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                    let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                    let mut page =
                        Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                    page.data_mut().resize(data_size, 0xAB);
                    pager.write_page(&page).unwrap();

                    b.iter(|| {
                        let read_page = pager.read_page(page_id).unwrap();
                        black_box(read_page);
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
    let data_size = 4000;

    group.throughput(Throughput::Bytes(data_size as u64));

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
                let config = PagerConfig::default()
                    .with_compression(*compression)
                    .with_encryption(EncryptionType::Aes256Gcm, encryption_key);
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                let pattern = b"The quick brown fox jumps over the lazy dog. ";
                for _ in 0..(data_size / pattern.len() + 1) {
                    page.data_mut().extend_from_slice(pattern);
                }
                page.data_mut().truncate(data_size);

                b.iter(|| {
                    pager.write_page(&page).unwrap();
                });
            },
        );

        group.bench_function(
            format!("memory_fs_read_{}_encrypted", compression_name),
            |b| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default()
                    .with_compression(*compression)
                    .with_encryption(EncryptionType::Aes256Gcm, encryption_key);
                let pager = Pager::create(&fs, "/bench.db", config.clone()).unwrap();
                let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, config.page_size.data_size());
                let pattern = b"The quick brown fox jumps over the lazy dog. ";
                for _ in 0..(data_size / pattern.len() + 1) {
                    page.data_mut().extend_from_slice(pattern);
                }
                page.data_mut().truncate(data_size);
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

criterion_group!(
    benches,
    bench_pager_creation,
    bench_page_allocation,
    bench_page_read_write,
    bench_page_serialization,
    bench_bulk_operations,
    bench_free_list_operations,
    bench_compression,
    bench_encryption,
    bench_compression_and_encryption,
);

criterion_main!(benches);
