//
// Copyright 2019-2026 Hans W. Uhlig. All Rights Reserved.
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

use super::{File, FileLockMode, FileSystem, FileSystemError, FileSystemResult};
use fs2::FileExt;
use std::io::{Read, Seek, SeekFrom, Write};

/// Local File System
pub struct LocalFileSystem {
    root: std::path::PathBuf,
}

impl LocalFileSystem {
    /// Create a new `LocalFileSystem` with the provided root path.
    pub fn new<T: AsRef<std::path::Path>>(root: T) -> Self {
        LocalFileSystem {
            root: root.as_ref().to_path_buf(),
        }
    }
    #[tracing::instrument]
    fn absolute_path(&self, path: &str) -> std::path::PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }
}

impl std::fmt::Debug for LocalFileSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LocalFileSystem({})", self.root.to_string_lossy())
    }
}

impl FileSystem for LocalFileSystem {
    type File = LocalFileHandle;

    #[tracing::instrument]
    fn exists(&self, path: &str) -> FileSystemResult<bool> {
        Ok(self.absolute_path(path).exists())
    }

    #[tracing::instrument]
    fn is_file(&self, path: &str) -> FileSystemResult<bool> {
        Ok(self.absolute_path(path).is_file())
    }

    #[tracing::instrument]
    fn is_directory(&self, path: &str) -> FileSystemResult<bool> {
        Ok(self.absolute_path(path).is_dir())
    }

    #[tracing::instrument]
    fn filesize(&self, path: &str) -> FileSystemResult<u64> {
        std::fs::metadata(self.absolute_path(path))
            .map(|m| m.len())
            .map_err(io_error_to_file_system_error)
    }

    #[tracing::instrument]
    fn create_directory(&self, path: &str) -> FileSystemResult<()> {
        std::fs::create_dir(self.absolute_path(path)).map_err(io_error_to_file_system_error)
    }

    #[tracing::instrument]
    fn create_directory_all(&self, path: &str) -> FileSystemResult<()> {
        std::fs::create_dir_all(self.absolute_path(path)).map_err(io_error_to_file_system_error)
    }

    #[tracing::instrument]
    fn list_directory<'a>(&self, path: &str) -> FileSystemResult<Vec<String>> {
        let rd =
            std::fs::read_dir(self.absolute_path(path)).map_err(io_error_to_file_system_error)?;
        let x = rd
            .filter_map(Result::ok)
            .filter_map(|r| r.file_name().into_string().ok())
            .collect::<Vec<String>>();
        Ok(x)
    }

    #[tracing::instrument]
    fn remove_directory(&self, path: &str) -> FileSystemResult<()> {
        std::fs::remove_dir(self.absolute_path(path)).map_err(io_error_to_file_system_error)
    }

    #[tracing::instrument]
    fn remove_directory_all(&self, path: &str) -> FileSystemResult<()> {
        std::fs::remove_dir_all(self.absolute_path(path)).map_err(io_error_to_file_system_error)
    }

    #[tracing::instrument]
    fn create_file(&self, path: &str) -> FileSystemResult<Self::File> {
        // Validate filename components on Windows (not the full path, which may have drive letters)
        #[cfg(windows)]
        {
            // Check each path component for invalid characters, skipping drive letters (e.g., "C:")
            for (i, component) in path.split(&['/', '\\'][..]).enumerate() {
                // Skip first component if it's a drive letter (e.g., "C:")
                if i == 0
                    && component.len() == 2
                    && component.ends_with(':')
                    && component.chars().next().unwrap().is_ascii_alphabetic()
                {
                    continue;
                }
                if component
                    .chars()
                    .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
                {
                    return Err(FileSystemError::invalid_path(
                        path,
                        "invalid characters in path component",
                    ));
                }
            }
        }

        std::fs::File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(self.absolute_path(path))
            .map(|file| LocalFileHandle {
                path: self.root.join(path.trim_start_matches('/')),
                file,
                lock: FileLockMode::Unlocked,
            })
            .map_err(io_error_to_file_system_error)
    }

    #[tracing::instrument]
    fn open_file(&self, path: &str) -> FileSystemResult<Self::File> {
        // Validate filename components on Windows (not the full path, which may have drive letters)
        #[cfg(windows)]
        {
            // Check each path component for invalid characters, skipping drive letters (e.g., "C:")
            for (i, component) in path.split(&['/', '\\'][..]).enumerate() {
                // Skip first component if it's a drive letter (e.g., "C:")
                if i == 0
                    && component.len() == 2
                    && component.ends_with(':')
                    && component.chars().next().unwrap().is_ascii_alphabetic()
                {
                    continue;
                }
                if component
                    .chars()
                    .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
                {
                    return Err(FileSystemError::invalid_path(
                        path,
                        "invalid characters in path component",
                    ));
                }
            }
        }

        std::fs::File::open(self.absolute_path(path))
            .map(|file| LocalFileHandle {
                path: self.root.join(path),
                file,
                lock: FileLockMode::Unlocked,
            })
            .map_err(io_error_to_file_system_error)
    }

    #[tracing::instrument]
    fn remove_file(&self, path: &str) -> FileSystemResult<()> {
        std::fs::remove_file(self.absolute_path(path)).map_err(io_error_to_file_system_error)
    }
}

/// Local File Handle
pub struct LocalFileHandle {
    path: std::path::PathBuf,
    file: std::fs::File,
    lock: FileLockMode,
}

impl std::fmt::Debug for LocalFileHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LocalFileHandle({})", self.path.to_string_lossy())
    }
}

impl Read for LocalFileHandle {
    #[tracing::instrument]
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }
}

impl Write for LocalFileHandle {
    #[tracing::instrument]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }

    #[tracing::instrument]
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Seek for LocalFileHandle {
    #[tracing::instrument]
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(pos)
    }
}

impl File for LocalFileHandle {
    type FileSystem = LocalFileSystem;

    #[tracing::instrument]
    fn path(&self) -> &str {
        self.path.to_str().unwrap()
    }

    #[tracing::instrument]
    fn get_size(&self) -> FileSystemResult<u64> {
        self.file
            .metadata()
            .map(|m| m.len())
            .map_err(|e| FileSystemError::WrappedError(Box::new(e)))
    }

    #[tracing::instrument]
    fn set_size(&mut self, new_size: u64) -> FileSystemResult<()> {
        self.file
            .set_len(new_size)
            .map_err(|e| FileSystemError::WrappedError(Box::new(e)))
    }

    #[tracing::instrument]
    fn sync_all(&mut self) -> FileSystemResult<()> {
        self.file
            .sync_all()
            .map_err(|e| FileSystemError::WrappedError(Box::new(e)))
    }

    #[tracing::instrument]
    fn sync_data(&mut self) -> FileSystemResult<()> {
        self.file
            .sync_data()
            .map_err(|e| FileSystemError::WrappedError(Box::new(e)))
    }

    fn get_lock_status(&self) -> FileSystemResult<FileLockMode> {
        Ok(self.lock)
    }

    fn set_lock_status(&mut self, mode: FileLockMode) -> FileSystemResult<()> {
        let result = match mode {
            FileLockMode::Unlocked => self.file.unlock(),
            FileLockMode::Shared => self.file.lock_shared(),
            FileLockMode::Exclusive => self.file.lock_exclusive(),
        }
        .map_err(io_error_to_file_system_error);

        if result.is_ok() {
            self.lock = mode;
        }

        result
    }
}

fn io_error_to_file_system_error(error: std::io::Error) -> FileSystemError {
    // Note: We lose path context here since std::io::Error doesn't include it
    // Callers should use the new helper methods when they have path context
    match error.kind() {
        std::io::ErrorKind::NotFound => FileSystemError::PathMissing {
            path: "<unknown>".to_string(),
        },
        std::io::ErrorKind::AlreadyExists => FileSystemError::PathExists {
            path: "<unknown>".to_string(),
        },
        std::io::ErrorKind::PermissionDenied => FileSystemError::PermissionDenied {
            path: "<unknown>".to_string(),
            operation: "<unknown>".to_string(),
        },
        std::io::ErrorKind::InvalidInput => FileSystemError::InvalidPath {
            path: "<unknown>".to_string(),
            reason: error.to_string(),
        },
        _ => FileSystemError::WrappedError(Box::new(error)),
    }
}

#[cfg(test)]
mod test {
    #[test]
    #[tracing_test::traced_test]
    fn test_local_filesystem() {
        use super::{File, FileSystem, LocalFileSystem};
        use std::io::{Read, Seek, SeekFrom, Write};
        use std::time::{SystemTime, UNIX_EPOCH};

        let fs = LocalFileSystem::new(std::env::temp_dir().to_str().unwrap());
        let filename = format!(
            "./test-{}.tst",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_nanos()
        );
        {
            // Create new File
            let mut file = fs
                .create_file(filename.as_str())
                .expect("Error Creating File");
            assert_eq!(file.get_size().unwrap(), 0, "File size wasn't zero");

            // Write to File
            file.write_all(b"Hello, World!").unwrap();
            assert_eq!(file.get_size().unwrap(), 13, "File size wasn't 13");

            // Read full File Contents and compare
            let mut buf = Vec::new();
            file.seek(SeekFrom::Start(0))
                .expect("Error Seeking to beginning of file");
            file.read_to_end(&mut buf).expect("Error Reading File");
            assert_eq!(buf, b"Hello, World!");

            // Shrink file to size 5 and test
            file.set_size(5).expect("Error Setting File Size");
            assert_eq!(file.get_size().unwrap(), 5);

            // Seek to start and read full file
            let mut buf = Vec::new();
            file.seek(SeekFrom::Start(0)).expect("Error Seeking File");
            file.read_to_end(&mut buf).expect("Error Reading File");
            assert_eq!(buf, b"Hello");

            // Set file size to zero and test
            file.set_size(0).unwrap();
            assert_eq!(file.get_size().expect("Unable to get file size"), 0);

            // Write new data to file and test
            file.seek(SeekFrom::Start(0))
                .expect("Error Seeking to beginning of file");
            file.write_all(b"Goodbye!").expect("Error Writing File");
            assert_eq!(file.get_size().expect("Error getting file size"), 8);

            // Seek to start and read full file
            let mut buf = Vec::new();
            file.seek(SeekFrom::Start(0)).expect("Error Seeking File");
            file.read_to_end(&mut buf).expect("Error Reading File");
            assert_eq!(buf, b"Goodbye!");
        }
        {
            // Open existing file and test
            let mut file = fs.open_file(filename.as_str()).expect("Error Opening File");
            assert_eq!(file.get_size().expect("Error getting file size"), 8);

            // Seek to start and read full file
            let mut buf = Vec::new();
            file.seek(SeekFrom::Start(0)).expect("Error Seeking File");
            file.read_to_end(&mut buf).expect("Error Reading File");
            assert_eq!(buf, b"Goodbye!");
        }

        // Remove file and test
        fs.remove_file(filename.as_str())
            .expect("Error Removing File");
        assert!(
            !fs.exists(filename.as_str())
                .expect("Error Checking File Existence")
        );
    }
}
