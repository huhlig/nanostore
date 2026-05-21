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

//! Merge iterator for LSM tree.
//!
//! This module provides iterators for reading across multiple data sources
//! in the LSM tree (memtable, immutable memtables, and `SSTables`).
//!
//! # Architecture
//!
//! - **`LsmIterator` trait**: Common interface for all data source iterators
//! - **`MemtableIterator`**: Iterates over a single memtable
//! - **`SStableIterator`**: Iterates over a single `SSTable`
//! - **`MergeIterator`**: K-way merge of multiple iterators using min-heap
//!
//! # Features
//!
//! - MVCC visibility filtering based on snapshot LSN
//! - Tombstone handling (skip deleted keys)
//! - Efficient seek operations
//! - Support for forward and backward iteration
//! - Maintains key ordering across all sources

use crate::table::error::TableResult;
use crate::table::lsm::{DataBlock, Memtable, SStableReader};
use crate::txn::{VersionChain, VersionValue};
use crate::vfs::FileSystem;
use crate::wal::LogSequenceNumber;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;

/// Direction of iteration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Forward iteration (ascending key order)
    Forward,
    /// Backward iteration (descending key order)
    Backward,
}

/// Entry returned by LSM iterators.
///
/// Contains the key, version chain, and source priority for merge ordering.
#[derive(Clone, Debug)]
pub struct LsmEntry {
    /// Key bytes
    pub key: Vec<u8>,
    /// Version chain for this key
    pub chain: VersionChain,
    /// Source priority (lower = newer, higher priority)
    pub priority: usize,
}

impl LsmEntry {
    /// Create a new LSM entry.
    #[must_use]
    pub fn new(key: Vec<u8>, chain: VersionChain, priority: usize) -> Self {
        Self {
            key,
            chain,
            priority,
        }
    }
}

/// Wrapper for heap ordering in merge iterator.
///
/// Orders by key (ascending/descending based on direction), then by priority
/// (lower priority = newer source = higher precedence).
#[derive(Clone, Debug)]
struct HeapEntry {
    entry: LsmEntry,
    direction: Direction,
    source_id: usize,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.entry.key == other.entry.key && self.entry.priority == other.entry.priority
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max-heap, so we reverse the ordering
        match self.direction {
            Direction::Forward => {
                // For forward iteration, we want min-heap behavior
                // Compare keys in reverse (other.key.cmp(self.key))
                // Then compare priority in reverse (other.priority.cmp(self.priority))
                match other.entry.key.cmp(&self.entry.key) {
                    Ordering::Equal => {
                        // Lower priority (newer) comes first
                        other.entry.priority.cmp(&self.entry.priority)
                    }
                    ord => ord,
                }
            }
            Direction::Backward => {
                // For backward iteration, we want max-heap behavior
                // Compare keys normally (self.key.cmp(other.key))
                // Then compare priority in reverse (other.priority.cmp(self.priority))
                match self.entry.key.cmp(&other.entry.key) {
                    Ordering::Equal => {
                        // Lower priority (newer) comes first
                        other.entry.priority.cmp(&self.entry.priority)
                    }
                    ord => ord,
                }
            }
        }
    }
}

/// Common iterator interface for LSM data sources.
pub trait LsmIterator {
    /// Get the current entry without advancing.
    fn peek(&self) -> Option<&LsmEntry>;

    /// Advance to the next entry and return it.
    fn next(&mut self) -> TableResult<Option<LsmEntry>>;

    /// Seek to the first key >= target (forward) or <= target (backward).
    fn seek(&mut self, target: &[u8]) -> TableResult<()>;

    /// Seek to the first entry.
    fn seek_to_first(&mut self) -> TableResult<()>;

    /// Seek to the last entry.
    fn seek_to_last(&mut self) -> TableResult<()>;

    /// Check if the iterator is valid (has a current entry).
    fn valid(&self) -> bool {
        self.peek().is_some()
    }
}

/// Iterator over a memtable.
pub struct MemtableIterator {
    /// Sorted entries from the memtable
    entries: Vec<(Vec<u8>, VersionChain)>,
    /// Current position
    position: Option<usize>,
    /// Iteration direction
    direction: Direction,
    /// Source priority for merge ordering
    priority: usize,
    /// Current entry cache
    current: Option<LsmEntry>,
}

impl MemtableIterator {
    /// Create a new memtable iterator.
    pub fn new(
        memtable: &Memtable,
        direction: Direction,
        priority: usize,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self> {
        // Get all entries from the memtable
        // Note: This requires the memtable to be immutable
        let entries = if memtable.is_immutable() {
            memtable.entries()?
        } else {
            // For mutable memtables, we need to take a snapshot
            // This is a simplified approach - in production, you'd want
            // a more efficient snapshot mechanism
            memtable
                .scan(None, None, snapshot_lsn)?
                .into_iter()
                .map(|(k, v)| {
                    let mut chain = VersionChain::new(v, crate::txn::TransactionId::from(0));
                    // Mark as committed at the snapshot LSN so it's visible
                    chain.commit(snapshot_lsn);
                    (k, chain)
                })
                .collect()
        };

        let mut iter = Self {
            entries,
            position: None,
            direction,
            priority,
            current: None,
        };

        // Position at first entry
        if !iter.entries.is_empty() {
            iter.seek_to_first()?;
        }

        Ok(iter)
    }

    /// Update the current entry cache.
    fn update_current(&mut self) {
        if let Some(pos) = self.position
            && pos < self.entries.len()
        {
            let (key, chain) = &self.entries[pos];
            self.current = Some(LsmEntry::new(key.clone(), chain.clone(), self.priority));
            return;
        }
        self.current = None;
    }
}

impl LsmIterator for MemtableIterator {
    fn peek(&self) -> Option<&LsmEntry> {
        self.current.as_ref()
    }

    fn next(&mut self) -> TableResult<Option<LsmEntry>> {
        if let Some(pos) = self.position {
            match self.direction {
                Direction::Forward => {
                    if pos + 1 < self.entries.len() {
                        self.position = Some(pos + 1);
                    } else {
                        self.position = None;
                    }
                }
                Direction::Backward => {
                    if pos > 0 {
                        self.position = Some(pos - 1);
                    } else {
                        self.position = None;
                    }
                }
            }
            self.update_current();
        }
        Ok(self.current.clone())
    }

    fn seek(&mut self, target: &[u8]) -> TableResult<()> {
        if self.entries.is_empty() {
            self.position = None;
            self.current = None;
            return Ok(());
        }

        match self.direction {
            Direction::Forward => {
                // Binary search for first key >= target
                let pos = self
                    .entries
                    .binary_search_by(|(k, _)| k.as_slice().cmp(target))
                    .unwrap_or_else(|pos| pos);

                if pos < self.entries.len() {
                    self.position = Some(pos);
                } else {
                    self.position = None;
                }
            }
            Direction::Backward => {
                // Binary search for last key <= target
                let pos = self
                    .entries
                    .binary_search_by(|(k, _)| k.as_slice().cmp(target));

                self.position = match pos {
                    Ok(exact) => Some(exact),
                    Err(insert_pos) => {
                        if insert_pos > 0 {
                            Some(insert_pos - 1)
                        } else {
                            None
                        }
                    }
                };
            }
        }

        self.update_current();
        Ok(())
    }

    fn seek_to_first(&mut self) -> TableResult<()> {
        if !self.entries.is_empty() {
            self.position = match self.direction {
                Direction::Forward => Some(0),
                Direction::Backward => Some(self.entries.len() - 1),
            };
            self.update_current();
        } else {
            self.position = None;
            self.current = None;
        }
        Ok(())
    }

    fn seek_to_last(&mut self) -> TableResult<()> {
        if !self.entries.is_empty() {
            self.position = match self.direction {
                Direction::Forward => Some(self.entries.len() - 1),
                Direction::Backward => Some(0),
            };
            self.update_current();
        } else {
            self.position = None;
            self.current = None;
        }
        Ok(())
    }
}

/// Iterator over an `SSTable` using block-level iteration.
///
/// This implementation loads data blocks on demand rather than loading all entries
/// into memory, providing better memory efficiency for large `SSTables`.
pub struct SStableIterator<FS: FileSystem> {
    /// `SSTable` reader
    reader: Arc<SStableReader<FS>>,
    /// Current data block index in the index block
    current_block_idx: Option<usize>,
    /// Current data block (loaded on demand)
    current_block: Option<DataBlock>,
    /// Position within the current block
    position_in_block: Option<usize>,
    /// Iteration direction
    direction: Direction,
    /// Source priority for merge ordering
    priority: usize,
    /// Current entry cache
    current: Option<LsmEntry>,
}

impl<FS: FileSystem> SStableIterator<FS> {
    /// Create a new `SSTable` iterator with block-level iteration.
    pub fn new(
        reader: Arc<SStableReader<FS>>,
        direction: Direction,
        priority: usize,
    ) -> TableResult<Self> {
        let mut iter = Self {
            reader,
            current_block_idx: None,
            current_block: None,
            position_in_block: None,
            direction,
            priority,
            current: None,
        };

        // Position at first entry
        iter.seek_to_first()?;

        Ok(iter)
    }

    /// Load a data block by index.
    fn load_block(&mut self, block_idx: usize) -> TableResult<()> {
        let index_entry = self.reader.index_block().get(block_idx).ok_or_else(|| {
            use crate::table::TableError;
            TableError::corruption(
                "SSTableIterator::load_block",
                "invalid_block_index",
                format!("Invalid block index: {}", block_idx),
            )
        })?;

        // Read the data block from the page
        let page = self.reader.pager().read_page(index_entry.page_id)?;
        let page_data = page.data();
        let block_start = index_entry.offset as usize;

        // Calculate block end
        let block_end = if block_idx + 1 < self.reader.index_block().len() {
            let next_entry = self.reader.index_block().get(block_idx + 1).unwrap();
            if next_entry.page_id == index_entry.page_id {
                next_entry.offset as usize
            } else {
                page_data.len()
            }
        } else {
            // Last data block - find where index block starts
            let page_size = self.reader.pager().page_size().data_size();
            let page_index = (index_entry.page_id.as_u64()
                - self.reader.metadata().first_page_id.as_u64())
                as usize;
            let page_start_offset = page_index * page_size;
            let index_start_in_sstable = self.reader.metadata().index_offset as usize;

            if index_start_in_sstable >= page_start_offset
                && index_start_in_sstable < page_start_offset + page_size
            {
                index_start_in_sstable - page_start_offset
            } else {
                page_data.len()
            }
        };

        if block_start >= page_data.len() || block_end > page_data.len() || block_start >= block_end
        {
            use crate::table::TableError;
            return Err(TableError::corruption(
                "SSTableIterator::load_block",
                "invalid_block_bounds",
                format!(
                    "Invalid data block bounds: start={}, end={}, page_len={}",
                    block_start,
                    block_end,
                    page_data.len()
                ),
            ));
        }

        let data_block_bytes = &page_data[block_start..block_end];
        let data_block = DataBlock::from_bytes(data_block_bytes)?;

        self.current_block = Some(data_block);
        self.current_block_idx = Some(block_idx);

        Ok(())
    }

    /// Update the current entry cache from the current block and position.
    fn update_current(&mut self) {
        if let (Some(block), Some(pos)) = (&self.current_block, self.position_in_block)
            && let Some((key, chain)) = block.entries().get(pos)
        {
            self.current = Some(LsmEntry::new(key.clone(), chain.clone(), self.priority));
            return;
        }
        self.current = None;
    }

    /// Move to the next block in the iteration direction.
    fn advance_to_next_block(&mut self) -> TableResult<bool> {
        let num_blocks = self.reader.index_block().len();

        if let Some(current_idx) = self.current_block_idx {
            let next_idx = match self.direction {
                Direction::Forward => {
                    if current_idx + 1 < num_blocks {
                        Some(current_idx + 1)
                    } else {
                        None
                    }
                }
                Direction::Backward => {
                    if current_idx > 0 {
                        Some(current_idx - 1)
                    } else {
                        None
                    }
                }
            };

            if let Some(idx) = next_idx {
                self.load_block(idx)?;
                // Position at first/last entry in new block
                if let Some(block) = &self.current_block
                    && !block.is_empty()
                {
                    self.position_in_block = match self.direction {
                        Direction::Forward => Some(0),
                        Direction::Backward => Some(block.len() - 1),
                    };
                    self.update_current();
                    return Ok(true);
                }
            }
        }

        // No more blocks
        self.current_block_idx = None;
        self.current_block = None;
        self.position_in_block = None;
        self.current = None;
        Ok(false)
    }
}

impl<FS: FileSystem> LsmIterator for SStableIterator<FS> {
    fn peek(&self) -> Option<&LsmEntry> {
        self.current.as_ref()
    }

    fn next(&mut self) -> TableResult<Option<LsmEntry>> {
        if self.position_in_block.is_none() {
            return Ok(None);
        }

        let pos = self.position_in_block.unwrap();
        let block_len = self.current_block.as_ref().map(|b| b.len()).unwrap_or(0);

        match self.direction {
            Direction::Forward => {
                if pos + 1 < block_len {
                    // Move to next entry in current block
                    self.position_in_block = Some(pos + 1);
                    self.update_current();
                } else {
                    // Move to next block
                    self.advance_to_next_block()?;
                }
            }
            Direction::Backward => {
                if pos > 0 {
                    // Move to previous entry in current block
                    self.position_in_block = Some(pos - 1);
                    self.update_current();
                } else {
                    // Move to previous block
                    self.advance_to_next_block()?;
                }
            }
        }

        Ok(self.current.clone())
    }

    fn seek(&mut self, target: &[u8]) -> TableResult<()> {
        let num_blocks = self.reader.index_block().len();
        if num_blocks == 0 {
            self.current_block_idx = None;
            self.current_block = None;
            self.position_in_block = None;
            self.current = None;
            return Ok(());
        }

        // Find the block that may contain the target key
        let block_idx = match self.reader.index_block().search(target) {
            Some(idx) => idx,
            None => {
                // Key is before all blocks
                if self.direction == Direction::Forward {
                    // Position at first block
                    0
                } else {
                    // No valid position for backward iteration
                    self.current_block_idx = None;
                    self.current_block = None;
                    self.position_in_block = None;
                    self.current = None;
                    return Ok(());
                }
            }
        };

        // Load the block
        self.load_block(block_idx)?;

        // Binary search within the block
        if let Some(block) = &self.current_block {
            match self.direction {
                Direction::Forward => {
                    // Find first entry >= target
                    let pos = block
                        .entries()
                        .binary_search_by(|(k, _)| k.as_slice().cmp(target))
                        .unwrap_or_else(|pos| pos);

                    if pos < block.len() {
                        self.position_in_block = Some(pos);
                        self.update_current();
                    } else {
                        // Target is after all entries in this block, move to next block
                        self.advance_to_next_block()?;
                    }
                }
                Direction::Backward => {
                    // Find last entry <= target
                    let pos = block
                        .entries()
                        .binary_search_by(|(k, _)| k.as_slice().cmp(target));

                    self.position_in_block = match pos {
                        Ok(exact) => Some(exact),
                        Err(insert_pos) => {
                            if insert_pos > 0 {
                                Some(insert_pos - 1)
                            } else {
                                // Target is before all entries in this block
                                // Move to previous block's last entry
                                if block_idx > 0 {
                                    self.load_block(block_idx - 1)?;
                                    if let Some(prev_block) = &self.current_block {
                                        if !prev_block.is_empty() {
                                            Some(prev_block.len() - 1)
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            }
                        }
                    };
                    self.update_current();
                }
            }
        }

        Ok(())
    }

    fn seek_to_first(&mut self) -> TableResult<()> {
        let num_blocks = self.reader.index_block().len();
        if num_blocks == 0 {
            self.current_block_idx = None;
            self.current_block = None;
            self.position_in_block = None;
            self.current = None;
            return Ok(());
        }

        let first_block_idx = match self.direction {
            Direction::Forward => 0,
            Direction::Backward => num_blocks - 1,
        };

        self.load_block(first_block_idx)?;

        if let Some(block) = &self.current_block {
            if !block.is_empty() {
                self.position_in_block = match self.direction {
                    Direction::Forward => Some(0),
                    Direction::Backward => Some(block.len() - 1),
                };
                self.update_current();
            } else {
                self.position_in_block = None;
                self.current = None;
            }
        }

        Ok(())
    }

    fn seek_to_last(&mut self) -> TableResult<()> {
        let num_blocks = self.reader.index_block().len();
        if num_blocks == 0 {
            self.current_block_idx = None;
            self.current_block = None;
            self.position_in_block = None;
            self.current = None;
            return Ok(());
        }

        let last_block_idx = match self.direction {
            Direction::Forward => num_blocks - 1,
            Direction::Backward => 0,
        };

        self.load_block(last_block_idx)?;

        if let Some(block) = &self.current_block {
            if !block.is_empty() {
                self.position_in_block = match self.direction {
                    Direction::Forward => Some(block.len() - 1),
                    Direction::Backward => Some(0),
                };
                self.update_current();
            } else {
                self.position_in_block = None;
                self.current = None;
            }
        }

        Ok(())
    }
}

/// K-way merge iterator that combines multiple LSM iterators.
///
/// Uses a min-heap to efficiently merge sorted streams while maintaining
/// key ordering and respecting source priority (newer sources override older).
pub struct MergeIterator {
    /// Heap of iterators with their current entries
    heap: BinaryHeap<HeapEntry>,
    /// Child iterators
    iterators: Vec<Box<dyn LsmIterator>>,
    /// Iteration direction
    direction: Direction,
    /// Snapshot LSN for MVCC visibility
    snapshot_lsn: LogSequenceNumber,
    /// Current entry (after deduplication and visibility filtering)
    current: Option<(Vec<u8>, Vec<u8>)>,
}

impl MergeIterator {
    /// Create a new merge iterator from multiple child iterators.
    ///
    /// Iterators should be ordered by priority (index 0 = highest priority/newest).
    pub fn new(
        iterators: Vec<Box<dyn LsmIterator>>,
        direction: Direction,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self> {
        let mut heap = BinaryHeap::new();

        // Initialize heap with first entry from each iterator
        for (source_id, iter) in iterators.iter().enumerate() {
            if let Some(entry) = iter.peek() {
                heap.push(HeapEntry {
                    entry: entry.clone(),
                    direction,
                    source_id,
                });
            }
        }

        let mut merge_iter = Self {
            heap,
            iterators,
            direction,
            snapshot_lsn,
            current: None,
        };

        // Position at first valid entry
        merge_iter.advance()?;

        Ok(merge_iter)
    }

    /// Advance to the next unique key with a visible, non-tombstone value.
    fn advance(&mut self) -> TableResult<()> {
        self.current = None;

        while let Some(heap_entry) = self.heap.pop() {
            let source_id = heap_entry.source_id;
            let entry = heap_entry.entry;

            // Advance the source iterator
            if let Some(next_entry) = self.iterators[source_id].next()? {
                self.heap.push(HeapEntry {
                    entry: next_entry,
                    direction: self.direction,
                    source_id,
                });
            }

            // Skip entries with the same key from lower-priority sources
            while let Some(next_heap_entry) = self.heap.peek() {
                if next_heap_entry.entry.key == entry.key {
                    let next_source_id = next_heap_entry.source_id;
                    self.heap.pop();

                    // Advance the skipped iterator
                    if let Some(next_entry) = self.iterators[next_source_id].next()? {
                        self.heap.push(HeapEntry {
                            entry: next_entry,
                            direction: self.direction,
                            source_id: next_source_id,
                        });
                    }
                } else {
                    break;
                }
            }

            // Find visible version in the chain
            let mut current_chain = Some(&entry.chain);
            while let Some(version) = current_chain {
                if let Some(commit_lsn) = version.commit_lsn
                    && commit_lsn <= self.snapshot_lsn
                {
                    // Found visible version
                    // Check if it's a tombstone (empty value)
                    if !version.value.is_empty() {
                        // Extract inline value from VersionValue
                        match &version.value {
                            VersionValue::Inline(data) => {
                                self.current = Some((entry.key.clone(), data.clone()));
                                return Ok(());
                            }
                            VersionValue::External(_) => {
                                // External values should not be in LSM iterators at this level
                                // They should be resolved before reaching here
                                return Err(crate::table::TableError::invalid_operation_state(
                                    "MergeIterator::advance",
                                    "Found external VersionValue in LSM iterator",
                                ));
                            }
                        }
                    } else {
                        // Tombstone - skip this key
                        break;
                    }
                }
                current_chain = version.prev_version.as_deref();
            }

            // No visible version found, continue to next key
        }

        Ok(())
    }

    /// Get the current key-value pair.
    #[must_use]
    pub fn current(&self) -> Option<(&[u8], &[u8])> {
        self.current
            .as_ref()
            .map(|(k, v)| (k.as_slice(), v.as_slice()))
    }

    /// Check if the iterator is valid.
    #[must_use]
    pub fn valid(&self) -> bool {
        self.current.is_some()
    }

    /// Advance to the next entry.
    pub fn step_forward(&mut self) -> TableResult<bool> {
        self.advance()?;
        Ok(self.valid())
    }

    /// Seek to the first key >= target (forward) or <= target (backward).
    pub fn seek(&mut self, target: &[u8]) -> TableResult<()> {
        // Seek all iterators
        for iter in &mut self.iterators {
            iter.seek(target)?;
        }

        // Rebuild heap
        self.heap.clear();
        for (source_id, iter) in self.iterators.iter().enumerate() {
            if let Some(entry) = iter.peek() {
                self.heap.push(HeapEntry {
                    entry: entry.clone(),
                    direction: self.direction,
                    source_id,
                });
            }
        }

        // Position at first valid entry
        self.advance()?;
        Ok(())
    }

    /// Seek to the first entry.
    pub fn seek_to_first(&mut self) -> TableResult<()> {
        // Seek all iterators to first
        for iter in &mut self.iterators {
            iter.seek_to_first()?;
        }

        // Rebuild heap
        self.heap.clear();
        for (source_id, iter) in self.iterators.iter().enumerate() {
            if let Some(entry) = iter.peek() {
                self.heap.push(HeapEntry {
                    entry: entry.clone(),
                    direction: self.direction,
                    source_id,
                });
            }
        }

        // Position at first valid entry
        self.advance()?;
        Ok(())
    }

    /// Seek to the last entry.
    pub fn seek_to_last(&mut self) -> TableResult<()> {
        // Seek all iterators to last
        for iter in &mut self.iterators {
            iter.seek_to_last()?;
        }

        // Rebuild heap
        self.heap.clear();
        for (source_id, iter) in self.iterators.iter().enumerate() {
            if let Some(entry) = iter.peek() {
                self.heap.push(HeapEntry {
                    entry: entry.clone(),
                    direction: self.direction,
                    source_id,
                });
            }
        }

        // Position at first valid entry
        self.advance()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::txn::TransactionId;

    fn create_lsn(n: u64) -> LogSequenceNumber {
        LogSequenceNumber::from(n)
    }

    fn create_txn_id(n: u64) -> TransactionId {
        TransactionId::from(n)
    }

    #[test]
    fn test_memtable_iterator_forward() {
        let memtable = Memtable::new(1024 * 1024);

        // Insert test data
        for i in 0..5 {
            let key = format!("key{}", i).into_bytes();
            let value = format!("value{}", i).into_bytes();
            memtable
                .insert(key, value, create_txn_id(1), Some(create_lsn(100)))
                .unwrap();
        }

        memtable.make_immutable();

        let mut iter = MemtableIterator::new(
            &memtable,
            Direction::Forward,
            0,
            LogSequenceNumber::from(u64::MAX),
        )
        .unwrap();

        // Check forward iteration
        let mut count = 0;
        while iter.valid() {
            let entry = iter.peek().unwrap();
            assert_eq!(entry.key, format!("key{}", count).into_bytes());
            iter.next().unwrap();
            count += 1;
        }
        assert_eq!(count, 5);
    }

    #[test]
    fn test_memtable_iterator_backward() {
        let memtable = Memtable::new(1024 * 1024);

        // Insert test data
        for i in 0..5 {
            let key = format!("key{}", i).into_bytes();
            let value = format!("value{}", i).into_bytes();
            memtable
                .insert(key, value, create_txn_id(1), Some(create_lsn(100)))
                .unwrap();
        }

        memtable.make_immutable();

        let mut iter = MemtableIterator::new(
            &memtable,
            Direction::Backward,
            0,
            LogSequenceNumber::from(u64::MAX),
        )
        .unwrap();

        // Check backward iteration
        let mut count = 4;
        while iter.valid() {
            let entry = iter.peek().unwrap();
            assert_eq!(entry.key, format!("key{}", count).into_bytes());
            iter.next().unwrap();
            if count > 0 {
                count -= 1;
            } else {
                break;
            }
        }
    }

    #[test]
    fn test_memtable_iterator_seek() {
        let memtable = Memtable::new(1024 * 1024);

        // Insert test data
        for i in 0..10 {
            let key = format!("key{:02}", i).into_bytes();
            let value = format!("value{}", i).into_bytes();
            memtable
                .insert(key, value, create_txn_id(1), Some(create_lsn(100)))
                .unwrap();
        }

        memtable.make_immutable();

        let mut iter = MemtableIterator::new(
            &memtable,
            Direction::Forward,
            0,
            LogSequenceNumber::from(u64::MAX),
        )
        .unwrap();

        // Seek to key05
        iter.seek(b"key05").unwrap();
        assert!(iter.valid());
        assert_eq!(iter.peek().unwrap().key, b"key05");

        // Seek to non-existent key (should position at next key)
        iter.seek(b"key04a").unwrap();
        assert!(iter.valid());
        assert_eq!(iter.peek().unwrap().key, b"key05");
    }

    #[test]
    fn test_merge_iterator_single_source() {
        let memtable = Memtable::new(1024 * 1024);

        // Insert test data
        for i in 0..5 {
            let key = format!("key{}", i).into_bytes();
            let value = format!("value{}", i).into_bytes();
            memtable
                .insert(key, value, create_txn_id(1), Some(create_lsn(100)))
                .unwrap();
        }

        memtable.make_immutable();

        let iter = MemtableIterator::new(
            &memtable,
            Direction::Forward,
            0,
            LogSequenceNumber::from(u64::MAX),
        )
        .unwrap();
        let iterators: Vec<Box<dyn LsmIterator>> = vec![Box::new(iter)];

        let mut merge_iter =
            MergeIterator::new(iterators, Direction::Forward, create_lsn(100)).unwrap();

        // Check iteration
        let mut count = 0;
        while merge_iter.valid() {
            let (key, value) = merge_iter.current().unwrap();
            assert_eq!(key, format!("key{}", count).as_bytes());
            assert_eq!(value, format!("value{}", count).as_bytes());
            merge_iter.step_forward().unwrap();
            count += 1;
        }
        assert_eq!(count, 5);
    }

    #[test]
    fn test_merge_iterator_multiple_sources() {
        // Create two memtables with overlapping keys
        let memtable1 = Memtable::new(1024 * 1024);
        let memtable2 = Memtable::new(1024 * 1024);

        // Memtable 1: key0, key2, key4
        for i in (0..5).step_by(2) {
            let key = format!("key{}", i).into_bytes();
            let value = format!("value1_{}", i).into_bytes();
            memtable1
                .insert(key, value, create_txn_id(1), Some(create_lsn(100)))
                .unwrap();
        }

        // Memtable 2: key1, key2, key3
        for i in 1..4 {
            let key = format!("key{}", i).into_bytes();
            let value = format!("value2_{}", i).into_bytes();
            memtable2
                .insert(key, value, create_txn_id(2), Some(create_lsn(200)))
                .unwrap();
        }

        memtable1.make_immutable();
        memtable2.make_immutable();

        // Memtable2 has higher priority (newer)
        let iter1 = MemtableIterator::new(
            &memtable1,
            Direction::Forward,
            1,
            LogSequenceNumber::from(u64::MAX),
        )
        .unwrap();
        let iter2 = MemtableIterator::new(
            &memtable2,
            Direction::Forward,
            0,
            LogSequenceNumber::from(u64::MAX),
        )
        .unwrap();
        let iterators: Vec<Box<dyn LsmIterator>> = vec![Box::new(iter2), Box::new(iter1)];

        let mut merge_iter =
            MergeIterator::new(iterators, Direction::Forward, create_lsn(200)).unwrap();

        // Expected order: key0 (from mt1), key1 (from mt2), key2 (from mt2), key3 (from mt2), key4 (from mt1)
        let expected = [
            ("key0", "value1_0"),
            ("key1", "value2_1"),
            ("key2", "value2_2"), // mt2 overrides mt1
            ("key3", "value2_3"),
            ("key4", "value1_4"),
        ];

        let mut results = Vec::new();
        while merge_iter.valid() {
            let (key, value) = merge_iter.current().unwrap();
            results.push((
                String::from_utf8(key.to_vec()).unwrap(),
                String::from_utf8(value.to_vec()).unwrap(),
            ));
            merge_iter.step_forward().unwrap();
        }

        assert_eq!(results.len(), expected.len());
        for (i, (exp_key, exp_val)) in expected.iter().enumerate() {
            assert_eq!(results[i].0, *exp_key);
            assert_eq!(results[i].1, *exp_val);
        }
    }

    #[test]
    fn test_merge_iterator_tombstone_handling() {
        let memtable1 = Memtable::new(1024 * 1024);
        let memtable2 = Memtable::new(1024 * 1024);

        // Memtable 1: key1, key2, key3
        for i in 1..4 {
            let key = format!("key{}", i).into_bytes();
            let value = format!("value{}", i).into_bytes();
            memtable1
                .insert(key, value, create_txn_id(1), Some(create_lsn(100)))
                .unwrap();
        }

        // Memtable 2: delete key2
        memtable2
            .delete(b"key2".to_vec(), create_txn_id(2), Some(create_lsn(200)))
            .unwrap();

        memtable1.make_immutable();
        memtable2.make_immutable();

        let iter1 = MemtableIterator::new(
            &memtable1,
            Direction::Forward,
            1,
            LogSequenceNumber::from(u64::MAX),
        )
        .unwrap();
        let iter2 = MemtableIterator::new(
            &memtable2,
            Direction::Forward,
            0,
            LogSequenceNumber::from(u64::MAX),
        )
        .unwrap();
        let iterators: Vec<Box<dyn LsmIterator>> = vec![Box::new(iter2), Box::new(iter1)];

        let mut merge_iter =
            MergeIterator::new(iterators, Direction::Forward, create_lsn(200)).unwrap();

        // Should see key1 and key3, but not key2 (tombstone)
        let mut results = Vec::new();
        while merge_iter.valid() {
            let (key, _) = merge_iter.current().unwrap();
            results.push(String::from_utf8(key.to_vec()).unwrap());
            merge_iter.step_forward().unwrap();
        }

        assert_eq!(results, vec!["key1", "key3"]);
    }

    #[test]
    fn test_merge_iterator_mvcc_visibility() {
        let memtable = Memtable::new(1024 * 1024);

        // Insert key1 at LSN 100
        memtable
            .insert(
                b"key1".to_vec(),
                b"value_old".to_vec(),
                create_txn_id(1),
                Some(create_lsn(100)),
            )
            .unwrap();

        // Update key1 at LSN 200
        memtable
            .insert(
                b"key1".to_vec(),
                b"value_new".to_vec(),
                create_txn_id(2),
                Some(create_lsn(200)),
            )
            .unwrap();

        memtable.make_immutable();

        // Iterator with snapshot at LSN 150 should see old value
        let iter = MemtableIterator::new(
            &memtable,
            Direction::Forward,
            0,
            LogSequenceNumber::from(u64::MAX),
        )
        .unwrap();
        let iterators: Vec<Box<dyn LsmIterator>> = vec![Box::new(iter)];
        let merge_iter =
            MergeIterator::new(iterators, Direction::Forward, create_lsn(150)).unwrap();

        assert!(merge_iter.valid());
        let (_, value) = merge_iter.current().unwrap();
        assert_eq!(value, b"value_old");

        // Iterator with snapshot at LSN 200 should see new value
        let memtable_clone = memtable.clone();
        let iter = MemtableIterator::new(
            &memtable_clone,
            Direction::Forward,
            0,
            LogSequenceNumber::from(u64::MAX),
        )
        .unwrap();
        let iterators: Vec<Box<dyn LsmIterator>> = vec![Box::new(iter)];
        let merge_iter =
            MergeIterator::new(iterators, Direction::Forward, create_lsn(200)).unwrap();

        assert!(merge_iter.valid());
        let (_, value) = merge_iter.current().unwrap();
        assert_eq!(value, b"value_new");
    }
}

// Made with Bob
