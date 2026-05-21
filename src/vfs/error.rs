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

/// Result Type for VFS Library
pub type FileSystemResult<T> = Result<T, FileSystemError>;

/// Error Type for VFS Library
#[derive(Debug)]
pub enum FileSystemError {
    /// Path is not valid in this `FileSystem`
    InvalidPath { path: String, reason: String },
    /// Attempt to create an object that already exists.
    PathExists { path: String },
    /// Path doesn't exist
    PathMissing { path: String },
    /// Parent directory missing
    ParentMissing { path: String },
    /// File Already Locked
    FileAlreadyLocked { path: String },
    /// Operation Disallowed
    PermissionDenied { path: String, operation: String },
    /// Already Locked
    AlreadyLocked { path: String },
    /// Operation Not supported on Path
    InvalidOperation { path: String, operation: String },
    /// Virtual File System doesn't support an operation.
    UnsupportedOperation { operation: String },
    /// Internal Error
    InternalError(String),
    /// IO Error
    IOError(std::io::Error),
    /// Wrapped Error
    WrappedError(Box<dyn std::error::Error>),
}

impl FileSystemError {
    /// Create a new IO Error from an IO Error
    #[must_use]
    pub fn io_error(err: std::io::Error) -> FileSystemError {
        FileSystemError::IOError(err)
    }

    /// Create a new Internal Error from a string
    #[must_use]
    pub fn internal_error(err: &str) -> FileSystemError {
        FileSystemError::InternalError(err.to_string())
    }

    /// Create a new Invalid Path error with context
    #[must_use]
    pub fn invalid_path(path: &str, reason: &str) -> FileSystemError {
        FileSystemError::InvalidPath {
            path: path.to_string(),
            reason: reason.to_string(),
        }
    }

    /// Create a new Path Missing error with context
    #[must_use]
    pub fn path_missing(path: &str) -> FileSystemError {
        FileSystemError::PathMissing {
            path: path.to_string(),
        }
    }

    /// Create a new Path Exists error with context
    #[must_use]
    pub fn path_exists(path: &str) -> FileSystemError {
        FileSystemError::PathExists {
            path: path.to_string(),
        }
    }

    /// Create a new Permission Denied error with context
    #[must_use]
    pub fn permission_denied(path: &str, operation: &str) -> FileSystemError {
        FileSystemError::PermissionDenied {
            path: path.to_string(),
            operation: operation.to_string(),
        }
    }

    /// Create a new Invalid Operation error with context
    #[must_use]
    pub fn invalid_operation(path: &str, operation: &str) -> FileSystemError {
        FileSystemError::InvalidOperation {
            path: path.to_string(),
            operation: operation.to_string(),
        }
    }

    /// Create a new Wrapper Error from an Error
    #[must_use]
    pub fn wrap_error<E: std::error::Error + 'static>(err: E) -> FileSystemError {
        FileSystemError::WrappedError(Box::new(err))
    }
}

impl std::fmt::Display for FileSystemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileSystemError::InvalidPath { path, reason } => {
                write!(f, "Invalid path '{}': {}", path, reason)
            }
            FileSystemError::PathExists { path } => {
                write!(f, "Path already exists: '{}'", path)
            }
            FileSystemError::PathMissing { path } => {
                write!(f, "Path not found: '{}'", path)
            }
            FileSystemError::ParentMissing { path } => {
                write!(f, "Parent directory missing for path: '{}'", path)
            }
            FileSystemError::FileAlreadyLocked { path } => {
                write!(f, "File already locked: '{}'", path)
            }
            FileSystemError::PermissionDenied { path, operation } => {
                write!(f, "Permission denied for {} on path: '{}'", operation, path)
            }
            FileSystemError::AlreadyLocked { path } => {
                write!(f, "Resource already locked: '{}'", path)
            }
            FileSystemError::InvalidOperation { path, operation } => {
                write!(f, "Invalid operation '{}' on path: '{}'", operation, path)
            }
            FileSystemError::UnsupportedOperation { operation } => {
                write!(f, "Unsupported operation: '{}'", operation)
            }
            FileSystemError::InternalError(msg) => {
                write!(f, "Internal error: {}", msg)
            }
            FileSystemError::IOError(err) => {
                write!(f, "I/O error: {}", err)
            }
            FileSystemError::WrappedError(err) => {
                write!(f, "Error: {}", err)
            }
        }
    }
}

impl std::error::Error for FileSystemError {}
