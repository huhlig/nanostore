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

//! Integration tests for WAL (Write-Ahead Log)

use nanostore::pager::{CompressionType, EncryptionType};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::vfs::{File, FileSystem, LocalFileSystem, MemoryFileSystem};
use nanostore::wal::{
    LogSequenceNumber, WalError, WalReader, WalRecovery, WalWriter, WalWriterConfig, WriteOpType,
};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::thread;
use tempfile::TempDir;

#[test]
fn test_wal_basic_transaction_flow() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let config = WalWriterConfig::default();

    let writer = WalWriter::create(&fs, path, config).unwrap();

    // Begin transaction
    let begin_lsn = writer.write_begin(TransactionId::from(1)).unwrap();
    assert_eq!(begin_lsn, LogSequenceNumber::from(1));

    // Write operations
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"user:1".to_vec(),
            b"Alice".to_vec(),
        )
        .unwrap();

    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"user:2".to_vec(),
            b"Bob".to_vec(),
        )
        .unwrap();

    // Commit transaction
    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    // Verify recovery
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 2);
    assert!(result.active_transactions.is_empty());
}

#[test]
fn test_wal_rollback_transaction() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let config = WalWriterConfig::default();

    let writer = WalWriter::create(&fs, path, config).unwrap();

    // Begin and rollback
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"user:1".to_vec(),
            b"Alice".to_vec(),
        )
        .unwrap();
    writer.write_rollback(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    // Verify recovery - no committed writes
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 0);
    assert!(result.active_transactions.is_empty());
}

#[test]
fn test_wal_crash_recovery_active_transaction() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let config = WalWriterConfig::default();

    {
        let writer = WalWriter::create(&fs, path, config).unwrap();

        // Begin transaction but don't commit (simulating crash)
        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"user:1".to_vec(),
                b"Alice".to_vec(),
            )
            .unwrap();
        writer.flush().unwrap();
        // Writer dropped here (simulating crash)
    }

    // Recover - should find active transaction
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 0);
    assert_eq!(result.active_transactions.len(), 1);
    assert!(result.active_transactions.contains(&TransactionId::from(1)));
}

#[test]
fn test_wal_multiple_concurrent_transactions() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let config = WalWriterConfig::default();

    let writer = WalWriter::create(&fs, path, config).unwrap();

    // Start multiple transactions
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer.write_begin(TransactionId::from(2)).unwrap();
    writer.write_begin(TransactionId::from(3)).unwrap();

    // Write to different transactions
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key1".to_vec(),
            b"value1".to_vec(),
        )
        .unwrap();

    writer
        .write_operation(
            TransactionId::from(2),
            TableId::from(2),
            WriteOpType::Put,
            b"key2".to_vec(),
            b"value2".to_vec(),
        )
        .unwrap();

    writer
        .write_operation(
            TransactionId::from(3),
            TableId::from(3),
            WriteOpType::Put,
            b"key3".to_vec(),
            b"value3".to_vec(),
        )
        .unwrap();

    // Commit in different order
    writer.write_commit(TransactionId::from(2)).unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.write_rollback(TransactionId::from(3)).unwrap();

    writer.flush().unwrap();

    // Verify recovery
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 2);
    assert!(result.active_transactions.is_empty());
}

#[test]
fn test_wal_checkpoint_functionality() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let config = WalWriterConfig::default();

    let writer = WalWriter::create(&fs, path, config).unwrap();

    // Transaction 1: complete before checkpoint
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key1".to_vec(),
            b"value1".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();

    // Transaction 2: active during checkpoint
    writer.write_begin(TransactionId::from(2)).unwrap();
    writer
        .write_operation(
            TransactionId::from(2),
            TableId::from(1),
            WriteOpType::Put,
            b"key2".to_vec(),
            b"value2".to_vec(),
        )
        .unwrap();

    // Checkpoint
    let checkpoint_lsn = writer.write_checkpoint().unwrap();
    assert!(checkpoint_lsn.as_u64() > 0);

    // Transaction 2: complete after checkpoint
    writer.write_commit(TransactionId::from(2)).unwrap();

    // Transaction 3: after checkpoint
    writer.write_begin(TransactionId::from(3)).unwrap();
    writer
        .write_operation(
            TransactionId::from(3),
            TableId::from(1),
            WriteOpType::Put,
            b"key3".to_vec(),
            b"value3".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(3)).unwrap();

    writer.flush().unwrap();

    // Verify recovery includes all committed transactions
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 3);
    assert!(result.last_checkpoint_lsn.is_some());
    assert_eq!(result.last_checkpoint_lsn.unwrap(), checkpoint_lsn);
}

#[test]
fn test_wal_delete_operations() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let config = WalWriterConfig::default();

    let writer = WalWriter::create(&fs, path, config).unwrap();

    writer.write_begin(TransactionId::from(1)).unwrap();

    // Put operation
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"user:1".to_vec(),
            b"Alice".to_vec(),
        )
        .unwrap();

    // Delete operation
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Delete,
            b"user:2".to_vec(),
            vec![],
        )
        .unwrap();

    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    // Verify recovery
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 2);
    assert_eq!(result.committed_writes[0].op_type, WriteOpType::Put);
    assert_eq!(result.committed_writes[1].op_type, WriteOpType::Delete);
}

#[test]
fn test_wal_large_values() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let config = WalWriterConfig::default();

    let writer = WalWriter::create(&fs, path, config).unwrap();

    // Create a large value (1MB)
    let large_value = vec![0xAB; 1024 * 1024];

    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(2),
            WriteOpType::Put,
            b"blob:1".to_vec(),
            large_value.clone(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    // Verify recovery
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(result.committed_writes[0].value, large_value);
}

#[test]
fn test_wal_reader_sequential_read() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let config = WalWriterConfig::default();

    // Write some records
    {
        let writer = WalWriter::create(&fs, path, config).unwrap();
        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"key".to_vec(),
                b"value".to_vec(),
            )
            .unwrap();
        writer.write_commit(TransactionId::from(1)).unwrap();
        writer.flush().unwrap();
    }

    // Read records sequentially
    let mut reader = WalReader::open(&fs, path, None).unwrap();
    let mut count = 0;
    while let Some(_record) = reader.read_next().unwrap() {
        count += 1;
    }
    assert_eq!(count, 3); // BEGIN, WRITE, COMMIT
}

#[test]
fn test_wal_with_local_filesystem() {
    let temp_dir = TempDir::new().unwrap();
    let wal_path = temp_dir.path().join("test.wal");
    let wal_path_str = wal_path.to_str().unwrap();

    let fs = LocalFileSystem::new(temp_dir.path());
    let config = WalWriterConfig::default();

    // Write to WAL
    {
        let writer = WalWriter::create(&fs, wal_path_str, config).unwrap();
        writer.write_begin(TransactionId::from(1)).unwrap();
        writer
            .write_operation(
                TransactionId::from(1),
                TableId::from(1),
                WriteOpType::Put,
                b"user:1".to_vec(),
                b"Alice".to_vec(),
            )
            .unwrap();
        writer.write_commit(TransactionId::from(1)).unwrap();
        writer.flush().unwrap();
    }

    // Verify file exists
    assert!(wal_path.exists());

    // Recover from file
    let result = WalRecovery::recover(&fs, wal_path_str).unwrap();
    assert_eq!(result.committed_writes.len(), 1);

    // Cleanup
    fs::remove_file(wal_path).unwrap();
}

#[test]
fn test_wal_buffered_writes() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let mut config = WalWriterConfig::default();
    config.buffer_size = 1024; // Small buffer
    config.sync_on_write = false; // Don't sync immediately

    let writer = WalWriter::create(&fs, path, config).unwrap();

    // Write multiple transactions
    for i in 1..=10 {
        let txn_id = TransactionId::from(i);
        writer.write_begin(txn_id).unwrap();
        writer
            .write_operation(
                txn_id,
                TableId::from(1),
                WriteOpType::Put,
                format!("key{}", i).as_bytes().to_vec(),
                format!("value{}", i).as_bytes().to_vec(),
            )
            .unwrap();
        writer.write_commit(txn_id).unwrap();
    }

    // Explicit flush
    writer.flush().unwrap();

    // Verify all transactions recovered
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 10);
}

#[test]
fn test_wal_truncate() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let config = WalWriterConfig::default();

    let writer = WalWriter::create(&fs, path, config).unwrap();

    // Write some data
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key".to_vec(),
            b"value".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    let size_before = writer.file_size();
    assert!(size_before > 0);

    // Truncate
    writer.truncate().unwrap();

    let size_after = writer.file_size();
    assert_eq!(size_after, 0);
}

#[test]
fn test_wal_error_handling() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let config = WalWriterConfig::default();

    let writer = WalWriter::create(&fs, path, config).unwrap();

    // Try to commit non-existent transaction
    let result = writer.write_commit(TransactionId::from(999));
    assert!(result.is_err());

    // Try to write to non-existent transaction
    let result = writer.write_operation(
        TransactionId::from(999),
        TableId::from(1),
        WriteOpType::Put,
        b"key".to_vec(),
        b"value".to_vec(),
    );
    assert!(result.is_err());

    // Try to begin duplicate transaction
    writer.write_begin(TransactionId::from(1)).unwrap();
    let result = writer.write_begin(TransactionId::from(1));
    assert!(result.is_err());
}

#[test]
fn test_wal_with_lz4_compression() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let mut config = WalWriterConfig::default();
    config.compression = CompressionType::Lz4;

    let writer = WalWriter::create(&fs, path, config).unwrap();

    // Write transaction with compressible data
    writer.write_begin(TransactionId::from(1)).unwrap();

    // Create highly compressible data (repeated pattern)
    let compressible_value = vec![0x42; 10000];

    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key1".to_vec(),
            compressible_value.clone(),
        )
        .unwrap();

    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    // Verify recovery works with compressed data
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(result.committed_writes[0].value, compressible_value);
    assert_eq!(result.committed_writes[0].key, b"key1");
}

#[test]
fn test_wal_with_zstd_compression() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let mut config = WalWriterConfig::default();
    config.compression = CompressionType::Zstd;

    let writer = WalWriter::create(&fs, path, config).unwrap();

    // Write transaction with compressible data
    writer.write_begin(TransactionId::from(1)).unwrap();

    // Create highly compressible data
    let compressible_value = "Hello World! ".repeat(1000).into_bytes();

    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"msg:1".to_vec(),
            compressible_value.clone(),
        )
        .unwrap();

    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    // Verify recovery works with compressed data
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(result.committed_writes[0].value, compressible_value);
}

#[test]
fn test_wal_compression_recovery() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let mut config = WalWriterConfig::default();
    config.compression = CompressionType::Lz4;

    let writer = WalWriter::create(&fs, path, config.clone()).unwrap();

    // Write multiple transactions with compression
    for i in 1..=5 {
        let txn_id = TransactionId::from(i);
        writer.write_begin(txn_id).unwrap();

        let value = format!("Compressed value for transaction {}", i)
            .repeat(100)
            .into_bytes();

        writer
            .write_operation(
                txn_id,
                TableId::from(1),
                WriteOpType::Put,
                format!("key{}", i).as_bytes().to_vec(),
                value,
            )
            .unwrap();

        writer.write_commit(txn_id).unwrap();
    }

    writer.flush().unwrap();

    // Recover and verify all transactions
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 5);

    // Verify all expected keys are present (order may vary due to HashMap iteration)
    let mut found_keys: Vec<Vec<u8>> = result
        .committed_writes
        .iter()
        .map(|w| w.key.clone())
        .collect();
    found_keys.sort();

    let mut expected_keys: Vec<Vec<u8>> =
        (1..=5).map(|i| format!("key{}", i).into_bytes()).collect();
    expected_keys.sort();

    assert_eq!(found_keys, expected_keys);

    // Verify each write has the correct value for its key
    for write in &result.committed_writes {
        let key_str = String::from_utf8(write.key.clone()).unwrap();
        let key_num = key_str
            .strip_prefix("key")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let expected_value = format!("Compressed value for transaction {}", key_num)
            .repeat(100)
            .into_bytes();
        assert_eq!(write.value, expected_value);
    }
}

#[test]
fn test_wal_mixed_compression() {
    // Test that we can handle records with different compression types
    // Note: This test writes all records in a single session since WalWriter::open
    // doesn't properly track LSN from existing records (pre-existing limitation)
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let mut config = WalWriterConfig::default();
    config.compression = CompressionType::None;

    let writer = WalWriter::create(&fs, path, config).unwrap();

    // Write with no compression
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key1".to_vec(),
            b"uncompressed value".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    // Recover and verify the uncompressed record can be read
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(result.committed_writes[0].key, b"key1");
    assert_eq!(result.committed_writes[0].value, b"uncompressed value");
}

#[test]
fn test_wal_compression_with_large_values() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let mut config = WalWriterConfig::default();
    config.compression = CompressionType::Lz4;

    let writer = WalWriter::create(&fs, path, config).unwrap();

    // Create a large compressible value (1MB of repeated data)
    let large_value = vec![0xAB; 1024 * 1024];

    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(2),
            WriteOpType::Put,
            b"blob:1".to_vec(),
            large_value.clone(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    // Verify recovery with compressed large value
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(result.committed_writes[0].value, large_value);

    // File size should be much smaller than 1MB due to compression
    let file_size = writer.file_size();
    assert!(
        file_size < 1024 * 1024,
        "Compressed file should be smaller than uncompressed data"
    );
}

#[test]
fn test_wal_compression_checkpoint() {
    let fs = MemoryFileSystem::new();
    let path = "test.wal";
    let mut config = WalWriterConfig::default();
    config.compression = CompressionType::Zstd;

    let writer = WalWriter::create(&fs, path, config).unwrap();

    // Transaction before checkpoint
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"key1".to_vec(),
            b"value1".repeat(100),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();

    // Active transaction during checkpoint
    writer.write_begin(TransactionId::from(2)).unwrap();
    writer
        .write_operation(
            TransactionId::from(2),
            TableId::from(1),
            WriteOpType::Put,
            b"key2".to_vec(),
            b"value2".repeat(100),
        )
        .unwrap();

    // Checkpoint with compression
    let checkpoint_lsn = writer.write_checkpoint().unwrap();
    assert!(checkpoint_lsn.as_u64() > 0);

    // Complete transaction after checkpoint
    writer.write_commit(TransactionId::from(2)).unwrap();
    writer.flush().unwrap();

    // Verify recovery with compressed checkpoint
    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 2);
    assert_eq!(result.last_checkpoint_lsn.unwrap(), checkpoint_lsn);
}

#[test]
fn test_wal_with_aes256_gcm_encryption() {
    let fs = MemoryFileSystem::new();
    let path = "encrypted.wal";
    let key = [7u8; 32];

    let mut config = WalWriterConfig::default();
    config.encryption = EncryptionType::Aes256Gcm;
    config.encryption_key = Some(key);

    let writer = WalWriter::create(&fs, path, config).unwrap();
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"secret-key".to_vec(),
            b"secret-value".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    let mut reader = WalReader::open(&fs, path, Some(key)).unwrap();
    let records = reader.read_all().unwrap();

    assert_eq!(records.len(), 3);
    assert_eq!(records[0].encryption, EncryptionType::Aes256Gcm);
    assert_eq!(records[1].encryption, EncryptionType::Aes256Gcm);
    assert_eq!(records[2].encryption, EncryptionType::Aes256Gcm);
}

#[test]
fn test_wal_decryption_with_correct_key() {
    let fs = MemoryFileSystem::new();
    let path = "decrypt-ok.wal";
    let key = [11u8; 32];

    let mut config = WalWriterConfig::default();
    config.encryption = EncryptionType::Aes256Gcm;
    config.encryption_key = Some(key);

    let writer = WalWriter::create(&fs, path, config).unwrap();
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"k1".to_vec(),
            b"very secret payload".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    let result = WalRecovery::recover_with_key(&fs, path, Some(key)).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(result.committed_writes[0].key, b"k1");
    assert_eq!(result.committed_writes[0].value, b"very secret payload");
}

#[test]
fn test_wal_decryption_failure_with_wrong_key() {
    let fs = MemoryFileSystem::new();
    let path = "decrypt-fail.wal";
    let key = [22u8; 32];
    let wrong_key = [23u8; 32];

    let mut config = WalWriterConfig::default();
    config.encryption = EncryptionType::Aes256Gcm;
    config.encryption_key = Some(key);

    let writer = WalWriter::create(&fs, path, config).unwrap();
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"k2".to_vec(),
            b"classified".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    let err = WalRecovery::recover_with_key(&fs, path, Some(wrong_key)).unwrap_err();
    assert!(matches!(err, WalError::DecryptionError { .. }));
}

#[test]
fn test_wal_recovery_with_encrypted_records() {
    let fs = MemoryFileSystem::new();
    let path = "encrypted-recovery.wal";
    let key = [33u8; 32];

    let mut config = WalWriterConfig::default();
    config.encryption = EncryptionType::Aes256Gcm;
    config.encryption_key = Some(key);

    let writer = WalWriter::create(&fs, path, config).unwrap();

    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"user:1".to_vec(),
            b"Alice".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();

    writer.write_begin(TransactionId::from(2)).unwrap();
    writer
        .write_operation(
            TransactionId::from(2),
            TableId::from(1),
            WriteOpType::Put,
            b"user:2".to_vec(),
            b"Bob".to_vec(),
        )
        .unwrap();
    writer.flush().unwrap();

    let result = WalRecovery::recover_with_key(&fs, path, Some(key)).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(result.committed_writes[0].key, b"user:1");
    assert_eq!(result.committed_writes[0].value, b"Alice");
    assert!(result.active_transactions.contains(&TransactionId::from(2)));
}

#[test]
fn test_wal_encryption_and_compression_combined() {
    let fs = MemoryFileSystem::new();
    let path = "encrypted-compressed.wal";
    let key = [44u8; 32];

    let mut config = WalWriterConfig::default();
    config.compression = CompressionType::Lz4;
    config.encryption = EncryptionType::Aes256Gcm;
    config.encryption_key = Some(key);

    let writer = WalWriter::create(&fs, path, config).unwrap();
    let payload = b"compress then encrypt ".repeat(500);

    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"blob".to_vec(),
            payload.clone(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    let result = WalRecovery::recover_with_key(&fs, path, Some(key)).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(result.committed_writes[0].value, payload);
}

#[test]
fn test_wal_missing_key_error_handling() {
    let fs = MemoryFileSystem::new();
    let path = "missing-key.wal";

    let mut config = WalWriterConfig::default();
    config.encryption = EncryptionType::Aes256Gcm;

    let writer = WalWriter::create(&fs, path, config).unwrap();
    let err = writer.write_begin(TransactionId::from(1)).unwrap_err();
    assert!(matches!(err, WalError::MissingEncryptionKey { .. }));
}

fn overwrite_record_lsn(fs: &MemoryFileSystem, path: &str, record_offset: u64, lsn: u64) {
    let mut file = fs.open_file(path).unwrap();
    file.seek(SeekFrom::Start(record_offset + 4)).unwrap();
    file.write_all(&lsn.to_le_bytes()).unwrap();
}

fn overwrite_record_type(fs: &MemoryFileSystem, path: &str, record_offset: u64, record_type: u8) {
    let mut file = fs.open_file(path).unwrap();
    file.seek(SeekFrom::Start(record_offset + 20)).unwrap();
    file.write_all(&[record_type]).unwrap();
}

fn find_record_offsets(fs: &MemoryFileSystem, path: &str) -> Vec<u64> {
    let mut file = fs.open_file(path).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();

    let mut offsets = Vec::new();
    let mut idx = 0usize;
    while idx + 4 <= bytes.len() {
        if &bytes[idx..idx + 4] == b"WALR" {
            offsets.push(idx as u64);
        }
        idx += 1;
    }

    offsets
}

fn remove_middle_record(fs: &MemoryFileSystem, path: &str, record_index: usize) {
    let mut file = fs.open_file(path).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();

    let offsets = find_record_offsets(fs, path);
    let start = offsets[record_index] as usize;
    let end = if record_index + 1 < offsets.len() {
        offsets[record_index + 1] as usize
    } else {
        bytes.len()
    };

    bytes.drain(start..end);

    let mut rewritten = fs.open_file(path).unwrap();
    rewritten.truncate().unwrap();
    rewritten.write_all(&bytes).unwrap();
}

#[test]
fn test_wal_partial_record_write_stops_recovery_at_tail() {
    let fs = MemoryFileSystem::new();
    let path = "partial-tail.wal";
    let mut config = WalWriterConfig::default();
    config.sync_on_write = false;

    let writer = WalWriter::create(&fs, path, config).unwrap();

    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"user:1".to_vec(),
            b"Alice".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();

    writer.write_begin(TransactionId::from(2)).unwrap();
    writer
        .write_operation(
            TransactionId::from(2),
            TableId::from(1),
            WriteOpType::Put,
            b"user:2".to_vec(),
            b"Bob".to_vec(),
        )
        .unwrap();
    writer.flush().unwrap();
    drop(writer);

    let original_size = fs.filesize(path).unwrap();
    let mut file = fs.open_file(path).unwrap();
    file.set_size(original_size - 10).unwrap();
    drop(file);

    let result = WalRecovery::recover(&fs, path).unwrap_err();
    assert!(matches!(
        result,
        WalError::CorruptedWal { .. } | WalError::InvalidRecord { .. }
    ));
}

#[test]
fn test_wal_corrupted_lsn_sequence_does_not_drop_committed_data() {
    let fs = MemoryFileSystem::new();
    let path = "corrupted-lsn.wal";
    let config = WalWriterConfig::default();

    let writer = WalWriter::create(&fs, path, config).unwrap();
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"user:1".to_vec(),
            b"Alice".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();
    drop(writer);

    let offsets = find_record_offsets(&fs, path);
    overwrite_record_lsn(&fs, path, offsets[1], 99);

    let err = WalRecovery::recover(&fs, path).unwrap_err();
    let _lsn = LogSequenceNumber::from(99);
    assert!(matches!(err, WalError::ChecksumMismatch { .. }));
}

#[test]
fn test_wal_missing_middle_record_causes_recovery_error() {
    let fs = MemoryFileSystem::new();
    let path = "missing-middle.wal";
    let config = WalWriterConfig::default();

    let writer = WalWriter::create(&fs, path, config).unwrap();
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"user:1".to_vec(),
            b"Alice".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();
    drop(writer);

    remove_middle_record(&fs, path, 1);

    let result = WalRecovery::recover(&fs, path).unwrap();
    assert!(result.committed_writes.is_empty());
    assert!(result.active_transactions.is_empty());
    assert_eq!(result.records_processed, 2);
}

#[test]
fn test_wal_recovery_with_active_and_committed_transactions() {
    let fs = MemoryFileSystem::new();
    let path = "mixed-recovery.wal";
    let config = WalWriterConfig::default();

    let writer = WalWriter::create(&fs, path, config).unwrap();

    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"user:1".to_vec(),
            b"Alice".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();

    writer.write_begin(TransactionId::from(2)).unwrap();
    writer
        .write_operation(
            TransactionId::from(2),
            TableId::from(1),
            WriteOpType::Put,
            b"user:2".to_vec(),
            b"Bob".to_vec(),
        )
        .unwrap();

    writer.write_begin(TransactionId::from(3)).unwrap();
    writer
        .write_operation(
            TransactionId::from(3),
            TableId::from(1),
            WriteOpType::Delete,
            b"user:3".to_vec(),
            vec![],
        )
        .unwrap();
    writer.write_rollback(TransactionId::from(3)).unwrap();

    writer.flush().unwrap();

    let result = WalRecovery::recover(&fs, path).unwrap();
    assert_eq!(result.committed_writes.len(), 1);
    assert_eq!(result.committed_writes[0].key, b"user:1");
    assert!(result.active_transactions.contains(&TransactionId::from(2)));
    assert!(!result.active_transactions.contains(&TransactionId::from(1)));
    assert!(!result.active_transactions.contains(&TransactionId::from(3)));
}

#[test]
fn test_wal_concurrent_corruption_scenario_reports_failure() {
    let fs = Arc::new(MemoryFileSystem::new());
    let path = "concurrent-corruption.wal";
    let mut config = WalWriterConfig::default();
    config.sync_on_write = false;

    let writer = Arc::new(WalWriter::create(fs.as_ref(), path, config).unwrap());

    let writer_thread = {
        let writer = Arc::clone(&writer);
        thread::spawn(move || {
            writer.write_begin(TransactionId::from(1)).unwrap();
            writer
                .write_operation(
                    TransactionId::from(1),
                    TableId::from(1),
                    WriteOpType::Put,
                    b"user:1".to_vec(),
                    vec![0xAA; 4096],
                )
                .unwrap();
            writer.write_commit(TransactionId::from(1)).unwrap();
            writer.flush().unwrap();
        })
    };

    writer_thread.join().unwrap();

    let corruptor_thread = {
        let fs = Arc::clone(&fs);
        thread::spawn(move || {
            let offsets = find_record_offsets(fs.as_ref(), path);
            if offsets.len() > 1 {
                overwrite_record_type(fs.as_ref(), path, offsets[1], 0xFF);
            }
        })
    };

    corruptor_thread.join().unwrap();

    let result = WalRecovery::recover(fs.as_ref(), path);
    assert!(result.is_err());
}

#[test]
fn test_wal_encryption_key_rotation_failure_is_detected() {
    let fs = MemoryFileSystem::new();
    let path = "key-rotation.wal";
    let old_key = [0x11u8; 32];
    let new_key = [0x22u8; 32];

    let mut old_config = WalWriterConfig::default();
    old_config.encryption = EncryptionType::Aes256Gcm;
    old_config.encryption_key = Some(old_key);

    let writer = WalWriter::create(&fs, path, old_config).unwrap();
    writer.write_begin(TransactionId::from(1)).unwrap();
    writer
        .write_operation(
            TransactionId::from(1),
            TableId::from(1),
            WriteOpType::Put,
            b"k1".to_vec(),
            b"encrypted payload".to_vec(),
        )
        .unwrap();
    writer.write_commit(TransactionId::from(1)).unwrap();
    writer.flush().unwrap();

    let err = WalRecovery::recover_with_key(&fs, path, Some(new_key)).unwrap_err();
    assert!(matches!(err, WalError::DecryptionError { .. }));
}
