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

//! Comprehensive integration tests for VFS implementations

use nanostore::vfs::{
    File, FileLockMode, FileSystem, FileSystemError, LocalFileSystem, MemoryFileSystem,
};
use std::io::{Read, Seek, SeekFrom, Write};

/// Test suite that can be run against any FileSystem implementation
fn test_filesystem_basic_operations<FS: FileSystem>(fs: &FS) {
    // Test file creation
    let path = "/test_basic.txt";
    let mut file = fs.create_file(path).expect("Failed to create file");

    // Verify initial state
    assert_eq!(file.get_size().unwrap(), 0, "New file should be empty");
    // Note: path() may return absolute path for LocalFileSystem, so we just check it's not empty
    assert!(!file.path().is_empty(), "File path should not be empty");

    // Test writing
    file.write_all(b"Hello, World!").expect("Failed to write");
    assert_eq!(file.get_size().unwrap(), 13, "File size should be 13");

    // Test reading
    file.seek(SeekFrom::Start(0)).expect("Failed to seek");
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).expect("Failed to read");
    assert_eq!(buffer, b"Hello, World!");

    // Test file exists
    assert!(fs.exists(path).unwrap(), "File should exist");
    assert!(fs.is_file(path).unwrap(), "Path should be a file");
    assert!(
        !fs.is_directory(path).unwrap(),
        "Path should not be a directory"
    );

    // Test filesize
    assert_eq!(fs.filesize(path).unwrap(), 13, "Filesize should be 13");

    // Clean up
    drop(file);
    fs.remove_file(path).expect("Failed to remove file");
    assert!(
        !fs.exists(path).unwrap(),
        "File should not exist after removal"
    );
}

fn test_filesystem_seek_operations<FS: FileSystem>(fs: &FS) {
    let path = "/test_seek.txt";
    let mut file = fs.create_file(path).expect("Failed to create file");

    // Write test data
    file.write_all(b"0123456789").expect("Failed to write");

    // Test SeekFrom::Start
    file.seek(SeekFrom::Start(5)).expect("Failed to seek");
    let mut buf = [0u8; 5];
    file.read_exact(&mut buf).expect("Failed to read");
    assert_eq!(&buf, b"56789");

    // Test SeekFrom::Current
    file.seek(SeekFrom::Start(0)).expect("Failed to seek");
    file.seek(SeekFrom::Current(3)).expect("Failed to seek");
    let mut buf = [0u8; 3];
    file.read_exact(&mut buf).expect("Failed to read");
    assert_eq!(&buf, b"345");

    // Test SeekFrom::End
    file.seek(SeekFrom::End(-3)).expect("Failed to seek");
    let mut buf = [0u8; 3];
    file.read_exact(&mut buf).expect("Failed to read");
    assert_eq!(&buf, b"789");

    // Clean up
    drop(file);
    fs.remove_file(path).expect("Failed to remove file");
}

fn test_filesystem_resize_operations<FS: FileSystem>(fs: &FS) {
    let path = "/test_resize.txt";
    let mut file = fs.create_file(path).expect("Failed to create file");

    // Write initial data
    file.write_all(b"Hello, World!").expect("Failed to write");
    assert_eq!(file.get_size().unwrap(), 13);

    // Shrink file
    file.set_size(5).expect("Failed to set size");
    assert_eq!(file.get_size().unwrap(), 5);

    // Read and verify truncated content
    file.seek(SeekFrom::Start(0)).expect("Failed to seek");
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).expect("Failed to read");
    assert_eq!(buf, b"Hello");

    // Expand file
    file.set_size(10).expect("Failed to set size");
    assert_eq!(file.get_size().unwrap(), 10);

    // Truncate to zero
    file.truncate().expect("Failed to truncate");
    assert_eq!(file.get_size().unwrap(), 0);

    // Clean up
    drop(file);
    fs.remove_file(path).expect("Failed to remove file");
}

fn test_filesystem_offset_operations<FS: FileSystem>(fs: &FS) {
    let path = "/test_offset.txt";
    let mut file = fs.create_file(path).expect("Failed to create file");

    // Write initial data
    file.write_all(b"0123456789").expect("Failed to write");

    // Test read_at_offset (should not change cursor)
    let cursor_before = file.stream_position().unwrap();
    let mut buf = [0u8; 3];
    file.read_at_offset(5, &mut buf)
        .expect("Failed to read at offset");
    assert_eq!(&buf, b"567");
    let cursor_after = file.stream_position().unwrap();
    assert_eq!(cursor_before, cursor_after, "Cursor should not change");

    // Test write_to_offset (should not change cursor)
    file.seek(SeekFrom::Start(0)).expect("Failed to seek");
    let cursor_before = file.stream_position().unwrap();
    file.write_to_offset(3, b"ABC")
        .expect("Failed to write at offset");
    let cursor_after = file.stream_position().unwrap();
    assert_eq!(cursor_before, cursor_after, "Cursor should not change");

    // Verify the write
    file.seek(SeekFrom::Start(0)).expect("Failed to seek");
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).expect("Failed to read");
    assert_eq!(buf, b"012ABC6789");

    // Clean up
    drop(file);
    fs.remove_file(path).expect("Failed to remove file");
}

fn test_filesystem_directory_operations<FS: FileSystem>(fs: &FS) {
    let dir_path = "/test_dir";

    // Create directory
    fs.create_directory(dir_path)
        .expect("Failed to create directory");
    assert!(fs.exists(dir_path).unwrap(), "Directory should exist");
    assert!(
        fs.is_directory(dir_path).unwrap(),
        "Path should be a directory"
    );
    assert!(!fs.is_file(dir_path).unwrap(), "Path should not be a file");

    // Test duplicate creation fails
    match fs.create_directory(dir_path) {
        Err(FileSystemError::PathExists { .. }) => {}
        _ => panic!("Should fail to create duplicate directory"),
    }

    // Remove directory
    fs.remove_directory(dir_path)
        .expect("Failed to remove directory");
    assert!(
        !fs.exists(dir_path).unwrap(),
        "Directory should not exist after removal"
    );
}

fn test_filesystem_nested_directories<FS: FileSystem>(fs: &FS) {
    let nested_path = "/parent/child/grandchild";

    // Create nested directories
    fs.create_directory_all(nested_path)
        .expect("Failed to create nested directories");
    assert!(
        fs.exists(nested_path).unwrap(),
        "Nested directory should exist"
    );
    assert!(
        fs.is_directory(nested_path).unwrap(),
        "Path should be a directory"
    );

    // Verify parent directories exist
    assert!(fs.exists("/parent").unwrap(), "Parent should exist");
    assert!(fs.exists("/parent/child").unwrap(), "Child should exist");

    // Clean up
    fs.remove_directory_all("/parent")
        .expect("Failed to remove directory tree");
    assert!(
        !fs.exists("/parent").unwrap(),
        "Parent should not exist after removal"
    );
}

fn test_filesystem_error_conditions<FS: FileSystem>(fs: &FS) {
    // Test opening non-existent file
    match fs.open_file("/nonexistent.txt") {
        Err(FileSystemError::PathMissing { .. }) => {}
        _ => panic!("Should fail to open non-existent file"),
    }

    // Test removing non-existent file
    match fs.remove_file("/nonexistent.txt") {
        Err(FileSystemError::PathMissing { .. }) => {}
        _ => panic!("Should fail to remove non-existent file"),
    }

    // Test creating duplicate file
    let path = "/duplicate.txt";
    fs.create_file(path).expect("Failed to create file");
    match fs.create_file(path) {
        Err(FileSystemError::PathExists { .. }) => {}
        _ => panic!("Should fail to create duplicate file"),
    }
    fs.remove_file(path).expect("Failed to remove file");

    // Test filesize on non-existent path
    match fs.filesize("/nonexistent.txt") {
        Err(FileSystemError::PathMissing { .. }) => {}
        _ => panic!("Should fail to get filesize of non-existent file"),
    }
}

fn test_filesystem_file_locking<FS: FileSystem>(fs: &FS) {
    let path = "/test_lock.txt";
    let mut file = fs.create_file(path).expect("Failed to create file");

    // Test initial lock status
    assert_eq!(
        file.get_lock_status().unwrap(),
        FileLockMode::Unlocked,
        "File should be unlocked initially"
    );

    // Test exclusive lock (skip shared lock on Windows as it can cause issues)
    file.set_lock_status(FileLockMode::Exclusive)
        .expect("Failed to set exclusive lock");
    assert_eq!(
        file.get_lock_status().unwrap(),
        FileLockMode::Exclusive,
        "File should be exclusively locked"
    );

    // Test unlock
    file.set_lock_status(FileLockMode::Unlocked)
        .expect("Failed to unlock");
    assert_eq!(
        file.get_lock_status().unwrap(),
        FileLockMode::Unlocked,
        "File should be unlocked"
    );

    // Clean up
    drop(file);
    fs.remove_file(path).expect("Failed to remove file");
}

fn test_filesystem_sync_operations<FS: FileSystem>(fs: &FS) {
    let path = "/test_sync.txt";
    let mut file = fs.create_file(path).expect("Failed to create file");

    // Write data
    file.write_all(b"Test data").expect("Failed to write");

    // Test sync operations (should not fail)
    file.sync_data().expect("Failed to sync data");
    file.sync_all().expect("Failed to sync all");
    file.flush().expect("Failed to flush");

    // Clean up
    drop(file);
    fs.remove_file(path).expect("Failed to remove file");
}

fn test_filesystem_multiple_files<FS: FileSystem>(fs: &FS) {
    // Create multiple files
    let paths = ["/file1.txt", "/file2.txt", "/file3.txt"];

    for path in &paths {
        let mut file = fs.create_file(path).expect("Failed to create file");
        file.write_all(path.as_bytes()).expect("Failed to write");
    }

    // Verify all files exist
    for path in &paths {
        assert!(fs.exists(path).unwrap(), "File should exist");
    }

    // Open and verify content
    for path in &paths {
        let mut file = fs.open_file(path).expect("Failed to open file");
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("Failed to read");
        assert_eq!(buf, path.as_bytes());
    }

    // Clean up
    for path in &paths {
        fs.remove_file(path).expect("Failed to remove file");
    }
}

// Memory FileSystem Tests
#[test]
fn test_memory_fs_basic_operations() {
    let fs = MemoryFileSystem::new();
    test_filesystem_basic_operations(&fs);
}

#[test]
fn test_memory_fs_seek_operations() {
    let fs = MemoryFileSystem::new();
    test_filesystem_seek_operations(&fs);
}

#[test]
fn test_memory_fs_resize_operations() {
    let fs = MemoryFileSystem::new();
    test_filesystem_resize_operations(&fs);
}

#[test]
fn test_memory_fs_offset_operations() {
    let fs = MemoryFileSystem::new();
    test_filesystem_offset_operations(&fs);
}

#[test]
fn test_memory_fs_directory_operations() {
    let fs = MemoryFileSystem::new();
    test_filesystem_directory_operations(&fs);
}

#[test]
fn test_memory_fs_nested_directories() {
    let fs = MemoryFileSystem::new();
    test_filesystem_nested_directories(&fs);
}

#[test]
fn test_memory_fs_error_conditions() {
    let fs = MemoryFileSystem::new();
    test_filesystem_error_conditions(&fs);
}

#[test]
fn test_memory_fs_file_locking() {
    let fs = MemoryFileSystem::new();
    test_filesystem_file_locking(&fs);
}

#[test]
fn test_memory_fs_sync_operations() {
    let fs = MemoryFileSystem::new();
    test_filesystem_sync_operations(&fs);
}

#[test]
fn test_memory_fs_multiple_files() {
    let fs = MemoryFileSystem::new();
    test_filesystem_multiple_files(&fs);
}

// Local FileSystem Tests
#[test]
fn test_local_fs_basic_operations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());
    test_filesystem_basic_operations(&fs);
}

#[test]
fn test_local_fs_seek_operations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());
    test_filesystem_seek_operations(&fs);
}

#[test]
fn test_local_fs_resize_operations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());
    test_filesystem_resize_operations(&fs);
}

#[test]
fn test_local_fs_offset_operations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());
    test_filesystem_offset_operations(&fs);
}

#[test]
fn test_local_fs_directory_operations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());
    test_filesystem_directory_operations(&fs);
}

#[test]
fn test_local_fs_nested_directories() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());
    test_filesystem_nested_directories(&fs);
}

#[test]
fn test_local_fs_error_conditions() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());
    test_filesystem_error_conditions(&fs);
}

#[test]
fn test_local_fs_file_locking() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());
    test_filesystem_file_locking(&fs);
}

#[test]
fn test_local_fs_sync_operations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());
    test_filesystem_sync_operations(&fs);
}

#[test]
fn test_local_fs_multiple_files() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());
    test_filesystem_multiple_files(&fs);
}

// Cross-implementation compatibility tests
#[test]
fn test_memory_and_local_fs_compatibility() {
    let memory_fs = MemoryFileSystem::new();
    let temp_dir = tempfile::tempdir().unwrap();
    let local_fs = LocalFileSystem::new(temp_dir.path());

    // Create same file in both filesystems
    let path = "/compat_test.txt";
    let test_data = b"Compatibility test data";

    // Write to memory fs
    let mut mem_file = memory_fs.create_file(path).unwrap();
    mem_file.write_all(test_data).unwrap();

    // Write to local fs
    let mut local_file = local_fs.create_file(path).unwrap();
    local_file.write_all(test_data).unwrap();

    // Verify both have same size
    assert_eq!(mem_file.get_size().unwrap(), local_file.get_size().unwrap());

    // Verify both have same content
    mem_file.seek(SeekFrom::Start(0)).unwrap();
    local_file.seek(SeekFrom::Start(0)).unwrap();

    let mut mem_buf = Vec::new();
    let mut local_buf = Vec::new();

    mem_file.read_to_end(&mut mem_buf).unwrap();
    local_file.read_to_end(&mut local_buf).unwrap();

    assert_eq!(mem_buf, local_buf);
    assert_eq!(mem_buf, test_data);
}
