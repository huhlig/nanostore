# Table and Index Architecture Design

**Version**: 1.0  
**Date**: 2026-05-07  
**Status**: Design

---

## Overview

This document defines the architecture for tables and indexes in Nanostore, supporting multiple storage engines and specialized index types while maintaining a clean, composable interface.

---

## Core Principles

1. **Separation of Concerns**: Tables store data, indexes accelerate queries
2. **Pluggable Engines**: Multiple table implementations (BTree, LSM, ART)
3. **Composable Indexes**: Indexes built on top of tables
4. **Type Safety**: Strong typing for keys and values
5. **Performance**: Zero-copy where possible, efficient serialization

---

## Table Layer Architecture

### Table Trait

```rust
/// Core table interface - all table types implement this
pub trait Table: Send + Sync {
    /// Insert or update a key-value pair
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()>;
    
    /// Get a value by key
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;
    
    /// Delete a key-value pair
    fn delete(&mut self, key: &[u8]) -> Result<bool>;
    
    /// Check if a key exists
    fn contains(&self, key: &[u8]) -> Result<bool>;
    
    /// Range scan from start to end (inclusive)
    fn range_scan(&self, start: &[u8], end: &[u8]) -> Result<Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>)>>>>;
    
    /// Prefix scan - all keys starting with prefix
    fn prefix_scan(&self, prefix: &[u8]) -> Result<Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>)>>>>;
    
    /// Get table statistics
    fn stats(&self) -> TableStats;
    
    /// Flush any pending writes
    fn flush(&mut self) -> Result<()>;
}

pub struct TableStats {
    pub entry_count: u64,
    pub total_key_bytes: u64,
    pub total_value_bytes: u64,
    pub page_count: u64,
}
```

### Table Types

```rust
pub enum TableType {
    /// Disk-backed B-Tree (ordered, range queries)
    BTree,
    /// Disk-backed LSM Tree (write-optimized)
    LSM,
    /// Memory-only Adaptive Radix Tree (fast lookups)
    ART,
}

pub struct TableConfig {
    pub table_type: TableType,
    pub name: String,
    pub persistent: bool,  // true = disk, false = memory-only
    pub cache_size: Option<usize>,
    pub compression: CompressionType,
    pub encryption: EncryptionType,
}
```

### Table Implementations

#### 1. BTree Table

```rust
pub struct BTreeTable<FS: FileSystem> {
    config: TableConfig,
    pager: Arc<Pager<FS>>,
    root_page_id: PageId,
    metadata: BTreeMetadata,
}

struct BTreeMetadata {
    order: u16,
    height: u8,
    entry_count: u64,
}

impl<FS: FileSystem> Table for BTreeTable<FS> {
    // Implementation using BTree nodes stored in pages
}
```

#### 2. LSM Table

```rust
pub struct LSMTable<FS: FileSystem> {
    config: TableConfig,
    pager: Arc<Pager<FS>>,
    memtable: MemTable,
    levels: Vec<LSMLevel>,
    bloom_filters: Vec<BloomFilter>,
}

struct LSMLevel {
    level: u8,
    sstables: Vec<SSTable>,
}

impl<FS: FileSystem> Table for LSMTable<FS> {
    // Implementation with memtable + sorted string tables
}
```

#### 3. ART Table (Memory-only)

```rust
pub struct ARTTable {
    config: TableConfig,
    root: Option<Box<ARTNode>>,
    entry_count: u64,
}

enum ARTNode {
    Node4 { keys: [u8; 4], children: [Option<Box<ARTNode>>; 4] },
    Node16 { keys: [u8; 16], children: [Option<Box<ARTNode>>; 16] },
    Node48 { key_index: [u8; 256], children: [Option<Box<ARTNode>>; 48] },
    Node256 { children: [Option<Box<ARTNode>>; 256] },
    Leaf { key: Vec<u8>, value: Vec<u8> },
}

impl Table for ARTTable {
    // Implementation using adaptive radix tree in memory
}
```

---

## Index Layer Architecture

### Index Trait

```rust
/// Core index interface - all index types implement this
pub trait Index: Send + Sync {
    /// Index type identifier
    fn index_type(&self) -> IndexType;
    
    /// Insert a key into the index pointing to a value location
    fn insert(&mut self, key: &[u8], location: &[u8]) -> Result<()>;
    
    /// Remove a key from the index
    fn remove(&mut self, key: &[u8]) -> Result<bool>;
    
    /// Lookup locations for a key
    fn lookup(&self, key: &[u8]) -> Result<Vec<Vec<u8>>>;
    
    /// Range query on index
    fn range(&self, start: &[u8], end: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>>;
    
    /// Specialized query (index-type specific)
    fn query(&self, query: &IndexQuery) -> Result<IndexResult>;
    
    /// Get index statistics
    fn stats(&self) -> IndexStats;
}

pub struct IndexStats {
    pub entry_count: u64,
    pub size_bytes: u64,
    pub index_type: IndexType,
}
```

### Index Types

```rust
pub enum IndexType {
    // Core indexes
    BTree,
    Hash,
    LSM,
    
    // Specialized indexes
    FullText(FullTextConfig),
    Vector(VectorConfig),
    Spatial(SpatialConfig),
    Graph(GraphConfig),
    TimeSeries(TimeSeriesConfig),
    Bloom(BloomConfig),
}

pub struct IndexConfig {
    pub index_type: IndexType,
    pub name: String,
    pub unique: bool,
    pub underlying_table: TableType,
}
```

### Specialized Index Configurations

```rust
pub struct FullTextConfig {
    pub tokenizer: TokenizerType,
    pub min_word_length: usize,
    pub stop_words: Vec<String>,
    pub stemming: bool,
}

pub struct VectorConfig {
    pub dimensions: u32,
    pub algorithm: VectorAlgorithm,
    pub metric: DistanceMetric,
}

pub enum VectorAlgorithm {
    HNSW { m: u32, ef_construction: u32 },
    IVF { n_lists: u32 },
    Flat,
}

pub enum DistanceMetric {
    Euclidean,
    Cosine,
    DotProduct,
}

pub struct SpatialConfig {
    pub algorithm: SpatialAlgorithm,
    pub dimensions: u8,
    pub max_entries_per_node: u16,
}

pub enum SpatialAlgorithm {
    RTree,
    Quadtree,
    Geohash,
}

pub struct GraphConfig {
    pub representation: GraphRepresentation,
    pub directed: bool,
}

pub enum GraphRepresentation {
    AdjacencyList,
    AdjacencyMatrix,
    EdgeList,
}

pub struct TimeSeriesConfig {
    pub bucket_duration: Duration,
    pub compression: TimeSeriesCompression,
    pub retention: Option<Duration>,
}

pub enum TimeSeriesCompression {
    None,
    DeltaOfDelta,
    Gorilla,
}

pub struct BloomConfig {
    pub size_bits: u64,
    pub hash_functions: u8,
    pub false_positive_rate: f64,
}
```

### Index Query Types

```rust
pub enum IndexQuery {
    // Core queries
    Exact(Vec<u8>),
    Range { start: Vec<u8>, end: Vec<u8> },
    Prefix(Vec<u8>),
    
    // Specialized queries
    FullText(FullTextQuery),
    Vector(VectorQuery),
    Spatial(SpatialQuery),
    Graph(GraphQuery),
    TimeSeries(TimeSeriesQuery),
}

pub struct FullTextQuery {
    pub terms: Vec<String>,
    pub operator: TextOperator,
}

pub enum TextOperator {
    And,
    Or,
    Phrase,
}

pub struct VectorQuery {
    pub vector: Vec<f32>,
    pub k: usize,  // top-k results
    pub ef_search: Option<u32>,  // HNSW parameter
}

pub struct SpatialQuery {
    pub query_type: SpatialQueryType,
}

pub enum SpatialQueryType {
    Point(Point),
    BoundingBox(BoundingBox),
    Radius { center: Point, radius: f64 },
    Polygon(Vec<Point>),
}

pub struct GraphQuery {
    pub query_type: GraphQueryType,
}

pub enum GraphQueryType {
    Neighbors { vertex: u64, edge_type: Option<String> },
    ShortestPath { from: u64, to: u64 },
    Traverse { start: u64, max_depth: u32 },
}

pub struct TimeSeriesQuery {
    pub start_time: Timestamp,
    pub end_time: Timestamp,
    pub aggregation: Option<Aggregation>,
}

pub enum Aggregation {
    Sum,
    Avg,
    Min,
    Max,
    Count,
}

pub enum IndexResult {
    Locations(Vec<Vec<u8>>),
    FullText(FullTextResult),
    Vector(VectorResult),
    Spatial(SpatialResult),
    Graph(GraphResult),
    TimeSeries(TimeSeriesResult),
}
```

---

## Index Implementations

### 1. BTree Index

```rust
pub struct BTreeIndex<FS: FileSystem> {
    config: IndexConfig,
    table: BTreeTable<FS>,
}

impl<FS: FileSystem> Index for BTreeIndex<FS> {
    // Standard ordered index using BTree table
}
```

### 2. Hash Index

```rust
pub struct HashIndex<FS: FileSystem> {
    config: IndexConfig,
    buckets: Vec<PageId>,
    bucket_count: u32,
}

impl<FS: FileSystem> Index for HashIndex<FS> {
    // Hash-based index for exact lookups
}
```

### 3. Full-Text Index

```rust
pub struct FullTextIndex<FS: FileSystem> {
    config: IndexConfig,
    inverted_index: BTreeTable<FS>,  // term -> posting list
    document_store: BTreeTable<FS>,  // doc_id -> metadata
    tokenizer: Box<dyn Tokenizer>,
}

impl<FS: FileSystem> Index for FullTextIndex<FS> {
    fn query(&self, query: &IndexQuery) -> Result<IndexResult> {
        match query {
            IndexQuery::FullText(q) => {
                let results = self.search_terms(&q.terms, &q.operator)?;
                Ok(IndexResult::FullText(results))
            }
            _ => Err(Error::InvalidQuery),
        }
    }
}
```

### 4. Vector Index (HNSW)

```rust
pub struct VectorIndex<FS: FileSystem> {
    config: IndexConfig,
    entry_point: PageId,
    vectors: BTreeTable<FS>,  // vector_id -> vector data
    graph: BTreeTable<FS>,    // node_id -> neighbor list
    metadata: VectorMetadata,
}

struct VectorMetadata {
    dimensions: u32,
    max_level: u8,
    m: u32,
    ef_construction: u32,
}

impl<FS: FileSystem> Index for VectorIndex<FS> {
    fn query(&self, query: &IndexQuery) -> Result<IndexResult> {
        match query {
            IndexQuery::Vector(q) => {
                let results = self.search_hnsw(&q.vector, q.k, q.ef_search)?;
                Ok(IndexResult::Vector(results))
            }
            _ => Err(Error::InvalidQuery),
        }
    }
}
```

### 5. Spatial Index (R-Tree)

```rust
pub struct SpatialIndex<FS: FileSystem> {
    config: IndexConfig,
    root_page_id: PageId,
    pager: Arc<Pager<FS>>,
    metadata: SpatialMetadata,
}

struct SpatialMetadata {
    dimensions: u8,
    height: u8,
    entry_count: u64,
}

impl<FS: FileSystem> Index for SpatialIndex<FS> {
    fn query(&self, query: &IndexQuery) -> Result<IndexResult> {
        match query {
            IndexQuery::Spatial(q) => {
                let results = self.search_rtree(&q.query_type)?;
                Ok(IndexResult::Spatial(results))
            }
            _ => Err(Error::InvalidQuery),
        }
    }
}
```

### 6. Graph Index

```rust
pub struct GraphIndex<FS: FileSystem> {
    config: IndexConfig,
    vertices: BTreeTable<FS>,     // vertex_id -> properties
    edges: BTreeTable<FS>,        // edge_id -> (src, dst, properties)
    adjacency: BTreeTable<FS>,    // vertex_id -> edge_list
}

impl<FS: FileSystem> Index for GraphIndex<FS> {
    fn query(&self, query: &IndexQuery) -> Result<IndexResult> {
        match query {
            IndexQuery::Graph(q) => {
                let results = match &q.query_type {
                    GraphQueryType::Neighbors { vertex, edge_type } => {
                        self.get_neighbors(*vertex, edge_type.as_deref())?
                    }
                    GraphQueryType::ShortestPath { from, to } => {
                        self.shortest_path(*from, *to)?
                    }
                    GraphQueryType::Traverse { start, max_depth } => {
                        self.traverse(*start, *max_depth)?
                    }
                };
                Ok(IndexResult::Graph(results))
            }
            _ => Err(Error::InvalidQuery),
        }
    }
}
```

### 7. Time Series Index

```rust
pub struct TimeSeriesIndex<FS: FileSystem> {
    config: IndexConfig,
    buckets: BTreeTable<FS>,  // timestamp_bucket -> compressed data
    metadata: TimeSeriesMetadata,
}

struct TimeSeriesMetadata {
    bucket_duration: Duration,
    compression: TimeSeriesCompression,
    first_timestamp: Timestamp,
    last_timestamp: Timestamp,
}

impl<FS: FileSystem> Index for TimeSeriesIndex<FS> {
    fn query(&self, query: &IndexQuery) -> Result<IndexResult> {
        match query {
            IndexQuery::TimeSeries(q) => {
                let results = self.query_range(
                    q.start_time,
                    q.end_time,
                    q.aggregation.as_ref()
                )?;
                Ok(IndexResult::TimeSeries(results))
            }
            _ => Err(Error::InvalidQuery),
        }
    }
}
```

### 8. Bloom Filter

```rust
pub struct BloomFilter<FS: FileSystem> {
    config: IndexConfig,
    bitmap: Vec<PageId>,
    size_bits: u64,
    hash_functions: u8,
}

impl<FS: FileSystem> Index for BloomFilter<FS> {
    fn lookup(&self, key: &[u8]) -> Result<Vec<Vec<u8>>> {
        // Returns empty if definitely not present
        // Returns possible locations if might be present
        if self.might_contain(key)? {
            Ok(vec![]) // Delegate to actual index
        } else {
            Ok(vec![]) // Definitely not present
        }
    }
}
```

---

## Composition Pattern

Indexes can be composed for enhanced functionality:

```rust
pub struct CompositeIndex<FS: FileSystem> {
    primary: Box<dyn Index>,
    secondary: Vec<Box<dyn Index>>,
}

// Example: LSM Table with Bloom Filter
pub struct LSMWithBloom<FS: FileSystem> {
    lsm_table: LSMTable<FS>,
    bloom_filters: Vec<BloomFilter<FS>>,  // One per SSTable
}

impl<FS: FileSystem> Table for LSMWithBloom<FS> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        // Check bloom filters first
        for (i, bloom) in self.bloom_filters.iter().enumerate() {
            if !bloom.might_contain(key)? {
                continue;  // Skip this SSTable
            }
            if let Some(value) = self.lsm_table.get_from_level(i, key)? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }
}
```

---

## Implementation Priority

### Phase 1: Core Tables (Weeks 4-6)
1. BTree Table (persistent)
2. BTree Table (memory-only)
3. Basic table tests

### Phase 2: Core Indexes (Weeks 7-8)
1. BTree Index
2. Hash Index
3. Bloom Filter
4. Index integration tests

### Phase 3: LSM Table (Weeks 9-10)
1. LSM Table implementation
2. LSM with Bloom filters
3. Compaction strategies

### Phase 4: Specialized Indexes (Weeks 11-14)
1. Full-Text Index
2. Vector Index (HNSW)
3. Spatial Index (R-Tree)
4. Graph Index
5. Time Series Index

### Phase 5: ART Table (Weeks 15-16)
1. ART implementation (memory-only)
2. Performance optimization
3. Benchmarking

---

## Testing Strategy

1. **Unit Tests**: Each table/index type
2. **Integration Tests**: Table + Index combinations
3. **Property Tests**: Invariants (ordering, uniqueness)
4. **Benchmark Tests**: Performance comparisons
5. **Stress Tests**: Large datasets, concurrent access

---

## Performance Considerations

1. **Zero-Copy**: Use byte slices where possible
2. **Batch Operations**: Support bulk inserts/deletes
3. **Lazy Loading**: Load index pages on-demand
4. **Compression**: Compress large values/posting lists
5. **Caching**: Cache hot index pages
6. **Bloom Filters**: Reduce unnecessary lookups

---

## Future Enhancements

1. **Concurrent Indexes**: Lock-free data structures
2. **Distributed Indexes**: Sharding support
3. **Learned Indexes**: ML-based index structures
4. **Approximate Indexes**: Trade accuracy for speed
5. **Hybrid Indexes**: Combine multiple strategies

---

**End of Architecture Document**