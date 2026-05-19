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

//! Benchmarks for TimeSeries table engine aggregation operations.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use nanostore::pager::{Pager, PagerConfig};
use nanostore::table::TimeSeries;
use nanostore::table::timeseries::{TimeSeriesAggregation, TimeSeriesConfig, TimeSeriesTable};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use std::sync::Arc;

/// Helper to create a TimeSeries table with test data
fn create_table_with_data(name: &str, num_points: usize) -> TimeSeriesTable<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "bench.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(1),
        name.to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    let series_key = b"sensor-bench";
    let tx_id = TransactionId::from(1);
    for i in 0..num_points {
        let timestamp = i as i64 * 1000; // 1 second intervals
        let value = format!("{}.{}", 20 + (i % 50), i % 10);
        table
            .append_point(series_key, timestamp, value.as_bytes(), tx_id)
            .unwrap();
    }

    table
}

/// Benchmark basic aggregation operations (sum, avg, min, max, count)
fn bench_aggregations(c: &mut Criterion) {
    let mut group = c.benchmark_group("timeseries_aggregations");

    for size in [100, 1_000, 10_000, 100_000].iter() {
        group.throughput(Throughput::Elements(*size as u64));

        let table = create_table_with_data("metrics", *size);
        let series_key = b"sensor-bench";
        let end_ts = (*size as i64) * 1000;

        // Benchmark sum
        group.bench_with_input(BenchmarkId::new("sum", size), size, |b, _| {
            b.iter(|| {
                let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                black_box(cursor.sum())
            });
        });

        // Benchmark avg
        group.bench_with_input(BenchmarkId::new("avg", size), size, |b, _| {
            b.iter(|| {
                let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                black_box(cursor.avg())
            });
        });

        // Benchmark min
        group.bench_with_input(BenchmarkId::new("min", size), size, |b, _| {
            b.iter(|| {
                let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                black_box(cursor.min())
            });
        });

        // Benchmark max
        group.bench_with_input(BenchmarkId::new("max", size), size, |b, _| {
            b.iter(|| {
                let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                black_box(cursor.max())
            });
        });

        // Benchmark count
        group.bench_with_input(BenchmarkId::new("count", size), size, |b, _| {
            b.iter(|| {
                let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                black_box(cursor.count())
            });
        });

        // Benchmark generic aggregate
        group.bench_with_input(BenchmarkId::new("aggregate_sum", size), size, |b, _| {
            b.iter(|| {
                let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                black_box(cursor.aggregate(TimeSeriesAggregation::Sum))
            });
        });
    }

    group.finish();
}

/// Benchmark downsampling with different window sizes
fn bench_downsampling(c: &mut Criterion) {
    let mut group = c.benchmark_group("timeseries_downsampling");

    let size = 100_000;
    let table = create_table_with_data("metrics", size);
    let series_key = b"sensor-bench";
    let end_ts = (size as i64) * 1000;

    // Test different downsampling intervals
    for interval in [60_000, 300_000, 3_600_000].iter() {
        // 1 min, 5 min, 1 hour
        let interval_name = match interval {
            60_000 => "1min",
            300_000 => "5min",
            3_600_000 => "1hour",
            _ => "unknown",
        };

        group.throughput(Throughput::Elements(size as u64));

        // Benchmark downsampling with avg
        group.bench_with_input(
            BenchmarkId::new(format!("avg_{}", interval_name), size),
            interval,
            |b, &interval| {
                b.iter(|| {
                    let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                    black_box(cursor.downsample(interval, TimeSeriesAggregation::Avg))
                });
            },
        );

        // Benchmark downsampling with sum
        group.bench_with_input(
            BenchmarkId::new(format!("sum_{}", interval_name), size),
            interval,
            |b, &interval| {
                b.iter(|| {
                    let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                    black_box(cursor.downsample(interval, TimeSeriesAggregation::Sum))
                });
            },
        );

        // Benchmark downsampling with count
        group.bench_with_input(
            BenchmarkId::new(format!("count_{}", interval_name), size),
            interval,
            |b, &interval| {
                b.iter(|| {
                    let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                    black_box(cursor.downsample(interval, TimeSeriesAggregation::Count))
                });
            },
        );
    }

    group.finish();
}

/// Benchmark aggregation with different data sparsity
fn bench_sparse_aggregation(c: &mut Criterion) {
    let mut group = c.benchmark_group("timeseries_sparse_aggregation");

    let size = 10_000;

    // Create tables with different sparsity (percentage of non-numeric values)
    for sparsity in [0, 25, 50, 75].iter() {
        let fs = MemoryFileSystem::new();
        let pager = Arc::new(Pager::create(&fs, "bench.db", PagerConfig::default()).unwrap());
        let mut table = TimeSeriesTable::new(
            TableId::from(1),
            "metrics".to_string(),
            pager,
            TimeSeriesConfig::default(),
        )
        .unwrap();

        let series_key = b"sensor-sparse";
        for i in 0..size {
            let timestamp = i as i64 * 1000;
            let value = if (i * 100 / size) < *sparsity {
                "non-numeric-value".to_string()
            } else {
                format!("{}.{}", 20 + (i % 50), i % 10)
            };
            let tx_id = TransactionId::from(1);
            table
                .append_point(series_key, timestamp, value.as_bytes(), tx_id)
                .unwrap();
        }

        let end_ts = (size as i64) * 1000;

        group.throughput(Throughput::Elements(size as u64));

        group.bench_with_input(
            BenchmarkId::new("sum", format!("{}%_sparse", sparsity)),
            sparsity,
            |b, _| {
                b.iter(|| {
                    let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                    black_box(cursor.sum())
                });
            },
        );
    }

    group.finish();
}

/// Benchmark memory efficiency of aggregation (no loading all data)
fn bench_streaming_aggregation(c: &mut Criterion) {
    let mut group = c.benchmark_group("timeseries_streaming");

    // Test with very large dataset to verify streaming behavior
    for size in [10_000, 50_000, 100_000].iter() {
        let table = create_table_with_data("metrics", *size);
        let series_key = b"sensor-bench";
        let end_ts = (*size as i64) * 1000;

        group.throughput(Throughput::Elements(*size as u64));

        // Measure time to compute aggregation
        group.bench_with_input(BenchmarkId::new("streaming_avg", size), size, |b, _| {
            b.iter(|| {
                let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                black_box(cursor.avg())
            });
        });
    }

    group.finish();
}

/// Benchmark multiple aggregations on the same cursor
fn bench_multiple_aggregations(c: &mut Criterion) {
    let mut group = c.benchmark_group("timeseries_multiple_aggregations");

    let size = 10_000;
    let table = create_table_with_data("metrics", size);
    let series_key = b"sensor-bench";
    let end_ts = (size as i64) * 1000;

    group.throughput(Throughput::Elements(size as u64));

    // Benchmark computing all aggregations at once
    group.bench_function("all_aggregations", |b| {
        b.iter(|| {
            let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
            let sum = cursor.sum();
            let avg = cursor.avg();
            let min = cursor.min();
            let max = cursor.max();
            let count = cursor.count();
            black_box((sum, avg, min, max, count))
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_aggregations,
    bench_downsampling,
    bench_sparse_aggregation,
    bench_streaming_aggregation,
    bench_multiple_aggregations
);
criterion_main!(benches);

// Made with Bob
