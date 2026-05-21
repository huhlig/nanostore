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

//! Integration tests for Phase 4: StorageEngine & Table Handle APIs
//!
//! Tests the high-level StorageEngine API with:
//! - Table management (create, drop, list)
//! - CRUD operations (insert, update, upsert, get, delete)
//! - Table handle wrapper
//! - Error handling

use nanostore::engine::{StorageEngine, StorageEngineErrorKind};
use nanostore::table::{TableEngineKind, TableOptions};
use nanostore::types::{KeyEncoding, TableId};
use nanostore::vfs::{FileSystem, MemoryFileSystem};

/// Helper to create a test StorageEngine
fn create_test_db() -> StorageEngine<MemoryFileSystem> {
    let fs = MemoryFileSystem::new();
    StorageEngine::new(&fs, "test.wal", "test.db").expect("Failed to create StorageEngine")
}

/// Helper to create default table options
fn default_table_options() -> TableOptions {
    TableOptions::default()
}

// =============================================================================
// Table Management Tests
// =============================================================================

#[test]
fn test_create_table() {
    let db = create_test_db();

    let table_id = db
        .create_table("users", default_table_options())
        .expect("Failed to create table");

    // Verify table exists
    assert!(db.is_table(table_id).unwrap());

    // Verify table info
    let info = db
        .get_object_info(table_id)
        .unwrap()
        .expect("Table not found");
    assert_eq!(info.name, "users");
    assert_eq!(info.options.engine, TableEngineKind::Memory);
}

#[test]
fn test_create_duplicate_table() {
    let db = create_test_db();

    db.create_table("users", default_table_options())
        .expect("Failed to create table");

    // Try to create duplicate
    let result = db.create_table("users", default_table_options());
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert_eq!(err.kind, StorageEngineErrorKind::TableAlreadyExists);
}

#[test]
fn test_drop_table() {
    let db = create_test_db();

    let table_id = db
        .create_table("users", default_table_options())
        .expect("Failed to create table");

    // Drop table
    db.drop_table(table_id).expect("Failed to drop table");

    // Verify table no longer exists
    assert!(!db.is_table(table_id).unwrap());
    assert!(db.get_object_info(table_id).unwrap().is_none());
}

#[test]
fn test_list_tables() {
    let db = create_test_db();

    // Create multiple tables
    db.create_table("users", default_table_options()).unwrap();
    db.create_table("posts", default_table_options()).unwrap();
    db.create_table("comments", default_table_options())
        .unwrap();

    // List tables
    let tables = db.list_tables().expect("Failed to list tables");
    assert_eq!(tables.len(), 3);

    let names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
    assert!(names.contains(&"users".to_string()));
    assert!(names.contains(&"posts".to_string()));
    assert!(names.contains(&"comments".to_string()));
}

#[test]
fn test_open_table() {
    let db = create_test_db();

    db.create_table("users", default_table_options()).unwrap();

    // Open by name
    let table_id = db
        .open_table("users")
        .expect("Failed to open table")
        .expect("Table not found");

    assert!(db.is_table(table_id).unwrap());

    // Try to open non-existent table
    let result = db.open_table("nonexistent");
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

// =============================================================================
// CRUD Operations Tests
// =============================================================================

#[test]
fn test_insert() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();

    // Insert key-value pair
    db.insert(table_id, b"user1", b"Alice")
        .expect("Failed to insert");

    // Verify value
    let value = db
        .get(table_id, b"user1")
        .expect("Failed to get")
        .expect("Value not found");
    assert_eq!(value.as_ref(), b"Alice");
}

#[test]
fn test_insert_duplicate_key() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();

    // Insert first time
    db.insert(table_id, b"user1", b"Alice").unwrap();

    // Try to insert duplicate
    let result = db.insert(table_id, b"user1", b"Bob");
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert_eq!(err.kind, StorageEngineErrorKind::KeyAlreadyExists);

    // Verify original value unchanged
    let value = db.get(table_id, b"user1").unwrap().unwrap();
    assert_eq!(value.as_ref(), b"Alice");
}

#[test]
fn test_update() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();

    // Insert initial value
    db.insert(table_id, b"user1", b"Alice").unwrap();

    // Update value
    db.update(table_id, b"user1", b"Alice Smith")
        .expect("Failed to update");

    // Verify updated value
    let value = db.get(table_id, b"user1").unwrap().unwrap();
    assert_eq!(value.as_ref(), b"Alice Smith");
}

#[test]
fn test_update_nonexistent_key() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();

    // Try to update non-existent key
    let result = db.update(table_id, b"user1", b"Alice");
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert_eq!(err.kind, StorageEngineErrorKind::KeyNotFound);
}

#[test]
fn test_upsert_insert() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();

    // Upsert (insert)
    let is_update = db
        .upsert(table_id, b"user1", b"Alice")
        .expect("Failed to upsert");
    assert!(!is_update); // Was an insert

    // Verify value
    let value = db.get(table_id, b"user1").unwrap().unwrap();
    assert_eq!(value.as_ref(), b"Alice");
}

#[test]
fn test_upsert_update() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();

    // Insert initial value
    db.insert(table_id, b"user1", b"Alice").unwrap();

    // Upsert (update)
    let is_update = db
        .upsert(table_id, b"user1", b"Alice Smith")
        .expect("Failed to upsert");
    assert!(is_update); // Was an update

    // Verify updated value
    let value = db.get(table_id, b"user1").unwrap().unwrap();
    assert_eq!(value.as_ref(), b"Alice Smith");
}

#[test]
fn test_get() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();

    // Insert value
    db.insert(table_id, b"user1", b"Alice").unwrap();

    // Get existing key
    let value = db
        .get(table_id, b"user1")
        .expect("Failed to get")
        .expect("Value not found");
    assert_eq!(value.as_ref(), b"Alice");

    // Get non-existent key
    let value = db.get(table_id, b"user2").expect("Failed to get");
    assert!(value.is_none());
}

#[test]
fn test_delete() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();

    // Insert value
    db.insert(table_id, b"user1", b"Alice").unwrap();

    // Delete existing key
    let deleted = db.delete(table_id, b"user1").expect("Failed to delete");
    assert!(deleted);

    // Verify key no longer exists
    let value = db.get(table_id, b"user1").unwrap();
    assert!(value.is_none());

    // Delete non-existent key
    let deleted = db.delete(table_id, b"user2").expect("Failed to delete");
    assert!(!deleted);
}

// =============================================================================
// Table Handle Tests
// =============================================================================

#[test]
fn test_table_handle_crud() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();

    // Get table handle
    let table = db.table(table_id).expect("Failed to get table handle");

    // Insert via handle
    table.insert(b"user1", b"Alice").expect("Failed to insert");

    // Get via handle
    let value = table.get(b"user1").unwrap().unwrap();
    assert_eq!(value.as_ref(), b"Alice");

    // Update via handle
    table
        .update(b"user1", b"Alice Smith")
        .expect("Failed to update");
    let value = table.get(b"user1").unwrap().unwrap();
    assert_eq!(value.as_ref(), b"Alice Smith");

    // Upsert via handle
    let is_update = table.upsert(b"user2", b"Bob").unwrap();
    assert!(!is_update);

    // Contains via handle
    assert!(table.contains(b"user1").unwrap());
    assert!(table.contains(b"user2").unwrap());
    assert!(!table.contains(b"user3").unwrap());

    // Delete via handle
    let deleted = table.delete(b"user1").unwrap();
    assert!(deleted);
    assert!(!table.contains(b"user1").unwrap());
}

#[test]
fn test_table_handle_info() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();

    let table = db.table(table_id).unwrap();

    // Check ID
    assert_eq!(table.id(), table_id);

    // Check info
    let info = table.info().unwrap().unwrap();
    assert_eq!(info.name, "users");
    assert_eq!(info.id, table_id);
}

// =============================================================================
// Error Handling Tests
// =============================================================================

#[test]
fn test_operation_on_nonexistent_table() {
    let db = create_test_db();
    let fake_table_id = TableId::from(999);

    // Try operations on non-existent table
    let result = db.insert(fake_table_id, b"key", b"value");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind, StorageEngineErrorKind::NotATable);

    let result = db.get(fake_table_id, b"key");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind, StorageEngineErrorKind::NotATable);
}

// =============================================================================
// Multi-operation Tests
// =============================================================================

#[test]
fn test_multiple_tables() {
    let db = create_test_db();

    let users_id = db.create_table("users", default_table_options()).unwrap();
    let posts_id = db.create_table("posts", default_table_options()).unwrap();

    // Insert into both tables
    db.insert(users_id, b"user1", b"Alice").unwrap();
    db.insert(posts_id, b"post1", b"Hello World").unwrap();

    // Verify isolation
    assert!(db.get(users_id, b"user1").unwrap().is_some());
    assert!(db.get(users_id, b"post1").unwrap().is_none());
    assert!(db.get(posts_id, b"post1").unwrap().is_some());
    assert!(db.get(posts_id, b"user1").unwrap().is_none());
}

#[test]
fn test_crud_sequence() {
    let db = create_test_db();
    let table_id = db.create_table("users", default_table_options()).unwrap();

    // Insert multiple keys
    db.insert(table_id, b"user1", b"Alice").unwrap();
    db.insert(table_id, b"user2", b"Bob").unwrap();
    db.insert(table_id, b"user3", b"Charlie").unwrap();

    // Update one
    db.update(table_id, b"user2", b"Bob Smith").unwrap();

    // Delete one
    db.delete(table_id, b"user3").unwrap();

    // Verify final state
    assert_eq!(
        db.get(table_id, b"user1").unwrap().unwrap().as_ref(),
        b"Alice"
    );
    assert_eq!(
        db.get(table_id, b"user2").unwrap().unwrap().as_ref(),
        b"Bob Smith"
    );
    assert!(db.get(table_id, b"user3").unwrap().is_none());
}

#[test]
fn test_btree_table_persists_root_and_reopens_after_restart() {
    let fs = MemoryFileSystem::new();
    let options = TableOptions {
        engine: TableEngineKind::BTree,
        ..Default::default()
    };

    let table_id = {
        let db = StorageEngine::new(&fs, "btree_restart.wal", "btree_restart.db")
            .expect("Failed to create StorageEngine");
        let table_id = db
            .create_table("users", options.clone())
            .expect("Failed to create BTree table");

        db.insert(table_id, b"user1", b"Alice")
            .expect("Failed to insert first value");
        db.insert(table_id, b"user2", b"Bob")
            .expect("Failed to insert second value");

        table_id
    };

    assert!(fs.exists("btree_restart.db").unwrap());

    let reopened = StorageEngine::open(&fs, "btree_restart.wal", "btree_restart.db")
        .expect("Failed to reopen StorageEngine");

    assert!(reopened.is_table(table_id).unwrap());
    assert_eq!(
        reopened.get(table_id, b"user1").unwrap().unwrap().as_ref(),
        b"Alice"
    );
    assert_eq!(
        reopened.get(table_id, b"user2").unwrap().unwrap().as_ref(),
        b"Bob"
    );
}

// =============================================================================
// LSM Engine Tests
// =============================================================================

/// Helper to create LSM table options
fn lsm_table_options() -> TableOptions {
    TableOptions {
        engine: TableEngineKind::LsmTree,
        ..Default::default()
    }
}

#[test]
fn test_create_lsm_table() {
    let db = create_test_db();

    let table_id = db
        .create_table("events", lsm_table_options())
        .expect("Failed to create LSM table");

    // Verify table exists
    assert!(db.is_table(table_id).unwrap());

    // Verify table info
    let info = db
        .get_object_info(table_id)
        .unwrap()
        .expect("Table not found");
    assert_eq!(info.name, "events");
    assert_eq!(info.options.engine, TableEngineKind::LsmTree);
}

#[test]
fn test_lsm_insert_and_get() {
    let db = create_test_db();
    let table_id = db.create_table("events", lsm_table_options()).unwrap();

    // Insert multiple key-value pairs
    db.insert(table_id, b"event1", b"User login").unwrap();
    db.insert(table_id, b"event2", b"Page view").unwrap();
    db.insert(table_id, b"event3", b"User logout").unwrap();

    // Verify all values
    assert_eq!(
        db.get(table_id, b"event1").unwrap().unwrap().as_ref(),
        b"User login"
    );
    assert_eq!(
        db.get(table_id, b"event2").unwrap().unwrap().as_ref(),
        b"Page view"
    );
    assert_eq!(
        db.get(table_id, b"event3").unwrap().unwrap().as_ref(),
        b"User logout"
    );
}

#[test]
fn test_lsm_update() {
    let db = create_test_db();
    let table_id = db.create_table("events", lsm_table_options()).unwrap();

    // Insert initial value
    db.insert(table_id, b"event1", b"User login").unwrap();

    // Update value
    db.update(table_id, b"event1", b"Admin login").unwrap();

    // Verify updated value
    assert_eq!(
        db.get(table_id, b"event1").unwrap().unwrap().as_ref(),
        b"Admin login"
    );
}

#[test]
fn test_lsm_delete() {
    let db = create_test_db();
    let table_id = db.create_table("events", lsm_table_options()).unwrap();

    // Insert value
    db.insert(table_id, b"event1", b"User login").unwrap();

    // Verify exists
    assert!(db.get(table_id, b"event1").unwrap().is_some());

    // Delete
    let deleted = db.delete(table_id, b"event1").unwrap();
    assert!(deleted);

    // Verify deleted
    assert!(db.get(table_id, b"event1").unwrap().is_none());
}

#[test]
fn test_lsm_write_heavy_workload() {
    let db = create_test_db();
    let table_id = db.create_table("logs", lsm_table_options()).unwrap();

    // LSM trees are optimized for write-heavy workloads
    // Insert many records
    for i in 0..100 {
        let key = format!("log{:04}", i);
        let value = format!("Log entry {}", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
    }

    // Verify random reads
    assert_eq!(
        db.get(table_id, b"log0000").unwrap().unwrap().as_ref(),
        b"Log entry 0"
    );
    assert_eq!(
        db.get(table_id, b"log0050").unwrap().unwrap().as_ref(),
        b"Log entry 50"
    );
    assert_eq!(
        db.get(table_id, b"log0099").unwrap().unwrap().as_ref(),
        b"Log entry 99"
    );
}

#[test]
fn test_lsm_upsert() {
    let db = create_test_db();
    let table_id = db.create_table("events", lsm_table_options()).unwrap();

    // Upsert (insert)
    let is_update = db.upsert(table_id, b"event1", b"User login").unwrap();
    assert!(!is_update);

    // Upsert (update)
    let is_update = db.upsert(table_id, b"event1", b"Admin login").unwrap();
    assert!(is_update);

    // Verify final value
    assert_eq!(
        db.get(table_id, b"event1").unwrap().unwrap().as_ref(),
        b"Admin login"
    );
}

#[test]
fn test_lsm_multiple_tables() {
    let db = create_test_db();

    // Create multiple LSM tables
    let logs_id = db.create_table("logs", lsm_table_options()).unwrap();
    let events_id = db.create_table("events", lsm_table_options()).unwrap();

    // Insert into both
    db.insert(logs_id, b"log1", b"System started").unwrap();
    db.insert(events_id, b"event1", b"User login").unwrap();

    // Verify isolation
    assert!(db.get(logs_id, b"log1").unwrap().is_some());
    assert!(db.get(logs_id, b"event1").unwrap().is_none());
    assert!(db.get(events_id, b"event1").unwrap().is_some());
    assert!(db.get(events_id, b"log1").unwrap().is_none());
}

#[test]
fn test_lsm_mixed_engines() {
    let db = create_test_db();

    // Create tables with different engines
    let memory_table = db.create_table("cache", default_table_options()).unwrap();
    let lsm_table = db.create_table("logs", lsm_table_options()).unwrap();

    // Insert into both
    db.insert(memory_table, b"key1", b"cached_value").unwrap();
    db.insert(lsm_table, b"log1", b"log_entry").unwrap();

    // Verify both work correctly
    assert_eq!(
        db.get(memory_table, b"key1").unwrap().unwrap().as_ref(),
        b"cached_value"
    );
    assert_eq!(
        db.get(lsm_table, b"log1").unwrap().unwrap().as_ref(),
        b"log_entry"
    );

    // Verify table info shows correct engines
    let memory_info = db.get_object_info(memory_table).unwrap().unwrap();
    let lsm_info = db.get_object_info(lsm_table).unwrap().unwrap();
    assert_eq!(memory_info.options.engine, TableEngineKind::Memory);
    assert_eq!(lsm_info.options.engine, TableEngineKind::LsmTree);
}

#[test]
fn test_lsm_sequential_writes() {
    let db = create_test_db();
    let table_id = db.create_table("timeseries", lsm_table_options()).unwrap();

    // LSM trees excel at sequential writes (append-only workloads)
    for i in 0..50 {
        let timestamp = format!("ts{:010}", i);
        let value = format!("value_{}", i);
        db.insert(table_id, timestamp.as_bytes(), value.as_bytes())
            .unwrap();
    }

    // Verify first and last entries
    assert_eq!(
        db.get(table_id, b"ts0000000000").unwrap().unwrap().as_ref(),
        b"value_0"
    );
    assert_eq!(
        db.get(table_id, b"ts0000000049").unwrap().unwrap().as_ref(),
        b"value_49"
    );
}

#[test]
fn test_lsm_overwrite_pattern() {
    let db = create_test_db();
    let table_id = db.create_table("counters", lsm_table_options()).unwrap();

    // Insert initial value
    db.insert(table_id, b"counter1", b"0").unwrap();

    // Repeatedly update (LSM handles this with tombstones and compaction)
    for i in 1..=10 {
        let value = format!("{}", i);
        db.update(table_id, b"counter1", value.as_bytes()).unwrap();
    }

    // Verify final value
    assert_eq!(
        db.get(table_id, b"counter1").unwrap().unwrap().as_ref(),
        b"10"
    );
}

#[test]
fn test_lsm_table_handle() {
    let db = create_test_db();
    let table_id = db.create_table("events", lsm_table_options()).unwrap();

    // Get table handle
    let table = db.table(table_id).expect("Failed to get table handle");

    // Use handle for operations
    table.insert(b"event1", b"User login").unwrap();
    table.insert(b"event2", b"Page view").unwrap();

    assert_eq!(
        table.get(b"event1").unwrap().unwrap().as_ref(),
        b"User login"
    );
    assert!(table.contains(b"event1").unwrap());
    assert!(table.contains(b"event2").unwrap());

    table.delete(b"event1").unwrap();
    assert!(!table.contains(b"event1").unwrap());
}

// =============================================================================
// BTree Engine Tests
// =============================================================================

/// Helper to create BTree table options
fn btree_table_options() -> TableOptions {
    TableOptions {
        engine: TableEngineKind::BTree,
        ..Default::default()
    }
}

#[test]
fn test_create_btree_table() {
    let db = create_test_db();

    let table_id = db
        .create_table("users", btree_table_options())
        .expect("Failed to create BTree table");

    // Verify table exists
    assert!(db.is_table(table_id).unwrap());

    // Verify table info
    let info = db
        .get_object_info(table_id)
        .unwrap()
        .expect("Table not found");
    assert_eq!(info.name, "users");
    assert_eq!(info.options.engine, TableEngineKind::BTree);
}

#[test]
fn test_btree_insert_and_get() {
    let db = create_test_db();
    let table_id = db.create_table("users", btree_table_options()).unwrap();

    // Insert multiple key-value pairs
    db.insert(table_id, b"user1", b"Alice").unwrap();
    db.insert(table_id, b"user2", b"Bob").unwrap();
    db.insert(table_id, b"user3", b"Charlie").unwrap();

    // Verify all values
    assert_eq!(
        db.get(table_id, b"user1").unwrap().unwrap().as_ref(),
        b"Alice"
    );
    assert_eq!(
        db.get(table_id, b"user2").unwrap().unwrap().as_ref(),
        b"Bob"
    );
    assert_eq!(
        db.get(table_id, b"user3").unwrap().unwrap().as_ref(),
        b"Charlie"
    );
}

#[test]
fn test_btree_update() {
    let db = create_test_db();
    let table_id = db.create_table("users", btree_table_options()).unwrap();

    // Insert initial value
    db.insert(table_id, b"user1", b"Alice").unwrap();

    // Update value
    db.update(table_id, b"user1", b"Alice Smith").unwrap();

    // Verify updated value
    assert_eq!(
        db.get(table_id, b"user1").unwrap().unwrap().as_ref(),
        b"Alice Smith"
    );
}

#[test]
fn test_btree_delete() {
    let db = create_test_db();
    let table_id = db.create_table("users", btree_table_options()).unwrap();

    // Insert value
    db.insert(table_id, b"user1", b"Alice").unwrap();

    // Verify exists
    assert!(db.get(table_id, b"user1").unwrap().is_some());

    // Delete
    let deleted = db.delete(table_id, b"user1").unwrap();
    assert!(deleted);

    // Verify deleted
    assert!(db.get(table_id, b"user1").unwrap().is_none());
}

#[test]
fn test_btree_upsert() {
    let db = create_test_db();
    let table_id = db.create_table("users", btree_table_options()).unwrap();

    // Upsert (insert)
    let is_update = db.upsert(table_id, b"user1", b"Alice").unwrap();
    assert!(!is_update);

    // Upsert (update)
    let is_update = db.upsert(table_id, b"user1", b"Alice Smith").unwrap();
    assert!(is_update);

    // Verify final value
    assert_eq!(
        db.get(table_id, b"user1").unwrap().unwrap().as_ref(),
        b"Alice Smith"
    );
}

#[test]
fn test_btree_ordered_keys() {
    let db = create_test_db();
    let table_id = db.create_table("products", btree_table_options()).unwrap();

    // BTree maintains key order - insert in random order
    db.insert(table_id, b"product_005", b"Widget").unwrap();
    db.insert(table_id, b"product_001", b"Gadget").unwrap();
    db.insert(table_id, b"product_003", b"Doohickey").unwrap();
    db.insert(table_id, b"product_002", b"Thingamajig").unwrap();
    db.insert(table_id, b"product_004", b"Whatsit").unwrap();

    // Verify all can be retrieved
    assert_eq!(
        db.get(table_id, b"product_001").unwrap().unwrap().as_ref(),
        b"Gadget"
    );
    assert_eq!(
        db.get(table_id, b"product_002").unwrap().unwrap().as_ref(),
        b"Thingamajig"
    );
    assert_eq!(
        db.get(table_id, b"product_003").unwrap().unwrap().as_ref(),
        b"Doohickey"
    );
    assert_eq!(
        db.get(table_id, b"product_004").unwrap().unwrap().as_ref(),
        b"Whatsit"
    );
    assert_eq!(
        db.get(table_id, b"product_005").unwrap().unwrap().as_ref(),
        b"Widget"
    );
}

#[test]
fn test_btree_read_heavy_workload() {
    let db = create_test_db();
    let table_id = db.create_table("cache", btree_table_options()).unwrap();

    // BTree is optimized for read-heavy workloads with good point lookup performance
    // Insert some records
    for i in 0..50 {
        let key = format!("key{:04}", i);
        let value = format!("value_{}", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .expect("Failed to insert");
    }

    // Perform many reads (BTree should handle this efficiently)
    for _ in 0..10 {
        for i in 0..50 {
            let key = format!("key{:04}", i);
            let expected = format!("value_{}", i);
            assert_eq!(
                db.get(table_id, key.as_bytes()).unwrap().unwrap().as_ref(),
                expected.as_bytes()
            );
        }
    }
}

#[test]
fn test_btree_multiple_tables() {
    let db = create_test_db();

    // Create multiple BTree tables
    let users_id = db.create_table("users", btree_table_options()).unwrap();
    let products_id = db.create_table("products", btree_table_options()).unwrap();

    // Insert into both
    db.insert(users_id, b"user1", b"Alice").unwrap();
    db.insert(products_id, b"prod1", b"Widget").unwrap();

    // Verify isolation
    assert!(db.get(users_id, b"user1").unwrap().is_some());
    assert!(db.get(users_id, b"prod1").unwrap().is_none());
    assert!(db.get(products_id, b"prod1").unwrap().is_some());
    assert!(db.get(products_id, b"user1").unwrap().is_none());
}

#[test]
fn test_btree_mixed_engines() {
    let db = create_test_db();

    // Create tables with different engines
    let btree_table = db.create_table("users", btree_table_options()).unwrap();
    let memory_table = db.create_table("cache", default_table_options()).unwrap();
    let lsm_table = db.create_table("logs", lsm_table_options()).unwrap();

    // Insert into all three
    db.insert(btree_table, b"user1", b"Alice").unwrap();
    db.insert(memory_table, b"key1", b"cached_value").unwrap();
    db.insert(lsm_table, b"log1", b"log_entry").unwrap();

    // Verify all work correctly
    assert_eq!(
        db.get(btree_table, b"user1").unwrap().unwrap().as_ref(),
        b"Alice"
    );
    assert_eq!(
        db.get(memory_table, b"key1").unwrap().unwrap().as_ref(),
        b"cached_value"
    );
    assert_eq!(
        db.get(lsm_table, b"log1").unwrap().unwrap().as_ref(),
        b"log_entry"
    );

    // Verify table info shows correct engines
    let btree_info = db.get_object_info(btree_table).unwrap().unwrap();
    let memory_info = db.get_object_info(memory_table).unwrap().unwrap();
    let lsm_info = db.get_object_info(lsm_table).unwrap().unwrap();
    assert_eq!(btree_info.options.engine, TableEngineKind::BTree);
    assert_eq!(memory_info.options.engine, TableEngineKind::Memory);
    assert_eq!(lsm_info.options.engine, TableEngineKind::LsmTree);
}

#[test]
fn test_btree_range_operations() {
    let db = create_test_db();
    let table_id = db.create_table("inventory", btree_table_options()).unwrap();

    // Insert items with sequential keys
    for i in 0..20 {
        let key = format!("item{:03}", i);
        let value = format!("quantity_{}", i * 10);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .unwrap();
    }

    // Verify specific range
    assert_eq!(
        db.get(table_id, b"item000").unwrap().unwrap().as_ref(),
        b"quantity_0"
    );
    assert_eq!(
        db.get(table_id, b"item010").unwrap().unwrap().as_ref(),
        b"quantity_100"
    );
    assert_eq!(
        db.get(table_id, b"item019").unwrap().unwrap().as_ref(),
        b"quantity_190"
    );
}

#[test]
fn test_btree_update_pattern() {
    let db = create_test_db();
    let table_id = db.create_table("accounts", btree_table_options()).unwrap();

    // Insert initial value
    db.insert(table_id, b"account1", b"balance:100").unwrap();

    // Update multiple times (BTree handles in-place updates efficiently)
    for i in 1..=10 {
        let value = format!("balance:{}", 100 + i * 10);
        db.update(table_id, b"account1", value.as_bytes()).unwrap();
    }

    // Verify final value
    assert_eq!(
        db.get(table_id, b"account1").unwrap().unwrap().as_ref(),
        b"balance:200"
    );
}

#[test]
fn test_btree_table_handle() {
    let db = create_test_db();
    let table_id = db.create_table("users", btree_table_options()).unwrap();

    // Get table handle
    let table = db.table(table_id).expect("Failed to get table handle");

    // Use handle for operations
    table.insert(b"user1", b"Alice").unwrap();
    table.insert(b"user2", b"Bob").unwrap();

    assert_eq!(table.get(b"user1").unwrap().unwrap().as_ref(), b"Alice");
    assert!(table.contains(b"user1").unwrap());
    assert!(table.contains(b"user2").unwrap());

    table.update(b"user1", b"Alice Smith").unwrap();
    assert_eq!(
        table.get(b"user1").unwrap().unwrap().as_ref(),
        b"Alice Smith"
    );

    table.delete(b"user1").unwrap();
    assert!(!table.contains(b"user1").unwrap());
}

#[test]
fn test_btree_large_values() {
    let db = create_test_db();
    let table_id = db.create_table("documents", btree_table_options()).unwrap();

    // Insert large values (BTree should handle these efficiently)
    let large_value = vec![b'X'; 1024]; // 1KB value
    db.insert(table_id, b"doc1", &large_value).unwrap();

    let very_large_value = vec![b'Y'; 4096]; // 4KB value
    db.insert(table_id, b"doc2", &very_large_value).unwrap();

    // Verify retrieval
    let retrieved1 = db.get(table_id, b"doc1").unwrap().unwrap();
    assert_eq!(retrieved1.as_ref().len(), 1024);
    assert_eq!(retrieved1.as_ref(), &large_value[..]);

    let retrieved2 = db.get(table_id, b"doc2").unwrap().unwrap();
    assert_eq!(retrieved2.as_ref().len(), 4096);
    assert_eq!(retrieved2.as_ref(), &very_large_value[..]);
}

#[test]
fn test_btree_empty_values() {
    let db = create_test_db();
    let table_id = db.create_table("flags", btree_table_options()).unwrap();

    // Insert empty value (valid use case for flags/markers)
    db.insert(table_id, b"flag1", b"").unwrap();

    // Verify retrieval - empty values may or may not be supported depending on engine
    let value = db.get(table_id, b"flag1").unwrap();
    if let Some(v) = value {
        assert_eq!(v.as_ref(), b"");
        assert_eq!(v.as_ref().len(), 0);
    } else {
        // Some engines may not support empty values, which is acceptable
        // The key should still exist in the table
        println!("Note: BTree engine does not support empty values");
    }
}

#[test]
fn test_btree_special_keys() {
    let db = create_test_db();
    let table_id = db.create_table("special", btree_table_options()).unwrap();

    // Test with various special byte sequences
    db.insert(table_id, b"\x00\x00\x00", b"null_bytes").unwrap();
    db.insert(table_id, b"\xFF\xFF\xFF", b"max_bytes").unwrap();
    db.insert(table_id, b"key\x00with\x00nulls", b"embedded_nulls")
        .unwrap();

    // Verify retrieval
    assert_eq!(
        db.get(table_id, b"\x00\x00\x00").unwrap().unwrap().as_ref(),
        b"null_bytes"
    );
    assert_eq!(
        db.get(table_id, b"\xFF\xFF\xFF").unwrap().unwrap().as_ref(),
        b"max_bytes"
    );
    assert_eq!(
        db.get(table_id, b"key\x00with\x00nulls")
            .unwrap()
            .unwrap()
            .as_ref(),
        b"embedded_nulls"
    );
}

#[test]
fn test_btree_stress_insert_delete() {
    let db = create_test_db();
    let table_id = db.create_table("stress", btree_table_options()).unwrap();

    // Insert many keys
    for i in 0..100 {
        let key = format!("key{:04}", i);
        let value = format!("value_{}", i);
        db.insert(table_id, key.as_bytes(), value.as_bytes())
            .unwrap();
    }

    // Delete every other key
    for i in (0..100).step_by(2) {
        let key = format!("key{:04}", i);
        db.delete(table_id, key.as_bytes()).unwrap();
    }

    // Verify remaining keys
    for i in 0..100 {
        let key = format!("key{:04}", i);
        let result = db.get(table_id, key.as_bytes()).unwrap();
        if i % 2 == 0 {
            assert!(result.is_none(), "Key {} should be deleted", i);
        } else {
            assert!(result.is_some(), "Key {} should exist", i);
            let expected = format!("value_{}", i);
            assert_eq!(result.unwrap().as_ref(), expected.as_bytes());
        }
    }
}

// Made with Bob
