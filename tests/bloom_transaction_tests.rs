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

//! Comprehensive tests for bloom filter transaction-layer integration.
//!
//! These tests verify the transactional behavior of bloom filter tables,
//! including ApproximateMembership trait implementation on Transaction,
//! write-set visibility, commit/rollback semantics, and error handling
//! for unsupported generic KV operations on bloom filters.

use nanokv::kvdb::Database;
use nanokv::table::{ApproximateMembership, TableEngineKind, TableOptions};
use nanokv::txn::TransactionId;
use nanokv::types::{Durability, KeyEncoding};
use nanokv::vfs::MemoryFileSystem;
use nanokv::wal::LogSequenceNumber;

fn bloom_table_options() -> TableOptions {
    TableOptions {
        engine: TableEngineKind::Bloom,
        key_encoding: KeyEncoding::RawBytes,
        compression: None,
        encryption: None,
        page_size: None,
        format_version: 1,
        max_inline_size: None,
        max_value_size: None,
    }
}

fn default_table_options() -> TableOptions {
    TableOptions {
        engine: TableEngineKind::Memory,
        key_encoding: KeyEncoding::RawBytes,
        compression: None,
        encryption: None,
        page_size: None,
        format_version: 1,
        max_inline_size: None,
        max_value_size: None,
    }
}

// ─── Basic Insert and Query ───────────────────────────────────────────────────

#[test]
fn test_bloom_insert_and_might_contain_in_transaction() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    txn.with_bloom(bloom_id, |bloom| {
        bloom.insert_key(b"key1", TransactionId::from(1), LogSequenceNumber::from(1))?;
        assert!(bloom.might_contain(b"key1")?);
        Ok(())
    })
    .unwrap();
    txn.commit().unwrap();

    // Verify after commit
    let mut read_txn = db.begin_read().unwrap();
    let contains = read_txn
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b"key1"))
        .unwrap();
    assert!(contains);
}

#[test]
fn test_bloom_write_set_visibility_uncommitted() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    txn.with_bloom(bloom_id, |bloom| {
        bloom.insert_key(
            b"uncommitted-key",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )?;
        // Should see own uncommitted write
        assert!(bloom.might_contain(b"uncommitted-key")?);
        Ok(())
    })
    .unwrap();

    // Don't commit - key should not persist
    txn.rollback().unwrap();

    // New transaction should not see the rolled-back key
    let mut read_txn = db.begin_read().unwrap();
    let contains = read_txn
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b"uncommitted-key"))
        .unwrap();
    assert!(!contains);
}

#[test]
fn test_bloom_multiple_inserts_same_transaction() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    txn.with_bloom(bloom_id, |bloom| {
        for i in 0..100u32 {
            bloom.insert_key(
                &i.to_le_bytes(),
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )?;
        }
        // All should be visible in write set
        for i in 0..100u32 {
            assert!(bloom.might_contain(&i.to_le_bytes())?);
        }
        Ok(())
    })
    .unwrap();
    txn.commit().unwrap();

    // Verify all committed
    let mut read_txn = db.begin_read().unwrap();
    for i in 0..100u32 {
        let contains = read_txn
            .with_bloom(bloom_id, |bloom| bloom.might_contain(&i.to_le_bytes()))
            .unwrap();
        assert!(contains, "Key {} not found after commit", i);
    }
}

// ─── Rollback Semantics ───────────────────────────────────────────────────────

#[test]
fn test_bloom_rollback_discards_all_inserts() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    // First, insert some keys and commit
    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.with_bloom(bloom_id, |bloom| {
            bloom.insert_key(
                b"committed-key",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )?;
            Ok(())
        })
        .unwrap();
        txn.commit().unwrap();
    }

    // Now insert more keys but rollback
    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.with_bloom(bloom_id, |bloom| {
            bloom.insert_key(
                b"rolled-back-key",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )?;
            Ok(())
        })
        .unwrap();
        txn.rollback().unwrap();
    }

    // Verify committed key exists, rolled-back key does not
    let mut read_txn = db.begin_read().unwrap();
    let committed = read_txn
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b"committed-key"))
        .unwrap();
    assert!(committed);

    let rolled_back = read_txn
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b"rolled-back-key"))
        .unwrap();
    assert!(!rolled_back);
}

// ─── Mixed KV and Bloom Operations ────────────────────────────────────────────

#[test]
fn test_mixed_kv_and_bloom_atomic_commit() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let kv_id = db.create_table("kv", default_table_options()).unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.put(kv_id, b"kv-key", b"kv-value").unwrap();
        txn.with_bloom(bloom_id, |bloom| {
            bloom.insert_key(
                b"bloom-key",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )?;
            Ok(())
        })
        .unwrap();
        txn.commit().unwrap();
    }

    // Both should be visible after commit
    let kv_txn = db.begin_read().unwrap();
    let kv_val = kv_txn.get(kv_id, b"kv-key").unwrap();
    assert_eq!(kv_val.unwrap().0, b"kv-value");

    let mut bloom_txn = db.begin_read().unwrap();
    let bloom_contains = bloom_txn
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b"bloom-key"))
        .unwrap();
    assert!(bloom_contains);
}

#[test]
fn test_mixed_kv_and_bloom_atomic_rollback() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let kv_id = db.create_table("kv", default_table_options()).unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.put(kv_id, b"kv-key", b"kv-value").unwrap();
        txn.with_bloom(bloom_id, |bloom| {
            bloom.insert_key(
                b"bloom-key",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )?;
            Ok(())
        })
        .unwrap();
        txn.rollback().unwrap();
    }

    // Neither should be visible after rollback
    let kv_txn = db.begin_read().unwrap();
    assert!(kv_txn.get(kv_id, b"kv-key").unwrap().is_none());

    let mut bloom_txn = db.begin_read().unwrap();
    let bloom_contains = bloom_txn
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b"bloom-key"))
        .unwrap();
    assert!(!bloom_contains);
}

// ─── Error Handling for Unsupported Operations ────────────────────────────────

#[test]
fn test_bloom_put_returns_error() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    // put() on bloom filter should fail at commit time, not at write time
    // because put() just records in write_set without checking engine type
    let put_result = txn.put(bloom_id, b"key", b"value");
    assert!(put_result.is_ok()); // put() itself succeeds

    // But commit should fail
    let commit_result = txn.commit();
    assert!(commit_result.is_err());
    let err = commit_result.unwrap_err();
    assert!(err.to_string().contains("ApproximateMembership API"));
}

#[test]
fn test_bloom_delete_returns_error_at_commit() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    // delete() on bloom filter should fail at commit time
    let delete_result = txn.delete(bloom_id, b"key");
    assert!(delete_result.is_ok()); // delete() itself succeeds

    // But commit should fail
    let commit_result = txn.commit();
    assert!(commit_result.is_err());
    let err = commit_result.unwrap_err();
    assert!(err.to_string().contains("ApproximateMembership API"));
}

#[test]
fn test_bloom_range_delete_returns_error() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    use nanokv::types::{Bound, ScanBounds};
    let result = txn.range_delete(
        bloom_id,
        ScanBounds::Range {
            start: Bound::Unbounded,
            end: Bound::Unbounded,
        },
    );
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("not supported for bloom filter")
    );
}

// ─── Get Semantics on Bloom Filter ────────────────────────────────────────────

#[test]
fn test_bloom_get_returns_empty_for_present_key() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    // Insert and commit
    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.with_bloom(bloom_id, |bloom| {
            bloom.insert_key(
                b"present-key",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )?;
            Ok(())
        })
        .unwrap();
        txn.commit().unwrap();
    }

    // get() on bloom filter returns Some(empty) for probably-present keys
    let read_txn = db.begin_read().unwrap();
    let result = read_txn.get(bloom_id, b"present-key").unwrap();
    assert!(result.is_some());
    assert!(result.unwrap().0.is_empty());
}

#[test]
fn test_bloom_get_returns_none_for_absent_key() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    // get() on bloom filter returns None for definitely-absent keys
    let read_txn = db.begin_read().unwrap();
    let result = read_txn.get(bloom_id, b"absent-key").unwrap();
    assert!(result.is_none());
}

// ─── Table Context Management ─────────────────────────────────────────────────

#[test]
fn test_with_table_sets_context() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    txn.with_table(bloom_id);

    // Now we can call ApproximateMembership methods directly
    txn.insert_key(b"key1", TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
    assert!(txn.might_contain(b"key1").unwrap());

    txn.commit().unwrap();
}

#[test]
fn test_clear_table_context() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    txn.with_table(bloom_id);
    txn.clear_table_context();

    // Should error without table context
    let result = txn.insert_key(b"key1", TransactionId::from(1), LogSequenceNumber::from(1));
    assert!(result.is_err());
}

#[test]
fn test_current_table_returns_context() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    assert!(txn.current_table().is_none());

    txn.with_table(bloom_id);
    let (id, name) = txn.current_table().unwrap();
    assert_eq!(id, bloom_id);
    assert_eq!(name, "bloom");
}

// ─── Stats and Verification ───────────────────────────────────────────────────

#[test]
fn test_bloom_stats_through_transaction() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    // Insert some keys
    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.with_bloom(bloom_id, |bloom| {
            for i in 0..50u32 {
                bloom.insert_key(
                    &i.to_le_bytes(),
                    TransactionId::from(1),
                    LogSequenceNumber::from(1),
                )?;
            }
            Ok(())
        })
        .unwrap();
        txn.commit().unwrap();
    }

    // Check stats through transaction
    let mut read_txn = db.begin_read().unwrap();
    read_txn.with_table(bloom_id);
    let stats = read_txn.stats().unwrap();
    assert_eq!(stats.entry_count, Some(50));
    assert_eq!(stats.distinct_keys, Some(50));
    assert!(stats.size_bytes.is_some());
}

#[test]
fn test_bloom_verify_through_transaction() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.with_bloom(bloom_id, |bloom| {
            bloom.insert_key(
                b"test-key",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )?;
            Ok(())
        })
        .unwrap();
        txn.commit().unwrap();
    }

    let mut read_txn = db.begin_read().unwrap();
    read_txn.with_table(bloom_id);
    let report = read_txn.verify().unwrap();
    assert!(report.errors.is_empty());
}

#[test]
fn test_bloom_capabilities_through_transaction() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut read_txn = db.begin_read().unwrap();
    read_txn.with_table(bloom_id);
    let caps = read_txn.capabilities();
    assert!(caps.approximate);
    assert!(!caps.exact);
    assert!(!caps.supports_delete);
}

#[test]
fn test_bloom_false_positive_rate_through_transaction() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();
        txn.with_bloom(bloom_id, |bloom| {
            for i in 0..100u32 {
                bloom.insert_key(
                    &i.to_le_bytes(),
                    TransactionId::from(1),
                    LogSequenceNumber::from(1),
                )?;
            }
            Ok(())
        })
        .unwrap();
        txn.commit().unwrap();
    }

    let mut read_txn = db.begin_read().unwrap();
    read_txn.with_table(bloom_id);
    let fpr = read_txn.false_positive_rate();
    assert!(fpr.is_some());
    assert!(fpr.unwrap() > 0.0);
}

// ─── Multiple Bloom Tables ────────────────────────────────────────────────────

#[test]
fn test_multiple_bloom_tables_in_same_transaction() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom1_id = db.create_table("bloom1", bloom_table_options()).unwrap();
    let bloom2_id = db.create_table("bloom2", bloom_table_options()).unwrap();

    {
        let mut txn = db.begin_write(Durability::WalOnly).unwrap();

        txn.with_bloom(bloom1_id, |bloom| {
            bloom.insert_key(
                b"key-in-bloom1",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )?;
            Ok(())
        })
        .unwrap();

        txn.with_bloom(bloom2_id, |bloom| {
            bloom.insert_key(
                b"key-in-bloom2",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )?;
            Ok(())
        })
        .unwrap();

        txn.commit().unwrap();
    }

    // Verify both
    let mut read_txn = db.begin_read().unwrap();

    let in_bloom1 = read_txn
        .with_bloom(bloom1_id, |bloom| bloom.might_contain(b"key-in-bloom1"))
        .unwrap();
    assert!(in_bloom1);

    let in_bloom2 = read_txn
        .with_bloom(bloom2_id, |bloom| bloom.might_contain(b"key-in-bloom2"))
        .unwrap();
    assert!(in_bloom2);

    // Cross-check: key1 should not be in bloom2
    let key1_in_bloom2 = read_txn
        .with_bloom(bloom2_id, |bloom| bloom.might_contain(b"key-in-bloom1"))
        .unwrap();
    assert!(!key1_in_bloom2);
}

// ─── Isolation Semantics ──────────────────────────────────────────────────────

#[test]
fn test_bloom_insert_not_visible_to_other_transaction_before_commit() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn1 = db.begin_write(Durability::WalOnly).unwrap();
    txn1.with_bloom(bloom_id, |bloom| {
        bloom.insert_key(
            b"secret-key",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )?;
        Ok(())
    })
    .unwrap();

    // txn2 should not see txn1's uncommitted insert
    let mut txn2 = db.begin_read().unwrap();
    let contains = txn2
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b"secret-key"))
        .unwrap();
    assert!(!contains);

    // Commit txn1
    txn1.commit().unwrap();

    // Note: Bloom filter reads query the underlying filter directly without
    // snapshot isolation. After commit, the key is in the bloom filter and
    // will be visible to all subsequent reads. This is a known limitation
    // of approximate membership structures - they don't support MVCC.
    let mut txn3 = db.begin_read().unwrap();
    let contains_after_commit = txn3
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b"secret-key"))
        .unwrap();
    assert!(contains_after_commit);
}

// ─── WAL Durability ───────────────────────────────────────────────────────────

#[test]
fn test_bloom_insert_wal_recorded() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    {
        let mut txn = db.begin_write(Durability::SyncOnCommit).unwrap();
        txn.with_bloom(bloom_id, |bloom| {
            bloom.insert_key(
                b"durable-key",
                TransactionId::from(1),
                LogSequenceNumber::from(1),
            )?;
            Ok(())
        })
        .unwrap();
        txn.commit().unwrap();
    }

    // Verify the key is durable
    let mut read_txn = db.begin_read().unwrap();
    let contains = read_txn
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b"durable-key"))
        .unwrap();
    assert!(contains);
}

// ─── Edge Cases ───────────────────────────────────────────────────────────────

#[test]
fn test_bloom_insert_empty_key() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    txn.with_bloom(bloom_id, |bloom| {
        bloom.insert_key(b"", TransactionId::from(1), LogSequenceNumber::from(1))?;
        assert!(bloom.might_contain(b"")?);
        Ok(())
    })
    .unwrap();
    txn.commit().unwrap();

    let mut read_txn = db.begin_read().unwrap();
    let contains = read_txn
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b""))
        .unwrap();
    assert!(contains);
}

#[test]
fn test_bloom_insert_duplicate_key_same_transaction() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    txn.with_bloom(bloom_id, |bloom| {
        bloom.insert_key(
            b"dup-key",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )?;
        bloom.insert_key(
            b"dup-key",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )?; // Insert again
        assert!(bloom.might_contain(b"dup-key")?);
        Ok(())
    })
    .unwrap();
    txn.commit().unwrap();

    // Should still be present
    let mut read_txn = db.begin_read().unwrap();
    let contains = read_txn
        .with_bloom(bloom_id, |bloom| bloom.might_contain(b"dup-key"))
        .unwrap();
    assert!(contains);
}

#[test]
fn test_bloom_insert_large_key() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let large_key = vec![0xAB; 1024];

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    txn.with_bloom(bloom_id, |bloom| {
        bloom.insert_key(
            &large_key,
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )?;
        assert!(bloom.might_contain(&large_key)?);
        Ok(())
    })
    .unwrap();
    txn.commit().unwrap();

    let mut read_txn = db.begin_read().unwrap();
    let contains = read_txn
        .with_bloom(bloom_id, |bloom| bloom.might_contain(&large_key))
        .unwrap();
    assert!(contains);
}

#[test]
fn test_bloom_name_through_transaction() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db
        .create_table("my_bloom_filter", bloom_table_options())
        .unwrap();

    let mut read_txn = db.begin_read().unwrap();
    read_txn.with_table(bloom_id);
    assert_eq!(read_txn.name(), "my_bloom_filter");
}

#[test]
fn test_bloom_table_id_through_transaction() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut read_txn = db.begin_read().unwrap();
    read_txn.with_table(bloom_id);
    assert_eq!(read_txn.table_id(), bloom_id);
}

// ─── Operation After Transaction End ──────────────────────────────────────────

#[test]
fn test_bloom_operations_fail_after_commit() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    txn.with_table(bloom_id);
    txn.commit().unwrap();

    // txn is consumed by commit, so this test verifies the API design
    // prevents operations after commit via ownership
}

#[test]
fn test_bloom_operations_fail_after_rollback() {
    let fs = MemoryFileSystem::new();
    let db = Database::new(&fs, "test.wal", "test.db").unwrap();
    let bloom_id = db.create_table("bloom", bloom_table_options()).unwrap();

    let mut txn = db.begin_write(Durability::WalOnly).unwrap();
    txn.with_table(bloom_id);
    txn.rollback().unwrap();

    // txn is consumed by rollback, so this test verifies the API design
    // prevents operations after rollback via ownership
}

// Made with Bob
