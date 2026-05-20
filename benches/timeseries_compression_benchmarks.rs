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

//! Benchmarks for TimeSeries compression algorithms
//!
//! This benchmark suite measures:
//! - Compression ratios for different data patterns
//! - Compression/decompression performance
//! - Delta-of-delta compression on timestamps
//! - Gorilla compression on floating-point values
//! - Delta compression on integer values

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use nanostore::table::timeseries::{
    compress_timestamps_delta_of_delta, compress_values_delta, compress_values_gorilla,
    decompress_timestamps_delta_of_delta, decompress_values_delta, decompress_values_gorilla,
};
use std::f64::consts::PI;

// =============================================================================
// Data Generation Helpers
// =============================================================================

/// Generate regular interval timestamps (e.g., every 10 seconds)
fn generate_regular_timestamps(count: usize, start: i64, interval: i64) -> Vec<i64> {
    (0..count).map(|i| start + (i as i64 * interval)).collect()
}

/// Generate irregular interval timestamps with jitter
fn generate_irregular_timestamps(count: usize, start: i64, base_interval: i64) -> Vec<i64> {
    let mut timestamps = Vec::with_capacity(count);
    let mut current = start;
    for i in 0..count {
        timestamps.push(current);
        // Add jitter: ±20% of base interval
        let jitter = (i as i64 % 5 - 2) * (base_interval / 10);
        current += base_interval + jitter;
    }
    timestamps
}

/// Generate slowly changing floating-point values (good for Gorilla)
fn generate_slowly_changing_values(count: usize, base: f64, variance: f64) -> Vec<f64> {
    let mut values = Vec::with_capacity(count);
    let mut current = base;
    for i in 0..count {
        values.push(current);
        // Small changes: ±variance
        let change = ((i as f64 * 0.1).sin() * variance);
        current += change;
    }
    values
}

/// Generate high variance floating-point values (challenging for Gorilla)
fn generate_high_variance_values(count: usize, base: f64, variance: f64) -> Vec<f64> {
    (0..count)
        .map(|i| base + ((i as f64 * PI / 10.0).sin() * variance))
        .collect()
}

/// Generate monotonically increasing integer values (good for delta)
fn generate_monotonic_integers(count: usize, start: i64, step: i64) -> Vec<i64> {
    (0..count).map(|i| start + (i as i64 * step)).collect()
}

/// Generate random-walk integer values
fn generate_random_walk_integers(count: usize, start: i64) -> Vec<i64> {
    let mut values = Vec::with_capacity(count);
    let mut current = start;
    for i in 0..count {
        values.push(current);
        // Random walk: ±1 to ±10
        let change = ((i * 7) % 20) as i64 - 10;
        current += change;
    }
    values
}

// =============================================================================
// Compression Ratio Measurement
// =============================================================================

/// Calculate compression ratio (original size / compressed size)
fn compression_ratio(original_bytes: usize, compressed_bytes: usize) -> f64 {
    if compressed_bytes == 0 {
        return 0.0;
    }
    original_bytes as f64 / compressed_bytes as f64
}

// =============================================================================
// Delta-of-Delta Timestamp Compression Benchmarks
// =============================================================================

fn bench_delta_of_delta_compression(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_of_delta_compression");

    for size in [100, 1000, 10000] {
        // Regular intervals (best case)
        group.bench_with_input(
            BenchmarkId::new("regular_intervals", size),
            &size,
            |b, &size| {
                let timestamps = generate_regular_timestamps(size, 1_000_000, 10);
                b.iter(|| {
                    let compressed = compress_timestamps_delta_of_delta(black_box(&timestamps)).unwrap();
                    black_box(compressed);
                });
            },
        );

        // Irregular intervals (realistic case)
        group.bench_with_input(
            BenchmarkId::new("irregular_intervals", size),
            &size,
            |b, &size| {
                let timestamps = generate_irregular_timestamps(size, 1_000_000, 10);
                b.iter(|| {
                    let compressed = compress_timestamps_delta_of_delta(black_box(&timestamps)).unwrap();
                    black_box(compressed);
                });
            },
        );
    }

    group.finish();
}

fn bench_delta_of_delta_decompression(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_of_delta_decompression");

    for size in [100, 1000, 10000] {
        // Regular intervals
        group.bench_with_input(
            BenchmarkId::new("regular_intervals", size),
            &size,
            |b, &size| {
                let timestamps = generate_regular_timestamps(size, 1_000_000, 10);
                let compressed = compress_timestamps_delta_of_delta(&timestamps).unwrap();
                b.iter(|| {
                    let decompressed = decompress_timestamps_delta_of_delta(black_box(&compressed), size).unwrap();
                    black_box(decompressed);
                });
            },
        );

        // Irregular intervals
        group.bench_with_input(
            BenchmarkId::new("irregular_intervals", size),
            &size,
            |b, &size| {
                let timestamps = generate_irregular_timestamps(size, 1_000_000, 10);
                let compressed = compress_timestamps_delta_of_delta(&timestamps).unwrap();
                b.iter(|| {
                    let decompressed = decompress_timestamps_delta_of_delta(black_box(&compressed), size).unwrap();
                    black_box(decompressed);
                });
            },
        );
    }

    group.finish();
}

fn bench_delta_of_delta_ratios(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_of_delta_ratios");
    group.sample_size(10); // Fewer samples for ratio measurements

    for size in [100, 1000, 10000] {
        // Regular intervals
        let timestamps = generate_regular_timestamps(size, 1_000_000, 10);
        let compressed = compress_timestamps_delta_of_delta(&timestamps).unwrap();
        let original_size = timestamps.len() * 8; // 8 bytes per i64
        let ratio = compression_ratio(original_size, compressed.len());
        
        group.bench_function(
            BenchmarkId::new("regular_intervals_ratio", size),
            |b| b.iter(|| black_box(ratio)),
        );

        // Irregular intervals
        let timestamps = generate_irregular_timestamps(size, 1_000_000, 10);
        let compressed = compress_timestamps_delta_of_delta(&timestamps).unwrap();
        let ratio = compression_ratio(original_size, compressed.len());
        
        group.bench_function(
            BenchmarkId::new("irregular_intervals_ratio", size),
            |b| b.iter(|| black_box(ratio)),
        );
    }

    group.finish();
}

// =============================================================================
// Gorilla Floating-Point Compression Benchmarks
// =============================================================================

fn bench_gorilla_compression(c: &mut Criterion) {
    let mut group = c.benchmark_group("gorilla_compression");

    for size in [100, 1000, 10000] {
        // Slowly changing values (best case)
        group.bench_with_input(
            BenchmarkId::new("slowly_changing", size),
            &size,
            |b, &size| {
                let values = generate_slowly_changing_values(size, 100.0, 0.1);
                b.iter(|| {
                    let compressed = compress_values_gorilla(black_box(&values)).unwrap();
                    black_box(compressed);
                });
            },
        );

        // High variance values (challenging case)
        group.bench_with_input(
            BenchmarkId::new("high_variance", size),
            &size,
            |b, &size| {
                let values = generate_high_variance_values(size, 100.0, 50.0);
                b.iter(|| {
                    let compressed = compress_values_gorilla(black_box(&values)).unwrap();
                    black_box(compressed);
                });
            },
        );

        // Low variance values (very good case)
        group.bench_with_input(
            BenchmarkId::new("low_variance", size),
            &size,
            |b, &size| {
                let values = generate_slowly_changing_values(size, 100.0, 0.01);
                b.iter(|| {
                    let compressed = compress_values_gorilla(black_box(&values)).unwrap();
                    black_box(compressed);
                });
            },
        );
    }

    group.finish();
}

fn bench_gorilla_decompression(c: &mut Criterion) {
    let mut group = c.benchmark_group("gorilla_decompression");

    for size in [100, 1000, 10000] {
        // Slowly changing values
        group.bench_with_input(
            BenchmarkId::new("slowly_changing", size),
            &size,
            |b, &size| {
                let values = generate_slowly_changing_values(size, 100.0, 0.1);
                let compressed = compress_values_gorilla(&values).unwrap();
                b.iter(|| {
                    let decompressed = decompress_values_gorilla(black_box(&compressed), size).unwrap();
                    black_box(decompressed);
                });
            },
        );

        // High variance values
        group.bench_with_input(
            BenchmarkId::new("high_variance", size),
            &size,
            |b, &size| {
                let values = generate_high_variance_values(size, 100.0, 50.0);
                let compressed = compress_values_gorilla(&values).unwrap();
                b.iter(|| {
                    let decompressed = decompress_values_gorilla(black_box(&compressed), size).unwrap();
                    black_box(decompressed);
                });
            },
        );

        // Low variance values
        group.bench_with_input(
            BenchmarkId::new("low_variance", size),
            &size,
            |b, &size| {
                let values = generate_slowly_changing_values(size, 100.0, 0.01);
                let compressed = compress_values_gorilla(&values).unwrap();
                b.iter(|| {
                    let decompressed = decompress_values_gorilla(black_box(&compressed), size).unwrap();
                    black_box(decompressed);
                });
            },
        );
    }

    group.finish();
}

fn bench_gorilla_ratios(c: &mut Criterion) {
    let mut group = c.benchmark_group("gorilla_ratios");
    group.sample_size(10);

    for size in [100, 1000, 10000] {
        let original_size = size * 8; // 8 bytes per f64

        // Slowly changing values
        let values = generate_slowly_changing_values(size, 100.0, 0.1);
        let compressed = compress_values_gorilla(&values).unwrap();
        let ratio = compression_ratio(original_size, compressed.len());
        
        group.bench_function(
            BenchmarkId::new("slowly_changing_ratio", size),
            |b| b.iter(|| black_box(ratio)),
        );

        // High variance values
        let values = generate_high_variance_values(size, 100.0, 50.0);
        let compressed = compress_values_gorilla(&values).unwrap();
        let ratio = compression_ratio(original_size, compressed.len());
        
        group.bench_function(
            BenchmarkId::new("high_variance_ratio", size),
            |b| b.iter(|| black_box(ratio)),
        );

        // Low variance values
        let values = generate_slowly_changing_values(size, 100.0, 0.01);
        let compressed = compress_values_gorilla(&values).unwrap();
        let ratio = compression_ratio(original_size, compressed.len());
        
        group.bench_function(
            BenchmarkId::new("low_variance_ratio", size),
            |b| b.iter(|| black_box(ratio)),
        );
    }

    group.finish();
}

// =============================================================================
// Delta Integer Compression Benchmarks
// =============================================================================

fn bench_delta_compression(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_compression");

    for size in [100, 1000, 10000] {
        // Monotonic values (best case)
        group.bench_with_input(
            BenchmarkId::new("monotonic", size),
            &size,
            |b, &size| {
                let values = generate_monotonic_integers(size, 1000, 5);
                b.iter(|| {
                    let compressed = compress_values_delta(black_box(&values)).unwrap();
                    black_box(compressed);
                });
            },
        );

        // Random walk values (realistic case)
        group.bench_with_input(
            BenchmarkId::new("random_walk", size),
            &size,
            |b, &size| {
                let values = generate_random_walk_integers(size, 1000);
                b.iter(|| {
                    let compressed = compress_values_delta(black_box(&values)).unwrap();
                    black_box(compressed);
                });
            },
        );
    }

    group.finish();
}

fn bench_delta_decompression(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_decompression");

    for size in [100, 1000, 10000] {
        // Monotonic values
        group.bench_with_input(
            BenchmarkId::new("monotonic", size),
            &size,
            |b, &size| {
                let values = generate_monotonic_integers(size, 1000, 5);
                let compressed = compress_values_delta(&values).unwrap();
                b.iter(|| {
                    let decompressed = decompress_values_delta(black_box(&compressed), size).unwrap();
                    black_box(decompressed);
                });
            },
        );

        // Random walk values
        group.bench_with_input(
            BenchmarkId::new("random_walk", size),
            &size,
            |b, &size| {
                let values = generate_random_walk_integers(size, 1000);
                let compressed = compress_values_delta(&values).unwrap();
                b.iter(|| {
                    let decompressed = decompress_values_delta(black_box(&compressed), size).unwrap();
                    black_box(decompressed);
                });
            },
        );
    }

    group.finish();
}

fn bench_delta_ratios(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_ratios");
    group.sample_size(10);

    for size in [100, 1000, 10000] {
        let original_size = size * 8; // 8 bytes per i64

        // Monotonic values
        let values = generate_monotonic_integers(size, 1000, 5);
        let compressed = compress_values_delta(&values).unwrap();
        let ratio = compression_ratio(original_size, compressed.len());
        
        group.bench_function(
            BenchmarkId::new("monotonic_ratio", size),
            |b| b.iter(|| black_box(ratio)),
        );

        // Random walk values
        let values = generate_random_walk_integers(size, 1000);
        let compressed = compress_values_delta(&values).unwrap();
        let ratio = compression_ratio(original_size, compressed.len());
        
        group.bench_function(
            BenchmarkId::new("random_walk_ratio", size),
            |b| b.iter(|| black_box(ratio)),
        );
    }

    group.finish();
}

// =============================================================================
// Combined Benchmarks (Timestamp + Value)
// =============================================================================

fn bench_combined_compression(c: &mut Criterion) {
    let mut group = c.benchmark_group("combined_compression");
    group.throughput(Throughput::Elements(1000));

    let size = 1000;

    group.bench_function("regular_timestamps_slowly_changing_values", |b| {
        let timestamps = generate_regular_timestamps(size, 1_000_000, 10);
        let values = generate_slowly_changing_values(size, 100.0, 0.1);
        
        b.iter(|| {
            let ts_compressed = compress_timestamps_delta_of_delta(black_box(&timestamps)).unwrap();
            let val_compressed = compress_values_gorilla(black_box(&values)).unwrap();
            black_box((ts_compressed, val_compressed));
        });
    });

    group.bench_function("irregular_timestamps_high_variance_values", |b| {
        let timestamps = generate_irregular_timestamps(size, 1_000_000, 10);
        let values = generate_high_variance_values(size, 100.0, 50.0);
        
        b.iter(|| {
            let ts_compressed = compress_timestamps_delta_of_delta(black_box(&timestamps)).unwrap();
            let val_compressed = compress_values_gorilla(black_box(&values)).unwrap();
            black_box((ts_compressed, val_compressed));
        });
    });

    group.finish();
}

// =============================================================================
// Criterion Configuration
// =============================================================================

criterion_group!(
    delta_of_delta_benches,
    bench_delta_of_delta_compression,
    bench_delta_of_delta_decompression,
    bench_delta_of_delta_ratios
);

criterion_group!(
    gorilla_benches,
    bench_gorilla_compression,
    bench_gorilla_decompression,
    bench_gorilla_ratios
);

criterion_group!(
    delta_benches,
    bench_delta_compression,
    bench_delta_decompression,
    bench_delta_ratios
);

criterion_group!(
    combined_benches,
    bench_combined_compression
);

criterion_main!(
    delta_of_delta_benches,
    gorilla_benches,
    delta_benches,
    combined_benches
);

// Made with Bob