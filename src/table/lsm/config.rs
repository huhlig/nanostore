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

//! LSM tree configuration and tuning parameters.
//!
//! This module defines configuration options for the LSM tree storage engine,
//! including memtable size, compaction strategy, bloom filter settings, and
//! level-specific parameters.

use crate::types::{CompressionKind, EncryptionKind};

/// LSM tree configuration.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct LsmConfig {
    /// Memtable configuration
    pub memtable: MemtableConfig,

    /// SSTable configuration
    pub sstable: SStableConfig,

    /// Compaction configuration
    pub compaction: CompactionConfig,

    /// Bloom filter configuration
    pub bloom_filter: BloomFilterConfig,

    /// Block cache configuration
    pub block_cache: BlockCacheConfig,
}

/// Memtable configuration.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MemtableConfig {
    /// Maximum memtable size in bytes before flush (default: 64MB)
    pub max_size: usize,

    /// Maximum number of immutable memtables to keep (default: 2)
    pub max_immutable_count: usize,

    /// Memtable implementation type
    pub implementation: MemtableType,
}

impl Default for MemtableConfig {
    fn default() -> Self {
        Self {
            max_size: 64 * 1024 * 1024, // 64MB
            max_immutable_count: 2,
            implementation: MemtableType::SkipList,
        }
    }
}

/// Memtable implementation type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MemtableType {
    /// Skip list based (better for concurrent writes)
    SkipList,
    /// B-tree based (better for sequential writes)
    BTree,
}

/// SSTable configuration.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SStableConfig {
    /// Target SSTable size in bytes (default: 2MB)
    pub target_size: usize,

    /// Block size in bytes (default: 4KB)
    pub block_size: usize,

    /// Compression algorithm (default: None)
    pub compression: Option<CompressionKind>,

    /// Encryption algorithm (default: None)
    pub encryption: Option<EncryptionKind>,

    /// Enable checksums for data blocks (default: true)
    pub enable_checksums: bool,

    /// Index block interval (keys per index entry, default: 16)
    pub index_interval: usize,
}

impl Default for SStableConfig {
    fn default() -> Self {
        Self {
            target_size: 2 * 1024 * 1024, // 2MB
            block_size: 4 * 1024,         // 4KB
            compression: None,
            encryption: None,
            enable_checksums: true,
            index_interval: 16,
        }
    }
}

/// Compaction configuration.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CompactionConfig {
    /// Compaction strategy
    pub strategy: CompactionStrategy,

    /// Maximum number of concurrent compaction threads (default: 1)
    pub max_threads: usize,

    /// Level-specific configuration
    pub levels: Vec<LevelConfig>,

    /// Minimum number of SSTables to trigger compaction (default: 4)
    pub min_merge_width: usize,

    /// Maximum number of SSTables to merge at once (default: 10)
    pub max_merge_width: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            strategy: CompactionStrategy::Leveled,
            max_threads: 1,
            levels: Self::default_level_configs(),
            min_merge_width: 4,
            max_merge_width: 10,
        }
    }
}

impl CompactionConfig {
    /// Create default level configurations (7 levels with exponential growth).
    fn default_level_configs() -> Vec<LevelConfig> {
        vec![
            LevelConfig {
                level: 0,
                max_size: 10 * 1024 * 1024, // 10MB (L0 is special)
                max_files: 4,
                target_file_size: 2 * 1024 * 1024, // 2MB
            },
            LevelConfig {
                level: 1,
                max_size: 10 * 1024 * 1024, // 10MB
                max_files: 10,
                target_file_size: 2 * 1024 * 1024, // 2MB
            },
            LevelConfig {
                level: 2,
                max_size: 100 * 1024 * 1024, // 100MB
                max_files: 100,
                target_file_size: 2 * 1024 * 1024, // 2MB
            },
            LevelConfig {
                level: 3,
                max_size: 1024 * 1024 * 1024, // 1GB
                max_files: 1000,
                target_file_size: 2 * 1024 * 1024, // 2MB
            },
            LevelConfig {
                level: 4,
                max_size: 10 * 1024 * 1024 * 1024, // 10GB
                max_files: 10000,
                target_file_size: 2 * 1024 * 1024, // 2MB
            },
            LevelConfig {
                level: 5,
                max_size: 100 * 1024 * 1024 * 1024, // 100GB
                max_files: 100000,
                target_file_size: 2 * 1024 * 1024, // 2MB
            },
            LevelConfig {
                level: 6,
                max_size: 1024 * 1024 * 1024 * 1024, // 1TB
                max_files: 1000000,
                target_file_size: 2 * 1024 * 1024, // 2MB
            },
        ]
    }
}

/// Compaction strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CompactionStrategy {
    /// Leveled compaction (RocksDB-style)
    /// - Each level has a size limit
    /// - SSTables in each level (except L0) are non-overlapping
    /// - Compaction merges SSTables from level N to N+1
    Leveled,

    /// Size-tiered compaction (Cassandra-style)
    /// - Group SSTables by size
    /// - Merge similar-sized SSTables
    /// - Better for write-heavy workloads
    SizeTiered,

    /// Universal compaction
    /// - Simpler strategy with fewer levels
    /// - Good for small datasets
    Universal,
}

/// Per-level configuration.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LevelConfig {
    /// Level number (0 = L0, 1 = L1, etc.)
    pub level: u32,

    /// Maximum total size for this level in bytes
    pub max_size: u64,

    /// Maximum number of SSTables in this level
    pub max_files: usize,

    /// Target size for SSTables in this level
    pub target_file_size: usize,
}

/// Bloom filter configuration.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BloomFilterConfig {
    /// Enable bloom filters (default: true)
    pub enabled: bool,

    /// Bits per key (default: 10, ~1% false positive rate)
    pub bits_per_key: usize,

    /// Number of hash functions (default: calculated from bits_per_key)
    pub num_hash_functions: Option<usize>,
}

impl Default for BloomFilterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bits_per_key: 10,
            num_hash_functions: None, // Auto-calculate
        }
    }
}

impl BloomFilterConfig {
    /// Calculate optimal number of hash functions.
    pub fn optimal_hash_functions(&self) -> usize {
        if let Some(n) = self.num_hash_functions {
            n
        } else {
            // k = (m/n) * ln(2), where m = bits_per_key, n = 1
            ((self.bits_per_key as f64) * 0.693).ceil() as usize
        }
    }

    /// Calculate expected false positive rate.
    pub fn false_positive_rate(&self) -> f64 {
        let k = self.optimal_hash_functions() as f64;
        let m = self.bits_per_key as f64;
        // FPR = (1 - e^(-k/m))^k
        (1.0 - (-k / m).exp()).powf(k)
    }
}

/// Block cache configuration.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BlockCacheConfig {
    /// Enable block cache (default: true)
    pub enabled: bool,

    /// Maximum cache size in bytes (default: 8MB)
    pub max_size: usize,

    /// Cache eviction policy
    pub eviction_policy: CacheEvictionPolicy,
}

impl Default for BlockCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_size: 8 * 1024 * 1024, // 8MB
            eviction_policy: CacheEvictionPolicy::Lru,
        }
    }
}

/// Cache eviction policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CacheEvictionPolicy {
    /// Least Recently Used
    Lru,
    /// Least Frequently Used
    Lfu,
    /// First In First Out
    Fifo,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = LsmConfig::default();
        assert_eq!(config.memtable.max_size, 64 * 1024 * 1024);
        assert_eq!(config.sstable.target_size, 2 * 1024 * 1024);
        assert_eq!(config.compaction.strategy, CompactionStrategy::Leveled);
        assert!(config.bloom_filter.enabled);
    }

    #[test]
    fn test_bloom_filter_calculations() {
        let config = BloomFilterConfig::default();
        let k = config.optimal_hash_functions();
        assert!(k > 0 && k < 20); // Reasonable range

        let fpr = config.false_positive_rate();
        assert!(fpr > 0.0 && fpr < 0.1); // Should be less than 10%
    }

    #[test]
    fn test_level_configs() {
        let config = CompactionConfig::default();
        assert_eq!(config.levels.len(), 7);

        // Verify exponential growth (starting from L2, since L0 and L1 are special)
        for i in 2..config.levels.len() {
            assert!(config.levels[i].max_size > config.levels[i - 1].max_size);
        }

        // Verify L0 and L1 have same size (both 10MB)
        assert_eq!(config.levels[0].max_size, 10 * 1024 * 1024);
        assert_eq!(config.levels[1].max_size, 10 * 1024 * 1024);
    }
}

// Made with Bob
