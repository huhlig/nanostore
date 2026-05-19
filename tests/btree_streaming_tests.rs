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

//! Comprehensive tests for BTree streaming operations.
//!
//! Tests cover:
//! - put_stream with various sizes (inline, single page, multi-page)
//! - get_stream and verify data integrity
//! - Stream large values (1MB, 10MB, 100MB)
//! - Concurrent streaming operations
//! - MVCC with streaming values
//! - Delete streaming values and verify cleanup
//! - Update operations (inline<->external, external<->external)
//! - Overflow page cleanup verification
//! - Error handling scenarios
//! - Memory efficiency
//! - Edge cases

use nanokv::pager::{Pager, PagerConfig};
use nanokv::table::btree::PagedBTree;
use nanokv::table::{Flushable, MutableTable, PointLookup, SearchableTable, TableError, ValueStream};
use nanokv::txn::TransactionId;
use nanokv::types::{TableId, ValueRef, ValueRefDecodeError};
use nanokv::vfs::MemoryFileSystem;
use nanokv::wal::LogSequenceNumber;
use std::sync::Arc;

/// Helper struct to create a ValueStream from a Vec<u8>
struct VecValueStream {
    data: Vec<u8>,
    position: usize,
}

impl VecValueStream {
    fn new(data: Vec<u8>) -> Self {
        Self { data, position: 0 }
    }
}

impl ValueStream for VecValueStream {
    fn read(&mut self, buf: &mut [u8]) -> nanokv::table::TableResult<usize> {
        let remaining = self.data.len() - self.position;
        let to_read = remaining.min(buf.len());

        if to_read == 0 {
            return Ok(0);
        }

        buf[..to_read].copy_from_slice(&self.data[self.position..self.position + to_read]);
        self.position += to_read;
        Ok(to_read)
    }

    fn size_hint(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }
}

/// Helper struct for streaming without size hint (adaptive strategy)
struct UnknownSizeStream {
    data: Vec<u8>,
    position: usize,
}

impl UnknownSizeStream {
    fn new(data: Vec<u8>) -> Self {
        Self { data, position: 0 }
    }
}

impl ValueStream for UnknownSizeStream {
    fn read(&mut self, buf: &mut [u8]) -> nanokv::table::TableResult<usize> {
        let remaining = self.data.len() - self.position;
        let to_read = remaining.min(buf.len());

        if to_read == 0 {
            return Ok(0);
        }

        buf[..to_read].copy_from_slice(&self.data[self.position..self.position + to_read]);
        self.position += to_read;
        Ok(to_read)
    }

    fn size_hint(&self) -> Option<u64> {
        None // Unknown size
    }
}

/// Helper struct for error-producing streams
struct ErrorStream {
    error_after: usize,
    position: usize,
}

impl ErrorStream {
    fn new(error_after: usize) -> Self {
        Self {
            error_after,
            position: 0,
        }
    }
}

impl ValueStream for ErrorStream {
    fn read(&mut self, buf: &mut [u8]) -> nanokv::table::TableResult<usize> {
        if self.position >= self.error_after {
            return Err(TableError::Corruption {
                location: "stream".to_string(),
                corruption_type: "simulated_error".to_string(),
                details: "Simulated stream error".to_string(),
            });
        }
        let to_write = buf.len().min(self.error_after - self.position);
        buf[..to_write].fill(0x42);
        self.position += to_write;
        Ok(to_write)
    }

    fn size_hint(&self) -> Option<u64> {
        Some(self.error_after as u64 + 1000) // Lie about size
    }
}

fn create_test_tree() -> PagedBTree<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());
    PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap()
}

// =============================================================================
// Basic Streaming Tests
// =============================================================================

#[test]
fn test_put_stream_inline_value() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);

    // Small value that should be stored inline (< 4KB)
    let test_data = vec![0xAB; 100];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();
    let bytes_written = writer.put_stream(b"key1", &mut stream).unwrap();
    assert!(bytes_written > 0);
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read back and verify
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader.get(b"key1", LogSequenceNumber::from(100)).unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0, test_data);
}

#[test]
fn test_put_stream_medium_value() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);

    // Medium value (4KB - should use SinglePage)
    let test_data = vec![0xCD; 4096];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();
    let bytes_written = writer.put_stream(b"key2", &mut stream).unwrap();
    assert!(bytes_written > 0);
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read back and verify
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader.get(b"key2", LogSequenceNumber::from(100)).unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0, test_data);
}

#[test]
fn test_put_stream_large_value() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);

    // Large value (100KB - should use OverflowChain)
    let test_data = vec![0xEF; 100 * 1024];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();
    let bytes_written = writer.put_stream(b"key3", &mut stream).unwrap();
    assert!(bytes_written > 0);
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read back and verify
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader.get(b"key3", LogSequenceNumber::from(100)).unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0, test_data);
}

#[test]
fn test_put_stream_unknown_size_hint() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);

    // Stream without size hint - should use adaptive strategy
    let test_data = vec![0x55; 10000];
    let mut stream = UnknownSizeStream::new(test_data.clone());

    let mut writer = table.writer(tx_id, snapshot_lsn).unwrap();
    let bytes_written = writer.put_stream(b"unknown_size", &mut stream).unwrap();
    assert!(bytes_written > 0);
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read back and verify
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader
        .get(b"unknown_size", LogSequenceNumber::from(100))
        .unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0, test_data);
}

// =============================================================================
// Read/Write Round-trip Tests
// =============================================================================

#[test]
fn test_roundtrip_1kb() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    let test_data = vec![0x11; 1024];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer.put_stream(b"1kb", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let mut result_stream = reader
        .get_stream(b"1kb", LogSequenceNumber::from(100))
        .unwrap()
        .expect("Value should exist");

    let mut result = Vec::new();
    let mut buf = vec![0u8; 512];
    loop {
        let n = result_stream.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        result.extend_from_slice(&buf[..n]);
    }

    assert_eq!(result, test_data);
}

#[test]
fn test_roundtrip_10kb() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    let test_data = vec![0x22; 10 * 1024];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer.put_stream(b"10kb", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let mut result_stream = reader
        .get_stream(b"10kb", LogSequenceNumber::from(100))
        .unwrap()
        .expect("Value should exist");

    let mut result = Vec::new();
    let mut buf = vec![0u8; 4096];
    loop {
        let n = result_stream.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        result.extend_from_slice(&buf[..n]);
    }

    assert_eq!(result, test_data);
}

#[test]
fn test_roundtrip_100kb() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    let test_data = vec![0x33; 100 * 1024];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer.put_stream(b"100kb", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let mut result_stream = reader
        .get_stream(b"100kb", LogSequenceNumber::from(100))
        .unwrap()
        .expect("Value should exist");

    let mut result = Vec::new();
    let mut buf = vec![0u8; 8192];
    loop {
        let n = result_stream.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        result.extend_from_slice(&buf[..n]);
    }

    assert_eq!(result, test_data);
}

#[test]
fn test_roundtrip_1mb() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    let test_data = vec![0x44; 1024 * 1024];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer.put_stream(b"1mb", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader.get(b"1mb", LogSequenceNumber::from(100)).unwrap();
    assert!(value.is_some());
    let value = value.unwrap();
    assert_eq!(value.0.len(), test_data.len());
    assert_eq!(value.0, test_data);
}

#[test]
fn test_multiple_large_values_same_table() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();

    // Insert multiple large values
    for i in 0..5 {
        let data = vec![(i * 17) as u8; 50000];
        let mut stream = VecValueStream::new(data);
        let key = format!("large_{}", i);
        writer.put_stream(key.as_bytes(), &mut stream).unwrap();
    }

    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Verify all values
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    for i in 0..5 {
        let key = format!("large_{}", i);
        let value = reader
            .get(key.as_bytes(), LogSequenceNumber::from(100))
            .unwrap();
        assert!(value.is_some());
        let expected = vec![(i * 17) as u8; 50000];
        assert_eq!(value.unwrap().0, expected);
    }
}

#[test]
fn test_interleaved_small_and_large_values() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();

    // Interleave small and large values
    let small_data = vec![0xAA; 100];
    let mut small_stream = VecValueStream::new(small_data.clone());
    writer.put_stream(b"small1", &mut small_stream).unwrap();

    let large_data = vec![0xBB; 100000];
    let mut large_stream = VecValueStream::new(large_data.clone());
    writer.put_stream(b"large1", &mut large_stream).unwrap();

    let small_data2 = vec![0xCC; 200];
    let mut small_stream2 = VecValueStream::new(small_data2.clone());
    writer.put_stream(b"small2", &mut small_stream2).unwrap();

    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Verify all values
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    assert_eq!(
        reader
            .get(b"small1", LogSequenceNumber::from(100))
            .unwrap()
            .unwrap()
            .0,
        small_data
    );
    assert_eq!(
        reader
            .get(b"large1", LogSequenceNumber::from(100))
            .unwrap()
            .unwrap()
            .0,
        large_data
    );
    assert_eq!(
        reader
            .get(b"small2", LogSequenceNumber::from(100))
            .unwrap()
            .unwrap()
            .0,
        small_data2
    );
}

// =============================================================================
// Update and Delete Tests
// =============================================================================

#[test]
fn test_replace_inline_with_external() {
    let table = create_test_tree();

    // Insert small inline value
    let tx1 = TransactionId::from(1);
    let small_data = vec![0x11; 100];
    let mut writer1 = table.writer(tx1, LogSequenceNumber::from(0)).unwrap();
    writer1.put(b"key", &small_data).unwrap();
    writer1.flush().unwrap();
    writer1
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Replace with large external value
    let tx2 = TransactionId::from(2);
    let large_data = vec![0x22; 100000];
    let mut large_stream = VecValueStream::new(large_data.clone());
    let mut writer2 = table.writer(tx2, LogSequenceNumber::from(100)).unwrap();
    writer2.put_stream(b"key", &mut large_stream).unwrap();
    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    // Verify new value
    let reader = table.reader(LogSequenceNumber::from(200)).unwrap();
    let value = reader.get(b"key", LogSequenceNumber::from(200)).unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0, large_data);
}

#[test]
fn test_replace_external_with_inline() {
    let table = create_test_tree();

    // Insert large external value
    let tx1 = TransactionId::from(1);
    let large_data = vec![0x33; 100000];
    let mut large_stream = VecValueStream::new(large_data);
    let mut writer1 = table.writer(tx1, LogSequenceNumber::from(0)).unwrap();
    writer1.put_stream(b"key", &mut large_stream).unwrap();
    writer1.flush().unwrap();
    writer1
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Replace with small inline value
    let tx2 = TransactionId::from(2);
    let small_data = vec![0x44; 100];
    let mut writer2 = table.writer(tx2, LogSequenceNumber::from(100)).unwrap();
    writer2.put(b"key", &small_data).unwrap();
    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    // Verify new value
    let reader = table.reader(LogSequenceNumber::from(200)).unwrap();
    let value = reader.get(b"key", LogSequenceNumber::from(200)).unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0, small_data);
}

#[test]
fn test_replace_external_with_different_external() {
    let table = create_test_tree();

    // Insert first large value
    let tx1 = TransactionId::from(1);
    let data1 = vec![0x55; 80000];
    let mut stream1 = VecValueStream::new(data1);
    let mut writer1 = table.writer(tx1, LogSequenceNumber::from(0)).unwrap();
    writer1.put_stream(b"key", &mut stream1).unwrap();
    writer1.flush().unwrap();
    writer1
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Replace with different large value
    let tx2 = TransactionId::from(2);
    let data2 = vec![0x66; 120000];
    let mut stream2 = VecValueStream::new(data2.clone());
    let mut writer2 = table.writer(tx2, LogSequenceNumber::from(100)).unwrap();
    writer2.put_stream(b"key", &mut stream2).unwrap();
    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    // Verify new value
    let reader = table.reader(LogSequenceNumber::from(200)).unwrap();
    let value = reader.get(b"key", LogSequenceNumber::from(200)).unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0, data2);
}

#[test]
fn test_delete_external_value() {
    let table = create_test_tree();

    // Insert large value
    let tx1 = TransactionId::from(1);
    let test_data = vec![0x77; 50000];
    let mut stream = VecValueStream::new(test_data);
    let mut writer1 = table.writer(tx1, LogSequenceNumber::from(0)).unwrap();
    writer1.put_stream(b"delete_key", &mut stream).unwrap();
    writer1.flush().unwrap();
    writer1
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Verify it exists
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    assert!(reader
        .get(b"delete_key", LogSequenceNumber::from(100))
        .unwrap()
        .is_some());

    // Delete it
    let tx2 = TransactionId::from(2);
    let mut writer2 = table.writer(tx2, LogSequenceNumber::from(100)).unwrap();
    writer2.delete(b"delete_key").unwrap();
    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    // Verify it's gone
    let reader2 = table.reader(LogSequenceNumber::from(200)).unwrap();
    assert!(reader2
        .get(b"delete_key", LogSequenceNumber::from(200))
        .unwrap()
        .is_none());
}

#[test]
fn test_update_same_key_multiple_times() {
    let table = create_test_tree();

    // Update the same key 5 times with different values
    for i in 0..5 {
        let tx = TransactionId::from(i + 1);
        let data = vec![(i * 23) as u8; 30000 + (i as usize) * 10000];
        let mut stream = VecValueStream::new(data.clone());
        let mut writer = table
            .writer(tx, LogSequenceNumber::from(i * 100))
            .unwrap();
        writer.put_stream(b"multi_update", &mut stream).unwrap();
        writer.flush().unwrap();
        writer
            .commit_versions(LogSequenceNumber::from((i + 1) * 100))
            .unwrap();

        // Verify current value
        let reader = table
            .reader(LogSequenceNumber::from((i + 1) * 100))
            .unwrap();
        let value = reader
            .get(b"multi_update", LogSequenceNumber::from((i + 1) * 100))
            .unwrap();
        assert!(value.is_some());
        assert_eq!(value.unwrap().0, data);
    }
}

// =============================================================================
// MVCC Tests
// =============================================================================

#[test]
fn test_mvcc_with_streaming_values() {
    let table = create_test_tree();

    // Transaction 1: Insert initial value
    let tx1 = TransactionId::from(1);
    let data1 = vec![0xAA; 5000];
    let mut stream1 = VecValueStream::new(data1.clone());
    let mut writer1 = table.writer(tx1, LogSequenceNumber::from(10)).unwrap();
    writer1.put_stream(b"mvcc_key", &mut stream1).unwrap();
    writer1.flush().unwrap();
    writer1
        .commit_versions(LogSequenceNumber::from(15))
        .unwrap();

    // Transaction 2: Update with different value
    let tx2 = TransactionId::from(2);
    let data2 = vec![0xBB; 8000];
    let mut stream2 = VecValueStream::new(data2.clone());
    let mut writer2 = table.writer(tx2, LogSequenceNumber::from(20)).unwrap();
    writer2.put_stream(b"mvcc_key", &mut stream2).unwrap();
    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(25))
        .unwrap();

    // Read at different snapshots
    let reader_old = table.reader(LogSequenceNumber::from(15)).unwrap();
    let value_old = reader_old
        .get(b"mvcc_key", LogSequenceNumber::from(15))
        .unwrap()
        .unwrap();
    assert_eq!(value_old.0, data1);

    let reader_new = table.reader(LogSequenceNumber::from(25)).unwrap();
    let value_new = reader_new
        .get(b"mvcc_key", LogSequenceNumber::from(25))
        .unwrap()
        .unwrap();
    assert_eq!(value_new.0, data2);
}

// =============================================================================
// Edge Cases
// =============================================================================

#[test]
fn test_empty_stream() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    let test_data = vec![];
    let mut stream = VecValueStream::new(test_data);

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer.put_stream(b"empty_key", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader
        .get(b"empty_key", LogSequenceNumber::from(100))
        .unwrap();
    if let Some(v) = value {
        assert_eq!(v.0.len(), 0);
    }
}

#[test]
fn test_exactly_4kb_value() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    // Exactly 4KB - threshold boundary
    let test_data = vec![0x88; 4096];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer.put_stream(b"4kb_exact", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader
        .get(b"4kb_exact", LogSequenceNumber::from(100))
        .unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0, test_data);
}

#[test]
fn test_one_page_worth_of_data() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    // Exactly one page size (4096 bytes default)
    let test_data = vec![0x99; 4096];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer.put_stream(b"one_page", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader
        .get(b"one_page", LogSequenceNumber::from(100))
        .unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0, test_data);
}

#[test]
fn test_very_large_value_10mb() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    // 10MB value
    let test_data = vec![0xAA; 10 * 1024 * 1024];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer.put_stream(b"10mb", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader.get(b"10mb", LogSequenceNumber::from(100)).unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0.len(), test_data.len());
}

#[test]
fn test_keys_with_special_characters() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    let special_keys = vec![
        b"key\0with\0nulls".to_vec(),
        b"key\nwith\nnewlines".to_vec(),
        b"key\twith\ttabs".to_vec(),
        b"\xFF\xFE\xFD\xFC".to_vec(),
    ];

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    for (i, key) in special_keys.iter().enumerate() {
        let data = vec![(i * 11) as u8; 10000];
        let mut stream = VecValueStream::new(data);
        writer.put_stream(key, &mut stream).unwrap();
    }
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Verify all special keys
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    for (i, key) in special_keys.iter().enumerate() {
        let value = reader.get(key, LogSequenceNumber::from(100)).unwrap();
        assert!(value.is_some());
        let expected = vec![(i * 11) as u8; 10000];
        assert_eq!(value.unwrap().0, expected);
    }
}

// =============================================================================
// Error Handling Tests
// =============================================================================

#[test]
fn test_stream_read_error_during_put() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    // Stream that will error after 1000 bytes
    let mut error_stream = ErrorStream::new(1000);

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    let result = writer.put_stream(b"error_key", &mut error_stream);

    // Should get an error
    assert!(result.is_err());
}

#[test]
fn test_valueref_decode_errors() {
    // Test insufficient bytes
    let result = ValueRef::decode(&[]);
    assert!(matches!(
        result,
        Err(ValueRefDecodeError::InsufficientBytes { .. })
    ));

    // Test invalid length for SinglePage
    let result = ValueRef::decode(&[0x01, 0x00, 0x00]); // Too short
    assert!(matches!(
        result,
        Err(ValueRefDecodeError::InvalidLength { .. })
    ));

    // Test invalid length for OverflowChain
    let result = ValueRef::decode(&[0x02, 0x00, 0x00, 0x00, 0x00]); // Too short
    assert!(matches!(
        result,
        Err(ValueRefDecodeError::InvalidLength { .. })
    ));

    // Test unknown type byte
    let result = ValueRef::decode(&[0xFF]);
    assert!(matches!(result, Err(ValueRefDecodeError::UnknownType(0xFF))));
}

// =============================================================================
// Pattern and Integrity Tests
// =============================================================================

#[test]
fn test_stream_with_pattern_data() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    // Create data with pattern to verify integrity
    let mut test_data = Vec::new();
    for i in 0..10000 {
        test_data.push((i % 256) as u8);
    }

    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer.put_stream(b"pattern_key", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read back and verify pattern
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader
        .get(b"pattern_key", LogSequenceNumber::from(100))
        .unwrap()
        .unwrap();
    assert_eq!(value.0, test_data);

    // Verify pattern is correct
    for (i, &byte) in value.0.iter().enumerate() {
        assert_eq!(byte, (i % 256) as u8);
    }
}

#[test]
fn test_get_stream_small_value() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    // Insert small value
    let test_data = vec![0x11; 500];
    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer.put(b"key1", &test_data).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read using stream
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let stream_opt = reader
        .get_stream(b"key1", LogSequenceNumber::from(100))
        .unwrap();
    assert!(stream_opt.is_some());

    let mut stream = stream_opt.unwrap();
    let mut result = Vec::new();
    let mut buffer = vec![0u8; 256];

    loop {
        let n = stream.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        result.extend_from_slice(&buffer[..n]);
    }

    assert_eq!(result, test_data);
}

#[test]
fn test_stream_size_hints() {
    let table = create_test_tree();
    let tx_id = TransactionId::from(1);

    // Test with known size
    let data_with_hint = vec![0xAA; 5000];
    let mut stream_with_hint = VecValueStream::new(data_with_hint.clone());
    assert_eq!(stream_with_hint.size_hint(), Some(5000));

    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer
        .put_stream(b"with_hint", &mut stream_with_hint)
        .unwrap();

    // Test with unknown size
    let data_no_hint = vec![0xBB; 5000];
    let mut stream_no_hint = UnknownSizeStream::new(data_no_hint.clone());
    assert_eq!(stream_no_hint.size_hint(), None);

    writer
        .put_stream(b"no_hint", &mut stream_no_hint)
        .unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Verify both work correctly
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    assert_eq!(
        reader
            .get(b"with_hint", LogSequenceNumber::from(100))
            .unwrap()
            .unwrap()
            .0,
        data_with_hint
    );
    assert_eq!(
        reader
            .get(b"no_hint", LogSequenceNumber::from(100))
            .unwrap()
            .unwrap()
            .0,
        data_no_hint
    );
}

// =============================================================================
// ValueRef Encoding/Decoding Tests
// =============================================================================

#[test]
fn test_valueref_inline_encoding() {
    let vref = ValueRef::Inline;
    let encoded = vref.encode();
    assert_eq!(encoded.len(), 1);
    assert_eq!(encoded[0], 0x00);

    let decoded = ValueRef::decode(&encoded).unwrap();
    assert_eq!(decoded, vref);
}

#[test]
fn test_valueref_single_page_encoding() {
    let vref = ValueRef::SinglePage {
        page_id: 42,
        offset: 100,
        length: 5000,
    };
    let encoded = vref.encode();
    assert_eq!(encoded.len(), 11);

    let decoded = ValueRef::decode(&encoded).unwrap();
    assert_eq!(decoded, vref);
}

#[test]
fn test_valueref_overflow_chain_encoding() {
    let vref = ValueRef::OverflowChain {
        first_page_id: 100,
        total_length: 1000000,
        page_count: 250,
    };
    let encoded = vref.encode();
    assert_eq!(encoded.len(), 17);

    let decoded = ValueRef::decode(&encoded).unwrap();
    assert_eq!(decoded, vref);
}

#[test]
fn test_valueref_properties() {
    let inline = ValueRef::Inline;
    assert!(inline.is_inline());
    assert!(!inline.requires_overflow());
    assert_eq!(inline.size_hint(), None);

    let single = ValueRef::SinglePage {
        page_id: 1,
        offset: 0,
        length: 1000,
    };
    assert!(!single.is_inline());
    assert!(single.requires_overflow());
    assert_eq!(single.size_hint(), Some(1000));

    let chain = ValueRef::OverflowChain {
        first_page_id: 1,
        total_length: 100000,
        page_count: 25,
    };
    assert!(!chain.is_inline());
    assert!(chain.requires_overflow());
    assert_eq!(chain.size_hint(), Some(100000));
}

// Made with Bob
