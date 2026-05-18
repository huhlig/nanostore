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

/// Logical version used by MVCC-capable engines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(u64);

impl Version {
    pub fn as_u64(&self) -> u64 {
        self.0
    }

    pub fn to_bytes(&self) -> [u8; 8] {
        self.0.to_le_bytes()
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Version({})", self.0)
    }
}

impl From<u64> for Version {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

/// Defines whether a bound is inclusive, exclusive, or unbounded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bound<T> {
    Included(T),
    Excluded(T),
    Unbounded,
}

/// Common scan bounds for ordered tables and ordered indexes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanBounds {
    /// Scan the full ordered keyspace.
    All,
    /// Scan keys beginning with the supplied prefix.
    Prefix(KeyBuf),
    /// Scan a bounded range.
    Range {
        start: Bound<KeyBuf>,
        end: Bound<KeyBuf>,
    },
}

/// Durability policy for a write transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    /// Useful for ephemeral in-memory or test engines.
    MemoryOnly,
    /// Write to WAL but do not force the file to stable storage immediately.
    WalOnly,
    /// Flush dirty buffers before reporting commit.
    FlushOnCommit,
    /// Force durable sync before reporting commit.
    SyncOnCommit,
}

/// Transaction isolation level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Serializable,
    SnapshotIsolation,
}

/// Memory pressure level for adaptive eviction.
///
/// Used by memory-aware components to respond to system memory pressure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemoryPressure {
    /// Normal operation, no pressure.
    None,
    /// Mild pressure, consider opportunistic eviction.
    Low,
    /// Moderate pressure, actively evict to stay within budget.
    Medium,
    /// High pressure, aggressively evict to avoid OOM.
    High,
    /// Critical pressure, emergency eviction required.
    Critical,
}

/// Consistency guarantees provided by a storage component.
///
/// This struct documents the ACID properties and crash recovery semantics
/// of a table or database implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsistencyGuarantees {
    /// Operations are atomic (all-or-nothing).
    pub atomicity: bool,
    /// Consistency checks are enforced (constraints, invariants).
    pub consistency: bool,
    /// Transaction isolation level.
    pub isolation: IsolationLevel,
    /// Durability guarantees for committed transactions.
    pub durability: Durability,
    /// Data survives process crashes and can be recovered.
    pub crash_safe: bool,
    /// Supports point-in-time recovery to any committed LSN.
    pub point_in_time_recovery: bool,
}

/// Table/index mutation type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationKind {
    Insert,
    Update,
    Upsert,
    Delete,
    RangeDelete,
}

/// Owned key buffer.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct KeyBuf(pub Vec<u8>);

impl AsRef<[u8]> for KeyBuf {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Owned value buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueBuf(pub Vec<u8>);

impl AsRef<[u8]> for ValueBuf {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Reference to a value that may be stored inline or in overflow pages.
///
/// This enum enables efficient storage of values of varying sizes:
/// - Small values (< max_inline_size) are stored directly in the table
/// - Medium values (< 1 page) use a single overflow page
/// - Large values (>= 1 page) use a linked chain of overflow pages
///
/// The encoding format is optimized for space efficiency:
/// - Inline: No encoding needed (value stored directly)
/// - SinglePage: 11 bytes (1 type + 4 page_id + 2 offset + 4 length)
/// - OverflowChain: 17 bytes (1 type + 4 page_id + 8 length + 4 page_count)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ValueRef {
    /// Value is stored inline in the table structure (no external reference)
    Inline,

    /// Value is stored in a single overflow page
    SinglePage {
        /// Page ID containing the value
        page_id: u32,
        /// Offset within the page where value data starts
        offset: u16,
        /// Length of the value in bytes
        length: u32,
    },

    /// Value is stored across multiple linked overflow pages
    OverflowChain {
        /// First page ID in the chain
        first_page_id: u32,
        /// Total length of the value across all pages
        total_length: u64,
        /// Number of pages in the chain
        page_count: u32,
    },
}

impl ValueRef {
    /// Type byte for Inline variant
    const TYPE_INLINE: u8 = 0x00;
    /// Type byte for SinglePage variant
    const TYPE_SINGLE_PAGE: u8 = 0x01;
    /// Type byte for OverflowChain variant
    const TYPE_OVERFLOW_CHAIN: u8 = 0x02;

    /// Check if this is an inline value reference.
    pub fn is_inline(&self) -> bool {
        matches!(self, ValueRef::Inline)
    }

    /// Get a size hint for the value, if known.
    ///
    /// Returns None for Inline (size depends on actual value),
    /// Some(size) for SinglePage and OverflowChain.
    pub fn size_hint(&self) -> Option<u64> {
        match self {
            ValueRef::Inline => None,
            ValueRef::SinglePage { length, .. } => Some(*length as u64),
            ValueRef::OverflowChain { total_length, .. } => Some(*total_length),
        }
    }

    /// Check if this value requires overflow pages.
    pub fn requires_overflow(&self) -> bool {
        !matches!(self, ValueRef::Inline)
    }

    /// Encode the ValueRef to bytes for storage in tables.
    ///
    /// Format:
    /// - Inline: [0x00] (1 byte)
    /// - SinglePage: [0x01][page_id: u32][offset: u16][length: u32] (11 bytes)
    /// - OverflowChain: [0x02][first_page_id: u32][total_length: u64][page_count: u32] (17 bytes)
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ValueRef::Inline => {
                vec![Self::TYPE_INLINE]
            }
            ValueRef::SinglePage {
                page_id,
                offset,
                length,
            } => {
                let mut buf = Vec::with_capacity(11);
                buf.push(Self::TYPE_SINGLE_PAGE);
                buf.extend_from_slice(&page_id.to_le_bytes());
                buf.extend_from_slice(&offset.to_le_bytes());
                buf.extend_from_slice(&length.to_le_bytes());
                buf
            }
            ValueRef::OverflowChain {
                first_page_id,
                total_length,
                page_count,
            } => {
                let mut buf = Vec::with_capacity(17);
                buf.push(Self::TYPE_OVERFLOW_CHAIN);
                buf.extend_from_slice(&first_page_id.to_le_bytes());
                buf.extend_from_slice(&total_length.to_le_bytes());
                buf.extend_from_slice(&page_count.to_le_bytes());
                buf
            }
        }
    }

    /// Decode a ValueRef from bytes.
    ///
    /// Returns an error if the bytes are invalid or the type byte is unknown.
    pub fn decode(bytes: &[u8]) -> Result<Self, ValueRefDecodeError> {
        if bytes.is_empty() {
            return Err(ValueRefDecodeError::InsufficientBytes {
                expected: 1,
                actual: 0,
            });
        }

        match bytes[0] {
            Self::TYPE_INLINE => {
                if bytes.len() != 1 {
                    return Err(ValueRefDecodeError::InvalidLength {
                        variant: "Inline",
                        expected: 1,
                        actual: bytes.len(),
                    });
                }
                Ok(ValueRef::Inline)
            }
            Self::TYPE_SINGLE_PAGE => {
                if bytes.len() != 11 {
                    return Err(ValueRefDecodeError::InvalidLength {
                        variant: "SinglePage",
                        expected: 11,
                        actual: bytes.len(),
                    });
                }
                let page_id = u32::from_le_bytes(bytes[1..5].try_into().unwrap());
                let offset = u16::from_le_bytes(bytes[5..7].try_into().unwrap());
                let length = u32::from_le_bytes(bytes[7..11].try_into().unwrap());
                Ok(ValueRef::SinglePage {
                    page_id,
                    offset,
                    length,
                })
            }
            Self::TYPE_OVERFLOW_CHAIN => {
                if bytes.len() != 17 {
                    return Err(ValueRefDecodeError::InvalidLength {
                        variant: "OverflowChain",
                        expected: 17,
                        actual: bytes.len(),
                    });
                }
                let first_page_id = u32::from_le_bytes(bytes[1..5].try_into().unwrap());
                let total_length = u64::from_le_bytes(bytes[5..13].try_into().unwrap());
                let page_count = u32::from_le_bytes(bytes[13..17].try_into().unwrap());
                Ok(ValueRef::OverflowChain {
                    first_page_id,
                    total_length,
                    page_count,
                })
            }
            unknown => Err(ValueRefDecodeError::UnknownType(unknown)),
        }
    }
}

/// Errors that can occur when decoding a ValueRef.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueRefDecodeError {
    /// Insufficient bytes to decode
    InsufficientBytes { expected: usize, actual: usize },
    /// Invalid length for variant
    InvalidLength {
        variant: &'static str,
        expected: usize,
        actual: usize,
    },
    /// Unknown type byte
    UnknownType(u8),
}

impl std::fmt::Display for ValueRefDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsufficientBytes { expected, actual } => {
                write!(
                    f,
                    "Insufficient bytes: expected at least {}, got {}",
                    expected, actual
                )
            }
            Self::InvalidLength {
                variant,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "Invalid length for {}: expected {}, got {}",
                    variant, expected, actual
                )
            }
            Self::UnknownType(type_byte) => {
                write!(f, "Unknown ValueRef type byte: 0x{:02X}", type_byte)
            }
        }
    }
}

impl std::error::Error for ValueRefDecodeError {}

/// A key-value entry returned by owned iterators or batch operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: KeyBuf,
    pub value: ValueBuf,
}

/// Key encoding strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum KeyEncoding {
    RawBytes,
    LexicographicTuple,
    BigEndianInteger,
    Utf8,
    TimestampMicros,
    Custom(u32),
}

/// Compression algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CompressionKind {
    None,
    Lz4,
    Zstd,
    Snappy,
    Custom(u32),
}

/// Encryption algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EncryptionKind {
    None,
    AesGcm,
    ChaCha20Poly1305,
    Custom(u32),
}

/// Unified object identifier for tables and indexes at the transaction/storage layer.
/// Provides type-safe conversion from TableId and IndexId while maintaining a unified
/// representation for transaction operations.
#[derive(
    Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct TableId(u64);

impl TableId {
    pub fn as_u64(&self) -> u64 {
        self.0
    }

    pub fn to_bytes(&self) -> [u8; 8] {
        self.0.to_le_bytes()
    }
}

impl std::fmt::Display for TableId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ObjectId({})", self.0)
    }
}

impl From<u64> for TableId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valueref_inline() {
        let vref = ValueRef::Inline;
        assert!(vref.is_inline());
        assert!(!vref.requires_overflow());
        assert_eq!(vref.size_hint(), None);
    }

    #[test]
    fn test_valueref_single_page() {
        let vref = ValueRef::SinglePage {
            page_id: 42,
            offset: 100,
            length: 1024,
        };
        assert!(!vref.is_inline());
        assert!(vref.requires_overflow());
        assert_eq!(vref.size_hint(), Some(1024));
    }

    #[test]
    fn test_valueref_overflow_chain() {
        let vref = ValueRef::OverflowChain {
            first_page_id: 100,
            total_length: 1_000_000,
            page_count: 250,
        };
        assert!(!vref.is_inline());
        assert!(vref.requires_overflow());
        assert_eq!(vref.size_hint(), Some(1_000_000));
    }

    #[test]
    fn test_valueref_encode_decode_inline() {
        let original = ValueRef::Inline;
        let encoded = original.encode();
        assert_eq!(encoded.len(), 1);
        assert_eq!(encoded[0], 0x00);

        let decoded = ValueRef::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_valueref_encode_decode_single_page() {
        let original = ValueRef::SinglePage {
            page_id: 12345,
            offset: 256,
            length: 4096,
        };
        let encoded = original.encode();
        assert_eq!(encoded.len(), 11);
        assert_eq!(encoded[0], 0x01);

        let decoded = ValueRef::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_valueref_encode_decode_overflow_chain() {
        let original = ValueRef::OverflowChain {
            first_page_id: 999,
            total_length: 10_000_000,
            page_count: 2500,
        };
        let encoded = original.encode();
        assert_eq!(encoded.len(), 17);
        assert_eq!(encoded[0], 0x02);

        let decoded = ValueRef::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_valueref_decode_empty_bytes() {
        let result = ValueRef::decode(&[]);
        assert!(matches!(
            result,
            Err(ValueRefDecodeError::InsufficientBytes {
                expected: 1,
                actual: 0
            })
        ));
    }

    #[test]
    fn test_valueref_decode_unknown_type() {
        let result = ValueRef::decode(&[0xFF]);
        assert!(matches!(
            result,
            Err(ValueRefDecodeError::UnknownType(0xFF))
        ));
    }

    #[test]
    fn test_valueref_decode_invalid_inline_length() {
        let result = ValueRef::decode(&[0x00, 0x01, 0x02]);
        assert!(matches!(
            result,
            Err(ValueRefDecodeError::InvalidLength {
                variant: "Inline",
                expected: 1,
                actual: 3
            })
        ));
    }

    #[test]
    fn test_valueref_decode_invalid_single_page_length() {
        let result = ValueRef::decode(&[0x01, 0x00, 0x00]);
        assert!(matches!(
            result,
            Err(ValueRefDecodeError::InvalidLength {
                variant: "SinglePage",
                expected: 11,
                actual: 3
            })
        ));
    }

    #[test]
    fn test_valueref_decode_invalid_overflow_chain_length() {
        let result = ValueRef::decode(&[0x02, 0x00, 0x00, 0x00, 0x00]);
        assert!(matches!(
            result,
            Err(ValueRefDecodeError::InvalidLength {
                variant: "OverflowChain",
                expected: 17,
                actual: 5
            })
        ));
    }

    #[test]
    fn test_valueref_round_trip_all_variants() {
        let test_cases = vec![
            ValueRef::Inline,
            ValueRef::SinglePage {
                page_id: 0,
                offset: 0,
                length: 0,
            },
            ValueRef::SinglePage {
                page_id: u32::MAX,
                offset: u16::MAX,
                length: u32::MAX,
            },
            ValueRef::OverflowChain {
                first_page_id: 0,
                total_length: 0,
                page_count: 0,
            },
            ValueRef::OverflowChain {
                first_page_id: u32::MAX,
                total_length: u64::MAX,
                page_count: u32::MAX,
            },
        ];

        for original in test_cases {
            let encoded = original.encode();
            let decoded = ValueRef::decode(&encoded).unwrap();
            assert_eq!(decoded, original, "Round-trip failed for {:?}", original);
        }
    }

    #[test]
    fn test_valueref_encoding_sizes() {
        assert_eq!(ValueRef::Inline.encode().len(), 1);
        assert_eq!(
            ValueRef::SinglePage {
                page_id: 1,
                offset: 0,
                length: 100
            }
            .encode()
            .len(),
            11
        );
        assert_eq!(
            ValueRef::OverflowChain {
                first_page_id: 1,
                total_length: 1000,
                page_count: 10
            }
            .encode()
            .len(),
            17
        );
    }

    #[test]
    fn test_valueref_decode_error_display() {
        let err1 = ValueRefDecodeError::InsufficientBytes {
            expected: 10,
            actual: 5,
        };
        assert_eq!(
            err1.to_string(),
            "Insufficient bytes: expected at least 10, got 5"
        );

        let err2 = ValueRefDecodeError::InvalidLength {
            variant: "SinglePage",
            expected: 11,
            actual: 5,
        };
        assert_eq!(
            err2.to_string(),
            "Invalid length for SinglePage: expected 11, got 5"
        );

        let err3 = ValueRefDecodeError::UnknownType(0xAB);
        assert_eq!(err3.to_string(), "Unknown ValueRef type byte: 0xAB");
    }
}
