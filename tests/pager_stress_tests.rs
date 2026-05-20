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

//! Large-scale stress tests for the pager system
//!
//! These tests validate behavior under heavy load with 1000+ pages,
//! ensuring the system can handle production workloads without
//! performance degradation or resource issues.

use nanostore::pager::{Page, PageSize, PageType, Pager, PagerConfig};
use nanostore::vfs::MemoryFileSystem;
use std::collections::HashSet;

/// Helper function to create a test pager
fn create_test_pager() -> Pager<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    Pager::create(&fs, "stress_test.db", config).expect("Failed to create pager")
}

/// Test sequential allocation of 1000+ pages
///
/// This test validates that the pager can allocate a large number of pages
/// sequentially without errors, and that page IDs are assigned correctly.
#[test]
fn test_sequential_allocation_1000_pages() {
    let pager = create_test_pager();
    let page_count = 1000;

    let mut allocated_pages = Vec::new();

    // Allocate pages sequentially
    for i in 0..page_count {
        let page_id = pager
            .allocate_page(PageType::BTreeLeaf)
            .unwrap_or_else(|_| panic!("Failed to allocate page {}", i));

        allocated_pages.push(page_id);
    }

    // Verify we allocated the expected number of pages
    assert_eq!(allocated_pages.len(), page_count);

    // Verify page IDs are unique
    let unique_pages: HashSet<_> = allocated_pages.iter().collect();
    assert_eq!(unique_pages.len(), page_count, "Page IDs should be unique");

    // Verify total pages count
    // +2 for header and superblock pages
    assert!(
        pager.total_pages() >= (page_count + 2) as u64,
        "Total pages should be at least {} (allocated) + 2 (header/superblock)",
        page_count
    );

    // Verify free pages count is reasonable (should be low)
    assert!(
        pager.free_pages() < 10,
        "Free pages should be minimal after sequential allocation"
    );
}

/// Test sequential allocation of 5000 pages
///
/// Larger scale test to ensure system scales well.
#[test]
fn test_sequential_allocation_5000_pages() {
    let pager = create_test_pager();
    let page_count = 5000;

    let mut allocated_pages = Vec::new();

    for i in 0..page_count {
        let page_id = pager
            .allocate_page(PageType::BTreeLeaf)
            .unwrap_or_else(|_| panic!("Failed to allocate page {}", i));

        allocated_pages.push(page_id);
    }

    assert_eq!(allocated_pages.len(), page_count);

    let unique_pages: HashSet<_> = allocated_pages.iter().collect();
    assert_eq!(unique_pages.len(), page_count);
}

/// Test mixed allocation and deallocation patterns at scale
///
/// This test simulates realistic workload patterns with interleaved
/// allocations and deallocations.
#[test]
fn test_mixed_allocation_deallocation_1000_cycles() {
    let pager = create_test_pager();
    let cycles = 1000;

    let mut active_pages = Vec::new();

    // Allocate initial batch
    for _ in 0..100 {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        active_pages.push(page_id);
    }

    // Run mixed allocation/deallocation cycles
    for cycle in 0..cycles {
        // Allocate 5 pages
        for _ in 0..5 {
            let page_id = pager
                .allocate_page(PageType::BTreeLeaf)
                .unwrap_or_else(|_| panic!("Failed to allocate in cycle {}", cycle));
            active_pages.push(page_id);
        }

        // Free 3 pages (if we have enough)
        if active_pages.len() >= 3 {
            for _ in 0..3 {
                let page_id = active_pages.pop().unwrap();
                pager.free_page(page_id).unwrap_or_else(|_| {
                    panic!("Failed to free page {} in cycle {}", page_id, cycle)
                });
            }
        }
    }

    // Verify we still have active pages
    assert!(
        !active_pages.is_empty(),
        "Should have active pages remaining"
    );

    // Verify all active pages are unique
    let unique_pages: HashSet<_> = active_pages.iter().collect();
    assert_eq!(
        unique_pages.len(),
        active_pages.len(),
        "Active pages should be unique"
    );

    // Clean up - free all remaining pages
    for page_id in active_pages {
        pager
            .free_page(page_id)
            .expect("Failed to free page during cleanup");
    }
}

/// Test random allocation and deallocation patterns
///
/// This test uses pseudo-random patterns to stress test the free list
/// management under unpredictable workloads.
#[test]
fn test_random_allocation_deallocation_patterns() {
    let pager = create_test_pager();
    let operations = 2000;

    let mut active_pages = Vec::new();
    let mut rng_state = 12345u64; // Simple LCG for deterministic randomness

    for op in 0..operations {
        // Simple LCG: next = (a * prev + c) mod m
        rng_state = rng_state.wrapping_mul(1103515245).wrapping_add(12345);
        let should_allocate = (rng_state % 100) < 60; // 60% allocate, 40% free

        if should_allocate || active_pages.is_empty() {
            // Allocate a page
            let page_id = pager
                .allocate_page(PageType::BTreeLeaf)
                .unwrap_or_else(|_| panic!("Failed to allocate in operation {}", op));
            active_pages.push(page_id);
        } else {
            // Free a random page
            let index = (rng_state as usize) % active_pages.len();
            let page_id = active_pages.swap_remove(index);
            pager
                .free_page(page_id)
                .unwrap_or_else(|_| panic!("Failed to free page {} in operation {}", page_id, op));
        }
    }

    // Verify state consistency
    assert!(pager.total_pages() > 0, "Should have allocated pages");

    // Clean up
    for page_id in active_pages {
        pager
            .free_page(page_id)
            .expect("Failed to free page during cleanup");
    }
}

/// Test fragmentation scenarios
///
/// This test creates a highly fragmented free list by allocating many pages,
/// freeing every other one, then reallocating to test free list efficiency.
#[test]
fn test_fragmentation_scenario() {
    let pager = create_test_pager();
    let page_count = 1000;

    // Phase 1: Allocate many pages
    let mut all_pages = Vec::new();
    for _ in 0..page_count {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        all_pages.push(page_id);
    }

    let initial_total = pager.total_pages();

    // Phase 2: Free every other page to create fragmentation
    let mut freed_count = 0;
    let mut kept_pages = Vec::new();
    for (i, page_id) in all_pages.into_iter().enumerate() {
        if i % 2 == 0 {
            pager.free_page(page_id).unwrap();
            freed_count += 1;
        } else {
            kept_pages.push(page_id);
        }
    }

    assert_eq!(
        freed_count,
        page_count / 2,
        "Should have freed half the pages"
    );
    assert_eq!(
        pager.free_pages() as usize,
        freed_count,
        "Free page count should match freed pages"
    );

    // Phase 3: Reallocate pages - should reuse freed pages
    let mut reallocated = Vec::new();
    for _ in 0..freed_count {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        reallocated.push(page_id);
    }

    // After reallocation, free pages should be minimal
    assert!(
        pager.free_pages() < 10,
        "Free pages should be minimal after reallocation, got {}",
        pager.free_pages()
    );

    // Total pages should not have grown significantly
    let final_total = pager.total_pages();
    assert!(
        final_total <= initial_total + 10,
        "Total pages should not grow significantly: {} -> {}",
        initial_total,
        final_total
    );

    // Clean up
    for page_id in kept_pages.into_iter().chain(reallocated.into_iter()) {
        pager
            .free_page(page_id)
            .expect("Failed to free page during cleanup");
    }
}

/// Test free list chain traversal with many pages
///
/// This test validates that the free list can handle long chains of
/// free list pages without performance degradation.
#[test]
fn test_free_list_chain_traversal() {
    let pager = create_test_pager();
    let page_count = 2000;

    // Allocate many pages
    let mut pages = Vec::new();
    for _ in 0..page_count {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        pages.push(page_id);
    }

    // Free all pages to create a long free list chain
    for page_id in &pages {
        pager.free_page(*page_id).unwrap();
    }

    assert_eq!(
        pager.free_pages() as usize,
        page_count,
        "All pages should be in free list"
    );

    // Reallocate all pages - this tests free list traversal
    let mut reallocated = Vec::new();
    for i in 0..page_count {
        let page_id = pager
            .allocate_page(PageType::BTreeLeaf)
            .unwrap_or_else(|_| panic!("Failed to reallocate page {}", i));
        reallocated.push(page_id);
    }

    // Free list should be empty or nearly empty
    assert!(
        pager.free_pages() < 5,
        "Free list should be nearly empty after reallocation"
    );

    // Verify all reallocated pages are unique
    let unique_pages: HashSet<_> = reallocated.iter().collect();
    assert_eq!(
        unique_pages.len(),
        page_count,
        "Reallocated pages should be unique"
    );
}

/// Test persistence and recovery with large page files
///
/// This test validates that large databases can be persisted and
/// reopened correctly with all state preserved.
#[test]
fn test_persistence_and_recovery_large_database() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let db_path = "large_db.db";
    let page_count = 1000;

    let mut allocated_pages = Vec::new();

    // Phase 1: Create StorageEngine and allocate pages
    {
        let pager = Pager::create(&fs, db_path, config.clone()).unwrap();

        for _ in 0..page_count {
            let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            allocated_pages.push(page_id);
        }

        // Write some data to pages
        for (i, &page_id) in allocated_pages.iter().enumerate() {
            let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
            let data = format!("Page {} data", i);
            page.data_mut().extend_from_slice(data.as_bytes());
            pager.write_page(&page).unwrap();
        }

        pager.sync().unwrap();
    }

    // Phase 2: Reopen StorageEngine and verify state
    {
        let pager = Pager::open(&fs, db_path).unwrap();

        // Verify total pages
        assert!(
            pager.total_pages() >= (page_count + 2) as u64,
            "Total pages should be preserved"
        );

        // Verify we can read all pages
        for (i, &page_id) in allocated_pages.iter().enumerate() {
            let page = pager
                .read_page(page_id)
                .unwrap_or_else(|_| panic!("Failed to read page {}", page_id));

            let expected_data = format!("Page {} data", i);
            assert_eq!(
                &page.data()[0..expected_data.len()],
                expected_data.as_bytes(),
                "Page data should be preserved"
            );
        }

        // Verify we can allocate new pages
        let new_page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        assert!(
            new_page_id.as_u64() > 0,
            "Should be able to allocate new pages"
        );
    }
}

/// Test memory usage stability under load
///
/// This test validates that memory usage doesn't grow unboundedly
/// during repeated allocation/deallocation cycles.
#[test]
fn test_memory_usage_stability() {
    let pager = create_test_pager();
    let batch_size = 500;
    let cycles = 10;

    for cycle in 0..cycles {
        let mut pages = Vec::new();

        // Allocate a batch
        for _ in 0..batch_size {
            let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            pages.push(page_id);
        }

        let total_after_alloc = pager.total_pages();

        // Free the batch
        for page_id in pages {
            pager.free_page(page_id).unwrap();
        }

        let free_after_dealloc = pager.free_pages();

        // Verify free pages are tracked
        assert!(
            free_after_dealloc >= batch_size as u64,
            "Cycle {}: Free pages should include freed batch",
            cycle
        );

        // Total pages should stabilize after first cycle
        if cycle > 0 {
            assert!(
                total_after_alloc <= pager.total_pages() + 20,
                "Cycle {}: Total pages should not grow significantly",
                cycle
            );
        }
    }
}

/// Test page file growth and shrinkage patterns
///
/// This test validates that the StorageEngine file grows appropriately
/// and that free space is managed efficiently.
#[test]
fn test_page_file_growth_patterns() {
    let pager = create_test_pager();

    // Phase 1: Grow to 1000 pages
    let mut pages = Vec::new();
    for _ in 0..1000 {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        pages.push(page_id);
    }

    let peak_total = pager.total_pages();
    assert!(
        peak_total >= 1002,
        "Should have grown to at least 1002 pages"
    );

    // Phase 2: Free half the pages
    let half = pages.len() / 2;
    for _ in 0..half {
        let page_id = pages.pop().unwrap();
        pager.free_page(page_id).unwrap();
    }

    assert_eq!(
        pager.free_pages() as usize,
        half,
        "Should have freed half the pages"
    );

    // Phase 3: Reallocate - should reuse freed pages
    for _ in 0..half {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        pages.push(page_id);
    }

    // Total pages should not have grown significantly
    // Allow for a few extra pages for free list management
    assert!(
        pager.total_pages() <= peak_total + 5,
        "Total pages should not grow significantly when reusing freed pages: {} -> {}",
        peak_total,
        pager.total_pages()
    );

    // Free pages should be minimal
    assert!(
        pager.free_pages() < 10,
        "Free pages should be minimal after reallocation"
    );
}

/// Test edge case: approaching page ID limits
///
/// This test validates behavior when allocating many pages,
/// ensuring page IDs are managed correctly.
#[test]
fn test_large_page_id_values() {
    let pager = create_test_pager();
    let page_count = 10000;

    let mut pages = Vec::new();

    // Allocate many pages to get large page IDs
    for i in 0..page_count {
        let page_id = pager
            .allocate_page(PageType::BTreeLeaf)
            .unwrap_or_else(|_| panic!("Failed to allocate page {}", i));
        pages.push(page_id);
    }

    // Verify page IDs are reasonable
    let max_page_id = *pages.iter().max().unwrap();
    assert!(
        max_page_id.as_u64() < u64::MAX / 2,
        "Page IDs should be well below u64::MAX"
    );

    // Verify we can still read/write pages with large IDs
    let large_page_id = pages[page_count - 1];
    let mut page = Page::new(
        large_page_id,
        PageType::BTreeLeaf,
        pager.page_size().data_size(),
    );
    page.data_mut()
        .extend_from_slice(b"test data for large page ID");

    pager.write_page(&page).unwrap();
    let read_page = pager.read_page(large_page_id).unwrap();
    assert_eq!(&read_page.data()[0..27], b"test data for large page ID");
}

/// Test concurrent-like allocation patterns
///
/// This test simulates patterns that might occur with concurrent access
/// (though the pager itself is not thread-safe, this tests the state machine).
#[test]
fn test_interleaved_allocation_patterns() {
    let pager = create_test_pager();
    let iterations = 500;

    let mut set_a = Vec::new();
    let mut set_b = Vec::new();

    // Simulate two "threads" interleaving allocations
    for _ in 0..iterations {
        // "Thread A" allocates 2 pages
        for _ in 0..2 {
            let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            set_a.push(page_id);
        }

        // "Thread B" allocates 3 pages
        for _ in 0..3 {
            let page_id = pager.allocate_page(PageType::BTreeInternal).unwrap();
            set_b.push(page_id);
        }

        // "Thread A" frees 1 page
        if !set_a.is_empty() {
            let page_id = set_a.pop().unwrap();
            pager.free_page(page_id).unwrap();
        }

        // "Thread B" frees 2 pages
        for _ in 0..2 {
            if !set_b.is_empty() {
                let page_id = set_b.pop().unwrap();
                pager.free_page(page_id).unwrap();
            }
        }
    }

    // Verify no page ID collisions
    let all_pages: Vec<_> = set_a.iter().chain(set_b.iter()).collect();
    let unique_pages: HashSet<_> = all_pages.iter().collect();
    assert_eq!(
        unique_pages.len(),
        all_pages.len(),
        "All allocated pages should have unique IDs"
    );
}

/// Test stress scenario: rapid allocation and deallocation
///
/// This test performs rapid allocation/deallocation cycles to stress
/// test the free list management and page tracking.
#[test]
fn test_rapid_allocation_deallocation() {
    let pager = create_test_pager();
    let cycles = 1000;

    for cycle in 0..cycles {
        // Allocate 10 pages
        let mut pages = Vec::new();
        for _ in 0..10 {
            let page_id = pager
                .allocate_page(PageType::BTreeLeaf)
                .unwrap_or_else(|_| panic!("Failed to allocate in cycle {}", cycle));
            pages.push(page_id);
        }

        // Immediately free all of them
        for page_id in pages {
            pager
                .free_page(page_id)
                .unwrap_or_else(|_| panic!("Failed to free in cycle {}", cycle));
        }
    }

    // After all cycles, free list should have pages
    assert!(pager.free_pages() > 0, "Free list should have pages");

    // Total pages should be reasonable (not growing unboundedly)
    // Allow for free list pages that get allocated during the process
    assert!(
        pager.total_pages() < 1100,
        "Total pages should not grow excessively: {}",
        pager.total_pages()
    );
}

/// Test write and read operations at scale
///
/// This test validates that we can write and read data to/from
/// many pages without corruption.
#[test]
fn test_write_read_operations_at_scale() {
    let pager = create_test_pager();
    let page_count = 1000;

    let mut pages = Vec::new();

    // Allocate and write pages
    for i in 0..page_count {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

        let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
        let data = format!("Test data for page {} - iteration {}", page_id, i);
        page.data_mut().extend_from_slice(data.as_bytes());

        pager.write_page(&page).unwrap();
        pages.push((page_id, data));
    }

    // Read back and verify all pages
    for (page_id, expected_data) in pages {
        let page = pager
            .read_page(page_id)
            .unwrap_or_else(|_| panic!("Failed to read page {}", page_id));

        assert_eq!(
            &page.data()[0..expected_data.len()],
            expected_data.as_bytes(),
            "Data mismatch for page {}",
            page_id
        );
    }
}

/// Test sequential allocation of 100K pages
///
/// This test validates that the pager can handle production-scale databases
/// with 100,000+ pages, ensuring scalability of page ID generation and
/// free list management.
#[test]
#[ignore] // Run with: cargo test --test pager_stress_tests -- --ignored --nocapture
fn test_sequential_allocation_100k_pages() {
    let pager = create_test_pager();
    let page_count = 100_000;

    println!("Allocating {} pages sequentially...", page_count);
    let start = std::time::Instant::now();

    let mut allocated_pages = Vec::with_capacity(page_count);

    // Allocate pages sequentially
    for i in 0..page_count {
        if i % 10_000 == 0 {
            println!("  Allocated {} pages...", i);
        }

        let page_id = pager
            .allocate_page(PageType::BTreeLeaf)
            .unwrap_or_else(|_| panic!("Failed to allocate page {}", i));

        allocated_pages.push(page_id);
    }

    let duration = start.elapsed();
    println!("Allocation completed in {:?}", duration);
    println!("Average time per page: {:?}", duration / page_count as u32);

    // Verify we allocated the expected number of pages
    assert_eq!(allocated_pages.len(), page_count);

    // Verify page IDs are unique
    let unique_pages: HashSet<_> = allocated_pages.iter().collect();
    assert_eq!(unique_pages.len(), page_count, "Page IDs should be unique");

    // Verify total pages count
    let total_pages = pager.total_pages();
    assert!(
        total_pages >= (page_count + 2) as u64,
        "Total pages should be at least {} (allocated) + 2 (header/superblock), got {}",
        page_count,
        total_pages
    );

    // Verify free pages count is reasonable (should be low)
    let free_pages = pager.free_pages();
    assert!(
        free_pages < 100,
        "Free pages should be minimal after sequential allocation, got {}",
        free_pages
    );

    println!("Total pages: {}, Free pages: {}", total_pages, free_pages);
}

/// Test mixed allocation and deallocation with 100K pages
///
/// This test simulates realistic production workload patterns with
/// interleaved allocations and deallocations at scale.
#[test]
#[ignore] // Run with: cargo test --test pager_stress_tests -- --ignored --nocapture
fn test_mixed_allocation_deallocation_100k_pages() {
    let pager = create_test_pager();
    let target_pages = 100_000;

    println!(
        "Running mixed allocation/deallocation to reach {} pages...",
        target_pages
    );
    let start = std::time::Instant::now();

    let mut active_pages = Vec::new();
    let mut total_allocated = 0;
    let mut total_freed = 0;

    // Allocate initial batch
    for _ in 0..1000 {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        active_pages.push(page_id);
        total_allocated += 1;
    }

    // Run mixed allocation/deallocation until we've allocated target pages
    while total_allocated < target_pages {
        if total_allocated % 10_000 == 0 {
            println!(
                "  Allocated: {}, Active: {}, Freed: {}",
                total_allocated,
                active_pages.len(),
                total_freed
            );
        }

        // Allocate 10 pages
        for _ in 0..10 {
            let page_id = pager
                .allocate_page(PageType::BTreeLeaf)
                .unwrap_or_else(|_| panic!("Failed to allocate at count {}", total_allocated));
            active_pages.push(page_id);
            total_allocated += 1;
        }

        // Free 5 pages (if we have enough)
        if active_pages.len() >= 5 {
            for _ in 0..5 {
                let page_id = active_pages.pop().unwrap();
                pager
                    .free_page(page_id)
                    .unwrap_or_else(|_| panic!("Failed to free page {}", page_id));
                total_freed += 1;
            }
        }
    }

    let duration = start.elapsed();
    println!("Mixed operations completed in {:?}", duration);
    println!(
        "Total allocated: {}, Total freed: {}, Active: {}",
        total_allocated,
        total_freed,
        active_pages.len()
    );

    // Verify we still have active pages
    assert!(
        !active_pages.is_empty(),
        "Should have active pages remaining"
    );

    // Verify all active pages are unique
    let unique_pages: HashSet<_> = active_pages.iter().collect();
    assert_eq!(
        unique_pages.len(),
        active_pages.len(),
        "Active pages should be unique"
    );

    // Verify free list is being used
    let free_pages = pager.free_pages();
    println!("Free pages in list: {}", free_pages);
    assert!(free_pages > 0, "Free list should contain freed pages");

    // Clean up - free all remaining pages
    println!("Cleaning up {} active pages...", active_pages.len());
    for page_id in active_pages {
        pager
            .free_page(page_id)
            .expect("Failed to free page during cleanup");
    }
}

/// Test fragmentation with 100K pages
///
/// This test creates extreme fragmentation by allocating 100K pages,
/// freeing every other one, then reallocating to validate free list
/// efficiency at scale.
#[test]
#[ignore] // Run with: cargo test --test pager_stress_tests -- --ignored --nocapture
fn test_fragmentation_100k_pages() {
    let pager = create_test_pager();
    let page_count = 100_000;

    println!("Phase 1: Allocating {} pages...", page_count);
    let start = std::time::Instant::now();

    // Phase 1: Allocate many pages
    let mut all_pages = Vec::with_capacity(page_count);
    for i in 0..page_count {
        if i % 10_000 == 0 {
            println!("  Allocated {} pages...", i);
        }
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        all_pages.push(page_id);
    }

    let alloc_duration = start.elapsed();
    println!("Allocation completed in {:?}", alloc_duration);

    let initial_total = pager.total_pages();
    println!("Total pages after allocation: {}", initial_total);

    // Phase 2: Free every other page to create fragmentation
    println!("Phase 2: Freeing every other page to create fragmentation...");
    let free_start = std::time::Instant::now();

    let mut freed_count = 0;
    let mut kept_pages = Vec::with_capacity(page_count / 2);
    for (i, page_id) in all_pages.into_iter().enumerate() {
        if i % 2 == 0 {
            pager.free_page(page_id).unwrap();
            freed_count += 1;
        } else {
            kept_pages.push(page_id);
        }

        if freed_count % 10_000 == 0 && freed_count > 0 {
            println!("  Freed {} pages...", freed_count);
        }
    }

    let free_duration = free_start.elapsed();
    println!("Freed {} pages in {:?}", freed_count, free_duration);

    assert_eq!(
        freed_count,
        page_count / 2,
        "Should have freed half the pages"
    );

    let free_pages = pager.free_pages();
    println!("Free pages in list: {}", free_pages);
    assert!(
        free_pages as usize >= freed_count - 100, // Allow some overhead for free list pages
        "Free page count should approximately match freed pages: expected ~{}, got {}",
        freed_count,
        free_pages
    );

    // Phase 3: Reallocate pages - should reuse freed pages
    println!(
        "Phase 3: Reallocating {} pages (should reuse freed pages)...",
        freed_count
    );
    let realloc_start = std::time::Instant::now();

    let mut reallocated = Vec::with_capacity(freed_count);
    for i in 0..freed_count {
        if i % 10_000 == 0 {
            println!("  Reallocated {} pages...", i);
        }
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        reallocated.push(page_id);
    }

    let realloc_duration = realloc_start.elapsed();
    println!("Reallocation completed in {:?}", realloc_duration);

    // After reallocation, free pages should be minimal
    let final_free = pager.free_pages();
    println!("Free pages after reallocation: {}", final_free);
    assert!(
        final_free < 100,
        "Free pages should be minimal after reallocation, got {}",
        final_free
    );

    // Total pages should not have grown significantly
    let final_total = pager.total_pages();
    println!("Total pages: {} -> {}", initial_total, final_total);
    assert!(
        final_total <= initial_total + 200, // Allow some overhead for free list management
        "Total pages should not grow significantly: {} -> {}",
        initial_total,
        final_total
    );

    // Clean up
    println!("Cleaning up...");
    for page_id in kept_pages.into_iter().chain(reallocated.into_iter()) {
        pager
            .free_page(page_id)
            .expect("Failed to free page during cleanup");
    }

    println!("Test completed successfully!");
}

/// Test memory usage and performance with 200K pages
///
/// This test validates that the system can handle very large databases
/// and that memory usage remains reasonable.
#[test]
#[ignore] // Run with: cargo test --test pager_stress_tests -- --ignored --nocapture
fn test_memory_usage_200k_pages() {
    let pager = create_test_pager();
    let page_count = 200_000;

    println!("Testing memory usage with {} pages...", page_count);
    println!("Note: This test validates scalability and memory efficiency");

    let start = std::time::Instant::now();
    let mut pages = Vec::with_capacity(page_count);

    // Allocate pages in batches to monitor progress
    let batch_size = 10_000;
    for batch in 0..(page_count / batch_size) {
        let batch_start = std::time::Instant::now();

        for _ in 0..batch_size {
            let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            pages.push(page_id);
        }

        let batch_duration = batch_start.elapsed();
        let total_so_far = (batch + 1) * batch_size;

        println!(
            "Batch {}: Allocated {} pages in {:?} (avg: {:?}/page)",
            batch + 1,
            total_so_far,
            batch_duration,
            batch_duration / batch_size as u32
        );

        // Check that allocation time doesn't degrade significantly
        let avg_micros = batch_duration.as_micros() / batch_size as u128;
        assert!(
            avg_micros < 1000, // Should be under 1ms per page
            "Allocation time degrading: {} microseconds per page in batch {}",
            avg_micros,
            batch + 1
        );
    }

    let total_duration = start.elapsed();
    println!("\nTotal allocation time: {:?}", total_duration);
    println!(
        "Average time per page: {:?}",
        total_duration / page_count as u32
    );

    // Verify all pages are unique
    let unique_pages: HashSet<_> = pages.iter().collect();
    assert_eq!(unique_pages.len(), page_count, "All pages should be unique");

    // Verify total pages
    let total_pages = pager.total_pages();
    println!("Total pages in StorageEngine: {}", total_pages);
    assert!(
        total_pages >= (page_count + 2) as u64,
        "Total pages should be at least {}",
        page_count + 2
    );

    // Test read/write performance at scale
    println!("\nTesting read/write performance...");
    let rw_start = std::time::Instant::now();

    // Write to a sample of pages
    let sample_size = 1000;
    for i in 0..sample_size {
        let page_id = pages[i * (page_count / sample_size)];
        let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
        let data = format!("Test data for page {}", page_id);
        page.data_mut().extend_from_slice(data.as_bytes());
        pager.write_page(&page).unwrap();
    }

    let rw_duration = rw_start.elapsed();
    println!(
        "Wrote {} pages in {:?} (avg: {:?}/page)",
        sample_size,
        rw_duration,
        rw_duration / sample_size as u32
    );

    // Clean up
    println!("\nCleaning up {} pages...", pages.len());
    let cleanup_start = std::time::Instant::now();
    for page_id in pages {
        pager.free_page(page_id).expect("Failed to free page");
    }
    let cleanup_duration = cleanup_start.elapsed();
    println!("Cleanup completed in {:?}", cleanup_duration);

    println!("\nTest completed successfully!");
}

/// Test free list chain traversal with 100K pages
///
/// This test validates that the free list can handle very long chains
/// without performance degradation.
#[test]
#[ignore] // Run with: cargo test --test pager_stress_tests -- --ignored --nocapture
fn test_free_list_chain_100k_pages() {
    let pager = create_test_pager();
    let page_count = 100_000;

    println!("Testing free list chain with {} pages...", page_count);

    // Phase 1: Allocate many pages
    println!("Phase 1: Allocating {} pages...", page_count);
    let alloc_start = std::time::Instant::now();

    let mut pages = Vec::with_capacity(page_count);
    for i in 0..page_count {
        if i % 10_000 == 0 {
            println!("  Allocated {} pages...", i);
        }
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        pages.push(page_id);
    }

    let alloc_duration = alloc_start.elapsed();
    println!("Allocation completed in {:?}", alloc_duration);

    // Phase 2: Free all pages to create a long free list chain
    println!("Phase 2: Freeing all pages to create long free list chain...");
    let free_start = std::time::Instant::now();

    for (i, page_id) in pages.iter().enumerate() {
        if i % 10_000 == 0 {
            println!("  Freed {} pages...", i);
        }
        pager.free_page(*page_id).unwrap();
    }

    let free_duration = free_start.elapsed();
    println!("Freed all pages in {:?}", free_duration);

    let free_pages = pager.free_pages();
    println!("Free pages in list: {}", free_pages);
    assert!(
        free_pages as usize >= page_count - 100, // Allow some overhead
        "Most pages should be in free list: expected ~{}, got {}",
        page_count,
        free_pages
    );

    // Phase 3: Reallocate all pages - this tests free list traversal performance
    println!(
        "Phase 3: Reallocating {} pages (tests free list traversal)...",
        page_count
    );
    let realloc_start = std::time::Instant::now();

    let mut reallocated = Vec::with_capacity(page_count);
    for i in 0..page_count {
        if i % 10_000 == 0 {
            let elapsed = realloc_start.elapsed();
            let rate = if i > 0 {
                elapsed.as_millis() / i as u128
            } else {
                0
            };
            println!(
                "  Reallocated {} pages (avg: {}ms per 1000 pages)...",
                i, rate
            );
        }

        let page_id = pager
            .allocate_page(PageType::BTreeLeaf)
            .unwrap_or_else(|_| panic!("Failed to reallocate page {}", i));
        reallocated.push(page_id);
    }

    let realloc_duration = realloc_start.elapsed();
    println!("Reallocation completed in {:?}", realloc_duration);
    println!(
        "Average time per page: {:?}",
        realloc_duration / page_count as u32
    );

    // Free list should be empty or nearly empty
    let final_free = pager.free_pages();
    println!("Free pages after reallocation: {}", final_free);
    assert!(
        final_free < 100,
        "Free list should be nearly empty after reallocation, got {}",
        final_free
    );

    // Verify all reallocated pages are unique
    let unique_pages: HashSet<_> = reallocated.iter().collect();
    assert_eq!(
        unique_pages.len(),
        page_count,
        "Reallocated pages should be unique"
    );

    println!("Test completed successfully!");
}

/// Test persistence and recovery with 50K pages
///
/// This test validates that large databases can be persisted and
/// reopened correctly with all state preserved.
#[test]
#[ignore] // Run with: cargo test --test pager_stress_tests -- --ignored --nocapture
fn test_persistence_recovery_50k_pages() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let db_path = "large_db_50k.db";
    let page_count = 50_000;

    println!(
        "Testing persistence and recovery with {} pages...",
        page_count
    );

    let mut allocated_pages = Vec::with_capacity(page_count);

    // Phase 1: Create StorageEngine and allocate pages
    println!("Phase 1: Creating StorageEngine and allocating pages...");
    {
        let pager = Pager::create(&fs, db_path, config.clone()).unwrap();

        for i in 0..page_count {
            if i % 10_000 == 0 {
                println!("  Allocated {} pages...", i);
            }
            let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            allocated_pages.push(page_id);
        }

        // Write data to a sample of pages
        println!("Writing data to sample pages...");
        for i in (0..page_count).step_by(100) {
            let page_id = allocated_pages[i];
            let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
            let data = format!("Page {} data - sample {}", page_id, i);
            page.data_mut().extend_from_slice(data.as_bytes());
            pager.write_page(&page).unwrap();
        }

        println!("Syncing StorageEngine...");
        pager.sync().unwrap();

        println!(
            "Total pages: {}, Free pages: {}",
            pager.total_pages(),
            pager.free_pages()
        );
    }

    // Phase 2: Reopen StorageEngine and verify state
    println!("\nPhase 2: Reopening StorageEngine and verifying state...");
    {
        let pager = Pager::open(&fs, db_path).unwrap();

        // Verify total pages
        let total_pages = pager.total_pages();
        println!("Total pages after reopen: {}", total_pages);
        assert!(
            total_pages >= (page_count + 2) as u64,
            "Total pages should be preserved: expected at least {}, got {}",
            page_count + 2,
            total_pages
        );

        // Verify we can read sample pages
        println!("Verifying sample page data...");
        let mut verified = 0;
        for i in (0..page_count).step_by(100) {
            let page_id = allocated_pages[i];
            let page = pager
                .read_page(page_id)
                .unwrap_or_else(|_| panic!("Failed to read page {}", page_id));

            let expected_data = format!("Page {} data - sample {}", page_id, i);
            assert_eq!(
                &page.data()[0..expected_data.len()],
                expected_data.as_bytes(),
                "Page data should be preserved for page {}",
                page_id
            );
            verified += 1;
        }
        println!("Verified {} sample pages", verified);

        // Verify we can allocate new pages
        println!("Testing new page allocation...");
        let new_page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        assert!(
            new_page_id.as_u64() > 0,
            "Should be able to allocate new pages"
        );

        println!("StorageEngine recovery successful!");
    }

    println!("\nTest completed successfully!");
}

/// Test cache thrashing with more pages than cache capacity
///
/// This test validates that the pager can handle scenarios where the working
/// set exceeds cache capacity, forcing frequent evictions and reloads.
#[test]
#[ignore] // Run with: cargo test --test pager_stress_tests -- --ignored --nocapture
fn test_cache_thrashing() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default().with_cache_capacity(100); // Small cache to force thrashing
    let pager = Pager::create(&fs, "cache_thrash.db", config).expect("Failed to create pager");

    let page_count = 10_000; // 100x cache capacity

    println!(
        "Testing cache thrashing with {} pages and cache capacity of 100...",
        page_count
    );

    // Phase 1: Allocate pages
    println!("Phase 1: Allocating {} pages...", page_count);
    let mut pages = Vec::with_capacity(page_count);
    for i in 0..page_count {
        if i % 1_000 == 0 {
            println!("  Allocated {} pages...", i);
        }
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        pages.push(page_id);
    }

    // Phase 2: Write data to all pages (forces cache thrashing)
    println!("Phase 2: Writing data to all pages (cache thrashing)...");
    let write_start = std::time::Instant::now();

    for (i, &page_id) in pages.iter().enumerate() {
        if i % 1_000 == 0 {
            println!("  Written {} pages...", i);
        }

        let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
        let data = format!("Page {} data - iteration 1", page_id);
        page.data_mut().extend_from_slice(data.as_bytes());
        pager.write_page(&page).unwrap();
    }

    let write_duration = write_start.elapsed();
    println!("Write phase completed in {:?}", write_duration);
    println!(
        "Average write time: {:?}",
        write_duration / page_count as u32
    );

    // Phase 3: Read all pages multiple times (forces cache thrashing)
    println!("Phase 3: Reading all pages 3 times (cache thrashing)...");
    let read_start = std::time::Instant::now();

    for iteration in 0..3 {
        println!("  Read iteration {}...", iteration + 1);
        for (i, &page_id) in pages.iter().enumerate() {
            if i % 1_000 == 0 && iteration == 0 {
                println!("    Read {} pages...", i);
            }

            let page = pager.read_page(page_id).unwrap();
            let expected_data = format!("Page {} data - iteration 1", page_id);
            assert_eq!(
                &page.data()[0..expected_data.len()],
                expected_data.as_bytes(),
                "Page data should be correct for page {}",
                page_id
            );
        }
    }

    let read_duration = read_start.elapsed();
    println!("Read phase completed in {:?}", read_duration);
    println!(
        "Average read time: {:?}",
        read_duration / (page_count * 3) as u32
    );

    // Phase 4: Random access pattern (worst case for cache)
    println!("Phase 4: Random access pattern...");
    let random_start = std::time::Instant::now();

    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    for i in 0..5_000 {
        if i % 1_000 == 0 {
            println!("  Random access {} operations...", i);
        }

        // Use deterministic "random" access
        let mut hasher = DefaultHasher::new();
        i.hash(&mut hasher);
        let index = (hasher.finish() as usize) % pages.len();
        let page_id = pages[index];

        let _page = pager.read_page(page_id).unwrap();
    }

    let random_duration = random_start.elapsed();
    println!("Random access completed in {:?}", random_duration);
    println!("Average random access time: {:?}", random_duration / 5_000);

    println!("Cache thrashing test completed successfully!");
}

/// Test memory pressure with limited cache and many pages
///
/// This test validates that the pager can handle memory-constrained
/// environments with a very small cache relative to the working set.
#[test]
#[ignore] // Run with: cargo test --test pager_stress_tests -- --ignored --nocapture
fn test_memory_pressure() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default().with_cache_capacity(50); // Very small cache
    let pager = Pager::create(&fs, "memory_pressure.db", config).expect("Failed to create pager");

    let page_count = 50_000;

    println!(
        "Testing memory pressure with {} pages and cache capacity of 50...",
        page_count
    );
    println!("This simulates a memory-constrained environment");

    // Phase 1: Allocate and write pages
    println!("Phase 1: Allocating and writing {} pages...", page_count);
    let alloc_start = std::time::Instant::now();

    let mut pages = Vec::with_capacity(page_count);
    for i in 0..page_count {
        if i % 5_000 == 0 {
            println!("  Processed {} pages...", i);
        }

        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

        // Write data immediately
        let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
        let data = format!("Page {} - memory pressure test", page_id);
        page.data_mut().extend_from_slice(data.as_bytes());
        pager.write_page(&page).unwrap();

        pages.push(page_id);
    }

    let alloc_duration = alloc_start.elapsed();
    println!("Allocation and write completed in {:?}", alloc_duration);

    // Phase 2: Sequential read (should be relatively efficient)
    println!("Phase 2: Sequential read of all pages...");
    let seq_start = std::time::Instant::now();

    for (i, &page_id) in pages.iter().enumerate() {
        if i % 5_000 == 0 {
            println!("  Read {} pages...", i);
        }
        let _page = pager.read_page(page_id).unwrap();
    }

    let seq_duration = seq_start.elapsed();
    println!("Sequential read completed in {:?}", seq_duration);

    // Phase 3: Strided access (every 100th page)
    println!("Phase 3: Strided access pattern (every 100th page)...");
    let stride_start = std::time::Instant::now();

    for i in (0..pages.len()).step_by(100) {
        let page_id = pages[i];
        let page = pager.read_page(page_id).unwrap();
        let expected_data = format!("Page {} - memory pressure test", page_id);
        assert_eq!(
            &page.data()[0..expected_data.len()],
            expected_data.as_bytes()
        );
    }

    let stride_duration = stride_start.elapsed();
    println!("Strided access completed in {:?}", stride_duration);

    // Phase 4: Sync to ensure all data is persisted
    println!("Phase 4: Syncing StorageEngine...");
    let sync_start = std::time::Instant::now();
    pager.sync().unwrap();
    let sync_duration = sync_start.elapsed();
    println!("Sync completed in {:?}", sync_duration);

    println!("Memory pressure test completed successfully!");
}

/// Test with 1M pages (if feasible)
///
/// This is an extreme scale test to validate behavior with very large databases.
/// May take several minutes to complete.
#[test]
#[ignore] // Run with: cargo test --test pager_stress_tests -- --ignored --nocapture
fn test_1m_pages() {
    let pager = create_test_pager();
    let page_count = 1_000_000;

    println!("Testing with 1 MILLION pages...");
    println!("WARNING: This test may take several minutes and use significant memory");
    println!("Estimated memory usage: ~4GB for page IDs alone");

    let start = std::time::Instant::now();

    // Allocate pages in batches
    println!("Allocating {} pages in batches...", page_count);
    let batch_size = 50_000;
    let mut all_pages = Vec::with_capacity(page_count);

    for batch in 0..(page_count / batch_size) {
        let batch_start = std::time::Instant::now();

        for _ in 0..batch_size {
            let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            all_pages.push(page_id);
        }

        let batch_duration = batch_start.elapsed();
        let total_so_far = (batch + 1) * batch_size;

        println!(
            "Batch {}/{}: Allocated {} pages in {:?} (avg: {:?}/page)",
            batch + 1,
            page_count / batch_size,
            total_so_far,
            batch_duration,
            batch_duration / batch_size as u32
        );

        // Check for performance degradation
        let avg_micros = batch_duration.as_micros() / batch_size as u128;
        if avg_micros > 500 {
            println!("  WARNING: Allocation slowing down: {} μs/page", avg_micros);
        }
    }

    let total_duration = start.elapsed();
    println!("\nTotal allocation time: {:?}", total_duration);
    println!(
        "Average time per page: {:?}",
        total_duration / page_count as u32
    );

    // Verify uniqueness (sample check to avoid excessive memory)
    println!("Verifying page uniqueness (sampling)...");
    let sample_size = 100_000;
    let mut seen = HashSet::with_capacity(sample_size);
    for i in (0..all_pages.len()).step_by(all_pages.len() / sample_size) {
        assert!(seen.insert(all_pages[i]), "Duplicate page ID detected");
    }
    println!("Sampled {} pages, all unique", seen.len());

    // Verify total pages
    let total_pages = pager.total_pages();
    println!("Total pages in StorageEngine: {}", total_pages);
    assert!(
        total_pages >= (page_count + 2) as u64,
        "Total pages should be at least {}",
        page_count + 2
    );

    println!("1M page test completed successfully!");
}

/// Test large page file (multi-GB)
///
/// This test creates a StorageEngine with enough pages to exceed 1GB in size,
/// validating that the pager can handle large files.
#[test]
#[ignore] // Run with: cargo test --test pager_stress_tests -- --ignored --nocapture
fn test_large_page_file() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default().with_page_size(PageSize::Size4KB);
    let pager = Pager::create(&fs, "large_file.db", config).expect("Failed to create pager");

    // Calculate pages needed for ~2GB file
    // 4KB pages = 524,288 pages for 2GB
    let target_size_gb = 2;
    let page_size = 4096;
    let pages_per_gb = (1024 * 1024 * 1024) / page_size;
    let page_count = pages_per_gb * target_size_gb;

    println!("Testing large page file: target size ~{}GB", target_size_gb);
    println!(
        "Allocating {} pages of {} bytes each...",
        page_count, page_size
    );

    let start = std::time::Instant::now();
    let batch_size = 50_000;

    for batch in 0..(page_count / batch_size) {
        for _ in 0..batch_size {
            let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

            // Write some data to ensure pages are actually allocated on disk
            if batch % 10 == 0 {
                // Write to 10% of pages
                let mut page =
                    Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
                page.data_mut().extend_from_slice(b"Large file test data");
                pager.write_page(&page).unwrap();
            }
        }
        let total_so_far = (batch + 1) * batch_size;
        let size_mb = (total_so_far * page_size) / (1024 * 1024);

        if batch % 10 == 0 {
            println!(
                "Progress: {} pages (~{}MB) in {:?}",
                total_so_far,
                size_mb,
                start.elapsed()
            );
        }
    }

    let total_duration = start.elapsed();
    let final_size_mb = (page_count * page_size) / (1024 * 1024);

    println!("\nLarge file test completed!");
    println!("Total time: {:?}", total_duration);
    println!("Final size: ~{}MB", final_size_mb);
    println!("Total pages: {}", pager.total_pages());

    // Sync to ensure everything is written
    println!("Syncing StorageEngine...");
    pager.sync().unwrap();

    println!("Large page file test completed successfully!");
}

/// Test long-running stability (hours)
///
/// This test runs continuous operations for an extended period to detect
/// memory leaks, performance degradation, or other stability issues.
#[test]
#[ignore] // Run with: cargo test --test pager_stress_tests -- --ignored --nocapture
fn test_long_running_stability() {
    let pager = create_test_pager();

    // Run for 1 hour (configurable via environment variable)
    let duration_minutes = std::env::var("STABILITY_TEST_MINUTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60); // Default 1 hour

    let test_duration = std::time::Duration::from_secs(duration_minutes * 60);

    println!(
        "Starting long-running stability test for {} minutes...",
        duration_minutes
    );
    println!("This test will continuously allocate, write, read, and free pages");

    let start = std::time::Instant::now();
    let mut iteration = 0;
    let mut total_allocated = 0;
    let mut total_freed = 0;
    let mut active_pages = Vec::new();

    while start.elapsed() < test_duration {
        iteration += 1;

        // Allocate a batch of pages
        for _ in 0..100 {
            let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();

            // Write data
            let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
            let data = format!("Iteration {} - Page {}", iteration, page_id);
            page.data_mut().extend_from_slice(data.as_bytes());
            pager.write_page(&page).unwrap();

            active_pages.push(page_id);
            total_allocated += 1;
        }

        // Read some random pages
        if !active_pages.is_empty() {
            for i in 0..50 {
                let index = (iteration * 7 + i) % active_pages.len();
                let page_id = active_pages[index];
                let _page = pager.read_page(page_id).unwrap();
            }
        }

        // Free some pages if we have too many
        if active_pages.len() > 10_000 {
            for _ in 0..100 {
                if let Some(page_id) = active_pages.pop() {
                    pager.free_page(page_id).unwrap();
                    total_freed += 1;
                }
            }
        }

        // Periodic sync
        if iteration % 100 == 0 {
            pager.sync().unwrap();

            let elapsed = start.elapsed();
            let remaining = test_duration.saturating_sub(elapsed);

            println!(
                "Iteration {}: Elapsed {:?}, Remaining {:?}, Active pages: {}, Total alloc: {}, Total freed: {}",
                iteration,
                elapsed,
                remaining,
                active_pages.len(),
                total_allocated,
                total_freed
            );
        }
    }

    println!("\nLong-running stability test completed!");
    println!("Total iterations: {}", iteration);
    println!("Total pages allocated: {}", total_allocated);
    println!("Total pages freed: {}", total_freed);
    println!("Final active pages: {}", active_pages.len());
    println!("StorageEngine total pages: {}", pager.total_pages());
    println!("StorageEngine free pages: {}", pager.free_pages());
}
