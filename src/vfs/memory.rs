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

use super::FileLockMode;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, RwLock};

use super::{File, FileSystem, FileSystemError, FileSystemResult};

/// Memory File System
///
/// ```rust
/// use nanostore::vfs::{File, FileSystem, MemoryFileSystem};
/// use std::io::{Read, Seek, SeekFrom, Write};
///
/// let fs = MemoryFileSystem::new();
///
/// let mut file = fs.create_file("/test.txt").expect("Error Creating File");
/// file.write_all(b"Hello, World!").unwrap();
/// assert_eq!(file.get_size().unwrap(), 13, "File size wasn't 13");
/// file.seek(SeekFrom::Start(0)).unwrap();
///
/// ```
///
#[derive(Default)]
pub struct MemoryFileSystem(Arc<RwLock<BTreeMap<String, MemoryEntry>>>);

impl MemoryFileSystem {
    /// Create a new Memory FileSystem
    pub fn new() -> MemoryFileSystem {
        MemoryFileSystem(Arc::new(RwLock::new(BTreeMap::new())))
    }
}

impl std::fmt::Debug for MemoryFileSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MemoryFileSystem {{ files: {:?} }}", self.0)
    }
}

impl FileSystem for MemoryFileSystem {
    type File = MemoryFileHandle;

    #[tracing::instrument]
    fn exists(&self, path: &str) -> FileSystemResult<bool> {
        let tree = self.0.read().expect("Poisoned Lock");
        Ok(tree.contains_key(path))
    }

    #[tracing::instrument]
    fn is_file(&self, path: &str) -> FileSystemResult<bool> {
        let tree = self.0.read().expect("Poisoned Lock");
        if let Some(entry) = tree.get(path) {
            match entry {
                MemoryEntry::File(_) => Ok(true),
                _ => Ok(false),
            }
        } else {
            Ok(false)
        }
    }

    #[tracing::instrument]
    fn is_directory(&self, path: &str) -> FileSystemResult<bool> {
        let tree = self.0.read().expect("Poisoned Lock");
        if let Some(entry) = tree.get(path) {
            match entry {
                MemoryEntry::Directory(_) => Ok(true),
                _ => Ok(false),
            }
        } else {
            Ok(false)
        }
    }

    #[tracing::instrument]
    fn filesize(&self, path: &str) -> FileSystemResult<u64> {
        let tree = self.0.read().expect("Poisoned Lock");
        if let Some(entry) = tree.get(path) {
            match entry {
                MemoryEntry::File(file) => {
                    let data = file.0.read().expect("Poisoned Lock");
                    Ok(data.buffer.len() as u64)
                }
                _ => Err(FileSystemError::invalid_operation(
                    path,
                    "filesize on non-file",
                )),
            }
        } else {
            Err(FileSystemError::path_missing(path))
        }
    }

    #[tracing::instrument]
    fn create_directory(&self, path: &str) -> FileSystemResult<()> {
        let mut tree = self.0.write().expect("Poisoned Lock");
        if tree.contains_key(path) {
            Err(FileSystemError::path_exists(path))
        } else {
            tree.insert(
                path.to_string(),
                MemoryEntry::Directory(MemoryDirectoryEntry(Arc::new(RwLock::new(
                    MemoryDirectoryData(BTreeMap::new()),
                )))),
            );
            Ok(())
        }
    }

    #[tracing::instrument]
    fn create_directory_all(&self, path: &str) -> FileSystemResult<()> {
        let mut tree = self.0.write().expect("Poisoned Lock");
        if tree.contains_key(path) {
            Err(FileSystemError::path_exists(path))
        } else {
            // Create all parent directories
            let path_str = path.trim_start_matches('/');
            let parts: Vec<&str> = path_str.split('/').collect();

            for i in 0..parts.len() {
                let parent_path = format!("/{}", parts[..=i].join("/"));
                tree.entry(parent_path).or_insert_with(|| {
                    MemoryEntry::Directory(MemoryDirectoryEntry(Arc::new(RwLock::new(
                        MemoryDirectoryData(BTreeMap::new()),
                    ))))
                });
            }
            Ok(())
        }
    }

    #[tracing::instrument]
    fn list_directory<'a>(&self, path: &str) -> FileSystemResult<Vec<String>> {
        let tree = self.0.read().expect("Poisoned Lock");
        if let Some(entry) = tree.get(path) {
            match entry {
                MemoryEntry::Directory(dir) => {
                    let dir = dir.0.read().expect("Poisoned Lock");
                    Ok(dir.0.keys().cloned().collect())
                }
                _ => Err(FileSystemError::invalid_operation(
                    path,
                    "list_directory on non-directory",
                )),
            }
        } else {
            Err(FileSystemError::path_missing(path))
        }
    }

    #[tracing::instrument]
    fn remove_directory(&self, path: &str) -> FileSystemResult<()> {
        self.remove_directory_all(path)
    }

    #[tracing::instrument]
    fn remove_directory_all(&self, path: &str) -> FileSystemResult<()> {
        let mut tree = self.0.write().expect("Poisoned Lock");
        match tree.remove(path) {
            Some(_) => Ok(()),
            None => Err(FileSystemError::path_missing(path)),
        }
    }

    #[tracing::instrument]
    fn create_file(&self, path: &str) -> FileSystemResult<Self::File> {
        let mut tree = self.0.write().expect("Poisoned Lock");
        if tree.contains_key(path) {
            Err(FileSystemError::path_exists(path))
        } else {
            let inner = Arc::new(RwLock::new(MemoryFileData {
                buffer: Vec::default(),
                lock: FileLockMode::Unlocked,
            }));
            tree.insert(
                path.to_string(),
                MemoryEntry::File(MemoryFileEntry(inner.clone())),
            );
            Ok(MemoryFileHandle {
                cursor: 0,
                name: path.to_string(),
                data: inner.clone(),
            })
        }
    }

    #[tracing::instrument]
    fn open_file(&self, path: &str) -> FileSystemResult<Self::File> {
        if let Some(entry) = self.0.read().expect("Poisoned Lock").get(path) {
            match entry {
                MemoryEntry::File(file) => Ok(MemoryFileHandle {
                    cursor: 0,
                    name: path.to_string(),
                    data: file.0.clone(),
                }),
                _ => Err(FileSystemError::invalid_operation(
                    path,
                    "open_file on non-file",
                )),
            }
        } else {
            Err(FileSystemError::path_missing(path))
        }
    }

    #[tracing::instrument]
    fn remove_file(&self, path: &str) -> FileSystemResult<()> {
        if self.0.read().expect("Poisoned Lock").contains_key(path) {
            self.0.write().expect("Poisoned Lock").remove(path);
            Ok(())
        } else {
            Err(FileSystemError::path_missing(path))
        }
    }
}

#[derive(Clone, Debug)]
enum MemoryEntry {
    Directory(MemoryDirectoryEntry),
    File(MemoryFileEntry),
}

#[derive(Clone, Debug)]
struct MemoryDirectoryEntry(Arc<RwLock<MemoryDirectoryData>>);

#[derive(Clone, Debug)]
struct MemoryDirectoryData(BTreeMap<String, String>);

#[derive(Clone, Debug)]
pub struct MemoryFileEntry(Arc<RwLock<MemoryFileData>>);

#[derive(Clone)]
struct MemoryFileData {
    buffer: Vec<u8>,
    lock: FileLockMode,
}

impl std::fmt::Debug for MemoryFileData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MemoryFileData {{ data: {} bytes }}", self.buffer.len())
    }
}

/// Memory File Handle
#[derive(Clone)]
pub struct MemoryFileHandle {
    cursor: usize,
    name: String,
    data: Arc<RwLock<MemoryFileData>>,
}

impl std::fmt::Debug for MemoryFileHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MemoryFileHandle {{ cursor: {}, data: {:?} }}",
            self.cursor, self.data
        )
    }
}

impl Read for MemoryFileHandle {
    #[tracing::instrument]
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let data = self.data.read().unwrap();

        // Handle case where cursor is beyond file size
        if self.cursor >= data.buffer.len() {
            return Ok(0);
        }

        let len = std::cmp::min(buf.len(), data.buffer.len() - self.cursor);
        buf[..len].copy_from_slice(&data.buffer[self.cursor..self.cursor + len]);
        self.cursor += len;
        Ok(len)
    }
}

impl Write for MemoryFileHandle {
    #[tracing::instrument]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut data = self.data.write().unwrap();
        if self.cursor + buf.len() > data.buffer.len() {
            data.buffer.resize(self.cursor + buf.len(), 0);
        }
        data.buffer[self.cursor..self.cursor + buf.len()].copy_from_slice(buf);
        self.cursor += buf.len();
        Ok(buf.len())
    }

    #[tracing::instrument]
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for MemoryFileHandle {
    #[tracing::instrument]
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let data = self.data.read().expect("Poisoned Lock");
        match pos {
            SeekFrom::Start(offset) => {
                self.cursor = offset as usize;
            }
            SeekFrom::End(offset) => {
                self.cursor = (data.buffer.len() as i64 + offset) as usize;
            }
            SeekFrom::Current(offset) => {
                self.cursor = (self.cursor as i64 + offset) as usize;
            }
        }
        Ok(self.cursor as u64)
    }
}

impl File for MemoryFileHandle {
    type FileSystem = MemoryFileSystem;

    #[tracing::instrument]
    fn path(&self) -> &str {
        &self.name.as_str()
    }

    #[tracing::instrument]
    fn get_size(&self) -> FileSystemResult<u64> {
        let file = self.data.read().expect("Poisoned Lock");
        Ok(file.buffer.len() as u64)
    }

    #[tracing::instrument]
    fn set_size(&mut self, new_length: u64) -> FileSystemResult<()> {
        let mut file = self.data.write().expect("Poisoned Lock");
        file.buffer.resize(new_length as usize, 0);
        Ok(())
    }

    #[tracing::instrument]
    fn sync_all(&mut self) -> FileSystemResult<()> {
        Ok(())
    }

    #[tracing::instrument]
    fn sync_data(&mut self) -> FileSystemResult<()> {
        Ok(())
    }

    #[tracing::instrument]
    fn get_lock_status(&self) -> FileSystemResult<FileLockMode> {
        let file = self.data.write().expect("Poisoned Lock");
        Ok(file.lock)
    }

    #[tracing::instrument]
    fn set_lock_status(&mut self, mode: FileLockMode) -> FileSystemResult<()> {
        let mut file = self.data.write().expect("Poisoned Lock");
        file.lock = mode;
        Ok(())
    }

    #[tracing::instrument]
    fn read_at_offset(&mut self, pos: u64, buf: &mut [u8]) -> FileSystemResult<usize> {
        let data = self.data.read().expect("Poisoned Lock");

        // Calculate Slice Bounds with overflow protection
        let off = usize::try_from(pos).map_err(|_| {
            FileSystemError::InternalError("Position exceeds addressable memory".to_string())
        })?;

        // Handle case where offset is beyond file size
        if off >= data.buffer.len() {
            return Ok(0);
        }

        // Use saturating_add to prevent overflow, then clamp to buffer length
        let end = off.saturating_add(buf.len()).min(data.buffer.len());
        let len = end - off;

        // Read only the available bytes into the buffer
        buf[..len].copy_from_slice(&data.buffer[off..end]);

        Ok(len)
    }
    #[tracing::instrument]
    fn write_to_offset(&mut self, pos: u64, buf: &[u8]) -> FileSystemResult<usize> {
        let mut data = self.data.write().unwrap();

        // Calculate Slice Bounds with overflow protection
        let off = usize::try_from(pos).map_err(|_| {
            FileSystemError::InternalError("Position exceeds addressable memory".to_string())
        })?;

        // Use checked_add to detect overflow
        let end = off.checked_add(buf.len()).ok_or_else(|| {
            FileSystemError::InternalError(
                "Write operation would overflow address space".to_string(),
            )
        })?;

        // Resize if array capacity too small
        if end > data.buffer.len() {
            data.buffer.resize(end, 0);
        }

        // Write data to buffer
        data.buffer[off..end].copy_from_slice(buf);

        Ok(buf.len())
    }
}

#[cfg(test)]
mod test {
    use std::time::{SystemTime, UNIX_EPOCH};
    use tracing_test::traced_test;

    #[test]
    #[traced_test]
    fn test_memory_filesystem() {
        use super::{File, FileSystem, MemoryFileSystem};
        use std::io::{Read, Seek, SeekFrom, Write};

        let fs = MemoryFileSystem::new();
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
            assert_eq!(file.get_size().expect("Unable to get file size"), 8);

            // Seek to start and read full file
            let mut buf = Vec::new();
            file.seek(SeekFrom::Start(0)).expect("Error Seeking File");
            file.read_to_end(&mut buf).expect("Error Reading File");
            assert_eq!(buf, b"Goodbye!");
        }
        {
            // Open existing file and test
            let mut file = fs.open_file(filename.as_str()).unwrap();
            assert_eq!(file.get_size().unwrap(), 8);

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
