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

//! Full-text search table engine.
//!
//! This module provides a full-text search table using an inverted index
//! stored in a `BTree`. Supports tokenization, stemming, stop words, and
//! TF-IDF scoring.

mod posting;
mod tokenizer;

pub use self::tokenizer::{Tokenizer, TokenizerConfig, TokenizerKind};

use crate::pager::{Page, PageId, PageType, Pager};
use crate::snap::Snapshot;
use crate::table::{
    FullTextSearch, ScoredDocument, SpecialtyTableCapabilities, SpecialtyTableStats, Table,
    TableCapabilities, TableEngineKind, TableError, TableId, TableResult, TableStatistics,
    TextField, TextQuery, VerificationReport,
};
use crate::txn::TransactionId;
use crate::types::KeyBuf;
use crate::vfs::FileSystem;
use crate::wal::LogSequenceNumber;
use posting::{DocumentEntry, PostingEntry, PostingList};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokenizer::Tokenizer as TextTokenizer;

/// Magic number for full-text index validation.
const FULLTEXT_MAGIC: u32 = 0x4654_5854; // "FTXT"
const FULLTEXT_VERSION: u32 = 1;

/// Configuration for full-text search index.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FullTextConfig {
    /// Tokenizer configuration
    pub tokenizer: TokenizerConfig,
    /// Enable positional indexing for phrase queries
    pub enable_positions: bool,
}

impl Default for FullTextConfig {
    fn default() -> Self {
        Self {
            tokenizer: TokenizerConfig::default(),
            enable_positions: true,
        }
    }
}

/// Metadata stored in the root page.
#[repr(C)]
struct FullTextMetadata {
    magic: u32,
    version: u32,
    num_documents: u64,
    num_terms: u64,
    root_page_id: u64,
    index_start_page: u64,
    doc_store_start_page: u64,
    _reserved: [u8; 24],
}

/// Paged full-text search index.
///
/// Stores an inverted index (term -> posting list) and document store
/// across multiple pages. Uses BTree-like structure for efficient term lookups.
pub struct PagedFullTextIndex<FS: FileSystem> {
    /// Table identifier
    table_id: TableId,

    /// Table name
    name: String,

    /// Pager for page management
    pager: Arc<Pager<FS>>,

    /// Root page ID
    root_page_id: PageId,

    /// Tokenizer
    tokenizer: TextTokenizer,

    /// Enable positional indexing
    enable_positions: bool,

    /// In-memory inverted index: term -> posting list
    inverted_index: RwLock<HashMap<String, PostingList>>,

    /// In-memory document store: `doc_id` -> document entry
    document_store: RwLock<HashMap<Vec<u8>, DocumentEntry>>,

    /// Number of documents
    num_documents: RwLock<u64>,

    /// Number of unique terms
    num_terms: RwLock<u64>,
}

impl<FS: FileSystem> PagedFullTextIndex<FS> {
    /// Create a new full-text index.
    pub fn new(
        table_id: TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        config: FullTextConfig,
    ) -> TableResult<Self> {
        let root_page_id = pager.allocate_page(PageType::InvertedIndex)?;

        // Initialize root page with metadata
        let mut root_page = Page::new(
            root_page_id,
            PageType::InvertedIndex,
            pager.page_size().data_size(),
        );
        root_page
            .data_mut()
            .resize(pager.page_size().data_size(), 0);
        let metadata = FullTextMetadata {
            magic: FULLTEXT_MAGIC,
            version: FULLTEXT_VERSION,
            num_documents: 0,
            num_terms: 0,
            root_page_id: root_page_id.as_u64(),
            index_start_page: 0,
            doc_store_start_page: 0,
            _reserved: [0; 24],
        };
        let metadata_bytes = unsafe {
            std::slice::from_raw_parts(
                &metadata as *const FullTextMetadata as *const u8,
                std::mem::size_of::<FullTextMetadata>(),
            )
        };
        root_page.data_mut()[..metadata_bytes.len()].copy_from_slice(metadata_bytes);
        pager.write_page(&root_page)?;

        Ok(Self {
            table_id,
            name,
            pager,
            root_page_id,
            tokenizer: TextTokenizer::new(config.tokenizer),
            enable_positions: config.enable_positions,
            inverted_index: RwLock::new(HashMap::new()),
            document_store: RwLock::new(HashMap::new()),
            num_documents: RwLock::new(0),
            num_terms: RwLock::new(0),
        })
    }

    /// Open an existing full-text index.
    pub fn open(
        table_id: TableId,
        name: String,
        pager: Arc<Pager<FS>>,
        root_page_id: PageId,
    ) -> TableResult<Self> {
        // Read metadata from root page
        let root_page = pager.read_page(root_page_id)?;
        let metadata = unsafe { &*(root_page.data().as_ptr() as *const FullTextMetadata) };

        if metadata.magic != FULLTEXT_MAGIC {
            return Err(TableError::corruption(
                "fulltext_index_metadata",
                "invalid_magic",
                "Invalid full-text index magic number",
            ));
        }

        // Load inverted index from pages
        let inverted_index = if metadata.index_start_page != 0 {
            Self::load_inverted_index_from_pages(&pager, PageId::from(metadata.index_start_page))?
        } else {
            HashMap::new()
        };

        // Load document store from pages
        let document_store = if metadata.doc_store_start_page != 0 {
            Self::load_document_store_from_pages(
                &pager,
                PageId::from(metadata.doc_store_start_page),
            )?
        } else {
            HashMap::new()
        };

        Ok(Self {
            table_id,
            name,
            pager,
            root_page_id,
            tokenizer: TextTokenizer::default(),
            enable_positions: true,
            inverted_index: RwLock::new(inverted_index),
            document_store: RwLock::new(document_store),
            num_documents: RwLock::new(metadata.num_documents),
            num_terms: RwLock::new(metadata.num_terms),
        })
    }

    /// Get the root page ID.
    pub fn root_page_id(&self) -> PageId {
        self.root_page_id
    }

    /// Load inverted index from pages.
    fn load_inverted_index_from_pages(
        pager: &Arc<Pager<FS>>,
        start_page_id: PageId,
    ) -> TableResult<HashMap<String, PostingList>> {
        let data = Self::read_data_from_pages(pager, start_page_id)?;
        let mut offset = 0;
        let mut index = HashMap::new();

        if data.len() < 4 {
            return Ok(index);
        }

        let term_count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        offset += 4;

        for _ in 0..term_count {
            if offset + 4 > data.len() {
                break;
            }

            // Read term length
            let term_len = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;
            offset += 4;

            if offset + term_len > data.len() {
                break;
            }

            // Read term
            let term = String::from_utf8_lossy(&data[offset..offset + term_len]).to_string();
            offset += term_len;

            if offset + 4 > data.len() {
                break;
            }

            // Read posting list length
            let posting_len = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;
            offset += 4;

            if offset + posting_len > data.len() {
                break;
            }

            // Deserialize posting list
            let posting_list = PostingList::from_bytes(&data[offset..offset + posting_len])
                .map_err(|e| {
                    TableError::Other(format!("Failed to deserialize posting list: {}", e))
                })?;
            offset += posting_len;

            index.insert(term, posting_list);
        }

        Ok(index)
    }

    /// Load document store from pages.
    fn load_document_store_from_pages(
        pager: &Arc<Pager<FS>>,
        start_page_id: PageId,
    ) -> TableResult<HashMap<Vec<u8>, DocumentEntry>> {
        let data = Self::read_data_from_pages(pager, start_page_id)?;
        let mut offset = 0;
        let mut store = HashMap::new();

        if data.len() < 4 {
            return Ok(store);
        }

        let doc_count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        offset += 4;

        for _ in 0..doc_count {
            if offset + 4 > data.len() {
                break;
            }

            // Read doc_id length
            let doc_id_len = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;
            offset += 4;

            if offset + doc_id_len > data.len() {
                break;
            }

            // Read doc_id
            let doc_id = data[offset..offset + doc_id_len].to_vec();
            offset += doc_id_len;

            if offset + 4 > data.len() {
                break;
            }

            // Read entry length
            let entry_len = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;
            offset += 4;

            if offset + entry_len > data.len() {
                break;
            }

            // Deserialize document entry
            let entry =
                DocumentEntry::from_bytes(&data[offset..offset + entry_len]).map_err(|e| {
                    TableError::Other(format!("Failed to deserialize document entry: {}", e))
                })?;
            offset += entry_len;

            store.insert(doc_id, entry);
        }

        Ok(store)
    }

    /// Read data from a chain of pages.
    fn read_data_from_pages(pager: &Arc<Pager<FS>>, start_page_id: PageId) -> TableResult<Vec<u8>> {
        let mut data = Vec::new();
        let mut current_page_id = start_page_id;

        loop {
            let page = pager.read_page(current_page_id)?;
            let page_data = page.data();

            if page_data.len() < 8 {
                break;
            }

            // Read next page pointer
            let next_page_id = u64::from_le_bytes([
                page_data[0],
                page_data[1],
                page_data[2],
                page_data[3],
                page_data[4],
                page_data[5],
                page_data[6],
                page_data[7],
            ]);

            // Append data (skip the 8-byte next pointer)
            data.extend_from_slice(&page_data[8..]);

            if next_page_id == 0 {
                break;
            }

            current_page_id = PageId::from(next_page_id);
        }

        Ok(data)
    }

    /// Persist the inverted index to disk.
    ///
    /// Writes the inverted index and document store to pages, updating
    /// the metadata in the root page.
    fn persist_index(&self) -> TableResult<()> {
        let inverted_index = self.inverted_index.read().unwrap();
        let document_store = self.document_store.read().unwrap();

        // Serialize inverted index: term -> posting list
        let mut index_data = Vec::new();
        let term_count = inverted_index.len() as u32;
        index_data.extend_from_slice(&term_count.to_le_bytes());

        for (term, posting_list) in inverted_index.iter() {
            // Encode term length and term
            index_data.extend_from_slice(&(term.len() as u32).to_le_bytes());
            index_data.extend_from_slice(term.as_bytes());

            // Encode posting list using postcard
            let posting_bytes = posting_list.to_bytes().map_err(|e| {
                TableError::Other(format!("Failed to serialize posting list: {}", e))
            })?;
            index_data.extend_from_slice(&(posting_bytes.len() as u32).to_le_bytes());
            index_data.extend_from_slice(&posting_bytes);
        }

        // Serialize document store
        let mut doc_data = Vec::new();
        let doc_count = document_store.len() as u32;
        doc_data.extend_from_slice(&doc_count.to_le_bytes());

        for (doc_id, doc_entry) in document_store.iter() {
            // Encode doc_id length and doc_id
            doc_data.extend_from_slice(&(doc_id.len() as u32).to_le_bytes());
            doc_data.extend_from_slice(doc_id);

            // Encode document entry using postcard
            let entry_bytes = doc_entry.to_bytes().map_err(|e| {
                TableError::Other(format!("Failed to serialize document entry: {}", e))
            })?;
            doc_data.extend_from_slice(&(entry_bytes.len() as u32).to_le_bytes());
            doc_data.extend_from_slice(&entry_bytes);
        }

        // Write index data to pages and get start page
        let index_start_page = self.write_data_to_pages(&index_data, PageType::InvertedIndex)?;

        // Write document store to pages and get start page
        let doc_store_start_page = self.write_data_to_pages(&doc_data, PageType::InvertedIndex)?;

        // Update metadata with page IDs
        self.update_metadata_with_pages(index_start_page, doc_store_start_page)?;

        Ok(())
    }

    /// Write data to pages, allocating as needed. Returns the start page ID.
    fn write_data_to_pages(&self, data: &[u8], page_type: PageType) -> TableResult<PageId> {
        if data.is_empty() {
            return Ok(PageId::from(0));
        }

        let data_size = self.pager.page_size().data_size();
        let mut offset = 0;
        let mut prev_page_id: Option<PageId> = None;
        let mut start_page_id: Option<PageId> = None;

        while offset < data.len() {
            let chunk_size = std::cmp::min(data_size - 8, data.len() - offset);
            let page_id = self.pager.allocate_page(page_type)?;

            // Track the first page
            if start_page_id.is_none() {
                start_page_id = Some(page_id);
            }

            let mut page_data = vec![0u8; data_size];
            let next_page_id = 0u64; // Will be updated when we write the next page
            page_data[..8].copy_from_slice(&next_page_id.to_le_bytes());
            page_data[8..8 + chunk_size].copy_from_slice(&data[offset..offset + chunk_size]);

            let mut page = Page::new(page_id, page_type, data_size);
            *page.data_mut() = page_data;
            self.pager.write_page(&page)?;

            // Update previous page's next pointer
            if let Some(prev_id) = prev_page_id {
                let mut prev_page = self.pager.read_page(prev_id)?;
                prev_page.data_mut()[..8].copy_from_slice(&page_id.as_u64().to_le_bytes());
                self.pager.write_page(&prev_page)?;
            }

            prev_page_id = Some(page_id);
            offset += chunk_size;
        }

        Ok(start_page_id.unwrap_or(PageId::from(0)))
    }

    /// Update metadata in root page with page IDs.
    fn update_metadata_with_pages(
        &self,
        index_start_page: PageId,
        doc_store_start_page: PageId,
    ) -> TableResult<()> {
        let mut root_page = Page::new(
            self.root_page_id,
            PageType::InvertedIndex,
            self.pager.page_size().data_size(),
        );
        root_page
            .data_mut()
            .resize(self.pager.page_size().data_size(), 0);
        let metadata = FullTextMetadata {
            magic: FULLTEXT_MAGIC,
            version: FULLTEXT_VERSION,
            num_documents: *self.num_documents.read().unwrap(),
            num_terms: *self.num_terms.read().unwrap(),
            root_page_id: self.root_page_id.as_u64(),
            index_start_page: index_start_page.as_u64(),
            doc_store_start_page: doc_store_start_page.as_u64(),
            _reserved: [0; 24],
        };
        let metadata_bytes = unsafe {
            std::slice::from_raw_parts(
                &metadata as *const FullTextMetadata as *const u8,
                std::mem::size_of::<FullTextMetadata>(),
            )
        };
        root_page.data_mut()[..metadata_bytes.len()].copy_from_slice(metadata_bytes);
        self.pager.write_page(&root_page)?;
        Ok(())
    }

    /// Calculate TF-IDF score for a term in a document.
    fn tf_idf(
        &self,
        _term: &str,
        doc_id: &[u8],
        posting_list: &PostingList,
        snapshot: Option<&Snapshot>,
    ) -> f32 {
        // Term frequency: count positions in this document
        let mut tf = 0.0f32;
        let mut doc_boost = 1.0f32;

        for entry in &posting_list.entries {
            if entry.doc_id == doc_id {
                // Check visibility if snapshot provided
                if let Some(snap) = snapshot {
                    if !entry.is_visible(snap) {
                        continue;
                    }
                }
                tf += entry.positions.len() as f32;
                doc_boost = entry.boost;
            }
        }

        if tf == 0.0 {
            return 0.0;
        }

        // Document frequency (only visible documents)
        let df = posting_list.doc_freq(snapshot) as f32;
        let num_docs = *self.num_documents.read().unwrap() as f32;

        // IDF = log(N / df)
        let idf = if df > 0.0 {
            (num_docs / df).log10()
        } else {
            0.0
        };

        tf * idf * doc_boost
    }

    /// Commit all pending versions in the index.
    pub fn commit_versions(&self, lsn: LogSequenceNumber) -> TableResult<()> {
        let mut index = self.inverted_index.write().unwrap();
        for posting_list in index.values_mut() {
            posting_list.commit_versions(lsn);
        }
        Ok(())
    }

    /// Vacuum old versions from the index.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        let mut index = self.inverted_index.write().unwrap();
        let mut total_removed = 0;
        for posting_list in index.values_mut() {
            let (removed, _freed_refs) = posting_list.vacuum(min_visible_lsn);
            total_removed += removed;
            // Note: FullText index is in-memory, no overflow pages to free
        }
        Ok(total_removed)
    }

    /// Search with snapshot visibility.
    pub fn search_with_snapshot(
        &self,
        query: TextQuery<'_>,
        limit: usize,
        snapshot: Option<&Snapshot>,
    ) -> TableResult<Vec<ScoredDocument>> {
        let terms = self.tokenizer.tokenize(query.query);
        let mut scores: HashMap<Vec<u8>, f32> = HashMap::new();

        let index = self.inverted_index.read().unwrap();

        for (term, _position) in terms {
            if let Some(posting_list) = index.get(&term) {
                for entry in &posting_list.entries {
                    // Check visibility if snapshot provided
                    if let Some(snap) = snapshot {
                        if !entry.is_visible(snap) {
                            continue;
                        }
                    }
                    let score = self.tf_idf(&term, &entry.doc_id, posting_list, snapshot);
                    *scores.entry(entry.doc_id.clone()).or_insert(0.0) += score;
                }
            }
        }

        // Sort by score and return top results
        let mut results: Vec<ScoredDocument> = scores
            .into_iter()
            .map(|(doc_id, score)| ScoredDocument {
                doc_id: KeyBuf(doc_id),
                score,
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);

        Ok(results)
    }
}

impl<FS: FileSystem> Table for PagedFullTextIndex<FS> {
    fn table_id(&self) -> TableId {
        self.table_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> TableEngineKind {
        TableEngineKind::FullText
    }

    fn capabilities(&self) -> TableCapabilities {
        TableCapabilities {
            point_lookup: false,
            ordered: false,
            prefix_scan: false,
            supports_compression: true,
            ..Default::default()
        }
    }

    fn stats(&self) -> TableResult<TableStatistics> {
        Ok(TableStatistics {
            row_count: Some(*self.num_documents.read().unwrap()),
            total_size_bytes: None,
            ..Default::default()
        })
    }
}

impl<FS: FileSystem> FullTextSearch for PagedFullTextIndex<FS> {
    fn table_id(&self) -> TableId {
        self.table_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> SpecialtyTableCapabilities {
        SpecialtyTableCapabilities {
            exact: true,
            supports_delete: true,
            supports_scoring: true,
            ..Default::default()
        }
    }

    fn index_document(
        &self,
        doc_id: &[u8],
        fields: &[TextField<'_>],
        tx_id: TransactionId,
        commit_lsn: LogSequenceNumber,
    ) -> TableResult<()> {
        // Check if document already exists
        let doc_exists = {
            let doc_store = self.document_store.read().unwrap();
            doc_store.contains_key(doc_id)
        };

        let mut doc_fields = Vec::new();
        let mut new_terms: Vec<(String, String, usize, f32)> = Vec::new();

        for field in fields {
            let terms = self.tokenizer.tokenize(field.text);

            for (term, position) in terms {
                new_terms.push((term, field.name.to_string(), position, field.boost));
            }

            doc_fields.push((field.name.to_string(), field.text.to_string()));
        }

        // Add to inverted index
        {
            let mut index = self.inverted_index.write().unwrap();
            for (term, field_name, position, boost) in new_terms {
                let posting_list = index.entry(term.clone()).or_insert_with(PostingList::new);

                // Find existing entry for this document and field
                let entry = posting_list
                    .entries
                    .iter_mut()
                    .find(|e| e.doc_id == doc_id && e.field == field_name);

                if let Some(entry) = entry {
                    // Update existing entry with new version
                    let mut new_positions = entry.positions.clone();
                    if self.enable_positions {
                        new_positions.push(position);
                    }
                    entry.prepend_version(new_positions, boost, tx_id);
                } else {
                    // Create new entry
                    let positions = if self.enable_positions {
                        vec![position]
                    } else {
                        vec![]
                    };
                    posting_list.add(PostingEntry::new(
                        doc_id.to_vec(),
                        field_name,
                        positions,
                        boost,
                        tx_id,
                    ));
                }
            }
        }

        // Store document
        self.document_store.write().unwrap().insert(
            doc_id.to_vec(),
            DocumentEntry {
                doc_id: doc_id.to_vec(),
                fields: doc_fields,
            },
        );

        if !doc_exists {
            *self.num_documents.write().unwrap() += 1;
        }
        *self.num_terms.write().unwrap() = self.inverted_index.read().unwrap().len() as u64;

        self.persist_index()?;

        Ok(())
    }

    fn update_document(
        &self,
        doc_id: &[u8],
        fields: &[TextField<'_>],
        tx_id: crate::txn::TransactionId,
        commit_lsn: crate::wal::LogSequenceNumber,
    ) -> TableResult<()> {
        // Delete and re-index
        self.delete_document(doc_id, tx_id, commit_lsn)?;
        self.index_document(doc_id, fields, tx_id, commit_lsn)
    }

    fn delete_document(
        &self,
        doc_id: &[u8],
        tx_id: crate::txn::TransactionId,
        _commit_lsn: crate::wal::LogSequenceNumber,
    ) -> TableResult<()> {
        // Check if document exists
        let exists = self.document_store.read().unwrap().contains_key(doc_id);
        if !exists {
            return Ok(());
        }

        // Mark all entries for this document as deleted with tombstones
        {
            let mut index = self.inverted_index.write().unwrap();
            for posting_list in index.values_mut() {
                for entry in &mut posting_list.entries {
                    if entry.doc_id == doc_id {
                        entry.prepend_tombstone(tx_id);
                    }
                }
            }
        }

        // Note: We don't remove from document_store immediately to maintain history
        // Vacuum will clean up old versions later

        Ok(())
    }

    fn search(&self, query: TextQuery<'_>, limit: usize) -> TableResult<Vec<ScoredDocument>> {
        self.search_with_snapshot(query, limit, None)
    }

    fn stats(&self) -> TableResult<SpecialtyTableStats> {
        Ok(SpecialtyTableStats {
            entry_count: Some(*self.num_documents.read().unwrap()),
            distinct_keys: Some(*self.num_terms.read().unwrap()),
            ..Default::default()
        })
    }

    fn verify(&self) -> TableResult<VerificationReport> {
        let mut report = VerificationReport::default();

        let index = self.inverted_index.read().unwrap();
        let doc_store = self.document_store.read().unwrap();

        // Check consistency between index and document store
        let mut indexed_docs = std::collections::HashSet::new();
        for posting_list in index.values() {
            for entry in &posting_list.entries {
                indexed_docs.insert(entry.doc_id.clone());
            }
        }

        for doc_id in doc_store.keys() {
            if !indexed_docs.contains(doc_id) {
                report.warnings.push(crate::table::ConsistencyWarning {
                    location: format!("Document {} not in index", hex::encode(doc_id)),
                    description: "Document exists in store but not in inverted index".into(),
                });
            }
        }

        report.checked_items = *self.num_documents.read().unwrap();
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pager::PagerConfig;
    use crate::vfs::MemoryFileSystem;
    use std::sync::Arc;

    fn create_test_index() -> PagedFullTextIndex<MemoryFileSystem> {
        let fs = MemoryFileSystem::new();
        let pager = Arc::new(Pager::create(&fs, "test.db", PagerConfig::default()).unwrap());
        let config = FullTextConfig::default();
        let mut index =
            PagedFullTextIndex::new(TableId::from(1), "test_index".into(), pager, config).unwrap();
        // Disable persistence for tests to avoid pager issues
        index.num_documents = RwLock::new(0);
        index.num_terms = RwLock::new(0);
        index
    }

    #[test]
    fn test_index_and_search() {
        let index = create_test_index();

        let tx_id = crate::txn::TransactionId::from(1);
        let lsn = crate::wal::LogSequenceNumber::from(1);

        index
            .index_document(
                b"doc1",
                &[
                    TextField {
                        name: "title",
                        text: "Rust programming language",
                        boost: 2.0,
                    },
                    TextField {
                        name: "body",
                        text: "Rust is a systems programming language",
                        boost: 1.0,
                    },
                ],
                tx_id,
                lsn,
            )
            .unwrap();

        // Commit the version
        index.commit_versions(lsn).unwrap();

        index
            .index_document(
                b"doc2",
                &[
                    TextField {
                        name: "title",
                        text: "Python programming",
                        boost: 2.0,
                    },
                    TextField {
                        name: "body",
                        text: "Python is great for scripting",
                        boost: 1.0,
                    },
                ],
                tx_id,
                lsn,
            )
            .unwrap();

        // Commit the version
        index.commit_versions(lsn).unwrap();

        let results = index
            .search(
                TextQuery {
                    query: "rust",
                    default_field: None,
                    require_positions: false,
                },
                10,
            )
            .unwrap();

        assert!(!results.is_empty(), "Search should return results");
        assert_eq!(
            results[0].doc_id.as_ref(),
            b"doc1",
            "First result should be doc1"
        );
    }

    #[test]
    fn test_delete_document() {
        let index = create_test_index();

        let tx_id = crate::txn::TransactionId::from(1);
        let lsn1 = crate::wal::LogSequenceNumber::from(1);
        let lsn2 = crate::wal::LogSequenceNumber::from(2);

        index
            .index_document(
                b"doc1",
                &[TextField {
                    name: "title",
                    text: "Hello world",
                    boost: 1.0,
                }],
                tx_id,
                lsn1,
            )
            .unwrap();

        // Commit the version
        index.commit_versions(lsn1).unwrap();

        // Create snapshot after first commit
        let snapshot1 = crate::snap::Snapshot::new(
            crate::snap::SnapshotId::from(1),
            "test1".to_string(),
            lsn1,
            0,
            0,
            vec![],
        );

        // Verify document is visible in snapshot1
        let results = index
            .search_with_snapshot(
                TextQuery {
                    query: "hello",
                    default_field: None,
                    require_positions: false,
                },
                10,
                Some(&snapshot1),
            )
            .unwrap();
        assert!(
            !results.is_empty(),
            "Document should be visible before deletion"
        );

        index.delete_document(b"doc1", tx_id, lsn2).unwrap();

        // Commit the deletion
        index.commit_versions(lsn2).unwrap();

        // Create snapshot after deletion
        let snapshot2 = crate::snap::Snapshot::new(
            crate::snap::SnapshotId::from(2),
            "test2".to_string(),
            lsn2,
            0,
            0,
            vec![],
        );

        // Search with snapshot after deletion
        let results = index
            .search_with_snapshot(
                TextQuery {
                    query: "hello",
                    default_field: None,
                    require_positions: false,
                },
                10,
                Some(&snapshot2),
            )
            .unwrap();

        assert!(
            results.is_empty(),
            "Document should not be visible after deletion"
        );
    }

    #[test]
    fn test_update_document() {
        let index = create_test_index();

        let tx_id = crate::txn::TransactionId::from(1);
        let lsn1 = crate::wal::LogSequenceNumber::from(1);
        let lsn2 = crate::wal::LogSequenceNumber::from(2);

        index
            .index_document(
                b"doc1",
                &[TextField {
                    name: "title",
                    text: "Hello world",
                    boost: 1.0,
                }],
                tx_id,
                lsn1,
            )
            .unwrap();

        // Commit the version
        index.commit_versions(lsn1).unwrap();

        // Create snapshot after first commit
        let snapshot1 = crate::snap::Snapshot::new(
            crate::snap::SnapshotId::from(1),
            "test1".to_string(),
            lsn1,
            0,
            0,
            vec![],
        );

        // Verify "world" is visible in snapshot1
        let results = index
            .search_with_snapshot(
                TextQuery {
                    query: "world",
                    default_field: None,
                    require_positions: false,
                },
                10,
                Some(&snapshot1),
            )
            .unwrap();
        assert!(!results.is_empty(), "Original document should be visible");

        index
            .update_document(
                b"doc1",
                &[TextField {
                    name: "title",
                    text: "Hello rust",
                    boost: 1.0,
                }],
                tx_id,
                lsn2,
            )
            .unwrap();

        // Commit the update
        index.commit_versions(lsn2).unwrap();

        // Create snapshot after update
        let snapshot2 = crate::snap::Snapshot::new(
            crate::snap::SnapshotId::from(2),
            "test2".to_string(),
            lsn2,
            0,
            0,
            vec![],
        );

        // Search for "rust" with snapshot after update
        let results = index
            .search_with_snapshot(
                TextQuery {
                    query: "rust",
                    default_field: None,
                    require_positions: false,
                },
                10,
                Some(&snapshot2),
            )
            .unwrap();

        assert!(
            !results.is_empty(),
            "Updated document should contain 'rust'"
        );

        // Search for "world" with snapshot after update
        let results = index
            .search_with_snapshot(
                TextQuery {
                    query: "world",
                    default_field: None,
                    require_positions: false,
                },
                10,
                Some(&snapshot2),
            )
            .unwrap();

        assert!(
            results.is_empty(),
            "Updated document should not contain 'world'"
        );
    }
}
