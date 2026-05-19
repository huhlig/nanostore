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

//! Cache Performance Benchmarks

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use nanostore::pager::{Page, PageId, PageSize, PageType, Pager, PagerConfig};
use nanostore::vfs::MemoryFileSystem;

fn bench_cache_hit_rate(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_hit_rate");

    for cache_size in [10, 100, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::new("with_cache", cache_size),
            cache_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::new()
                    .with_cache_capacity(size)
                    .with_cache_write_back(true);
                let pager = Pager::create(&fs, "bench.db", config).unwrap();

                // Allocate pages
                let page_ids: Vec<PageId> = (0..50)
                    .map(|_| pager.allocate_page(PageType::BTreeLeaf).unwrap())
                    .collect();

                // Write pages
                for (i, &page_id) in page_ids.iter().enumerate() {
                    let mut page =
                        Page::new(page_id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
                    page.data_mut()
                        .extend_from_slice(format!("page {}", i).as_bytes());
                    pager.write_page(&page).unwrap();
                }

                b.iter(|| {
                    // Read pages in a pattern that benefits from caching
                    for &page_id in page_ids.iter().take(20) {
                        black_box(pager.read_page(page_id).unwrap());
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("without_cache", cache_size),
            cache_size,
            |b, _| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::new().with_cache_capacity(0); // Disable cache
                let pager = Pager::create(&fs, "bench.db", config).unwrap();

                // Allocate pages
                let page_ids: Vec<PageId> = (0..50)
                    .map(|_| pager.allocate_page(PageType::BTreeLeaf).unwrap())
                    .collect();

                // Write pages
                for (i, &page_id) in page_ids.iter().enumerate() {
                    let mut page =
                        Page::new(page_id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
                    page.data_mut()
                        .extend_from_slice(format!("page {}", i).as_bytes());
                    pager.write_page(&page).unwrap();
                }

                b.iter(|| {
                    // Read pages - no caching benefit
                    for &page_id in page_ids.iter().take(20) {
                        black_box(pager.read_page(page_id).unwrap());
                    }
                });
            },
        );
    }

    group.finish();
}

fn bench_write_modes(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_modes");

    group.bench_function("write_back", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::new()
            .with_cache_capacity(100)
            .with_cache_write_back(true);
        let pager = Pager::create(&fs, "bench.db", config).unwrap();

        let page_ids: Vec<PageId> = (0..50)
            .map(|_| pager.allocate_page(PageType::BTreeLeaf).unwrap())
            .collect();

        b.iter(|| {
            for (i, &page_id) in page_ids.iter().enumerate() {
                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
                page.data_mut()
                    .extend_from_slice(format!("data {}", i).as_bytes());
                black_box(pager.write_page(&page).unwrap());
            }
        });
    });

    group.bench_function("write_through", |b| {
        let fs = MemoryFileSystem::new();
        let config = PagerConfig::new()
            .with_cache_capacity(100)
            .with_cache_write_back(false);
        let pager = Pager::create(&fs, "bench.db", config).unwrap();

        let page_ids: Vec<PageId> = (0..50)
            .map(|_| pager.allocate_page(PageType::BTreeLeaf).unwrap())
            .collect();

        b.iter(|| {
            for (i, &page_id) in page_ids.iter().enumerate() {
                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
                page.data_mut()
                    .extend_from_slice(format!("data {}", i).as_bytes());
                black_box(pager.write_page(&page).unwrap());
            }
        });
    });

    group.finish();
}

fn bench_cache_eviction(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_eviction");

    for cache_size in [10, 50, 100].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(cache_size),
            cache_size,
            |b, &size| {
                let fs = MemoryFileSystem::new();
                let config = PagerConfig::new()
                    .with_cache_capacity(size)
                    .with_cache_write_back(true);
                let pager = Pager::create(&fs, "bench.db", config).unwrap();

                // Allocate more pages than cache can hold
                let page_ids: Vec<PageId> = (0..(size * 2))
                    .map(|_| pager.allocate_page(PageType::BTreeLeaf).unwrap())
                    .collect();

                b.iter(|| {
                    // Access pattern that causes evictions
                    for (i, &page_id) in page_ids.iter().enumerate() {
                        let mut page =
                            Page::new(page_id, PageType::BTreeLeaf, PageSize::Size4KB.data_size());
                        page.data_mut()
                            .extend_from_slice(format!("data {}", i).as_bytes());
                        black_box(pager.write_page(&page).unwrap());
                        black_box(pager.read_page(page_id).unwrap());
                    }
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_cache_hit_rate,
    bench_write_modes,
    bench_cache_eviction
);
criterion_main!(benches);
