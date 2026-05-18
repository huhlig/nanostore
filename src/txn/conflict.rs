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

//! Conflict detection for MVCC transactions
//!
//! This module provides conflict detection mechanisms for ensuring
//! transaction isolation. It tracks which transactions have accessed
//! which keys and detects conflicts based on the isolation level.

use crate::txn::{TransactionError, TransactionId, TransactionResult};
use crate::types::TableId;
use std::collections::{HashMap, HashSet};

/// Types of conflicts that can occur between transactions:
/// - WriteWrite: Two transactions write to the same key
/// - ReadWrite: A transaction reads a key that another transaction writes
/// - Serialization: Complex conflict in serializable isolation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictType {
    WriteWrite,
    ReadWrite,
    Serialization,
}

/// Tracks which transactions have locked which keys for writing.
/// Used to detect write-write conflicts before they occur.
///
/// # Usage
/// 1. Before writing a key, call check_write_conflict()
/// 2. If no conflict, call acquire_write_lock()
/// 3. On commit/abort, call release_locks()
pub struct ConflictDetector {
    // Maps (object_id, key) -> transaction ID that has write lock
    write_locks: HashMap<(TableId, Vec<u8>), TransactionId>,
}

impl ConflictDetector {
    pub fn new() -> Self {
        Self {
            write_locks: HashMap::new(),
        }
    }

    /// Check if a write would conflict with an existing write lock.
    ///
    /// Returns an error if another transaction holds a write lock on the key.
    pub fn check_write_conflict(
        &self,
        object_id: TableId,
        key: &[u8],
        txn_id: TransactionId,
    ) -> TransactionResult<()> {
        let lock_key = (object_id, key.to_vec());
        if let Some(&other_txn) = self.write_locks.get(&lock_key)
            && other_txn != txn_id
        {
            // Record conflict metric
            metrics::counter!("nanokv.transaction.conflict.write_write").increment(1);

            return Err(TransactionError::write_write_conflict(
                object_id,
                key.to_vec(),
                other_txn,
                txn_id,
            ));
        }
        Ok(())
    }

    /// Acquire a write lock on a key for the given transaction.
    ///
    /// Should be called after check_write_conflict() succeeds.
    pub fn acquire_write_lock(&mut self, object_id: TableId, key: Vec<u8>, txn_id: TransactionId) {
        self.write_locks.insert((object_id, key), txn_id);
    }

    /// Release all write locks held by the given transaction.
    ///
    /// Called on commit or abort to free locks.
    pub fn release_locks(&mut self, txn_id: TransactionId) {
        self.write_locks.retain(|_, &mut holder| holder != txn_id);
    }

    /// Check for read-write conflicts in serializable isolation.
    ///
    /// For serializable isolation, check if any key in the read set
    /// has been written by another transaction.
    pub fn check_read_write_conflicts(
        &self,
        read_set: &HashSet<(TableId, Vec<u8>)>,
        txn_id: TransactionId,
    ) -> TransactionResult<()> {
        for (object_id, key) in read_set {
            if let Some(&other_txn) = self.write_locks.get(&(*object_id, key.clone()))
                && other_txn != txn_id
            {
                // Record conflict metric
                metrics::counter!("nanokv.transaction.conflict.read_write").increment(1);

                return Err(TransactionError::read_write_conflict(
                    *object_id,
                    key.clone(),
                    txn_id,
                    other_txn,
                ));
            }
        }
        Ok(())
    }
}

impl Default for ConflictDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Tracks wait-for relationships between transactions to detect cycles.
///
/// When a transaction waits for a lock held by another transaction,
/// we add an edge to the wait-for graph. If a cycle is detected,
/// we abort one of the transactions to break the deadlock.
///
/// Uses a simple directed graph representation where an edge from A to B
/// means transaction A is waiting for transaction B to release a lock.
pub struct DeadlockDetector {
    /// Maps each waiting transaction to the set of transactions it's waiting for
    wait_for_graph: HashMap<TransactionId, Vec<TransactionId>>,
}

impl DeadlockDetector {
    /// Create a new deadlock detector with an empty wait-for graph.
    pub fn new() -> Self {
        Self {
            wait_for_graph: HashMap::new(),
        }
    }

    /// Add a wait-for edge: waiter is waiting for holder.
    ///
    /// This records that `waiter` is blocked waiting for a lock held by `holder`.
    pub fn add_wait(&mut self, waiter: TransactionId, holder: TransactionId) {
        self.wait_for_graph.entry(waiter).or_default().push(holder);
    }

    /// Remove all wait-for edges originating from the given transaction.
    ///
    /// Called when a transaction completes (commits or aborts) or acquires
    /// the lock it was waiting for.
    pub fn remove_wait(&mut self, waiter: TransactionId) {
        self.wait_for_graph.remove(&waiter);
    }

    /// Detect if there's a cycle in the wait-for graph using DFS.
    ///
    /// Returns the cycle as a vector of transaction IDs if found, or None.
    /// Uses depth-first search with a recursion stack to detect back edges.
    pub fn detect_cycle(&self) -> Option<Vec<TransactionId>> {
        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();
        let mut path = Vec::new();

        // Try DFS from each unvisited node
        for &txn_id in self.wait_for_graph.keys() {
            if !visited.contains(&txn_id)
                && let Some(cycle) =
                    self.dfs_detect_cycle(txn_id, &mut visited, &mut rec_stack, &mut path)
            {
                // Record deadlock detection metric
                metrics::counter!("nanokv.transaction.deadlock.detected").increment(1);

                return Some(cycle);
            }
        }

        None
    }

    /// Helper function for cycle detection using DFS.
    fn dfs_detect_cycle(
        &self,
        current: TransactionId,
        visited: &mut HashSet<TransactionId>,
        rec_stack: &mut HashSet<TransactionId>,
        path: &mut Vec<TransactionId>,
    ) -> Option<Vec<TransactionId>> {
        visited.insert(current);
        rec_stack.insert(current);
        path.push(current);

        // Visit all neighbors (transactions this one is waiting for)
        if let Some(neighbors) = self.wait_for_graph.get(&current) {
            for &neighbor in neighbors {
                if !visited.contains(&neighbor) {
                    // Recurse on unvisited neighbor
                    if let Some(cycle) = self.dfs_detect_cycle(neighbor, visited, rec_stack, path) {
                        return Some(cycle);
                    }
                } else if rec_stack.contains(&neighbor) {
                    // Found a back edge - cycle detected!
                    // Extract the cycle from the path
                    let cycle_start = path.iter().position(|&id| id == neighbor).unwrap();
                    return Some(path[cycle_start..].to_vec());
                }
            }
        }

        // Backtrack
        path.pop();
        rec_stack.remove(&current);
        None
    }
}

impl Default for DeadlockDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deadlock_detector_new() {
        let detector = DeadlockDetector::new();
        assert!(detector.detect_cycle().is_none());
    }

    #[test]
    fn test_no_cycle_single_wait() {
        let mut detector = DeadlockDetector::new();
        let txn1 = TransactionId::from(1);
        let txn2 = TransactionId::from(2);

        // txn1 waits for txn2
        detector.add_wait(txn1, txn2);

        // No cycle - just a simple wait
        assert!(detector.detect_cycle().is_none());
    }

    #[test]
    fn test_no_cycle_chain() {
        let mut detector = DeadlockDetector::new();
        let txn1 = TransactionId::from(1);
        let txn2 = TransactionId::from(2);
        let txn3 = TransactionId::from(3);

        // txn1 -> txn2 -> txn3 (chain, no cycle)
        detector.add_wait(txn1, txn2);
        detector.add_wait(txn2, txn3);

        assert!(detector.detect_cycle().is_none());
    }

    #[test]
    fn test_simple_cycle_two_transactions() {
        let mut detector = DeadlockDetector::new();
        let txn1 = TransactionId::from(1);
        let txn2 = TransactionId::from(2);

        // Create a cycle: txn1 -> txn2 -> txn1
        detector.add_wait(txn1, txn2);
        detector.add_wait(txn2, txn1);

        let cycle = detector.detect_cycle();
        assert!(cycle.is_some());
        let cycle = cycle.unwrap();
        assert_eq!(cycle.len(), 2);
        assert!(cycle.contains(&txn1));
        assert!(cycle.contains(&txn2));
    }

    #[test]
    fn test_cycle_three_transactions() {
        let mut detector = DeadlockDetector::new();
        let txn1 = TransactionId::from(1);
        let txn2 = TransactionId::from(2);
        let txn3 = TransactionId::from(3);

        // Create a cycle: txn1 -> txn2 -> txn3 -> txn1
        detector.add_wait(txn1, txn2);
        detector.add_wait(txn2, txn3);
        detector.add_wait(txn3, txn1);

        let cycle = detector.detect_cycle();
        assert!(cycle.is_some());
        let cycle = cycle.unwrap();
        assert_eq!(cycle.len(), 3);
        assert!(cycle.contains(&txn1));
        assert!(cycle.contains(&txn2));
        assert!(cycle.contains(&txn3));
    }

    #[test]
    fn test_cycle_with_multiple_waiters() {
        let mut detector = DeadlockDetector::new();
        let txn1 = TransactionId::from(1);
        let txn2 = TransactionId::from(2);
        let txn3 = TransactionId::from(3);
        let txn4 = TransactionId::from(4);

        // txn1 waits for both txn2 and txn3
        detector.add_wait(txn1, txn2);
        detector.add_wait(txn1, txn3);

        // txn2 waits for txn4
        detector.add_wait(txn2, txn4);

        // txn3 waits for txn1 (creates cycle: txn1 -> txn3 -> txn1)
        detector.add_wait(txn3, txn1);

        let cycle = detector.detect_cycle();
        assert!(cycle.is_some());
        let cycle = cycle.unwrap();
        // Cycle should be txn1 -> txn3 -> txn1
        assert!(cycle.contains(&txn1));
        assert!(cycle.contains(&txn3));
    }

    #[test]
    fn test_remove_wait_breaks_cycle() {
        let mut detector = DeadlockDetector::new();
        let txn1 = TransactionId::from(1);
        let txn2 = TransactionId::from(2);

        // Create a cycle
        detector.add_wait(txn1, txn2);
        detector.add_wait(txn2, txn1);
        assert!(detector.detect_cycle().is_some());

        // Remove one wait edge
        detector.remove_wait(txn1);

        // Cycle should be broken
        assert!(detector.detect_cycle().is_none());
    }

    #[test]
    fn test_remove_wait_from_chain() {
        let mut detector = DeadlockDetector::new();
        let txn1 = TransactionId::from(1);
        let txn2 = TransactionId::from(2);
        let txn3 = TransactionId::from(3);

        // Create chain: txn1 -> txn2 -> txn3
        detector.add_wait(txn1, txn2);
        detector.add_wait(txn2, txn3);

        // Remove middle transaction
        detector.remove_wait(txn2);

        // Should still have txn1 -> txn2 edge
        assert!(detector.detect_cycle().is_none());
    }

    #[test]
    fn test_self_cycle() {
        let mut detector = DeadlockDetector::new();
        let txn1 = TransactionId::from(1);

        // Transaction waiting for itself (shouldn't happen in practice, but test it)
        detector.add_wait(txn1, txn1);

        let cycle = detector.detect_cycle();
        assert!(cycle.is_some());
        let cycle = cycle.unwrap();
        assert_eq!(cycle.len(), 1);
        assert_eq!(cycle[0], txn1);
    }

    #[test]
    fn test_complex_graph_with_cycle() {
        let mut detector = DeadlockDetector::new();
        let txn1 = TransactionId::from(1);
        let txn2 = TransactionId::from(2);
        let txn3 = TransactionId::from(3);
        let txn4 = TransactionId::from(4);
        let txn5 = TransactionId::from(5);

        // Create a complex graph:
        // txn1 -> txn2
        // txn2 -> txn3
        // txn3 -> txn4
        // txn4 -> txn2 (cycle: 2 -> 3 -> 4 -> 2)
        // txn5 -> txn1 (separate chain)
        detector.add_wait(txn1, txn2);
        detector.add_wait(txn2, txn3);
        detector.add_wait(txn3, txn4);
        detector.add_wait(txn4, txn2);
        detector.add_wait(txn5, txn1);

        let cycle = detector.detect_cycle();
        assert!(cycle.is_some());
        let cycle = cycle.unwrap();
        // Cycle should involve txn2, txn3, txn4
        assert!(cycle.contains(&txn2));
        assert!(cycle.contains(&txn3));
        assert!(cycle.contains(&txn4));
    }

    #[test]
    fn test_multiple_add_wait_same_edge() {
        let mut detector = DeadlockDetector::new();
        let txn1 = TransactionId::from(1);
        let txn2 = TransactionId::from(2);

        // Add same edge multiple times
        detector.add_wait(txn1, txn2);
        detector.add_wait(txn1, txn2);
        detector.add_wait(txn1, txn2);

        // Should still work correctly
        assert!(detector.detect_cycle().is_none());

        // Add reverse edge to create cycle
        detector.add_wait(txn2, txn1);
        assert!(detector.detect_cycle().is_some());
    }

    #[test]
    fn test_default_trait() {
        let detector = DeadlockDetector::default();
        assert!(detector.detect_cycle().is_none());
    }
}
