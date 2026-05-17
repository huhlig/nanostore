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

//! Benchmarks for PagedBloomFilter operations

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use nanokv::pager::{PageSize, Pager, PagerConfig};
use nanokv::table::bloom::PagedBloomFilter;
use nanokv::txn::TransactionId;
use nanokv::types::TableId;
use nanokv::vfs::MemoryFileSystem;
use nanokv::wal::LogSequenceNumber;
use std::sync::Arc;

fn create_test_pager() -> Arc<Pager<MemoryFileSystem>> {
    let fs = Arc::new(MemoryFileSystem::new());
    let pager = Pager::create(
        fs.as_ref(),
        "bench.db",
        PagerConfig::new().with_page_size(PageSize::Size4KB),
    )
    .unwrap();
    Arc::new(pager)
}

fn bench_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("bloom_insert");

    for size in [100, 1000, 10000] {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter_batched(
                || {
                    let pager = create_test_pager();
                    PagedBloomFilter::new(
                        TableId::from(1),
                        "bench_bloom".to_string(),
                        pager,
                        size,
                        10,
                        None,
                    )
                    .unwrap()
                },
                |mut filter| {
                    let tx_id = TransactionId::from(1);
                    let commit_lsn = LogSequenceNumber::from(1);
                    for i in 0..size {
                        filter
                            .insert(black_box(&i.to_le_bytes()), tx_id, commit_lsn)
                            .unwrap();
                    }
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_contains(c: &mut Criterion) {
    let mut group = c.benchmark_group("bloom_contains");

    for size in [100, 1000, 10000] {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            // Setup: create and populate filter
            let pager = create_test_pager();
            let mut filter = PagedBloomFilter::new(
                TableId::from(1),
                "bench_bloom".to_string(),
                pager,
                size,
                10,
                None,
            )
            .unwrap();

            let tx_id = TransactionId::from(1);
            let commit_lsn = LogSequenceNumber::from(1);
            for i in 0..size {
                filter.insert(&i.to_le_bytes(), tx_id, commit_lsn).unwrap();
            }

            // Benchmark lookups
            b.iter(|| {
                for i in 0..size {
                    black_box(filter.contains(black_box(&i.to_le_bytes())).unwrap());
                }
            });
        });
    }

    group.finish();
}

fn bench_false_positive_check(c: &mut Criterion) {
    let mut group = c.benchmark_group("bloom_false_positive");

    for size in [1000, 10000] {
        group.throughput(Throughput::Elements(1000));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            // Setup: create and populate filter
            let pager = create_test_pager();
            let mut filter = PagedBloomFilter::new(
                TableId::from(1),
                "bench_bloom".to_string(),
                pager,
                size,
                10,
                None,
            )
            .unwrap();

            let tx_id = TransactionId::from(1);
            let commit_lsn = LogSequenceNumber::from(1);
            for i in 0..size {
                filter.insert(&i.to_le_bytes(), tx_id, commit_lsn).unwrap();
            }

            // Benchmark lookups for non-existent keys
            b.iter(|| {
                for i in size..(size + 1000) {
                    black_box(filter.contains(black_box(&i.to_le_bytes())).unwrap());
                }
            });
        });
    }

    group.finish();
}

fn bench_different_bits_per_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("bloom_bits_per_key");

    let size = 1000;
    for bits_per_key in [5, 10, 15, 20] {
        group.bench_with_input(
            BenchmarkId::from_parameter(bits_per_key),
            &bits_per_key,
            |b, &bits_per_key| {
                b.iter_batched(
                    || {
                        let pager = create_test_pager();
                        let mut filter = PagedBloomFilter::new(
                            TableId::from(1),
                            "bench_bloom".to_string(),
                            pager,
                            size,
                            bits_per_key,
                            None,
                        )
                        .unwrap();

                        let tx_id = TransactionId::from(1);
                        let commit_lsn = LogSequenceNumber::from(1);
                        for i in 0..size {
                            filter.insert(&i.to_le_bytes(), tx_id, commit_lsn).unwrap();
                        }

                        filter
                    },
                    |filter| {
                        for i in 0..size {
                            black_box(filter.contains(black_box(&i.to_le_bytes())).unwrap());
                        }
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_different_hash_functions(c: &mut Criterion) {
    let mut group = c.benchmark_group("bloom_hash_functions");

    let size = 1000;
    for num_hash in [3, 5, 7, 10] {
        group.bench_with_input(
            BenchmarkId::from_parameter(num_hash),
            &num_hash,
            |b, &num_hash| {
                b.iter_batched(
                    || {
                        let pager = create_test_pager();
                        let mut filter = PagedBloomFilter::new(
                            TableId::from(1),
                            "bench_bloom".to_string(),
                            pager,
                            size,
                            10,
                            Some(num_hash),
                        )
                        .unwrap();

                        let tx_id = TransactionId::from(1);
                        let commit_lsn = LogSequenceNumber::from(1);
                        for i in 0..size {
                            filter.insert(&i.to_le_bytes(), tx_id, commit_lsn).unwrap();
                        }

                        filter
                    },
                    |filter| {
                        for i in 0..size {
                            black_box(filter.contains(black_box(&i.to_le_bytes())).unwrap());
                        }
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_clear(c: &mut Criterion) {
    let mut group = c.benchmark_group("bloom_clear");

    for size in [100, 1000, 10000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter_batched(
                || {
                    let pager = create_test_pager();
                    let mut filter = PagedBloomFilter::new(
                        TableId::from(1),
                        "bench_bloom".to_string(),
                        pager,
                        size,
                        10,
                        None,
                    )
                    .unwrap();

                    let tx_id = TransactionId::from(1);
                    let commit_lsn = LogSequenceNumber::from(1);
                    for i in 0..size {
                        filter.insert(&i.to_le_bytes(), tx_id, commit_lsn).unwrap();
                    }

                    filter
                },
                |mut filter| {
                    filter.clear().unwrap();
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_persistence(c: &mut Criterion) {
    let mut group = c.benchmark_group("bloom_persistence");

    let size = 1000;

    // Benchmark write (create + populate)
    group.bench_function("write", |b| {
        b.iter_batched(
            || create_test_pager(),
            |pager| {
                let mut filter = PagedBloomFilter::new(
                    TableId::from(1),
                    "bench_bloom".to_string(),
                    pager,
                    size,
                    10,
                    None,
                )
                .unwrap();

                let tx_id = TransactionId::from(1);
                let commit_lsn = LogSequenceNumber::from(1);
                for i in 0..size {
                    filter
                        .insert(black_box(&i.to_le_bytes()), tx_id, commit_lsn)
                        .unwrap();
                }

                black_box(filter.root_page_id())
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // Benchmark read (open existing)
    group.bench_function("read", |b| {
        b.iter_batched(
            || {
                let pager = create_test_pager();
                let mut filter = PagedBloomFilter::new(
                    TableId::from(1),
                    "bench_bloom".to_string(),
                    pager.clone(),
                    size,
                    10,
                    None,
                )
                .unwrap();

                let tx_id = TransactionId::from(1);
                let commit_lsn = LogSequenceNumber::from(1);
                for i in 0..size {
                    filter.insert(&i.to_le_bytes(), tx_id, commit_lsn).unwrap();
                }

                (pager, filter.root_page_id())
            },
            |(pager, root_page_id)| {
                black_box(
                    PagedBloomFilter::open(
                        TableId::from(1),
                        "bench_bloom".to_string(),
                        pager,
                        root_page_id,
                    )
                    .unwrap(),
                )
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_insert,
    bench_contains,
    bench_false_positive_check,
    bench_different_bits_per_key,
    bench_different_hash_functions,
    bench_clear,
    bench_persistence,
);

criterion_main!(benches);

// Made with Bob
