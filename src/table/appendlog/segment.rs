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
    /// Buffered segment bytes not yet flushed to pages
    pub buffer: Vec<u8>,
    /// Fully flushed data pages in order
    pub flushed_pages: Vec<PageId>,
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

    /// Pager for persistent storage
    pager: Arc<dyn SegmentPager>,

    /// Data pages that hold flushed bytes
    flushed_pages: RwLock<Vec<PageId>>,

    /// Write buffer for the active segment
    write_buffer: RwLock<Vec<u8>>,
}

trait SegmentPager: Send + Sync {
    fn allocate_page(&self, page_type: PageType) -> TableResult<PageId>;
    fn write_page(&self, page: &Page) -> TableResult<()>;
    fn read_page(&self, page_id: PageId) -> TableResult<Page>;
    fn data_size(&self) -> usize;
}

impl<FS: FileSystem + 'static> SegmentPager for Pager<FS> {
    fn allocate_page(&self, page_type: PageType) -> TableResult<PageId> {
        Pager::allocate_page(self, page_type).map_err(TableError::from)
    }

    fn write_page(&self, page: &Page) -> TableResult<()> {
        Pager::write_page(self, page).map_err(TableError::from)
    }

    fn read_page(&self, page_id: PageId) -> TableResult<Page> {
        Pager::read_page(self, page_id).map_err(TableError::from)
    }

    fn data_size(&self) -> usize {
        self.page_size().data_size()
    }
}

impl Segment {
    const PAGE_HEADER_SIZE: usize = 16;

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
        let pager: Arc<dyn SegmentPager> = pager;

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

        Ok(Self {
            id,
            metadata: RwLock::new(metadata),
            pager,
            flushed_pages: RwLock::new(Vec::new()),
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
    pub fn append(&self, key: &[u8], value: &[u8], flush_threshold: usize) -> TableResult<u64> {
        let mut buffer = self.write_buffer.write().unwrap();
        let mut metadata = self.metadata.write().unwrap();

        let entry_size = 4 + key.len() + 4 + value.len();
        let offset = metadata.size;

        buffer.extend_from_slice(&(key.len() as u32).to_le_bytes());
        buffer.extend_from_slice(key);
        buffer.extend_from_slice(&(value.len() as u32).to_le_bytes());
        buffer.extend_from_slice(value);

        metadata.entry_count += 1;
        metadata.size += entry_size as u64;

        let should_flush = buffer.len() >= flush_threshold;
        drop(metadata);

        if should_flush {
            self.flush_locked(&mut buffer)?;
        }

        Ok(offset)
    }

    /// Read a value at the specified offset.
    pub fn read_at(&self, offset: u64) -> TableResult<Option<ValueBuf>> {
        let total_size = self.metadata.read().unwrap().size;
        if offset >= total_size {
            return Ok(None);
        }

        let flushed_len = self.flushed_len()?;
        if offset < flushed_len as u64 {
            return self.read_at_from_flushed(offset, flushed_len);
        }

        let buffer = self.write_buffer.read().unwrap();
        let in_memory_offset = offset.checked_sub(flushed_len as u64).ok_or_else(|| {
            TableError::corruption(
                "appendlog-segment",
                "offset-underflow",
                "offset before flushed prefix",
            )
        })? as usize;

        Self::decode_value_at(&buffer, in_memory_offset)
    }

    /// Flush the write buffer to disk.
    pub fn flush(&self) -> TableResult<()> {
        let mut buffer = self.write_buffer.write().unwrap();
        self.flush_locked(&mut buffer)
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

    /// Get a copy of the flushed page IDs.
    pub fn flushed_pages(&self) -> Vec<PageId> {
        self.flushed_pages.read().unwrap().clone()
    }

    fn flush_locked(&self, buffer: &mut Vec<u8>) -> TableResult<()> {
        let page_payload_size = self.page_payload_size();
        if page_payload_size == 0 || buffer.is_empty() {
            return Ok(());
        }

        let flush_len = if buffer.len() < page_payload_size {
            buffer.len()
        } else {
            (buffer.len() / page_payload_size) * page_payload_size
        };
        let bytes_to_flush = buffer[..flush_len].to_vec();
        let new_pages = self.write_bytes_to_pages(&bytes_to_flush)?;

        {
            let mut pages = self.flushed_pages.write().unwrap();
            pages.extend(new_pages);
            let mut metadata = self.metadata.write().unwrap();
            if let Some(last_page_id) = pages.last().copied() {
                metadata.last_page_id = last_page_id;
            }
        }

        buffer.drain(..flush_len);
        Ok(())
    }

    fn flushed_len(&self) -> TableResult<usize> {
        let page_ids = self.flushed_pages.read().unwrap().clone();
        let mut total = 0usize;

        for page_id in page_ids {
            let page = self.pager.read_page(page_id)?;
            if page.data.len() < Self::PAGE_HEADER_SIZE {
                return Err(TableError::corruption(
                    format!("appendlog-segment-page-{}", page_id.as_u64()),
                    "short-page",
                    "segment page missing appendlog payload header",
                ));
            }

            let chunk_len = u64::from_le_bytes(page.data[0..8].try_into().unwrap()) as usize;
            let available = page.data.len().saturating_sub(Self::PAGE_HEADER_SIZE);
            if chunk_len > available {
                return Err(TableError::corruption(
                    format!("appendlog-segment-page-{}", page_id.as_u64()),
                    "invalid-chunk-len",
                    format!("chunk length {} exceeds available {}", chunk_len, available),
                ));
            }

            total += chunk_len;
        }

        Ok(total)
    }

    fn page_payload_size(&self) -> usize {
        self.pager
            .data_size()
            .saturating_sub(Self::PAGE_HEADER_SIZE)
    }

    fn write_bytes_to_pages(&self, bytes: &[u8]) -> TableResult<Vec<PageId>> {
        let payload_size = self.page_payload_size();
        if payload_size == 0 {
            return Err(TableError::Other(
                "AppendLog segment page payload size is zero".to_string(),
            ));
        }

        let total_pages = bytes.len().div_ceil(payload_size);
        let mut written_pages = Vec::with_capacity(total_pages);

        for (page_index, chunk) in bytes.chunks(payload_size).enumerate() {
            let page_id = self.pager.allocate_page(PageType::LsmData)?;
            let mut page = Page::new(page_id, PageType::LsmData, self.pager.data_size());
            let next_page = if page_index + 1 < total_pages {
                1u64
            } else {
                0u64
            };

            page.data
                .extend_from_slice(&(chunk.len() as u64).to_le_bytes());
            page.data.extend_from_slice(&next_page.to_le_bytes());
            page.data.extend_from_slice(chunk);
            self.pager.write_page(&page)?;
            written_pages.push(page_id);
        }

        Ok(written_pages)
    }

    fn read_at_from_flushed(
        &self,
        offset: u64,
        flushed_len: usize,
    ) -> TableResult<Option<ValueBuf>> {
        let mut flushed_bytes = Vec::with_capacity(flushed_len);
        let page_ids = self.flushed_pages.read().unwrap().clone();

        for page_id in page_ids {
            let page = self.pager.read_page(page_id)?;
            if page.data.len() < Self::PAGE_HEADER_SIZE {
                return Err(TableError::corruption(
                    format!("appendlog-segment-page-{}", page_id.as_u64()),
                    "short-page",
                    "segment page missing appendlog payload header",
                ));
            }

            let chunk_len = u64::from_le_bytes(page.data[0..8].try_into().unwrap()) as usize;
            let available = page.data.len().saturating_sub(Self::PAGE_HEADER_SIZE);
            if chunk_len > available {
                return Err(TableError::corruption(
                    format!("appendlog-segment-page-{}", page_id.as_u64()),
                    "invalid-chunk-len",
                    format!("chunk length {} exceeds available {}", chunk_len, available),
                ));
            }

            flushed_bytes.extend_from_slice(
                &page.data[Self::PAGE_HEADER_SIZE..Self::PAGE_HEADER_SIZE + chunk_len],
            );
        }

        Self::decode_value_at(&flushed_bytes, offset as usize)
    }

    fn decode_value_at(bytes: &[u8], offset: usize) -> TableResult<Option<ValueBuf>> {
        if offset >= bytes.len() {
            return Ok(None);
        }

        let mut pos = offset;

        if pos + 4 > bytes.len() {
            return Ok(None);
        }
        let key_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        if pos + key_len > bytes.len() {
            return Ok(None);
        }
        pos += key_len;

        if pos + 4 > bytes.len() {
            return Ok(None);
        }
        let value_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        if pos + value_len > bytes.len() {
            return Ok(None);
        }

        Ok(Some(ValueBuf(bytes[pos..pos + value_len].to_vec())))
    }

    /// Restore a segment from previously persisted metadata and buffer contents.
    pub fn from_persisted<FS: FileSystem + 'static>(
        persisted: PersistedSegment,
        pager: Arc<Pager<FS>>,
    ) -> TableResult<Self> {
        Ok(Self {
            id: persisted.metadata.id,
            metadata: RwLock::new(persisted.metadata),
            pager,
            flushed_pages: RwLock::new(persisted.flushed_pages),
            write_buffer: RwLock::new(persisted.buffer),
        })
    }

    /// Persist the current segment state into a serializable snapshot.
    pub fn persist(&self) -> TableResult<PersistedSegment> {
        let metadata = self.metadata();
        let buffer = self.buffer();
        let flushed_pages = self.flushed_pages.read().unwrap().clone();
        Ok(PersistedSegment {
            metadata,
            buffer,
            flushed_pages,
        })
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
        let offset = segment.append(key, value, usize::MAX).unwrap();

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
            let offset = segment.append(key, value, usize::MAX).unwrap();
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
        segment.append(key, value, usize::MAX).unwrap();

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
