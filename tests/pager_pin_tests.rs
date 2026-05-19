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

//! Tests for page pinning mechanism to prevent concurrent free/read corruption

use nanostore::pager::{Page, PageId, PageType, Pager, PagerConfig, PagerError};
use nanostore::vfs::MemoryFileSystem;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

/// Helper function to create a test pager
fn create_pager() -> Pager<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    Pager::create(&fs, "test.db", config).expect("Failed to create pager")
}

/// Test that the pinning mechanism prevents data corruption
///
/// This test verifies that pages cannot be freed while being read,
/// which prevents the corruption scenario described in the bug report.
#[test]
fn test_cannot_free_pinned_page() {
    let _pager = Arc::new(create_pager());

    // Create a pager with no cache to ensure reads always go to disk
    let fs = MemoryFileSystem::new();
    let mut config = PagerConfig::default();
    config.cache_capacity = 0; // Disable cache
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Allocate and write multiple pages
    let page_count = 10;
    let mut page_ids = Vec::new();
    for i in 0..page_count {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
        page.data_mut()
            .extend_from_slice(format!("Page {} data", i).as_bytes());
        pager.write_page(&page).unwrap();
        page_ids.push(page_id);
    }

    let barrier = Arc::new(Barrier::new(2));
    let page_ids = Arc::new(page_ids);

    // Reader thread - continuously reads pages
    let pager_clone = Arc::clone(&pager);
    let page_ids_clone = Arc::clone(&page_ids);
    let barrier_clone = Arc::clone(&barrier);
    let reader = thread::spawn(move || {
        barrier_clone.wait();

        for _ in 0..50 {
            for &page_id in page_ids_clone.iter() {
                let _page = pager_clone.read_page(page_id).unwrap();
            }
        }
    });

    // Freer thread - tries to free pages while they're being read
    let pager_clone = Arc::clone(&pager);
    let page_ids_clone = Arc::clone(&page_ids);
    let barrier_clone = Arc::clone(&barrier);
    let freer = thread::spawn(move || {
        barrier_clone.wait();

        let mut pinned_encountered = false;

        // Try to free pages - some attempts should fail with PagePinned
        for _ in 0..50 {
            for &page_id in page_ids_clone.iter() {
                match pager_clone.free_page(page_id) {
                    Ok(_) => {
                        // Successfully freed - page wasn't pinned at this moment
                    }
                    Err(PagerError::PagePinned(_)) => {
                        // Page is pinned - this is what we want to see
                        pinned_encountered = true;
                    }
                    Err(PagerError::PageAlreadyFree(_)) => {
                        // Already freed by a previous attempt
                    }
                    Err(e) => panic!("Unexpected error: {:?}", e),
                }
            }
            thread::sleep(Duration::from_micros(100));
        }

        pinned_encountered
    });

    reader.join().unwrap();
    let pinned_encountered = freer.join().unwrap();

    // We should have encountered at least one pinned page during concurrent operations
    assert!(
        pinned_encountered,
        "Should have encountered pinned pages during concurrent read/free operations"
    );
}

/// Test concurrent reads and frees with multiple threads
#[test]
fn test_concurrent_read_free_no_corruption() {
    let pager = Arc::new(create_pager());
    let thread_count = 8;
    let pages_per_thread = 10;

    // Pre-allocate pages
    let mut page_ids = Vec::new();
    for i in 0..thread_count * pages_per_thread {
        let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
        let data = format!("Page {} data", i);
        page.data_mut().extend_from_slice(data.as_bytes());
        pager.write_page(&page).unwrap();
        page_ids.push((page_id, data));
    }

    let page_ids = Arc::new(page_ids);
    let barrier = Arc::new(Barrier::new(thread_count * 2));
    let mut handles = vec![];

    // Reader threads
    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let page_ids_clone = Arc::clone(&page_ids);
        let barrier_clone = Arc::clone(&barrier);

        let handle = thread::spawn(move || {
            barrier_clone.wait();

            let start = thread_id * pages_per_thread;
            let end = start + pages_per_thread;

            for i in start..end {
                let (page_id, expected_data) = &page_ids_clone[i];

                // Read the page multiple times
                for _ in 0..5 {
                    let page = pager_clone.read_page(*page_id).unwrap();
                    assert_eq!(
                        &page.data()[0..expected_data.len()],
                        expected_data.as_bytes(),
                        "Data corruption detected for page {}",
                        page_id
                    );
                    thread::sleep(Duration::from_micros(10));
                }
            }
        });
        handles.push(handle);
    }

    // Freer threads (try to free pages while they're being read)
    for thread_id in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let page_ids_clone = Arc::clone(&page_ids);
        let barrier_clone = Arc::clone(&barrier);

        let handle = thread::spawn(move || {
            barrier_clone.wait();

            let start = thread_id * pages_per_thread;
            let end = start + pages_per_thread;

            for i in start..end {
                let (page_id, _) = &page_ids_clone[i];

                // Try to free the page - may fail if pinned
                for _ in 0..10 {
                    match pager_clone.free_page(*page_id) {
                        Ok(_) => break, // Successfully freed
                        Err(PagerError::PagePinned(_)) => {
                            // Page is pinned, wait and retry
                            thread::sleep(Duration::from_micros(50));
                        }
                        Err(e) => panic!("Unexpected error: {:?}", e),
                    }
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

/// Test that pages are properly unpinned after read
#[test]
fn test_pages_unpinned_after_read() {
    let pager = Arc::new(create_pager());

    // Allocate a page
    let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
    let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
    page.data_mut().extend_from_slice(b"test data");
    pager.write_page(&page).unwrap();

    // Read the page
    let _page = pager.read_page(page_id).unwrap();

    // Page should be unpinned after read completes
    // So we should be able to free it
    pager.free_page(page_id).unwrap();
}

/// Test that read errors still unpin the page
#[test]
fn test_unpin_on_read_error() {
    let pager = Arc::new(create_pager());

    // Try to read a non-existent page
    let result = pager.read_page(PageId::from(999));
    assert!(result.is_err());

    // Even though the read failed, the page should be unpinned
    // (This is more of a sanity check - we can't easily verify the pin state directly)
}

/// Test high contention scenario
#[test]
fn test_high_contention_pin_unpin() {
    let pager = Arc::new(create_pager());
    let thread_count = 16;
    let operations_per_thread = 50;

    // Allocate a single page that all threads will contend for
    let page_id = pager.allocate_page(PageType::BTreeLeaf).unwrap();
    let mut page = Page::new(page_id, PageType::BTreeLeaf, pager.page_size().data_size());
    page.data_mut().extend_from_slice(b"shared page data");
    pager.write_page(&page).unwrap();

    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for _ in 0..thread_count {
        let pager_clone = Arc::clone(&pager);
        let barrier_clone = Arc::clone(&barrier);

        let handle = thread::spawn(move || {
            barrier_clone.wait();

            for _ in 0..operations_per_thread {
                // Read the page
                let _page = pager_clone.read_page(page_id).unwrap();

                // Small delay to increase contention
                thread::sleep(Duration::from_micros(1));
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // After all reads complete, page should be unpinned and can be freed
    pager.free_page(page_id).unwrap();
}
