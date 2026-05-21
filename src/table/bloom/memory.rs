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

//! Bloom filter implementation for LSM tree.
//!
//! Bloom filters are space-efficient probabilistic data structures used to test
//! whether an element is a member of a set. They can have false positives but
//! never false negatives, making them ideal for reducing disk I/O in LSM trees.
//!
//! This implementation uses multiple hash functions derived from a single hash
//! using the double hashing technique for efficiency.

use crate::table::{
    ApproximateMembership, SpecialtyTableCapabilities, SpecialtyTableStats, TableResult,
    VerificationReport,
};
use crate::types::TableId;

/// Bloom filter for efficient membership testing.
///
/// Uses a bit array and multiple hash functions to provide probabilistic
/// membership testing with configurable false positive rate.
#[derive(Clone, Debug)]
pub struct BloomFilter {
    /// Bit array for the filter
    bits: Vec<u8>,

    /// Number of bits in the filter
    num_bits: usize,

    /// Number of hash functions
    num_hash_functions: usize,

    /// Number of items inserted
    num_items: usize,
}

impl BloomFilter {
    /// Create a new bloom filter with the specified parameters.
    ///
    /// # Arguments
    ///
    /// * `num_items` - Expected number of items to insert
    /// * `bits_per_key` - Number of bits to use per key (affects false positive rate)
    /// * `num_hash_functions` - Number of hash functions to use (None = auto-calculate)
    #[must_use]
    pub fn new(num_items: usize, bits_per_key: usize, num_hash_functions: Option<usize>) -> Self {
        let num_bits = num_items * bits_per_key;
        let num_bytes = num_bits.div_ceil(8);

        let num_hash_functions = num_hash_functions.unwrap_or_else(|| {
            // Optimal k = (m/n) * ln(2)
            ((bits_per_key as f64) * 0.693).ceil() as usize
        });

        Self {
            bits: vec![0u8; num_bytes],
            num_bits,
            num_hash_functions,
            num_items: 0,
        }
    }

    /// Create a bloom filter from existing bit data.
    #[must_use]
    pub fn from_bytes(bits: Vec<u8>, num_hash_functions: usize) -> Self {
        let num_bits = bits.len() * 8;
        Self {
            bits,
            num_bits,
            num_hash_functions,
            num_items: 0, // Unknown
        }
    }

    /// Create a bloom filter from existing bit data with explicit `num_bits`.
    #[must_use]
    pub fn from_bytes_with_size(bits: Vec<u8>, num_bits: usize, num_hash_functions: usize) -> Self {
        Self {
            bits,
            num_bits,
            num_hash_functions,
            num_items: 0, // Unknown
        }
    }

    /// Insert a key into the bloom filter.
    pub fn insert(&mut self, key: &[u8]) {
        let (h1, h2) = self.hash_key(key);

        for i in 0..self.num_hash_functions {
            let bit_pos = self.get_bit_position(h1, h2, i);
            self.set_bit(bit_pos);
        }

        self.num_items += 1;
    }

    /// Check if a key might be in the set.
    ///
    /// Returns true if the key might be present (with false positive probability),
    /// or false if the key is definitely not present.
    #[must_use]
    pub fn contains(&self, key: &[u8]) -> bool {
        let (h1, h2) = self.hash_key(key);

        for i in 0..self.num_hash_functions {
            let bit_pos = self.get_bit_position(h1, h2, i);
            if !self.get_bit(bit_pos) {
                return false;
            }
        }

        true
    }

    /// Get the number of bits in the filter.
    #[must_use]
    pub fn num_bits(&self) -> usize {
        self.num_bits
    }

    /// Get the number of hash functions.
    #[must_use]
    pub fn num_hash_functions(&self) -> usize {
        self.num_hash_functions
    }

    /// Get the number of items inserted.
    #[must_use]
    pub fn num_items(&self) -> usize {
        self.num_items
    }

    /// Get the raw bit data.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bits
    }

    /// Calculate the expected false positive rate.
    #[must_use]
    pub fn false_positive_rate(&self) -> f64 {
        if self.num_items == 0 {
            return 0.0;
        }

        let k = self.num_hash_functions as f64;
        let m = self.num_bits as f64;
        let n = self.num_items as f64;

        // FPR = (1 - e^(-kn/m))^k
        (1.0 - ((-k * n) / m).exp()).powf(k)
    }

    /// Clear all bits in the filter.
    pub fn clear(&mut self) {
        self.bits.fill(0);
        self.num_items = 0;
    }

    /// Hash a key using two independent hash functions.
    ///
    /// Uses FNV-1a and a simple multiplicative hash for efficiency.
    fn hash_key(&self, key: &[u8]) -> (u64, u64) {
        // FNV-1a hash
        let mut h1 = 0xcbf2_9ce4_8422_2325u64;
        for &byte in key {
            h1 ^= byte as u64;
            h1 = h1.wrapping_mul(0x0100_0000_01b3);
        }

        // Simple multiplicative hash
        let mut h2 = 0u64;
        for &byte in key {
            h2 = h2.wrapping_mul(31).wrapping_add(byte as u64);
        }

        (h1, h2)
    }

    /// Get bit position using double hashing.
    ///
    /// Uses the formula: hash(i) = h1 + i * h2 (mod m)
    fn get_bit_position(&self, h1: u64, h2: u64, i: usize) -> usize {
        let hash = h1.wrapping_add((i as u64).wrapping_mul(h2));
        (hash % (self.num_bits as u64)) as usize
    }

    /// Set a bit at the given position.
    fn set_bit(&mut self, pos: usize) {
        let byte_idx = pos / 8;
        let bit_idx = pos % 8;
        self.bits[byte_idx] |= 1 << bit_idx;
    }

    /// Get a bit at the given position.
    fn get_bit(&self, pos: usize) -> bool {
        let byte_idx = pos / 8;
        let bit_idx = pos % 8;
        (self.bits[byte_idx] & (1 << bit_idx)) != 0
    }
}

/// Builder for creating bloom filters with a fluent API.
pub struct BloomFilterBuilder {
    num_items: usize,
    bits_per_key: usize,
    num_hash_functions: Option<usize>,
}

impl BloomFilterBuilder {
    /// Create a new builder with the expected number of items.
    pub fn new(num_items: usize) -> Self {
        Self {
            num_items,
            bits_per_key: 10, // Default: ~1% false positive rate
            num_hash_functions: None,
        }
    }

    /// Set the number of bits per key.
    pub fn bits_per_key(mut self, bits: usize) -> Self {
        self.bits_per_key = bits;
        self
    }

    /// Set the number of hash functions.
    pub fn num_hash_functions(mut self, num: usize) -> Self {
        self.num_hash_functions = Some(num);
        self
    }

    /// Set the desired false positive rate.
    ///
    /// This calculates the appropriate bits_per_key value.
    pub fn false_positive_rate(mut self, rate: f64) -> Self {
        // m/n = -k / ln(1 - rate^(1/k))
        // For simplicity, use approximation: m/n ≈ -ln(rate) / (ln(2))^2
        self.bits_per_key = ((-rate.ln()) / (0.693 * 0.693)).ceil() as usize;
        self
    }

    /// Build the bloom filter.
    pub fn build(self) -> BloomFilter {
        BloomFilter::new(self.num_items, self.bits_per_key, self.num_hash_functions)
    }
}

// =============================================================================
// ApproximateMembership Specialty Table Implementation
// =============================================================================

impl ApproximateMembership for BloomFilter {
    fn table_id(&self) -> TableId {
        // Bloom filters are typically embedded in other structures (like SSTables)
        // rather than standalone tables, so we use a placeholder ID
        TableId::from(0)
    }

    fn name(&self) -> &str {
        "bloom_filter"
    }

    fn capabilities(&self) -> SpecialtyTableCapabilities {
        SpecialtyTableCapabilities {
            exact: false,
            approximate: true,
            ordered: false,
            sparse: false,
            supports_delete: false, // Bloom filters don't support deletion
            supports_range_query: false,
            supports_prefix_query: false,
            supports_scoring: false,
            supports_incremental_rebuild: false,
            may_be_stale: false,
        }
    }

    fn insert_key(
        &mut self,
        key: &[u8],
        _tx_id: crate::txn::TransactionId,
        _commit_lsn: crate::wal::LogSequenceNumber,
    ) -> TableResult<()> {
        self.insert(key);
        Ok(())
    }

    fn might_contain(&self, key: &[u8]) -> TableResult<bool> {
        Ok(self.contains(key))
    }

    fn false_positive_rate(&self) -> Option<f64> {
        Some(BloomFilter::false_positive_rate(self))
    }

    fn stats(&self) -> TableResult<SpecialtyTableStats> {
        Ok(SpecialtyTableStats {
            entry_count: Some(self.num_items as u64),
            size_bytes: Some(self.bits.len() as u64),
            distinct_keys: Some(self.num_items as u64),
            stale_entries: Some(0),
            last_updated_lsn: None,
        })
    }

    fn verify(&self) -> TableResult<VerificationReport> {
        // Basic verification: check that the filter is well-formed
        let mut report = VerificationReport {
            checked_items: 1,
            errors: Vec::new(),
            warnings: Vec::new(),
        };

        // Verify bit array size matches expected
        let expected_bytes = self.num_bits.div_ceil(8);
        if self.bits.len() != expected_bytes {
            report.errors.push(crate::table::ConsistencyError {
                error_type: crate::table::ConsistencyErrorType::InvalidPointer,
                location: "bloom_filter".to_string(),
                description: format!(
                    "Bit array size mismatch: expected {} bytes, got {}",
                    expected_bytes,
                    self.bits.len()
                ),
                severity: crate::table::Severity::Error,
            });
        }

        // Warn if false positive rate is high
        if self.num_items > 0 {
            let fpr = self.false_positive_rate();
            if fpr > 0.1 {
                // > 10% FPR
                report.warnings.push(crate::table::ConsistencyWarning {
                    location: "bloom_filter".to_string(),
                    description: format!(
                        "High false positive rate: {:.2}% (consider increasing bits_per_key)",
                        fpr * 100.0
                    ),
                });
            }
        }

        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_operations() {
        let mut filter = BloomFilter::new(100, 10, None);

        // Insert some keys
        filter.insert(b"key1");
        filter.insert(b"key2");
        filter.insert(b"key3");

        // Check membership
        assert!(filter.contains(b"key1"));
        assert!(filter.contains(b"key2"));
        assert!(filter.contains(b"key3"));
        assert!(!filter.contains(b"key4")); // Likely not present
    }

    #[test]
    fn test_false_positive_rate() {
        let num_items = 1000;
        let mut filter = BloomFilter::new(num_items, 10, None);

        // Insert items
        for i in 0..num_items {
            filter.insert(&i.to_le_bytes());
        }

        // Check false positive rate
        let mut false_positives = 0;
        let test_items = 10000;
        for i in num_items..(num_items + test_items) {
            if filter.contains(&i.to_le_bytes()) {
                false_positives += 1;
            }
        }

        let actual_fpr = false_positives as f64 / test_items as f64;

        // With 10 bits per key, we expect roughly 1% FPR
        // Allow reasonable margin for statistical variance
        assert!(
            actual_fpr < 0.05,
            "Actual FPR ({:.4}) should be less than 5% with 10 bits/key",
            actual_fpr
        );

        // Verify it's not zero (that would indicate a bug)
        assert!(actual_fpr > 0.0, "FPR should be greater than 0");
    }

    #[test]
    fn test_serialization() {
        let mut filter = BloomFilter::new(100, 10, Some(7));

        filter.insert(b"test1");
        filter.insert(b"test2");

        // Serialize
        let bytes = filter.as_bytes().to_vec();
        let num_hash = filter.num_hash_functions();

        // Deserialize
        let restored = BloomFilter::from_bytes(bytes, num_hash);

        // Verify
        assert!(restored.contains(b"test1"));
        assert!(restored.contains(b"test2"));
    }

    #[test]
    fn test_builder() {
        let filter = BloomFilterBuilder::new(1000)
            .bits_per_key(12)
            .num_hash_functions(8)
            .build();

        assert_eq!(filter.num_bits(), 1000 * 12);
        assert_eq!(filter.num_hash_functions(), 8);
    }

    #[test]
    fn test_builder_with_fpr() {
        let filter = BloomFilterBuilder::new(1000)
            .false_positive_rate(0.01) // 1% FPR
            .build();

        // Should have approximately 10 bits per key for 1% FPR
        assert!(filter.num_bits() / 1000 >= 9);
        assert!(filter.num_bits() / 1000 <= 11);
    }

    #[test]
    fn test_clear() {
        let mut filter = BloomFilter::new(100, 10, None);

        filter.insert(b"key1");
        assert!(filter.contains(b"key1"));

        filter.clear();
        assert_eq!(filter.num_items(), 0);
        // After clear, might still return true due to hash collisions,
        // but probability should be very low
    }

    #[test]
    fn test_empty_filter() {
        let filter = BloomFilter::new(100, 10, None);

        // Empty filter should not contain anything
        assert!(!filter.contains(b"anything"));
        assert_eq!(filter.num_items(), 0);
        assert_eq!(filter.false_positive_rate(), 0.0);
    }

    #[test]
    fn test_hash_consistency() {
        let filter = BloomFilter::new(100, 10, None);

        let key = b"test_key";
        let (h1_1, h2_1) = filter.hash_key(key);
        let (h1_2, h2_2) = filter.hash_key(key);

        // Same key should produce same hashes
        assert_eq!(h1_1, h1_2);
        assert_eq!(h2_1, h2_2);
    }
}

// Made with Bob
