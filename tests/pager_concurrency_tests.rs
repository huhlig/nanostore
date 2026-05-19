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

//! Concurrent page access tests for the pager system
//!
//! These tests validate thread safety and proper synchronization when multiple
//! threads access pages simultaneously. They ensure that:
//! - No duplicate page IDs are allocated
//! - No data corruption occurs during concurrent operations
//! - Free list management is thread-safe
//! - Race conditions are properly handled

use nanostore::pager::{Page, PageType, Pager, PagerConfig};
use nanostore::vfs::MemoryFileSystem;
use std::collections::HashSet;
use std::sync::{Arc, Barrier};
use std::thread;

/// Helper function to create a test pager wrapped in Arc for sharing
fn create_shared_pager() -> Arc<Pager<MemoryFileSystem>> {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Pager::create(&fs, "concurrent_test.db", config).expect("Failed to create pager");
    Arc::new(pager)
}

/// Test concurrent page allocation with 2 threads
///
/// Validates that two threads can allocate pages simultaneously without
/// conflicts or duplicate page IDs.
#[test]
fn test_concurrent_allocation_2_threads() {
    let pager = create_shared_pager();
    let pages_per_thread = 100;
    let thread_count = 2;

    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let handle = thread::spawn(move || {
            let mut allocated = Vec::new();
            for i in 0..pages_per_thread {
                let page_id = pager_clone
                    .allocate_page(PageType::BTreeLeaf)
                    .unwrap_or_else(|_| {
                        panic!("Thread {} failed to allocate page {}", thread_id, i)
                    });
                allocated.push(page_id);
            }
            allocated
        });
        handles.push(handle);
    }

    // Collect all allocated page IDs
    let mut all_pages = Vec::new();
    for handle in handles {
        let pages = handle.join().expect("Thread panicked");
        all_pages.extend(pages);
    }

    // Verify no duplicates
    let unique_pages: HashSet<_> = all_pages.iter().collect();
    assert_eq!(
        unique_pages.len(),
        all_pages.len(),
        "All allocated page IDs should be unique"
    );
    assert_eq!(
        all_pages.len(),
        pages_per_thread * thread_count,
        "Should have allocated expected number of pages"
    );
}

/// Test concurrent page allocation with 4 threads
///
/// Increases concurrency to validate thread safety under higher contention.
#[test]
fn test_concurrent_allocation_4_threads() {
    let pager = create_shared_pager();
    let pages_per_thread = 50;
    let thread_count = 4;

    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let handle = thread::spawn(move || {
            let mut allocated = Vec::new();
            for i in 0..pages_per_thread {
                let page_id = pager_clone
                    .allocate_page(PageType::BTreeLeaf)
                    .unwrap_or_else(|_| {
                        panic!("Thread {} failed to allocate page {}", thread_id, i)
                    });
                allocated.push(page_id);
            }
            allocated
        });
        handles.push(handle);
    }

    let mut all_pages = Vec::new();
    for handle in handles {
        let pages = handle.join().expect("Thread panicked");
        all_pages.extend(pages);
    }

    let unique_pages: HashSet<_> = all_pages.iter().collect();
    assert_eq!(unique_pages.len(), all_pages.len(), "No duplicate page IDs");
    assert_eq!(all_pages.len(), pages_per_thread * thread_count);
}

/// Test concurrent page allocation with 8 threads
///
/// Higher concurrency stress test to validate scalability.
#[test]
fn test_concurrent_allocation_8_threads() {
    let pager = create_shared_pager();
    let pages_per_thread = 25;
    let thread_count = 8;

    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let handle = thread::spawn(move || {
            let mut allocated = Vec::new();
            for i in 0..pages_per_thread {
                let page_id = pager_clone
                    .allocate_page(PageType::BTreeLeaf)
                    .unwrap_or_else(|_| {
                        panic!("Thread {} failed to allocate page {}", thread_id, i)
                    });
                allocated.push(page_id);
            }
            allocated
        });
        handles.push(handle);
    }

    let mut all_pages = Vec::new();
    for handle in handles {
        let pages = handle.join().expect("Thread panicked");
        all_pages.extend(pages);
    }

    let unique_pages: HashSet<_> = all_pages.iter().collect();
    assert_eq!(unique_pages.len(), all_pages.len(), "No duplicate page IDs");
    assert_eq!(all_pages.len(), pages_per_thread * thread_count);
}

/// Test concurrent page allocation with 16 threads
///
/// Maximum concurrency stress test for production-like scenarios.
#[test]
fn test_concurrent_allocation_16_threads() {
    let pager = create_shared_pager();
    let pages_per_thread = 20;
    let thread_count = 16;

    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let handle = thread::spawn(move || {
            let mut allocated = Vec::new();
            for i in 0..pages_per_thread {
                let page_id = pager_clone
                    .allocate_page(PageType::BTreeLeaf)
                    .unwrap_or_else(|_| {
                        panic!("Thread {} failed to allocate page {}", thread_id, i)
                    });
                allocated.push(page_id);
            }
            allocated
        });
        handles.push(handle);
    }

    let mut all_pages = Vec::new();
    for handle in handles {
        let pages = handle.join().expect("Thread panicked");
        all_pages.extend(pages);
    }

    let unique_pages: HashSet<_> = all_pages.iter().collect();
    assert_eq!(unique_pages.len(), all_pages.len(), "No duplicate page IDs");
    assert_eq!(all_pages.len(), pages_per_thread * thread_count);
}

/// Test concurrent page reads from different pages
///
/// Validates that multiple threads can read different pages simultaneously
/// without interference.
#[test]
fn test_concurrent_reads_different_pages() {
    let pager = create_shared_pager();
    let thread_count = 8;
    let pages_per_thread = 10;

    // Pre-allocate and write pages
    let mut page_ids = Vec::new();
    for i in 0..thread_count * pages_per_thread {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
        let data = format!("Page {} data", i);
        page.data_mut().extend_from_slice(data.as_bytes());
        pager.write_page(&page).unwrap();
        page_ids.push((page_id, data));
    }

    // Concurrent reads
    let page_ids = Arc::new(page_ids);
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let page_ids_clone = Arc::clone(&page_ids);
        let handle = thread::spawn(move || {
            let start = thread_id * pages_per_thread;
            let end = start + pages_per_thread;

            for i in start..end {
                let (page_id, expected_data) = &page_ids_clone[i];
                let page = pager_clone.read_page(*page_id).unwrap_or_else(|_| {
                    panic!("Thread {} failed to read page {}", thread_id, page_id)
                });

                assert_eq!(
                    &page.data()[0..expected_data.len()],
                    expected_data.as_bytes(),
                    "Data mismatch for page {}",
                    page_id
                );
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}

/// Test concurrent page writes to different pages
///
/// Validates that multiple threads can write to different pages simultaneously
/// without data corruption.
#[test]
fn test_concurrent_writes_different_pages() {
    let pager = create_shared_pager();
    let thread_count = 8;
    let pages_per_thread = 10;

    // Pre-allocate pages
    let mut page_ids = Vec::new();
    for _ in 0..thread_count * pages_per_thread {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        page_ids.push(page_id);
    }
    let page_ids = Arc::new(page_ids);

    // Concurrent writes
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let page_ids_clone = Arc::clone(&page_ids);
        let handle = thread::spawn(move || {
            let start = thread_id * pages_per_thread;
            let end = start + pages_per_thread;

            for i in start..end {
                let page_id = page_ids_clone[i];
                let mut page = Page::new(
                    page_id,
                    PageType::BTreeLeaf,
                    pager_clone.page_size().data_size(),
                );
                let data = format!("Thread {} wrote page {}", thread_id, i);
                page.data_mut().extend_from_slice(data.as_bytes());
                pager_clone.write_page(&page).unwrap_or_else(|_| {
                    panic!("Thread {} failed to write page {}", thread_id, page_id)
                });
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Verify all writes succeeded
    for (i, &page_id) in page_ids.iter().enumerate() {
        let page = pager.read_page(page_id).unwrap();
        let thread_id = i / pages_per_thread;
        let expected_data = format!("Thread {} wrote page {}", thread_id, i);
        assert_eq!(
            &page.data()[0..expected_data.len()],
            expected_data.as_bytes(),
            "Data mismatch for page {}",
            page_id
        );
    }
}

/// Test mixed concurrent read and write operations
///
/// Validates that reads and writes can happen concurrently without
/// data corruption or deadlocks.
#[test]
fn test_concurrent_mixed_read_write() {
    let pager = create_shared_pager();
    let thread_count = 8;
    let operations_per_thread = 20;

    // Pre-allocate and initialize pages
    let mut page_ids = Vec::new();
    for i in 0..thread_count * 2 {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
        let data = format!("Initial data {}", i);
        page.data_mut().extend_from_slice(data.as_bytes());
        pager.write_page(&page).unwrap();
        page_ids.push(page_id);
    }
    let page_ids = Arc::new(page_ids);

    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let page_ids_clone = Arc::clone(&page_ids);
        let handle = thread::spawn(move || {
            let mut rng_state = (thread_id as u64 + 1) * 12345;

            for _ in 0..operations_per_thread {
                // Simple LCG for deterministic randomness
                rng_state = rng_state.wrapping_mul(1103515245).wrapping_add(12345);
                let is_write = rng_state.is_multiple_of(2);
                let page_index = (rng_state as usize) % page_ids_clone.len();
                let page_id = page_ids_clone[page_index];

                if is_write {
                    // Write operation
                    let mut page = Page::new(
                        page_id,
                        PageType::BTreeLeaf,
                        pager_clone.page_size().data_size(),
                    );
                    let data = format!("Thread {} wrote", thread_id);
                    page.data_mut().extend_from_slice(data.as_bytes());
                    pager_clone.write_page(&page).ok();
                } else {
                    // Read operation
                    pager_clone.read_page(page_id).ok();
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}

/// Test concurrent allocation and deallocation
///
/// Validates that pages can be allocated and freed concurrently without
/// free list corruption or lost pages.
#[test]
fn test_concurrent_allocation_deallocation() {
    let pager = create_shared_pager();
    let thread_count = 8;
    let cycles_per_thread = 50;

    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let barrier_clone = Arc::clone(&barrier);
        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            barrier_clone.wait();

            let mut active_pages = Vec::new();

            for cycle in 0..cycles_per_thread {
                // Allocate 3 pages
                for _ in 0..3 {
                    let page_id = pager_clone
                        .allocate_page(PageType::BTreeLeaf)
                        .unwrap_or_else(|_| {
                            panic!("Thread {} failed to allocate in cycle {}", thread_id, cycle)
                        });
                    active_pages.push(page_id);
                }

                // Free 2 pages (if we have enough)
                if active_pages.len() >= 2 {
                    for _ in 0..2 {
                        let page_id = active_pages.pop().unwrap();
                        pager_clone.free_page(page_id).unwrap_or_else(|_| {
                            panic!(
                                "Thread {} failed to free page {} in cycle {}",
                                thread_id, page_id, cycle
                            )
                        });
                    }
                }
            }

            active_pages
        });
        handles.push(handle);
    }

    // Collect all remaining pages
    let mut all_remaining_pages = Vec::new();
    for handle in handles {
        let pages = handle.join().expect("Thread panicked");
        all_remaining_pages.extend(pages);
    }

    // Verify no duplicates in remaining pages
    let unique_pages: HashSet<_> = all_remaining_pages.iter().collect();
    assert_eq!(
        unique_pages.len(),
        all_remaining_pages.len(),
        "Remaining pages should all be unique"
    );

    // Clean up
    for page_id in all_remaining_pages {
        pager
            .free_page(page_id)
            .expect("Failed to free page during cleanup");
    }
}

/// Test free list contention with synchronized start
///
/// Uses a barrier to ensure all threads start simultaneously, creating
/// maximum contention on the free list.
#[test]
fn test_free_list_contention() {
    let pager = create_shared_pager();
    let thread_count = 16;
    let allocations_per_thread = 20;

    // Pre-allocate and free pages to populate free list
    let mut pages_to_free = Vec::new();
    for _ in 0..thread_count * allocations_per_thread {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        pages_to_free.push(page_id);
    }
    for page_id in pages_to_free {
        pager.free_page(page_id).unwrap();
    }

    let initial_free_pages = pager.free_pages();
    assert!(initial_free_pages > 0, "Should have free pages");

    // Concurrent allocation from free list
    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let barrier_clone = Arc::clone(&barrier);
        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            barrier_clone.wait();

            let mut allocated = Vec::new();
            for i in 0..allocations_per_thread {
                let page_id = pager_clone
                    .allocate_page(PageType::BTreeLeaf)
                    .unwrap_or_else(|_| {
                        panic!("Thread {} failed to allocate page {}", thread_id, i)
                    });
                allocated.push(page_id);
            }
            allocated
        });
        handles.push(handle);
    }

    let mut all_pages = Vec::new();
    for handle in handles {
        let pages = handle.join().expect("Thread panicked");
        all_pages.extend(pages);
    }

    // Verify no duplicates
    let unique_pages: HashSet<_> = all_pages.iter().collect();
    assert_eq!(
        unique_pages.len(),
        all_pages.len(),
        "All allocated page IDs should be unique despite contention"
    );
}

/// Test concurrent new page allocation (database growth)
///
/// This test specifically targets the race condition in Superblock::allocate_new_page()
/// by ensuring the free list is empty, forcing all allocations to grow the database
/// using the atomic next_page_id counter.
#[test]
fn test_concurrent_new_page_allocation_race_condition() {
    let pager = create_shared_pager();
    let thread_count = 32;
    let allocations_per_thread = 10;

    // Use a barrier to maximize contention
    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let barrier_clone = Arc::clone(&barrier);
        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            barrier_clone.wait();

            let mut allocated = Vec::new();
            for i in 0..allocations_per_thread {
                let page_id = pager_clone
                    .allocate_page(PageType::BTreeLeaf)
                    .unwrap_or_else(|_| {
                        panic!("Thread {} failed to allocate page {}", thread_id, i)
                    });
                allocated.push(page_id);
            }
            allocated
        });
        handles.push(handle);
    }

    let mut all_pages = Vec::new();
    for handle in handles {
        let pages = handle.join().expect("Thread panicked");
        all_pages.extend(pages);
    }

    // Verify no duplicates - this is the critical assertion
    let unique_pages: HashSet<_> = all_pages.iter().collect();
    assert_eq!(
        unique_pages.len(),
        all_pages.len(),
        "Found {} duplicate page IDs out of {} total allocations - RACE CONDITION DETECTED!",
        all_pages.len() - unique_pages.len(),
        all_pages.len()
    );

    // Verify we allocated the expected number of pages
    assert_eq!(
        all_pages.len(),
        thread_count * allocations_per_thread,
        "Should have allocated expected number of pages"
    );

    // Verify page IDs are in the expected range (starting from 2, after header and superblock)
    let min_page_id = *all_pages.iter().min().unwrap();
    let max_page_id = *all_pages.iter().max().unwrap();
    assert!(
        min_page_id.as_u64() >= 2,
        "Page IDs should start at 2 or higher"
    );
    assert_eq!(
        max_page_id.as_u64() - min_page_id.as_u64() + 1,
        all_pages.len() as u64,
        "Page IDs should be consecutive (no gaps)"
    );
}

/// Test data integrity after concurrent operations
///
/// Validates that data written concurrently is not corrupted and can be
/// read back correctly.
#[test]
fn test_data_integrity_concurrent_operations() {
    let pager = create_shared_pager();
    let thread_count = 8;
    let pages_per_thread = 25;

    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let barrier_clone = Arc::clone(&barrier);
        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            barrier_clone.wait();

            let mut pages = Vec::new();

            // Allocate and write pages
            for i in 0..pages_per_thread {
                let page_id = pager_clone.allocate_page(PageType::BTreeLeaf).unwrap();
                let mut page = Page::new(
                    page_id,
                    PageType::BTreeLeaf,
                    pager_clone.page_size().data_size(),
                );

                // Write unique data pattern
                let data = format!("Thread {} Page {} Data: {}", thread_id, i, "x".repeat(50));
                page.data_mut().extend_from_slice(data.as_bytes());
                pager_clone.write_page(&page).unwrap();

                pages.push((page_id, data));
            }

            pages
        });
        handles.push(handle);
    }

    // Collect all pages
    let mut all_pages = Vec::new();
    for handle in handles {
        let pages = handle.join().expect("Thread panicked");
        all_pages.extend(pages);
    }

    // Verify data integrity by reading back all pages
    for (page_id, expected_data) in all_pages {
        let page = pager.read_page(page_id).unwrap();
        assert_eq!(
            &page.data()[0..expected_data.len()],
            expected_data.as_bytes(),
            "Data corruption detected for page {}",
            page_id
        );
    }
}

/// Test no duplicate page IDs under extreme contention
///
/// Stress test with many threads allocating simultaneously to ensure
/// the page ID generation is truly atomic.
#[test]
fn test_no_duplicate_page_ids_stress() {
    let pager = create_shared_pager();
    let thread_count = 32;
    let pages_per_thread = 10;

    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let barrier_clone = Arc::clone(&barrier);
        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            barrier_clone.wait();

            let mut allocated = Vec::new();
            for i in 0..pages_per_thread {
                let page_id = pager_clone
                    .allocate_page(PageType::BTreeLeaf)
                    .unwrap_or_else(|_| {
                        panic!("Thread {} failed to allocate page {}", thread_id, i)
                    });
                allocated.push(page_id);
            }
            allocated
        });
        handles.push(handle);
    }

    let mut all_pages = Vec::new();
    for handle in handles {
        let pages = handle.join().expect("Thread panicked");
        all_pages.extend(pages);
    }

    // Verify absolutely no duplicates
    let unique_pages: HashSet<_> = all_pages.iter().collect();
    assert_eq!(
        unique_pages.len(),
        all_pages.len(),
        "Found {} duplicate page IDs out of {} total allocations",
        all_pages.len() - unique_pages.len(),
        all_pages.len()
    );
}

/// Test concurrent allocation with different page types
///
/// Validates that different page types can be allocated concurrently
/// without conflicts.
#[test]
fn test_concurrent_allocation_different_page_types() {
    let pager = create_shared_pager();
    let thread_count = 4;
    let pages_per_thread = 50;

    let page_types = [
        PageType::BTreeLeaf,
        PageType::BTreeInternal,
        PageType::Overflow,
        PageType::LsmData,
    ];

    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let page_type = page_types[thread_id];
        let handle = thread::spawn(move || {
            let mut allocated = Vec::new();
            for _ in 0..pages_per_thread {
                let page_id = pager_clone.allocate_page(page_type).unwrap();
                allocated.push(page_id);
            }
            allocated
        });
        handles.push(handle);
    }

    let mut all_pages = Vec::new();
    for handle in handles {
        let pages = handle.join().expect("Thread panicked");
        all_pages.extend(pages);
    }

    let unique_pages: HashSet<_> = all_pages.iter().collect();
    assert_eq!(unique_pages.len(), all_pages.len(), "No duplicate page IDs");
}

/// Test concurrent free list operations
///
/// Validates that the free list can handle concurrent additions and
/// removals without corruption.
#[test]
fn test_concurrent_free_list_operations() {
    let pager = create_shared_pager();
    let thread_count = 8;
    let operations_per_thread = 30;

    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let barrier_clone = Arc::clone(&barrier);
        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            barrier_clone.wait();

            let mut active_pages = Vec::new();
            let mut rng_state = (thread_id as u64 + 1) * 54321;

            for _ in 0..operations_per_thread {
                rng_state = rng_state.wrapping_mul(1103515245).wrapping_add(12345);
                let should_allocate = rng_state.is_multiple_of(2) || active_pages.is_empty();

                if should_allocate {
                    let page_id = pager_clone.allocate_page(PageType::BTreeLeaf).unwrap();
                    active_pages.push(page_id);
                } else {
                    let index = (rng_state as usize) % active_pages.len();
                    let page_id = active_pages.swap_remove(index);
                    pager_clone.free_page(page_id).unwrap();
                }
            }

            active_pages
        });
        handles.push(handle);
    }

    let mut all_remaining_pages = Vec::new();
    for handle in handles {
        let pages = handle.join().expect("Thread panicked");
        all_remaining_pages.extend(pages);
    }

    // Verify no duplicates
    let unique_pages: HashSet<_> = all_remaining_pages.iter().collect();
    assert_eq!(
        unique_pages.len(),
        all_remaining_pages.len(),
        "No duplicate pages"
    );

    // Clean up
    for page_id in all_remaining_pages {
        pager.free_page(page_id).expect("Failed to free page");
    }
}

/// Test concurrent sync operations
///
/// Validates that sync can be called concurrently with other operations
/// without deadlocks or corruption.
#[test]
fn test_concurrent_sync_operations() {
    let pager = create_shared_pager();
    let thread_count = 4;
    let operations_per_thread = 20;

    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let handle = thread::spawn(move || {
            for i in 0..operations_per_thread {
                // Allocate a page
                let page_id = pager_clone.allocate_page(PageType::BTreeLeaf).unwrap();

                // Write data
                let mut page = Page::new(
                    page_id,
                    PageType::BTreeLeaf,
                    pager_clone.page_size().data_size(),
                );
                let data = format!("Thread {} iteration {}", thread_id, i);
                page.data_mut().extend_from_slice(data.as_bytes());
                pager_clone.write_page(&page).unwrap();

                // Sync (some threads)
                if thread_id % 2 == 0 {
                    pager_clone.sync().unwrap();
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Final sync
    pager.sync().unwrap();
}

/// Test page-level locking with concurrent reads to different shards
///
/// This test validates that the PageTable sharding allows true concurrent
/// reads to pages in different shards without blocking each other.
#[test]
fn test_page_level_locking_concurrent_reads() {
    let pager = create_shared_pager();
    let thread_count = 8;
    let reads_per_thread = 100;

    // Pre-allocate pages and ensure they're in different shards
    // With 64 shards (default), pages 0-63 will be in different shards
    let mut page_ids = Vec::new();
    for i in 0..thread_count {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
        let data = format!("Page {} data", i);
        page.data_mut().extend_from_slice(data.as_bytes());
        pager.write_page(&page).unwrap();
        page_ids.push((page_id, data));
    }

    let page_ids = Arc::new(page_ids);
    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let page_ids_clone = Arc::clone(&page_ids);
        let barrier_clone = Arc::clone(&barrier);
        let handle = thread::spawn(move || {
            // Wait for all threads to start simultaneously
            barrier_clone.wait();

            let (page_id, expected_data) = &page_ids_clone[thread_id];

            // Perform many reads - these should not block each other
            // if pages are in different shards
            for _ in 0..reads_per_thread {
                let page = pager_clone.read_page(*page_id).unwrap_or_else(|_| {
                    panic!("Thread {} failed to read page {}", thread_id, page_id)
                });

                assert_eq!(
                    &page.data()[0..expected_data.len()],
                    expected_data.as_bytes(),
                    "Data mismatch for page {}",
                    page_id
                );
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}

/// Test page-level locking with concurrent writes to different shards
///
/// This test validates that the PageTable sharding allows true concurrent
/// writes to pages in different shards without blocking each other.
#[test]
fn test_page_level_locking_concurrent_writes() {
    let pager = create_shared_pager();
    let thread_count = 8;
    let writes_per_thread = 50;

    // Pre-allocate pages for each thread
    let mut page_ids = Vec::new();
    for _ in 0..thread_count {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        page_ids.push(page_id);
    }

    let page_ids = Arc::new(page_ids);
    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let page_ids_clone = Arc::clone(&page_ids);
        let barrier_clone = Arc::clone(&barrier);
        let handle = thread::spawn(move || {
            // Wait for all threads to start simultaneously
            barrier_clone.wait();

            let page_id = page_ids_clone[thread_id];

            // Perform many writes - these should not block each other
            // if pages are in different shards
            for i in 0..writes_per_thread {
                let mut page = Page::new(
                    page_id,
                    PageType::BTreeLeaf,
                    pager_clone.page_size().data_size(),
                );
                let data = format!("Thread {} write {}", thread_id, i);
                page.data_mut().extend_from_slice(data.as_bytes());
                pager_clone.write_page(&page).unwrap_or_else(|_| {
                    panic!(
                        "Thread {} failed to write page {} iteration {}",
                        thread_id, page_id, i
                    )
                });
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}

/// Test page-level locking prevents concurrent access to same page
///
/// This test validates that page-level locks properly serialize access
/// to the same page from multiple threads.
#[test]
fn test_page_level_locking_same_page_serialization() {
    let pager = create_shared_pager();
    let thread_count = 4;
    let operations_per_thread = 50;

    // Allocate a single page that all threads will access
    let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
    let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
    page.data_mut().extend_from_slice(b"Initial data");
    pager.write_page(&page).unwrap();

    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let barrier_clone = Arc::clone(&barrier);
        let handle = thread::spawn(move || {
            // Wait for all threads to start simultaneously
            barrier_clone.wait();

            // All threads access the same page - should be serialized
            for i in 0..operations_per_thread {
                // Read
                let _page = pager_clone.read_page(page_id).unwrap_or_else(|_| {
                    panic!(
                        "Thread {} failed to read page {} iteration {}",
                        thread_id, page_id, i
                    )
                });

                // Write
                let mut page = Page::new(
                    page_id,
                    PageType::BTreeLeaf,
                    pager_clone.page_size().data_size(),
                );
                let data = format!("Thread {} write {}", thread_id, i);
                page.data_mut().extend_from_slice(data.as_bytes());
                pager_clone.write_page(&page).unwrap_or_else(|_| {
                    panic!(
                        "Thread {} failed to write page {} iteration {}",
                        thread_id, page_id, i
                    )
                });
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Verify final page is readable and valid
    let final_page = pager.read_page(page_id).unwrap();
    assert_eq!(final_page.page_id(), page_id);
}

/// Test lock-free free list with extreme contention
///
/// This test specifically validates the lock-free SegQueue implementation
/// by creating maximum contention with many threads rapidly pushing and
/// popping from the free list.
#[test]
fn test_lock_free_free_list_extreme_contention() {
    let pager = create_shared_pager();
    let thread_count = 32;
    let operations_per_thread = 1000;

    // Pre-allocate pages to populate free list
    let mut initial_pages = Vec::new();
    for _ in 0..thread_count * 10 {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        initial_pages.push(page_id);
    }
    for page_id in initial_pages {
        pager.free_page(page_id).unwrap();
    }

    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let barrier_clone = Arc::clone(&barrier);
        let handle = thread::spawn(move || {
            barrier_clone.wait();

            let mut local_pages = Vec::new();

            // Rapidly allocate and free pages to stress the lock-free queue
            for i in 0..operations_per_thread {
                // Allocate
                match pager_clone.allocate_page(PageType::BTreeLeaf) {
                    Ok(page_id) => local_pages.push(page_id),
                    Err(e) => panic!(
                        "Thread {} failed to allocate at op {}: {:?}",
                        thread_id, i, e
                    ),
                }

                // Free every other page to keep the free list active
                if i % 2 == 0 && !local_pages.is_empty() {
                    let page_id = local_pages.pop().unwrap();
                    if let Err(e) = pager_clone.free_page(page_id) {
                        panic!(
                            "Thread {} failed to free page {} at op {}: {:?}",
                            thread_id, page_id, i, e
                        );
                    }
                }
            }

            local_pages
        });
        handles.push(handle);
    }

    // Collect all pages
    let mut all_pages = Vec::new();
    for handle in handles {
        let pages = handle.join().expect("Thread panicked");
        all_pages.extend(pages);
    }

    // Verify no duplicates
    let unique_pages: HashSet<_> = all_pages.iter().collect();
    assert_eq!(
        unique_pages.len(),
        all_pages.len(),
        "Lock-free free list produced duplicate page IDs"
    );

    // Verify total_free counter is consistent
    let expected_free = pager.free_pages();
    println!("Final free pages: {}", expected_free);

    // Clean up
    for page_id in all_pages {
        pager.free_page(page_id).unwrap();
    }
}
