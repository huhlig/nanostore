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

use crate::snap::Snapshot;
use crate::txn::TransactionId;
use crate::types::ValueRef;
use crate::wal::LogSequenceNumber;

/// Storage strategy for version chain values.
///
/// This enum enables efficient storage of values of varying sizes:
/// - Small values (< inline threshold) are stored directly in memory
/// - Large values (>= inline threshold) are stored in overflow pages via ValueRef
///
/// This hybrid approach allows gradual migration and optimization per table type.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum VersionValue {
    /// Value stored inline in the version chain (for small values)
    Inline(Vec<u8>),
    
    /// Value stored externally in overflow pages (for large values)
    /// The ValueRef contains the page location information
    External(ValueRef),
}

impl VersionValue {
    /// Create an inline version value.
    pub fn inline(data: Vec<u8>) -> Self {
        Self::Inline(data)
    }
    
    /// Create an external version value with a ValueRef.
    pub fn external(value_ref: ValueRef) -> Self {
        Self::External(value_ref)
    }
    
    /// Check if this is an inline value.
    pub fn is_inline(&self) -> bool {
        matches!(self, Self::Inline(_))
    }
    
    /// Check if this is an external value.
    pub fn is_external(&self) -> bool {
        matches!(self, Self::External(_))
    }
    
    /// Get the inline data if this is an inline value.
    pub fn as_inline(&self) -> Option<&[u8]> {
        match self {
            Self::Inline(data) => Some(data),
            Self::External(_) => None,
        }
    }
    
    /// Get the ValueRef if this is an external value.
    pub fn as_external(&self) -> Option<&ValueRef> {
        match self {
            Self::Inline(_) => None,
            Self::External(value_ref) => Some(value_ref),
        }
    }
    
    /// Get a size hint for the value.
    pub fn size_hint(&self) -> Option<u64> {
        match self {
            Self::Inline(data) => Some(data.len() as u64),
            Self::External(value_ref) => value_ref.size_hint(),
        }
    }
    
    /// Check if this is an empty value (for tombstone detection).
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Inline(data) => data.is_empty(),
            Self::External(value_ref) => {
                // External values are never empty by definition
                // (they wouldn't be stored externally if they were)
                value_ref.size_hint().map_or(false, |size| size == 0)
            }
        }
    }
    
    /// Get the length of the value in bytes.
    pub fn len(&self) -> usize {
        match self {
            Self::Inline(data) => data.len(),
            Self::External(value_ref) => {
                value_ref.size_hint().map_or(0, |size| size as usize)
            }
        }
    }
    
    /// Convert to a Vec<u8> if this is an inline value.
    /// Returns None for external values (caller must read from overflow pages).
    pub fn to_vec(&self) -> Option<Vec<u8>> {
        match self {
            Self::Inline(data) => Some(data.clone()),
            Self::External(_) => None,
        }
    }
    
    /// Get inline data as a slice, or None for external values.
    pub fn as_slice(&self) -> Option<&[u8]> {
        match self {
            Self::Inline(data) => Some(data.as_slice()),
            Self::External(_) => None,
        }
    }
}

impl PartialEq for VersionValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Inline(a), Self::Inline(b)) => a == b,
            (Self::External(a), Self::External(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for VersionValue {}

// TODO(MVCC): Version chain structure for storing multiple versions of a value
// This enables MVCC by maintaining a linked list of value versions.
// Each version tracks:
// - The value data (inline or external via ValueRef)
// - Which transaction created it
// - When it was committed (None if uncommitted)
// - Link to previous (older) version
//
// Usage: Tables store the head of the version chain for each key.
// When reading, traverse the chain to find the first visible version
// based on the transaction's snapshot LSN and active transaction list.
//
// Garbage collection: Periodically remove versions older than the
// minimum visible LSN across all active snapshots. For external values,
// this also frees the associated overflow pages.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VersionChain {
    /// Current value data (inline or external)
    pub value: VersionValue,
    /// Transaction that created this version
    pub created_by: TransactionId,
    /// LSN when this version was committed (None if uncommitted)
    pub commit_lsn: Option<LogSequenceNumber>,
    /// Previous version (older), if any
    pub prev_version: Option<Box<VersionChain>>,
}

impl VersionChain {
    /// Create a new version chain entry with inline value
    pub fn new(value: Vec<u8>, created_by: TransactionId) -> Self {
        Self {
            value: VersionValue::Inline(value),
            created_by,
            commit_lsn: None,
            prev_version: None,
        }
    }
    
    /// Create a new version chain entry with external value (ValueRef)
    pub fn new_external(value_ref: ValueRef, created_by: TransactionId) -> Self {
        Self {
            value: VersionValue::External(value_ref),
            created_by,
            commit_lsn: None,
            prev_version: None,
        }
    }

    /// Mark this version as committed at the given LSN
    pub fn commit(&mut self, lsn: LogSequenceNumber) {
        self.commit_lsn = Some(lsn);
    }

    /// Add a new inline version to the front of the chain
    pub fn prepend(self, value: Vec<u8>, created_by: TransactionId) -> Self {
        Self {
            value: VersionValue::Inline(value),
            created_by,
            commit_lsn: None,
            prev_version: Some(Box::new(self)),
        }
    }
    
    /// Add a new external version to the front of the chain
    pub fn prepend_external(self, value_ref: ValueRef, created_by: TransactionId) -> Self {
        Self {
            value: VersionValue::External(value_ref),
            created_by,
            commit_lsn: None,
            prev_version: Some(Box::new(self)),
        }
    }

    /// Find the newest version visible to the provided snapshot.
    ///
    /// Traverses the chain from newest to oldest and returns the first version
    /// that is committed and visible at the snapshot's LSN and transaction set.
    /// Uncommitted versions are never visible.
    ///
    /// Returns the VersionValue (which may be inline or external).
    /// Callers must handle external values by reading from overflow pages.
    pub fn find_visible_version(&self, snapshot: &Snapshot) -> Option<&VersionValue> {
        let mut current = Some(self);

        while let Some(version) = current {
            if let Some(commit_lsn) = version.commit_lsn
                && snapshot.is_visible(commit_lsn, version.created_by)
            {
                return Some(&version.value);
            }

            current = version.prev_version.as_deref();
        }

        None
    }
    
    /// Find the newest visible inline value.
    ///
    /// This is a convenience method for callers that only want inline values.
    /// Returns None if the visible version is external or if no version is visible.
    pub fn find_visible_inline(&self, snapshot: &Snapshot) -> Option<&[u8]> {
        self.find_visible_version(snapshot)
            .and_then(|v| v.as_inline())
    }

    /// Remove obsolete committed versions older than the visibility watermark.
    ///
    /// Retains:
    /// - all uncommitted versions
    /// - committed versions with `commit_lsn >= min_visible_lsn`
    /// - the newest committed version older than `min_visible_lsn` as a base
    ///
    /// Returns the number of removed versions and a list of ValueRefs that need
    /// to have their overflow pages freed by the caller.
    ///
    /// Note: The caller is responsible for freeing overflow pages for external values.
    /// This method only identifies which ValueRefs need cleanup.
    pub fn vacuum(&mut self, min_visible_lsn: LogSequenceNumber) -> (usize, Vec<ValueRef>) {
        let mut freed_refs = Vec::new();
        
        fn retain_obsolete_versions(
            node: &VersionChain,
            min_visible_lsn: LogSequenceNumber,
            keep_obsolete_budget: &mut usize,
            freed_refs: &mut Vec<ValueRef>,
        ) -> VersionChain {
            let rebuilt_prev = node.prev_version.as_ref().map(|prev| {
                Box::new(retain_obsolete_versions(
                    prev,
                    min_visible_lsn,
                    keep_obsolete_budget,
                    freed_refs,
                ))
            });

            let keep_this_obsolete = matches!(node.commit_lsn, Some(lsn) if lsn < min_visible_lsn)
                && *keep_obsolete_budget > 0;

            if keep_this_obsolete {
                *keep_obsolete_budget -= 1;
            }

            let is_obsolete = matches!(node.commit_lsn, Some(lsn) if lsn < min_visible_lsn);
            let will_be_removed = is_obsolete && !keep_this_obsolete;
            
            // Track external values that will be removed so caller can free overflow pages
            if will_be_removed {
                if let VersionValue::External(value_ref) = &node.value {
                    freed_refs.push(*value_ref);
                }
            }

            let prev_version = if will_be_removed {
                rebuilt_prev.and_then(|prev| prev.prev_version)
            } else {
                rebuilt_prev
            };

            VersionChain {
                value: node.value.clone(),
                created_by: node.created_by,
                commit_lsn: node.commit_lsn,
                prev_version,
            }
        }

        fn count_removable_obsolete_versions(
            node: &VersionChain,
            min_visible_lsn: LogSequenceNumber,
        ) -> usize {
            let current = usize::from(matches!(
                node.commit_lsn,
                Some(lsn) if lsn < min_visible_lsn
            ));

            current
                + node
                    .prev_version
                    .as_deref()
                    .map(|prev| count_removable_obsolete_versions(prev, min_visible_lsn))
                    .unwrap_or(0)
        }

        let obsolete_count = count_removable_obsolete_versions(self, min_visible_lsn);
        let removed = obsolete_count.saturating_sub(1);

        if removed == 0 {
            return (0, Vec::new());
        }

        let mut keep_obsolete_budget = 1;
        *self = retain_obsolete_versions(self, min_visible_lsn, &mut keep_obsolete_budget, &mut freed_refs);
        (removed, freed_refs)
    }
    
    /// Collect all external ValueRefs in this version chain.
    ///
    /// This is useful for operations that need to track or free overflow pages.
    pub fn collect_external_refs(&self) -> Vec<ValueRef> {
        let mut refs = Vec::new();
        let mut current = Some(self);
        
        while let Some(version) = current {
            if let VersionValue::External(value_ref) = &version.value {
                refs.push(*value_ref);
            }
            current = version.prev_version.as_deref();
        }
        
        refs
    }
}

// Made with Bob
