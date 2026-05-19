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

//! Performance benchmarks for streaming operations.
//!
//! Benchmarks cover:
//! - Overflow chain allocation and reading
//! - ValueRef encoding/decoding
//! - BTree streaming operations
//! - LSM streaming operations
//! - Memory usage and throughput

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use nanostore::pager::{OverflowChainStream, Pager, PagerConfig};
use nanostore::table::btree::PagedBTree;
use nanostore::table::{Flushable, MutableTable, PointLookup, SearchableTable, Table, ValueStream};
use nanostore::txn::TransactionId;
use nanostore::types::{TableId, ValueRef};
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
use std::sync::Arc;

/// Helper struct for streaming
struct VecValueStream {
    data: Vec<u8>,
    position: usize,
}

impl VecValueStream {
    fn new(data: Vec<u8>) -> Self {
        Self { data, position: 0 }
    }
}

impl ValueStream for VecValueStream {
    fn read(&mut self, buf: &mut [u8]) -> nanostore::table::TableResult<usize> {
        let remaining = self.data.len() - self.position;
        let to_read = remaining.min(buf.len());

        if to_read == 0 {
            return Ok(0);
        }

        buf[..to_read].copy_from_slice(&self.data[self.position..self.position + to_read]);
        self.position += to_read;
        Ok(to_read)
    }

    fn size_hint(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }
}

fn bench_overflow_chain_allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("overflow_chain_allocation");

    for size in [1024, 10 * 1024, 100 * 1024, 1024 * 1024].iter() {
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter(|| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default();
                let pager = Pager::create(&fs, "bench.db", config).unwrap();

                let data = vec![0xAB; size];
                let page_ids = pager.allocate_overflow_chain(&data).unwrap();
                black_box(page_ids);
            });
        });
    }

    group.finish();
}

fn bench_overflow_chain_reading(c: &mut Criterion) {
    let mut group = c.benchmark_group("overflow_chain_reading");

    for size in [1024, 10 * 1024, 100 * 1024, 1024 * 1024].iter() {
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            // Setup
            let fs = MemoryFileSystem::new();
            let config = PagerConfig::default();
            let pager = Pager::create(&fs, "bench.db", config).unwrap();
            let data = vec![0xCD; size];
            let page_ids = pager.allocate_overflow_chain(&data).unwrap();

            b.iter(|| {
                let result = pager.read_overflow_chain(page_ids[0]).unwrap();
                black_box(result);
            });
        });
    }

    group.finish();
}

fn bench_overflow_chain_streaming(c: &mut Criterion) {
    let mut group = c.benchmark_group("overflow_chain_streaming");

    for size in [1024, 10 * 1024, 100 * 1024, 1024 * 1024].iter() {
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            // Setup
            let fs = MemoryFileSystem::new();
            let config = PagerConfig::default();
            let pager = Pager::create(&fs, "bench.db", config).unwrap();
            let data = vec![0xEF; size];
            let page_ids = pager.allocate_overflow_chain(&data).unwrap();

            b.iter(|| {
                let mut stream = OverflowChainStream::new(&pager, page_ids[0], size as u64);
                let mut result = Vec::new();
                let mut buffer = vec![0u8; 8192];

                loop {
                    let n = stream.read(&mut buffer).unwrap();
                    if n == 0 {
                        break;
                    }
                    result.extend_from_slice(&buffer[..n]);
                }

                black_box(result);
            });
        });
    }

    group.finish();
}

fn bench_valueref_encoding(c: &mut Criterion) {
    let mut group = c.benchmark_group("valueref_encoding");

    group.bench_function("inline", |b| {
        let vref = ValueRef::Inline;
        b.iter(|| {
            let encoded = vref.encode();
            black_box(encoded);
        });
    });

    group.bench_function("single_page", |b| {
        let vref = ValueRef::SinglePage {
            page_id: 12345,
            offset: 256,
            length: 4096,
        };
        b.iter(|| {
            let encoded = vref.encode();
            black_box(encoded);
        });
    });

    group.bench_function("overflow_chain", |b| {
        let vref = ValueRef::OverflowChain {
            first_page_id: 999,
            total_length: 1_000_000,
            page_count: 250,
        };
        b.iter(|| {
            let encoded = vref.encode();
            black_box(encoded);
        });
    });

    group.finish();
}

fn bench_valueref_decoding(c: &mut Criterion) {
    let mut group = c.benchmark_group("valueref_decoding");

    group.bench_function("inline", |b| {
        let vref = ValueRef::Inline;
        let encoded = vref.encode();
        b.iter(|| {
            let decoded = ValueRef::decode(&encoded).unwrap();
            black_box(decoded);
        });
    });

    group.bench_function("single_page", |b| {
        let vref = ValueRef::SinglePage {
            page_id: 12345,
            offset: 256,
            length: 4096,
        };
        let encoded = vref.encode();
        b.iter(|| {
            let decoded = ValueRef::decode(&encoded).unwrap();
            black_box(decoded);
        });
    });

    group.bench_function("overflow_chain", |b| {
        let vref = ValueRef::OverflowChain {
            first_page_id: 999,
            total_length: 1_000_000,
            page_count: 250,
        };
        let encoded = vref.encode();
        b.iter(|| {
            let decoded = ValueRef::decode(&encoded).unwrap();
            black_box(decoded);
        });
    });

    group.finish();
}

fn bench_btree_put_stream(c: &mut Criterion) {
    let mut group = c.benchmark_group("btree_put_stream");

    for size in [1024, 10 * 1024, 100 * 1024].iter() {
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter(|| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::default();
                let pager = Arc::new(Pager::create(&fs, "bench.db", config).unwrap());
                let table =
                    PagedBTree::new(TableId::from(1), "bench_table".to_string(), pager).unwrap();

                let data = vec![0x42; size];
                let mut stream = VecValueStream::new(data);

                let mut writer = table
                    .writer(TransactionId::from(1), LogSequenceNumber::from(0))
                    .unwrap();
                writer.put_stream(b"bench_key", &mut stream).unwrap();
                writer.flush().unwrap();
            });
        });
    }

    group.finish();
}

fn bench_btree_get(c: &mut Criterion) {
    let mut group = c.benchmark_group("btree_get");

    for size in [1024, 10 * 1024, 100 * 1024].iter() {
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            // Setup
            let fs = MemoryFileSystem::new();
            let config = PagerConfig::default();
            let pager = Arc::new(Pager::create(&fs, "bench.db", config).unwrap());
            let table =
                PagedBTree::new(TableId::from(1), "bench_table".to_string(), pager).unwrap();

            let data = vec![0x42; size];
            let mut stream = VecValueStream::new(data);

            let mut writer = table
                .writer(TransactionId::from(1), LogSequenceNumber::from(0))
                .unwrap();
            writer.put_stream(b"bench_key", &mut stream).unwrap();
            writer.flush().unwrap();

            b.iter(|| {
                let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
                let value = reader
                    .get(b"bench_key", LogSequenceNumber::from(100))
                    .unwrap();
                black_box(value);
            });
        });
    }

    group.finish();
}

fn bench_btree_get_stream(c: &mut Criterion) {
    let mut group = c.benchmark_group("btree_get_stream");

    for size in [1024, 10 * 1024, 100 * 1024].iter() {
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            // Setup
            let fs = MemoryFileSystem::new();
            let config = PagerConfig::default();
            let pager = Arc::new(Pager::create(&fs, "bench.db", config).unwrap());
            let table =
                PagedBTree::new(TableId::from(1), "bench_table".to_string(), pager).unwrap();

            let data = vec![0x42; size];
            let mut stream = VecValueStream::new(data);

            let mut writer = table
                .writer(TransactionId::from(1), LogSequenceNumber::from(0))
                .unwrap();
            writer.put_stream(b"bench_key", &mut stream).unwrap();
            writer.flush().unwrap();

            b.iter(|| {
                let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
                let mut stream_opt = reader
                    .get_stream(b"bench_key", LogSequenceNumber::from(100))
                    .unwrap();

                if let Some(mut stream) = stream_opt {
                    let mut result = Vec::new();
                    let mut buffer = vec![0u8; 4096];

                    loop {
                        let n = stream.read(&mut buffer).unwrap();
                        if n == 0 {
                            break;
                        }
                        result.extend_from_slice(&buffer[..n]);
                    }

                    black_box(result);
                }
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_overflow_chain_allocation,
    bench_overflow_chain_reading,
    bench_overflow_chain_streaming,
    bench_valueref_encoding,
    bench_valueref_decoding,
    bench_btree_put_stream,
    bench_btree_get,
    bench_btree_get_stream,
);

criterion_main!(benches);

// Made with Bob
