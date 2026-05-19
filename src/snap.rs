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

use crate::txn::TransactionId;
use crate::wal::LogSequenceNumber;
use std::collections::HashSet;
use std::fmt::Formatter;

/// Snapshot identifier.
#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct SnapshotId(u64);

impl From<u64> for SnapshotId {
    fn from(value: u64) -> Self {
        SnapshotId(value)
    }
}

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "SnapshotId({})", self.0)
    }
}

/// A named, persistent snapshot of the database at a specific LSN.
///
/// Snapshots enable point-in-time queries, backups, and long-running analytics
/// without blocking writers. They pin the necessary pages/segments in memory
/// or on disk until explicitly released.
///
/// # Lifecycle
///
/// 1. Create snapshot with [`KvDatabase::create_snapshot`]
/// 2. Use snapshot LSN to open read transactions
/// 3. Release snapshot with [`KvDatabase::release_snapshot`] when done
///
/// # Examples
///
/// ```ignore
/// // Create a snapshot for backup
/// let snapshot = db.create_snapshot("backup-2024")?;
///
/// // Use the snapshot LSN for consistent reads
/// let tx = db.begin_read_at(snapshot.lsn)?;
/// // ... perform backup operations ...
///
/// // Release when done
/// db.release_snapshot(snapshot.id)?;
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Unique snapshot identifier.
    pub id: SnapshotId,
    /// User-provided name for the snapshot.
    pub name: String,
    /// LSN at which the snapshot was taken.
    pub lsn: LogSequenceNumber,
    /// Timestamp when the snapshot was created.
    pub created_at: i64,
    /// Estimated size in bytes (pages/segments pinned).
    pub size_bytes: u64,
    /// Minimum active transaction ID at snapshot time (watermark).
    ///
    /// Any transaction with ID < min_active_txn is guaranteed to be committed
    /// or aborted at snapshot time. This enables O(1) visibility checks for
    /// most transactions.
    pub min_active_txn: TransactionId,
    /// Transactions >= min_active_txn that were active at snapshot time.
    ///
    /// Uses HashSet for O(1) lookup. Only stores exceptions above the watermark,
    /// keeping the set small even with many concurrent transactions.
    /// This is the PostgreSQL approach for efficient snapshot visibility.
    pub active_txns: HashSet<TransactionId>,
}

impl Snapshot {
    /// Create a new snapshot from a list of active transaction IDs.
    ///
    /// This constructor implements the PostgreSQL-style watermark optimization:
    /// - Finds the minimum active transaction ID (watermark)
    /// - Only stores transactions >= watermark in the HashSet
    /// - Enables O(1) visibility checks for most transactions
    ///
    /// # Arguments
    ///
    /// * `id` - Unique snapshot identifier
    /// * `name` - User-provided name for the snapshot
    /// * `lsn` - LSN at which the snapshot was taken
    /// * `created_at` - Timestamp when the snapshot was created
    /// * `size_bytes` - Estimated size in bytes
    /// * `active_txn_list` - List of active transaction IDs at snapshot time
    ///
    /// # Examples
    ///
    /// ```
    /// # use nanostore::snap::Snapshot;
    /// # use nanostore::snap::SnapshotId;
    /// # use nanostore::wal::LogSequenceNumber;
    /// # use nanostore::txn::TransactionId;
    /// let active_txns = vec![
    ///     TransactionId::from(5),
    ///     TransactionId::from(10),
    ///     TransactionId::from(15),
    /// ];
    /// let snapshot = Snapshot::new(
    ///     SnapshotId::from(1),
    ///     "backup".to_string(),
    ///     LogSequenceNumber::from(100),
    ///     0,
    ///     0,
    ///     active_txns,
    /// );
    /// // min_active_txn will be 5, active_txns HashSet will contain {5, 10, 15}
    /// ```
    pub fn new(
        id: SnapshotId,
        name: String,
        lsn: LogSequenceNumber,
        created_at: i64,
        size_bytes: u64,
        active_txn_list: Vec<TransactionId>,
    ) -> Self {
        // Find minimum active transaction (watermark)
        let min_active_txn = active_txn_list
            .iter()
            .min()
            .copied()
            .unwrap_or(TransactionId::from(0));

        // Convert to HashSet for O(1) lookup
        let active_txns: HashSet<TransactionId> = active_txn_list.into_iter().collect();

        Self {
            id,
            name,
            lsn,
            created_at,
            size_bytes,
            min_active_txn,
            active_txns,
        }
    }

    /// Determines if a version is visible to this snapshot.
    ///
    /// A version is visible if:
    /// 1. It was committed before this snapshot's LSN
    /// 2. It was not created by a transaction that was active at snapshot time
    ///
    /// # Arguments
    ///
    /// * `version_lsn` - The LSN at which the version was committed
    /// * `created_by` - The transaction ID that created the version
    ///
    /// # Returns
    ///
    /// `true` if the version is visible to this snapshot, `false` otherwise
    ///
    /// # Examples
    ///
    /// ```
    /// # use nanostore::snap::Snapshot;
    /// # use nanostore::wal::LogSequenceNumber;
    /// # use nanostore::txn::TransactionId;
    /// # use nanostore::snap::SnapshotId;
    /// let snapshot = Snapshot {
    ///     id: SnapshotId::from(1),
    ///     name: "test".to_string(),
    ///     lsn: LogSequenceNumber::from(100),
    ///     created_at: 0,
    ///     size_bytes: 0,
    ///     min_active_txn: TransactionId::from(3),
    ///     active_txns: [TransactionId::from(5)].into_iter().collect(),
    /// };
    ///
    /// // Version committed before snapshot and not by active transaction
    /// assert!(snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(1)));
    ///
    /// // Version committed after snapshot
    /// assert!(!snapshot.is_visible(LogSequenceNumber::from(150), TransactionId::from(1)));
    ///
    /// // Version created by active transaction
    /// assert!(!snapshot.is_visible(LogSequenceNumber::from(50), TransactionId::from(5)));
    /// ```
    pub fn is_visible(&self, version_lsn: LogSequenceNumber, created_by: TransactionId) -> bool {
        // Version must be committed before this snapshot
        if version_lsn > self.lsn {
            return false;
        }

        // Fast path: transactions below watermark are guaranteed committed/aborted
        // This is O(1) and handles the common case
        if created_by < self.min_active_txn {
            return true;
        }

        // Slow path: check if transaction was in the active set
        // This is O(1) with HashSet, but only for transactions >= watermark
        !self.active_txns.contains(&created_by)
    }
}
