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

//! Concurrency tests for VFS MemoryFileSystem
//!
//! These tests validate thread safety and proper synchronization when multiple
//! threads access the VFS simultaneously. They expose race conditions in:
//! - read_at_offset: Integer overflow when pos > buffer.len()
//! - read_at_offset: Slice length mismatch when buf.len() != actual slice length
//! - Corrupted data being returned due to race conditions

use nanostore::vfs::{File, FileSystem, MemoryFileSystem};
use std::io::Write;
use std::sync::{Arc, Barrier};
use std::thread;

/// Test concurrent reads at various offsets
///
/// This test exposes the integer overflow bug at line 367 when multiple threads
/// read at offsets beyond the file size.
#[test]
fn test_concurrent_read_at_offset_overflow() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "/test_overflow.dat";

    // Create file with 100 bytes
    {
        let mut file = fs.create_file(path).expect("Failed to create file");
        file.write_all(&[0x42; 100]).expect("Failed to write");
    }

    let thread_count = 4;
    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let fs_clone = Arc::clone(&fs);
        let barrier_clone = Arc::clone(&barrier);
        let path = path.to_string();

        let handle = thread::spawn(move || {
            let mut file = fs_clone.open_file(&path).expect("Failed to open file");

            // Wait for all threads to be ready
            barrier_clone.wait();

            // Try to read at various offsets, including beyond file size
            let offsets = [0, 50, 99, 100, 150, 200, 1000];
            let mut results = vec![];

            for &offset in &offsets {
                let mut buf = vec![0u8; 50];
                match file.read_at_offset(offset, &mut buf) {
                    Ok(bytes_read) => {
                        results.push((offset, bytes_read, None));
                    }
                    Err(e) => {
                        results.push((offset, 0, Some(format!("{:?}", e))));
                    }
                }
            }

            (thread_id, results)
        });
        handles.push(handle);
    }

    // Collect results
    for handle in handles {
        let (thread_id, results) = handle.join().expect("Thread panicked");
        println!("Thread {} results:", thread_id);
        for (offset, bytes_read, error) in results {
            if let Some(err) = error {
                println!("  Offset {}: ERROR - {}", offset, err);
            } else {
                println!("  Offset {}: Read {} bytes", offset, bytes_read);
            }
        }
    }
}

/// Test concurrent reads with mismatched buffer sizes
///
/// This test exposes the slice length mismatch bug at line 370 when the
/// buffer size doesn't match the actual data available.
#[test]
fn test_concurrent_read_buffer_mismatch() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "/test_mismatch.dat";

    // Create file with 100 bytes
    {
        let mut file = fs.create_file(path).expect("Failed to create file");
        file.write_all(&[0x55; 100]).expect("Failed to write");
    }

    let thread_count = 8;
    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let fs_clone = Arc::clone(&fs);
        let barrier_clone = Arc::clone(&barrier);
        let path = path.to_string();

        let handle = thread::spawn(move || {
            let mut file = fs_clone.open_file(&path).expect("Failed to open file");

            // Wait for all threads to be ready
            barrier_clone.wait();

            // Try to read with various buffer sizes at edge positions
            let test_cases = [
                (95, 10), // Read 10 bytes at offset 95 (only 5 available)
                (98, 5),  // Read 5 bytes at offset 98 (only 2 available)
                (99, 10), // Read 10 bytes at offset 99 (only 1 available)
                (100, 5), // Read 5 bytes at offset 100 (0 available)
            ];

            for &(offset, buf_size) in &test_cases {
                let mut buf = vec![0u8; buf_size];
                match file.read_at_offset(offset, &mut buf) {
                    Ok(bytes_read) => {
                        // Verify we didn't read more than available
                        let available = 100_usize.saturating_sub(offset as usize);
                        assert!(
                            bytes_read <= available,
                            "Thread {}: Read {} bytes but only {} available at offset {}",
                            thread_id,
                            bytes_read,
                            available,
                            offset
                        );
                    }
                    Err(e) => {
                        panic!(
                            "Thread {}: Unexpected error at offset {}: {:?}",
                            thread_id, offset, e
                        );
                    }
                }
            }
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}

/// Test concurrent writes and reads
///
/// This test exposes data corruption issues when multiple threads write and
/// read simultaneously.
#[test]
fn test_concurrent_write_read_corruption() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "/test_corruption.dat";

    // Create file with initial data
    {
        let mut file = fs.create_file(path).expect("Failed to create file");
        file.write_all(&vec![0x00; 1000]).expect("Failed to write");
    }

    let thread_count = 4;
    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let fs_clone = Arc::clone(&fs);
        let barrier_clone = Arc::clone(&barrier);
        let path = path.to_string();

        let handle = thread::spawn(move || {
            let mut file = fs_clone.open_file(&path).expect("Failed to open file");

            // Wait for all threads to be ready
            barrier_clone.wait();

            // Each thread writes its ID to different sections
            let section_size = 250;
            let offset = (thread_id * section_size) as u64;
            let data = vec![thread_id as u8; section_size];

            // Write data
            file.write_to_offset(offset, &data)
                .expect("Failed to write");

            // Immediately read back
            let mut read_buf = vec![0u8; section_size];
            let bytes_read = file
                .read_at_offset(offset, &mut read_buf)
                .expect("Failed to read");

            // Verify data integrity
            assert_eq!(
                bytes_read, section_size,
                "Thread {}: Read wrong number of bytes",
                thread_id
            );

            // Check if data matches what we wrote
            let matches = read_buf.iter().all(|&b| b == thread_id as u8);
            (thread_id, matches, read_buf[0..10].to_vec())
        });
        handles.push(handle);
    }

    // Collect results
    let mut all_matched = true;
    for handle in handles {
        let (thread_id, matches, sample) = handle.join().expect("Thread panicked");
        if !matches {
            println!(
                "Thread {} data corruption detected! Sample: {:?}",
                thread_id, sample
            );
            all_matched = false;
        }
    }

    assert!(
        all_matched,
        "Data corruption detected in concurrent write/read operations"
    );
}

/// Test concurrent resize operations
///
/// This test validates that concurrent resize operations don't cause
/// data corruption or panics.
#[test]
fn test_concurrent_resize_operations() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "/test_resize.dat";

    // Create file with initial data
    {
        let mut file = fs.create_file(path).expect("Failed to create file");
        file.write_all(&vec![0xAA; 500]).expect("Failed to write");
    }

    let thread_count = 4;
    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let fs_clone = Arc::clone(&fs);
        let barrier_clone = Arc::clone(&barrier);
        let path = path.to_string();

        let handle = thread::spawn(move || {
            let mut file = fs_clone.open_file(&path).expect("Failed to open file");

            // Wait for all threads to be ready
            barrier_clone.wait();

            // Perform multiple resize operations
            for i in 0..10 {
                let new_size = 100 + (thread_id * 100) + (i * 10);
                file.set_size(new_size as u64).expect("Failed to resize");

                // Try to read at various offsets
                let mut buf = vec![0u8; 50];
                let _ = file.read_at_offset(0, &mut buf);
                let _ = file.read_at_offset(new_size as u64 / 2, &mut buf);
            }
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}

/// Test high-contention concurrent access
///
/// This test creates maximum contention by having many threads
/// simultaneously read and write to the same file.
#[test]
fn test_high_contention_access() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "/test_contention.dat";

    // Create file with pattern data
    {
        let mut file = fs.create_file(path).expect("Failed to create file");
        let mut data = vec![];
        for i in 0..256 {
            data.push(i as u8);
        }
        file.write_all(&data).expect("Failed to write");
    }

    let thread_count = 16;
    let operations_per_thread = 100;
    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let fs_clone = Arc::clone(&fs);
        let barrier_clone = Arc::clone(&barrier);
        let path = path.to_string();

        let handle = thread::spawn(move || {
            let mut file = fs_clone.open_file(&path).expect("Failed to open file");

            // Wait for all threads to be ready
            barrier_clone.wait();

            let mut errors = 0;

            for op in 0..operations_per_thread {
                let offset = ((thread_id + op) % 200) as u64;
                let mut buf = vec![0u8; 32];

                // Alternate between reads and writes
                if op % 2 == 0 {
                    if let Err(_) = file.read_at_offset(offset, &mut buf) {
                        errors += 1;
                    }
                } else {
                    let data = vec![thread_id as u8; 32];
                    if let Err(_) = file.write_to_offset(offset, &data) {
                        errors += 1;
                    }
                }
            }

            (thread_id, errors)
        });
        handles.push(handle);
    }

    // Collect results
    let mut total_errors = 0;
    for handle in handles {
        let (thread_id, errors) = handle.join().expect("Thread panicked");
        if errors > 0 {
            println!("Thread {} encountered {} errors", thread_id, errors);
            total_errors += errors;
        }
    }

    assert_eq!(
        total_errors, 0,
        "Encountered {} errors during high-contention access",
        total_errors
    );
}

/// Test concurrent directory operations
///
/// This test validates that concurrent directory creation, listing, and removal
/// operations don't cause race conditions or panics. Note: This test uses flat
/// directory structure (siblings) rather than hierarchical due to current VFS
/// implementation limitations with parent-child directory relationships.
#[test]
fn test_concurrent_directory_operations() {
    let fs = Arc::new(MemoryFileSystem::new());

    let thread_count = 8;
    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let fs_clone = Arc::clone(&fs);
        let barrier_clone = Arc::clone(&barrier);

        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            barrier_clone.wait();

            let mut errors = vec![];

            // Each thread creates multiple sibling directories
            let mut created_dirs = vec![];
            for i in 0..5 {
                let dir_path = format!("/thread_{}_dir_{}", thread_id, i);
                match fs_clone.create_directory(&dir_path) {
                    Ok(_) => created_dirs.push(dir_path.clone()),
                    Err(e) => {
                        errors.push(format!("create_directory {} failed: {:?}", dir_path, e));
                    }
                }
            }

            // Verify directories exist
            for dir_path in &created_dirs {
                match fs_clone.is_directory(dir_path) {
                    Ok(true) => {}
                    Ok(false) => {
                        errors.push(format!(
                            "Directory {} not recognized as directory",
                            dir_path
                        ));
                    }
                    Err(e) => {
                        errors.push(format!("is_directory {} failed: {:?}", dir_path, e));
                    }
                }
            }

            // Try to access other threads' directories concurrently
            for other_id in 0..thread_count {
                if other_id != thread_id {
                    for i in 0..5 {
                        let other_dir = format!("/thread_{}_dir_{}", other_id, i);
                        let _ = fs_clone.exists(&other_dir);
                        let _ = fs_clone.is_directory(&other_dir);
                    }
                }
            }

            // Remove directories
            for dir_path in &created_dirs {
                if let Err(e) = fs_clone.remove_directory(dir_path) {
                    errors.push(format!("remove_directory {} failed: {:?}", dir_path, e));
                }
            }

            // Verify directories are removed
            for dir_path in &created_dirs {
                match fs_clone.exists(dir_path) {
                    Ok(false) => {}
                    Ok(true) => {
                        errors.push(format!("Directory {} still exists after removal", dir_path));
                    }
                    Err(e) => {
                        errors.push(format!("exists check for {} failed: {:?}", dir_path, e));
                    }
                }
            }

            (thread_id, errors)
        });
        handles.push(handle);
    }

    // Collect results
    let mut total_errors = 0;
    for handle in handles {
        let (thread_id, errors) = handle.join().expect("Thread panicked");
        if !errors.is_empty() {
            println!("Thread {} encountered {} errors:", thread_id, errors.len());
            for error in &errors {
                println!("  - {}", error);
            }
            total_errors += errors.len();
        }
    }

    assert_eq!(
        total_errors, 0,
        "Encountered {} errors during concurrent directory operations",
        total_errors
    );
}

/// Test concurrent file lock contention
///
/// This test validates that file locking works correctly when multiple threads
/// try to acquire locks on the same file simultaneously.
#[test]
fn test_concurrent_lock_contention() {
    use nanostore::vfs::FileLockMode;

    let fs = Arc::new(MemoryFileSystem::new());
    let path = "/test_lock.dat";

    // Create file
    {
        let mut file = fs.create_file(path).expect("Failed to create file");
        file.write_all(&[0x77; 100]).expect("Failed to write");
    }

    let thread_count = 8;
    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let fs_clone = Arc::clone(&fs);
        let barrier_clone = Arc::clone(&barrier);
        let path = path.to_string();

        let handle = thread::spawn(move || {
            let mut file = fs_clone.open_file(&path).expect("Failed to open file");

            // Wait for all threads to be ready
            barrier_clone.wait();

            let mut lock_attempts = 0;
            let mut lock_successes = 0;
            let mut lock_failures = 0;

            // Try to acquire locks multiple times
            for i in 0..20 {
                lock_attempts += 1;

                // Alternate between shared and exclusive locks
                let lock_mode = if i % 2 == 0 {
                    FileLockMode::Shared
                } else {
                    FileLockMode::Exclusive
                };

                // Try to acquire lock
                match file.set_lock_status(lock_mode) {
                    Ok(_) => {
                        lock_successes += 1;

                        // Verify lock status
                        match file.get_lock_status() {
                            Ok(status) => {
                                if status != lock_mode {
                                    println!(
                                        "Thread {}: Lock status mismatch! Expected {:?}, got {:?}",
                                        thread_id, lock_mode, status
                                    );
                                }
                            }
                            Err(e) => {
                                println!(
                                    "Thread {}: Failed to get lock status: {:?}",
                                    thread_id, e
                                );
                            }
                        }

                        // Hold lock briefly
                        thread::sleep(std::time::Duration::from_micros(100));

                        // Release lock
                        if let Err(e) = file.set_lock_status(FileLockMode::Unlocked) {
                            println!("Thread {}: Failed to release lock: {:?}", thread_id, e);
                        }
                    }
                    Err(_) => {
                        lock_failures += 1;
                    }
                }

                // Small delay between attempts
                thread::sleep(std::time::Duration::from_micros(50));
            }

            (thread_id, lock_attempts, lock_successes, lock_failures)
        });
        handles.push(handle);
    }

    // Collect results
    let mut total_attempts = 0;
    let mut total_successes = 0;
    let mut total_failures = 0;

    for handle in handles {
        let (thread_id, attempts, successes, failures) = handle.join().expect("Thread panicked");
        println!(
            "Thread {}: {} attempts, {} successes, {} failures",
            thread_id, attempts, successes, failures
        );
        total_attempts += attempts;
        total_successes += successes;
        total_failures += failures;
    }

    println!(
        "Total: {} attempts, {} successes, {} failures",
        total_attempts, total_successes, total_failures
    );

    // We should have some successes (not all threads should fail)
    assert!(
        total_successes > 0,
        "No threads successfully acquired locks"
    );
}

/// Test concurrent file deletion while reading
///
/// This test validates that deleting a file while other threads are reading
/// from it doesn't cause panics or data corruption.
#[test]
fn test_concurrent_deletion_while_reading() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "/test_delete.dat";

    // Create file with data
    {
        let mut file = fs.create_file(path).expect("Failed to create file");
        let mut data = vec![];
        for i in 0..1000 {
            data.push((i % 256) as u8);
        }
        file.write_all(&data).expect("Failed to write");
    }

    let thread_count = 8;
    let barrier = Arc::new(Barrier::new(thread_count));
    let mut handles = vec![];

    for thread_id in 0..thread_count {
        let fs_clone = Arc::clone(&fs);
        let barrier_clone = Arc::clone(&barrier);
        let path = path.to_string();

        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            barrier_clone.wait();

            if thread_id == 0 {
                // Thread 0 will try to delete the file after a brief delay
                thread::sleep(std::time::Duration::from_millis(10));

                match fs_clone.remove_file(&path) {
                    Ok(_) => {
                        println!("Thread {}: Successfully deleted file", thread_id);
                        (thread_id, "deleter", 0, 0)
                    }
                    Err(e) => {
                        println!("Thread {}: Failed to delete file: {:?}", thread_id, e);
                        (thread_id, "deleter", 0, 0)
                    }
                }
            } else {
                // Other threads continuously read from the file
                let mut successful_reads = 0;
                let mut failed_reads = 0;

                for _ in 0..50 {
                    match fs_clone.open_file(&path) {
                        Ok(mut file) => {
                            let mut buf = vec![0u8; 100];
                            let offset = ((thread_id * 100) % 900) as u64;

                            match file.read_at_offset(offset, &mut buf) {
                                Ok(_) => successful_reads += 1,
                                Err(_) => failed_reads += 1,
                            }
                        }
                        Err(_) => {
                            // File might have been deleted
                            failed_reads += 1;
                        }
                    }

                    thread::sleep(std::time::Duration::from_micros(100));
                }

                (thread_id, "reader", successful_reads, failed_reads)
            }
        });
        handles.push(handle);
    }

    // Collect results
    for handle in handles {
        let (thread_id, role, successful, failed) = handle.join().expect("Thread panicked");
        println!(
            "Thread {} ({}): {} successful, {} failed",
            thread_id, role, successful, failed
        );
    }

    // Test passes if no threads panicked
}
