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

//! WAL record types and serialization

use crate::pager::{CompressionType, EncryptionType};
use crate::txn::TransactionId;
use crate::types::TableId;
use crate::wal::{WalError, WalResult};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::Rng;
use sha2::{Digest, Sha256};
use std::fmt::Formatter;
use std::io::Write;

/// Monotonic Log Sequence Number (LSN)  - unique identifier for each WAL record
///
/// Implementations may encode term, segment, offset, shard, or epoch information
/// in a richer internal representation. The public trait only requires stable
/// ordering.
#[derive(
    Clone,
    Copy,
    Debug,
    Ord,
    PartialOrd,
    Eq,
    PartialEq,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct LogSequenceNumber(u64);

impl LogSequenceNumber {
    #[must_use]
    pub fn as_u64(&self) -> u64 {
        self.0
    }

    #[must_use]
    pub fn to_bytes(&self) -> [u8; 8] {
        self.0.to_le_bytes()
    }
}

impl From<u64> for LogSequenceNumber {
    fn from(value: u64) -> Self {
        LogSequenceNumber(value)
    }
}

impl std::fmt::Display for LogSequenceNumber {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "LSN({})", self.0)
    }
}

/// WAL record type discriminator
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordType {
    /// Begin transaction
    Begin = 1,
    /// Write operation (insert/update/delete)
    Write = 2,
    /// Commit transaction
    Commit = 3,
    /// Rollback transaction
    Rollback = 4,
    /// Checkpoint
    Checkpoint = 5,
    /// Prepare transaction (two-phase commit)
    Prepare = 6,
}

impl RecordType {
    /// Convert from u8
    pub fn from_u8(value: u8) -> WalResult<Self> {
        match value {
            1 => Ok(RecordType::Begin),
            2 => Ok(RecordType::Write),
            3 => Ok(RecordType::Commit),
            4 => Ok(RecordType::Rollback),
            5 => Ok(RecordType::Checkpoint),
            6 => Ok(RecordType::Prepare),
            _ => Err(WalError::InvalidRecord {
                lsn: LogSequenceNumber::from(0),
                details: format!("Invalid record type: {}", value),
            }),
        }
    }

    /// Convert to u8
    #[must_use]
    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Write operation type
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOpType {
    /// Insert or update a key-value pair
    Put = 1,
    /// Delete a key
    Delete = 2,
    /// Insert a key into a bloom filter
    BloomInsert = 3,
    /// Add an edge to a graph
    GraphAddEdge = 4,
    /// Remove an edge from a graph
    GraphRemoveEdge = 5,
    /// Append a point to a time series
    TimeSeriesInsert = 6,
    /// Delete a point from a time series
    TimeSeriesDelete = 7,
    /// Insert a vector
    VectorInsert = 8,
    /// Delete a vector
    VectorDelete = 9,
    /// Insert a geometry into a geospatial index
    GeoInsert = 10,
    /// Delete a geometry from a geospatial index
    GeoDelete = 11,
    /// Index a document in a full-text index
    FullTextIndex = 12,
    /// Update a document in a full-text index
    FullTextUpdate = 13,
    /// Delete a document from a full-text index
    FullTextDelete = 14,
}

impl WriteOpType {
    /// Convert from u8
    pub fn from_u8(value: u8) -> WalResult<Self> {
        match value {
            1 => Ok(WriteOpType::Put),
            2 => Ok(WriteOpType::Delete),
            3 => Ok(WriteOpType::BloomInsert),
            4 => Ok(WriteOpType::GraphAddEdge),
            5 => Ok(WriteOpType::GraphRemoveEdge),
            6 => Ok(WriteOpType::TimeSeriesInsert),
            7 => Ok(WriteOpType::TimeSeriesDelete),
            8 => Ok(WriteOpType::VectorInsert),
            9 => Ok(WriteOpType::VectorDelete),
            10 => Ok(WriteOpType::GeoInsert),
            11 => Ok(WriteOpType::GeoDelete),
            12 => Ok(WriteOpType::FullTextIndex),
            13 => Ok(WriteOpType::FullTextUpdate),
            14 => Ok(WriteOpType::FullTextDelete),
            _ => Err(WalError::InvalidRecord {
                lsn: LogSequenceNumber::from(0),
                details: format!("Invalid write op type: {}", value),
            }),
        }
    }

    /// Convert to u8
    #[must_use]
    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

/// WAL record data
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordData {
    /// Begin transaction
    Begin {
        /// Transaction ID
        txn_id: TransactionId,
    },
    /// Write operation
    Write {
        /// Transaction ID
        txn_id: TransactionId,
        /// Table ID
        table_id: TableId,
        /// Operation type
        op_type: WriteOpType,
        /// Key
        key: Vec<u8>,
        /// Value (empty for delete)
        value: Vec<u8>,
    },
    /// Commit transaction
    Commit {
        /// Transaction ID
        txn_id: TransactionId,
    },
    /// Rollback transaction
    Rollback {
        /// Transaction ID
        txn_id: TransactionId,
    },
    /// Checkpoint
    Checkpoint {
        /// LSN of the checkpoint
        lsn: LogSequenceNumber,
        /// Number of active transactions at checkpoint
        active_txns: Vec<TransactionId>,
    },
    /// Prepare transaction (two-phase commit)
    Prepare {
        /// Transaction ID
        txn_id: TransactionId,
    },
}

impl RecordData {
    /// Get the record type
    #[must_use]
    pub fn record_type(&self) -> RecordType {
        match self {
            RecordData::Begin { .. } => RecordType::Begin,
            RecordData::Write { .. } => RecordType::Write,
            RecordData::Commit { .. } => RecordType::Commit,
            RecordData::Rollback { .. } => RecordType::Rollback,
            RecordData::Checkpoint { .. } => RecordType::Checkpoint,
            RecordData::Prepare { .. } => RecordType::Prepare,
        }
    }

    /// Get the transaction ID (if applicable)
    #[must_use]
    pub fn txn_id(&self) -> Option<TransactionId> {
        match self {
            RecordData::Begin { txn_id } => Some(*txn_id),
            RecordData::Write { txn_id, .. } => Some(*txn_id),
            RecordData::Commit { txn_id } => Some(*txn_id),
            RecordData::Rollback { txn_id } => Some(*txn_id),
            RecordData::Checkpoint { .. } => None,
            RecordData::Prepare { txn_id } => Some(*txn_id),
        }
    }
}

/// WAL record with header and data
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalRecord {
    /// Log Sequence Number
    pub lsn: LogSequenceNumber,
    /// Timestamp (microseconds since epoch)
    pub timestamp: u64,
    /// Record data
    pub data: RecordData,
    /// Compression type
    pub compression: CompressionType,
    /// Encryption type
    pub encryption: EncryptionType,
}

impl WalRecord {
    /// Create a new WAL record
    #[must_use]
    pub fn new(
        lsn: LogSequenceNumber,
        data: RecordData,
        compression: CompressionType,
        encryption: EncryptionType,
    ) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;

        Self {
            lsn,
            timestamp,
            data,
            compression,
            encryption,
        }
    }

    /// Serialize the record to bytes
    ///
    /// Format:
    /// - Magic (4 bytes): 0x57414C52 ("WALR")
    /// - LSN (8 bytes)
    /// - Timestamp (8 bytes)
    /// - Record type (1 byte)
    /// - Compression type (1 byte)
    /// - Encryption type (1 byte)
    /// - Uncompressed size (4 bytes)
    /// - Compressed/encrypted size (4 bytes)
    /// - Data (variable, possibly compressed and/or encrypted)
    /// - Checksum (32 bytes, SHA-256)
    pub fn to_bytes(&self, encryption_key: Option<&[u8; 32]>) -> WalResult<Vec<u8>> {
        let mut buffer = Vec::new();

        // Magic number
        buffer.write_all(b"WALR").map_err(WalError::IoError)?;

        // LSN
        buffer
            .write_all(&self.lsn.to_bytes())
            .map_err(WalError::IoError)?;

        // Timestamp
        buffer
            .write_all(&self.timestamp.to_le_bytes())
            .map_err(WalError::IoError)?;

        // Record type
        buffer
            .write_all(&[self.data.record_type().to_u8()])
            .map_err(WalError::IoError)?;

        // Compression type
        buffer
            .write_all(&[self.compression.to_u8()])
            .map_err(WalError::IoError)?;

        // Encryption type
        buffer
            .write_all(&[self.encryption.to_u8()])
            .map_err(WalError::IoError)?;

        // Serialize data
        let data_bytes = self.serialize_data()?;
        let uncompressed_size = data_bytes.len() as u32;

        // Compress data if needed
        let compressed_data = match self.compression {
            CompressionType::None => data_bytes,
            CompressionType::Lz4 => lz4_flex::compress_prepend_size(&data_bytes),
            CompressionType::Zstd => {
                zstd::encode_all(&data_bytes[..], 3).map_err(|e| WalError::SerializationError {
                    lsn: self.lsn,
                    record_type: format!("{:?}", self.data.record_type()),
                    details: format!("Zstd compression failed: {}", e),
                })?
            }
        };

        // Encrypt data if needed (after compression)
        let final_data = match self.encryption {
            EncryptionType::None => compressed_data,
            EncryptionType::Aes256Gcm => {
                let key = encryption_key.ok_or_else(|| WalError::MissingEncryptionKey {
                    encryption_type: "AES-256-GCM".to_string(),
                })?;

                let mut nonce_bytes = [0u8; 12];
                rand::rng().fill_bytes(&mut nonce_bytes);
                let nonce = Nonce::from_slice(&nonce_bytes);

                let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
                let encrypted = cipher
                    .encrypt(nonce, compressed_data.as_ref())
                    .map_err(|e| WalError::EncryptionError {
                        lsn: self.lsn,
                        encryption_type: "AES-256-GCM".to_string(),
                        details: e.to_string(),
                    })?;

                let mut result = Vec::with_capacity(12 + encrypted.len());
                result.extend_from_slice(&nonce_bytes);
                result.extend_from_slice(&encrypted);
                result
            }
        };

        let stored_size = final_data.len() as u32;

        // Uncompressed size
        buffer
            .write_all(&uncompressed_size.to_le_bytes())
            .map_err(WalError::IoError)?;

        // Stored size
        buffer
            .write_all(&stored_size.to_le_bytes())
            .map_err(WalError::IoError)?;

        // Data
        buffer.write_all(&final_data).map_err(WalError::IoError)?;

        // Calculate checksum (excluding the checksum itself)
        let mut hasher = Sha256::new();
        hasher.update(&buffer);
        let checksum = hasher.finalize();

        // Append checksum
        buffer.write_all(&checksum).map_err(WalError::IoError)?;

        Ok(buffer)
    }

    /// Deserialize a record from bytes
    pub fn from_bytes(bytes: &[u8], encryption_key: Option<&[u8; 32]>) -> WalResult<Self> {
        if bytes.len() < 67 {
            // Minimum size: magic(4) + lsn(8) + timestamp(8) + type(1) + compression(1) + encryption(1) + uncompressed(4) + stored(4) + checksum(32)
            return Err(WalError::InvalidRecord {
                lsn: LogSequenceNumber::from(0),
                details: format!("Record too short: {} bytes", bytes.len()),
            });
        }

        let mut cursor = 0;

        // Check magic
        if &bytes[cursor..cursor + 4] != b"WALR" {
            return Err(WalError::InvalidRecord {
                lsn: LogSequenceNumber::from(0),
                details: format!("Invalid magic number: {:?}", &bytes[0..4]),
            });
        }
        cursor += 4;

        // LSN
        let lsn = LogSequenceNumber::from(u64::from_le_bytes(
            bytes[cursor..cursor + 8]
                .try_into()
                .map_err(|_| WalError::DeserializationError {
                    offset: 4,
                    details: "Invalid LSN bytes".to_string(),
                })?,
        ));
        cursor += 8;

        // Timestamp
        let timestamp = u64::from_le_bytes(bytes[cursor..cursor + 8].try_into().map_err(|_| {
            WalError::DeserializationError {
                offset: 12,
                details: "Invalid timestamp bytes".to_string(),
            }
        })?);
        cursor += 8;

        // Record type
        let record_type = RecordType::from_u8(bytes[cursor])?;
        cursor += 1;

        // Compression type
        let compression = CompressionType::from_u8(bytes[cursor]).ok_or_else(|| {
            WalError::DeserializationError {
                offset: cursor as u64,
                details: format!("Invalid compression type: {}", bytes[cursor]),
            }
        })?;
        cursor += 1;

        // Encryption type
        let encryption = EncryptionType::from_u8(bytes[cursor]).ok_or_else(|| {
            WalError::DeserializationError {
                offset: cursor as u64,
                details: format!("Invalid encryption type: {}", bytes[cursor]),
            }
        })?;
        cursor += 1;

        // Uncompressed size
        let _uncompressed_size =
            u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().map_err(|_| {
                WalError::DeserializationError {
                    offset: cursor as u64,
                    details: "Invalid uncompressed size bytes".to_string(),
                }
            })?) as usize;
        cursor += 4;

        // Stored size
        let stored_size =
            u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().map_err(|_| {
                WalError::DeserializationError {
                    offset: cursor as u64,
                    details: "Invalid stored size bytes".to_string(),
                }
            })?) as usize;
        cursor += 4;

        // Verify we have enough bytes
        if bytes.len() < cursor + stored_size + 32 {
            return Err(WalError::InvalidRecord {
                lsn,
                details: format!(
                    "Incomplete record: expected {} bytes, have {} bytes",
                    cursor + stored_size + 32,
                    bytes.len()
                ),
            });
        }

        // Verify checksum
        let data_end = cursor + stored_size;
        let expected_checksum = &bytes[data_end..data_end + 32];
        let mut hasher = Sha256::new();
        hasher.update(&bytes[..data_end]);
        let actual_checksum = hasher.finalize();

        if expected_checksum != actual_checksum.as_slice() {
            return Err(WalError::ChecksumMismatch {
                lsn,
                offset: 0,
                expected: u32::from_le_bytes(expected_checksum[0..4].try_into().unwrap()),
                found: u32::from_le_bytes(actual_checksum[0..4].try_into().unwrap()),
            });
        }

        // Get stored data
        let stored_data = &bytes[cursor..data_end];

        // Decrypt if needed
        let compressed_data = match encryption {
            EncryptionType::None => stored_data.to_vec(),
            EncryptionType::Aes256Gcm => {
                let key = encryption_key.ok_or_else(|| WalError::MissingEncryptionKey {
                    encryption_type: "AES-256-GCM".to_string(),
                })?;

                if stored_data.len() < 12 {
                    return Err(WalError::DecryptionError {
                        offset: cursor as u64,
                        encryption_type: "AES-256-GCM".to_string(),
                        details: format!(
                            "Insufficient data for nonce: {} bytes",
                            stored_data.len()
                        ),
                    });
                }

                let nonce = Nonce::from_slice(&stored_data[0..12]);
                let ciphertext = &stored_data[12..];
                let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));

                cipher
                    .decrypt(nonce, ciphertext)
                    .map_err(|e| WalError::DecryptionError {
                        offset: cursor as u64,
                        encryption_type: "AES-256-GCM".to_string(),
                        details: e.to_string(),
                    })?
            }
        };

        // Decompress if needed
        let data_bytes =
            match compression {
                CompressionType::None => compressed_data,
                CompressionType::Lz4 => lz4_flex::decompress_size_prepended(&compressed_data)
                    .map_err(|e| WalError::DeserializationError {
                        offset: cursor as u64,
                        details: format!("LZ4 decompression failed: {}", e),
                    })?,
                CompressionType::Zstd => zstd::decode_all(&compressed_data[..]).map_err(|e| {
                    WalError::DeserializationError {
                        offset: cursor as u64,
                        details: format!("Zstd decompression failed: {}", e),
                    }
                })?,
            };

        // Deserialize data
        let data = Self::deserialize_data(record_type, &data_bytes)?;

        Ok(Self {
            lsn,
            timestamp,
            data,
            compression,
            encryption,
        })
    }

    /// Serialize record data
    fn serialize_data(&self) -> WalResult<Vec<u8>> {
        let mut buffer = Vec::new();

        match &self.data {
            RecordData::Begin { txn_id } => {
                buffer
                    .write_all(&txn_id.to_bytes())
                    .map_err(WalError::IoError)?;
            }
            RecordData::Write {
                txn_id,
                table_id,
                op_type,
                key,
                value,
            } => {
                // Transaction ID
                buffer
                    .write_all(&txn_id.to_bytes())
                    .map_err(WalError::IoError)?;

                // Table ID (8 bytes)
                buffer
                    .write_all(&table_id.to_bytes())
                    .map_err(WalError::IoError)?;

                // Operation type
                buffer
                    .write_all(&[op_type.to_u8()])
                    .map_err(WalError::IoError)?;

                // Key length and data
                buffer
                    .write_all(&(key.len() as u32).to_le_bytes())
                    .map_err(WalError::IoError)?;
                buffer.write_all(key).map_err(WalError::IoError)?;

                // Value length and data
                buffer
                    .write_all(&(value.len() as u32).to_le_bytes())
                    .map_err(WalError::IoError)?;
                buffer.write_all(value).map_err(WalError::IoError)?;
            }
            RecordData::Commit { txn_id } => {
                buffer
                    .write_all(&txn_id.to_bytes())
                    .map_err(WalError::IoError)?;
            }
            RecordData::Rollback { txn_id } => {
                buffer
                    .write_all(&txn_id.to_bytes())
                    .map_err(WalError::IoError)?;
            }
            RecordData::Checkpoint { lsn, active_txns } => {
                // LSN
                buffer
                    .write_all(&lsn.to_bytes())
                    .map_err(WalError::IoError)?;

                // Number of active transactions
                buffer
                    .write_all(&(active_txns.len() as u32).to_le_bytes())
                    .map_err(WalError::IoError)?;

                // Active transaction IDs
                for txn_id in active_txns {
                    buffer
                        .write_all(&txn_id.to_bytes())
                        .map_err(WalError::IoError)?;
                }
            }
            RecordData::Prepare { txn_id } => {
                buffer
                    .write_all(&txn_id.to_bytes())
                    .map_err(WalError::IoError)?;
            }
        }

        Ok(buffer)
    }

    /// Deserialize record data
    fn deserialize_data(record_type: RecordType, bytes: &[u8]) -> WalResult<RecordData> {
        let mut cursor = 0;

        match record_type {
            RecordType::Begin => {
                if bytes.len() < 8 {
                    return Err(WalError::DeserializationError {
                        offset: 0,
                        details: format!("Invalid Begin record: {} bytes", bytes.len()),
                    });
                }
                let txn_id = TransactionId::from(u64::from_le_bytes(
                    bytes[cursor..cursor + 8].try_into().unwrap(),
                ));
                Ok(RecordData::Begin { txn_id })
            }
            RecordType::Write => {
                // Transaction ID
                if bytes.len() < cursor + 8 {
                    return Err(WalError::DeserializationError {
                        offset: cursor as u64,
                        details: "Invalid Write record: missing transaction ID".to_string(),
                    });
                }
                let txn_id = TransactionId::from(u64::from_le_bytes(
                    bytes[cursor..cursor + 8].try_into().unwrap(),
                ));
                cursor += 8;

                // Table ID (8 bytes)
                if bytes.len() < cursor + 8 {
                    return Err(WalError::DeserializationError {
                        offset: cursor as u64,
                        details: "Invalid Write record: missing table ID".to_string(),
                    });
                }
                let table_id = TableId::from(u64::from_le_bytes(
                    bytes[cursor..cursor + 8].try_into().unwrap(),
                ));
                cursor += 8;

                // Operation type
                if bytes.len() < cursor + 1 {
                    return Err(WalError::DeserializationError {
                        offset: cursor as u64,
                        details: "Invalid Write record: missing operation type".to_string(),
                    });
                }
                let op_type = WriteOpType::from_u8(bytes[cursor])?;
                cursor += 1;

                // Key
                if bytes.len() < cursor + 4 {
                    return Err(WalError::DeserializationError {
                        offset: cursor as u64,
                        details: "Invalid Write record: missing key length".to_string(),
                    });
                }
                let key_len =
                    u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
                cursor += 4;

                if bytes.len() < cursor + key_len {
                    return Err(WalError::DeserializationError {
                        offset: cursor as u64,
                        details: format!(
                            "Invalid Write record: missing key data ({} bytes)",
                            key_len
                        ),
                    });
                }
                let key = bytes[cursor..cursor + key_len].to_vec();
                cursor += key_len;

                // Value
                if bytes.len() < cursor + 4 {
                    return Err(WalError::DeserializationError {
                        offset: cursor as u64,
                        details: "Invalid Write record: missing value length".to_string(),
                    });
                }
                let value_len =
                    u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
                cursor += 4;

                if bytes.len() < cursor + value_len {
                    return Err(WalError::DeserializationError {
                        offset: cursor as u64,
                        details: format!(
                            "Invalid Write record: missing value data ({} bytes)",
                            value_len
                        ),
                    });
                }
                let value = bytes[cursor..cursor + value_len].to_vec();

                Ok(RecordData::Write {
                    txn_id,
                    table_id,
                    op_type,
                    key,
                    value,
                })
            }
            RecordType::Commit => {
                if bytes.len() < 8 {
                    return Err(WalError::DeserializationError {
                        offset: 0,
                        details: format!("Invalid Commit record: {} bytes", bytes.len()),
                    });
                }
                let txn_id = TransactionId::from(u64::from_le_bytes(
                    bytes[cursor..cursor + 8].try_into().unwrap(),
                ));
                Ok(RecordData::Commit { txn_id })
            }
            RecordType::Rollback => {
                if bytes.len() < 8 {
                    return Err(WalError::DeserializationError {
                        offset: 0,
                        details: format!("Invalid Rollback record: {} bytes", bytes.len()),
                    });
                }
                let txn_id = TransactionId::from(u64::from_le_bytes(
                    bytes[cursor..cursor + 8].try_into().unwrap(),
                ));
                Ok(RecordData::Rollback { txn_id })
            }
            RecordType::Checkpoint => {
                // LSN
                if bytes.len() < cursor + 8 {
                    return Err(WalError::DeserializationError {
                        offset: cursor as u64,
                        details: "Invalid Checkpoint record: missing LSN".to_string(),
                    });
                }
                let lsn = LogSequenceNumber::from(u64::from_le_bytes(
                    bytes[cursor..cursor + 8].try_into().unwrap(),
                ));
                cursor += 8;

                // Number of active transactions
                if bytes.len() < cursor + 4 {
                    return Err(WalError::DeserializationError {
                        offset: cursor as u64,
                        details: "Invalid Checkpoint record: missing transaction count".to_string(),
                    });
                }
                let num_txns =
                    u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
                cursor += 4;

                // Active transaction IDs
                let mut active_txns = Vec::with_capacity(num_txns);
                for _ in 0..num_txns {
                    if bytes.len() < cursor + 8 {
                        return Err(WalError::DeserializationError {
                            offset: cursor as u64,
                            details: "Invalid Checkpoint record: missing transaction ID"
                                .to_string(),
                        });
                    }
                    let txn_id = TransactionId::from(u64::from_le_bytes(
                        bytes[cursor..cursor + 8].try_into().unwrap(),
                    ));
                    active_txns.push(txn_id);
                    cursor += 8;
                }

                Ok(RecordData::Checkpoint { lsn, active_txns })
            }
            RecordType::Prepare => {
                if bytes.len() < 8 {
                    return Err(WalError::DeserializationError {
                        offset: 0,
                        details: format!("Invalid Prepare record: {} bytes", bytes.len()),
                    });
                }
                let txn_id = TransactionId::from(u64::from_le_bytes(
                    bytes[cursor..cursor + 8].try_into().unwrap(),
                ));
                Ok(RecordData::Prepare { txn_id })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_type_conversion() {
        assert_eq!(RecordType::from_u8(1).unwrap(), RecordType::Begin);
        assert_eq!(RecordType::from_u8(2).unwrap(), RecordType::Write);
        assert_eq!(RecordType::from_u8(3).unwrap(), RecordType::Commit);
        assert_eq!(RecordType::from_u8(4).unwrap(), RecordType::Rollback);
        assert_eq!(RecordType::from_u8(5).unwrap(), RecordType::Checkpoint);
        assert!(RecordType::from_u8(99).is_err());
    }

    #[test]
    fn test_begin_record_serialization() {
        let record = WalRecord::new(
            LogSequenceNumber::from(1),
            RecordData::Begin {
                txn_id: TransactionId::from(42),
            },
            CompressionType::None,
            EncryptionType::None,
        );
        let bytes = record.to_bytes(None).unwrap();
        let deserialized = WalRecord::from_bytes(&bytes, None).unwrap();

        assert_eq!(record.lsn, deserialized.lsn);
        assert_eq!(record.data, deserialized.data);
        assert_eq!(record.compression, deserialized.compression);
        assert_eq!(record.encryption, deserialized.encryption);
    }

    #[test]
    fn test_write_record_serialization() {
        let record = WalRecord::new(
            LogSequenceNumber::from(2),
            RecordData::Write {
                txn_id: TransactionId::from(42),
                table_id: TableId::from(1),
                op_type: WriteOpType::Put,
                key: b"key1".to_vec(),
                value: b"value1".to_vec(),
            },
            CompressionType::None,
            EncryptionType::None,
        );
        let bytes = record.to_bytes(None).unwrap();
        let deserialized = WalRecord::from_bytes(&bytes, None).unwrap();

        assert_eq!(record.lsn, deserialized.lsn);
        assert_eq!(record.data, deserialized.data);
        assert_eq!(record.compression, deserialized.compression);
        assert_eq!(record.encryption, deserialized.encryption);
    }

    #[test]
    fn test_bloom_insert_record_serialization() {
        let record = WalRecord::new(
            LogSequenceNumber::from(5),
            RecordData::Write {
                txn_id: TransactionId::from(42),
                table_id: TableId::from(7),
                op_type: WriteOpType::BloomInsert,
                key: b"bloom-key".to_vec(),
                value: vec![],
            },
            CompressionType::None,
            EncryptionType::None,
        );
        let bytes = record.to_bytes(None).unwrap();
        let deserialized = WalRecord::from_bytes(&bytes, None).unwrap();

        assert_eq!(record.lsn, deserialized.lsn);
        assert_eq!(record.data, deserialized.data);
        assert_eq!(record.compression, deserialized.compression);
        assert_eq!(record.encryption, deserialized.encryption);
    }

    #[test]
    fn test_commit_record_serialization() {
        let record = WalRecord::new(
            LogSequenceNumber::from(3),
            RecordData::Commit {
                txn_id: TransactionId::from(42),
            },
            CompressionType::None,
            EncryptionType::None,
        );
        let bytes = record.to_bytes(None).unwrap();
        let deserialized = WalRecord::from_bytes(&bytes, None).unwrap();

        assert_eq!(record.lsn, deserialized.lsn);
        assert_eq!(record.data, deserialized.data);
        assert_eq!(record.compression, deserialized.compression);
        assert_eq!(record.encryption, deserialized.encryption);
    }

    #[test]
    fn test_checkpoint_record_serialization() {
        let record = WalRecord::new(
            LogSequenceNumber::from(4),
            RecordData::Checkpoint {
                lsn: LogSequenceNumber::from(100),
                active_txns: vec![
                    TransactionId::from(1),
                    TransactionId::from(2),
                    TransactionId::from(3),
                ],
            },
            CompressionType::None,
            EncryptionType::None,
        );
        let bytes = record.to_bytes(None).unwrap();
        let deserialized = WalRecord::from_bytes(&bytes, None).unwrap();

        assert_eq!(record.lsn, deserialized.lsn);
        assert_eq!(record.data, deserialized.data);
        assert_eq!(record.compression, deserialized.compression);
        assert_eq!(record.encryption, deserialized.encryption);
    }

    #[test]
    fn test_checksum_validation() {
        let record = WalRecord::new(
            LogSequenceNumber::from(1),
            RecordData::Begin {
                txn_id: TransactionId::from(42),
            },
            CompressionType::None,
            EncryptionType::None,
        );
        let mut bytes = record.to_bytes(None).unwrap();

        // Corrupt the data
        bytes[20] ^= 0xFF;

        // Should fail checksum validation
        assert!(WalRecord::from_bytes(&bytes, None).is_err());
    }
}
