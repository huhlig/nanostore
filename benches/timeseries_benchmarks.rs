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

//! Benchmarks for TimeSeries table engine query and aggregation operations.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use nanostore::pager::{Pager, PagerConfig};
use nanostore::table::timeseries::{TimeSeriesAggregation, TimeSeriesConfig, TimeSeriesTable};
use nanostore::table::{TimeSeries, TimeSeriesCursor};
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

/// Benchmark scan_series performance across different time ranges
fn bench_scan_series_ranges(c: &mut Criterion) {
    let mut group = c.benchmark_group("timeseries_scan_ranges");

    let total_points = 100_000;
    let table = create_table_with_data("metrics", total_points);
    let series_key = b"sensor-bench";

    // Test different time range sizes
    let ranges = [
        ("small_1pct", total_points / 100),
        ("medium_10pct", total_points / 10),
        ("large_50pct", total_points / 2),
        ("full_100pct", total_points),
    ];

    for (name, range_size) in ranges.iter() {
        let end_ts = (*range_size as i64) * 1000;
        group.throughput(Throughput::Elements(*range_size as u64));

        // Benchmark creating cursor and iterating through all points
        group.bench_with_input(BenchmarkId::new("scan", name), range_size, |b, _| {
            b.iter(|| {
                let mut cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                let mut count = 0;
                while cursor.valid() {
                    black_box(cursor.current());
                    cursor.next().unwrap();
                    count += 1;
                }
                black_box(count)
            });
        });

        // Benchmark just creating the cursor (setup cost)
        group.bench_with_input(
            BenchmarkId::new("cursor_create", name),
            range_size,
            |b, _| {
                b.iter(|| black_box(table.scan_series(series_key, 0, end_ts).unwrap()));
            },
        );
    }

    group.finish();
}

/// Benchmark latest_before query performance
fn bench_latest_before(c: &mut Criterion) {
    let mut group = c.benchmark_group("timeseries_latest_before");

    let total_points = 100_000;
    let table = create_table_with_data("metrics", total_points);
    let series_key = b"sensor-bench";

    // Test queries at different positions in the timeline
    let positions = [
        ("early_10pct", (total_points / 10) as i64 * 1000),
        ("middle_50pct", (total_points / 2) as i64 * 1000),
        ("late_90pct", (total_points * 9 / 10) as i64 * 1000),
        ("end_100pct", (total_points as i64) * 1000),
    ];

    for (name, timestamp) in positions.iter() {
        group.bench_with_input(BenchmarkId::new("query", name), timestamp, |b, &ts| {
            b.iter(|| black_box(table.latest_before(series_key, ts).unwrap()));
        });
    }

    // Benchmark query for non-existent series
    group.bench_function("nonexistent_series", |b| {
        b.iter(|| black_box(table.latest_before(b"nonexistent", 50000).unwrap()));
    });

    group.finish();
}

/// Benchmark performance with varying bucket sizes
fn bench_bucket_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("timeseries_bucket_sizes");

    let num_points = 50_000;
    let series_key = b"sensor-bench";

    // Test different bucket sizes (in seconds)
    let bucket_sizes = [
        ("1min", 60),
        ("5min", 300),
        ("1hour", 3600),
        ("1day", 86400),
    ];

    for (name, bucket_size) in bucket_sizes.iter() {
        let fs = MemoryFileSystem::new();
        let pager = Arc::new(Pager::create(&fs, "bench.db", PagerConfig::default()).unwrap());
        let config = TimeSeriesConfig::default().with_bucket_size(*bucket_size);
        let mut table =
            TimeSeriesTable::new(TableId::from(1), "metrics".to_string(), pager, config).unwrap();

        // Insert data
        let tx_id = TransactionId::from(1);
        for i in 0..num_points {
            let timestamp = i as i64 * 1000;
            let value = format!("{}.{}", 20 + (i % 50), i % 10);
            table
                .append_point(series_key, timestamp, value.as_bytes(), tx_id)
                .unwrap();
        }

        let end_ts = (num_points as i64) * 1000;
        group.throughput(Throughput::Elements(num_points as u64));

        // Benchmark full scan
        group.bench_with_input(BenchmarkId::new("full_scan", name), bucket_size, |b, _| {
            b.iter(|| {
                let mut cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                let mut count = 0;
                while cursor.valid() {
                    black_box(cursor.current());
                    cursor.next().unwrap();
                    count += 1;
                }
                black_box(count)
            });
        });

        // Benchmark latest_before
        group.bench_with_input(
            BenchmarkId::new("latest_before", name),
            bucket_size,
            |b, _| {
                b.iter(|| black_box(table.latest_before(series_key, end_ts / 2).unwrap()));
            },
        );
    }

    group.finish();
}

/// Benchmark memory usage during queries (measure allocation patterns)
fn bench_memory_usage(c: &mut Criterion) {
    let mut group = c.benchmark_group("timeseries_memory_usage");

    // Test with different dataset sizes to observe memory scaling
    for size in [1_000, 10_000, 50_000].iter() {
        let table = create_table_with_data("metrics", *size);
        let series_key = b"sensor-bench";
        let end_ts = (*size as i64) * 1000;

        group.throughput(Throughput::Elements(*size as u64));

        // Benchmark cursor creation (measures initial allocation)
        group.bench_with_input(BenchmarkId::new("cursor_alloc", size), size, |b, _| {
            b.iter(|| black_box(table.scan_series(series_key, 0, end_ts).unwrap()));
        });

        // Benchmark full iteration (measures total memory footprint)
        group.bench_with_input(BenchmarkId::new("full_iteration", size), size, |b, _| {
            b.iter(|| {
                let mut cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                let mut points = Vec::new();
                while cursor.valid() {
                    if let Some(point) = cursor.current() {
                        points.push((point.timestamp, point.value_key.0.clone()));
                    }
                    cursor.next().unwrap();
                }
                black_box(points)
            });
        });

        // Benchmark aggregation (streaming, minimal memory)
        group.bench_with_input(BenchmarkId::new("streaming_agg", size), size, |b, _| {
            b.iter(|| {
                let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
                black_box(cursor.avg())
            });
        });
    }

    group.finish();
}

/// Benchmark query performance with multiple concurrent series
fn bench_multiple_series(c: &mut Criterion) {
    let mut group = c.benchmark_group("timeseries_multiple_series");

    let num_series = 10;
    let points_per_series = 1_000;

    let fs = MemoryFileSystem::new();
    let pager = Arc::new(Pager::create(&fs, "bench.db", PagerConfig::default()).unwrap());
    let mut table = TimeSeriesTable::new(
        TableId::from(1),
        "metrics".to_string(),
        pager,
        TimeSeriesConfig::default(),
    )
    .unwrap();

    // Insert data for multiple series
    let tx_id = TransactionId::from(1);
    for series_idx in 0..num_series {
        let series_key = format!("sensor-{}", series_idx);
        for i in 0..points_per_series {
            let timestamp = i as i64 * 1000;
            let value = format!("{}.{}", 20 + (i % 50), i % 10);
            table
                .append_point(series_key.as_bytes(), timestamp, value.as_bytes(), tx_id)
                .unwrap();
        }
    }

    let end_ts = (points_per_series as i64) * 1000;
    group.throughput(Throughput::Elements(
        (num_series * points_per_series) as u64,
    ));

    // Benchmark scanning a single series
    group.bench_function("single_series_scan", |b| {
        let series_key = b"sensor-5";
        b.iter(|| {
            let mut cursor = table.scan_series(series_key, 0, end_ts).unwrap();
            let mut count = 0;
            while cursor.valid() {
                black_box(cursor.current());
                cursor.next().unwrap();
                count += 1;
            }
            black_box(count)
        });
    });

    // Benchmark latest_before on a single series
    group.bench_function("single_series_latest", |b| {
        let series_key = b"sensor-5";
        b.iter(|| black_box(table.latest_before(series_key, end_ts / 2).unwrap()));
    });

    // Benchmark aggregation on a single series
    group.bench_function("single_series_aggregation", |b| {
        let series_key = b"sensor-5";
        b.iter(|| {
            let cursor = table.scan_series(series_key, 0, end_ts).unwrap();
            black_box(cursor.avg())
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
    bench_multiple_aggregations,
    bench_scan_series_ranges,
    bench_latest_before,
    bench_bucket_sizes,
    bench_memory_usage,
    bench_multiple_series
);
criterion_main!(benches);

// Made with Bob
