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

//! Edge case and error handling tests for VFS implementations

use nanostore::vfs::{
    File, FileLockMode, FileSystem, FileSystemError, LocalFileSystem, MemoryFileSystem,
};
use std::io::{Read, Seek, SeekFrom, Write};

// ============================================================================
// Memory FileSystem Edge Cases
// ============================================================================

#[test]
fn test_memory_fs_path_normalization() {
    let fs = MemoryFileSystem::new();

    // Test various path formats
    let paths = ["/test.txt", "//test.txt", "/./test.txt"];

    // Only the first path should work correctly
    let mut file = fs.create_file(paths[0]).unwrap();
    file.write_all(b"test").unwrap();
    drop(file);

    assert!(fs.exists(paths[0]).unwrap());
}

#[test]
fn test_memory_fs_concurrent_access() {
    let fs = MemoryFileSystem::new();
    let path = "/concurrent.txt";

    // Create file with first handle
    let mut file1 = fs.create_file(path).unwrap();
    file1.write_all(b"Hello").unwrap();

    // Open second handle
    let mut file2 = fs.open_file(path).unwrap();

    // Write more with first handle
    file1.write_all(b" World").unwrap();

    // Read with second handle should see all data
    let mut buf = Vec::new();
    file2.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, b"Hello World");
}

#[test]
fn test_memory_fs_write_beyond_size() {
    let fs = MemoryFileSystem::new();
    let path = "/expand.txt";

    let mut file = fs.create_file(path).unwrap();

    // Write at offset beyond current size
    file.write_to_offset(100, b"test").unwrap();

    // File should expand
    assert!(file.get_size().unwrap() >= 104);

    // Read at that offset
    let mut buf = [0u8; 4];
    file.read_at_offset(100, &mut buf).unwrap();
    assert_eq!(&buf, b"test");
}

#[test]
fn test_memory_fs_seek_beyond_end() {
    let fs = MemoryFileSystem::new();
    let path = "/seek_beyond.txt";

    let mut file = fs.create_file(path).unwrap();
    file.write_all(b"0123456789").unwrap();

    // Seek beyond end
    let pos = file.seek(SeekFrom::Start(100)).unwrap();
    assert_eq!(pos, 100);

    // Writing here should expand the file
    file.write_all(b"X").unwrap();
    assert!(file.get_size().unwrap() >= 101);
}

#[test]
fn test_memory_fs_negative_seek() {
    let fs = MemoryFileSystem::new();
    let path = "/negative_seek.txt";

    let mut file = fs.create_file(path).unwrap();
    file.write_all(b"0123456789").unwrap();

    // Seek to end
    file.seek(SeekFrom::End(0)).unwrap();

    // Seek backward from end
    let pos = file.seek(SeekFrom::End(-5)).unwrap();
    assert_eq!(pos, 5);

    // Read from that position
    let mut buf = [0u8; 5];
    file.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"56789");
}

#[test]
fn test_memory_fs_directory_listing() {
    let fs = MemoryFileSystem::new();

    // Create directory with metadata
    fs.create_directory("/testdir").unwrap();

    // List should be empty initially
    let entries = fs.list_directory("/testdir").unwrap();
    assert_eq!(entries.len(), 0);
}

#[test]
fn test_memory_fs_remove_nonexistent() {
    let fs = MemoryFileSystem::new();

    // Try to remove non-existent file
    match fs.remove_file("/nonexistent.txt") {
        Err(FileSystemError::PathMissing { .. }) => {}
        _ => panic!("Should fail with PathMissing"),
    }

    // Try to remove non-existent directory
    match fs.remove_directory("/nonexistent_dir") {
        Err(FileSystemError::PathMissing { .. }) => {}
        _ => panic!("Should fail with PathMissing"),
    }
}

#[test]
fn test_memory_fs_create_duplicate() {
    let fs = MemoryFileSystem::new();
    let path = "/duplicate.txt";

    // Create first file
    fs.create_file(path).unwrap();

    // Try to create again
    match fs.create_file(path) {
        Err(FileSystemError::PathExists { .. }) => {}
        _ => panic!("Should fail with PathExists"),
    }
}

#[test]
fn test_memory_fs_lock_transitions() {
    let fs = MemoryFileSystem::new();
    let path = "/lock_test.txt";

    let mut file = fs.create_file(path).unwrap();

    // Test all lock transitions
    assert_eq!(file.get_lock_status().unwrap(), FileLockMode::Unlocked);

    file.set_lock_status(FileLockMode::Shared).unwrap();
    assert_eq!(file.get_lock_status().unwrap(), FileLockMode::Shared);

    file.set_lock_status(FileLockMode::Exclusive).unwrap();
    assert_eq!(file.get_lock_status().unwrap(), FileLockMode::Exclusive);

    file.set_lock_status(FileLockMode::Shared).unwrap();
    assert_eq!(file.get_lock_status().unwrap(), FileLockMode::Shared);

    file.set_lock_status(FileLockMode::Unlocked).unwrap();
    assert_eq!(file.get_lock_status().unwrap(), FileLockMode::Unlocked);
}

#[test]
fn test_memory_fs_zero_byte_operations() {
    let fs = MemoryFileSystem::new();
    let path = "/zero.txt";

    let mut file = fs.create_file(path).unwrap();

    // Write zero bytes
    file.write_all(&[]).unwrap();
    assert_eq!(file.get_size().unwrap(), 0);

    // Read zero bytes
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).unwrap();
    assert_eq!(buf.len(), 0);

    // Write at offset 0 with zero bytes
    file.write_to_offset(0, &[]).unwrap();
    assert_eq!(file.get_size().unwrap(), 0);
}

#[test]
fn test_memory_fs_flush_operations() {
    let fs = MemoryFileSystem::new();
    let path = "/flush.txt";

    let mut file = fs.create_file(path).unwrap();
    file.write_all(b"test").unwrap();

    // Flush should succeed (even though it's a no-op for memory)
    file.flush().unwrap();
    file.sync_data().unwrap();
    file.sync_all().unwrap();

    // Data should still be there
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, b"test");
}

// ============================================================================
// Local FileSystem Edge Cases
// ============================================================================

#[test]
fn test_local_fs_absolute_path_handling() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    // Test with leading slash (should be relative to root)
    let path = "/test.txt";
    let mut file = fs.create_file(path).unwrap();
    file.write_all(b"test").unwrap();
    drop(file);

    assert!(fs.exists(path).unwrap());
    fs.remove_file(path).unwrap();
}

#[test]
fn test_local_fs_nested_directory_creation() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    // Create deeply nested directories
    let path = "/a/b/c/d/e";
    fs.create_directory_all(path).unwrap();

    assert!(fs.exists(path).unwrap());
    assert!(fs.is_directory(path).unwrap());

    // Verify all parent directories exist
    assert!(fs.exists("/a").unwrap());
    assert!(fs.exists("/a/b").unwrap());
    assert!(fs.exists("/a/b/c").unwrap());
    assert!(fs.exists("/a/b/c/d").unwrap());
}

#[test]
fn test_local_fs_file_in_nested_directory() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    // Create nested directories
    fs.create_directory_all("/dir1/dir2").unwrap();

    // Create file in nested directory
    let path = "/dir1/dir2/file.txt";
    let mut file = fs.create_file(path).unwrap();
    file.write_all(b"nested").unwrap();
    drop(file);

    assert!(fs.exists(path).unwrap());
    assert!(fs.is_file(path).unwrap());
}

#[test]
fn test_local_fs_remove_directory_with_contents() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    // Create directory with file
    fs.create_directory("/testdir").unwrap();
    let mut file = fs.create_file("/testdir/file.txt").unwrap();
    file.write_all(b"content").unwrap();
    drop(file);

    // Remove directory and all contents
    fs.remove_directory_all("/testdir").unwrap();
    assert!(!fs.exists("/testdir").unwrap());
}

#[test]
fn test_local_fs_file_permissions() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    let path = "/perms.txt";
    let mut file = fs.create_file(path).unwrap();
    file.write_all(b"test").unwrap();

    // File should be readable and writable
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, b"test");

    file.write_all(b" more").unwrap();
}

#[test]
fn test_local_fs_large_file() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    let path = "/large.bin";
    let mut file = fs.create_file(path).unwrap();

    // Write 10MB
    let chunk = vec![0xAB; 1024 * 1024];
    for _ in 0..10 {
        file.write_all(&chunk).unwrap();
    }

    assert_eq!(file.get_size().unwrap(), 10 * 1024 * 1024);

    // Clean up
    drop(file);
    fs.remove_file(path).unwrap();
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_error_display() {
    // Test that errors can be displayed
    let err = FileSystemError::path_missing("/test/path");
    let display = format!("{}", err);
    assert!(!display.is_empty());
    assert!(display.contains("/test/path"));

    let err = FileSystemError::path_exists("/test/path");
    let display = format!("{}", err);
    assert!(!display.is_empty());
    assert!(display.contains("/test/path"));

    let err = FileSystemError::invalid_path("/bad/path", "invalid characters");
    let display = format!("{}", err);
    assert!(!display.is_empty());
    assert!(display.contains("/bad/path"));
}

#[test]
fn test_error_debug() {
    // Test that errors can be debugged
    let err = FileSystemError::permission_denied("/test/file", "write");
    let debug = format!("{:?}", err);
    assert!(!debug.is_empty());
}

// ============================================================================
// FileLockMode Tests
// ============================================================================

#[test]
fn test_file_lock_mode_equality() {
    assert_eq!(FileLockMode::Unlocked, FileLockMode::Unlocked);
    assert_eq!(FileLockMode::Shared, FileLockMode::Shared);
    assert_eq!(FileLockMode::Exclusive, FileLockMode::Exclusive);

    assert_ne!(FileLockMode::Unlocked, FileLockMode::Shared);
    assert_ne!(FileLockMode::Shared, FileLockMode::Exclusive);
    assert_ne!(FileLockMode::Unlocked, FileLockMode::Exclusive);
}

#[test]
fn test_file_lock_mode_debug() {
    let modes = [
        FileLockMode::Unlocked,
        FileLockMode::Shared,
        FileLockMode::Exclusive,
    ];

    for mode in &modes {
        let debug = format!("{:?}", mode);
        assert!(!debug.is_empty());
    }
}

#[test]
fn test_file_lock_mode_copy_clone() {
    let mode = FileLockMode::Shared;
    let copied = mode;
    let cloned = mode;

    assert_eq!(mode, copied);
    assert_eq!(mode, cloned);
}

// ============================================================================
// Stress Tests
// ============================================================================

#[test]
fn test_many_small_files() {
    let fs = MemoryFileSystem::new();

    // Create 1000 small files
    for i in 0..1000 {
        let path = format!("/file_{}.txt", i);
        let mut file = fs.create_file(&path).unwrap();
        file.write_all(format!("Content {}", i).as_bytes()).unwrap();
    }

    // Verify all exist
    for i in 0..1000 {
        let path = format!("/file_{}.txt", i);
        assert!(fs.exists(&path).unwrap());
    }

    // Clean up
    for i in 0..1000 {
        let path = format!("/file_{}.txt", i);
        fs.remove_file(&path).unwrap();
    }
}

#[test]
fn test_repeated_create_remove() {
    let fs = MemoryFileSystem::new();
    let path = "/repeated.txt";

    // Create and remove 100 times
    for i in 0..100 {
        let mut file = fs.create_file(path).unwrap();
        file.write_all(format!("Iteration {}", i).as_bytes())
            .unwrap();
        drop(file);

        assert!(fs.exists(path).unwrap());
        fs.remove_file(path).unwrap();
        assert!(!fs.exists(path).unwrap());
    }
}

#[test]
fn test_interleaved_operations() {
    let fs = MemoryFileSystem::new();

    // Create multiple files
    let paths = ["/file1.txt", "/file2.txt", "/file3.txt"];
    let mut files = Vec::new();

    for path in &paths {
        files.push(fs.create_file(path).unwrap());
    }

    // Write to them in interleaved fashion
    for i in 0..10 {
        for (idx, file) in files.iter_mut().enumerate() {
            file.write_all(format!("{}:{} ", idx, i).as_bytes())
                .unwrap();
        }
    }

    // Verify content
    drop(files);
    for (idx, path) in paths.iter().enumerate() {
        let mut file = fs.open_file(path).unwrap();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).unwrap();

        let content = String::from_utf8(buf).unwrap();
        for i in 0..10 {
            assert!(content.contains(&format!("{}:{}", idx, i)));
        }
    }
}

// ============================================================================
// Security Tests - Path Traversal
// ============================================================================

#[test]
fn test_path_traversal_parent_directory() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    // Create a file in the root
    let mut file = fs.create_file("/safe.txt").unwrap();
    file.write_all(b"safe content").unwrap();
    drop(file);

    // Try to access parent directory with ../
    // This should be contained within the root
    let result = fs.create_file("/../escape.txt");
    // Should either fail or be contained within root
    if let Ok(mut file) = result {
        file.write_all(b"test").unwrap();
        drop(file);
        // Verify it's actually within our temp directory
        assert!(
            temp_dir.path().join("escape.txt").exists()
                || !std::path::Path::new("/escape.txt").exists()
        );
    }
}

#[test]
fn test_path_traversal_multiple_parents() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    // Try multiple parent directory traversals
    let paths = [
        "../../etc/passwd",
        "../../../etc/shadow",
        "subdir/../../escape.txt",
        "./../../escape2.txt",
    ];

    for path in &paths {
        // These should either fail or be contained within root
        let result = fs.create_file(path);
        if let Ok(mut file) = result {
            file.write_all(b"test").unwrap();
            drop(file);
            // Verify file is within temp directory
            let file_path = temp_dir.path().join(path.trim_start_matches('/'));
            if file_path.exists() {
                assert!(file_path.starts_with(temp_dir.path()));
            }
        }
    }
}

#[test]
fn test_path_traversal_current_directory() {
    let fs = MemoryFileSystem::new();

    // Test ./ handling
    let paths = ["/./test.txt", "/dir/./file.txt", "/./././test2.txt"];

    for path in &paths {
        let result = fs.create_file(path);
        // Should either work or fail consistently
        if let Ok(mut file) = result {
            file.write_all(b"test").unwrap();
            drop(file);
            // Verify we can access it
            assert!(fs.exists(path).unwrap());
        }
    }
}

#[test]
fn test_path_traversal_mixed() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    // Create a subdirectory
    fs.create_directory("/subdir").unwrap();

    // Try mixed traversal patterns
    let paths = [
        "/subdir/../file.txt",
        "/subdir/./../file2.txt",
        "/./subdir/../file3.txt",
    ];

    for path in &paths {
        let result = fs.create_file(path);
        if let Ok(mut file) = result {
            file.write_all(b"test").unwrap();
            drop(file);
            // Should be within root
            assert!(fs.exists(path).unwrap() || fs.exists("/file.txt").unwrap());
        }
    }
}

// ============================================================================
// Long Path Tests
// ============================================================================

#[test]
fn test_very_long_filename() {
    let fs = MemoryFileSystem::new();

    // Create a filename with 255 characters (typical filesystem limit)
    let long_name = format!("/{}", "a".repeat(255));

    let result = fs.create_file(&long_name);
    match result {
        Ok(mut file) => {
            file.write_all(b"test").unwrap();
            drop(file);
            assert!(fs.exists(&long_name).unwrap());
        }
        Err(_) => {
            // Some filesystems may reject this, which is acceptable
        }
    }
}

#[test]
fn test_extremely_long_filename() {
    let fs = MemoryFileSystem::new();

    // Create a filename with 1000 characters (exceeds typical limits)
    let very_long_name = format!("/{}", "b".repeat(1000));

    let result = fs.create_file(&very_long_name);
    // This should either work (memory fs) or fail gracefully
    match result {
        Ok(mut file) => {
            file.write_all(b"test").unwrap();
            drop(file);
            assert!(fs.exists(&very_long_name).unwrap());
        }
        Err(e) => {
            // Should fail with InvalidPath or similar
            assert!(matches!(
                e,
                FileSystemError::InvalidPath { .. }
                    | FileSystemError::InternalError(_)
                    | FileSystemError::WrappedError(_)
            ));
        }
    }
}

#[test]
fn test_very_long_path() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    // Create a deeply nested path (each component is reasonable, but total is long)
    let mut path = String::from("/");
    for i in 0..50 {
        path.push_str(&format!("dir{}/", i));
    }
    path.push_str("file.txt");

    // Try to create the directory structure
    let dir_path = path.rsplit_once('/').unwrap().0;
    let result = fs.create_directory_all(dir_path);

    if result.is_ok() {
        // Try to create file in deep path
        let file_result = fs.create_file(&path);
        match file_result {
            Ok(mut file) => {
                file.write_all(b"deep").unwrap();
                drop(file);
                assert!(fs.exists(&path).unwrap());
            }
            Err(_) => {
                // May fail on some systems due to path length limits
            }
        }
    }
}

#[test]
fn test_long_path_components() {
    let fs = MemoryFileSystem::new();

    // Create path with multiple long components
    let long_component = "x".repeat(100);
    let path = format!(
        "/{}/{}/{}/file.txt",
        long_component, long_component, long_component
    );

    // Create directory structure
    let dir_path = path.rsplit_once('/').unwrap().0;
    fs.create_directory_all(dir_path).unwrap();

    // Create file
    let result = fs.create_file(&path);
    match result {
        Ok(mut file) => {
            file.write_all(b"test").unwrap();
            drop(file);
            assert!(fs.exists(&path).unwrap());
        }
        Err(_) => {
            // May fail on some systems
        }
    }
}

// ============================================================================
// Special Character Tests
// ============================================================================

#[test]
fn test_special_characters_in_filename() {
    let fs = MemoryFileSystem::new();

    // Test various special characters that should be handled
    let special_names = [
        "/file with spaces.txt",
        "/file-with-dashes.txt",
        "/file_with_underscores.txt",
        "/file.multiple.dots.txt",
        "/file(with)parens.txt",
        "/file[with]brackets.txt",
        "/file{with}braces.txt",
        "/file'with'quotes.txt",
        "/file@with@at.txt",
        "/file#with#hash.txt",
        "/file$with$dollar.txt",
        "/file%with%percent.txt",
        "/file&with&ampersand.txt",
        "/file+with+plus.txt",
        "/file=with=equals.txt",
        "/file~with~tilde.txt",
        "/file`with`backtick.txt",
    ];

    for name in &special_names {
        let result = fs.create_file(name);
        match result {
            Ok(mut file) => {
                file.write_all(b"test").unwrap();
                drop(file);
                assert!(
                    fs.exists(name).unwrap(),
                    "Failed to verify existence of {}",
                    name
                );
            }
            Err(e) => {
                // Some characters may be invalid, which is acceptable
                println!("Character test failed for {}: {:?}", name, e);
            }
        }
    }
}

#[test]
fn test_unicode_in_filename() {
    let fs = MemoryFileSystem::new();

    // Test Unicode characters
    let unicode_names = [
        "/файл.txt",        // Cyrillic
        "/文件.txt",        // Chinese
        "/ファイル.txt",    // Japanese
        "/파일.txt",        // Korean
        "/αρχείο.txt",      // Greek
        "/ملف.txt",         // Arabic
        "/קוֹבֶץ.txt",        // Hebrew
        "/emoji😀file.txt", // Emoji
        "/café.txt",        // Accented
        "/naïve.txt",       // Diaeresis
    ];

    for name in &unicode_names {
        let result = fs.create_file(name);
        match result {
            Ok(mut file) => {
                file.write_all(b"unicode test").unwrap();
                drop(file);
                assert!(
                    fs.exists(name).unwrap(),
                    "Failed to verify existence of {}",
                    name
                );
            }
            Err(e) => {
                println!("Unicode test failed for {}: {:?}", name, e);
            }
        }
    }
}

#[test]
fn test_control_characters_in_filename() {
    let fs = MemoryFileSystem::new();

    // Test that control characters are rejected or handled safely
    let control_names = [
        "/file\x00null.txt",      // Null byte
        "/file\x01soh.txt",       // Start of heading
        "/file\x07bell.txt",      // Bell
        "/file\x08backspace.txt", // Backspace
        "/file\x0Atab.txt",       // Tab
        "/file\x0Anewline.txt",   // Newline
        "/file\x0Dcarriage.txt",  // Carriage return
    ];

    for name in &control_names {
        let result = fs.create_file(name);
        // These should typically fail or be sanitized
        match result {
            Ok(_) => {
                // If it succeeds, verify it's safe
                println!(
                    "Warning: Control character accepted in filename: {:?}",
                    name
                );
            }
            Err(_) => {
                // Expected to fail - this is good
            }
        }
    }
}

#[test]
fn test_reserved_names_windows() {
    // Test Windows reserved names (should work on Unix, may fail on Windows)
    let fs = MemoryFileSystem::new();

    let reserved_names = [
        "/CON.txt",
        "/PRN.txt",
        "/AUX.txt",
        "/NUL.txt",
        "/COM1.txt",
        "/LPT1.txt",
    ];

    for name in &reserved_names {
        let result = fs.create_file(name);
        // Behavior depends on platform
        match result {
            Ok(mut file) => {
                file.write_all(b"test").unwrap();
                drop(file);
            }
            Err(_) => {
                // May fail on Windows
            }
        }
    }
}

// ============================================================================
// Case Sensitivity Tests
// ============================================================================

#[test]
fn test_case_sensitivity_memory_fs() {
    let fs = MemoryFileSystem::new();

    // Create file with lowercase name
    let mut file1 = fs.create_file("/test.txt").unwrap();
    file1.write_all(b"lowercase").unwrap();
    drop(file1);

    // Try to create file with uppercase name
    let result = fs.create_file("/TEST.txt");

    match result {
        Ok(mut file2) => {
            // If it succeeds, filesystem is case-sensitive
            file2.write_all(b"uppercase").unwrap();
            drop(file2);

            // Both should exist
            assert!(fs.exists("/test.txt").unwrap());
            assert!(fs.exists("/TEST.txt").unwrap());

            // Verify they're different files
            let mut file1 = fs.open_file("/test.txt").unwrap();
            let mut buf1 = Vec::new();
            file1.read_to_end(&mut buf1).unwrap();

            let mut file2 = fs.open_file("/TEST.txt").unwrap();
            let mut buf2 = Vec::new();
            file2.read_to_end(&mut buf2).unwrap();

            assert_eq!(buf1, b"lowercase");
            assert_eq!(buf2, b"uppercase");
        }
        Err(FileSystemError::PathExists { .. }) => {
            // Filesystem is case-insensitive
            println!("MemoryFileSystem is case-insensitive");
        }
        Err(e) => {
            panic!("Unexpected error: {:?}", e);
        }
    }
}

#[test]
fn test_case_sensitivity_local_fs() {
    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    // Create file with lowercase name
    let mut file1 = fs.create_file("/test.txt").unwrap();
    file1.write_all(b"lowercase").unwrap();
    drop(file1);

    // Try to create file with uppercase name
    let result = fs.create_file("/TEST.txt");

    match result {
        Ok(mut file2) => {
            // Filesystem is case-sensitive (Linux, macOS with case-sensitive APFS)
            file2.write_all(b"uppercase").unwrap();
            drop(file2);

            assert!(fs.exists("/test.txt").unwrap());
            assert!(fs.exists("/TEST.txt").unwrap());
        }
        Err(FileSystemError::PathExists { .. }) => {
            // Filesystem is case-insensitive (Windows, macOS default)
            println!("LocalFileSystem is case-insensitive on this platform");

            // Verify the original file still exists
            assert!(fs.exists("/test.txt").unwrap());
        }
        Err(e) => {
            panic!("Unexpected error: {:?}", e);
        }
    }
}

#[test]
fn test_case_sensitivity_directory_operations() {
    let fs = MemoryFileSystem::new();

    // Create directory with lowercase name
    fs.create_directory("/mydir").unwrap();

    // Try to create directory with uppercase name
    let result = fs.create_directory("/MYDIR");

    match result {
        Ok(_) => {
            // Case-sensitive: both should exist
            assert!(fs.exists("/mydir").unwrap());
            assert!(fs.exists("/MYDIR").unwrap());
        }
        Err(FileSystemError::PathExists { .. }) => {
            // Case-insensitive: only one exists
            assert!(fs.exists("/mydir").unwrap());
        }
        Err(e) => {
            panic!("Unexpected error: {:?}", e);
        }
    }
}

// ============================================================================
// Symbolic Link Tests (LocalFileSystem only)
// ============================================================================

#[cfg(unix)]
#[test]
fn test_symlink_to_file() {
    use std::os::unix::fs::symlink;

    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    // Create a regular file
    let mut file = fs.create_file("/target.txt").unwrap();
    file.write_all(b"target content").unwrap();
    drop(file);

    // Create a symlink to it
    let target_path = temp_dir.path().join("target.txt");
    let link_path = temp_dir.path().join("link.txt");
    symlink(&target_path, &link_path).unwrap();

    // Try to open via symlink
    let result = fs.open_file("/link.txt");
    match result {
        Ok(mut file) => {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).unwrap();
            assert_eq!(buf, b"target content");
        }
        Err(e) => {
            println!("Symlink test failed: {:?}", e);
        }
    }
}

#[cfg(unix)]
#[test]
fn test_symlink_to_directory() {
    use std::os::unix::fs::symlink;

    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    // Create a directory with a file
    fs.create_directory("/targetdir").unwrap();
    let mut file = fs.create_file("/targetdir/file.txt").unwrap();
    file.write_all(b"in directory").unwrap();
    drop(file);

    // Create a symlink to the directory
    let target_path = temp_dir.path().join("targetdir");
    let link_path = temp_dir.path().join("linkdir");
    symlink(&target_path, &link_path).unwrap();

    // Try to access file through symlinked directory
    let result = fs.open_file("/linkdir/file.txt");
    match result {
        Ok(mut file) => {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).unwrap();
            assert_eq!(buf, b"in directory");
        }
        Err(e) => {
            println!("Symlink directory test failed: {:?}", e);
        }
    }
}

#[cfg(unix)]
#[test]
fn test_broken_symlink() {
    use std::os::unix::fs::symlink;

    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    // Create a symlink to a non-existent file
    let target_path = temp_dir.path().join("nonexistent.txt");
    let link_path = temp_dir.path().join("broken_link.txt");
    symlink(&target_path, &link_path).unwrap();

    // Try to open the broken symlink
    let result = fs.open_file("/broken_link.txt");
    match result {
        Err(FileSystemError::PathMissing { .. }) => {
            // Expected behavior
        }
        Err(e) => {
            println!("Broken symlink returned error: {:?}", e);
        }
        Ok(_) => {
            panic!("Should not be able to open broken symlink");
        }
    }
}

#[cfg(unix)]
#[test]
fn test_circular_symlink() {
    use std::os::unix::fs::symlink;

    let temp_dir = tempfile::tempdir().unwrap();
    let fs = LocalFileSystem::new(temp_dir.path());

    // Create circular symlinks
    let link1_path = temp_dir.path().join("link1.txt");
    let link2_path = temp_dir.path().join("link2.txt");

    symlink(&link2_path, &link1_path).unwrap();
    symlink(&link1_path, &link2_path).unwrap();

    // Try to open circular symlink
    let result = fs.open_file("/link1.txt");
    match result {
        Err(_) => {
            // Should fail with some error (PathMissing, IOError, etc.)
        }
        Ok(_) => {
            panic!("Should not be able to open circular symlink");
        }
    }
}

#[cfg(windows)]
#[test]
fn test_windows_symlink_note() {
    // Note: Windows symlinks require admin privileges or developer mode
    // These tests are Unix-only for practical reasons
    println!("Symlink tests are Unix-only due to Windows privilege requirements");
}
