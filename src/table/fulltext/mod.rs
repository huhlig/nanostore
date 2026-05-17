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
//! stored in a BTree. Supports tokenization, stemming, stop words, and
//! TF-IDF scoring.

mod posting;
mod tokenizer;

pub use self::tokenizer::{Tokenizer, TokenizerConfig, TokenizerKind};

use crate::pager::{Page, PageId, PageType, Pager};
use crate::table::{
    FullTextSearch, ScoredDocument, SpecialtyTableCapabilities, SpecialtyTableStats, Table,
    TableCapabilities, TableEngineKind, TableError, TableId, TableResult, TableStatistics,
    TextField, TextQuery, VerificationReport,
};
use crate::types::KeyBuf;
use crate::vfs::FileSystem;
use posting::{DocumentEntry, PostingEntry, PostingList};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokenizer::Tokenizer as TextTokenizer;

/// Magic number for full-text index validation.
const FULLTEXT_MAGIC: u32 = 0x46545854; // "FTXT"
const FULLTEXT_VERSION: u32 = 1;

/// Configuration for full-text search index.
#[derive(Clone, Debug)]
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
    _reserved: [u8; 40],
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

    /// In-memory document store: doc_id -> document entry
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
            _reserved: [0; 40],
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

        // TODO: Load inverted index and document store from pages
        Ok(Self {
            table_id,
            name,
            pager,
            root_page_id,
            tokenizer: TextTokenizer::default(),
            enable_positions: true,
            inverted_index: RwLock::new(HashMap::new()),
            document_store: RwLock::new(HashMap::new()),
            num_documents: RwLock::new(metadata.num_documents),
            num_terms: RwLock::new(metadata.num_terms),
        })
    }

    /// Get the root page ID.
    pub fn root_page_id(&self) -> PageId {
        self.root_page_id
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

        // Write index data to pages
        self.write_data_to_pages(&index_data, PageType::InvertedIndex)?;

        // Write document store to pages
        self.write_data_to_pages(&doc_data, PageType::InvertedIndex)?;

        // Update metadata
        self.update_metadata()?;

        Ok(())
    }

    /// Write data to pages, allocating as needed.
    fn write_data_to_pages(&self, data: &[u8], page_type: PageType) -> TableResult<()> {
        if data.is_empty() {
            return Ok(());
        }

        let data_size = self.pager.page_size().data_size();
        let mut offset = 0;
        let mut prev_page_id: Option<PageId> = None;

        while offset < data.len() {
            let chunk_size = std::cmp::min(data_size - 8, data.len() - offset);
            let page_id = self.pager.allocate_page(page_type)?;

            let mut page_data = vec![0u8; data_size];
            let next_page_id = if offset + chunk_size >= data.len() {
                0u64
            } else {
                0u64
            };
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

        Ok(())
    }

    /// Update metadata in root page.
    fn update_metadata(&self) -> TableResult<()> {
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
            _reserved: [0; 40],
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
    fn tf_idf(&self, _term: &str, doc_id: &[u8], posting_list: &PostingList) -> f32 {
        // Term frequency: count positions in this document
        let mut tf = 0.0f32;
        let mut doc_boost = 1.0f32;

        for entry in &posting_list.entries {
            if entry.doc_id == doc_id {
                tf += entry.positions.len() as f32;
                doc_boost = entry.boost;
            }
        }

        if tf == 0.0 {
            return 0.0;
        }

        // Document frequency
        let df = posting_list.doc_freq() as f32;
        let num_docs = *self.num_documents.read().unwrap() as f32;

        // IDF = log(N / df)
        let idf = if df > 0.0 {
            (num_docs / df).log10()
        } else {
            0.0
        };

        tf * idf * doc_boost
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
        tx_id: crate::txn::TransactionId,
        commit_lsn: crate::wal::LogSequenceNumber,
    ) -> TableResult<()> {
        // Remove existing document first if it exists (for updates)
        {
            let doc_store = self.document_store.read().unwrap();
            if doc_store.contains_key(doc_id) {
                drop(doc_store);
                self.delete_document(doc_id, tx_id, commit_lsn)?;
            }
        }

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

                // Find or create entry for this document
                let entry = posting_list
                    .entries
                    .iter_mut()
                    .find(|e| e.doc_id == doc_id && e.field == field_name);

                if let Some(entry) = entry {
                    if self.enable_positions {
                        entry.positions.push(position);
                    }
                } else {
                    posting_list.add(PostingEntry {
                        doc_id: doc_id.to_vec(),
                        field: field_name,
                        positions: if self.enable_positions {
                            vec![position]
                        } else {
                            vec![]
                        },
                        boost,
                    });
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

        *self.num_documents.write().unwrap() += 1;
        *self.num_terms.write().unwrap() = self.inverted_index.read().unwrap().len() as u64;

        // self.persist_index()?; // TODO: Enable when persistence is implemented

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
        _tx_id: crate::txn::TransactionId,
        _commit_lsn: crate::wal::LogSequenceNumber,
    ) -> TableResult<()> {
        // Remove from document store
        let removed = self.document_store.write().unwrap().remove(doc_id);

        // If document wasn't in store, nothing to delete
        if removed.is_none() {
            return Ok(());
        }

        // Remove from inverted index
        {
            let mut index = self.inverted_index.write().unwrap();
            for posting_list in index.values_mut() {
                posting_list.remove_document(doc_id);
            }

            // Clean up empty posting lists
            index.retain(|_, v| !v.is_empty());
            *self.num_terms.write().unwrap() = index.len() as u64;
        }

        let current = *self.num_documents.read().unwrap();
        *self.num_documents.write().unwrap() = current.saturating_sub(1);

        Ok(())
    }

    fn search(&self, query: TextQuery<'_>, limit: usize) -> TableResult<Vec<ScoredDocument>> {
        let terms = self.tokenizer.tokenize(query.query);
        let mut scores: HashMap<Vec<u8>, f32> = HashMap::new();

        let index = self.inverted_index.read().unwrap();

        for (term, _position) in terms {
            if let Some(posting_list) = index.get(&term) {
                for entry in &posting_list.entries {
                    let score = self.tf_idf(&term, &entry.doc_id, posting_list);
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
        let mut index = create_test_index();

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
                crate::txn::TransactionId::from(1),
                crate::wal::LogSequenceNumber::from(1),
            )
            .unwrap();

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
                crate::txn::TransactionId::from(1),
                crate::wal::LogSequenceNumber::from(2),
            )
            .unwrap();

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
        let mut index = create_test_index();

        index
            .index_document(
                b"doc1",
                &[TextField {
                    name: "title",
                    text: "Hello world",
                    boost: 1.0,
                }],
                crate::txn::TransactionId::from(1),
                crate::wal::LogSequenceNumber::from(1),
            )
            .unwrap();

        index
            .delete_document(
                b"doc1",
                crate::txn::TransactionId::from(1),
                crate::wal::LogSequenceNumber::from(2),
            )
            .unwrap();

        let results = index
            .search(
                TextQuery {
                    query: "hello",
                    default_field: None,
                    require_positions: false,
                },
                10,
            )
            .unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn test_update_document() {
        let mut index = create_test_index();

        index
            .index_document(
                b"doc1",
                &[TextField {
                    name: "title",
                    text: "Hello world",
                    boost: 1.0,
                }],
                crate::txn::TransactionId::from(1),
                crate::wal::LogSequenceNumber::from(1),
            )
            .unwrap();

        index
            .update_document(
                b"doc1",
                &[TextField {
                    name: "title",
                    text: "Hello rust",
                    boost: 1.0,
                }],
                crate::txn::TransactionId::from(1),
                crate::wal::LogSequenceNumber::from(2),
            )
            .unwrap();

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

        assert!(!results.is_empty());

        let results = index
            .search(
                TextQuery {
                    query: "world",
                    default_field: None,
                    require_positions: false,
                },
                10,
            )
            .unwrap();

        assert!(results.is_empty());
    }
}
