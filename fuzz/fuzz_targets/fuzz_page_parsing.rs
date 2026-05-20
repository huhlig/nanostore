#![no_main]

use libfuzzer_sys::fuzz_target;
use nanostore::pager::page::{Page, PageHeader, PageType};

fuzz_target!(|data: &[u8]| {
    // Skip if data is too small to be interesting
    if data.is_empty() {
        return;
    }

    // Test 1: Try to parse page header from arbitrary bytes
    if data.len() >= PageHeader::SIZE {
        let _ = PageHeader::from_bytes(data);
    }

    // Test 2: Try to parse full page with checksum verification disabled
    // This tests decompression and decryption edge cases
    if data.len() >= PageHeader::SIZE + Page::CHECKSUM_SIZE {
        let _ = Page::from_bytes(data, false, None);
    }

    // Test 3: Try to parse full page with checksum verification enabled
    // This tests checksum validation with malformed data
    if data.len() >= PageHeader::SIZE + Page::CHECKSUM_SIZE {
        let _ = Page::from_bytes(data, true, None);
    }

    // Test 4: Try with encryption key (tests decryption edge cases)
    if data.len() >= 32 {
        let key: [u8; 32] = data[0..32].try_into().unwrap();
        if data.len() >= PageHeader::SIZE + Page::CHECKSUM_SIZE + 32 {
            let _ = Page::from_bytes(&data[32..], false, Some(&key));
            let _ = Page::from_bytes(&data[32..], true, Some(&key));
        }
    }

    // Test 5: Create a valid page and try to serialize with fuzzy compression/encryption settings
    // This tests edge cases in the serialization path
    if data.len() >= 8 {
        let page_id = u64::from_le_bytes(data[0..8].try_into().unwrap());
        let mut page = Page::new(page_id.into(), PageType::BTreeLeaf, 100);
        
        // Add some of the fuzz data as page content
        let content_len = data.len().min(1000);
        page.data.extend_from_slice(&data[..content_len]);

        // Try to serialize with different page sizes
        for page_size in [512, 1024, 2048, 4096, 8192, 16384, 32768, 65536] {
            let _ = page.to_bytes(page_size, None);
        }

        // Try with encryption key if we have enough data
        if data.len() >= 32 {
            let key: [u8; 32] = data[0..32].try_into().unwrap();
            let _ = page.to_bytes(4096, Some(&key));
        }
    }

    // Test 6: Test overflow page header parsing
    if data.len() >= 32 {
        use nanostore::pager::page::OverflowPageHeader;
        let _ = OverflowPageHeader::from_bytes(data);
    }

    // Test 7: Test page type conversion with arbitrary bytes
    if !data.is_empty() {
        let _ = PageType::from_u8(data[0]);
    }

    // Test 8: Test truncated inputs at various boundaries
    for truncate_at in [1, 8, 16, 31, 32, 64, 128, 256] {
        if data.len() > truncate_at {
            let _ = PageHeader::from_bytes(&data[..truncate_at]);
            let _ = Page::from_bytes(&data[..truncate_at], false, None);
        }
    }
});

// Made with Bob
