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

//! Configuration for AppendLog table engine.

use std::time::Duration;

/// Configuration for AppendLog table engine.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppendLogConfig {
    /// Maximum size of a segment before rolling to a new one (in bytes).
    /// Default: 64 MB
    pub segment_size: u64,

    /// Compression type for segments.
    /// Default: None
    pub compression: CompressionType,

    /// Retention policy for old segments.
    /// Default: None (keep all segments)
    pub retention_policy: RetentionPolicy,

    /// Whether to sync writes to disk immediately.
    /// Default: false (buffer writes for better performance)
    pub sync_writes: bool,

    /// Buffer size for writes (in bytes).
    /// Default: 4 KB
    pub write_buffer_size: usize,
}

impl Default for AppendLogConfig {
    fn default() -> Self {
        Self {
            segment_size: 64 * 1024 * 1024, // 64 MB
            compression: CompressionType::None,
            retention_policy: RetentionPolicy::None,
            sync_writes: false,
            write_buffer_size: 4096, // 4 KB
        }
    }
}

impl AppendLogConfig {
    /// Create a new configuration with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the segment size.
    pub fn with_segment_size(mut self, size: u64) -> Self {
        self.segment_size = size;
        self
    }

    /// Set the compression type.
    pub fn with_compression(mut self, compression: CompressionType) -> Self {
        self.compression = compression;
        self
    }

    /// Set the retention policy.
    pub fn with_retention_policy(mut self, policy: RetentionPolicy) -> Self {
        self.retention_policy = policy;
        self
    }

    /// Enable or disable sync writes.
    pub fn with_sync_writes(mut self, sync: bool) -> Self {
        self.sync_writes = sync;
        self
    }

    /// Set the write buffer size.
    pub fn with_write_buffer_size(mut self, size: usize) -> Self {
        self.write_buffer_size = size;
        self
    }
}

/// Compression type for segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CompressionType {
    /// No compression
    None,
    /// LZ4 compression (fast, moderate compression)
    Lz4,
    /// Zstd compression (slower, better compression)
    Zstd,
    /// Snappy compression (very fast, light compression)
    Snappy,
}

/// Retention policy for old segments.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RetentionPolicy {
    /// Keep all segments (no retention)
    None,
    /// Keep at most N segments (oldest are removed first)
    MaxSegments(usize),
    /// Keep segments for at most the specified duration
    MaxAge(Duration),
}

impl RetentionPolicy {
    /// Create a policy to keep at most N segments.
    pub fn max_segments(n: usize) -> Self {
        Self::MaxSegments(n)
    }

    /// Create a policy to keep segments for at most the specified duration.
    pub fn max_age(duration: Duration) -> Self {
        Self::MaxAge(duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AppendLogConfig::default();
        assert_eq!(config.segment_size, 64 * 1024 * 1024);
        assert_eq!(config.compression, CompressionType::None);
        assert_eq!(config.retention_policy, RetentionPolicy::None);
        assert!(!config.sync_writes);
        assert_eq!(config.write_buffer_size, 4096);
    }

    #[test]
    fn test_config_builder() {
        let config = AppendLogConfig::new()
            .with_segment_size(128 * 1024 * 1024)
            .with_compression(CompressionType::Lz4)
            .with_retention_policy(RetentionPolicy::max_segments(10))
            .with_sync_writes(true)
            .with_write_buffer_size(8192);

        assert_eq!(config.segment_size, 128 * 1024 * 1024);
        assert_eq!(config.compression, CompressionType::Lz4);
        assert_eq!(config.retention_policy, RetentionPolicy::MaxSegments(10));
        assert!(config.sync_writes);
        assert_eq!(config.write_buffer_size, 8192);
    }

    #[test]
    fn test_retention_policy() {
        let policy1 = RetentionPolicy::max_segments(5);
        assert_eq!(policy1, RetentionPolicy::MaxSegments(5));

        let policy2 = RetentionPolicy::max_age(Duration::from_secs(3600));
        assert_eq!(policy2, RetentionPolicy::MaxAge(Duration::from_secs(3600)));
    }
}

// Made with Bob
