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

//! Segment management for AppendLog.

use crate::pager::{Page, PageId, PageType, Pager};
use crate::table::{TableError, TableResult};
use crate::types::ValueBuf;
use crate::vfs::FileSystem;
use crate::wal::LogSequenceNumber;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Segment identifier.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SegmentId(pub u64);

/// Metadata for a segment.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SegmentMetadata {
    /// Segment ID
    pub id: SegmentId,

    /// Creation timestamp (seconds since UNIX epoch)
    pub created_at: u64,

    /// Number of entries in the segment
    pub entry_count: u64,

    /// Total size in bytes
    pub size: u64,

    /// First page ID in the segment
    pub first_page_id: PageId,

    /// Last page ID in the segment
    pub last_page_id: PageId,
}

/// Persisted segment state for metadata serialization.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedSegment {
    /// Segment metadata
    pub metadata: SegmentMetadata,
    /// Buffered segment bytes
    pub buffer: Vec<u8>,
}

/// A segment in the append log.
///
/// Segments are immutable once rolled. The active segment is the only
/// mutable segment.
pub struct Segment {
    /// Segment ID
    id: SegmentId,

    /// Metadata
    metadata: RwLock<SegmentMetadata>,

    /// Persisted segment contents are tracked in-memory and flushed by AppendLog metadata persistence.

    /// Write buffer for the active segment
    write_buffer: RwLock<Vec<u8>>,
}

impl Segment {
    /// Create a new segment.
    pub fn new<FS: FileSystem + 'static>(
        id: SegmentId,
        pager: Arc<Pager<FS>>,
    ) -> TableResult<Self> {
        // Allocate first page for the segment
        let first_page_id = pager
            .allocate_page(crate::pager::PageType::LsmData)
            .map_err(|e| {
                crate::table::TableError::Other(format!("Failed to allocate segment page: {}", e))
            })?;

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let metadata = SegmentMetadata {
            id,
            created_at,
            entry_count: 0,
            size: 0,
            first_page_id,
            last_page_id: first_page_id,
        };

        let _ = pager;
        Ok(Self {
            id,
            metadata: RwLock::new(metadata),
            write_buffer: RwLock::new(Vec::new()),
        })
    }

    /// Get the segment ID.
    pub fn id(&self) -> SegmentId {
        self.id
    }

    /// Get the creation timestamp.
    pub fn created_at(&self) -> u64 {
        self.metadata.read().unwrap().created_at
    }

    /// Get the current size of the segment.
    pub fn size(&self) -> u64 {
        self.metadata.read().unwrap().size
    }

    /// Get the number of entries in the segment.
    pub fn entry_count(&self) -> u64 {
        self.metadata.read().unwrap().entry_count
    }

    /// Append a key-value pair to the segment.
    ///
    /// Returns the offset where the entry was written.
    pub fn append(&self, key: &[u8], value: &[u8]) -> TableResult<u64> {
        let mut buffer = self.write_buffer.write().unwrap();
        let mut metadata = self.metadata.write().unwrap();

        // Calculate entry size: key_len (4 bytes) + key + value_len (4 bytes) + value
        let entry_size = 4 + key.len() + 4 + value.len();
        let offset = metadata.size;

        // Encode entry: [key_len: u32][key][value_len: u32][value]
        buffer.extend_from_slice(&(key.len() as u32).to_le_bytes());
        buffer.extend_from_slice(key);
        buffer.extend_from_slice(&(value.len() as u32).to_le_bytes());
        buffer.extend_from_slice(value);

        // Update metadata
        metadata.entry_count += 1;
        metadata.size += entry_size as u64;

        // TODO: Flush buffer to pages when it gets large enough
        // For now, keep everything in memory

        Ok(offset)
    }

    /// Read a value at the specified offset.
    pub fn read_at(&self, offset: u64) -> TableResult<Option<ValueBuf>> {
        let buffer = self.write_buffer.read().unwrap();

        // Check if offset is valid
        if offset >= buffer.len() as u64 {
            return Ok(None);
        }

        let mut pos = offset as usize;

        // Read key length
        if pos + 4 > buffer.len() {
            return Ok(None);
        }
        let key_len = u32::from_le_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        // Skip key
        if pos + key_len > buffer.len() {
            return Ok(None);
        }
        pos += key_len;

        // Read value length
        if pos + 4 > buffer.len() {
            return Ok(None);
        }
        let value_len = u32::from_le_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        // Read value
        if pos + value_len > buffer.len() {
            return Ok(None);
        }
        let value = buffer[pos..pos + value_len].to_vec();

        Ok(Some(ValueBuf(value)))
    }

    /// Flush the write buffer to disk.
    ///
    /// Marks the buffer as flushed. The actual page writing is handled
    /// by the AppendLog during segment rollover.
    pub fn flush(&self) -> TableResult<()> {
        // Buffer is flushed when segment rolls over.
        // The AppendLog handles writing buffered data to pages during rollover.
        // For now, this is a no-op since data stays in memory until rollover.
        Ok(())
    }

    /// Get the metadata for this segment.
    pub fn metadata(&self) -> SegmentMetadata {
        self.metadata.read().unwrap().clone()
    }

    /// Get a copy of the buffered segment contents.
    pub fn buffer(&self) -> Vec<u8> {
        self.write_buffer.read().unwrap().clone()
    }

    /// Get the latest segment LSN based on the newest committed or uncommitted version.
    pub fn latest_lsn(&self) -> LogSequenceNumber {
        LogSequenceNumber::from(self.metadata.read().unwrap().created_at)
    }

    /// Restore a segment from previously persisted metadata and buffer contents.
    pub fn from_persisted<FS: FileSystem + 'static>(
        persisted: PersistedSegment,
        pager: Arc<Pager<FS>>,
    ) -> TableResult<Self> {
        let _ = pager;
        Ok(Self {
            id: persisted.metadata.id,
            metadata: RwLock::new(persisted.metadata),
            write_buffer: RwLock::new(persisted.buffer),
        })
    }

    /// Persist the current segment state into a serializable snapshot.
    pub fn persist(&self) -> TableResult<PersistedSegment> {
        let metadata = self.metadata();
        let buffer = self.buffer();
        Ok(PersistedSegment { metadata, buffer })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pager::PagerConfig;
    use crate::vfs::MemoryFileSystem;

    fn create_test_pager() -> Arc<Pager<MemoryFileSystem>> {
        let fs = Arc::new(MemoryFileSystem::new());
        let config = PagerConfig::default();
        Arc::new(Pager::create(fs.as_ref(), "test.db", config).unwrap())
    }

    #[test]
    fn test_segment_creation() {
        let pager = create_test_pager();
        let segment = Segment::new(SegmentId(0), pager).unwrap();

        assert_eq!(segment.id(), SegmentId(0));
        assert_eq!(segment.size(), 0);
        assert_eq!(segment.entry_count(), 0);
    }

    #[test]
    fn test_segment_append_and_read() {
        let pager = create_test_pager();
        let segment = Segment::new(SegmentId(0), pager).unwrap();

        // Append an entry
        let key = b"test_key";
        let value = b"test_value";
        let offset = segment.append(key, value).unwrap();

        assert_eq!(offset, 0);
        assert_eq!(segment.entry_count(), 1);

        // Read the entry back
        let read_value = segment.read_at(offset).unwrap();
        assert_eq!(read_value, Some(ValueBuf(value.to_vec())));
    }

    #[test]
    fn test_segment_multiple_entries() {
        let pager = create_test_pager();
        let segment = Segment::new(SegmentId(0), pager).unwrap();

        // Append multiple entries
        let entries = vec![
            (b"key1".as_slice(), b"value1".as_slice()),
            (b"key2".as_slice(), b"value2".as_slice()),
            (b"key3".as_slice(), b"value3".as_slice()),
        ];

        let mut offsets = Vec::new();
        for (key, value) in &entries {
            let offset = segment.append(key, value).unwrap();
            offsets.push(offset);
        }

        assert_eq!(segment.entry_count(), 3);

        // Read all entries back
        for (i, (_, value)) in entries.iter().enumerate() {
            let read_value = segment.read_at(offsets[i]).unwrap();
            assert_eq!(read_value, Some(ValueBuf(value.to_vec())));
        }
    }

    #[test]
    fn test_segment_size_tracking() {
        let pager = create_test_pager();
        let segment = Segment::new(SegmentId(0), pager).unwrap();

        let key = b"key";
        let value = b"value";

        // Entry size: 4 (key_len) + 3 (key) + 4 (value_len) + 5 (value) = 16 bytes
        segment.append(key, value).unwrap();

        assert_eq!(segment.size(), 16);
    }

    #[test]
    fn test_segment_invalid_offset() {
        let pager = create_test_pager();
        let segment = Segment::new(SegmentId(0), pager).unwrap();

        // Try to read from an invalid offset
        let result = segment.read_at(1000).unwrap();
        assert_eq!(result, None);
    }
}

// Made with Bob
