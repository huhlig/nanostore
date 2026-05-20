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

//! Tests for StorageEngine and table metadata storage

use nanostore::pager::{CompressionType, EncryptionType, FileHeader, PageSize};
use nanostore::table::TableInfo;
use nanostore::types::TableId;
use nanostore::wal::LogSequenceNumber;
use std::collections::HashMap;

#[test]
fn test_file_header_metadata_operations() {
    let mut header = FileHeader::new(
        PageSize::Size4KB,
        CompressionType::None,
        EncryptionType::None,
    );

    // Test setting metadata
    header.set_metadata("version".to_string(), b"1.0.0".to_vec());
    header.set_metadata("author".to_string(), b"test_user".to_vec());
    header.set_metadata("description".to_string(), b"Test StorageEngine".to_vec());

    // Test getting metadata
    assert_eq!(header.get_metadata("version"), Some(&b"1.0.0".to_vec()));
    assert_eq!(header.get_metadata("author"), Some(&b"test_user".to_vec()));
    assert_eq!(
        header.get_metadata("description"),
        Some(&b"Test StorageEngine".to_vec())
    );
    assert_eq!(header.get_metadata("nonexistent"), None);

    // Test metadata count
    assert_eq!(header.metadata().len(), 3);

    // Test removing metadata
    let removed = header.remove_metadata("author");
    assert_eq!(removed, Some(b"test_user".to_vec()));
    assert_eq!(header.get_metadata("author"), None);
    assert_eq!(header.metadata().len(), 2);

    // Test clearing metadata
    header.clear_metadata();
    assert_eq!(header.metadata().len(), 0);
    assert_eq!(header.get_metadata("version"), None);
}

#[test]
fn test_file_header_metadata_persistence() {
    let mut header = FileHeader::new(
        PageSize::Size4KB,
        CompressionType::None,
        EncryptionType::None,
    );

    // Add metadata
    header.set_metadata("key1".to_string(), b"value1".to_vec());
    header.set_metadata("key2".to_string(), b"value2".to_vec());

    // Note: The fixed header doesn't serialize metadata (it's stored separately)
    // This test verifies that metadata is maintained in memory
    assert_eq!(header.metadata().len(), 2);
    assert_eq!(header.get_metadata("key1"), Some(&b"value1".to_vec()));
    assert_eq!(header.get_metadata("key2"), Some(&b"value2".to_vec()));
}

#[test]
fn test_table_info_metadata_operations() {
    let mut table_info = TableInfo {
        id: TableId::from(1),
        name: "test_table".to_string(),
        options: Default::default(),
        root: None,
        created_lsn: LogSequenceNumber::from(0),
        metadata: HashMap::new(),
    };

    // Test setting metadata
    table_info.set_metadata("schema_version".to_string(), b"2".to_vec());
    table_info.set_metadata("owner".to_string(), b"admin".to_vec());
    table_info.set_metadata("tags".to_string(), b"production,critical".to_vec());

    // Test getting metadata
    assert_eq!(
        table_info.get_metadata("schema_version"),
        Some(&b"2".to_vec())
    );
    assert_eq!(table_info.get_metadata("owner"), Some(&b"admin".to_vec()));
    assert_eq!(
        table_info.get_metadata("tags"),
        Some(&b"production,critical".to_vec())
    );
    assert_eq!(table_info.get_metadata("nonexistent"), None);

    // Test metadata count
    assert_eq!(table_info.metadata().len(), 3);

    // Test removing metadata
    let removed = table_info.remove_metadata("owner");
    assert_eq!(removed, Some(b"admin".to_vec()));
    assert_eq!(table_info.get_metadata("owner"), None);
    assert_eq!(table_info.metadata().len(), 2);

    // Test clearing metadata
    table_info.clear_metadata();
    assert_eq!(table_info.metadata().len(), 0);
    assert_eq!(table_info.get_metadata("schema_version"), None);
}

#[test]
fn test_table_info_metadata_serialization() {
    let mut table_info = TableInfo {
        id: TableId::from(42),
        name: "users".to_string(),
        options: Default::default(),
        root: None,
        created_lsn: LogSequenceNumber::from(100),
        metadata: HashMap::new(),
    };

    // Add metadata
    table_info.set_metadata("created_by".to_string(), b"system".to_vec());
    table_info.set_metadata("purpose".to_string(), b"user_management".to_vec());

    // Serialize to JSON
    let json = serde_json::to_string(&table_info).expect("Failed to serialize");

    // Deserialize from JSON
    let deserialized: TableInfo = serde_json::from_str(&json).expect("Failed to deserialize");

    // Verify metadata is preserved
    assert_eq!(deserialized.id, table_info.id);
    assert_eq!(deserialized.name, table_info.name);
    assert_eq!(deserialized.metadata.len(), 2);
    assert_eq!(
        deserialized.get_metadata("created_by"),
        Some(&b"system".to_vec())
    );
    assert_eq!(
        deserialized.get_metadata("purpose"),
        Some(&b"user_management".to_vec())
    );
}

#[test]
fn test_metadata_empty_values() {
    let mut header = FileHeader::new(
        PageSize::Size4KB,
        CompressionType::None,
        EncryptionType::None,
    );

    // Test empty byte array values
    header.set_metadata("empty_key".to_string(), vec![]);
    assert_eq!(header.get_metadata("empty_key"), Some(&vec![]));

    // Test overwriting values
    header.set_metadata("key".to_string(), b"value1".to_vec());
    assert_eq!(header.get_metadata("key"), Some(&b"value1".to_vec()));
    header.set_metadata("key".to_string(), b"value2".to_vec());
    assert_eq!(header.get_metadata("key"), Some(&b"value2".to_vec()));
}

#[test]
fn test_metadata_special_characters() {
    let mut table_info = TableInfo {
        id: TableId::from(1),
        name: "test".to_string(),
        options: Default::default(),
        root: None,
        created_lsn: LogSequenceNumber::from(0),
        metadata: HashMap::new(),
    };

    // Test special characters in keys and values
    table_info.set_metadata("key-with-dashes".to_string(), b"value".to_vec());
    table_info.set_metadata("key.with.dots".to_string(), b"value".to_vec());
    table_info.set_metadata("key_with_underscores".to_string(), b"value".to_vec());
    table_info.set_metadata(
        "unicode_key".to_string(),
        "值 with 中文 characters".as_bytes().to_vec(),
    );

    assert_eq!(
        table_info.get_metadata("key-with-dashes"),
        Some(&b"value".to_vec())
    );
    assert_eq!(
        table_info.get_metadata("key.with.dots"),
        Some(&b"value".to_vec())
    );
    assert_eq!(
        table_info.get_metadata("key_with_underscores"),
        Some(&b"value".to_vec())
    );
    assert_eq!(
        table_info.get_metadata("unicode_key"),
        Some(&"值 with 中文 characters".as_bytes().to_vec())
    );
}

#[test]
fn test_metadata_large_values() {
    let mut header = FileHeader::new(
        PageSize::Size4KB,
        CompressionType::None,
        EncryptionType::None,
    );

    // Test large metadata values
    let large_value = vec![b'x'; 10000];
    header.set_metadata("large_key".to_string(), large_value.clone());
    assert_eq!(header.get_metadata("large_key"), Some(&large_value));
}

#[test]
fn test_metadata_binary_values() {
    let mut table_info = TableInfo {
        id: TableId::from(1),
        name: "test".to_string(),
        options: Default::default(),
        root: None,
        created_lsn: LogSequenceNumber::from(0),
        metadata: HashMap::new(),
    };

    // Test binary data (not valid UTF-8)
    let binary_data = vec![0xFF, 0xFE, 0xFD, 0xFC, 0x00, 0x01, 0x02];
    table_info.set_metadata("binary_key".to_string(), binary_data.clone());
    assert_eq!(table_info.get_metadata("binary_key"), Some(&binary_data));

    // Test storing serialized data (e.g., a u64)
    let number: u64 = 42;
    table_info.set_metadata("number".to_string(), number.to_le_bytes().to_vec());
    let retrieved = table_info.get_metadata("number").unwrap();
    let decoded = u64::from_le_bytes(retrieved.as_slice().try_into().unwrap());
    assert_eq!(decoded, 42);
}

#[test]
fn test_metadata_iteration() {
    let mut table_info = TableInfo {
        id: TableId::from(1),
        name: "test".to_string(),
        options: Default::default(),
        root: None,
        created_lsn: LogSequenceNumber::from(0),
        metadata: HashMap::new(),
    };

    // Add multiple metadata entries
    table_info.set_metadata("key1".to_string(), b"value1".to_vec());
    table_info.set_metadata("key2".to_string(), b"value2".to_vec());
    table_info.set_metadata("key3".to_string(), b"value3".to_vec());

    // Iterate over metadata
    let metadata = table_info.metadata();
    assert_eq!(metadata.len(), 3);

    let mut keys: Vec<_> = metadata.keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, vec!["key1", "key2", "key3"]);
}

// Made with Bob
