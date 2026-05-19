# Table and Index Implementation Issues

**Created**: 2026-05-07  
**Epic**: Nanostore-y8u

This document summarizes all beads issues created for the table and index architecture implementation.

---

## Epic

**Nanostore-y8u**: Table and Index Architecture Implementation (Priority 1, Epic)
- Complete implementation of table and index architecture as defined in docs/TABLE_INDEX_ARCHITECTURE.md
- Foundation for higher-level database systems

---

## Foundation Issues (Priority 1)

### Page Layer
**Nanostore-jud**: Extend PageType enum for specialized index types (Task)
- Add page types: HashBucket, ArtNode4/16/48/256, BloomFilter, InvertedIndex, RTreeNode, GraphAdjList, VectorIndex, TimeSeriesBucket, IndexMetadata
- Update from_u8() and to_u8() methods in src/pager/page.rs

### Table Layer
**Nanostore-590**: Define core Table trait and TableConfig (Task)
- Create src/table/mod.rs with Table trait
- Define TableType enum (BTree, LSM, ART)
- Define TableConfig and TableStats structs

**Nanostore-c6d**: Implement BTree Table (persistent) (Feature)
- Disk-backed B-Tree in src/table/btree.rs
- Node structures, serialization, insert/delete/scan operations
- Depends on: Nanostore-590

### Index Layer
**Nanostore-7f1**: Define core Index trait and IndexConfig (Task)
- Create src/index/mod.rs with Index trait
- Define IndexType enum (BTree, Hash, LSM, FullText, Vector, Spatial, Graph, TimeSeries, Bloom)
- Define IndexConfig, IndexStats, IndexQuery, IndexResult
- Depends on: Nanostore-590

**Nanostore-f49**: Implement BTree Index (Feature)
- Standard ordered index using BTreeTable
- Support exact lookups, range queries, prefix scans
- Depends on: Nanostore-7f1

**Nanostore-784**: Implement Hash Index (Feature)
- Hash-based exact lookups with O(1) performance
- Bucket management and collision handling
- Depends on: Nanostore-7f1

**Nanostore-vdw**: Implement Bloom Filter (Feature)
- Probabilistic membership testing
- Configurable false positive rate
- LSM table optimization
- Depends on: Nanostore-7f1

---

## Advanced Table Types (Priority 2)

**Nanostore-1lf**: Implement LSM Table (Feature)
- Write-optimized LSM tree storage
- MemTable, SSTable, level compaction
- Bloom filters per SSTable
- Depends on: Nanostore-590

**Nanostore-tq3**: Implement ART Table (memory-only) (Feature)
- Adaptive Radix Tree for fast in-memory lookups
- Node types: Node4, Node16, Node48, Node256, Leaf
- Path compression
- Depends on: Nanostore-590

---

## Specialized Indexes (Priority 2)

**Nanostore-pb2**: Implement Full-Text Index (Feature)
- Inverted index for text search
- Tokenizer, posting lists, document store
- Support AND/OR/phrase queries
- Depends on: Nanostore-7f1

**Nanostore-ejy**: Implement Vector Index (HNSW) (Feature)
- Approximate nearest neighbor search
- HNSW algorithm with configurable parameters
- Distance metrics: Euclidean, Cosine, DotProduct
- Depends on: Nanostore-7f1

**Nanostore-dat**: Implement Spatial Index (R-Tree) (Feature)
- R-Tree for spatial queries
- Support point, bounding box, radius, polygon queries
- MBR calculations and node splitting
- Depends on: Nanostore-7f1

**Nanostore-0bz**: Implement Graph Index (Feature)
- Graph traversal queries
- Adjacency list representation
- Neighbor queries, shortest path, BFS/DFS traversal
- Depends on: Nanostore-7f1

**Nanostore-yzr**: Implement Time Series Index (Feature)
- Time-based data queries
- Bucket management with specialized compression
- Support aggregations (sum, avg, min, max, count)
- Depends on: Nanostore-7f1

---

## Advanced Patterns (Priority 2)

**Nanostore-btu**: Implement Composite Index pattern (Feature)
- Combine multiple index types
- LSMWithBloom pattern for performance
- Query routing strategies
- Depends on: Nanostore-7f1

---

## Testing (Priority 2)

**Nanostore-ufp**: Add table and index integration tests (Task)
- Comprehensive integration tests in tests/table_index_tests.rs
- Test CRUD, scans, index operations, composite indexes
- Property-based tests and benchmarks
- Depends on: Nanostore-7f1

---

## Implementation Priority

### Phase 1: Core Foundation (Weeks 4-6)
1. Nanostore-jud - Extend PageType enum
2. Nanostore-590 - Define Table trait
3. Nanostore-c6d - Implement BTree Table
4. Nanostore-7f1 - Define Index trait

### Phase 2: Core Indexes (Weeks 7-8)
1. Nanostore-f49 - BTree Index
2. Nanostore-784 - Hash Index
3. Nanostore-vdw - Bloom Filter

### Phase 3: LSM Table (Weeks 9-10)
1. Nanostore-1lf - LSM Table with bloom filters

### Phase 4: Specialized Indexes (Weeks 11-14)
1. Nanostore-pb2 - Full-Text Index
2. Nanostore-ejy - Vector Index (HNSW)
3. Nanostore-dat - Spatial Index (R-Tree)
4. Nanostore-0bz - Graph Index
5. Nanostore-yzr - Time Series Index

### Phase 5: Advanced Features (Weeks 15-16)
1. Nanostore-tq3 - ART Table (memory-only)
2. Nanostore-btu - Composite Index pattern
3. Nanostore-ufp - Integration tests

---

## Dependencies Graph

```
Nanostore-y8u (Epic)
├── Nanostore-jud (PageType extension)
├── Nanostore-590 (Table trait)
│   ├── Nanostore-c6d (BTree Table)
│   ├── Nanostore-1lf (LSM Table)
│   ├── Nanostore-tq3 (ART Table)
│   └── Nanostore-7f1 (Index trait)
│       ├── Nanostore-f49 (BTree Index)
│       ├── Nanostore-784 (Hash Index)
│       ├── Nanostore-vdw (Bloom Filter)
│       ├── Nanostore-pb2 (Full-Text Index)
│       ├── Nanostore-ejy (Vector Index)
│       ├── Nanostore-dat (Spatial Index)
│       ├── Nanostore-0bz (Graph Index)
│       ├── Nanostore-yzr (Time Series Index)
│       ├── Nanostore-btu (Composite Index)
│       └── Nanostore-ufp (Integration tests)
```

---

## Quick Reference

To see ready work:
```bash
bd ready
```

To claim an issue:
```bash
bd update <id> --claim
```

To view issue details:
```bash
bd show <id>
```

---

**Total Issues Created**: 15
- 1 Epic
- 2 Foundation tasks
- 12 Feature implementations
- 1 Testing task

All issues are tracked in the beads system and linked to the architecture document at docs/TABLE_INDEX_ARCHITECTURE.md.