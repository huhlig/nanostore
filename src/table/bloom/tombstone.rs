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

//! Tombstone support for bloom filter rollback.
//!
//! This module implements tombstone-based undo for bloom filter operations,
//! enabling proper transaction rollback for append-only bloom filters.
//!
//! When a transaction that inserted keys into a bloom filter is rolled back,
//! those keys are marked with tombstones so they return false on contains()
//! checks, even though the bits remain set in the filter.

use crate::snap::Snapshot;
use crate::txn::TransactionId;
use crate::wal::LogSequenceNumber;
use std::collections::HashSet;

/// Tombstone for a rolled-back bloom filter insert.
///
/// When a transaction is rolled back, we mark the inserted keys as tombstoned
/// so they are filtered out during membership tests. This allows us to support
/// rollback for append-only bloom filters without physically clearing bits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct BloomTombstone {
    /// The key that was inserted and then rolled back
    pub key: Vec<u8>,
    /// Transaction ID that created this tombstone
    pub tombstone_tx_id: TransactionId,
    /// LSN when the tombstone was created (None = uncommitted)
    pub tombstone_lsn: Option<LogSequenceNumber>,
}

impl BloomTombstone {
    /// Create a new tombstone for a bloom filter key.
    pub fn new(key: Vec<u8>, tx_id: TransactionId) -> Self {
        Self {
            key,
            tombstone_tx_id: tx_id,
            tombstone_lsn: None,
        }
    }

    /// Mark the tombstone as committed.
    pub fn commit(&mut self, lsn: LogSequenceNumber) {
        self.tombstone_lsn = Some(lsn);
    }

    /// Check if this tombstone is visible to a snapshot.
    ///
    /// A tombstone is visible (hides the key) if:
    /// - The tombstone has been committed (tombstone_lsn.is_some())
    /// - AND the tombstone's LSN <= snapshot LSN
    pub fn is_visible(&self, snapshot: &Snapshot) -> bool {
        if let Some(lsn) = self.tombstone_lsn {
            lsn <= snapshot.lsn
        } else {
            false
        }
    }
}

/// Collection of tombstones for a bloom filter.
///
/// This structure manages tombstones efficiently, supporting:
/// - Fast lookup by key
/// - Commit operations
/// - Cleanup of old tombstones
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BloomTombstoneSet {
    /// Tombstones indexed by key for fast lookup
    tombstones: HashSet<BloomTombstone>,
}

impl BloomTombstoneSet {
    /// Create a new empty tombstone set.
    pub fn new() -> Self {
        Self {
            tombstones: HashSet::new(),
        }
    }

    /// Add a tombstone for a rolled-back key.
    pub fn add(&mut self, key: Vec<u8>, tx_id: TransactionId) {
        let tombstone = BloomTombstone::new(key, tx_id);
        self.tombstones.insert(tombstone);
    }

    /// Check if a key is tombstoned for a given snapshot.
    ///
    /// Returns true if the key has a visible tombstone, meaning it should
    /// be treated as not present in the bloom filter.
    pub fn is_tombstoned(&self, key: &[u8], snapshot: &Snapshot) -> bool {
        self.tombstones.iter().any(|t| t.key == key && t.is_visible(snapshot))
    }

    /// Commit all tombstones created by the given transaction.
    pub fn commit_tombstones(&mut self, tx_id: TransactionId, commit_lsn: LogSequenceNumber) {
        // We need to collect and replace tombstones since HashSet doesn't allow
        // in-place mutation of elements
        let mut updated = HashSet::new();
        for mut tombstone in self.tombstones.drain() {
            if tombstone.tombstone_tx_id == tx_id && tombstone.tombstone_lsn.is_none() {
                tombstone.commit(commit_lsn);
            }
            updated.insert(tombstone);
        }
        self.tombstones = updated;
    }

    /// Remove tombstones that are no longer needed.
    ///
    /// Tombstones can be removed when:
    /// - They are committed and older than the minimum visible LSN
    ///
    /// Returns the number of tombstones removed.
    pub fn vacuum(&mut self, min_visible_lsn: LogSequenceNumber) -> usize {
        let before = self.tombstones.len();
        self.tombstones.retain(|t| {
            if let Some(lsn) = t.tombstone_lsn {
                // Keep tombstones that might still be visible to active snapshots
                lsn > min_visible_lsn
            } else {
                // Keep uncommitted tombstones (shouldn't happen in practice)
                true
            }
        });
        before - self.tombstones.len()
    }

    /// Get the number of tombstones.
    pub fn len(&self) -> usize {
        self.tombstones.len()
    }

    /// Check if the tombstone set is empty.
    pub fn is_empty(&self) -> bool {
        self.tombstones.is_empty()
    }

    /// Clear all tombstones.
    pub fn clear(&mut self) {
        self.tombstones.clear();
    }

    /// Get an iterator over all tombstones.
    pub fn iter(&self) -> impl Iterator<Item = &BloomTombstone> {
        self.tombstones.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snap::SnapshotId;

    #[test]
    fn test_tombstone_visibility() {
        let mut tombstone = BloomTombstone::new(b"key1".to_vec(), TransactionId::from(1));
        let snapshot = Snapshot::new(
            SnapshotId::from(1),
            "test".to_string(),
            LogSequenceNumber::from(100),
            0,
            0,
            vec![],
        );

        // Uncommitted tombstone is not visible
        assert!(!tombstone.is_visible(&snapshot));

        // Commit the tombstone
        tombstone.commit(LogSequenceNumber::from(50));

        // Now it's visible (LSN 50 <= 100)
        assert!(tombstone.is_visible(&snapshot));

        // Not visible to older snapshot
        let old_snapshot = Snapshot::new(
            SnapshotId::from(2),
            "test".to_string(),
            LogSequenceNumber::from(40),
            0,
            0,
            vec![],
        );
        assert!(!tombstone.is_visible(&old_snapshot));
    }

    #[test]
    fn test_tombstone_set_operations() {
        let mut set = BloomTombstoneSet::new();
        let snapshot = Snapshot::new(
            SnapshotId::from(1),
            "test".to_string(),
            LogSequenceNumber::from(100),
            0,
            0,
            vec![],
        );

        // Add tombstones
        set.add(b"key1".to_vec(), TransactionId::from(1));
        set.add(b"key2".to_vec(), TransactionId::from(1));
        assert_eq!(set.len(), 2);

        // Keys are not tombstoned yet (uncommitted)
        assert!(!set.is_tombstoned(b"key1", &snapshot));
        assert!(!set.is_tombstoned(b"key2", &snapshot));

        // Commit tombstones
        set.commit_tombstones(TransactionId::from(1), LogSequenceNumber::from(50));

        // Now keys are tombstoned
        assert!(set.is_tombstoned(b"key1", &snapshot));
        assert!(set.is_tombstoned(b"key2", &snapshot));
        assert!(!set.is_tombstoned(b"key3", &snapshot));
    }

    #[test]
    fn test_tombstone_vacuum() {
        let mut set = BloomTombstoneSet::new();

        // Add and commit tombstones at different LSNs
        set.add(b"key1".to_vec(), TransactionId::from(1));
        set.add(b"key2".to_vec(), TransactionId::from(2));
        set.add(b"key3".to_vec(), TransactionId::from(3));

        set.commit_tombstones(TransactionId::from(1), LogSequenceNumber::from(50));
        set.commit_tombstones(TransactionId::from(2), LogSequenceNumber::from(100));
        set.commit_tombstones(TransactionId::from(3), LogSequenceNumber::from(150));

        assert_eq!(set.len(), 3);

        // Vacuum with min_visible_lsn = 75 should remove key1 (LSN 50)
        let removed = set.vacuum(LogSequenceNumber::from(75));
        assert_eq!(removed, 1);
        assert_eq!(set.len(), 2);

        // Vacuum with min_visible_lsn = 125 should remove key2 (LSN 100)
        let removed = set.vacuum(LogSequenceNumber::from(125));
        assert_eq!(removed, 1);
        assert_eq!(set.len(), 1);
    }
}

// Made with Bob
