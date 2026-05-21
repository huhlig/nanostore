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

//! Configuration for `TimeSeries` table engine.

use std::time::Duration;

/// Configuration for `TimeSeries` table engine.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TimeSeriesConfig {
    /// Time bucket size for organizing data (in seconds).
    /// Data points are grouped into buckets based on their timestamp.
    /// Default: 3600 (1 hour)
    pub bucket_size: u64,

    /// Compression type for time series data.
    /// Default: `DeltaOfDelta` (efficient for time series)
    pub compression: TimeSeriesCompression,

    /// Retention policy for old data.
    /// Default: None (keep all data)
    pub retention_policy: TimeSeriesRetentionPolicy,

    /// Whether to enable downsampling for old data.
    /// Default: false
    pub enable_downsampling: bool,

    /// Downsampling interval (in seconds).
    /// Only used if `enable_downsampling` is true.
    /// Default: 86400 (1 day)
    pub downsampling_interval: u64,

    /// Maximum number of data points per bucket before splitting.
    /// Default: 10000
    pub max_points_per_bucket: usize,

    /// Whether to maintain an in-memory index for fast lookups.
    /// Default: true
    pub use_memory_index: bool,
}

impl Default for TimeSeriesConfig {
    fn default() -> Self {
        Self {
            bucket_size: 3600, // 1 hour
            compression: TimeSeriesCompression::DeltaOfDelta,
            retention_policy: TimeSeriesRetentionPolicy::None,
            enable_downsampling: false,
            downsampling_interval: 86400, // 1 day
            max_points_per_bucket: 10000,
            use_memory_index: true,
        }
    }
}

impl TimeSeriesConfig {
    /// Create a new configuration with default values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the bucket size.
    #[must_use]
    pub fn with_bucket_size(mut self, size: u64) -> Self {
        self.bucket_size = size;
        self
    }

    /// Set the compression type.
    #[must_use]
    pub fn with_compression(mut self, compression: TimeSeriesCompression) -> Self {
        self.compression = compression;
        self
    }

    /// Set the retention policy.
    #[must_use]
    pub fn with_retention_policy(mut self, policy: TimeSeriesRetentionPolicy) -> Self {
        self.retention_policy = policy;
        self
    }

    /// Enable or disable downsampling.
    #[must_use]
    pub fn with_downsampling(mut self, enable: bool, interval: u64) -> Self {
        self.enable_downsampling = enable;
        self.downsampling_interval = interval;
        self
    }

    /// Set the maximum points per bucket.
    #[must_use]
    pub fn with_max_points_per_bucket(mut self, max: usize) -> Self {
        self.max_points_per_bucket = max;
        self
    }

    /// Enable or disable memory index.
    #[must_use]
    pub fn with_memory_index(mut self, use_index: bool) -> Self {
        self.use_memory_index = use_index;
        self
    }
}

/// Compression type for time series data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TimeSeriesCompression {
    /// No compression
    None,
    /// Delta-of-delta encoding (efficient for timestamps)
    DeltaOfDelta,
    /// Gorilla compression (Facebook's time series compression)
    Gorilla,
    /// Simple delta encoding
    Delta,
    /// Run-length encoding
    Rle,
}

/// Retention policy for time series data.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TimeSeriesRetentionPolicy {
    /// Keep all data (no retention)
    None,
    /// Keep data for at most the specified duration
    MaxAge(Duration),
    /// Keep at most N data points per series
    MaxPoints(usize),
    /// Keep data until a specific timestamp
    UntilTimestamp(i64),
}

impl TimeSeriesRetentionPolicy {
    /// Create a policy to keep data for at most the specified duration.
    pub fn max_age(duration: Duration) -> Self {
        Self::MaxAge(duration)
    }

    /// Create a policy to keep at most N data points per series.
    pub fn max_points(n: usize) -> Self {
        Self::MaxPoints(n)
    }

    /// Create a policy to keep data until a specific timestamp.
    pub fn until_timestamp(ts: i64) -> Self {
        Self::UntilTimestamp(ts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = TimeSeriesConfig::default();
        assert_eq!(config.bucket_size, 3600);
        assert_eq!(config.compression, TimeSeriesCompression::DeltaOfDelta);
        assert_eq!(config.retention_policy, TimeSeriesRetentionPolicy::None);
        assert!(!config.enable_downsampling);
        assert_eq!(config.downsampling_interval, 86400);
        assert_eq!(config.max_points_per_bucket, 10000);
        assert!(config.use_memory_index);
    }

    #[test]
    fn test_config_builder() {
        let config = TimeSeriesConfig::new()
            .with_bucket_size(7200)
            .with_compression(TimeSeriesCompression::Gorilla)
            .with_retention_policy(TimeSeriesRetentionPolicy::max_age(Duration::from_secs(
                86400 * 30,
            )))
            .with_downsampling(true, 3600)
            .with_max_points_per_bucket(5000)
            .with_memory_index(false);

        assert_eq!(config.bucket_size, 7200);
        assert_eq!(config.compression, TimeSeriesCompression::Gorilla);
        assert_eq!(
            config.retention_policy,
            TimeSeriesRetentionPolicy::MaxAge(Duration::from_secs(86400 * 30))
        );
        assert!(config.enable_downsampling);
        assert_eq!(config.downsampling_interval, 3600);
        assert_eq!(config.max_points_per_bucket, 5000);
        assert!(!config.use_memory_index);
    }

    #[test]
    fn test_retention_policy() {
        let policy1 = TimeSeriesRetentionPolicy::max_age(Duration::from_secs(3600));
        assert_eq!(
            policy1,
            TimeSeriesRetentionPolicy::MaxAge(Duration::from_secs(3600))
        );

        let policy2 = TimeSeriesRetentionPolicy::max_points(1000);
        assert_eq!(policy2, TimeSeriesRetentionPolicy::MaxPoints(1000));

        let policy3 = TimeSeriesRetentionPolicy::until_timestamp(1_234_567_890);
        assert_eq!(
            policy3,
            TimeSeriesRetentionPolicy::UntilTimestamp(1_234_567_890)
        );
    }
}

// Made with Bob
