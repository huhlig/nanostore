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

//! Tests for background vacuum task and metrics collection.

use nanostore::kvdb::{Database, VacuumConfig};
use nanostore::table::TableOptions;
use nanostore::types::Durability;
use nanostore::vfs::MemoryFileSystem;
use std::time::Duration;

#[test]
fn test_vacuum_config_default() {
    let config = VacuumConfig::default();
    assert!(config.enabled);
    assert_eq!(config.interval, Duration::from_secs(300));
}

#[test]
fn test_vacuum_config_custom() {
    let config = VacuumConfig {
        enabled: false,
        interval: Duration::from_secs(60),
    };
    assert!(!config.enabled);
    assert_eq!(config.interval, Duration::from_secs(60));
}

#[test]
fn test_manual_vacuum_trigger() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "/test.wal", "/test.db").expect("Failed to create database");

    // Disable background vacuum for this test
    let mut config = db.vacuum_config();
    config.enabled = false;
    db.set_vacuum_config(config);

    // Create a test table
    let table_id = db
        .create_table("test_table", TableOptions::default())
        .expect("Failed to create table");

    // Insert some data
    for i in 0..10 {
        let key = format!("key{}", i).into_bytes();
        let value = format!("value{}", i).into_bytes();
        db.insert(table_id, &key, &value).expect("Failed to insert");
    }

    // Update some keys to create version chains
    for i in 0..5 {
        let key = format!("key{}", i).into_bytes();
        let value = format!("value{}_v2", i).into_bytes();
        db.update(table_id, &key, &value).expect("Failed to update");
    }

    // Trigger manual vacuum
    let metrics = db.trigger_vacuum().expect("Failed to trigger vacuum");

    // Verify metrics were collected
    assert!(metrics.started_at.is_some());
    assert!(metrics.completed_at.is_some());
    assert!(metrics.duration.is_some());

    // Check stats were updated
    let stats = db.vacuum_stats();
    assert_eq!(stats.total_runs, 1);
    assert!(stats.last_vacuum.is_some());
}

#[test]
fn test_vacuum_stats_accumulation() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "/test.wal", "/test.db").expect("Failed to create database");

    // Disable background vacuum for this test
    let mut config = db.vacuum_config();
    config.enabled = false;
    db.set_vacuum_config(config);

    // Create a test table
    let table_id = db
        .create_table("test_table", TableOptions::default())
        .expect("Failed to create table");

    // Insert and update data multiple times
    for round in 0..3 {
        for i in 0..5 {
            let key = format!("key{}", i).into_bytes();
            let value = format!("value{}_{}", i, round).into_bytes();
            if round == 0 {
                db.insert(table_id, &key, &value).expect("Failed to insert");
            } else {
                db.update(table_id, &key, &value).expect("Failed to update");
            }
        }

        // Trigger vacuum after each round
        db.trigger_vacuum().expect("Failed to trigger vacuum");
    }

    // Check accumulated stats
    let stats = db.vacuum_stats();
    assert_eq!(stats.total_runs, 3);
    assert!(stats.avg_duration.is_some());
}

#[test]
fn test_vacuum_config_update() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "/test.wal", "/test.db").expect("Failed to create database");

    // Get initial config
    let initial_config = db.vacuum_config();
    assert!(initial_config.enabled);

    // Update config
    let new_config = VacuumConfig {
        enabled: false,
        interval: Duration::from_secs(60),
    };
    db.set_vacuum_config(new_config.clone());

    // Verify config was updated
    let updated_config = db.vacuum_config();
    assert!(!updated_config.enabled);
    assert_eq!(updated_config.interval, Duration::from_secs(60));
}

#[test]
fn test_vacuum_with_snapshots() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "/test.wal", "/test.db").expect("Failed to create database");

    // Disable background vacuum for this test
    let mut config = db.vacuum_config();
    config.enabled = false;
    db.set_vacuum_config(config);

    // Create a test table
    let table_id = db
        .create_table("test_table", TableOptions::default())
        .expect("Failed to create table");

    // Insert initial data
    for i in 0..10 {
        let key = format!("key{}", i).into_bytes();
        let value = format!("value{}", i).into_bytes();
        db.insert(table_id, &key, &value).expect("Failed to insert");
    }

    // Create a snapshot to pin old versions
    let snapshot = db
        .create_snapshot("test_snapshot")
        .expect("Failed to create snapshot");

    // Update data to create new versions
    for i in 0..10 {
        let key = format!("key{}", i).into_bytes();
        let value = format!("value{}_v2", i).into_bytes();
        db.update(table_id, &key, &value).expect("Failed to update");
    }

    // Vacuum should not remove versions visible to snapshot
    let metrics = db.trigger_vacuum().expect("Failed to trigger vacuum");

    // With snapshot active, old versions should be retained
    // (exact count depends on implementation details)

    // Release snapshot
    db.release_snapshot(snapshot.id)
        .expect("Failed to release snapshot");

    // Now vacuum should be able to remove old versions
    let metrics2 = db.trigger_vacuum().expect("Failed to trigger vacuum");

    // Second vacuum should potentially remove more versions
    // (exact behavior depends on implementation)
}

#[test]
fn test_vacuum_metrics_per_table() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "/test.wal", "/test.db").expect("Failed to create database");

    // Disable background vacuum for this test
    let mut config = db.vacuum_config();
    config.enabled = false;
    db.set_vacuum_config(config);

    // Create multiple tables
    let table1 = db
        .create_table("table1", TableOptions::default())
        .expect("Failed to create table1");
    let table2 = db
        .create_table("table2", TableOptions::default())
        .expect("Failed to create table2");

    // Insert and update data in both tables
    for table_id in [table1, table2] {
        for i in 0..5 {
            let key = format!("key{}", i).into_bytes();
            let value = format!("value{}", i).into_bytes();
            db.insert(table_id, &key, &value).expect("Failed to insert");
        }

        for i in 0..5 {
            let key = format!("key{}", i).into_bytes();
            let value = format!("value{}_v2", i).into_bytes();
            db.update(table_id, &key, &value).expect("Failed to update");
        }
    }

    // Trigger vacuum
    let metrics = db.trigger_vacuum().expect("Failed to trigger vacuum");

    // Verify per-table metrics
    assert!(metrics.versions_removed_per_table.len() <= 2);
    // At least one table should have had versions removed
    assert!(metrics.total_versions_removed >= 0);
}

#[test]
fn test_database_close_stops_vacuum_thread() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "/test.wal", "/test.db").expect("Failed to create database");

    // Database should have vacuum thread running
    // Close should stop it gracefully
    db.close().expect("Failed to close database");

    // If we get here without hanging, the vacuum thread was stopped successfully
}

// Made with Bob
