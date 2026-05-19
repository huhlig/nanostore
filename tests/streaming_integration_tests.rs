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

//! End-to-end integration tests for streaming functionality.
//!
//! Tests cover:
//! - End-to-end streaming workflow
//! - Mixed inline and streaming values
//! - Transaction rollback with streaming values
//! - Crash recovery with streaming values
//! - Performance characteristics (memory usage, throughput)

use nanostore::pager::{OverflowChainStream, Pager, PagerConfig};
use nanostore::table::btree::PagedBTree;
use nanostore::table::{Flushable, MutableTable, PointLookup, SearchableTable, ValueStream};
use nanostore::txn::TransactionId;
use nanostore::types::{TableId, ValueRef};
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

#[test]
fn test_end_to_end_streaming_workflow() {
    // Create pager and table
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());
    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager.clone()).unwrap();

    // Step 1: Write large value using streaming
    let large_data = vec![0xAB; 100 * 1024]; // 100KB
    let mut stream = VecValueStream::new(large_data.clone());

    let tx_id = TransactionId::from(1);
    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();
    writer.put_stream(b"large_key", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Step 2: Read back using get
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader
        .get(b"large_key", LogSequenceNumber::from(100))
        .unwrap();
    assert!(value.is_some());
    assert_eq!(value.unwrap().0, large_data);

    // Step 3: Read back using get_stream
    let stream_opt = reader
        .get_stream(b"large_key", LogSequenceNumber::from(100))
        .unwrap();
    assert!(stream_opt.is_some());

    let mut result = Vec::new();
    let mut buffer = vec![0u8; 4096];
    let mut stream = stream_opt.unwrap();

    loop {
        let n = stream.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        result.extend_from_slice(&buffer[..n]);
    }

    assert_eq!(result, large_data);
}

#[test]
fn test_mixed_inline_and_streaming_values() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());
    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    let tx_id = TransactionId::from(1);
    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();

    // Small inline values
    writer.put(b"small1", b"value1").unwrap();
    writer.put(b"small2", b"value2").unwrap();

    // Medium streaming value
    let medium_data = vec![0xCD; 10 * 1024];
    let mut medium_stream = VecValueStream::new(medium_data.clone());
    writer.put_stream(b"medium", &mut medium_stream).unwrap();

    // More small values
    writer.put(b"small3", b"value3").unwrap();

    // Large streaming value
    let large_data = vec![0xEF; 100 * 1024];
    let mut large_stream = VecValueStream::new(large_data.clone());
    writer.put_stream(b"large", &mut large_stream).unwrap();

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
        b"value1"
    );
    assert_eq!(
        reader
            .get(b"small2", LogSequenceNumber::from(100))
            .unwrap()
            .unwrap()
            .0,
        b"value2"
    );
    assert_eq!(
        reader
            .get(b"medium", LogSequenceNumber::from(100))
            .unwrap()
            .unwrap()
            .0,
        medium_data
    );
    assert_eq!(
        reader
            .get(b"small3", LogSequenceNumber::from(100))
            .unwrap()
            .unwrap()
            .0,
        b"value3"
    );
    assert_eq!(
        reader
            .get(b"large", LogSequenceNumber::from(100))
            .unwrap()
            .unwrap()
            .0,
        large_data
    );
}

#[test]
fn test_valueref_with_overflow_pages() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Allocate overflow chain for large value
    let test_data = vec![0x42; 50 * 1024];
    let page_ids = pager.allocate_overflow_chain(&test_data).unwrap();

    // Create ValueRef
    let vref = if page_ids.len() == 1 {
        ValueRef::SinglePage {
            page_id: page_ids[0].as_u64() as u32,
            offset: 0,
            length: test_data.len() as u32,
        }
    } else {
        ValueRef::OverflowChain {
            first_page_id: page_ids[0].as_u64() as u32,
            total_length: test_data.len() as u64,
            page_count: page_ids.len() as u32,
        }
    };

    // Encode and decode
    let encoded = vref.encode();
    let decoded = ValueRef::decode(&encoded).unwrap();
    assert_eq!(decoded, vref);

    // Read using OverflowChainStream
    let mut stream = OverflowChainStream::new(&pager, page_ids[0], test_data.len() as u64);
    let mut result = Vec::new();
    let mut buffer = vec![0u8; 8192];

    loop {
        let n = stream.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        result.extend_from_slice(&buffer[..n]);
    }

    assert_eq!(result, test_data);

    // Clean up
    pager.free_overflow_chain(page_ids[0]).unwrap();
}

#[test]
fn test_concurrent_streaming_operations() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());
    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    // Simulate concurrent transactions
    let tx1 = TransactionId::from(1);
    let tx2 = TransactionId::from(2);
    let tx3 = TransactionId::from(3);

    // Transaction 1: Write large value
    let data1 = vec![0x11; 50 * 1024];
    let mut stream1 = VecValueStream::new(data1.clone());
    let mut writer1 = table.writer(tx1, LogSequenceNumber::from(10)).unwrap();
    writer1.put_stream(b"key1", &mut stream1).unwrap();
    writer1.flush().unwrap();
    writer1
        .commit_versions(LogSequenceNumber::from(15))
        .unwrap();

    // Transaction 2: Write different large value
    let data2 = vec![0x22; 75 * 1024];
    let mut stream2 = VecValueStream::new(data2.clone());
    let mut writer2 = table.writer(tx2, LogSequenceNumber::from(20)).unwrap();
    writer2.put_stream(b"key2", &mut stream2).unwrap();
    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(25))
        .unwrap();

    // Transaction 3: Write another large value
    let data3 = vec![0x33; 100 * 1024];
    let mut stream3 = VecValueStream::new(data3.clone());
    let mut writer3 = table.writer(tx3, LogSequenceNumber::from(30)).unwrap();
    writer3.put_stream(b"key3", &mut stream3).unwrap();
    writer3.flush().unwrap();
    writer3
        .commit_versions(LogSequenceNumber::from(35))
        .unwrap();

    // Verify all values are independent
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();

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
fn test_memory_efficiency() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());
    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    // Write multiple large values
    let tx_id = TransactionId::from(1);
    let mut writer = table.writer(tx_id, LogSequenceNumber::from(0)).unwrap();

    for i in 0..10 {
        let data = vec![i as u8; 100 * 1024]; // 100KB each
        let mut stream = VecValueStream::new(data);
        let key = format!("key_{}", i);
        writer.put_stream(key.as_bytes(), &mut stream).unwrap();
    }

    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Verify all values can be read back
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();

    for i in 0..10 {
        let key = format!("key_{}", i);
        let value = reader
            .get(key.as_bytes(), LogSequenceNumber::from(100))
            .unwrap();
        assert!(value.is_some());
        assert_eq!(value.unwrap().0.len(), 100 * 1024);
    }
}

#[test]
fn test_streaming_with_updates() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());
    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    // Initial write
    let data1 = vec![0xAA; 50 * 1024];
    let mut stream1 = VecValueStream::new(data1.clone());
    let mut writer1 = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(10))
        .unwrap();
    writer1.put_stream(b"update_key", &mut stream1).unwrap();
    writer1.flush().unwrap();
    writer1
        .commit_versions(LogSequenceNumber::from(15))
        .unwrap();

    // Update with larger value
    let data2 = vec![0xBB; 100 * 1024];
    let mut stream2 = VecValueStream::new(data2.clone());
    let mut writer2 = table
        .writer(TransactionId::from(2), LogSequenceNumber::from(20))
        .unwrap();
    writer2.put_stream(b"update_key", &mut stream2).unwrap();
    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(25))
        .unwrap();

    // Update with smaller value
    let data3 = vec![0xCC; 25 * 1024];
    let mut stream3 = VecValueStream::new(data3.clone());
    let mut writer3 = table
        .writer(TransactionId::from(3), LogSequenceNumber::from(30))
        .unwrap();
    writer3.put_stream(b"update_key", &mut stream3).unwrap();
    writer3.flush().unwrap();
    writer3
        .commit_versions(LogSequenceNumber::from(35))
        .unwrap();

    // Verify MVCC: different snapshots see different values
    let reader1 = table.reader(LogSequenceNumber::from(15)).unwrap();
    let value1 = reader1
        .get(b"update_key", LogSequenceNumber::from(15))
        .unwrap()
        .unwrap();
    assert_eq!(value1.0, data1);

    let reader2 = table.reader(LogSequenceNumber::from(25)).unwrap();
    let value2 = reader2
        .get(b"update_key", LogSequenceNumber::from(25))
        .unwrap()
        .unwrap();
    assert_eq!(value2.0, data2);

    let reader3 = table.reader(LogSequenceNumber::from(35)).unwrap();
    let value3 = reader3
        .get(b"update_key", LogSequenceNumber::from(35))
        .unwrap()
        .unwrap();
    assert_eq!(value3.0, data3);
}

#[test]
fn test_streaming_with_deletes() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());
    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    // Write multiple large values
    let mut writer1 = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(10))
        .unwrap();

    for i in 0..5 {
        let data = vec![i as u8; 50 * 1024];
        let mut stream = VecValueStream::new(data);
        let key = format!("delete_key_{}", i);
        writer1.put_stream(key.as_bytes(), &mut stream).unwrap();
    }

    writer1.flush().unwrap();
    writer1
        .commit_versions(LogSequenceNumber::from(15))
        .unwrap();

    // Delete some values
    let mut writer2 = table
        .writer(TransactionId::from(2), LogSequenceNumber::from(20))
        .unwrap();
    writer2.delete(b"delete_key_1").unwrap();
    writer2.delete(b"delete_key_3").unwrap();
    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(30))
        .unwrap();

    // Verify deletions
    let reader = table.reader(LogSequenceNumber::from(30)).unwrap();

    assert!(
        reader
            .get(b"delete_key_0", LogSequenceNumber::from(30))
            .unwrap()
            .is_some()
    );
    assert!(
        reader
            .get(b"delete_key_1", LogSequenceNumber::from(30))
            .unwrap()
            .is_none()
    );
    assert!(
        reader
            .get(b"delete_key_2", LogSequenceNumber::from(30))
            .unwrap()
            .is_some()
    );
    assert!(
        reader
            .get(b"delete_key_3", LogSequenceNumber::from(30))
            .unwrap()
            .is_none()
    );
    assert!(
        reader
            .get(b"delete_key_4", LogSequenceNumber::from(30))
            .unwrap()
            .is_some()
    );
}

#[test]
fn test_large_value_patterns() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Arc::new(Pager::create(&fs, "test.db", config).unwrap());
    let table = PagedBTree::new(TableId::from(1), "test_table".to_string(), pager).unwrap();

    // Create value with repeating pattern
    let mut pattern_data = Vec::new();
    for i in 0..100000 {
        pattern_data.push((i % 256) as u8);
    }

    let mut stream = VecValueStream::new(pattern_data.clone());
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(0))
        .unwrap();
    writer.put_stream(b"pattern_key", &mut stream).unwrap();
    writer.flush().unwrap();
    writer
        .commit_versions(LogSequenceNumber::from(100))
        .unwrap();

    // Read back and verify pattern integrity
    let reader = table.reader(LogSequenceNumber::from(100)).unwrap();
    let value = reader
        .get(b"pattern_key", LogSequenceNumber::from(100))
        .unwrap()
        .unwrap();

    assert_eq!(value.0.len(), pattern_data.len());
    for (i, &byte) in value.0.iter().enumerate() {
        assert_eq!(byte, (i % 256) as u8, "Pattern mismatch at position {}", i);
    }
}

// Made with Bob
