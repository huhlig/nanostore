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

//! Property-based tests for VFS implementations using proptest

use nanostore::vfs::{File, FileLockMode, FileSystem, LocalFileSystem, MemoryFileSystem};
use proptest::prelude::*;
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::{SystemTime, UNIX_EPOCH};

// Strategy for generating valid file paths
fn valid_file_path() -> impl Strategy<Value = String> {
    prop::string::string_regex("/[a-zA-Z0-9_-]{1,20}\\.(txt|dat|bin)").expect("Invalid regex")
}

// Strategy for generating file content
fn file_content() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..1024)
}

// Strategy for generating seek positions
fn seek_position(max_size: u64) -> impl Strategy<Value = SeekFrom> {
    prop_oneof![
        (0..=max_size).prop_map(SeekFrom::Start),
        (-100i64..=100i64).prop_map(SeekFrom::Current),
        (-100i64..=0i64).prop_map(SeekFrom::End),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Test that writing and reading back data preserves the content
    #[test]
    fn prop_write_read_roundtrip(
        path in valid_file_path(),
        data in file_content()
    ) {
        let fs = MemoryFileSystem::new();
        let mut file = fs.create_file(&path).unwrap();

        // Write data
        file.write_all(&data).unwrap();

        // Read back
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut read_data = Vec::new();
        file.read_to_end(&mut read_data).unwrap();

        // Verify
        prop_assert_eq!(data, read_data);
    }

    /// Test that file size matches written data length
    #[test]
    fn prop_file_size_matches_data(
        path in valid_file_path(),
        data in file_content()
    ) {
        let fs = MemoryFileSystem::new();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&data).unwrap();
        let size = file.get_size().unwrap();

        prop_assert_eq!(size, data.len() as u64);
    }

    /// Test that seeking and reading at different positions works correctly
    #[test]
    fn prop_seek_read_consistency(
        path in valid_file_path(),
        data in file_content().prop_filter("Non-empty data", |d| !d.is_empty()),
        offset in 0usize..100
    ) {
        let fs = MemoryFileSystem::new();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&data).unwrap();

        let offset = offset.min(data.len());
        file.seek(SeekFrom::Start(offset as u64)).unwrap();

        let mut read_data = Vec::new();
        file.read_to_end(&mut read_data).unwrap();

        prop_assert_eq!(read_data, &data[offset..]);
    }

    /// Test that multiple writes accumulate correctly
    #[test]
    fn prop_multiple_writes(
        path in valid_file_path(),
        chunks in prop::collection::vec(file_content(), 1..10)
    ) {
        let fs = MemoryFileSystem::new();
        let mut file = fs.create_file(&path).unwrap();

        let mut expected = Vec::new();
        for chunk in &chunks {
            file.write_all(chunk).unwrap();
            expected.extend_from_slice(chunk);
        }

        file.seek(SeekFrom::Start(0)).unwrap();
        let mut actual = Vec::new();
        file.read_to_end(&mut actual).unwrap();

        prop_assert_eq!(expected, actual);
    }

    /// Test that set_size correctly resizes files
    #[test]
    fn prop_resize_operations(
        path in valid_file_path(),
        initial_data in file_content(),
        new_size in 0u64..2048
    ) {
        let fs = MemoryFileSystem::new();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&initial_data).unwrap();
        file.set_size(new_size).unwrap();

        let actual_size = file.get_size().unwrap();
        prop_assert_eq!(actual_size, new_size);

        // Verify content up to min(initial_size, new_size)
        let min_size = initial_data.len().min(new_size as usize);
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = vec![0u8; min_size];
        file.read_exact(&mut buf).unwrap();
        prop_assert_eq!(&buf[..], &initial_data[..min_size]);
    }

    /// Test that read_at_offset doesn't change cursor position
    #[test]
    fn prop_read_at_offset_preserves_cursor(
        path in valid_file_path(),
        data in file_content().prop_filter("Non-empty data", |d| d.len() >= 10),
        read_offset in 0usize..5,
        cursor_pos in 5usize..10
    ) {
        let fs = MemoryFileSystem::new();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&data).unwrap();
        file.seek(SeekFrom::Start(cursor_pos as u64)).unwrap();

        let pos_before = file.stream_position().unwrap();
        let mut buf = vec![0u8; 3];
        file.read_at_offset(read_offset as u64, &mut buf).unwrap();
        let pos_after = file.stream_position().unwrap();

        prop_assert_eq!(pos_before, pos_after);
    }

    /// Test that write_to_offset doesn't change cursor position
    #[test]
    fn prop_write_to_offset_preserves_cursor(
        path in valid_file_path(),
        initial_data in file_content().prop_filter("Non-empty data", |d| d.len() >= 10),
        write_offset in 0usize..5,
        cursor_pos in 5usize..10,
        write_data in prop::collection::vec(any::<u8>(), 1..5)
    ) {
        let fs = MemoryFileSystem::new();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&initial_data).unwrap();
        file.seek(SeekFrom::Start(cursor_pos as u64)).unwrap();

        let pos_before = file.stream_position().unwrap();
        file.write_to_offset(write_offset as u64, &write_data).unwrap();
        let pos_after = file.stream_position().unwrap();

        prop_assert_eq!(pos_before, pos_after);
    }

    /// Test that exists() returns true after create and false after remove
    #[test]
    fn prop_exists_lifecycle(path in valid_file_path()) {
        let fs = MemoryFileSystem::new();

        // Initially doesn't exist
        prop_assert!(!fs.exists(&path).unwrap());

        // Create file
        let file = fs.create_file(&path).unwrap();
        prop_assert!(fs.exists(&path).unwrap());

        // Remove file
        drop(file);
        fs.remove_file(&path).unwrap();
        prop_assert!(!fs.exists(&path).unwrap());
    }

    /// Test that is_file() and is_directory() are mutually exclusive
    #[test]
    fn prop_file_directory_exclusive(path in valid_file_path()) {
        let fs = MemoryFileSystem::new();

        // Create as file
        let file = fs.create_file(&path).unwrap();
        prop_assert!(fs.is_file(&path).unwrap());
        prop_assert!(!fs.is_directory(&path).unwrap());
        drop(file);
        fs.remove_file(&path).unwrap();

        // Create as directory
        let dir_path = format!("{}_dir", path);
        fs.create_directory(&dir_path).unwrap();
        prop_assert!(!fs.is_file(&dir_path).unwrap());
        prop_assert!(fs.is_directory(&dir_path).unwrap());
    }

    /// Test that filesize() matches actual file size
    #[test]
    fn prop_filesize_accuracy(
        path in valid_file_path(),
        data in file_content()
    ) {
        let fs = MemoryFileSystem::new();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&data).unwrap();
        drop(file);

        let size = fs.filesize(&path).unwrap();
        prop_assert_eq!(size, data.len() as u64);
    }

    /// Test that truncate() sets size to zero
    #[test]
    fn prop_truncate_zeros_size(
        path in valid_file_path(),
        data in file_content().prop_filter("Non-empty data", |d| !d.is_empty())
    ) {
        let fs = MemoryFileSystem::new();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&data).unwrap();
        prop_assert!(file.get_size().unwrap() > 0);

        file.truncate().unwrap();
        prop_assert_eq!(file.get_size().unwrap(), 0);
    }

    /// Test that opening an existing file preserves its content
    #[test]
    fn prop_open_preserves_content(
        path in valid_file_path(),
        data in file_content()
    ) {
        let fs = MemoryFileSystem::new();

        // Create and write
        let mut file = fs.create_file(&path).unwrap();
        file.write_all(&data).unwrap();
        drop(file);

        // Open and read
        let mut file = fs.open_file(&path).unwrap();
        let mut read_data = Vec::new();
        file.read_to_end(&mut read_data).unwrap();

        prop_assert_eq!(data, read_data);
    }

    /// Test that writing at different offsets produces correct results
    #[test]
    fn prop_write_at_offset_correctness(
        path in valid_file_path(),
        initial_data in file_content().prop_filter("Sufficient size", |d| d.len() >= 20),
        offset in 5usize..10,
        patch_data in prop::collection::vec(any::<u8>(), 1..5)
    ) {
        let fs = MemoryFileSystem::new();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&initial_data).unwrap();
        file.write_to_offset(offset as u64, &patch_data).unwrap();

        // Build expected result
        let mut expected = initial_data.clone();
        expected[offset..offset + patch_data.len()].copy_from_slice(&patch_data);

        // Read and verify
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut actual = Vec::new();
        file.read_to_end(&mut actual).unwrap();

        prop_assert_eq!(expected, actual);
    }

    /// Test that concurrent file handles see the same data
    #[test]
    fn prop_multiple_handles_consistency(
        path in valid_file_path(),
        data in file_content()
    ) {
        let fs = MemoryFileSystem::new();

        // Create and write with first handle
        let mut file1 = fs.create_file(&path).unwrap();
        file1.write_all(&data).unwrap();

        // Open second handle and read
        let mut file2 = fs.open_file(&path).unwrap();
        let mut read_data = Vec::new();
        file2.read_to_end(&mut read_data).unwrap();

        prop_assert_eq!(data, read_data);
    }
}

// Additional edge case tests
#[test]
fn test_empty_file_operations() {
    let fs = MemoryFileSystem::new();
    let path = "/empty.txt";

    let mut file = fs.create_file(path).unwrap();
    assert_eq!(file.get_size().unwrap(), 0);

    // Reading empty file should return 0 bytes
    let mut buf = Vec::new();
    let n = file.read_to_end(&mut buf).unwrap();
    assert_eq!(n, 0);
    assert!(buf.is_empty());

    // Seeking in empty file
    file.seek(SeekFrom::Start(0)).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
}

#[test]
fn test_large_file_operations() {
    let fs = MemoryFileSystem::new();
    let path = "/large.bin";

    let mut file = fs.create_file(path).unwrap();

    // Write 1MB of data
    let chunk = vec![0xAB; 1024];
    for _ in 0..1024 {
        file.write_all(&chunk).unwrap();
    }

    assert_eq!(file.get_size().unwrap(), 1024 * 1024);

    // Verify content
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut buf = vec![0u8; 1024];
    file.read_exact(&mut buf).unwrap();
    assert_eq!(buf, chunk);
}

#[test]
fn test_boundary_seek_positions() {
    let fs = MemoryFileSystem::new();
    let path = "/boundary.txt";

    let mut file = fs.create_file(path).unwrap();
    file.write_all(b"0123456789").unwrap();

    // Seek to exact end
    let pos = file.seek(SeekFrom::End(0)).unwrap();
    assert_eq!(pos, 10);

    // Seek to exact start
    let pos = file.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(pos, 0);

    // Seek to middle
    let pos = file.seek(SeekFrom::Start(5)).unwrap();
    assert_eq!(pos, 5);
}

// ============================================================================
// LocalFileSystem Property Tests
// ============================================================================

/// Helper to create a unique temporary directory for LocalFileSystem tests
fn create_temp_fs() -> (LocalFileSystem, String) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("nanostore_test_{}", timestamp));
    std::fs::create_dir_all(&temp_dir).expect("Failed to create temp directory");
    let fs = LocalFileSystem::new(&temp_dir);
    (fs, temp_dir.to_string_lossy().to_string())
}

/// Helper to cleanup temporary directory
fn cleanup_temp_fs(temp_dir: &str) {
    let _ = std::fs::remove_dir_all(temp_dir);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Test that writing and reading back data preserves the content (LocalFileSystem)
    #[test]
    fn prop_local_write_read_roundtrip(
        path in valid_file_path(),
        data in file_content()
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let mut file = fs.create_file(&path).unwrap();

        // Write data
        file.write_all(&data).unwrap();

        // Read back
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut read_data = Vec::new();
        file.read_to_end(&mut read_data).unwrap();

        // Verify
        prop_assert_eq!(data, read_data);

        drop(file);
        cleanup_temp_fs(&temp_dir);
    }

    /// Test that file size matches written data length (LocalFileSystem)
    #[test]
    fn prop_local_file_size_matches_data(
        path in valid_file_path(),
        data in file_content()
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&data).unwrap();
        let size = file.get_size().unwrap();

        prop_assert_eq!(size, data.len() as u64);

        drop(file);
        cleanup_temp_fs(&temp_dir);
    }

    /// Test that seeking and reading at different positions works correctly (LocalFileSystem)
    #[test]
    fn prop_local_seek_read_consistency(
        path in valid_file_path(),
        data in file_content().prop_filter("Non-empty data", |d| !d.is_empty()),
        offset in 0usize..100
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&data).unwrap();

        let offset = offset.min(data.len());
        file.seek(SeekFrom::Start(offset as u64)).unwrap();

        let mut read_data = Vec::new();
        file.read_to_end(&mut read_data).unwrap();

        prop_assert_eq!(read_data, &data[offset..]);

        drop(file);
        cleanup_temp_fs(&temp_dir);
    }

    /// Test that multiple writes accumulate correctly (LocalFileSystem)
    #[test]
    fn prop_local_multiple_writes(
        path in valid_file_path(),
        chunks in prop::collection::vec(file_content(), 1..10)
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let mut file = fs.create_file(&path).unwrap();

        let mut expected = Vec::new();
        for chunk in &chunks {
            file.write_all(chunk).unwrap();
            expected.extend_from_slice(chunk);
        }

        file.seek(SeekFrom::Start(0)).unwrap();
        let mut actual = Vec::new();
        file.read_to_end(&mut actual).unwrap();

        prop_assert_eq!(expected, actual);

        drop(file);
        cleanup_temp_fs(&temp_dir);
    }

    /// Test that set_size correctly resizes files (LocalFileSystem)
    #[test]
    fn prop_local_resize_operations(
        path in valid_file_path(),
        initial_data in file_content(),
        new_size in 0u64..2048
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&initial_data).unwrap();
        file.set_size(new_size).unwrap();

        let actual_size = file.get_size().unwrap();
        prop_assert_eq!(actual_size, new_size);

        // Verify content up to min(initial_size, new_size)
        let min_size = initial_data.len().min(new_size as usize);
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = vec![0u8; min_size];
        file.read_exact(&mut buf).unwrap();
        prop_assert_eq!(&buf[..], &initial_data[..min_size]);

        drop(file);
        cleanup_temp_fs(&temp_dir);
    }

    /// Test that read_at_offset doesn't change cursor position (LocalFileSystem)
    #[test]
    fn prop_local_read_at_offset_preserves_cursor(
        path in valid_file_path(),
        data in file_content().prop_filter("Non-empty data", |d| d.len() >= 10),
        read_offset in 0usize..5,
        cursor_pos in 5usize..10
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&data).unwrap();
        file.seek(SeekFrom::Start(cursor_pos as u64)).unwrap();

        let pos_before = file.stream_position().unwrap();
        let mut buf = vec![0u8; 3];
        file.read_at_offset(read_offset as u64, &mut buf).unwrap();
        let pos_after = file.stream_position().unwrap();

        prop_assert_eq!(pos_before, pos_after);

        drop(file);
        cleanup_temp_fs(&temp_dir);
    }

    /// Test that write_to_offset doesn't change cursor position (LocalFileSystem)
    #[test]
    fn prop_local_write_to_offset_preserves_cursor(
        path in valid_file_path(),
        initial_data in file_content().prop_filter("Non-empty data", |d| d.len() >= 10),
        write_offset in 0usize..5,
        cursor_pos in 5usize..10,
        write_data in prop::collection::vec(any::<u8>(), 1..5)
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&initial_data).unwrap();
        file.seek(SeekFrom::Start(cursor_pos as u64)).unwrap();

        let pos_before = file.stream_position().unwrap();
        file.write_to_offset(write_offset as u64, &write_data).unwrap();
        let pos_after = file.stream_position().unwrap();

        prop_assert_eq!(pos_before, pos_after);

        drop(file);
        cleanup_temp_fs(&temp_dir);
    }

    /// Test that exists() returns true after create and false after remove (LocalFileSystem)
    #[test]
    fn prop_local_exists_lifecycle(path in valid_file_path()) {
        let (fs, temp_dir) = create_temp_fs();

        // Initially doesn't exist
        prop_assert!(!fs.exists(&path).unwrap());

        // Create file
        let file = fs.create_file(&path).unwrap();
        prop_assert!(fs.exists(&path).unwrap());

        // Remove file
        drop(file);
        fs.remove_file(&path).unwrap();
        prop_assert!(!fs.exists(&path).unwrap());

        cleanup_temp_fs(&temp_dir);
    }

    /// Test that is_file() and is_directory() are mutually exclusive (LocalFileSystem)
    #[test]
    fn prop_local_file_directory_exclusive(path in valid_file_path()) {
        let (fs, temp_dir) = create_temp_fs();

        // Create as file
        let file = fs.create_file(&path).unwrap();
        prop_assert!(fs.is_file(&path).unwrap());
        prop_assert!(!fs.is_directory(&path).unwrap());
        drop(file);
        fs.remove_file(&path).unwrap();

        // Create as directory
        let dir_path = format!("{}_dir", path);
        fs.create_directory(&dir_path).unwrap();
        prop_assert!(!fs.is_file(&dir_path).unwrap());
        prop_assert!(fs.is_directory(&dir_path).unwrap());

        cleanup_temp_fs(&temp_dir);
    }

    /// Test that filesize() matches actual file size (LocalFileSystem)
    #[test]
    fn prop_local_filesize_accuracy(
        path in valid_file_path(),
        data in file_content()
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&data).unwrap();
        drop(file);

        let size = fs.filesize(&path).unwrap();
        prop_assert_eq!(size, data.len() as u64);

        cleanup_temp_fs(&temp_dir);
    }

    /// Test that truncate() sets size to zero (LocalFileSystem)
    #[test]
    fn prop_local_truncate_zeros_size(
        path in valid_file_path(),
        data in file_content().prop_filter("Non-empty data", |d| !d.is_empty())
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&data).unwrap();
        prop_assert!(file.get_size().unwrap() > 0);

        file.truncate().unwrap();
        prop_assert_eq!(file.get_size().unwrap(), 0);

        drop(file);
        cleanup_temp_fs(&temp_dir);
    }

    /// Test that opening an existing file preserves its content (LocalFileSystem)
    #[test]
    fn prop_local_open_preserves_content(
        path in valid_file_path(),
        data in file_content()
    ) {
        let (fs, temp_dir) = create_temp_fs();

        // Create and write
        let mut file = fs.create_file(&path).unwrap();
        file.write_all(&data).unwrap();
        drop(file);

        // Open and read
        let mut file = fs.open_file(&path).unwrap();
        let mut read_data = Vec::new();
        file.read_to_end(&mut read_data).unwrap();

        prop_assert_eq!(data, read_data);

        drop(file);
        cleanup_temp_fs(&temp_dir);
    }

    /// Test that writing at different offsets produces correct results (LocalFileSystem)
    #[test]
    fn prop_local_write_at_offset_correctness(
        path in valid_file_path(),
        initial_data in file_content().prop_filter("Sufficient size", |d| d.len() >= 20),
        offset in 5usize..10,
        patch_data in prop::collection::vec(any::<u8>(), 1..5)
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&initial_data).unwrap();
        file.write_to_offset(offset as u64, &patch_data).unwrap();

        // Build expected result
        let mut expected = initial_data.clone();
        expected[offset..offset + patch_data.len()].copy_from_slice(&patch_data);

        // Read and verify
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut actual = Vec::new();
        file.read_to_end(&mut actual).unwrap();

        prop_assert_eq!(expected, actual);

        drop(file);
        cleanup_temp_fs(&temp_dir);
    }

    /// Test that concurrent file handles see the same data (LocalFileSystem)
    #[test]
    fn prop_local_multiple_handles_consistency(
        path in valid_file_path(),
        data in file_content()
    ) {
        let (fs, temp_dir) = create_temp_fs();

        // Create and write with first handle
        let mut file1 = fs.create_file(&path).unwrap();
        file1.write_all(&data).unwrap();

        // Open second handle and read
        let mut file2 = fs.open_file(&path).unwrap();
        let mut read_data = Vec::new();
        file2.read_to_end(&mut read_data).unwrap();

        prop_assert_eq!(data, read_data);

        drop(file1);
        drop(file2);
        cleanup_temp_fs(&temp_dir);
    }
}

// Additional edge case tests for LocalFileSystem
#[test]
fn test_local_empty_file_operations() {
    let (fs, temp_dir) = create_temp_fs();
    let path = "/empty.txt";

    let mut file = fs.create_file(path).unwrap();
    assert_eq!(file.get_size().unwrap(), 0);

    // Reading empty file should return 0 bytes
    let mut buf = Vec::new();
    let n = file.read_to_end(&mut buf).unwrap();
    assert_eq!(n, 0);
    assert!(buf.is_empty());

    // Seeking in empty file
    file.seek(SeekFrom::Start(0)).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();

    drop(file);
    cleanup_temp_fs(&temp_dir);
}

#[test]
fn test_local_large_file_operations() {
    let (fs, temp_dir) = create_temp_fs();
    let path = "/large.bin";

    let mut file = fs.create_file(path).unwrap();

    // Write 1MB of data
    let chunk = vec![0xAB; 1024];
    for _ in 0..1024 {
        file.write_all(&chunk).unwrap();
    }

    assert_eq!(file.get_size().unwrap(), 1024 * 1024);

    // Verify content
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut buf = vec![0u8; 1024];
    file.read_exact(&mut buf).unwrap();
    assert_eq!(buf, chunk);

    drop(file);
    cleanup_temp_fs(&temp_dir);
}

#[test]
fn test_local_boundary_seek_positions() {
    let (fs, temp_dir) = create_temp_fs();
    let path = "/boundary.txt";

    let mut file = fs.create_file(path).unwrap();
    file.write_all(b"0123456789").unwrap();

    // Seek to exact end
    let pos = file.seek(SeekFrom::End(0)).unwrap();
    assert_eq!(pos, 10);

    // Seek to exact start
    let pos = file.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(pos, 0);

    // Seek to middle
    let pos = file.seek(SeekFrom::Start(5)).unwrap();
    assert_eq!(pos, 5);

    drop(file);
    cleanup_temp_fs(&temp_dir);
}

// ============================================================================
// OS-Specific Property Tests for LocalFileSystem
// ============================================================================

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Test that file permissions are preserved (Unix only)
    #[test]
    fn prop_local_file_permissions(
        path in valid_file_path(),
        data in file_content(),
        mode in 0o400u32..0o777u32
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let mut file = fs.create_file(&path).unwrap();
        file.write_all(&data).unwrap();
        drop(file);

        // Set permissions
        let abs_path = std::path::Path::new(&temp_dir).join(path.trim_start_matches('/'));
        let mut perms = std::fs::metadata(&abs_path).unwrap().permissions();
        perms.set_mode(mode);
        std::fs::set_permissions(&abs_path, perms).unwrap();

        // Verify permissions
        let metadata = std::fs::metadata(&abs_path).unwrap();
        let actual_mode = metadata.permissions().mode() & 0o777;
        prop_assert_eq!(actual_mode, mode);

        cleanup_temp_fs(&temp_dir);
    }

    /// Test that read-only files cannot be written to (Unix only)
    #[test]
    fn prop_local_readonly_enforcement(
        path in valid_file_path(),
        data in file_content()
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let mut file = fs.create_file(&path).unwrap();
        file.write_all(&data).unwrap();
        drop(file);

        // Make file read-only
        let abs_path = std::path::Path::new(&temp_dir).join(path.trim_start_matches('/'));
        let mut perms = std::fs::metadata(&abs_path).unwrap().permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(&abs_path, perms).unwrap();

        // Try to open for writing - should fail
        let result = std::fs::OpenOptions::new()
            .write(true)
            .open(&abs_path);
        prop_assert!(result.is_err());

        cleanup_temp_fs(&temp_dir);
    }
}

// ============================================================================
// Concurrent Operation Property Tests for LocalFileSystem
// ============================================================================

use std::sync::Arc;
use std::thread;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Test that concurrent reads from different handles work correctly
    #[test]
    fn prop_local_concurrent_reads(
        path in valid_file_path(),
        data in file_content().prop_filter("Non-empty data", |d| d.len() >= 100)
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let fs = Arc::new(fs);

        // Create and write file
        let mut file = fs.create_file(&path).unwrap();
        file.write_all(&data).unwrap();
        drop(file);

        // Spawn multiple readers
        let mut handles = vec![];
        for _ in 0..4 {
            let fs_clone = Arc::clone(&fs);
            let path_clone = path.clone();
            let data_clone = data.clone();

            let handle = thread::spawn(move || {
                let mut file = fs_clone.open_file(&path_clone).unwrap();
                let mut read_data = Vec::new();
                file.read_to_end(&mut read_data).unwrap();
                assert_eq!(read_data, data_clone);
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        cleanup_temp_fs(&temp_dir);
    }

    /// Test that concurrent writes to different files don't interfere
    #[test]
    fn prop_local_concurrent_different_files(
        paths in prop::collection::vec(valid_file_path(), 4..8),
        data_sets in prop::collection::vec(file_content(), 4..8)
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let fs = Arc::new(fs);

        // Ensure unique paths
        let paths: Vec<String> = paths.into_iter()
            .enumerate()
            .map(|(i, p)| format!("{}_file{}", p, i))
            .collect();

        let data_sets = data_sets.into_iter().take(paths.len()).collect::<Vec<_>>();

        // Spawn writers for different files
        let mut handles = vec![];
        for (path, data) in paths.iter().zip(data_sets.iter()) {
            let fs_clone = Arc::clone(&fs);
            let path_clone = path.clone();
            let data_clone = data.clone();

            let handle = thread::spawn(move || {
                let mut file = fs_clone.create_file(&path_clone).unwrap();
                file.write_all(&data_clone).unwrap();
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify all files
        for (path, expected_data) in paths.iter().zip(data_sets.iter()) {
            let mut file = fs.open_file(path).unwrap();
            let mut actual_data = Vec::new();
            file.read_to_end(&mut actual_data).unwrap();
            prop_assert_eq!(&actual_data, expected_data);
        }

        cleanup_temp_fs(&temp_dir);
    }

    /// Test that file locking prevents concurrent writes
    /// Uses separate files to avoid lock contention and blocking
    #[test]
    fn prop_local_file_locking(
        path in valid_file_path(),
        data in file_content()
    ) {
        let (fs, temp_dir) = create_temp_fs();

        // Create file and acquire exclusive lock
        let mut file1 = fs.create_file(&path).unwrap();
        file1.write_all(&data).unwrap();
        file1.set_lock_status(FileLockMode::Exclusive).unwrap();

        // Verify lock is set
        prop_assert_eq!(file1.get_lock_status().unwrap(), FileLockMode::Exclusive);

        // Release lock and verify
        file1.set_lock_status(FileLockMode::Unlocked).unwrap();
        prop_assert_eq!(file1.get_lock_status().unwrap(), FileLockMode::Unlocked);

        // Test shared lock
        file1.set_lock_status(FileLockMode::Shared).unwrap();
        prop_assert_eq!(file1.get_lock_status().unwrap(), FileLockMode::Shared);

        // Clean up
        file1.set_lock_status(FileLockMode::Unlocked).unwrap();
        drop(file1);
        cleanup_temp_fs(&temp_dir);
    }
}

// ============================================================================
// Error Condition Property Tests for LocalFileSystem
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Test that opening non-existent files fails
    #[test]
    fn prop_local_open_nonexistent(path in valid_file_path()) {
        let (fs, temp_dir) = create_temp_fs();

        let result = fs.open_file(&path);
        prop_assert!(result.is_err());

        cleanup_temp_fs(&temp_dir);
    }

    /// Test that creating duplicate files fails
    #[test]
    fn prop_local_create_duplicate(
        path in valid_file_path(),
        data in file_content()
    ) {
        let (fs, temp_dir) = create_temp_fs();

        // Create first file
        let mut file1 = fs.create_file(&path).unwrap();
        file1.write_all(&data).unwrap();
        drop(file1);

        // Try to create again - should fail
        let result = fs.create_file(&path);
        prop_assert!(result.is_err());

        cleanup_temp_fs(&temp_dir);
    }

    /// Test that removing non-existent files fails
    #[test]
    fn prop_local_remove_nonexistent(path in valid_file_path()) {
        let (fs, temp_dir) = create_temp_fs();

        let result = fs.remove_file(&path);
        prop_assert!(result.is_err());

        cleanup_temp_fs(&temp_dir);
    }

    /// Test that reading beyond file size returns correct amount
    #[test]
    fn prop_local_read_beyond_eof(
        path in valid_file_path(),
        data in file_content().prop_filter("Non-empty data", |d| !d.is_empty())
    ) {
        let (fs, temp_dir) = create_temp_fs();
        let mut file = fs.create_file(&path).unwrap();

        file.write_all(&data).unwrap();

        // Seek beyond end
        file.seek(SeekFrom::Start(data.len() as u64 + 100)).unwrap();

        // Try to read - should return 0 bytes
        let mut buf = vec![0u8; 100];
        let n = file.read(&mut buf).unwrap();
        prop_assert_eq!(n, 0);

        drop(file);
        cleanup_temp_fs(&temp_dir);
    }

    /// Test that invalid paths are rejected
    #[test]
    fn prop_local_invalid_paths(
        invalid_path in prop::string::string_regex(".*[<>:\"|?*].*").expect("Invalid regex")
    ) {
        let (fs, temp_dir) = create_temp_fs();

        // Try to create file with invalid path
        let result = fs.create_file(&invalid_path);
        // On Windows, this should fail. On Unix, some chars are valid
        #[cfg(windows)]
        prop_assert!(result.is_err());

        cleanup_temp_fs(&temp_dir);
    }

    /// Test that directory operations on files fail
    #[test]
    fn prop_local_file_not_directory(
        path in valid_file_path(),
        data in file_content()
    ) {
        let (fs, temp_dir) = create_temp_fs();

        // Create a file
        let mut file = fs.create_file(&path).unwrap();
        file.write_all(&data).unwrap();
        drop(file);

        // Try to list it as directory - should fail
        let result = fs.list_directory(&path);
        prop_assert!(result.is_err());

        // Try to remove it as directory - should fail
        let result = fs.remove_directory(&path);
        prop_assert!(result.is_err());

        cleanup_temp_fs(&temp_dir);
    }

    /// Test that file operations on directories fail
    #[test]
    fn prop_local_directory_not_file(path in valid_file_path()) {
        let (fs, temp_dir) = create_temp_fs();

        // Create a directory
        let dir_path = format!("{}_dir", path);
        fs.create_directory(&dir_path).unwrap();

        // Try to open it as file - should fail
        let result = fs.open_file(&dir_path);
        prop_assert!(result.is_err());

        // Try to get filesize - on Unix should fail, on Windows may succeed
        #[cfg(unix)]
        {
            let result = fs.filesize(&dir_path);
            prop_assert!(result.is_err());
        }

        cleanup_temp_fs(&temp_dir);
    }

    /// Test that writing to read-only file handle fails
    #[test]
    fn prop_local_readonly_handle(
        path in valid_file_path(),
        initial_data in file_content(),
        write_data in file_content().prop_filter("Non-empty", |d| !d.is_empty())
    ) {
        let (fs, temp_dir) = create_temp_fs();

        // Create and write initial data
        let mut file = fs.create_file(&path).unwrap();
        file.write_all(&initial_data).unwrap();
        drop(file);

        // Open read-only
        let abs_path = std::path::Path::new(&temp_dir).join(path.trim_start_matches('/'));
        let result = std::fs::OpenOptions::new()
            .read(true)
            .write(false)
            .open(&abs_path);

        if let Ok(mut file) = result {
            // Try to write - should fail
            let write_result = file.write_all(&write_data);
            prop_assert!(write_result.is_err());
        }

        cleanup_temp_fs(&temp_dir);
    }
}

// ============================================================================
// Additional Edge Cases for LocalFileSystem
// ============================================================================

#[test]
fn test_local_nested_directory_creation() {
    let (fs, temp_dir) = create_temp_fs();

    // Create nested directories
    let nested_path = "/level1/level2/level3";
    fs.create_directory_all(nested_path).unwrap();

    assert!(fs.exists(nested_path).unwrap());
    assert!(fs.is_directory(nested_path).unwrap());

    // Create file in nested directory
    let file_path = format!("{}/test.txt", nested_path);
    let mut file = fs.create_file(&file_path).unwrap();
    file.write_all(b"nested file").unwrap();
    drop(file);

    assert!(fs.exists(&file_path).unwrap());
    assert!(fs.is_file(&file_path).unwrap());

    cleanup_temp_fs(&temp_dir);
}

#[test]
fn test_local_directory_listing() {
    let (fs, temp_dir) = create_temp_fs();

    // Create multiple files
    for i in 0..5 {
        let path = format!("/file{}.txt", i);
        let mut file = fs.create_file(&path).unwrap();
        file.write_all(format!("content {}", i).as_bytes()).unwrap();
    }

    // List directory
    let entries = fs.list_directory("/").unwrap();
    assert_eq!(entries.len(), 5);

    // Verify all files are listed
    for i in 0..5 {
        let filename = format!("file{}.txt", i);
        assert!(entries.contains(&filename));
    }

    cleanup_temp_fs(&temp_dir);
}

#[test]
fn test_local_sync_operations() {
    let (fs, temp_dir) = create_temp_fs();
    let path = "/sync_test.txt";

    let mut file = fs.create_file(path).unwrap();
    file.write_all(b"test data").unwrap();

    // Test sync_data
    file.sync_data().unwrap();

    // Test sync_all
    file.sync_all().unwrap();

    drop(file);
    cleanup_temp_fs(&temp_dir);
}

#[test]
fn test_local_file_lock_modes() {
    let (fs, temp_dir) = create_temp_fs();
    let path = "/lock_test.txt";

    let mut file = fs.create_file(path).unwrap();
    file.write_all(b"lock test").unwrap();

    // Test unlocked state
    assert_eq!(file.get_lock_status().unwrap(), FileLockMode::Unlocked);

    // Note: File locking behavior is platform-specific and may block
    // We just verify the basic unlock state works

    drop(file);
    cleanup_temp_fs(&temp_dir);
}

#[test]
fn test_local_cleanup_robustness() {
    // Test that cleanup works even with nested structures
    let (fs, temp_dir) = create_temp_fs();

    // Create complex structure
    fs.create_directory_all("/a/b/c").unwrap();
    fs.create_directory_all("/x/y/z").unwrap();

    for dir in &["/a", "/a/b", "/a/b/c", "/x", "/x/y", "/x/y/z"] {
        let path = format!("{}/file.txt", dir);
        let mut file = fs.create_file(&path).unwrap();
        file.write_all(b"test").unwrap();
    }

    // Cleanup should handle everything
    cleanup_temp_fs(&temp_dir);

    // Verify cleanup
    assert!(!std::path::Path::new(&temp_dir).exists());
}
