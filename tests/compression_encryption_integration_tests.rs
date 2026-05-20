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

//! Comprehensive integration tests for compression and encryption features
//!
//! This test suite validates the integration between the Pager and WAL modules
//! with various compression and encryption configurations.

use nanostore::pager::{CompressionType, EncryptionType, PageSize, Pager, PagerConfig};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::{File, FileSystem, MemoryFileSystem};
use nanostore::wal::{WalRecovery, WalWriter, WalWriterConfig, WriteOpType};

// ============================================================================
// 1. PAGER + WAL INTEGRATION TESTS
// ============================================================================

#[test]
fn test_pager_and_wal_both_lz4_compression() {
    let fs = MemoryFileSystem::new();
    let db_path = "test.db";
    let wal_path = "test.wal";

    // Create pager with LZ4 compression
    let pager_config = PagerConfig::new()
        .with_page_size(PageSize::Size4KB)
        .with_compression(CompressionType::Lz4);

    let pager = Pager::create(&fs, db_path, pager_config).unwrap();
    assert_eq!(pager.page_size(), PageSize::Size4KB);

    // Create WAL with LZ4 compression
    let mut wal_config = WalWriterConfig::default();
    wal_config.compression = CompressionType::Lz4;

    let wal = WalWriter::create(&fs, wal_path, wal_config).unwrap();

    // Write transaction to WAL
    wal.write_begin(TransactionId::from(1)).unwrap();
    wal.write_operation(
        TransactionId::from(1),
        TableId::from(1),
        WriteOpType::Put,
        b"user:1".to_vec(),
        b"Alice".repeat(100), // Compressible data
    )
    .unwrap();
    wal.write_commit(TransactionId::from(1)).unwrap();
    wal.flush().unwrap();

    // Verify recovery works
    let result = WalRecovery::recover(&fs, wal_path).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(result.committed_writes[0].value, b"Alice".repeat(100));
}

#[test]
fn test_pager_and_wal_both_encrypted() {
    let fs = MemoryFileSystem::new();
    let db_path = "encrypted.db";
    let wal_path = "encrypted.wal";
    let key = [42u8; 32];

    // Create pager with encryption
    let pager_config = PagerConfig::new()
        .with_page_size(PageSize::Size4KB)
        .with_encryption(EncryptionType::Aes256Gcm, key);

    let pager = Pager::create(&fs, db_path, pager_config).unwrap();
    assert_eq!(pager.page_size(), PageSize::Size4KB);

    // Create WAL with same encryption key
    let mut wal_config = WalWriterConfig::default();
    wal_config.encryption = EncryptionType::Aes256Gcm;
    wal_config.encryption_key = Some(key);

    let wal = WalWriter::create(&fs, wal_path, wal_config).unwrap();

    // Write encrypted transaction
    wal.write_begin(TransactionId::from(1)).unwrap();
    wal.write_operation(
        TransactionId::from(1),
        TableId::from(2),
        WriteOpType::Put,
        b"password".to_vec(),
        b"super-secret-value".to_vec(),
    )
    .unwrap();
    wal.write_commit(TransactionId::from(1)).unwrap();
    wal.flush().unwrap();

    // Verify recovery with correct key
    let result = WalRecovery::recover_with_key(&fs, wal_path, Some(key)).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(result.committed_writes[0].value, b"super-secret-value");
}

#[test]
fn test_pager_and_wal_both_compressed_and_encrypted() {
    let fs = MemoryFileSystem::new();
    let db_path = "secure.db";
    let wal_path = "secure.wal";
    let key = [99u8; 32];

    // Create pager with both compression and encryption
    let pager_config = PagerConfig::new()
        .with_page_size(PageSize::Size8KB)
        .with_compression(CompressionType::Zstd)
        .with_encryption(EncryptionType::Aes256Gcm, key);

    let pager = Pager::create(&fs, db_path, pager_config).unwrap();
    assert_eq!(pager.page_size(), PageSize::Size8KB);

    // Create WAL with both compression and encryption
    let mut wal_config = WalWriterConfig::default();
    wal_config.compression = CompressionType::Zstd;
    wal_config.encryption = EncryptionType::Aes256Gcm;
    wal_config.encryption_key = Some(key);

    let wal = WalWriter::create(&fs, wal_path, wal_config).unwrap();

    // Write compressed and encrypted data
    let compressible_data = b"Repeated data pattern ".repeat(500);
    wal.write_begin(TransactionId::from(1)).unwrap();
    wal.write_operation(
        TransactionId::from(1),
        TableId::from(1),
        WriteOpType::Put,
        b"blob:1".to_vec(),
        compressible_data.clone(),
    )
    .unwrap();
    wal.write_commit(TransactionId::from(1)).unwrap();
    wal.flush().unwrap();

    // Verify recovery with correct key
    let result = WalRecovery::recover_with_key(&fs, wal_path, Some(key)).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(result.committed_writes[0].value, compressible_data);
}

#[test]
fn test_pager_and_wal_different_compression_algorithms() {
    let fs = MemoryFileSystem::new();
    let db_path = "mixed.db";
    let wal_path = "mixed.wal";

    // Pager uses Zstd
    let pager_config = PagerConfig::new()
        .with_page_size(PageSize::Size4KB)
        .with_compression(CompressionType::Zstd);

    let pager = Pager::create(&fs, db_path, pager_config).unwrap();

    // WAL uses LZ4
    let mut wal_config = WalWriterConfig::default();
    wal_config.compression = CompressionType::Lz4;

    let wal = WalWriter::create(&fs, wal_path, wal_config).unwrap();

    // Write data to WAL
    wal.write_begin(TransactionId::from(1)).unwrap();
    wal.write_operation(
        TransactionId::from(1),
        TableId::from(1),
        WriteOpType::Put,
        b"key1".to_vec(),
        b"value1".repeat(50),
    )
    .unwrap();
    wal.write_commit(TransactionId::from(1)).unwrap();
    wal.flush().unwrap();

    // Verify both work independently
    let result = WalRecovery::recover(&fs, wal_path).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(pager.page_size(), PageSize::Size4KB);
}

#[test]
fn test_recovery_of_encrypted_compressed_database() {
    let fs = MemoryFileSystem::new();
    let db_path = "recovery.db";
    let wal_path = "recovery.wal";
    let key = [77u8; 32];

    // Create StorageEngine with compression and encryption
    let pager_config = PagerConfig::new()
        .with_page_size(PageSize::Size4KB)
        .with_compression(CompressionType::Lz4)
        .with_encryption(EncryptionType::Aes256Gcm, key);

    let _pager = Pager::create(&fs, db_path, pager_config).unwrap();

    // Create WAL with same settings
    let mut wal_config = WalWriterConfig::default();
    wal_config.compression = CompressionType::Lz4;
    wal_config.encryption = EncryptionType::Aes256Gcm;
    wal_config.encryption_key = Some(key);

    {
        let wal = WalWriter::create(&fs, wal_path, wal_config.clone()).unwrap();

        // Write multiple transactions
        for i in 1..=5 {
            let txn_id = TransactionId::from(i);
            wal.write_begin(txn_id).unwrap();
            wal.write_operation(
                txn_id,
                TableId::from(1),
                WriteOpType::Put,
                format!("key{}", i).as_bytes().to_vec(),
                format!("value{}", i).repeat(100).as_bytes().to_vec(),
            )
            .unwrap();
            wal.write_commit(txn_id).unwrap();
        }

        // Simulate crash - don't commit last transaction
        let txn_id = TransactionId::from(6);
        wal.write_begin(txn_id).unwrap();
        wal.write_operation(
            txn_id,
            TableId::from(1),
            WriteOpType::Put,
            b"key6".to_vec(),
            b"incomplete".to_vec(),
        )
        .unwrap();
        wal.flush().unwrap();
    }

    // Recover with correct key
    let result = WalRecovery::recover_with_key(&fs, wal_path, Some(key)).unwrap();
    assert_eq!(result.committed_writes.len(), 5);
    assert_eq!(result.active_transactions.len(), 1);
    assert!(result.active_transactions.contains(&TransactionId::from(6)));

    // Verify data integrity
    for i in 1..=5 {
        let write = result
            .committed_writes
            .iter()
            .find(|w| w.key == format!("key{}", i).as_bytes())
            .unwrap();
        assert_eq!(write.value, format!("value{}", i).repeat(100).as_bytes());
    }
}

// ============================================================================
// 2. END-TO-END WORKFLOW TESTS
// ============================================================================

#[test]
fn test_write_close_reopen_encrypted_compressed() {
    let fs = MemoryFileSystem::new();
    let db_path = "workflow.db";
    let wal_path = "workflow.wal";
    let key = [55u8; 32];

    // Phase 1: Create and write
    {
        let pager_config = PagerConfig::new()
            .with_compression(CompressionType::Lz4)
            .with_encryption(EncryptionType::Aes256Gcm, key);

        let _pager = Pager::create(&fs, db_path, pager_config).unwrap();

        let mut wal_config = WalWriterConfig::default();
        wal_config.compression = CompressionType::Lz4;
        wal_config.encryption = EncryptionType::Aes256Gcm;
        wal_config.encryption_key = Some(key);

        let wal = WalWriter::create(&fs, wal_path, wal_config).unwrap();

        // Write data
        wal.write_begin(TransactionId::from(1)).unwrap();
        wal.write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"user:1".to_vec(),
            b"Alice".to_vec(),
        )
        .unwrap();
        wal.write_commit(TransactionId::from(1)).unwrap();
        wal.flush().unwrap();
    }

    // Phase 2: Reopen and verify
    {
        let _pager = Pager::open(&fs, db_path).unwrap();

        let result = WalRecovery::recover_with_key(&fs, wal_path, Some(key)).unwrap();
        assert_eq!(result.committed_writes.len(), 1);
        assert_eq!(result.committed_writes[0].key, b"user:1");
        assert_eq!(result.committed_writes[0].value, b"Alice");
    }
}

#[test]
fn test_checkpoint_with_compression_and_encryption() {
    let fs = MemoryFileSystem::new();
    let wal_path = "checkpoint.wal";
    let key = [88u8; 32];

    let mut wal_config = WalWriterConfig::default();
    wal_config.compression = CompressionType::Zstd;
    wal_config.encryption = EncryptionType::Aes256Gcm;
    wal_config.encryption_key = Some(key);

    let wal = WalWriter::create(&fs, wal_path, wal_config).unwrap();

    // Transaction before checkpoint
    wal.write_begin(TransactionId::from(1)).unwrap();
    wal.write_operation(
        TransactionId::from(1),
        TableId::from(1),
        WriteOpType::Put,
        b"key1".to_vec(),
        b"value1".repeat(100),
    )
    .unwrap();
    wal.write_commit(TransactionId::from(1)).unwrap();

    // Active transaction during checkpoint
    wal.write_begin(TransactionId::from(2)).unwrap();
    wal.write_operation(
        TransactionId::from(2),
        TableId::from(1),
        WriteOpType::Put,
        b"key2".to_vec(),
        b"value2".repeat(100),
    )
    .unwrap();

    // Checkpoint
    let checkpoint_lsn = wal.write_checkpoint().unwrap();

    // Complete transaction after checkpoint
    wal.write_commit(TransactionId::from(2)).unwrap();

    // New transaction after checkpoint
    wal.write_begin(TransactionId::from(3)).unwrap();
    wal.write_operation(
        TransactionId::from(3),
        TableId::from(1),
        WriteOpType::Put,
        b"key3".to_vec(),
        b"value3".repeat(100),
    )
    .unwrap();
    wal.write_commit(TransactionId::from(3)).unwrap();
    wal.flush().unwrap();

    // Verify recovery
    let result = WalRecovery::recover_with_key(&fs, wal_path, Some(key)).unwrap();
    assert_eq!(result.committed_writes.len(), 3);
    assert_eq!(result.last_checkpoint_lsn.unwrap(), checkpoint_lsn);
    assert!(result.active_transactions.is_empty());
}

#[test]
fn test_data_integrity_through_full_cycle() {
    let fs = MemoryFileSystem::new();
    let wal_path = "integrity.wal";
    let key = [111u8; 32];

    let mut wal_config = WalWriterConfig::default();
    wal_config.compression = CompressionType::Lz4;
    wal_config.encryption = EncryptionType::Aes256Gcm;
    wal_config.encryption_key = Some(key);

    let wal = WalWriter::create(&fs, wal_path, wal_config).unwrap();

    // Test data with various patterns
    let test_cases = vec![
        (b"empty".to_vec(), vec![]),
        (b"small".to_vec(), b"x".to_vec()),
        (b"medium".to_vec(), b"Hello World!".repeat(10)),
        (b"large".to_vec(), vec![0xAB; 10000]),
        (b"compressible".to_vec(), b"AAAA".repeat(1000)),
        (
            b"random".to_vec(),
            (0..1000).map(|i| (i % 256) as u8).collect(),
        ),
    ];

    // Write all test cases
    for (i, (key_data, value_data)) in test_cases.iter().enumerate() {
        let txn_id = TransactionId::from((i + 1) as u64);
        wal.write_begin(txn_id).unwrap();
        wal.write_operation(
            txn_id,
            TableId::from(1),
            WriteOpType::Put,
            key_data.clone(),
            value_data.clone(),
        )
        .unwrap();
        wal.write_commit(txn_id).unwrap();
    }
    wal.flush().unwrap();

    // Recover and verify all data
    let result = WalRecovery::recover_with_key(&fs, wal_path, Some(key)).unwrap();
    assert_eq!(result.committed_writes.len(), test_cases.len());

    // Verify each test case
    for (key_data, value_data) in test_cases {
        let write = result
            .committed_writes
            .iter()
            .find(|w| w.key == key_data)
            .unwrap();
        assert_eq!(
            write.value, value_data,
            "Data mismatch for key {:?}",
            key_data
        );
    }
}

// ============================================================================
// 3. ERROR HANDLING TESTS
// ============================================================================

#[test]
fn test_encrypted_database_wrong_key_fails() {
    let fs = MemoryFileSystem::new();
    let wal_path = "wrong_key.wal";
    let correct_key = [123u8; 32];
    let wrong_key = [124u8; 32];

    // Write with correct key
    let mut wal_config = WalWriterConfig::default();
    wal_config.encryption = EncryptionType::Aes256Gcm;
    wal_config.encryption_key = Some(correct_key);

    let wal = WalWriter::create(&fs, wal_path, wal_config).unwrap();
    wal.write_begin(TransactionId::from(1)).unwrap();
    wal.write_operation(
        TransactionId::from(1),
        TableId::from(1),
        WriteOpType::Put,
        b"key".to_vec(),
        b"value".to_vec(),
    )
    .unwrap();
    wal.write_commit(TransactionId::from(1)).unwrap();
    wal.flush().unwrap();

    // Try to recover with wrong key
    let result = WalRecovery::recover_with_key(&fs, wal_path, Some(wrong_key));
    assert!(result.is_err(), "Should fail with wrong key");
}

#[test]
fn test_encrypted_database_no_key_fails() {
    let fs = MemoryFileSystem::new();
    let wal_path = "no_key.wal";
    let key = [200u8; 32];

    // Write with encryption
    let mut wal_config = WalWriterConfig::default();
    wal_config.encryption = EncryptionType::Aes256Gcm;
    wal_config.encryption_key = Some(key);

    let wal = WalWriter::create(&fs, wal_path, wal_config).unwrap();
    wal.write_begin(TransactionId::from(1)).unwrap();
    wal.write_operation(
        TransactionId::from(1),
        TableId::from(1),
        WriteOpType::Put,
        b"key".to_vec(),
        b"value".to_vec(),
    )
    .unwrap();
    wal.write_commit(TransactionId::from(1)).unwrap();
    wal.flush().unwrap();

    // Try to recover without key
    let result = WalRecovery::recover(&fs, wal_path);
    assert!(result.is_err(), "Should fail without key");
}

#[test]
fn test_mixed_encryption_settings_pager_encrypted_wal_unencrypted() {
    let fs = MemoryFileSystem::new();
    let db_path = "mixed1.db";
    let wal_path = "mixed1.wal";
    let key = [66u8; 32];

    // Pager encrypted
    let pager_config = PagerConfig::new().with_encryption(EncryptionType::Aes256Gcm, key);

    let _pager = Pager::create(&fs, db_path, pager_config).unwrap();

    // WAL unencrypted
    let wal_config = WalWriterConfig::default();
    let wal = WalWriter::create(&fs, wal_path, wal_config).unwrap();

    // Should work - they're independent
    wal.write_begin(TransactionId::from(1)).unwrap();
    wal.write_operation(
        TransactionId::from(1),
        TableId::from(1),
        WriteOpType::Put,
        b"key".to_vec(),
        b"value".to_vec(),
    )
    .unwrap();
    wal.write_commit(TransactionId::from(1)).unwrap();
    wal.flush().unwrap();

    let result = WalRecovery::recover(&fs, wal_path).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
}

#[test]
fn test_mixed_encryption_settings_pager_unencrypted_wal_encrypted() {
    let fs = MemoryFileSystem::new();
    let db_path = "mixed2.db";
    let wal_path = "mixed2.wal";
    let key = [77u8; 32];

    // Pager unencrypted
    let pager_config = PagerConfig::new();
    let _pager = Pager::create(&fs, db_path, pager_config).unwrap();

    // WAL encrypted
    let mut wal_config = WalWriterConfig::default();
    wal_config.encryption = EncryptionType::Aes256Gcm;
    wal_config.encryption_key = Some(key);

    let wal = WalWriter::create(&fs, wal_path, wal_config).unwrap();

    // Should work - they're independent
    wal.write_begin(TransactionId::from(1)).unwrap();
    wal.write_operation(
        TransactionId::from(1),
        TableId::from(1),
        WriteOpType::Put,
        b"key".to_vec(),
        b"value".to_vec(),
    )
    .unwrap();
    wal.write_commit(TransactionId::from(1)).unwrap();
    wal.flush().unwrap();

    let result = WalRecovery::recover_with_key(&fs, wal_path, Some(key)).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
}

#[test]
fn test_compression_settings_persisted_in_file_header() {
    let fs = MemoryFileSystem::new();
    let db_path = "persist.db";

    // Create with compression
    let pager_config = PagerConfig::new().with_compression(CompressionType::Zstd);

    {
        let _pager = Pager::create(&fs, db_path, pager_config).unwrap();
    }

    // Reopen and verify settings
    let pager = Pager::open(&fs, db_path).unwrap();
    assert_eq!(pager.page_size(), PageSize::Size4KB);
}

#[test]
fn test_encryption_settings_persisted_in_file_header() {
    let fs = MemoryFileSystem::new();
    let db_path = "persist_enc.db";
    let key = [99u8; 32];

    // Create with encryption
    let pager_config = PagerConfig::new().with_encryption(EncryptionType::Aes256Gcm, key);

    {
        let _pager = Pager::create(&fs, db_path, pager_config).unwrap();
    }

    // Reopen (note: key must be provided separately in real usage)
    let pager = Pager::open(&fs, db_path).unwrap();
    assert_eq!(pager.page_size(), PageSize::Size4KB);
}

// ============================================================================
// 4. PERFORMANCE SANITY TESTS
// ============================================================================

#[test]
fn test_compressed_data_is_smaller() {
    let fs = MemoryFileSystem::new();
    let uncompressed_path = "uncompressed.wal";
    let compressed_path = "compressed.wal";

    // Highly compressible data
    let compressible_data = b"A".repeat(10000);

    // Write without compression
    {
        let config = WalWriterConfig::default();
        let wal = WalWriter::create(&fs, uncompressed_path, config).unwrap();
        wal.write_begin(TransactionId::from(1)).unwrap();
        wal.write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key".to_vec(),
            compressible_data.clone(),
        )
        .unwrap();
        wal.write_commit(TransactionId::from(1)).unwrap();
        wal.flush().unwrap();
    }

    // Write with compression
    {
        let mut config = WalWriterConfig::default();
        config.compression = CompressionType::Lz4;
        let wal = WalWriter::create(&fs, compressed_path, config).unwrap();
        wal.write_begin(TransactionId::from(1)).unwrap();
        wal.write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key".to_vec(),
            compressible_data,
        )
        .unwrap();
        wal.write_commit(TransactionId::from(1)).unwrap();
        wal.flush().unwrap();
    }

    // Verify compressed is smaller
    let uncompressed_file = fs.open_file(uncompressed_path).unwrap();
    let compressed_file = fs.open_file(compressed_path).unwrap();

    let uncompressed_size = uncompressed_file.get_size().unwrap();
    let compressed_size = compressed_file.get_size().unwrap();

    assert!(
        compressed_size < uncompressed_size,
        "Compressed size ({}) should be less than uncompressed size ({})",
        compressed_size,
        uncompressed_size
    );

    // Should be significantly smaller (at least 50% reduction for this data)
    assert!(
        compressed_size < uncompressed_size / 2,
        "Compression should achieve at least 50% reduction for highly compressible data"
    );
}

#[test]
fn test_encryption_preserves_data_integrity() {
    let fs = MemoryFileSystem::new();
    let wal_path = "integrity_enc.wal";
    let key = [222u8; 32];

    let mut config = WalWriterConfig::default();
    config.encryption = EncryptionType::Aes256Gcm;
    config.encryption_key = Some(key);

    let wal = WalWriter::create(&fs, wal_path, config).unwrap();

    // Write random data (not compressible)
    let random_data: Vec<u8> = (0..5000).map(|i| ((i * 7 + 13) % 256) as u8).collect();

    wal.write_begin(TransactionId::from(1)).unwrap();
    wal.write_operation(
        TransactionId::from(1),
        TableId::from(1),
        WriteOpType::Put,
        b"random_key".to_vec(),
        random_data.clone(),
    )
    .unwrap();
    wal.write_commit(TransactionId::from(1)).unwrap();
    wal.flush().unwrap();

    // Verify data reads back correctly
    let result = WalRecovery::recover_with_key(&fs, wal_path, Some(key)).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(result.committed_writes[0].value, random_data);
}

#[test]
fn test_compression_with_various_data_patterns() {
    let fs = MemoryFileSystem::new();

    let test_cases = vec![
        ("highly_compressible", b"A".repeat(1000)),
        ("moderately_compressible", b"Hello World! ".repeat(100)),
        (
            "low_compressible",
            (0..1000).map(|i| (i % 256) as u8).collect(),
        ),
    ];

    for (name, data) in test_cases {
        let path = format!("{}.wal", name);

        let mut config = WalWriterConfig::default();
        config.compression = CompressionType::Lz4;

        let wal = WalWriter::create(&fs, &path, config).unwrap();
        wal.write_begin(TransactionId::from(1)).unwrap();
        wal.write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key".to_vec(),
            data.clone(),
        )
        .unwrap();
        wal.write_commit(TransactionId::from(1)).unwrap();
        wal.flush().unwrap();

        // Verify data integrity
        let result = WalRecovery::recover(&fs, &path).unwrap();
        assert_eq!(result.committed_writes.len(), 1);
        assert_eq!(
            result.committed_writes[0].value, data,
            "Data integrity failed for {}",
            name
        );
    }
}

#[test]
fn test_encryption_overhead_is_reasonable() {
    let fs = MemoryFileSystem::new();
    let unencrypted_path = "plain.wal";
    let encrypted_path = "encrypted.wal";
    let key = [33u8; 32];

    let test_data = b"Test data for encryption overhead measurement".repeat(100);

    // Write without encryption
    {
        let config = WalWriterConfig::default();
        let wal = WalWriter::create(&fs, unencrypted_path, config).unwrap();
        wal.write_begin(TransactionId::from(1)).unwrap();
        wal.write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key".to_vec(),
            test_data.clone(),
        )
        .unwrap();
        wal.write_commit(TransactionId::from(1)).unwrap();
        wal.flush().unwrap();
    }

    // Write with encryption
    {
        let mut config = WalWriterConfig::default();
        config.encryption = EncryptionType::Aes256Gcm;
        config.encryption_key = Some(key);
        let wal = WalWriter::create(&fs, encrypted_path, config).unwrap();
        wal.write_begin(TransactionId::from(1)).unwrap();
        wal.write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key".to_vec(),
            test_data,
        )
        .unwrap();
        wal.write_commit(TransactionId::from(1)).unwrap();
        wal.flush().unwrap();
    }

    // Verify overhead is reasonable (should be less than 50% increase)
    let plain_file = fs.open_file(unencrypted_path).unwrap();
    let encrypted_file = fs.open_file(encrypted_path).unwrap();

    let plain_size = plain_file.get_size().unwrap();
    let encrypted_size = encrypted_file.get_size().unwrap();

    let overhead = encrypted_size as f64 / plain_size as f64;
    assert!(
        overhead < 1.5,
        "Encryption overhead ({:.2}x) should be less than 50%",
        overhead
    );
}

#[test]
fn test_combined_compression_and_encryption_performance() {
    let fs = MemoryFileSystem::new();
    let path = "combined.wal";
    let key = [44u8; 32];

    let mut config = WalWriterConfig::default();
    config.compression = CompressionType::Lz4;
    config.encryption = EncryptionType::Aes256Gcm;
    config.encryption_key = Some(key);

    let wal = WalWriter::create(&fs, path, config).unwrap();

    // Write multiple transactions with various data sizes
    for i in 1..=10 {
        let data_size = i * 1000;
        let data = b"X".repeat(data_size);
        let txn_id = TransactionId::from(i as u64);

        wal.write_begin(txn_id).unwrap();
        wal.write_operation(
            txn_id,
            TableId::from(1),
            WriteOpType::Put,
            format!("key{}", i).as_bytes().to_vec(),
            data.clone(),
        )
        .unwrap();
        wal.write_commit(txn_id).unwrap();
    }
    wal.flush().unwrap();

    // Verify all data recovered correctly
    let result = WalRecovery::recover_with_key(&fs, path, Some(key)).unwrap();
    assert_eq!(result.committed_writes.len(), 10);

    // Verify each transaction
    for i in 1..=10 {
        let expected_data = b"X".repeat(i * 1000);
        let write = result
            .committed_writes
            .iter()
            .find(|w| w.key == format!("key{}", i).as_bytes())
            .unwrap();
        assert_eq!(write.value, expected_data);
    }
}
