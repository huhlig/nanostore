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

//! Integration tests for ValueRef with overflow page chains.
//!
//! Tests cover:
//! - ValueRef creation for different value sizes
//! - Integration with overflow page allocation
//! - Round-trip encoding/decoding with actual page IDs
//! - Size calculations and thresholds

use nanostore::pager::{Pager, PagerConfig};
use nanostore::types::ValueRef;
use nanostore::vfs::MemoryFileSystem;

#[test]
fn test_valueref_inline_usage() {
    // Inline values don't need overflow pages
    let vref = ValueRef::Inline;

    assert!(vref.is_inline());
    assert!(!vref.requires_overflow());
    assert_eq!(vref.size_hint(), None);

    // Encoding should be minimal
    let encoded = vref.encode();
    assert_eq!(encoded.len(), 1);

    // Round-trip
    let decoded = ValueRef::decode(&encoded).unwrap();
    assert_eq!(decoded, vref);
}

#[test]
fn test_valueref_single_page_with_pager() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Allocate a small value that fits in one page
    let test_data = vec![0xAB; 500];
    let page_ids = pager.allocate_overflow_chain(&test_data).unwrap();
    assert_eq!(page_ids.len(), 1);

    // Create ValueRef for single page
    let vref = ValueRef::SinglePage {
        page_id: page_ids[0].as_u64() as u32,
        offset: 0,
        length: test_data.len() as u32,
    };

    assert!(!vref.is_inline());
    assert!(vref.requires_overflow());
    assert_eq!(vref.size_hint(), Some(test_data.len() as u64));

    // Encode and decode
    let encoded = vref.encode();
    assert_eq!(encoded.len(), 11);

    let decoded = ValueRef::decode(&encoded).unwrap();
    assert_eq!(decoded, vref);

    // Verify we can read the data back
    let result = pager.read_overflow_chain(page_ids[0]).unwrap();
    assert_eq!(result, test_data);
}

#[test]
fn test_valueref_overflow_chain_with_pager() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let page_size = config.page_size.data_size();
    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Allocate a large value that requires multiple pages
    let test_data = vec![0xCD; page_size * 10];
    let page_ids = pager.allocate_overflow_chain(&test_data).unwrap();
    assert!(page_ids.len() > 1);

    // Create ValueRef for overflow chain
    let vref = ValueRef::OverflowChain {
        first_page_id: page_ids[0].as_u64() as u32,
        total_length: test_data.len() as u64,
        page_count: page_ids.len() as u32,
    };

    assert!(!vref.is_inline());
    assert!(vref.requires_overflow());
    assert_eq!(vref.size_hint(), Some(test_data.len() as u64));

    // Encode and decode
    let encoded = vref.encode();
    assert_eq!(encoded.len(), 17);

    let decoded = ValueRef::decode(&encoded).unwrap();
    assert_eq!(decoded, vref);

    // Verify we can read the data back
    let result = pager.read_overflow_chain(page_ids[0]).unwrap();
    assert_eq!(result, test_data);
}

#[test]
fn test_valueref_size_thresholds() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Test various sizes and their ValueRef representations
    let test_cases = vec![
        (100, "small inline"),
        (1000, "medium inline"),
        (4000, "large inline/single page"),
        (10000, "multi-page"),
        (100000, "large multi-page"),
    ];

    for (size, description) in test_cases {
        let test_data = vec![0x42; size];
        let page_ids = pager.allocate_overflow_chain(&test_data).unwrap();

        let vref = if page_ids.len() == 1 {
            ValueRef::SinglePage {
                page_id: page_ids[0].as_u64() as u32,
                offset: 0,
                length: size as u32,
            }
        } else {
            ValueRef::OverflowChain {
                first_page_id: page_ids[0].as_u64() as u32,
                total_length: size as u64,
                page_count: page_ids.len() as u32,
            }
        };

        // Verify size hint matches
        assert_eq!(
            vref.size_hint(),
            Some(size as u64),
            "Failed for {}",
            description
        );

        // Verify round-trip
        let encoded = vref.encode();
        let decoded = ValueRef::decode(&encoded).unwrap();
        assert_eq!(decoded, vref, "Round-trip failed for {}", description);

        // Clean up
        pager.free_overflow_chain(page_ids[0]).unwrap();
    }
}

#[test]
fn test_valueref_encoding_sizes() {
    // Verify encoding sizes are as documented

    // Inline: 1 byte
    let inline = ValueRef::Inline;
    assert_eq!(inline.encode().len(), 1);

    // SinglePage: 11 bytes (1 type + 4 page_id + 2 offset + 4 length)
    let single = ValueRef::SinglePage {
        page_id: 12345,
        offset: 256,
        length: 4096,
    };
    assert_eq!(single.encode().len(), 11);

    // OverflowChain: 17 bytes (1 type + 4 page_id + 8 length + 4 count)
    let chain = ValueRef::OverflowChain {
        first_page_id: 999,
        total_length: 1_000_000,
        page_count: 250,
    };
    assert_eq!(chain.encode().len(), 17);
}

#[test]
fn test_valueref_max_values() {
    // Test with maximum field values
    let max_single = ValueRef::SinglePage {
        page_id: u32::MAX,
        offset: u16::MAX,
        length: u32::MAX,
    };

    let encoded = max_single.encode();
    let decoded = ValueRef::decode(&encoded).unwrap();
    assert_eq!(decoded, max_single);

    let max_chain = ValueRef::OverflowChain {
        first_page_id: u32::MAX,
        total_length: u64::MAX,
        page_count: u32::MAX,
    };

    let encoded = max_chain.encode();
    let decoded = ValueRef::decode(&encoded).unwrap();
    assert_eq!(decoded, max_chain);
}

#[test]
fn test_valueref_zero_values() {
    // Test with zero/minimum field values
    let zero_single = ValueRef::SinglePage {
        page_id: 0,
        offset: 0,
        length: 0,
    };

    let encoded = zero_single.encode();
    let decoded = ValueRef::decode(&encoded).unwrap();
    assert_eq!(decoded, zero_single);

    let zero_chain = ValueRef::OverflowChain {
        first_page_id: 0,
        total_length: 0,
        page_count: 0,
    };

    let encoded = zero_chain.encode();
    let decoded = ValueRef::decode(&encoded).unwrap();
    assert_eq!(decoded, zero_chain);
}

#[test]
fn test_valueref_multiple_refs_same_pager() {
    let fs = MemoryFileSystem::new();
    let config = PagerConfig::default();
    let pager = Pager::create(&fs, "test.db", config).unwrap();

    // Create multiple values with different sizes
    let data1 = vec![0x11; 1000];
    let data2 = vec![0x22; 5000];
    let data3 = vec![0x33; 20000];

    let pages1 = pager.allocate_overflow_chain(&data1).unwrap();
    let pages2 = pager.allocate_overflow_chain(&data2).unwrap();
    let pages3 = pager.allocate_overflow_chain(&data3).unwrap();

    // Create ValueRefs
    let vref1 = ValueRef::SinglePage {
        page_id: pages1[0].as_u64() as u32,
        offset: 0,
        length: data1.len() as u32,
    };

    let vref2 = if pages2.len() == 1 {
        ValueRef::SinglePage {
            page_id: pages2[0].as_u64() as u32,
            offset: 0,
            length: data2.len() as u32,
        }
    } else {
        ValueRef::OverflowChain {
            first_page_id: pages2[0].as_u64() as u32,
            total_length: data2.len() as u64,
            page_count: pages2.len() as u32,
        }
    };

    let vref3 = ValueRef::OverflowChain {
        first_page_id: pages3[0].as_u64() as u32,
        total_length: data3.len() as u64,
        page_count: pages3.len() as u32,
    };

    // Encode all refs
    let encoded1 = vref1.encode();
    let encoded2 = vref2.encode();
    let encoded3 = vref3.encode();

    // Decode and verify
    let decoded1 = ValueRef::decode(&encoded1).unwrap();
    let decoded2 = ValueRef::decode(&encoded2).unwrap();
    let decoded3 = ValueRef::decode(&encoded3).unwrap();

    assert_eq!(decoded1, vref1);
    assert_eq!(decoded2, vref2);
    assert_eq!(decoded3, vref3);

    // Verify data can be read back
    let result1 = pager.read_overflow_chain(pages1[0]).unwrap();
    let result2 = pager.read_overflow_chain(pages2[0]).unwrap();
    let result3 = pager.read_overflow_chain(pages3[0]).unwrap();

    assert_eq!(result1, data1);
    assert_eq!(result2, data2);
    assert_eq!(result3, data3);
}

#[test]
fn test_valueref_decode_errors() {
    // Empty bytes
    let result = ValueRef::decode(&[]);
    assert!(result.is_err());

    // Unknown type byte
    let result = ValueRef::decode(&[0xFF]);
    assert!(result.is_err());

    // Invalid length for Inline
    let result = ValueRef::decode(&[0x00, 0x01, 0x02]);
    assert!(result.is_err());

    // Invalid length for SinglePage (too short)
    let result = ValueRef::decode(&[0x01, 0x00, 0x00]);
    assert!(result.is_err());

    // Invalid length for OverflowChain (too short)
    let result = ValueRef::decode(&[0x02, 0x00, 0x00, 0x00, 0x00]);
    assert!(result.is_err());
}

#[test]
fn test_valueref_storage_efficiency() {
    // Verify that ValueRef encoding is space-efficient

    // Inline: just 1 byte overhead
    assert_eq!(ValueRef::Inline.encode().len(), 1);

    // SinglePage: 11 bytes total (reasonable for page reference)
    let single = ValueRef::SinglePage {
        page_id: 100,
        offset: 0,
        length: 10000,
    };
    assert_eq!(single.encode().len(), 11);

    // OverflowChain: 17 bytes total (reasonable for chain metadata)
    let chain = ValueRef::OverflowChain {
        first_page_id: 100,
        total_length: 1_000_000,
        page_count: 250,
    };
    assert_eq!(chain.encode().len(), 17);

    // Compare to storing the actual data
    // For a 1MB value, ValueRef is 17 bytes vs 1,048,576 bytes
    // That's a 61,680x space savings!
}

// Made with Bob
