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

//! Comprehensive tests for the hash table implementation.

use nanostore::table::{
    BatchOps, Flushable, MemoryHashTable, MutableTable, PointLookup, SearchableTable, Table,
    TableEngineKind, TableReader, WriteBatch,
};
use nanostore::txn::TransactionId;
use nanostore::types::{ScanBounds, TableId, ValueBuf};
use nanostore::wal::LogSequenceNumber;

#[test]
fn test_hash_table_creation() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());

    assert_eq!(table.table_id(), TableId::from(1));
    assert_eq!(table.name(), "test_hash");
    assert_eq!(table.kind(), TableEngineKind::Hash);
}

#[test]
fn test_hash_table_capabilities() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
    let caps = table.capabilities();

    // Hash tables support point lookups but NOT ordered operations
    assert!(caps.point_lookup);
    assert!(!caps.ordered);
    assert!(!caps.prefix_scan);
    assert!(!caps.reverse_scan);
    assert!(!caps.range_delete);
    assert!(caps.mvcc_native);
    assert!(caps.memory_resident);
    assert!(!caps.disk_resident);
}

#[test]
fn test_hash_table_basic_put_get() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    // Put a value
    let bytes = writer.put(b"key1", b"value1").unwrap();
    assert!(bytes > 0);
    writer.flush().unwrap();
    writer.commit_versions(LogSequenceNumber::from(10)).unwrap();

    // Get the value - visible after commit
    let reader = table.reader(LogSequenceNumber::from(10)).unwrap();
    let value = reader.get(b"key1", LogSequenceNumber::from(10)).unwrap();
    assert_eq!(value, Some(ValueBuf(b"value1".to_vec())));
}

#[test]
fn test_hash_table_update() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());

    // Initial put
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    writer.put(b"key1", b"value1").unwrap();
    writer.flush().unwrap();
    writer.commit_versions(LogSequenceNumber::from(10)).unwrap();

    // Update
    let mut writer = table
        .writer(TransactionId::from(2), LogSequenceNumber::from(2))
        .unwrap();
    writer.put(b"key1", b"value2").unwrap();
    writer.flush().unwrap();
    writer.commit_versions(LogSequenceNumber::from(20)).unwrap();

    // Verify update
    let reader = table.reader(LogSequenceNumber::from(20)).unwrap();
    let value = reader.get(b"key1", LogSequenceNumber::from(20)).unwrap();
    assert_eq!(value, Some(ValueBuf(b"value2".to_vec())));
}

#[test]
fn test_hash_table_delete() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());

    // Put a value
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    writer.put(b"key1", b"value1").unwrap();
    writer.flush().unwrap();
    writer.commit_versions(LogSequenceNumber::from(10)).unwrap();

    // Delete it
    let mut writer = table
        .writer(TransactionId::from(2), LogSequenceNumber::from(2))
        .unwrap();
    assert!(writer.delete(b"key1").unwrap());
    writer.flush().unwrap();
    writer.commit_versions(LogSequenceNumber::from(20)).unwrap();

    // Verify deletion
    let reader = table.reader(LogSequenceNumber::from(20)).unwrap();
    let value = reader.get(b"key1", LogSequenceNumber::from(20)).unwrap();
    assert_eq!(value, None);
}

#[test]
fn test_hash_table_delete_nonexistent() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    // Delete non-existent key should return false
    assert!(!writer.delete(b"nonexistent").unwrap());
}

#[test]
fn test_hash_table_batch_get() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    // Put multiple values
    writer.put(b"key1", b"value1").unwrap();
    writer.put(b"key2", b"value2").unwrap();
    writer.put(b"key3", b"value3").unwrap();
    writer.flush().unwrap();
    writer.commit_versions(LogSequenceNumber::from(10)).unwrap();

    // Batch get - use writer's batch_get with committed LSN
    let writer2 = table
        .writer(TransactionId::from(2), LogSequenceNumber::from(10))
        .unwrap();
    let keys = vec![
        b"key1".as_ref(),
        b"key2".as_ref(),
        b"key3".as_ref(),
        b"key4".as_ref(),
    ];
    let values = writer2.batch_get(&keys).unwrap();

    assert_eq!(values.len(), 4);
    assert_eq!(values[0], Some(ValueBuf(b"value1".to_vec())));
    assert_eq!(values[1], Some(ValueBuf(b"value2".to_vec())));
    assert_eq!(values[2], Some(ValueBuf(b"value3".to_vec())));
    assert_eq!(values[3], None); // key4 doesn't exist
}

#[test]
fn test_hash_table_batch_operations() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    // Create a batch
    use nanostore::table::Mutation;
    use std::borrow::Cow;

    let batch = WriteBatch {
        mutations: vec![
            Mutation::Put {
                key: Cow::Borrowed(b"key1"),
                value: Cow::Borrowed(b"value1"),
            },
            Mutation::Put {
                key: Cow::Borrowed(b"key2"),
                value: Cow::Borrowed(b"value2"),
            },
            Mutation::Put {
                key: Cow::Borrowed(b"key3"),
                value: Cow::Borrowed(b"value3"),
            },
        ],
    };

    let report = writer.apply_batch(batch).unwrap();
    assert_eq!(report.attempted, 3);
    assert_eq!(report.applied, 3);

    writer.flush().unwrap();
    writer.commit_versions(LogSequenceNumber::from(10)).unwrap();

    // Verify all values - visible after commit
    let reader = table.reader(LogSequenceNumber::from(10)).unwrap();
    assert_eq!(
        reader.get(b"key1", LogSequenceNumber::from(10)).unwrap(),
        Some(ValueBuf(b"value1".to_vec()))
    );
    assert_eq!(
        reader.get(b"key2", LogSequenceNumber::from(10)).unwrap(),
        Some(ValueBuf(b"value2".to_vec()))
    );
    assert_eq!(
        reader.get(b"key3", LogSequenceNumber::from(10)).unwrap(),
        Some(ValueBuf(b"value3".to_vec()))
    );
}

#[test]
fn test_hash_table_no_range_delete() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    // Range delete should fail for hash tables
    let result = writer.range_delete(ScanBounds::All);
    assert!(result.is_err());
}

#[test]
fn test_hash_table_collision_handling() {
    // Rust's HashMap handles collisions internally with chaining
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    // Insert many keys to potentially trigger collisions
    for i in 0..1000 {
        let key = format!("key{}", i);
        let value = format!("value{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }
    writer.flush().unwrap();
    writer.commit_versions(LogSequenceNumber::from(10)).unwrap();

    // Verify all keys are retrievable - visible after commit
    let reader = table.reader(LogSequenceNumber::from(10)).unwrap();
    for i in 0..1000 {
        let key = format!("key{}", i);
        let expected_value = format!("value{}", i);
        let value = reader
            .get(key.as_bytes(), LogSequenceNumber::from(10))
            .unwrap();
        assert_eq!(value, Some(ValueBuf(expected_value.as_bytes().to_vec())));
    }
}

#[test]
fn test_hash_table_hash_distribution() {
    // Test that keys are distributed across the hash table
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    // Insert keys with different patterns
    let patterns = vec![
        "sequential_1",
        "sequential_2",
        "sequential_3",
        "random_abc",
        "random_xyz",
        "random_123",
        "prefix_a",
        "prefix_b",
        "prefix_c",
    ];

    for pattern in &patterns {
        writer.put(pattern.as_bytes(), pattern.as_bytes()).unwrap();
    }
    writer.flush().unwrap();
    writer.commit_versions(LogSequenceNumber::from(10)).unwrap();

    // Verify all keys are retrievable - visible after commit
    let reader = table.reader(LogSequenceNumber::from(10)).unwrap();
    for pattern in &patterns {
        let value = reader
            .get(pattern.as_bytes(), LogSequenceNumber::from(10))
            .unwrap();
        assert_eq!(value, Some(ValueBuf(pattern.as_bytes().to_vec())));
    }
}

#[test]
fn test_hash_table_large_values() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    // Create a large value (1MB)
    let large_value = vec![0xAB; 1024 * 1024];
    writer.put(b"large_key", &large_value).unwrap();
    writer.flush().unwrap();
    writer.commit_versions(LogSequenceNumber::from(10)).unwrap();

    // Verify retrieval - visible after commit
    let reader = table.reader(LogSequenceNumber::from(10)).unwrap();
    let value = reader
        .get(b"large_key", LogSequenceNumber::from(10))
        .unwrap();
    assert_eq!(value, Some(ValueBuf(large_value)));
}

#[test]
fn test_hash_table_empty_key_value() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    // Empty key with small value (empty values are reserved for tombstones)
    writer.put(b"", b"empty_key_value").unwrap();
    writer.flush().unwrap();
    writer.commit_versions(LogSequenceNumber::from(10)).unwrap();

    let reader = table.reader(LogSequenceNumber::from(10)).unwrap();
    let value = reader.get(b"", LogSequenceNumber::from(10)).unwrap();
    assert_eq!(value, Some(ValueBuf(b"empty_key_value".to_vec())));
}

#[test]
fn test_hash_table_statistics() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    // Insert some data
    for i in 0..10 {
        let key = format!("key{}", i);
        let value = format!("value{}", i);
        writer.put(key.as_bytes(), value.as_bytes()).unwrap();
    }
    writer.flush().unwrap();

    // Check statistics
    let stats = table.stats().unwrap();
    assert_eq!(stats.row_count, Some(10));
    assert!(stats.total_size_bytes.is_some());
    assert!(stats.total_size_bytes.unwrap() > 0);
}

#[test]
fn test_hash_table_memory_tracking() {
    let table = MemoryHashTable::with_budget(
        TableId::from(1),
        "test_hash".to_string(),
        1024 * 1024, // 1MB budget
    );

    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    // Insert data
    for i in 0..100 {
        let key = format!("key{}", i);
        let value = vec![0u8; 1024]; // 1KB values
        writer.put(key.as_bytes(), &value).unwrap();
    }
    writer.flush().unwrap();

    // Memory usage should be tracked
    let stats = table.stats().unwrap();
    assert!(stats.total_size_bytes.is_some());
    assert!(stats.total_size_bytes.unwrap() > 0);
}

#[test]
fn test_hash_table_concurrent_readers() {
    use std::sync::Arc;
    use std::thread;

    let table = Arc::new(MemoryHashTable::new(
        TableId::from(1),
        "test_hash".to_string(),
    ));

    // Write some data
    {
        let mut writer = table
            .writer(TransactionId::from(1), LogSequenceNumber::from(1))
            .unwrap();
        for i in 0..100 {
            let key = format!("key{}", i);
            let value = format!("value{}", i);
            writer.put(key.as_bytes(), value.as_bytes()).unwrap();
        }
        writer.flush().unwrap();
        writer.commit_versions(LogSequenceNumber::from(10)).unwrap();
    }

    // Spawn multiple reader threads
    let mut handles = vec![];
    for _ in 0..10 {
        let table_clone = Arc::clone(&table);
        let handle = thread::spawn(move || {
            let reader = table_clone.reader(LogSequenceNumber::from(10)).unwrap();
            for i in 0..100 {
                let key = format!("key{}", i);
                let expected_value = format!("value{}", i);
                let value = reader
                    .get(key.as_bytes(), LogSequenceNumber::from(10))
                    .unwrap();
                assert_eq!(value, Some(ValueBuf(expected_value.as_bytes().to_vec())));
            }
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_hash_table_mvcc_visibility() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());

    // Transaction 1: Insert initial value
    let mut writer1 = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    writer1.put(b"key1", b"value1").unwrap();
    writer1.flush().unwrap();
    writer1
        .commit_versions(LogSequenceNumber::from(10))
        .unwrap();

    // Transaction 2: Update value
    let mut writer2 = table
        .writer(TransactionId::from(2), LogSequenceNumber::from(2))
        .unwrap();
    writer2.put(b"key1", b"value2").unwrap();
    writer2.flush().unwrap();
    writer2
        .commit_versions(LogSequenceNumber::from(20))
        .unwrap();

    // Reader at LSN 10 should see value1
    let reader1 = table.reader(LogSequenceNumber::from(10)).unwrap();
    let value1 = reader1.get(b"key1", LogSequenceNumber::from(10)).unwrap();
    assert_eq!(value1, Some(ValueBuf(b"value1".to_vec())));

    // Reader at LSN 20 should see value2
    let reader2 = table.reader(LogSequenceNumber::from(20)).unwrap();
    let value2 = reader2.get(b"key1", LogSequenceNumber::from(20)).unwrap();
    assert_eq!(value2, Some(ValueBuf(b"value2".to_vec())));
}

#[test]
fn test_hash_table_approximate_len() {
    let table = MemoryHashTable::new(TableId::from(1), "test_hash".to_string());
    let mut writer = table
        .writer(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();

    // Insert 50 items
    for i in 0..50 {
        let key = format!("key{}", i);
        writer.put(key.as_bytes(), b"value").unwrap();
    }
    writer.flush().unwrap();
    writer.commit_versions(LogSequenceNumber::from(10)).unwrap();

    let reader = table.reader(LogSequenceNumber::from(10)).unwrap();
    let len = reader.approximate_len().unwrap();
    assert_eq!(len, Some(50));
}

// Made with Bob
