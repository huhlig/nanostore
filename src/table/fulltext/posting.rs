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

//! Posting list for full-text search.
//!
//! A posting list stores the document IDs and positions where a term appears.

use crate::snap::Snapshot;
use crate::txn::{TransactionId, VersionChain};
use crate::wal::LogSequenceNumber;
use serde::{Deserialize, Serialize};

/// A single position occurrence in a document.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PostingEntry {
    /// Document ID
    pub doc_id: Vec<u8>,
    /// Field name where term appears
    pub field: String,
    /// Positions within the field
    pub positions: Vec<usize>,
    /// Field boost factor
    pub boost: f32,
    /// Version chain for MVCC support
    pub version_chain: VersionChain,
}

impl PostingEntry {
    /// Create a new posting entry with a version chain.
    pub fn new(
        doc_id: Vec<u8>,
        field: String,
        positions: Vec<usize>,
        boost: f32,
        tx_id: TransactionId,
    ) -> Self {
        // Serialize positions and boost as the version value
        let value = postcard::to_allocvec(&(positions.clone(), boost)).unwrap_or_default();
        let version_chain = VersionChain::new(value, tx_id);
        Self {
            doc_id,
            field,
            positions,
            boost,
            version_chain,
        }
    }

    /// Check if this entry is visible to the given snapshot.
    pub fn is_visible(&self, snapshot: &Snapshot) -> bool {
        match self.version_chain.find_visible_version(snapshot) {
            Some(value) => !Self::is_tombstone(value),
            None => false,
        }
    }

    /// Check if a version value is a tombstone marker.
    fn is_tombstone(value: &[u8]) -> bool {
        value == &[0xFF]
    }

    /// Create a tombstone marker value.
    fn tombstone_marker() -> Vec<u8> {
        vec![0xFF]
    }

    /// Commit this entry's version at the given LSN.
    pub fn commit(&mut self, lsn: LogSequenceNumber) {
        self.version_chain.commit(lsn);
    }

    /// Prepend a new version to this entry's chain.
    pub fn prepend_version(&mut self, positions: Vec<usize>, boost: f32, tx_id: TransactionId) {
        let value = postcard::to_allocvec(&(positions.clone(), boost)).unwrap_or_default();
        let old_chain = std::mem::replace(
            &mut self.version_chain,
            VersionChain::new(value.clone(), tx_id),
        );
        self.version_chain = old_chain.prepend(value, tx_id);
        self.positions = positions;
        self.boost = boost;
    }

    /// Prepend a tombstone version to mark this entry as deleted.
    pub fn prepend_tombstone(&mut self, tx_id: TransactionId) {
        let old_chain = std::mem::replace(
            &mut self.version_chain,
            VersionChain::new(Self::tombstone_marker(), tx_id),
        );
        self.version_chain = old_chain.prepend(Self::tombstone_marker(), tx_id);
    }

    /// Vacuum old versions from this entry's chain.
    pub fn vacuum(&mut self, min_visible_lsn: LogSequenceNumber) -> usize {
        self.version_chain.vacuum(min_visible_lsn)
    }
}

/// Posting list for a single term.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PostingList {
    /// List of posting entries
    pub entries: Vec<PostingEntry>,
}

impl PostingList {
    /// Create a new empty posting list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a posting entry.
    pub fn add(&mut self, entry: PostingEntry) {
        self.entries.push(entry);
    }

    /// Remove all entries for a document.
    pub fn remove_document(&mut self, doc_id: &[u8]) {
        self.entries.retain(|e| e.doc_id != doc_id);
    }

    /// Get the document frequency (number of documents containing this term).
    /// Only counts visible entries for the given snapshot.
    pub fn doc_freq(&self, snapshot: Option<&Snapshot>) -> usize {
        if let Some(snap) = snapshot {
            self.entries.iter().filter(|e| e.is_visible(snap)).count()
        } else {
            self.entries.len()
        }
    }

    /// Check if the posting list is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Commit all entries in this posting list.
    pub fn commit_versions(&mut self, lsn: LogSequenceNumber) {
        for entry in &mut self.entries {
            entry.commit(lsn);
        }
    }

    /// Vacuum old versions from all entries.
    pub fn vacuum(&mut self, min_visible_lsn: LogSequenceNumber) -> usize {
        self.entries
            .iter_mut()
            .map(|e| e.vacuum(min_visible_lsn))
            .sum()
    }

    /// Serialize to bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    /// Deserialize from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

/// Document store entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentEntry {
    /// Document ID
    pub doc_id: Vec<u8>,
    /// Stored fields (name -> value)
    pub fields: Vec<(String, String)>,
}

impl DocumentEntry {
    /// Serialize to bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    /// Deserialize from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}
