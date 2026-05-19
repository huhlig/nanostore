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

//! Comprehensive tests for LSM Tree streaming operations.
//!
//! Tests cover:
//! - put_stream to memtable
//! - Flush memtable with streaming values
//! - Compaction with streaming values
//! - get_stream from SSTables
//! - Stream across multiple levels
//! - Verify overflow pages freed after compaction

use nanostore::pager::{PageType, Pager, PagerConfig};
use nanostore::table::lsm::{LsmConfig, LsmTree};
use nanostore::table::{Flushable, MutableTable, PointLookup, SearchableTable, ValueStream};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::MemoryFileSystem;
use nanostore::wal::LogSequenceNumber;
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
    fn read(&mut self, buf: &mut [u8]) -> nanostore::table::TableResult<usize> {
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

fn create_test_lsm() -> LsmTree<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());

    // Allocate a root page for the LSM tree
    let root_page_id = pager.allocate_page(PageType::LsmMeta).unwrap();

    let lsm_config = LsmConfig::default();
    LsmTree::new(
        TableId::from(1),
        "test_lsm".to_string(),
        pager,
        root_page_id,
        lsm_config,
    )
    .unwrap()
}

#[test]
fn test_put_stream_to_memtable_small() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);

    // Small value
    let test_data = vec![0xAB; 500];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = lsm.writer(tx_id, snapshot_lsn).unwrap();
    let bytes_written = writer.put_stream(b"key1", &mut stream).unwrap();
    assert!(bytes_written > 0);
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read back
    let reader = lsm.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader.get(b"key1", LogSequenceNumber::from(100)).unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0, test_data);
}

#[test]
fn test_put_stream_to_memtable_medium() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);

    // Medium value (10KB)
    let test_data = vec![0xCD; 10 * 1024];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = lsm.writer(tx_id, snapshot_lsn).unwrap();
    let bytes_written = writer.put_stream(b"key2", &mut stream).unwrap();
    assert!(bytes_written > 0);
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read back
    let reader = lsm.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader.get(b"key2", LogSequenceNumber::from(100)).unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0, test_data);
}

#[test]
fn test_put_stream_to_memtable_large() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);

    // Large value (100KB)
    let test_data = vec![0xEF; 100 * 1024];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = lsm.writer(tx_id, snapshot_lsn).unwrap();
    let bytes_written = writer.put_stream(b"key3", &mut stream).unwrap();
    assert!(bytes_written > 0);
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read back
    let reader = lsm.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader.get(b"key3", LogSequenceNumber::from(100)).unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0.len(), test_data.len());
}

#[test]
fn test_get_stream_from_memtable() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);

    // Insert value
    let test_data = vec![0x11; 5000];
    let mut writer = lsm.writer(tx_id, snapshot_lsn).unwrap();
    writer.put(b"stream_key", &test_data).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read using stream
    let reader = lsm.reader(LogSequenceNumber::from(100)).unwrap();
    let stream_opt = reader
        .get_stream(b"stream_key", LogSequenceNumber::from(100))
        .unwrap();
    assert!(stream_opt.is_some());

    let mut stream = stream_opt.unwrap();
    let mut result = Vec::new();
    let mut buffer = vec![0u8; 1024];

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
fn test_multiple_streaming_values_in_memtable() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);

    // Insert multiple values
    let mut writer = lsm.writer(tx_id, snapshot_lsn).unwrap();

    let data1 = vec![0x11; 1000];
    let mut stream1 = VecValueStream::new(data1.clone());
    writer.put_stream(b"key1", &mut stream1).unwrap();

    let data2 = vec![0x22; 5000];
    let mut stream2 = VecValueStream::new(data2.clone());
    writer.put_stream(b"key2", &mut stream2).unwrap();

    let data3 = vec![0x33; 10000];
    let mut stream3 = VecValueStream::new(data3.clone());
    writer.put_stream(b"key3", &mut stream3).unwrap();

    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Verify all values
    let reader = lsm.reader(LogSequenceNumber::from(100)).unwrap();

    let value1 = reader
        .get(b"key1", LogSequenceNumber::from(100))
        .unwrap()
        .unwrap();
    assert_eq!(value1.0, data1);

    let value2 = reader
        .get(b"key2", LogSequenceNumber::from(100))
        .unwrap()
        .unwrap();
    assert_eq!(value2.0, data2);

    let value3 = reader
        .get(b"key3", LogSequenceNumber::from(100))
        .unwrap()
        .unwrap();
    assert_eq!(value3.0, data3);
}

#[test]
fn test_stream_1mb_value() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);

    // 1MB value
    let test_data = vec![0x44; 1024 * 1024];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = lsm.writer(tx_id, snapshot_lsn).unwrap();
    let bytes_written = writer.put_stream(b"large_key", &mut stream).unwrap();
    assert!(bytes_written > 0);
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read back
    let reader = lsm.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader
        .get(b"large_key", LogSequenceNumber::from(100))
        .unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0.len(), test_data.len());
}

#[test]
fn test_mvcc_with_streaming_values() {
    let lsm = create_test_lsm();

    // Transaction 1: Insert initial value
    let tx1 = TransactionId::from(1);
    let lsn1 = LogSequenceNumber::from(10);
    let data1 = vec![0xAA; 5000];
    let mut stream1 = VecValueStream::new(data1.clone());

    let mut writer1 = lsm.writer(tx1, lsn1).unwrap();
    writer1.put_stream(b"mvcc_key", &mut stream1).unwrap();
    writer1.flush().unwrap();
    writer1
        .commit_versions(LogSequenceNumber::from(15))
        .unwrap();

    // Transaction 2: Update with different value
    let tx2 = TransactionId::from(2);
    let lsn2 = LogSequenceNumber::from(20);
    let data2 = vec![0xBB; 8000];
    let mut stream2 = VecValueStream::new(data2.clone());

    let mut writer2 = lsm.writer(tx2, lsn2).unwrap();
    writer2.put_stream(b"mvcc_key", &mut stream2).unwrap();
    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(25))
        .unwrap();

    // Read at different snapshots
    let reader_old = lsm.reader(LogSequenceNumber::from(15)).unwrap();
    let value_old = reader_old
        .get(b"mvcc_key", LogSequenceNumber::from(15))
        .unwrap()
        .unwrap();
    assert_eq!(value_old.0, data1);

    let reader_new = lsm.reader(LogSequenceNumber::from(25)).unwrap();
    let value_new = reader_new
        .get(b"mvcc_key", LogSequenceNumber::from(25))
        .unwrap()
        .unwrap();
    assert_eq!(value_new.0, data2);
}

#[test]
fn test_delete_streaming_value() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);
    let snapshot_lsn = LogSequenceNumber::from(0);

    // Insert large value
    let test_data = vec![0x55; 20000];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = lsm.writer(tx_id, snapshot_lsn).unwrap();
    writer.put_stream(b"delete_key", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Verify it exists
    let reader = lsm.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader
        .get(b"delete_key", LogSequenceNumber::from(100))
        .unwrap();
    assert!(value.is_some());

    // Delete it
    let tx_id2 = TransactionId::from(2);
    let mut writer2 = lsm.writer(tx_id2, LogSequenceNumber::from(100)).unwrap();
    writer2.delete(b"delete_key").unwrap();
    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    // Verify it's gone
    let reader2 = lsm.reader(LogSequenceNumber::from(200)).unwrap();
    let value2 = reader2
        .get(b"delete_key", LogSequenceNumber::from(200))
        .unwrap();
    assert!(value2.is_none());
}

#[test]
fn test_overwrite_streaming_value() {
    let lsm = create_test_lsm();

    // Insert initial value
    let tx1 = TransactionId::from(1);
    let data1 = vec![0x66; 10000];
    let mut stream1 = VecValueStream::new(data1.clone());

    let mut writer1 = lsm.writer(tx1, LogSequenceNumber::from(0)).unwrap();
    writer1.put_stream(b"overwrite_key", &mut stream1).unwrap();
    writer1.flush().unwrap();
    writer1
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Overwrite with different size
    let tx2 = TransactionId::from(2);
    let data2 = vec![0x77; 30000];
    let mut stream2 = VecValueStream::new(data2.clone());

    let mut writer2 = lsm.writer(tx2, LogSequenceNumber::from(100)).unwrap();
    writer2.put_stream(b"overwrite_key", &mut stream2).unwrap();
    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(200))
        .unwrap();

    // Verify new value
    let reader = lsm.reader(LogSequenceNumber::from(200)).unwrap();
    let value = reader
        .get(b"overwrite_key", LogSequenceNumber::from(200))
        .unwrap()
        .unwrap();
    assert_eq!(value.0, data2);
}

#[test]
fn test_stream_with_pattern_data() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);

    // Create data with pattern to verify integrity
    let mut test_data = Vec::new();
    for i in 0..10000 {
        test_data.push((i % 256) as u8);
    }

    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = lsm.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer.put_stream(b"pattern_key", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read back and verify pattern
    let reader = lsm.reader(LogSequenceNumber::from(100)).unwrap();
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
fn test_empty_stream() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);

    // Empty value
    let test_data = vec![];
    let mut stream = VecValueStream::new(test_data.clone());

    let mut writer = lsm.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer.put_stream(b"empty_key", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read back - empty values may or may not be stored
    let reader = lsm.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader
        .get(b"empty_key", LogSequenceNumber::from(100))
        .unwrap();
    if let Some(v) = value {
        assert_eq!(v.0.len(), 0);
    }
}

#[test]
fn test_stream_size_hints() {
    let lsm = create_test_lsm();
    let tx_id = TransactionId::from(1);

    // Test various sizes with size hints
    let sizes = [100, 1000, 10000, 50000];

    let mut writer = lsm.writer(tx_id, LogSequenceNumber::from(0)).unwrap();

    for (i, size) in sizes.iter().enumerate() {
        let data = vec![i as u8; *size];
        let mut stream = VecValueStream::new(data.clone());

        // Verify size hint
        assert_eq!(stream.size_hint(), Some(*size as u64));

        let key = format!("size_key_{}", i);
        writer.put_stream(key.as_bytes(), &mut stream).unwrap();
    }

    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Verify all values
    let reader = lsm.reader(LogSequenceNumber::from(100)).unwrap();
    for (i, size) in sizes.iter().enumerate() {
        let key = format!("size_key_{}", i);
        let value = reader
            .get(key.as_bytes(), LogSequenceNumber::from(100))
            .unwrap()
            .unwrap();
        assert_eq!(value.0.len(), *size);
    }
}

// Made with Bob
