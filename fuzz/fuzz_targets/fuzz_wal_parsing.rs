#![no_main]

use libfuzzer_sys::fuzz_target;
use nanostore::wal::record::{WalRecord, RecordType, WriteOpType, RecordData, LogSequenceNumber};
use nanostore::pager::{CompressionType, EncryptionType};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;

fuzz_target!(|data: &[u8]| {
    // Skip if data is too small to be interesting
    if data.is_empty() {
        return;
    }

    // Test 1: Try to parse WAL record from arbitrary bytes without encryption key
    let _ = WalRecord::from_bytes(data, None);

    // Test 2: Try to parse WAL record with encryption key
    if data.len() >= 32 {
        let key: [u8; 32] = data[0..32].try_into().unwrap();
        let _ = WalRecord::from_bytes(data, Some(&key));
        
        // Also try with offset data
        if data.len() > 32 {
            let _ = WalRecord::from_bytes(&data[32..], Some(&key));
        }
    }

    // Test 3: Test record type conversion with arbitrary bytes
    if !data.is_empty() {
        let _ = RecordType::from_u8(data[0]);
    }

    // Test 4: Test write operation type conversion
    if !data.is_empty() {
        let _ = WriteOpType::from_u8(data[0]);
    }

    // Test 5: Create valid records and serialize with fuzzy data
    if data.len() >= 16 {
        let lsn = u64::from_le_bytes(data[0..8].try_into().unwrap());
        let txn_id = u64::from_le_bytes(data[8..16].try_into().unwrap());

        // Test Begin record
        let begin_record = WalRecord::new(
            LogSequenceNumber::from(lsn),
            RecordData::Begin {
                txn_id: TransactionId::from(txn_id),
            },
            CompressionType::None,
            EncryptionType::None,
        );
        let _ = begin_record.to_bytes(None);

        // Test Commit record
        let commit_record = WalRecord::new(
            LogSequenceNumber::from(lsn),
            RecordData::Commit {
                txn_id: TransactionId::from(txn_id),
            },
            CompressionType::None,
            EncryptionType::None,
        );
        let _ = commit_record.to_bytes(None);

        // Test Rollback record
        let rollback_record = WalRecord::new(
            LogSequenceNumber::from(lsn),
            RecordData::Rollback {
                txn_id: TransactionId::from(txn_id),
            },
            CompressionType::None,
            EncryptionType::None,
        );
        let _ = rollback_record.to_bytes(None);

        // Test Prepare record
        let prepare_record = WalRecord::new(
            LogSequenceNumber::from(lsn),
            RecordData::Prepare {
                txn_id: TransactionId::from(txn_id),
            },
            CompressionType::None,
            EncryptionType::None,
        );
        let _ = prepare_record.to_bytes(None);
    }

    // Test 6: Create Write records with fuzzy key/value data
    if data.len() >= 24 {
        let lsn = u64::from_le_bytes(data[0..8].try_into().unwrap());
        let txn_id = u64::from_le_bytes(data[8..16].try_into().unwrap());
        let table_id = u64::from_le_bytes(data[16..24].try_into().unwrap());

        // Use remaining data as key/value
        let key_len = data.len().min(100);
        let key = data[..key_len].to_vec();
        let value = data[key_len..].to_vec();

        // Test different write operation types
        for op_type in [
            WriteOpType::Put,
            WriteOpType::Delete,
            WriteOpType::BloomInsert,
            WriteOpType::GraphAddEdge,
            WriteOpType::GraphRemoveEdge,
            WriteOpType::TimeSeriesInsert,
            WriteOpType::TimeSeriesDelete,
            WriteOpType::VectorInsert,
            WriteOpType::VectorDelete,
            WriteOpType::GeoInsert,
            WriteOpType::GeoDelete,
            WriteOpType::FullTextIndex,
            WriteOpType::FullTextUpdate,
            WriteOpType::FullTextDelete,
        ] {
            let write_record = WalRecord::new(
                LogSequenceNumber::from(lsn),
                RecordData::Write {
                    txn_id: TransactionId::from(txn_id),
                    table_id: TableId::from(table_id),
                    op_type,
                    key: key.clone(),
                    value: value.clone(),
                },
                CompressionType::None,
                EncryptionType::None,
            );
            let _ = write_record.to_bytes(None);
        }
    }

    // Test 7: Create Checkpoint records with fuzzy active transaction lists
    if data.len() >= 16 {
        let lsn = u64::from_le_bytes(data[0..8].try_into().unwrap());
        
        // Create active transaction list from fuzzy data
        let mut active_txns = Vec::new();
        let mut offset = 8;
        while offset + 8 <= data.len() && active_txns.len() < 100 {
            let txn_id = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            active_txns.push(TransactionId::from(txn_id));
            offset += 8;
        }

        let checkpoint_record = WalRecord::new(
            LogSequenceNumber::from(lsn),
            RecordData::Checkpoint {
                lsn: LogSequenceNumber::from(lsn),
                active_txns,
            },
            CompressionType::None,
            EncryptionType::None,
        );
        let _ = checkpoint_record.to_bytes(None);
    }

    // Test 8: Test compression combinations
    if data.len() >= 24 {
        let lsn = u64::from_le_bytes(data[0..8].try_into().unwrap());
        let txn_id = u64::from_le_bytes(data[8..16].try_into().unwrap());
        let table_id = u64::from_le_bytes(data[16..24].try_into().unwrap());

        let key = data[..data.len().min(50)].to_vec();
        let value = data[data.len().min(50)..].to_vec();

        // Test LZ4 compression
        let lz4_record = WalRecord::new(
            LogSequenceNumber::from(lsn),
            RecordData::Write {
                txn_id: TransactionId::from(txn_id),
                table_id: TableId::from(table_id),
                op_type: WriteOpType::Put,
                key: key.clone(),
                value: value.clone(),
            },
            CompressionType::Lz4,
            EncryptionType::None,
        );
        if let Ok(bytes) = lz4_record.to_bytes(None) {
            let _ = WalRecord::from_bytes(&bytes, None);
        }

        // Test Zstd compression
        let zstd_record = WalRecord::new(
            LogSequenceNumber::from(lsn),
            RecordData::Write {
                txn_id: TransactionId::from(txn_id),
                table_id: TableId::from(table_id),
                op_type: WriteOpType::Put,
                key: key.clone(),
                value: value.clone(),
            },
            CompressionType::Zstd,
            EncryptionType::None,
        );
        if let Ok(bytes) = zstd_record.to_bytes(None) {
            let _ = WalRecord::from_bytes(&bytes, None);
        }
    }

    // Test 9: Test encryption combinations
    if data.len() >= 56 {
        let key: [u8; 32] = data[0..32].try_into().unwrap();
        let lsn = u64::from_le_bytes(data[32..40].try_into().unwrap());
        let txn_id = u64::from_le_bytes(data[40..48].try_into().unwrap());
        let table_id = u64::from_le_bytes(data[48..56].try_into().unwrap());

        let record_key = data[..data.len().min(50)].to_vec();
        let record_value = data[data.len().min(50)..].to_vec();

        // Test AES-256-GCM encryption
        let encrypted_record = WalRecord::new(
            LogSequenceNumber::from(lsn),
            RecordData::Write {
                txn_id: TransactionId::from(txn_id),
                table_id: TableId::from(table_id),
                op_type: WriteOpType::Put,
                key: record_key.clone(),
                value: record_value.clone(),
            },
            CompressionType::None,
            EncryptionType::Aes256Gcm,
        );
        if let Ok(bytes) = encrypted_record.to_bytes(Some(&key)) {
            let _ = WalRecord::from_bytes(&bytes, Some(&key));
            // Also try with wrong key
            let wrong_key = [0u8; 32];
            let _ = WalRecord::from_bytes(&bytes, Some(&wrong_key));
        }

        // Test compression + encryption
        let compressed_encrypted_record = WalRecord::new(
            LogSequenceNumber::from(lsn),
            RecordData::Write {
                txn_id: TransactionId::from(txn_id),
                table_id: TableId::from(table_id),
                op_type: WriteOpType::Put,
                key: record_key,
                value: record_value,
            },
            CompressionType::Lz4,
            EncryptionType::Aes256Gcm,
        );
        if let Ok(bytes) = compressed_encrypted_record.to_bytes(Some(&key)) {
            let _ = WalRecord::from_bytes(&bytes, Some(&key));
        }
    }

    // Test 10: Test truncated inputs at various boundaries
    for truncate_at in [1, 4, 8, 16, 32, 64, 67, 100, 200] {
        if data.len() > truncate_at {
            let _ = WalRecord::from_bytes(&data[..truncate_at], None);
        }
    }

    // Test 11: Test corrupted checksums
    if data.len() >= 100 {
        // Create a valid record
        let lsn = u64::from_le_bytes(data[0..8].try_into().unwrap());
        let txn_id = u64::from_le_bytes(data[8..16].try_into().unwrap());
        
        let record = WalRecord::new(
            LogSequenceNumber::from(lsn),
            RecordData::Begin {
                txn_id: TransactionId::from(txn_id),
            },
            CompressionType::None,
            EncryptionType::None,
        );
        
        if let Ok(mut bytes) = record.to_bytes(None) {
            // Corrupt the checksum (last 32 bytes)
            if bytes.len() >= 32 {
                let checksum_start = bytes.len() - 32;
                for i in 0..32.min(data.len()) {
                    bytes[checksum_start + i] ^= data[i];
                }
                let _ = WalRecord::from_bytes(&bytes, None);
            }
        }
    }
});

// Made with Bob
