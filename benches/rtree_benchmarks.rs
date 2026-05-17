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

//! Benchmarks for R-Tree geospatial operations

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use nanokv::pager::{PageSize, Pager, PagerConfig};
use nanokv::table::{GeoPoint, GeoSpatial, GeometryRef, PagedRTree, SpatialConfig, SplitStrategy};
use nanokv::txn::TransactionId;
use nanokv::types::TableId;
use nanokv::vfs::MemoryFileSystem;
use nanokv::wal::LogSequenceNumber;
use std::hint::black_box;
use std::sync::Arc;

fn create_test_pager(fs: &MemoryFileSystem, path: &str) -> Arc<Pager<MemoryFileSystem>> {
    let config = PagerConfig::new()
        .with_page_size(PageSize::Size16KB)
        .with_cache_capacity(0);
    Arc::new(Pager::create(fs, path, config).unwrap())
}

fn create_rtree(
    fs: &MemoryFileSystem,
    path: &str,
    config: SpatialConfig,
) -> PagedRTree<MemoryFileSystem> {
    let pager = create_test_pager(fs, path);
    PagedRTree::new(TableId::from(1), "bench_rtree".to_string(), pager, config).unwrap()
}

fn generate_points(count: usize, seed: u64) -> Vec<(Vec<u8>, GeoPoint)> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    (0..count)
        .map(|i| {
            let mut hasher = DefaultHasher::new();
            seed.hash(&mut hasher);
            i.hash(&mut hasher);
            let h = hasher.finish();

            let x = ((h >> 32) % 10000) as f64 / 100.0;
            let y = (h % 10000) as f64 / 100.0;

            (format!("point_{}", i).into_bytes(), GeoPoint { x, y })
        })
        .collect()
}

// ============================================================================
// Insertion Throughput Benchmarks
// ============================================================================

fn bench_insertion_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("insertion_throughput");

    for count in [100, 1_000, 10_000].iter() {
        group.throughput(Throughput::Elements(*count as u64));

        group.bench_with_input(BenchmarkId::new("points", count), count, |b, &count| {
            let points = generate_points(count, 42);
            b.iter(|| {
                let fs = MemoryFileSystem::new();
                let mut rtree = create_rtree(&fs, "/bench_insert.db", SpatialConfig::default());

                for (id, point) in &points {
                    rtree
                        .insert_geometry(
                            id,
                            GeometryRef::Point(*point),
                            TransactionId::from(0),
                            LogSequenceNumber::from(0),
                        )
                        .unwrap();
                }
                black_box(rtree);
            });
        });
    }

    group.finish();
}

// ============================================================================
// Insertion Throughput by Split Strategy
// ============================================================================

fn bench_insertion_by_strategy(c: &mut Criterion) {
    let mut group = c.benchmark_group("insertion_by_strategy");

    let strategies = [
        ("Linear", SplitStrategy::Linear),
        ("Quadratic", SplitStrategy::Quadratic),
        ("RStar", SplitStrategy::RStar),
    ];

    for count in [1_000, 10_000].iter() {
        group.throughput(Throughput::Elements(*count as u64));
        let points = generate_points(*count, 42);

        for (name, strategy) in &strategies {
            group.bench_with_input(
                BenchmarkId::new(name.to_string(), count),
                &(points.clone(), *strategy),
                |b, (points, strategy)| {
                    let config = SpatialConfig::default()
                        .with_max_entries(64)
                        .with_split_strategy(*strategy);
                    b.iter(|| {
                        let fs = MemoryFileSystem::new();
                        let mut rtree = create_rtree(&fs, "/bench_strategy.db", config.clone());

                        for (id, point) in points {
                            rtree
                                .insert_geometry(
                                    id,
                                    GeometryRef::Point(*point),
                                    TransactionId::from(0),
                                    LogSequenceNumber::from(0),
                                )
                                .unwrap();
                        }
                        black_box(rtree);
                    });
                },
            );
        }
    }

    group.finish();
}

// ============================================================================
// Query Latency Benchmarks - Intersects
// ============================================================================

fn bench_intersects_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("intersects_query");

    for count in [1_000, 10_000].iter() {
        let points = generate_points(*count, 42);

        for query_size in [1.0, 10.0, 50.0].iter() {
            group.bench_with_input(
                BenchmarkId::new(format!("query_size_{}", query_size), count),
                &(count, points.clone(), *query_size),
                |b, (_, points, query_size)| {
                    let fs = MemoryFileSystem::new();
                    let mut rtree =
                        create_rtree(&fs, "/bench_intersects.db", SpatialConfig::default());

                    for (id, point) in points {
                        rtree
                            .insert_geometry(
                                &id,
                                GeometryRef::Point(*point),
                                TransactionId::from(0),
                                LogSequenceNumber::from(0),
                            )
                            .unwrap();
                    }

                    let query = GeometryRef::BoundingBox {
                        min: GeoPoint {
                            x: 50.0 - query_size / 2.0,
                            y: 50.0 - query_size / 2.0,
                        },
                        max: GeoPoint {
                            x: 50.0 + query_size / 2.0,
                            y: 50.0 + query_size / 2.0,
                        },
                    };

                    b.iter(|| {
                        let results = rtree.intersects(query.clone(), 1000).unwrap();
                        black_box(results);
                    });
                },
            );
        }
    }

    group.finish();
}

// ============================================================================
// Query Latency Benchmarks - Nearest Neighbor
// ============================================================================

fn bench_nearest_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("nearest_query");

    for count in [1_000, 10_000].iter() {
        let points = generate_points(*count, 42);

        for k in [1, 10, 100].iter() {
            group.bench_with_input(
                BenchmarkId::new(format!("k_{}", k), count),
                &(count, points.clone(), *k),
                |b, (_, points, k)| {
                    let fs = MemoryFileSystem::new();
                    let mut rtree =
                        create_rtree(&fs, "/bench_nearest.db", SpatialConfig::default());

                    for (id, point) in points {
                        rtree
                            .insert_geometry(
                                &id,
                                GeometryRef::Point(*point),
                                TransactionId::from(0),
                                LogSequenceNumber::from(0),
                            )
                            .unwrap();
                    }

                    let query_point = GeoPoint { x: 50.0, y: 50.0 };

                    b.iter(|| {
                        let results = rtree.nearest(query_point, *k).unwrap();
                        black_box(results);
                    });
                },
            );
        }
    }

    group.finish();
}

// ============================================================================
// Tree Height vs Dataset Size
// ============================================================================

fn bench_tree_height(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_height");

    for count in [100, 1_000, 10_000, 100_000].iter() {
        group.throughput(Throughput::Elements(*count as u64));

        group.bench_with_input(BenchmarkId::new("height", count), count, |b, &count| {
            let points = generate_points(count, 42);
            b.iter(|| {
                let fs = MemoryFileSystem::new();
                let mut rtree = create_rtree(&fs, "/bench_height.db", SpatialConfig::default());

                for (id, point) in &points {
                    rtree
                        .insert_geometry(
                            id,
                            GeometryRef::Point(*point),
                            TransactionId::from(0),
                            LogSequenceNumber::from(0),
                        )
                        .unwrap();
                }

                let stats = rtree.stats().unwrap();
                black_box(stats);
            });
        });
    }

    group.finish();
}

// ============================================================================
// Memory Usage Benchmarks (via page allocation)
// ============================================================================

fn bench_memory_usage(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_usage");

    for count in [1_000, 10_000, 100_000].iter() {
        group.throughput(Throughput::Elements(*count as u64));

        group.bench_with_input(BenchmarkId::new("pages", count), count, |b, &count| {
            let points = generate_points(count, 42);
            b.iter(|| {
                let fs = MemoryFileSystem::new();
                let mut rtree = create_rtree(&fs, "/bench_memory.db", SpatialConfig::default());

                for (id, point) in &points {
                    rtree
                        .insert_geometry(
                            id,
                            GeometryRef::Point(*point),
                            TransactionId::from(0),
                            LogSequenceNumber::from(0),
                        )
                        .unwrap();
                }

                let stats = rtree.stats().unwrap();
                black_box(stats);
            });
        });
    }

    group.finish();
}

// ============================================================================
// Node Capacity Comparison
// ============================================================================

fn bench_node_capacity(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_capacity");

    let capacities = [20, 50, 100, 200];
    let count = 10_000;
    let points = generate_points(count, 42);

    for capacity in &capacities {
        group.throughput(Throughput::Elements(count as u64));

        group.bench_with_input(
            BenchmarkId::new(format!("capacity_{}", capacity), count),
            &(*capacity, points.clone()),
            |b, (capacity, points)| {
                let config = SpatialConfig::default().with_max_entries(*capacity);
                b.iter(|| {
                    let fs = MemoryFileSystem::new();
                    let mut rtree = create_rtree(&fs, "/bench_capacity.db", config.clone());

                    for (id, point) in points {
                        rtree
                            .insert_geometry(
                                id,
                                GeometryRef::Point(*point),
                                TransactionId::from(0),
                                LogSequenceNumber::from(0),
                            )
                            .unwrap();
                    }

                    let stats = rtree.stats().unwrap();
                    black_box(stats);
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Combined Strategy and Capacity Benchmark
// ============================================================================

fn bench_strategy_capacity_matrix(c: &mut Criterion) {
    let mut group = c.benchmark_group("strategy_capacity_matrix");

    let strategies = [
        ("Linear", SplitStrategy::Linear),
        ("Quadratic", SplitStrategy::Quadratic),
        ("RStar", SplitStrategy::RStar),
    ];

    let capacities = [50, 100, 200];
    let count = 5_000;
    let points = generate_points(count, 42);

    for (strategy_name, strategy) in &strategies {
        for capacity in &capacities {
            let config = SpatialConfig::default()
                .with_max_entries(*capacity)
                .with_split_strategy(*strategy);

            group.bench_with_input(
                BenchmarkId::new(format!("{}_cap_{}", strategy_name, capacity), count),
                &(config, points.clone()),
                |b, (config, points)| {
                    b.iter(|| {
                        let fs = MemoryFileSystem::new();
                        let mut rtree = create_rtree(&fs, "/bench_matrix.db", config.clone());

                        for (id, point) in points {
                            rtree
                                .insert_geometry(
                                    id,
                                    GeometryRef::Point(*point),
                                    TransactionId::from(0),
                                    LogSequenceNumber::from(0),
                                )
                                .unwrap();
                        }

                        let stats = rtree.stats().unwrap();
                        black_box(stats);
                    });
                },
            );
        }
    }

    group.finish();
}

// ============================================================================
// Bulk Loading vs Sequential Insertion Benchmark
// ============================================================================

fn bench_bulk_loading(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_loading");

    for count in [1_000, 10_000, 100_000].iter() {
        group.throughput(Throughput::Elements(*count as u64));
        let points = generate_points(*count, 42);

        // Sequential insertion benchmark
        group.bench_with_input(
            BenchmarkId::new("sequential", count),
            &points.clone(),
            |b, points| {
                b.iter(|| {
                    let fs = MemoryFileSystem::new();
                    let mut rtree = create_rtree(
                        &fs,
                        "/bench_sequential.db",
                        SpatialConfig::default().with_max_entries(64),
                    );

                    for (id, point) in points {
                        rtree
                            .insert_geometry(
                                id,
                                GeometryRef::Point(*point),
                                TransactionId::from(0),
                                LogSequenceNumber::from(0),
                            )
                            .unwrap();
                    }

                    let stats = rtree.stats().unwrap();
                    black_box(stats);
                });
            },
        );

        // Bulk loading benchmark
        group.bench_with_input(
            BenchmarkId::new("bulk_load", count),
            &points.clone(),
            |b, points| {
                let entries: Vec<(Vec<u8>, GeometryRef<'_>)> = points
                    .iter()
                    .map(|(id, point)| (id.clone(), GeometryRef::Point(*point)))
                    .collect();

                b.iter(|| {
                    let fs = MemoryFileSystem::new();
                    let pager = create_test_pager(&fs, "/bench_bulk.db");
                    let config = SpatialConfig::default().with_max_entries(64);
                    let rtree = PagedRTree::bulk_load(
                        TableId::from(1),
                        "bench_bulk".to_string(),
                        pager,
                        config,
                        entries.clone(),
                    )
                    .unwrap();

                    let stats = rtree.stats().unwrap();
                    black_box(stats);
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Bulk Loading Query Performance Benchmark
// ============================================================================

fn bench_bulk_query_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_query_performance");

    let count = 10_000;
    let points = generate_points(count, 42);

    // Create sequentially loaded tree
    let fs_seq = MemoryFileSystem::new();
    let mut rtree_seq = create_rtree(
        &fs_seq,
        "/bench_seq_query.db",
        SpatialConfig::default().with_max_entries(64),
    );
    for (id, point) in &points {
        rtree_seq
            .insert_geometry(
                id,
                GeometryRef::Point(*point),
                TransactionId::from(0),
                LogSequenceNumber::from(0),
            )
            .unwrap();
    }

    // Create bulk loaded tree
    let fs_bulk = MemoryFileSystem::new();
    let entries: Vec<(Vec<u8>, GeometryRef<'_>)> = points
        .iter()
        .map(|(id, point)| (id.clone(), GeometryRef::Point(*point)))
        .collect();
    let pager = create_test_pager(&fs_bulk, "/bench_bulk_query.db");
    let config = SpatialConfig::default().with_max_entries(64);
    let rtree_bulk = PagedRTree::bulk_load(
        TableId::from(1),
        "bench_bulk_query".to_string(),
        pager,
        config,
        entries,
    )
    .unwrap();

    let query = GeometryRef::BoundingBox {
        min: GeoPoint { x: 45.0, y: 45.0 },
        max: GeoPoint { x: 55.0, y: 55.0 },
    };

    group.bench_function("sequential_intersects", |b| {
        b.iter(|| {
            let results = rtree_seq.intersects(query.clone(), 1000).unwrap();
            black_box(results);
        });
    });

    group.bench_function("bulk_intersects", |b| {
        b.iter(|| {
            let results = rtree_bulk.intersects(query.clone(), 1000).unwrap();
            black_box(results);
        });
    });

    let query_point = GeoPoint { x: 50.0, y: 50.0 };

    group.bench_function("sequential_nearest", |b| {
        b.iter(|| {
            let results = rtree_seq.nearest(query_point, 10).unwrap();
            black_box(results);
        });
    });

    group.bench_function("bulk_nearest", |b| {
        b.iter(|| {
            let results = rtree_bulk.nearest(query_point, 10).unwrap();
            black_box(results);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_insertion_throughput,
    bench_insertion_by_strategy,
    bench_intersects_query,
    bench_nearest_query,
    bench_tree_height,
    bench_memory_usage,
    bench_node_capacity,
    bench_strategy_capacity_matrix,
    bench_bulk_loading,
    bench_bulk_query_performance,
);

criterion_main!(benches);
