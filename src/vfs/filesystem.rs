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

use crate::vfs::{FileSystemError, FileSystemResult};
use std::io::{Read, Seek, SeekFrom, Write};

/// API definition all `KBase` [`FileSystem`] implementations must adhere to.
pub trait FileSystem: std::fmt::Debug + Sized + Sync + Send + 'static {
    /// Type of File Returned by this Virtual File System
    type File: File<FileSystem = Self>;

    /// Check if an entry exists at the provided path.
    fn exists(&self, path: &str) -> FileSystemResult<bool>;
    /// See if an entry at the path is a file.
    fn is_file(&self, path: &str) -> FileSystemResult<bool>;
    /// See if an entry at the path is a folder.
    fn is_directory(&self, path: &str) -> FileSystemResult<bool>;
    /// Get file or directory size.
    fn filesize(&self, path: &str) -> FileSystemResult<u64>;
    /// Creates a new, empty folder entry at the provided path.
    fn create_directory(&self, path: &str) -> FileSystemResult<()>;
    /// Creates a new, empty folder entry at the provided path.
    fn create_directory_all(&self, path: &str) -> FileSystemResult<()>;
    /// Returns an iterator over the names of entries within a Folder.
    fn list_directory(&self, path: &str) -> FileSystemResult<Vec<String>>;
    /// Removes the folder at this path.
    fn remove_directory(&self, path: &str) -> FileSystemResult<()>;
    /// Removes the folder at this path and all children.
    fn remove_directory_all(&self, path: &str) -> FileSystemResult<()>;
    /// Create or Open a new append only file for writing.
    fn create_file(&self, path: &str) -> FileSystemResult<Self::File>;
    /// Create or Open a new append only file for writing.
    fn open_file(&self, path: &str) -> FileSystemResult<Self::File>;
    /// Removes the file at this path
    fn remove_file(&self, path: &str) -> FileSystemResult<()>;
}

/// Handle for File Access
pub trait File: std::fmt::Debug + Sized + Read + Write + Seek + Sync + Send + 'static {
    /// `FileSystem` owning this File
    type FileSystem: FileSystem<File = Self>;

    /// Path to this File
    fn path(&self) -> &str;
    /// Get File Size
    fn get_size(&self) -> FileSystemResult<u64>;
    /// Set File Length
    fn set_size(&mut self, new_size: u64) -> FileSystemResult<()>;
    /// Flushes all data and metadata to storage.
    fn sync_all(&mut self) -> FileSystemResult<()>;
    /// Flush all data to storage.
    fn sync_data(&mut self) -> FileSystemResult<()>;
    /// Get Advisory Lock Status of this file
    fn get_lock_status(&self) -> FileSystemResult<FileLockMode>;
    /// Apply or Clear Advisory Lock of this File
    fn set_lock_status(&mut self, mode: FileLockMode) -> FileSystemResult<()>;
    /// Write directly to a location without modifying cursor.
    fn read_at_offset(&mut self, offset: u64, buffer: &mut [u8]) -> FileSystemResult<usize> {
        let pos = self.stream_position().map_err(FileSystemError::io_error)?;
        self.seek(SeekFrom::Start(offset))
            .map_err(FileSystemError::io_error)?;
        let rv = self.read(buffer).map_err(FileSystemError::io_error)?;
        self.seek(SeekFrom::Start(pos))
            .map_err(FileSystemError::io_error)?;
        Ok(rv)
    }
    /// Write directly to a location without modifying cursor.
    fn write_to_offset(&mut self, offset: u64, buffer: &[u8]) -> FileSystemResult<usize> {
        let pos = self.stream_position().map_err(FileSystemError::io_error)?;
        self.seek(SeekFrom::Start(offset))
            .map_err(FileSystemError::io_error)?;
        let rv = self.write(buffer).map_err(FileSystemError::io_error)?;
        self.seek(SeekFrom::Start(pos))
            .map_err(FileSystemError::io_error)?;
        Ok(rv)
    }

    /// Truncate a file
    fn truncate(&mut self) -> FileSystemResult<()> {
        self.set_size(0)
    }
}

/// An enumeration of types which represents the state of an advisory lock.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum FileLockMode {
    /// ## UNLOCKED
    Unlocked,
    /// ## SHARED
    Shared,
    /// ## EXCLUSIVE
    Exclusive,
}
