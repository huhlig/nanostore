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

//! Integration tests for Blob Tables in the unified table architecture.
//!
//! These tests verify blob storage using the BlobTable trait with the
//! MemoryBlob implementation.

use nanostore::table::blob::MemoryBlob;
use nanostore::table::{BlobTable, Table};
use nanostore::types::{ObjectId, ValueBuf};

/// Test blob table creation
#[test]
fn test_blob_table_creation() {
    let table = MemoryBlob::new(ObjectId::from(1), "test_blobs".to_string());
    
    assert_eq!(Table::table_id(&table), ObjectId::from(1));
    assert_eq!(Table::name(&table), "test_blobs");
    
    // Verify inline threshold
    assert!(table.max_inline_size() > 0);
    
    // Verify max blob size
    assert!(table.max_blob_size() > 0);
}

/// Test storing and retrieving a blob
#[test]
fn test_put_and_get_blob() {
    let mut table = MemoryBlob::new(ObjectId::from(1), "test_blobs".to_string());
    
    let key = b"test_key";
    let data = b"Hello, blob storage!";
    
    // Store the blob
    let size = table.put_blob(key, data).unwrap();
    assert_eq!(size, data.len() as u64);
    
    // Retrieve the blob
    let retrieved = table.get_blob(key).unwrap();
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().as_ref(), data);
}

/// Test blob existence check
#[test]
fn test_contains_blob() {
    let mut table = MemoryBlob::new(ObjectId::from(1), "test_blobs".to_string());
    
    let key = b"test_key";
    let data = b"test data";
    
    // Initially doesn't exist
    assert!(!table.contains_blob(key).unwrap());
    
    // Store the blob
    table.put_blob(key, data).unwrap();
    
    // Now it exists
    assert!(table.contains_blob(key).unwrap());
}

/// Test blob size query
#[test]
fn test_blob_size() {
    let mut table = MemoryBlob::new(ObjectId::from(1), "test_blobs".to_string());
    
    let key = b"test_key";
    let data = b"test data with some length";
    
    // Initially no size
    assert!(table.blob_size(key).unwrap().is_none());
    
    // Store the blob
    table.put_blob(key, data).unwrap();
    
    // Check size
    let size = table.blob_size(key).unwrap();
    assert_eq!(size, Some(data.len() as u64));
}

/// Test blob deletion
#[test]
fn test_delete_blob() {
    let mut table = MemoryBlob::new(ObjectId::from(1), "test_blobs".to_string());
    
    let key = b"test_key";
    let data = b"test data";
    
    // Store the blob
    table.put_blob(key, data).unwrap();
    assert!(table.contains_blob(key).unwrap());
    
    // Delete the blob
    let deleted = table.delete_blob(key).unwrap();
    assert!(deleted);
    
    // Verify it's gone
    assert!(!table.contains_blob(key).unwrap());
    assert!(table.get_blob(key).unwrap().is_none());
    
    // Deleting again returns false
    let deleted_again = table.delete_blob(key).unwrap();
    assert!(!deleted_again);
}

/// Test blob replacement
#[test]
fn test_blob_replacement() {
    let mut table = MemoryBlob::new(ObjectId::from(1), "test_blobs".to_string());
    
    let key = b"test_key";
    let data1 = b"original data";
    let data2 = b"replacement data";
    
    // Store original
    table.put_blob(key, data1).unwrap();
    assert_eq!(table.get_blob(key).unwrap().unwrap().as_ref(), data1);
    
    // Replace with new data
    table.put_blob(key, data2).unwrap();
    assert_eq!(table.get_blob(key).unwrap().unwrap().as_ref(), data2);
}

/// Test multiple blobs
#[test]
fn test_multiple_blobs() {
    let mut table = MemoryBlob::new(ObjectId::from(1), "test_blobs".to_string());
    
    // Store multiple blobs
    for i in 0..10 {
        let key = format!("key_{}", i).into_bytes();
        let data = format!("data_{}", i).into_bytes();
        table.put_blob(&key, &data).unwrap();
    }
    
    // Verify all exist
    for i in 0..10 {
        let key = format!("key_{}", i).into_bytes();
        assert!(table.contains_blob(&key).unwrap());
    }
    
    // Verify correct data
    for i in 0..10 {
        let key = format!("key_{}", i).into_bytes();
        let expected = format!("data_{}", i).into_bytes();
        let retrieved = table.get_blob(&key).unwrap().unwrap();
        assert_eq!(retrieved.as_ref(), expected.as_slice());
    }
}

/// Test empty blob
#[test]
fn test_empty_blob() {
    let mut table = MemoryBlob::new(ObjectId::from(1), "test_blobs".to_string());
    
    let key = b"empty_key";
    let data = b"";
    
    // Store empty blob
    let size = table.put_blob(key, data).unwrap();
    assert_eq!(size, 0);
    
    // Retrieve empty blob
    let retrieved = table.get_blob(key).unwrap();
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().as_ref().len(), 0);
}

/// Test large blob
#[test]
fn test_large_blob() {
    let mut table = MemoryBlob::new(ObjectId::from(1), "test_blobs".to_string());
    
    let key = b"large_key";
    // Create a 1MB blob
    let data = vec![0xAB; 1024 * 1024];
    
    // Store large blob
    let size = table.put_blob(key, &data).unwrap();
    assert_eq!(size, data.len() as u64);
    
    // Retrieve and verify
    let retrieved = table.get_blob(key).unwrap().unwrap();
    assert_eq!(retrieved.as_ref().len(), data.len());
    assert_eq!(retrieved.as_ref(), data.as_slice());
}

/// Test ValueRef type usage
#[test]
fn test_value_ref_type() {
    use nanostore::types::ValueRef;
    
    // ValueRef is used for externally stored values
    let value_ref = ValueRef::new(42, 1024, 0x12345678);
    
    assert_eq!(value_ref.first_page(), 42);
    assert_eq!(value_ref.size(), 1024);
    assert_eq!(value_ref.checksum(), 0x12345678);
}

/// Test blob table error handling
#[test]
fn test_blob_error_handling() {
    use nanostore::table::TableError;
    
    // Test creating value ref errors
    let value_ref = nanostore::types::ValueRef::new(1, 100, 0xABCD);
    
    let error = TableError::value_ref_not_found(value_ref);
    assert!(error.to_string().contains("not found"));
    
    let error = TableError::stale_value_ref(value_ref, 0xABCD, 0x1234);
    assert!(error.to_string().contains("checksum mismatch"));
    
    let error = TableError::value_too_large(value_ref, 1000000, 100000);
    assert!(error.to_string().contains("too large"));
}

// Made with Bob
